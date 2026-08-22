//! 算法调试工具（`debug run` / `debug sweep`）——不打开 app 即可完成
//! 「跑管线 → 逐帧检测/跟踪/覆盖率报告 → 标注帧导出 → 参数扫描对比」。
//!
//! 产物（写入 OUT_DIR）：
//! - `out.mp4`         处理后的视频
//! - `report.json`     逐帧记录：person/face 检出、活跃 track（含漏检保持 lost>0）、
//!   mask 覆盖、单帧推理耗时
//! - `annotated/*.png` 可选标注帧（真实打码效果 + mask 绿罩 + 着色 track 框 + 人脸白框）
//! - sweep 额外产出 `sweep.json` / `sweep.csv` 汇总表

use automosaic_core::compose::{self, MaskStyle};
use automosaic_core::detect::{Detector, FaceBox, FaceDetector};
use automosaic_core::media;
use automosaic_core::pipe::{self, FrameTransform};
use automosaic_core::track::{IouTracker, MaskSmoother, TrackerOptions};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone)]
pub struct DebugConfig {
    pub input: PathBuf,
    pub out_dir: PathBuf,
    pub model: PathBuf,
    pub face_model: PathBuf,
    pub conf: f32,
    pub device: String,
    pub style: String,
    pub strength: u32,
    pub hwaccel: String,
    pub encoder: String,
    pub bitrate: String,
    pub batch: u32,
    pub detect_every: u32,
    pub face: bool,
    pub track: bool,
    pub smooth: bool,
    pub face_roi: bool,
    pub landmark_expand: bool,
    pub mask_ema: bool,
    /// 翻转 TTA（sweep 键 tta；极致档等价行为）。
    pub tta: bool,
    /// 相位相关全局运动补偿（sweep 键 gmc）。
    pub gmc: bool,
    /// OC-SORT 观测中心重更新（sweep 键 ocru；默认开）。
    pub ocru: bool,
    pub annotate_every: Option<u32>,
    pub annotate_at: Vec<f64>,
}

impl DebugConfig {
    pub fn label(&self) -> String {
        format!(
            "conf={:.2} style={}({}) every={} face={} track={} smooth={} tta={}",
            self.conf, self.style, self.strength, self.detect_every, self.face, self.track, self.smooth, self.tta
        )
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "input": self.input.display().to_string(),
            "model": self.model.display().to_string(),
            "conf": self.conf, "device": self.device, "style": self.style,
            "strength": self.strength, "batch": self.batch,
            "detect_every": self.detect_every, "face": self.face, "face_roi": self.face_roi, "landmark_expand": self.landmark_expand, "mask_ema": self.mask_ema,
            "track": self.track, "smooth": self.smooth, "tta": self.tta, "gmc": self.gmc, "ocru": self.ocru,
        })
    }
}

// --------------------------------------------------------------------------- //
// 报告结构
// --------------------------------------------------------------------------- //

#[derive(Serialize, Clone)]
pub struct DetRec {
    pub score: f32,
    pub xyxy: [f32; 4],
}

#[derive(Serialize, Clone)]
pub struct TrackRec {
    pub id: u64,
    pub lost: u32,
    pub score: f32,
    pub xyxy: [f32; 4],
}

#[derive(Serialize, Clone)]
pub struct FrameRecord {
    pub idx: u64,
    pub t_ms: f64,
    /// 本帧是否执行了推理（隔帧模式下为 false）。
    pub detected: bool,
    pub persons: Vec<DetRec>,
    pub faces: Vec<DetRec>,
    /// 打码所用的全部 track（含漏检保持 lost>0）。
    pub tracks: Vec<TrackRec>,
    pub mask_px: u64,
    pub infer_ms: f64,
}

#[derive(Serialize)]
pub struct RunReport {
    pub config: serde_json::Value,
    pub frames: u64,
    pub fps: f64,
    pub infer_ms_total: f64,
    pub mean_persons: f64,
    pub mean_faces: f64,
    /// 平均 mask 覆盖率（占画面百分比）。
    pub mask_cov_pct: f64,
    /// 漏检保持帧占比（打码全部来自 lost>0 的 track 的帧比例）。
    pub held_pct: f64,
    pub frame_records: Vec<FrameRecord>,
}

// --------------------------------------------------------------------------- //
// 调试运行
// --------------------------------------------------------------------------- //

pub fn run(cfg: &DebugConfig) -> Result<RunReport, String> {
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(cfg.out_dir.join("annotated")).map_err(|e| e.to_string())?;
    let meta = media::probe(&cfg.input).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let batch_n = cfg.batch.max(1) as usize;
    let batch_ready = batch_n > 1
        && batch_variant(&cfg.model, cfg.batch).exists()
        && (!cfg.face || batch_variant(&cfg.face_model, cfg.batch).exists());

    // 编码器回退每次尝试需重建 transform → 工厂模式（模型重载，CoreML 有缓存）
    let records: Arc<Mutex<Vec<FrameRecord>>> = Arc::new(Mutex::new(vec![]));
    let infer_us_total: Arc<Mutex<u128>> = Arc::new(Mutex::new(0));
    let make_transform = || -> Result<FrameTransform, String> {
        let mut det = Detector::load(&cfg.model, &cfg.device, cfg.conf).map_err(|e| e.to_string())?;
        det.low_conf = Some(automosaic_core::track::BYTE_LOW_CONF); // BYTE 二段救援
        det.tta = cfg.tta; // 翻转 TTA（sweep 键 tta）
        if batch_n > 1 {
            let bp = batch_variant(&cfg.model, cfg.batch);
            if bp.exists() {
                det.enable_batch(&bp, batch_n).map_err(|e| e.to_string())?;
            }
        }
        let mut face = if cfg.face {
            let mut fd = FaceDetector::load(&cfg.face_model, &cfg.device, (cfg.conf - 0.1).max(0.1))
                .map_err(|e| e.to_string())?;
            if batch_n > 1 {
                let bp = batch_variant(&cfg.face_model, cfg.batch);
                if bp.exists() {
                    fd.enable_batch(&bp, batch_n).map_err(|e| e.to_string())?;
                }
            }
            Some(fd)
        } else {
            None
        };
        let style = match cfg.style.as_str() {
            "mosaic" => MaskStyle::Mosaic { cell: cfg.strength.clamp(2, 128) as usize },
            "blur" => MaskStyle::Blur { radius: cfg.strength.clamp(1, 64) as usize },
            "solid" => MaskStyle::Solid,
            s => return Err(format!("未知样式 {s}")),
        };

        let cfg2 = cfg.clone();
        let records = Arc::clone(&records);
        let infer_us_total = Arc::clone(&infer_us_total);
        let fps = meta.fps.max(1.0);
        let mut tracker = IouTracker::new(TrackerOptions { ema: cfg.mask_ema, ocru: cfg.ocru, ..Default::default() });
        let mut smoother = MaskSmoother::new();
        let mut scratch: Vec<u8> = Vec::new();
        let mut frame_idx: u64 = 0;
        let mut last_faces: Vec<FaceBox> = vec![];
        Ok(Box::new(move |frames: &mut [&mut [u8]]| {
            let every = cfg2.detect_every.max(1) as u64;
            let need: Vec<usize> = (0..frames.len())
                .filter(|i| (frame_idx + *i as u64) % every == 0)
                .collect();

            let t_infer = Instant::now();
            let mut bodies: Vec<Vec<automosaic_core::detect::PersonInstance>> =
                vec![vec![]; frames.len()];
            let mut faces: Vec<Vec<FaceBox>> = vec![vec![]; frames.len()];
            if !need.is_empty() {
                let refs: Vec<&[u8]> = need.iter().map(|&i| &*frames[i]).collect();
                let batch_res = det
                    .detect_person_instances_batch(&refs, w, h)
                    .map_err(|e| format!("人体推理失败: {e}"))?;
                for (k, &i) in need.iter().enumerate() {
                    bodies[i] = batch_res[k].clone();
                }
                if let Some(fd) = face.as_mut() {
                    let fb = fd
                        .detect_boxes_batch(&refs, w, h)
                        .map_err(|e| format!("人脸推理失败: {e}"))?;
                    for (k, &i) in need.iter().enumerate() {
                        faces[i] = fb[k].clone();
                    }
                }
            }
            let infer_us = t_infer.elapsed().as_micros();
            *infer_us_total.lock().unwrap() += infer_us as u128;
            let infer_ms_each = infer_us as f64 / 1000.0 / frames.len() as f64;

            for (i, frame) in frames.iter_mut().enumerate() {
                let idx = frame_idx + i as u64;
                let detected = need.contains(&i);
                let instances = if detected { std::mem::take(&mut bodies[i]) } else { vec![] };
                let mut face_boxes: Vec<FaceBox> =
                    if detected { faces[i].clone() } else { last_faces.clone() };
                let persons: Vec<DetRec> = instances
                    .iter()
                    .map(|p| DetRec { score: p.score, xyxy: p.xyxy })
                    .collect();

                let mut mask = vec![0u8; w * h];
                let mut track_recs: Vec<TrackRec> = Vec::new();
                if cfg2.track {
                    // 同一借用作用域内既累 mask 又记录（id/lost/score/box）。
                    // 保持帧与主管线（MosaicPipeline）同语义：mask 按累积
                    // 位移（KF 速度外推 + GMC）平移后再膨胀——人物移走后
                    // 马赛克跟随外推而非原地冻结（残影修复）
                    for t in tracker.update_with_motion(instances, cfg2.conf, [0.0, 0.0]) {
                        // 离场快速衰减（与主管线 MosaicPipeline 同语义）
                        if t.lost > 3
                            && automosaic_core::track::near_frame_edge(t.xyxy, w, h, 12.0)
                        {
                            continue;
                        }
                        let (sx, sy) = (t.shift[0], t.shift[1]);
                        let box_shifted = [
                            t.xyxy[0] + sx,
                            t.xyxy[1] + sy,
                            t.xyxy[2] + sx,
                            t.xyxy[3] + sy,
                        ];
                        if !detected || sx != 0.0 || sy != 0.0 {
                            scratch.clear();
                            scratch.extend_from_slice(&t.mask);
                            if sx != 0.0 || sy != 0.0 {
                                compose::shift_mask_region(
                                    &mut scratch,
                                    w,
                                    h,
                                    t.xyxy,
                                    sx.round() as isize,
                                    sy.round() as isize,
                                );
                            }
                            compose::dilate_region(&mut scratch, w, h, box_shifted, t.hold_dilate_px());
                            for (o, m) in mask.iter_mut().zip(&scratch) {
                                *o |= *m;
                            }
                        } else {
                            for (o, m) in mask.iter_mut().zip(&t.mask) {
                                *o |= *m;
                            }
                        }
                        track_recs.push(TrackRec {
                            id: t.id,
                            lost: t.lost,
                            score: t.score,
                            xyxy: box_shifted,
                        });
                    }
                } else {
                    for inst in &instances {
                        for (o, m) in mask.iter_mut().zip(&inst.mask) {
                            *o |= *m;
                        }
                        track_recs.push(TrackRec {
                            id: u64::MAX,
                            lost: 0,
                            score: inst.score,
                            xyxy: inst.xyxy,
                        });
                    }
                }
                if cfg2.face_roi && detected {
                    if let Some(fd) = &mut face {
                        for t in &track_recs {
                            let (pw, ph) = (t.xyxy[2] - t.xyxy[0], t.xyxy[3] - t.xyxy[1]);
                            let roi = [
                                (t.xyxy[0] + pw * 0.08).max(0.0),
                                (t.xyxy[1] - ph * 0.05).max(0.0),
                                (t.xyxy[2] - pw * 0.08).min(w as f32),
                                (t.xyxy[1] + ph * 0.30).min(h as f32),
                            ];
                            if let Ok(rf) = fd.detect_boxes_roi(frame, w, h, roi) {
                                face_boxes = automosaic_core::detect::merge_faces(face_boxes, rf, 0.6);
                            }
                        }
                    }
                }
                let face_boxes = automosaic_core::detect::filter_implausible_faces(
                    face_boxes,
                    &track_recs.iter().map(|t| t.xyxy).collect::<Vec<_>>(),
                );
                let face_boxes = automosaic_core::detect::gate_faces(
                    face_boxes,
                    &track_recs.iter().map(|t| t.xyxy).collect::<Vec<_>>(),
                    0.6,
                );
                for fb in &face_boxes {
                    const EXPAND: usize = 12;
                    let (ex, ey) = automosaic_core::detect::face_expand_xy(fb, EXPAND, cfg2.landmark_expand);
                    let x1 = (fb.xyxy[0] as usize).saturating_sub(ex);
                    let y1 = (fb.xyxy[1] as usize).saturating_sub(ey);
                    let x2 = (fb.xyxy[2] as usize + 1 + ex).min(w);
                    let y2 = (fb.xyxy[3] as usize + 1 + ey).min(h);
                    for y in y1..y2 {
                        for x in x1..x2 {
                            mask[y * w + x] = 1;
                        }
                    }
                }
                if !detected {
                    compose::dilate(&mut mask, w, h, 10);
                }
                if cfg2.smooth {
                    smoother.apply(&mut mask, w, h);
                }
                let mask_px = mask.iter().filter(|&&v| v != 0).count() as u64;
                compose::apply(frame, w, h, &mask, &style);

                // 标注帧：真实打码效果 + mask 绿罩 + 着色 track 框 + 人脸白框
                let t_ms = idx as f64 * 1000.0 / fps;
                let half_frame_ms = 500.0 / fps;
                let want_annotate = cfg2
                    .annotate_every
                    .is_some_and(|n| n > 0 && idx % n as u64 == 0)
                    || cfg2
                        .annotate_at
                        .iter()
                        .any(|&t| (t * 1000.0 - t_ms).abs() < half_frame_ms);
                if want_annotate {
                    let mut rgba = media::nv12_to_rgba(frame, w, h);
                    overlay_mask(&mut rgba, w, h, &mask);
                    for tr in &track_recs {
                        draw_rect(&mut rgba, w, h, tr.xyxy, track_color(tr.id), 4);
                    }
                    for fb in &face_boxes {
                        draw_rect(&mut rgba, w, h, fb.xyxy, [255, 255, 255, 255], 3);
                    }
                    write_png(
                        cfg2.out_dir.join("annotated").join(format!("{idx:06}.png")),
                        &rgba,
                        w,
                        h,
                    );
                }

                records.lock().unwrap().push(FrameRecord {
                    idx,
                    t_ms,
                    detected,
                    persons,
                    faces: face_boxes
                        .iter()
                        .map(|f| DetRec { score: f.score, xyxy: f.xyxy })
                        .collect(),
                    tracks: track_recs,
                    mask_px,
                    infer_ms: infer_ms_each,
                });
                if detected && !faces[i].is_empty() {
                    last_faces = faces[i].clone();
                }
            }
            frame_idx += frames.len() as u64;
            Ok(())
        }))
    };

    let hw = crate::resolve_hwaccel(&cfg.input, &cfg.hwaccel);
    let encoders = crate::resolve_encoders(&cfg.encoder);
    let t0 = Instant::now();
    let stats = crate::run_with_encoder_fallback(&encoders, |enc| {
        let transform = make_transform()
            .map_err(|e| pipe::PipelineError::TransformFailed { frames: 0, reason: e })?;
        pipe::run(
            &cfg.input,
            &cfg.out_dir.join("out.mp4"),
            pipe::PipelineOptions {
                hwaccel: hw.clone(),
                encoder: enc,
                bitrate: cfg.bitrate.clone(),
                transform: Some(transform),
                batch_size: if batch_ready { batch_n } else { 1 },
                cancel: None,
                frame_format: media::FrameFormat::Nv12,
            },
            |_| {},
        )
    })
    .map_err(|e| e.to_string())?;

    let recs = records.lock().unwrap();
    let n = recs.len().max(1) as f64;
    let total_px = w as f64 * h as f64;
    let report = RunReport {
        config: cfg.to_json(),
        frames: stats.frames,
        fps: stats.frames as f64 / t0.elapsed().as_secs_f64().max(1e-9),
        infer_ms_total: *infer_us_total.lock().unwrap() as f64 / 1000.0,
        mean_persons: recs.iter().map(|r| r.persons.len() as f64).sum::<f64>() / n,
        mean_faces: recs.iter().map(|r| r.faces.len() as f64).sum::<f64>() / n,
        mask_cov_pct: recs.iter().map(|r| r.mask_px as f64).sum::<f64>() / n / total_px * 100.0,
        held_pct: recs
            .iter()
            .filter(|r| r.detected && r.persons.is_empty() && !r.tracks.is_empty())
            .count() as f64
            / n
            * 100.0,
        frame_records: recs.clone(),
    };
    std::fs::write(
        cfg.out_dir.join("report.json"),
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(report)
}

// --------------------------------------------------------------------------- //
// 参数扫描
// --------------------------------------------------------------------------- //

/// `--sweep key=v1,v2` 覆盖配置键；笛卡尔积逐组合运行，汇总表输出。
pub fn sweep(cfg: &DebugConfig, sweeps: &[(String, Vec<String>)]) -> Result<(), String> {
    if sweeps.is_empty() {
        return Err("至少需要一个 --sweep key=v1,v2".into());
    }
    // 展开笛卡尔积
    let mut combos: Vec<Vec<(String, String)>> = vec![vec![]];
    for (key, values) in sweeps {
        let mut next = vec![];
        for c in &combos {
            for v in values {
                let mut c2 = c.clone();
                c2.push((key.clone(), v.clone()));
                next.push(c2);
            }
        }
        combos = next;
    }

    let mut rows: Vec<(String, RunReport)> = vec![];
    for (i, combo) in combos.iter().enumerate() {
        let mut c = cfg.clone();
        for (k, v) in combo {
            apply_override(&mut c, k, v)?;
        }
        c.out_dir = cfg.out_dir.join(format!("run{i:03}"));
        println!("[{}/{}] {} …", i + 1, combos.len(), c.label());
        let r = run(&c)?;
        rows.push((c.label(), r));
    }

    // 汇总表
    println!(
        "\n{:<58} {:>7} {:>8} {:>8} {:>8} {:>8}",
        "配置", "fps", "persons", "faces", "cov%", "held%"
    );
    for (label, r) in &rows {
        println!(
            "{:<58} {:>7.1} {:>8.2} {:>8.2} {:>8.2} {:>8.1}",
            label, r.fps, r.mean_persons, r.mean_faces, r.mask_cov_pct, r.held_pct
        );
    }
    let json = serde_json::to_string_pretty(&rows.iter().map(|(l, r)| {
        serde_json::json!({"label": l, "fps": r.fps, "frames": r.frames,
            "mean_persons": r.mean_persons, "mean_faces": r.mean_faces,
            "mask_cov_pct": r.mask_cov_pct, "held_pct": r.held_pct,
            "infer_ms_total": r.infer_ms_total})
    }).collect::<Vec<_>>())
    .map_err(|e| e.to_string())?;
    std::fs::write(cfg.out_dir.join("sweep.json"), json).map_err(|e| e.to_string())?;
    let mut csv = String::from("label,fps,mean_persons,mean_faces,mask_cov_pct,held_pct\n");
    for (l, r) in &rows {
        csv.push_str(&format!(
            "\"{l}\",{:.2},{:.3},{:.3},{:.3},{:.2}\n",
            r.fps, r.mean_persons, r.mean_faces, r.mask_cov_pct, r.held_pct
        ));
    }
    std::fs::write(cfg.out_dir.join("sweep.csv"), csv).map_err(|e| e.to_string())?;
    println!("\n汇总: {}/sweep.json + sweep.csv", cfg.out_dir.display());
    Ok(())
}

fn apply_override(cfg: &mut DebugConfig, key: &str, v: &str) -> Result<(), String> {
    match key {
        "conf" => cfg.conf = v.parse().map_err(|_| format!("conf={v} 不是数字"))?,
        "strength" => cfg.strength = v.parse().map_err(|_| format!("strength={v} 不是数字"))?,
        "detect-every" => cfg.detect_every = v.parse().map_err(|_| format!("detect-every={v} 不是数字"))?,
        "batch" => cfg.batch = v.parse().map_err(|_| format!("batch={v} 不是数字"))?,
        "style" => cfg.style = v.to_string(),
        "device" => cfg.device = v.to_string(),
        "face" => cfg.face = parse_bool(v, "face")?,
        "face-roi" => cfg.face_roi = parse_bool(v, "face-roi")?,
        "landmark" => cfg.landmark_expand = parse_bool(v, "landmark")?,
        "ema" => cfg.mask_ema = parse_bool(v, "ema")?,
        "track" => cfg.track = parse_bool(v, "track")?,
        "smooth" => cfg.smooth = parse_bool(v, "smooth")?,
        "tta" => cfg.tta = parse_bool(v, "tta")?,
        "gmc" => cfg.gmc = parse_bool(v, "gmc")?,
        "ocru" => cfg.ocru = parse_bool(v, "ocru")?,
        other => {
            return Err(format!(
                "未知 sweep 键 {other}（可用：conf/strength/detect-every/batch/style/device/face/track/smooth/tta/gmc/ocru）"
            ))
        }
    }
    Ok(())
}

fn parse_bool(v: &str, key: &str) -> Result<bool, String> {
    match v {
        "1" | "true" | "on" => Ok(true),
        "0" | "false" | "off" => Ok(false),
        _ => Err(format!("{key}={v} 不是布尔（1/0/true/false）")),
    }
}

// --------------------------------------------------------------------------- //
// 标注绘制（RGBA 直绘 + ffmpeg 管道写 PNG，零图像库依赖）
// --------------------------------------------------------------------------- //

fn overlay_mask(rgba: &mut [u8], w: usize, h: usize, mask: &[u8]) {
    for i in 0..w * h {
        if mask[i] != 0 {
            rgba[i * 4] = (rgba[i * 4] as u32 * 3 / 4 + 60).min(255) as u8;
            rgba[i * 4 + 1] = (rgba[i * 4 + 1] as u32 + 90).min(255) as u8;
            rgba[i * 4 + 2] = (rgba[i * 4 + 2] as u32 * 3 / 4 + 20).min(255) as u8;
        }
    }
}

fn draw_rect(rgba: &mut [u8], w: usize, h: usize, xyxy: [f32; 4], color: [u8; 4], t: usize) {
    let (x1, y1) = (xyxy[0].max(0.0) as usize, xyxy[1].max(0.0) as usize);
    let (x2, y2) = ((xyxy[2] as usize).min(w - 1), (xyxy[3] as usize).min(h - 1));
    for y in y1..=y2 {
        for x in x1..=x2 {
            let border = x < x1 + t || x + t > x2 || y < y1 + t || y + t > y2;
            if border {
                let i = (y * w + x) * 4;
                rgba[i] = color[0];
                rgba[i + 1] = color[1];
                rgba[i + 2] = color[2];
                rgba[i + 3] = 255;
            }
        }
    }
}

/// track id → 稳定配色（8 色循环）。
fn track_color(id: u64) -> [u8; 4] {
    const PALETTE: [[u8; 4]; 8] = [
        [255, 76, 76, 255], [76, 175, 255, 255], [255, 165, 0, 255], [156, 39, 176, 255],
        [0, 230, 118, 255], [255, 105, 180, 255], [121, 85, 72, 255], [240, 240, 60, 255],
    ];
    PALETTE[(id % 8) as usize]
}

fn write_png(path: PathBuf, rgba: &[u8], w: usize, h: usize) {
    let Ok(mut child) = Command::new(media::tool_path("ffmpeg"))
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pixel_format", "rgba"])
        .arg("-video_size")
        .arg(format!("{w}x{h}"))
        .args(["-i", "-", "-frames:v", "1"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(rgba);
    }
    let _ = child.wait();
}

fn batch_variant(model: &Path, batch: u32) -> PathBuf {
    let stem = model.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    model.with_file_name(format!("{stem}-b{batch}.onnx"))
}
