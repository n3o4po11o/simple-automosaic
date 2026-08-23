//! RetinaFace 滑窗人脸检测（DESIGN §5.6 管线 A 步骤 4：SAHI 滑窗 + 多尺度）。
//!
//! 模型资产：yakhyo/retinaface-pytorch v0.0.1 的 retinaface_r34.onnx
//!（biubug6 Pytorch_Retinaface 的 ResNet34 移植，MIT；设计点名的 R50 官方权重
//! 仅 Google Drive 分发、镜像实测偏弱，R34 为 2026-08-21 真实帧验证的可得最优，
//! R50 待可靠镜像后按同接口替换）。
//!
//! 推理规格（2026-08-21 双图验证）：
//! - 输入 `[1,3,H,W]` 动态尺寸，**BGR float32 − (104,117,123)**（原仓库 detect.py 语义）；
//! - 输出 `loc [1,N,4]`（原始 delta）、`conf [1,N,2]`（已 softmax）、`landmarks [1,N,10]`；
//! - 锚框：steps [8,16,32]、min_sizes [[16,32],[64,128],[256,512]]、**归一化 cxcywh**、
//!   方差 [0.1, 0.2]；解码后 ×[W,H] 得像素框；landmarks 前两点为左右眼。
//!
//! 滑窗（SAHI 思路）：原生尺度 1280² tile（25% overlap）保小脸 + 半尺度全帧一遍
//! 保大脸稳定；tile 间 NMS 合并（IoU 0.4）。

use std::path::Path;

use crate::detect::{crop_nv12, DetectError, FaceBox};

/// 滑窗 tile 边长（原生尺度）。
const TILE: usize = 1280;
/// tile 重叠比例（DESIGN §5.6：20-25%）。
const OVERLAP: usize = 320;
/// NMS IoU 阈值。
const NMS_IOU: f32 = 0.4;
/// 每锚框 min_sizes（与导出图一致）。
const MIN_SIZES: [&[usize]; 3] = [&[16, 32], &[64, 128], &[256, 512]];
/// 步长。
const STEPS: [usize; 3] = [8, 16, 32];

fn commit(device: &str, model: &Path) -> Result<ort::session::Session, DetectError> {
    let mut b = ort::session::Session::builder()?;
    #[cfg(target_os = "macos")]
    if device != "cpu" {
        let units = match device {
            "gpu" => ort::ep::coreml::ComputeUnits::CPUAndGPU,
            "ane" => ort::ep::coreml::ComputeUnits::CPUAndNeuralEngine,
            _ => ort::ep::coreml::ComputeUnits::All,
        };
        let cache = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".cache/automosaic/coreml")
            .join(device);
        let _ = std::fs::create_dir_all(&cache);
        b = b
            .with_execution_providers(
                [ort::ep::CoreML::default()
                    .with_compute_units(units)
                    .with_model_cache_dir(cache.display().to_string())
                    .build()],
            )
            .unwrap_or_else(|e| e.recover());
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if device != "cpu" && device != "openvino" {
        use ort::ep::webgpu::{DawnBackendType, WebGPU};
        b = b
            .with_execution_providers(
                [WebGPU::default()
                    .with_dawn_backend_type(DawnBackendType::Vulkan)
                    .build()],
            )
            .unwrap_or_else(|e| e.recover());
    }
    #[cfg(target_os = "windows")]
    crate::windows_ep!(b, device);
    Ok(b.commit_from_file(model)?)
}

/// NV12 → BGR float CHW（直接转换，无缩放；BGR 通道序是 RetinaFace 训练分布）。
fn nv12_to_bgrchw(nv12: &[u8], w: usize, h: usize, out_w: usize, out_h: usize) -> Vec<f32> {
    let plane = out_w * out_h;
    let mut out = vec![0f32; 3 * plane];
    let y_plane = &nv12[..w * h];
    let uv = &nv12[w * h..];
    let (chw, chh) = (w / 2, h / 2);
    let (sx, sy) = (w as f32 / out_w as f32, h as f32 / out_h as f32);
    let mean = [104.0f32, 117.0, 123.0]; // BGR 顺序

    for oy in 0..out_h {
        let fy = (oy as f32 + 0.5) * sy - 0.5;
        let y0 = fy.floor().max(0.0) as usize;
        let iy0 = y0.min(h - 1);
        let iy1 = (y0 + 1).min(h - 1);
        let dy = (fy - y0 as f32).clamp(0.0, 1.0);
        let cy = (((fy + 0.5) * 0.5) as usize).min(chh - 1);
        for ox in 0..out_w {
            let fx = (ox as f32 + 0.5) * sx - 0.5;
            let x0 = fx.floor().max(0.0) as usize;
            let ix0 = x0.min(w - 1);
            let ix1 = (x0 + 1).min(w - 1);
            let dx = (fx - x0 as f32).clamp(0.0, 1.0);
            let cx = (((fx + 0.5) * 0.5) as usize).min(chw - 1);

            let y00 = y_plane[iy0 * w + ix0] as f32;
            let y10 = y_plane[iy0 * w + ix1] as f32;
            let y01 = y_plane[iy1 * w + ix0] as f32;
            let y11 = y_plane[iy1 * w + ix1] as f32;
            let yv = y00 * (1.0 - dx) * (1.0 - dy)
                + y10 * dx * (1.0 - dy)
                + y01 * (1.0 - dx) * dy
                + y11 * dx * dy;
            let u = uv[cy * w + cx * 2] as f32;
            let v = uv[cy * w + cx * 2 + 1] as f32;
            let yy = (yv - 16.0) * 1.1644;
            let r = (yy + 1.5960 * (v - 128.0)).clamp(0.0, 255.0);
            let g = (yy - 0.3917 * (u - 128.0) - 0.8130 * (v - 128.0)).clamp(0.0, 255.0);
            let b = (yy + 2.0172 * (u - 128.0)).clamp(0.0, 255.0);

            let i = oy * out_w + ox;
            out[i] = b - mean[0];
            out[plane + i] = g - mean[1];
            out[2 * plane + i] = r - mean[2];
        }
    }
    out
}

/// 归一化锚框（cxcywh，与导出图的 loc 顺序一致：step×{min_sizes} 行主序）。
/// 特征图行数 = ceil(边/step)（biubug6 PriorBox 语义，非整数整除时补边）。
fn anchors(iw: usize, ih: usize) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    for (k, &st) in STEPS.iter().enumerate() {
        let (cols, rows) = ((iw + st - 1) / st, (ih + st - 1) / st);
        for y in 0..rows {
            for x in 0..cols {
                for &s in MIN_SIZES[k] {
                    out.push([
                        (x * st + st / 2) as f32 / iw as f32,
                        (y * st + st / 2) as f32 / ih as f32,
                        s as f32 / iw as f32,
                        s as f32 / ih as f32,
                    ]);
                }
            }
        }
    }
    out
}

fn box_iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let aa = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let ab = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    if aa + ab <= 0.0 { 0.0 } else { inter / (aa + ab - inter) }
}

/// 滑窗 RetinaFace 人脸检测器。
pub struct RetinaFace {
    session: ort::session::Session,
    pub conf: f32,
    /// 滑窗开关（关 = 全帧单次推理，小图/调试用）。
    pub sliding: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RetinaError {
    #[error("模型文件不存在: {0}（设置中下载或 scripts/fetch_m5_models.sh）")]
    ModelNotFound(std::path::PathBuf),
    #[error("ort 错误: {0}")]
    Ort(#[from] ort::Error),
    #[error("{0}")]
    Load(#[from] DetectError),
    #[error("输出形状异常: loc {0:?}")]
    BadShape(Vec<i64>),
}

/// 一次 tile 推理的原始候选（tile 像素坐标）。
struct TileDet {
    xyxy: [f32; 4],
    score: f32,
    eyes: Option<([f32; 2], [f32; 2])>,
}

impl RetinaFace {
    pub fn load(model: &Path, device: &str, conf: f32) -> Result<Self, RetinaError> {
        if !model.exists() {
            return Err(RetinaError::ModelNotFound(model.to_path_buf()));
        }
        Ok(Self { session: commit(device, model)?, conf, sliding: true })
    }

    /// 检测人脸（原帧像素坐标；含双眼 landmark）。
    pub fn detect_faces(&mut self, nv12: &[u8], w: usize, h: usize) -> Result<Vec<FaceBox>, RetinaError> {
        let mut all: Vec<FaceBox> = Vec::new();
        let mut dets: Vec<TileDet> = Vec::new();

        if self.sliding {
            // 尺度 1（原生）：1280² tile，25% overlap——小脸有效分辨率不减
            let stride = TILE - OVERLAP;
            let mut y = 0usize;
            while y < h {
                let mut x = 0usize;
                while x < w {
                    let (x2, y2) = ((x + TILE).min(w), (y + TILE).min(h));
                    self.run_tile(nv12, w, h, x as f32, y as f32, x2 as f32, y2 as f32, &mut dets)?;
                    if x + TILE >= w { break; }
                    x += stride;
                }
                if y + TILE >= h { break; }
                y += stride;
            }
            // 尺度 2（半分辨率全帧）：大脸的尺度稳定确认（一次推理）
            self.run_half_scale(nv12, w, h, &mut dets)?;
        } else {
            self.run_tile(nv12, w, h, 0.0, 0.0, w as f32, h as f32, &mut dets)?;
        }

        // 跨 tile/跨尺度合并：按分数降序 NMS
        dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        for d in dets {
            if all.iter().all(|k| box_iou(&k.xyxy, &d.xyxy) <= NMS_IOU) {
                all.push(FaceBox { xyxy: d.xyxy, score: d.score, eyes: d.eyes });
            }
        }
        Ok(all)
    }

    /// 单 tile 推理（tile 原生分辨率直入，无缩放；锚框随 tile 尺寸生成）。
    /// roi 为全帧坐标；产出映射回全帧。
    #[allow(clippy::too_many_arguments)]
    fn run_tile(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
        roi_x1: f32,
        roi_y1: f32,
        roi_x2: f32,
        roi_y2: f32,
        out: &mut Vec<TileDet>,
    ) -> Result<(), RetinaError> {
        let (cw, ch, ox, oy, crop) = crop_nv12(nv12, w, h, [roi_x1, roi_y1, roi_x2, roi_y2]);
        if cw < 64 || ch < 64 {
            return Ok(()); // 极小 tile 无意义
        }
        let input = nv12_to_bgrchw(&crop, cw, ch, cw, ch);
        let anc = anchors(cw, ch);
        let dets = self.infer(&input, cw, ch, &anc)?;
        for d in dets {
            out.push(TileDet {
                xyxy: [
                    d.xyxy[0] + ox as f32,
                    d.xyxy[1] + oy as f32,
                    d.xyxy[2] + ox as f32,
                    d.xyxy[3] + oy as f32,
                ],
                score: d.score,
                eyes: d.eyes.map(|(l, r)| ([l[0] + ox as f32, l[1] + oy as f32], [r[0] + ox as f32, r[1] + oy as f32])),
            });
        }
        Ok(())
    }

    /// 半尺度全帧一遍：宽高减半（大脸的 anchor 尺度更贴），框 ×2 映射回。
    fn run_half_scale(&mut self, nv12: &[u8], w: usize, h: usize, out: &mut Vec<TileDet>) -> Result<(), RetinaError> {
        let (hw, hh) = (w / 2, h / 2);
        if hw < 64 || hh < 64 {
            return Ok(());
        }
        let input = nv12_to_bgrchw(nv12, w, h, hw, hh);
        let anc = anchors(hw, hh);
        let dets = self.infer(&input, hw, hh, &anc)?;
        for d in dets {
            out.push(TileDet {
                xyxy: [d.xyxy[0] * 2.0, d.xyxy[1] * 2.0, d.xyxy[2] * 2.0, d.xyxy[3] * 2.0],
                score: d.score,
                eyes: d.eyes.map(|(l, r)| ([l[0] * 2.0, l[1] * 2.0], [r[0] * 2.0, r[1] * 2.0])),
            });
        }
        Ok(())
    }

    /// 单次推理 + 解码（阈值过滤 + 眼点提取）。
    fn infer(&mut self, input: &[f32], iw: usize, ih: usize, anc: &[[f32; 4]]) -> Result<Vec<TileDet>, RetinaError> {
        let outputs = self.session.run(ort::inputs! {
            "input" => ort::value::Tensor::from_array((
                [1i64, 3, ih as i64, iw as i64],
                input.to_vec(),
            ))?,
        })?;
        let (ls, lt) = outputs["loc"].try_extract_tensor::<f32>()?;
        let (cs, ct) = outputs["conf"].try_extract_tensor::<f32>()?;
        let (ms, mt) = outputs["landmarks"].try_extract_tensor::<f32>()?;
        if ls.len() != 3 || cs.len() != 3 || ms.len() != 3 || ls[2] != 4 || cs[2] != 2 {
            return Err(RetinaError::BadShape(ls.to_vec()));
        }
        let n = ls[1] as usize;
        if n != anc.len() || n != cs[1] as usize {
            return Err(RetinaError::BadShape(ls.to_vec()));
        }
        let mut out = Vec::new();
        for i in 0..n {
            let score = ct[i * 2 + 1];
            if score < self.conf {
                continue;
            }
            let a = anc[i];
            let (l0, l1, l2, l3) = (lt[i * 4], lt[i * 4 + 1], lt[i * 4 + 2], lt[i * 4 + 3]);
            let cx = (a[0] + l0 * 0.1 * a[2]) * iw as f32;
            let cy = (a[1] + l1 * 0.1 * a[3]) * ih as f32;
            let bw = a[2] * (l2 * 0.2).exp() * iw as f32;
            let bh = a[3] * (l3 * 0.2).exp() * ih as f32;
            if bw < 2.0 || bh < 2.0 {
                continue;
            }
            // landmarks 前两点 = 左右眼（归一化 delta 同方差解码）
            let eye = |k: usize| {
                let ex = (a[0] + mt[i * 10 + 2 * k] * 0.1 * a[2]) * iw as f32;
                let ey = (a[1] + mt[i * 10 + 2 * k + 1] * 0.1 * a[3]) * ih as f32;
                [ex, ey]
            };
            out.push(TileDet {
                xyxy: [cx - bw / 2.0, cy - bh / 2.0, cx + bw / 2.0, cy + bh / 2.0],
                score,
                eyes: Some((eye(0), eye(1))),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_match_expected_count_and_order() {
        // 640×640: (80²+40²+20²)×2 = 16800；首锚 stride8 中心 (4,4) size 16
        let a = anchors(640, 640);
        assert_eq!(a.len(), 16800);
        assert!((a[0][0] - 4.0 / 640.0).abs() < 1e-6);
        assert!((a[0][2] - 16.0 / 640.0).abs() < 1e-6);
        // 第二锚同 cell 32
        assert!((a[1][2] - 32.0 / 640.0).abs() < 1e-6);
    }

    #[test]
    fn bgrchw_swaps_channels() {
        // 红 NV12（R=255）→ B 通道 0、R 通道最大；均值已减（B 通道 = b−104）
        let (w, h) = (32, 32);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        nv12[..w * h].fill(81); // limited-range 红 Y
        let uv = &mut nv12[w * h..];
        for i in 0..uv.len() / 2 {
            uv[i * 2] = 90; // U 小 → 红方向
            uv[i * 2 + 1] = 240; // V 大
        }
        let chw = nv12_to_bgrchw(&nv12, w, h, w, h);
        let plane = w * h;
        let (b, g, r) = (chw[0], chw[plane], chw[2 * plane]);
        assert!(r > g && r > b, "R 应最大: r={r} g={g} b={b}");
    }

    /// 真实模型回归锚点：tests/face_test.mp4（biubug6 curve/test.jpg 夹具，
    /// 2026-08-21 Python 对照：多张人脸，top score ≈ 0.99）。
    #[test]
    fn sliding_detects_multiple_faces_on_fixture() {
        let model = crate::models::resolve_model("retinaface-r34.onnx");
        let video = std::path::Path::new("../../tests/face_test.mp4");
        if !model.exists() || !video.exists() {
            eprintln!("skip: 无 RetinaFace 模型或夹具（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut rf = RetinaFace::load(&model, "cpu", 0.6).expect("加载");
        let faces = rf.detect_faces(&nv12, w, h).expect("推理");
        assert!(faces.len() >= 3, "群像夹具应检出 ≥3 张脸，得 {}", faces.len());
        assert!(faces[0].score > 0.9, "最高分 {}", faces[0].score);
        // Python 锚点之一：(71,51)-(118,89)（±15px 容差内存在一张脸）
        let hit = faces.iter().any(|f| {
            (f.xyxy[0] - 71.0).abs() < 15.0
                && (f.xyxy[1] - 51.0).abs() < 15.0
                && (f.xyxy[2] - 118.0).abs() < 15.0
                && (f.xyxy[3] - 89.0).abs() < 15.0
        });
        assert!(hit, "应命中 Python 锚点脸 (71,51,118,89)：{:?}", faces.iter().map(|f| f.xyxy).collect::<Vec<_>>());
        // 双眼点在框内
        for f in faces.iter().take(3) {
            if let Some((l, r)) = f.eyes {
                assert!(l[0] >= f.xyxy[0] - 5.0 && l[0] <= f.xyxy[2] + 5.0, "左眼 {l:?} 框 {:?}", f.xyxy);
                assert!(r[0] >= f.xyxy[0] - 5.0 && r[0] <= f.xyxy[2] + 5.0);
            }
        }
    }
}
