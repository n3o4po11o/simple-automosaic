//! frb API：探测 / 处理（事件流）/ 取消 / 预览关键帧 / 预设与模型管理。
//! 只做参数翻译与事件桥接，管线逻辑全部复用 automosaic-core
//! （变换本体在 core::mosaic，CLI 与本路径共用）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use automosaic_core::compose::MaskStyle;
use automosaic_core::detect::{Detector, FaceDetector};
use automosaic_core::media;
use automosaic_core::models as model_store;
use automosaic_core::mosaic;
use automosaic_core::pipe::{self, CancelFlag, PipelineOptions};
use automosaic_core::preset::QualityPreset;

// StreamSink 由 frb_generated_boilerplate! 宏生成于本 crate 的 frb_generated 模块
use crate::frb_generated::StreamSink;

static CANCEL: OnceLock<CancelFlag> = OnceLock::new();

fn cancel_flag() -> CancelFlag {
    CANCEL.get_or_init(|| Arc::new(AtomicBool::new(false))).clone()
}

// --------------------------------------------------------------------------- //
// 数据类型（frb 自动翻译为 Dart 类）
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub total_frames: Option<u64>,
    pub duration_secs: Option<f64>,
    pub has_audio: bool,
    /// 容器旋转元数据（度；±90 时 width/height 已是旋转后的显示尺寸）
    pub rotation: f32,
}

#[derive(Debug, Clone)]
pub struct ProcessOptions {
    /// 质量预设 id：speed / balanced / accurate / extreme
    pub preset: String,
    pub conf: f32,
    /// auto（macOS=CoreML）/ cpu
    pub device: String,
    /// mosaic / blur / solid
    pub style: String,
    pub strength: u32,
    /// 显式人体模型（自导入 ONNX）；None = 用预设模型
    pub model_path: Option<String>,
    /// auto / none / 显式名
    pub hwaccel: String,
    /// auto / 显式名
    pub encoder: String,
    /// 目标码率；"auto" = 按分辨率档位缩放（1080p 基准 6M，core 侧解析）
    pub bitrate: String,
    /// 是否启用人脸检测
    pub face: bool,
    /// 人脸框外扩像素；0 = 取预设默认
    pub face_expand: u32,
    /// 隔帧检测间隔覆写；0 = 取预设默认
    pub detect_every: u32,
    /// 人脸级联 ROI 三态：0 = 跟随预设（极致档开），1 = 强制开，2 = 强制关
    /// （俯视视角建议关：头部区域假设失效时 ROI 会引入误检）
    pub face_roi: u8,
    /// ByteTrack 跟踪（含 Kalman 预测/低分救援/漏检保持）
    pub track: bool,
    /// mask 时序平滑（上一帧膨胀并集）
    pub mask_smooth: bool,
    /// per-ID mask EMA（α=0.7）
    pub mask_ema: bool,
    /// landmark 外扩（眼距自适应抗转头）
    pub landmark_expand: bool,
    /// 批推理大小覆写；0 = 取预设默认
    pub batch: u32,
    /// 翻转 TTA 三态：0 = 跟随预设（极致档开），1 = 强制开，2 = 强制关
    /// （推理 ×2 换召回，离线档推荐）
    pub tta: u8,
    /// 相位相关全局运动补偿（运动镜头：预测框平移 + 保持帧遮罩跟随；静机位自动零位移）
    pub gmc: bool,
}

/// 预设可用性与描述（ConfigScreen 预设选择器 + 模型卡片数据源）。
#[derive(Debug, Clone)]
pub struct PresetInfo {
    pub id: String,
    pub label: String,
    /// 人体模型是否就绪
    pub available: bool,
    pub body_model: String,
    pub face_model: String,
    /// 预设默认置信度（切预设时 UI 联动滑杆）
    pub conf: f32,
    /// 模型缺失但可在应用内下载（manifest 配置了 URL）
    pub downloadable: bool,
    /// 模型缺失/未实现时的指引文本；可用时为空（明细走 detail）。
    pub desc: String,
    /// 预设就绪时的结构化明细（available 时 Some）。
    pub detail: Option<PresetDetail>,
}

/// 预设就绪时的结构化明细（UI 键值展示；替代此前拼接的纯文本 desc）。
#[derive(Debug, Clone)]
pub struct PresetDetail {
    /// 人体模型名（去 .onnx 后缀）。
    pub body_model: String,
    pub body_size_mb: f64,
    /// 存在 -b4 批推理伴生模型。
    pub body_batch: bool,
    /// 人脸模型名；None = 预设未启用人脸。
    pub face_model: Option<String>,
    pub face_size_mb: f64,
    pub face_batch: bool,
    /// 预设检测间隔（1 = 逐帧）。
    pub detect_every: u32,
    pub conf: f32,
    /// 推理后端人读描述。
    pub backend_desc: String,
}

/// 推理后端条目（DESIGN §4.3 `list_backends()`；设置屏「加速器」组数据源）。
#[derive(Debug, Clone)]
pub struct BackendInfo {
    /// 设备 id（与 ProcessOptions.device 同一词表）。
    pub id: String,
    /// 人读名。
    pub label: String,
    /// 本机可用（macOS 的 CoreML/CPU 恒可用；描述为配置口径——CoreML 内部
    /// 调度无法逐算子查询，与 backend_desc 的口径一致）。
    pub available: bool,
    /// 一行说明。
    pub desc: String,
}

/// 推理后端清单（macOS=CoreML 计算单元四选 + CPU；其他平台=CPU）。
/// 注：trait 插件化与 tract 纯 Rust 兜底未做（§0.5-E），此 API 是其后端
/// 枚举的稳定面——后续新增后端不改 UI。
pub fn list_backends() -> Vec<BackendInfo> {
    use automosaic_core::detect::backend_desc;
    let entry = |id: &str, label: &str| BackendInfo {
        id: id.to_string(),
        label: label.to_string(),
        available: true,
        desc: backend_desc(id),
    };
    vec![
        entry("auto", "自动"),
        #[cfg(target_os = "macos")]
        entry("gpu", "GPU（CoreML CPU+GPU）"),
        #[cfg(target_os = "macos")]
        entry("ane", "NPU（CoreML CPU+神经引擎）"),
        // Windows：DirectML EP（auto 即默认走它；DESIGN §4.2）
        #[cfg(target_os = "windows")]
        entry("directml", "DirectML（DX12）"),
        entry("cpu", "CPU"),
    ]
}

/// manifest 模型条目状态（设置屏模型管理列表）。
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub file: String,
    pub imgsz: u32,
    pub size_mb: f64,
    pub present: bool,
    pub batch_present: bool,
    /// 缺失且 manifest 配置了下载地址 → 可在应用内下载。
    pub downloadable: bool,
}

/// 下载进度事件流。
#[derive(Clone)]
pub enum DownloadEvent {
    /// name = 当前正在下载的文件名（主文件或 -b4 伴生文件）。
    Progress { name: String, done_bytes: u64, total_bytes: u64 },
    Finished { name: String },
    Failed { error: String },
}

/// 处理阶段（DESIGN §7.4 Stage 的流式管线子集：流式模式下各段并发，
/// 事件标记任务级状态机边界而非独占段）。
#[derive(Debug, Clone)]
pub enum ProcessStage {
    /// 探测视频元数据（分辨率/音轨/旋转）。
    Probing,
    /// 模型就绪，推理+合成+编码管线启动。
    Inferring,
    /// 帧流结束，编码器收尾写 moov（faststart 二次排布）。
    Finalizing,
}

/// 处理事件流（DESIGN §7.4 的 M2 子集）。
#[derive(Clone)]
pub enum ProcessEvent {
    /// 任务级阶段切换（结构化状态机事件，UI 阶段行数据源）。
    StageEnter { stage: ProcessStage },
    /// 任务元数据（结构化，处理屏任务卡"任务信息"数据源；替代此前的
    /// 两条纯文本日志——预设/模型/后端/分辨率等以键值展示而非管道符文本）。
    JobMeta {
        /// 预设 id（speed/balanced/accurate/extreme）。
        preset: String,
        /// 预设人读名（速度/均衡/准确/极致）。
        preset_label: String,
        /// 人体模型文件名。
        body_model: String,
        /// 是否启用人脸检测。
        face: bool,
        /// 人脸模型文件名（未启用为 None）。
        face_model: Option<String>,
        /// 隔帧检测间隔（1 = 逐帧）。
        detect_every: u32,
        /// 批推理大小。
        batch: u32,
        width: u32,
        height: u32,
        total_frames: Option<u64>,
        /// 模型加载耗时（秒；CoreML 编译有持久缓存时 <1s）。
        model_load_secs: f64,
        /// 推理后端人读描述（backend_desc）。
        device_desc: String,
        /// 实际使用的解码 hwaccel（None = 软件解码）。
        decoder: String,
        /// 本次尝试的编码器（编码器回退重试时随重发更新）。
        encoder: String,
    },
    Progress {
        frames: u64,
        decoded: u64,
        written: u64,
        total_frames: Option<u64>,
        fps: f64,
        eta_secs: Option<f64>,
    },
    /// 关键节点日志（模型加载/编码器回退/完成等，日志面板展示）。
    Log { line: String },
    /// 处理中的左右对照预览（缩放后的 RGBA，每 8 帧一对）。
    PreviewPair {
        frame_idx: u64,
        original: Vec<u8>,
        processed: Vec<u8>,
        width: u32,
        height: u32,
    },
    Finished { output: String, frames: u64, elapsed_secs: f64 },
    Failed { error: String },
    Cancelled { frames: u64 },
}

/// 预览帧：应用真实遮罩样式后的 RGBA（"修改后画面"）+ 叠加检测框
/// （DESIGN §7.5：CustomPainter 画 person 框/人脸框/双眼点）。
pub struct PreviewFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// person 检测框（原始分辨率坐标）。
    pub persons: Vec<PreviewBox>,
    /// 人脸框（含双眼 landmark，像素坐标）。
    pub faces: Vec<PreviewBox>,
}

/// 叠加框（原始分辨率像素坐标；eyes = [lx,ly,rx,ry]，无 landmark 为 None）。
#[derive(Debug, Clone)]
pub struct PreviewBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    pub eyes: Option<Vec<f32>>,
}

// --------------------------------------------------------------------------- //
// 预设与模型管理
// --------------------------------------------------------------------------- //

/// 五档预设列表（含可用性与描述）。archive 档可用性 = ensemble 五件套齐
/// （ReID 可选；M5 两阶段，流式 process 拒绝并指引 archiveAnalyze）。
pub fn list_presets(device: String) -> Vec<PresetInfo> {
    QualityPreset::ALL
        .iter()
        .map(|p| {
            // Archive（M5）：可用性 = ensemble 五件套齐（ReID 可选）；缺件给出清单
            if *p == QualityPreset::Archive {
                let pp = p.params().expect("Archive 参数恒 Ok");
                let refs = pp.archive.expect("Archive 模型组");
                let resolve = |n: &str| model_store::resolve_model(n);
                let comps: [(&str, PathBuf); 5] = [
                    ("YOLO26x@1536", resolve(&pp.body_model)),
                    ("GroundingDINO", resolve(refs.gd)),
                    ("SAM2.1-enc", resolve(refs.sam_encoder)),
                    ("SAM2.1-dec", resolve(refs.sam_decoder)),
                    ("RetinaFace", resolve(refs.retina)),
                ];
                let missing: Vec<&str> = comps
                    .iter()
                    .filter(|(_, pth)| !pth.exists())
                    .map(|(n, _)| *n)
                    .collect();
                let available = missing.is_empty();
                let downloadable = !available
                    && model_store::load_manifest().is_some_and(|m| {
                        comps.iter().all(|(_, pth)| {
                            let name = pth.file_name().and_then(|s| s.to_str()).unwrap_or_default();
                            m.find(name).is_some_and(|e| {
                                e.url.is_some() || e.mirror_url.is_some() || e.direct_url.is_some()
                            })
                        })
                    });
                let desc = if available {
                    String::new()
                } else {
                    format!(
                        "极限·档案级（两阶段 分析→复核→渲染）：缺少 {}（scripts/fetch_m5_models.sh 或本页下载）",
                        missing.join("、")
                    )
                };
                let detail = available.then(|| PresetDetail {
                    body_model: format!(
                        "ensemble: {} + {} + SAM2.1-large + {}",
                        pp.body_model.trim_end_matches(".onnx"),
                        refs.gd.trim_end_matches(".onnx"),
                        refs.retina.trim_end_matches(".onnx")
                    ),
                    body_size_mb: comps.iter().map(|(_, pth)| {
                        pth.metadata().map(|m| m.len() as f64 / 1048576.0).unwrap_or(0.0)
                    }).sum(),
                    body_batch: false,
                    face_model: Some("retinaface-r34 滑窗".into()),
                    face_size_mb: 0.0,
                    face_batch: false,
                    detect_every: 1,
                    conf: pp.conf,
                    backend_desc: automosaic_core::detect::backend_desc(&device),
                });
                return PresetInfo {
                    id: p.id().to_string(),
                    label: p.label().to_string(),
                    available,
                    body_model: pp.body_model.clone(),
                    face_model: "retinaface-r34.onnx".into(),
                    conf: pp.conf,
                    downloadable,
                    desc,
                    detail,
                };
            }
            let params = p.params();
            let (body, face, available, downloadable, desc, conf, detail) = match &params {
                Ok(pp) => {
                    let body = model_store::resolve_model(&pp.body_model);
                    let face = model_store::resolve_first(&[
                        &pp.face_model,
                        "yolo11n-face-pose.onnx",
                        "yolov8n-face.onnx",
                    ]);
                    let available = body.exists();
                    // 结构化明细：模型名/大小/批伴生/检测节奏/后端（UI 键值展示）
                    let detail = available.then(|| {
                        let file_info = |p: &Path| -> (String, f64, bool) {
                            let size = p
                                .metadata()
                                .map(|m| m.len() as f64 / 1048576.0)
                                .unwrap_or(0.0);
                            let stem =
                                p.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                            let b4 = p
                                .with_file_name(format!("{stem}-b4.onnx"))
                                .exists();
                            (stem.to_string(), size, b4)
                        };
                        let (b_model, b_size, b_batch) = file_info(&body);
                        let (f_model, f_size, f_batch) =
                            face.as_deref().map(file_info).unwrap_or_default();
                        PresetDetail {
                            body_model: b_model,
                            body_size_mb: b_size,
                            body_batch: b_batch,
                            face_model: face.is_some().then_some(f_model),
                            face_size_mb: f_size,
                            face_batch: f_batch,
                            detect_every: pp.detect_every,
                            conf: pp.conf,
                            backend_desc: automosaic_core::detect::backend_desc(&device),
                        }
                    });
                    let downloadable = !available
                        && model_store::load_manifest().is_some_and(|m| {
                            m.find(&pp.body_model)
                                .is_some_and(|e| e.url.is_some() || e.mirror_url.is_some())
                        });
                    let desc = preset_desc(p, pp, &body, face.as_deref(), &device, available);
                    (
                        body.to_string_lossy().into_owned(),
                        face.map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                        available,
                        downloadable,
                        desc,
                        pp.conf,
                        detail,
                    )
                }
                Err(e) => (String::new(), String::new(), false, false, e.clone(), 0.35, None),
            };
            PresetInfo {
                id: p.id().to_string(),
                label: p.label().to_string(),
                available,
                body_model: body,
                face_model: face,
                conf,
                downloadable,
                desc,
                detail,
            }
        })
        .collect()
}

/// 预设一行描述：模型（尺寸/大小/批推理）+ 隔帧 + 后端。
fn preset_desc(
    _p: &QualityPreset,
    pp: &automosaic_core::preset::PresetParams,
    _body: &Path,
    _face: Option<&Path>,
    _device: &str,
    available: bool,
) -> String {
    // 可用预设的明细已结构化（PresetDetail），desc 只承载缺失/未实现指引
    if !available {
        return format!(
            "缺少模型 {}（未随安装包分发：可在本页直接下载，或获取 ONNX 放入 {}）",
            pp.body_model,
            model_store::user_models_dir().display(),
        );
    }
    String::new()
}

/// manifest 模型清单（含本地存在性；SHA 校验按需走 [`verify_model`]）。
pub fn list_models() -> Vec<ModelInfo> {
    let manifest = model_store::load_manifest();
    match manifest {
        Some(m) => m
            .models
            .iter()
            .map(|e| {
                let p = model_store::resolve_model(&e.file);
                ModelInfo {
                    file: e.file.clone(),
                    imgsz: e.imgsz,
                    size_mb: e.size_mb,
                    present: p.exists(),
                    batch_present: e
                        .batch_file
                        .as_ref()
                        .map(|b| model_store::resolve_model(b).exists())
                        .unwrap_or(false),
                    downloadable: !p.exists()
                        && (e.url.is_some() || e.mirror_url.is_some() || e.direct_url.is_some()),
                }
            })
            .collect(),
        None => vec![],
    }
}

/// 应用内下载模型（主文件 + 批推理伴生文件）到用户模型目录，
/// SHA256 校验 + 镜像回退；进度经事件流回报。
pub fn download_model(
    file: String,
    sink: StreamSink<DownloadEvent>,
) -> Result<(), String> {
    let manifest =
        model_store::load_manifest().ok_or_else(|| "manifest.json 不存在".to_string())?;
    let entry = manifest
        .find(&file)
        .ok_or_else(|| format!("manifest 中无 {file}"))?
        .clone();
    if entry.url.is_none() && entry.mirror_url.is_none() && entry.direct_url.is_none() {
        return Err(format!("{file} 未配置下载地址"));
    }
    // 节流：进度回调最多 ~20Hz，避免淹没 Dart 事件循环
    let mut last_emit = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap_or_else(std::time::Instant::now);
    let sink_progress = sink.clone();
    match model_store::download_entry(&entry, move |name, done, total| {
        let now = std::time::Instant::now();
        if total == 0 || done == total || now.duration_since(last_emit) >= Duration::from_millis(50)
        {
            last_emit = now;
            let _ = sink_progress.add(DownloadEvent::Progress {
                name: name.to_string(),
                done_bytes: done,
                total_bytes: total,
            });
        }
    }) {
        Ok(paths) => {
            for p in paths {
                let _ = sink.add(DownloadEvent::Finished {
                    name: p.to_string_lossy().into_owned(),
                });
            }
            Ok(())
        }
        Err(e) => {
            let _ = sink.add(DownloadEvent::Failed { error: e.to_string() });
            Err(e.to_string())
        }
    }
}

/// 校验模型 SHA256 与 manifest 是否一致（大文件约 0.5s，按需调用）。
pub fn verify_model(file: String) -> Result<bool, String> {
    let manifest =
        model_store::load_manifest().ok_or_else(|| "manifest.json 不存在".to_string())?;
    let entry = manifest
        .find(&file)
        .ok_or_else(|| format!("manifest 中无 {file}"))?;
    let path = model_store::resolve_model(&file);
    model_store::verify_sha256(&path, &entry.sha256)
        .ok_or_else(|| format!("模型文件不存在: {}", path.display()))
}

// --------------------------------------------------------------------------- //
// API
// --------------------------------------------------------------------------- //

pub fn probe_video(path: String) -> Result<VideoInfo, String> {
    let m = media::probe(Path::new(&path)).map_err(|e| e.to_string())?;
    Ok(VideoInfo {
        width: m.width,
        height: m.height,
        fps: m.fps,
        codec: m.codec,
        total_frames: m.total_frames,
        duration_secs: m.duration_secs,
        has_audio: m.has_audio,
        rotation: m.rotation,
    })
}

/// 取消当前处理（幂等）。
pub fn cancel_process() {
    cancel_flag().store(true, Ordering::Relaxed);
}

/// 预设 + 用户覆写 → 最终管线参数。
struct Effective {
    body: PathBuf,
    face: Option<PathBuf>,
    conf: f32,
    face_expand: u32,
    detect_every: u32,
    batch: u32,
    face_roi: bool,
    /// 翻转 TTA（极致档默认开；UI 覆写开关待后续 FFI 字量变更时一并加）。
    tta: bool,
}

fn effective_opts(opts: &ProcessOptions) -> Result<Effective, String> {
    let preset = QualityPreset::from_id(&opts.preset)
        .ok_or_else(|| format!("未知预设 {}（可选 speed/balanced/accurate/extreme/archive）", opts.preset))?;
    // Archive 档走两阶段（archiveAnalyze → 复核 → archiveRender），流式参数不承载
    if preset == QualityPreset::Archive {
        return Err("archive 档走两阶段：分析（archiveAnalyze）→ 复核 → 渲染（archiveRender）".into());
    }
    let pp = preset.params()?;
    let body = match &opts.model_path {
        Some(m) if !m.is_empty() => PathBuf::from(m),
        _ => {
            let resolved = model_store::resolve_model(&pp.body_model);
            if !resolved.exists() {
                return Err(format!(
                    "预设[{}]缺少人体模型 {}（运行 scripts/export_models.sh 或放入 models/）",
                    preset.label(),
                    pp.body_model
                ));
            }
            resolved
        }
    };
    let face = if opts.face {
        model_store::resolve_first(&[
            &pp.face_model,
            "yolo11n-face-pose.onnx",
            "yolov8n-face.onnx",
        ])
    } else {
        None
    };
    Ok(Effective {
        body,
        face,
        conf: opts.conf,
        face_expand: if opts.face_expand > 0 { opts.face_expand } else { pp.face_expand },
        detect_every: if opts.detect_every > 0 { opts.detect_every } else { pp.detect_every },
        face_roi: match opts.face_roi {
            1 => true,
            2 => false,
            _ => pp.face_roi,
        },
        batch: if opts.batch > 0 { opts.batch } else { pp.batch },
        tta: match opts.tta {
            1 => true,
            2 => false,
            _ => pp.tta,
        },
    })
}

/// 处理视频：检测人体并打码。结果经事件流回报（Finished/Failed/Cancelled），
/// 返回值 Ok(()) 仅表示"已开始并结束（无论成败）"。
pub fn process_video(
    input: String,
    output: String,
    opts: ProcessOptions,
    sink: StreamSink<ProcessEvent>,
) -> Result<(), String> {
    let cancel = cancel_flag();
    cancel.store(false, Ordering::Relaxed);

    let log = |line: String| {
        let _ = sink.add(ProcessEvent::Log { line });
    };
    let stage = |s: ProcessStage| {
        let _ = sink.add(ProcessEvent::StageEnter { stage: s });
    };
    let fail = |e: String| -> Result<(), String> {
        let _ = sink.add(ProcessEvent::Failed { error: e.clone() });
        Err(e)
    };

    stage(ProcessStage::Probing);
    let meta = match media::probe(Path::new(&input)) {
        Ok(m) => m,
        Err(e) => return fail(e.to_string()),
    };
    let hwaccels = resolve_hwaccels(Path::new(&input), &opts.hwaccel, &meta);
    let (w, h) = (meta.width as usize, meta.height as usize);
    let eff = match effective_opts(&opts) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let style = match parse_style(&opts.style, opts.strength) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let encoders = resolve_encoders(&opts.encoder);
    let batch_n = eff.batch.max(1) as usize;
    let body_b4 = batch_n > 1 && batch_variant(&eff.body, batch_n).is_some();
    let use_face = eff.face.is_some();
    // 推理后端运行期回退：帧零 TransformFailed（如 DML 在 Gen9.5 驱动上
    // 建立成功、暖机通过、真实数据崩溃）时降 CPU 重载模型重跑
    let mut dev = opts.device.clone();
    let mut key = cache_key(&eff.body, use_face, &dev, eff.conf, eff.tta);


    let preset_label = QualityPreset::from_id(&opts.preset)
        .map(|p| p.label().to_string())
        .unwrap_or_else(|| opts.preset.clone());
    let mut last_err = String::new();
    'dev: loop {
    'hw: for hwaccel in hwaccels.iter() {
        let t_load = std::time::Instant::now();
      for (i, enc) in encoders.iter().enumerate() {
        let (det, face) = match take_models(&key, &eff, &dev) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        // 结构化任务元数据（UI 任务卡键值展示；编码器回退重试时会以新加载耗时重发）
        let _ = sink.add(ProcessEvent::JobMeta {
            preset: opts.preset.clone(),
            preset_label: preset_label.clone(),
            body_model: eff
                .body
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            face: use_face,
            face_model: eff.face.as_ref().and_then(|f| {
                f.file_name().map(|n| n.to_string_lossy().into_owned())
            }),
            detect_every: eff.detect_every,
            batch: if body_b4 { batch_n as u32 } else { 1 },
            width: meta.width,
            height: meta.height,
            total_frames: meta.total_frames,
            model_load_secs: t_load.elapsed().as_secs_f64(),
            device_desc: automosaic_core::detect::backend_desc(&dev),
            decoder: hwaccel.clone().unwrap_or_else(|| "软件解码".into()),
            encoder: enc.clone(),
        });
        stage(ProcessStage::Inferring);
        let transform = {
            let sink2 = sink.clone();
            let preview = PairSink { sink: sink2 };
            let det_dyn: Arc<Mutex<dyn automosaic_core::detect::DetectorBackend>> = det.clone();
            let face_dyn: Option<Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>>> =
                face.clone().map(|fd| {
                    let f: Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>> = fd;
                    f
                });
            mosaic::build(
                det_dyn,
                face_dyn,
                mosaic::MosaicOptions {
                    conf: eff.conf,
                    face: use_face,
                    face_roi: eff.face_roi,
                    face_expand: eff.face_expand,
                    track: opts.track,
                    smooth: opts.mask_smooth,
                    landmark_expand: opts.landmark_expand,
                    mask_ema: opts.mask_ema,
                    gmc: opts.gmc,
                    ocru: true, // OC-SORT ORU 默认开（CLI --no-ocru 可关；UI 开关待 FFI 参数扩展）
                    detect_every: eff.detect_every,
                    fps: meta.fps as f32,
                    adaptive_skip_max: 0, // UI 暂无开关入口；默认关（全档逐帧决策）
                    style: style.clone(),
                },
                w,
                h,
                Some(Box::new(preview)),
            )
        };

        let t0 = std::time::Instant::now();
        let result = pipe::run(
            Path::new(&input),
            Path::new(&output),
            PipelineOptions {
                hwaccel: hwaccel.clone(),
                encoder: enc.clone(),
                bitrate: opts.bitrate.clone(),
                transform: Some(transform),
                batch_size: if body_b4 { batch_n } else { 1 },
                cancel: Some(cancel.clone()),
                frame_format: media::FrameFormat::Nv12,
            },
            |p| {
                let _ = sink.add(ProcessEvent::Progress {
                    frames: p.frames,
                    decoded: p.decoded,
                    written: p.written,
                    total_frames: p.total_frames,
                    fps: p.fps,
                    eta_secs: p.eta_secs,
                });
            },
        );
        // 无论成败回收模型（CoreML 编译昂贵，下次任务直接复用）
        store_models(key.clone(), det, face);
        match result {
            Ok(stats) => {
                stage(ProcessStage::Finalizing);
                let _ = sink.add(ProcessEvent::Finished {
                    output,
                    frames: stats.frames,
                    elapsed_secs: t0.elapsed().as_secs_f64(),
                });
                return Ok(());
            }
            Err(pipe::PipelineError::Cancelled { frames }) => {
                let _ = sink.add(ProcessEvent::Cancelled { frames });
                return Ok(());
            }
            Err(pipe::PipelineError::EncoderFailed { frames, stderr }) if i + 1 < encoders.len() => {
                last_err = format!("编码器 {enc} 运行不可用");
                // stderr 尾部随日志上屏：回退可诊断（真机排障靠它）
                log(format!(
                    "[回退] {last_err}，尝试 {} …（stderr 尾部: {}）",
                    encoders[i + 1],
                    compact_stderr(&stderr, frames)
                ));
                continue; // 运行期降级（-encoders 列表存在 ≠ 可用）
            }
            // 帧未开始流动即解码失败：hwaccel 运行期不兼容（冒烟≠全程），
            // 降下一候选（终将落到软解）；已处理过帧则真实故障，不重试
            Err(pipe::PipelineError::DecoderFailed { frames: 0, stderr }) => {
                last_err = format!(
                    "解码器 {} 运行不可用",
                    hwaccel.clone().unwrap_or_else(|| "软解".into())
                );
                log(format!("[回退] {last_err}（stderr 尾部: {}）", compact_stderr(&stderr, 0)));
                continue 'hw;
            }
            // 帧零推理失败且非 CPU：推理后端运行期崩溃（DML 暖机≠全程——
            // Gen9.5 真机：session 建立成功、零张量暖机过、真实数据崩
            // DmlCommandRecorder），降 CPU 重载模型整链重跑
            Err(pipe::PipelineError::TransformFailed { frames: 0, .. }) if dev != "cpu" => {
                last_err = format!("推理后端 {dev} 运行不可用");
                log(format!("[回退] {last_err}，重载模型改用 CPU …"));
                dev = "cpu".into();
                key = cache_key(&eff.body, use_face, &dev, eff.conf, eff.tta);
                continue 'dev;
            }
            Err(e) => return fail(e.to_string()),
        }
      }
    }
    break;
    }
    fail(last_err)
}

/// 预览对照帧的 StreamSink 适配器。
struct PairSink {
    sink: StreamSink<ProcessEvent>,
}

impl mosaic::PreviewSink for PairSink {
    fn wants(&mut self, frame_idx: u64) -> bool {
        frame_idx % mosaic::PREVIEW_EVERY == 0
    }
    fn emit(&mut self, frame_idx: u64, original: Vec<u8>, processed: Vec<u8>, w: u32, h: u32) {
        let _ = self.sink.add(ProcessEvent::PreviewPair {
            frame_idx,
            original,
            processed,
            width: w,
            height: h,
        });
    }
}

/// 抽取指定时间点的帧，跑完整遮罩管线（与 process_video 同一 core 检测/
/// 过滤/合成语义），返回"修改后画面"RGBA + 叠加检测框——用户在播放器
/// 任意位置所见即所得（DESIGN §7.5 预览叠加层的数据源）。
/// 单帧语义 = 管线首帧行为：track 首建即观测、EMA 首值即当前、平滑首帧透传。
#[allow(clippy::too_many_arguments)]
pub fn preview_frame(
    input: String,
    position_secs: f64,
    conf: f32,
    device: String,
    preset: String,
    style: String,
    strength: u32,
) -> Result<PreviewFrame, String> {
    let meta = media::probe(Path::new(&input)).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let mut nv12 = media::decode_frame_at(Path::new(&input), position_secs, &meta)
        .map_err(|e| format!("抽帧失败: {e}"))?;

    // 极限·档案级：交互预览降级到均衡档模型（DESIGN §5.1 预览与出片解耦——
    // 出片走两阶段 ensemble，预览不付五件套加载成本）
    let preset = if preset == "archive" { "balanced".to_string() } else { preset };
    let eff = effective_opts(&ProcessOptions {
        preset,
        conf,
        device: device.clone(),
        style: style.clone(),
        strength,
        model_path: None,
        hwaccel: "auto".into(),
        encoder: "auto".into(),
        bitrate: "auto".into(),
        face: true,
        face_expand: 0,
        track: true,
        mask_smooth: true,
        mask_ema: true,
        landmark_expand: true,
        detect_every: 1, // 单帧预览无所谓隔帧
        face_roi: 0,
        batch: 0,
        tta: 0,
        gmc: false,
    })?;
    let key = cache_key(&eff.body, eff.face.is_some(), &device, eff.conf, eff.tta);
    let (det, face) = take_models(&key, &eff, &device)?;

    let style_parsed = parse_style(&style, strength)?;
    // 直接检测（首帧语义，与 mosaic::build 管线一致：track 首建即观测、
    // EMA 首值即当前、平滑首帧透传）——单帧预览无历史可依，也顺带拿到叠加框
    let instances = {
        let mut d = det.lock().unwrap_or_else(|p| p.into_inner());
        d.detect_person_instances(&nv12, w, h).map_err(|e| format!("人体推理失败: {e}"))?
    };
    let person_boxes: Vec<[f32; 4]> = instances.iter().map(|i| i.xyxy).collect();
    let faces = match &face {
        Some(fd) => {
            let mut fd = fd.lock().unwrap_or_else(|p| p.into_inner());
            let raw = fd
                .detect_boxes(&nv12, w, h)
                .map_err(|e| format!("人脸推理失败: {e}"))?;
            // 与 build 相同防线：关联过滤（person 外低分脸=误检）+ 几何合理性
            automosaic_core::detect::filter_implausible_faces(
                automosaic_core::detect::gate_faces(raw, &person_boxes, 0.6),
                &person_boxes,
            )
        }
        None => vec![],
    };

    // mask 组装同 build：person mask 并集 + 人脸外扩框（landmark 自适应）
    let expand = eff.face_expand as usize;
    let mut mask = vec![0u8; w * h];
    for inst in &instances {
        for (o, m) in mask.iter_mut().zip(&inst.mask) {
            *o |= *m;
        }
    }
    for fb in &faces {
        let (ex, ey) = automosaic_core::detect::face_expand_xy(fb, expand, true);
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
    automosaic_core::compose::apply(&mut nv12, w, h, &mask, &style_parsed);
    store_models(key, det, face);

    let to_box = |xyxy: [f32; 4], score: f32, eyes: Option<Vec<f32>>| PreviewBox {
        x1: xyxy[0],
        y1: xyxy[1],
        x2: xyxy[2],
        y2: xyxy[3],
        score,
        eyes,
    };
    Ok(PreviewFrame {
        rgba: media::nv12_to_rgba(&nv12, w, h),
        width: w as u32,
        height: h as u32,
        persons: instances
            .iter()
            .map(|i| to_box(i.xyxy, i.score, None))
            .collect(),
        faces: faces
            .iter()
            .map(|f| {
                to_box(
                    f.xyxy,
                    f.score,
                    f.eyes.map(|(l, r)| vec![l[0], l[1], r[0], r[1]]),
                )
            })
            .collect(),
    })
}

// --------------------------------------------------------------------------- //
// 参数解析（与 CLI 一致的 auto 解析逻辑）
// --------------------------------------------------------------------------- //

fn parse_style(s: &str, strength: u32) -> Result<MaskStyle, String> {
    match s {
        "mosaic" => Ok(MaskStyle::Mosaic { cell: strength.clamp(2, 128) as usize }),
        "blur" => Ok(MaskStyle::Blur { radius: strength.clamp(1, 64) as usize }),
        "solid" => Ok(MaskStyle::Solid),
        other => Err(format!("未知样式 {other}（可选 mosaic/blur/solid）")),
    }
}

/// hwaccel 选择 → 有序候选（运行期回退链）。auto = 冒烟通过的候选链 +
/// 软解兜底；显式指定 = 该项 + 软解兜底；none = 仅软解。冒烟通过 ≠ 全程
/// 可跑（真机实测：`-pix_fmt nv12` 约束下过滤器协商失败，帧 0 解码中断），
/// DecoderFailed@0 帧时按序降级重试。
fn resolve_hwaccels(input: &Path, choice: &str, meta: &media::VideoMeta) -> Vec<Option<String>> {
    match choice {
        "none" => vec![None],
        "auto" => {
            let mut list: Vec<Option<String>> = media::decode_chain()
                .into_iter()
                .flatten()
                .filter(|c| media::hwaccel_usable(input, c, meta))
                .map(|c| Some(c.to_string()))
                .collect();
            list.push(None);
            list
        }
        name => vec![Some(name.to_string()), None],
    }
}

/// stderr 压缩成单行摘要（回退日志用：尾部 3 行、去换行截 240 字）。
fn compact_stderr(stderr: &str, frames: u64) -> String {
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail: Vec<&str> = if lines.len() > 3 { lines[lines.len() - 3..].to_vec() } else { lines };
    let joined = tail.join(" ⏎ ").replace('\r', "");
    let cut: String = joined.chars().take(240).collect();
    format!("帧{frames}: {cut}")
}

/// 编码器选择 → 有序候选（auto = 候选链 ∩ 能力；libx264 兜底；运行期失败降级重试）。
fn resolve_encoders(choice: &str) -> Vec<String> {
    let mut list: Vec<String> = match choice {
        "auto" => media::encoder_chain()
            .into_iter()
            .filter(|e| media::has_encoder(e))
            .map(String::from)
            .collect(),
        name => vec![name.to_string()],
    };
    if !list.iter().any(|e| e == "libx264") && media::has_encoder("libx264") {
        list.push("libx264".into());
    }
    if list.is_empty() {
        list.push("libx264".into());
    }
    list
}

// --------------------------------------------------------------------------- //
// 模型常驻缓存：CoreML EP 每个 session 首次编译约 3s，任务间复用避免重复付费
// --------------------------------------------------------------------------- //

struct CachedModels {
    key: String,
    det: Arc<Mutex<Detector>>,
    face: Option<Arc<Mutex<FaceDetector>>>,
}

static MODELS: OnceLock<Mutex<Option<CachedModels>>> = OnceLock::new();

fn models_cell() -> &'static Mutex<Option<CachedModels>> {
    MODELS.get_or_init(|| Mutex::new(None))
}

fn cache_key(body: &Path, use_face: bool, device: &str, conf: f32, tta: bool) -> String {
    format!("{}|use_face={use_face}|{device}|{conf:.2}|tta={tta}", body.display())
}

fn load_models(
    body: &Path,
    face: Option<&PathBuf>,
    device: &str,
    conf: f32,
    tta: bool,
) -> Result<(Arc<Mutex<Detector>>, Option<Arc<Mutex<FaceDetector>>>), String> {
    let mut det = Detector::load(body, device, conf).map_err(|e| e.to_string())?;
    det.low_conf = Some(automosaic_core::track::BYTE_LOW_CONF); // BYTE 二段救援
    det.tta = tta; // 翻转 TTA（极致档）
    if let Some(b4) = batch_variant(body, 4) {
        let _ = det.enable_batch(&b4, 4); // 失败自动逐帧
    }
    let face = if let Some(fm) = face {
        let mut fd =
            FaceDetector::load(fm, device, (conf - 0.1).max(0.1)).map_err(|e| e.to_string())?;
        if let Some(b4) = batch_variant(fm, 4) {
            let _ = fd.enable_batch(&b4, 4);
        }
        Some(Arc::new(Mutex::new(fd)))
    } else {
        None
    };
    Ok((Arc::new(Mutex::new(det)), face))
}

/// 取模型：key 命中缓存则复用（从缓存移出，任务结束后 store 回收）。
fn take_models(
    key: &str,
    eff: &Effective,
    device: &str,
) -> Result<(Arc<Mutex<Detector>>, Option<Arc<Mutex<FaceDetector>>>), String> {
    let cached = models_cell().lock().unwrap_or_else(|p| p.into_inner()).take();
    match cached {
        Some(c) if c.key == key => Ok((c.det, c.face)),
        _ => load_models(&eff.body, eff.face.as_ref(), device, eff.conf, eff.tta),
    }
}

fn store_models(key: String, det: Arc<Mutex<Detector>>, face: Option<Arc<Mutex<FaceDetector>>>) {
    *models_cell().lock().unwrap_or_else(|p| p.into_inner()) = Some(CachedModels { key, det, face });
}

/// 固定 batch=N 的伴生模型（同目录 `{stem}-b{N}.onnx`），存在才启用批推理。
fn batch_variant(model: &Path, n: usize) -> Option<PathBuf> {
    let stem = model.file_stem()?.to_str()?;
    let b = model.with_file_name(format!("{stem}-b{n}.onnx"));
    b.exists().then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(preset: &str) -> ProcessOptions {
        ProcessOptions {
            preset: preset.into(),
            conf: 0.35,
            device: "cpu".into(),
            style: "mosaic".into(),
            strength: 35,
            model_path: None,
            hwaccel: "auto".into(),
            encoder: "auto".into(),
            bitrate: "auto".into(),
            face: true,
            face_expand: 0,
            face_roi: 0,
            detect_every: 0,
            batch: 0,
            track: true,
            mask_smooth: true,
            mask_ema: true,
            landmark_expand: true,
            tta: 0,
            gmc: false,
        }
    }

    #[test]
    fn effective_opts_resolves_preset_models_and_overrides() {
        // CI 检出无 models/（gitignore）时跳过文件断言，参数覆写逻辑仍验证
        let has_models = std::path::Path::new("models/yolo26n-seg.onnx").exists()
            || models_root_has("yolo26n-seg.onnx");
        if !has_models {
            eprintln!("skip: 无 models/（CI 环境）——仅验证错误分支");
            // 预设解析依赖模型文件存在：断言缺模型时给出可行动的错误
            let err = match effective_opts(&opts("balanced")) {
                Err(e) => e,
                Ok(_) => panic!("无 models 时预设解析应失败"),
            };
            assert!(err.contains("yolo26n-seg"), "缺模型错误应指明文件：{err}");
            return;
        }
        // 仓库内应能解析到 models/ 下的预设模型
        let e = effective_opts(&opts("balanced")).unwrap();
        assert!(e.body.to_string_lossy().contains("yolo26n-seg"));
        assert_eq!(e.detect_every, 1); // 预设默认（全档逐帧，2026-08-20 决策）
        assert!(e.face.is_some()); // yolo11n-face-pose 或 yolov8n-face 回退

        // 覆写优先
        let mut o = opts("speed");
        o.detect_every = 1;
        o.batch = 1;
        o.face_expand = 20;
        let e = effective_opts(&o).unwrap();
        assert_eq!((e.detect_every, e.batch, e.face_expand), (1, 1, 20));
    }

    /// models/ 是否在某搜索根下存在该文件（CI 无则 false）。
    fn models_root_has(file: &str) -> bool {
        model_store::resolve_model(file).to_string_lossy().contains("models")
            && model_store::resolve_model(file).exists()
    }

    #[test]
    fn effective_opts_rejects_unknown_and_archive() {
        assert!(effective_opts(&opts("bogus")).is_err());
        assert!(effective_opts(&opts("archive")).is_err()); // 两阶段档：流式入口拒绝
    }
}

// --------------------------------------------------------------------------- //
// M5 极限·档案级（DESIGN §5.6）：两阶段 analyze→复核→render 的 FFI 面
// --------------------------------------------------------------------------- //

/// Archive 分析参数（复核 UI 与队列共用的入口配置）。
#[derive(Debug, Clone)]
pub struct ArchiveAnalyzeOptions {
    pub device: String,
    /// SAM2.1 规格："large"（档案级默认）/ "tiny"（调试/低配）。
    pub sam_size: String,
    pub conf: f32,
    pub tta: bool,
    pub hwaccel: String,
    pub encoder: String,
    /// 预览合成样式（mosaic/blur/solid；仅用于处理屏对照图，不影响缓存）。
    pub style: String,
    /// 预览合成强度。
    pub strength: u32,
    /// 编码侧输出处理："null"（默认，-f null 帧丢弃）/"file"（真编码探针，调试）。
    pub drain: String,
}

/// Archive 分析：ensemble + WBF + SAM2.1 精修 + 滑窗人脸 + masklet 关联，
/// 逐帧落盘 .mask/.inst（断点续跑：masks_dir 已有缓存则继续）。
/// 进度/完成/取消经 ProcessEvent 流（与流式 process 同协议，UI 复用处理屏）。
pub fn archive_analyze(
    input: String,
    masks_dir: String,
    opts: ArchiveAnalyzeOptions,
    sink: StreamSink<ProcessEvent>,
) -> Result<(), String> {
    use automosaic_core::archive::{ArchiveAnalyzer, ArchiveModelPaths, ArchiveOptions};
    use automosaic_core::maskstore::{InstanceRecord, MaskStore};

    let cancel = cancel_flag();
    cancel.store(false, Ordering::Relaxed);
    let fail = |e: String| -> Result<(), String> {
        let _ = sink.add(ProcessEvent::Failed { error: e.clone() });
        Err(e)
    };
    let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Probing });
    let meta = match media::probe(Path::new(&input)) {
        Ok(m) => m,
        Err(e) => return fail(e.to_string()),
    };
    let (w, h) = (meta.width as usize, meta.height as usize);

    let pp = QualityPreset::Archive.params()?;
    let refs = pp.archive.expect("Archive 模型组");
    let (sam_enc, sam_dec) = match opts.sam_size.as_str() {
        "tiny" => ("sam2.1-tiny-encoder.onnx", "sam2.1-tiny-decoder.onnx"),
        "large" => (refs.sam_encoder, refs.sam_decoder),
        s => return fail(format!("未知 SAM 规格 {s}")),
    };
    let paths = ArchiveModelPaths {
        yolo: model_store::resolve_model(&pp.body_model),
        gd: model_store::resolve_model(refs.gd),
        sam_encoder: model_store::resolve_model(sam_enc),
        sam_decoder: model_store::resolve_model(sam_dec),
        retina: model_store::resolve_model(refs.retina),
        reid: model_store::resolve_model(refs.reid)
            .exists()
            .then(|| model_store::resolve_model(refs.reid)),
    };
    for (name, p) in [
        ("YOLO26x@1536", &paths.yolo),
        ("Grounding DINO", &paths.gd),
        ("SAM2.1 encoder", &paths.sam_encoder),
        ("SAM2.1 decoder", &paths.sam_decoder),
        ("RetinaFace", &paths.retina),
    ] {
        if !p.exists() {
            return fail(format!("缺少{name}: {}（设置→模型管理下载）", p.display()));
        }
    }

    let store = match MaskStore::new(Path::new(&masks_dir)) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    // 断点续跑（尺寸不符则重来）
    let start = match store.load_meta() {
        Ok(m) if m.width == meta.width && m.height == meta.height => store.analyzed_frames(),
        _ => {
            let _ = store.save_meta(&automosaic_core::maskstore::MaskMeta {
                width: meta.width,
                height: meta.height,
                frames: 0,
            });
            0
        }
    };

    let hwaccel = resolve_hwaccels(Path::new(&input), &opts.hwaccel, &meta)
        .into_iter()
        .next()
        .flatten();
    // 输出处理：null（默认）帧丢弃不落盘；file 真编码探针（设置屏调试开关）
    let encoders = if opts.drain == "file" {
        resolve_encoders(&opts.encoder)
    } else {
        vec!["null".to_string()]
    };

    // 模型预载（有帧待分析时）：五件套 ~1.4GB 权重，CoreML 首次编译可达数分钟
    // （缓存于 ~/.cache/automosaic/coreml，编译产物可达数 GB，此后秒级）
    // ——逐件发日志避免"无进度"观感
    let az_opts = ArchiveOptions { conf: opts.conf, tta: opts.tta, ..Default::default() };
    let need_analyze = meta.total_frames.map(|t| start < t).unwrap_or(true);
    let mut az = None;
    let mut load_secs = 0.0f64;
    if need_analyze {
        let t_load = std::time::Instant::now();
        let sink2 = sink.clone();
        match ArchiveAnalyzer::new_with_progress(&paths, az_opts.clone(), &opts.device, w, h, move |stage| {
            let _ = sink2.add(ProcessEvent::Log {
                line: format!("加载模型：{stage} …"),
            });
        }) {
            Ok(a) => az = Some(a),
            Err(e) => return fail(e),
        }
        load_secs = t_load.elapsed().as_secs_f64();
        let _ = sink.add(ProcessEvent::Log {
            line: format!("模型就绪（{load_secs:.1}s）"),
        });
    }

    let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Inferring });
    let _ = sink.add(ProcessEvent::JobMeta {
        preset: "archive".into(),
        preset_label: QualityPreset::Archive.label().to_string(),
        body_model: format!("x@1536+GD+SAM2.1-{sam_size}", sam_size = opts.sam_size),
        face: true,
        face_model: Some(refs.retina.trim_end_matches(".onnx").to_string()),
        detect_every: 1,
        batch: 1,
        width: meta.width,
        height: meta.height,
        total_frames: meta.total_frames,
        model_load_secs: load_secs,
        device_desc: automosaic_core::detect::backend_desc(&opts.device),
        // 与流式任务同口径：真实解码 hwaccel；分析段编码器仅为管道占位
        //（产物是 mask 缓存，探针文件即删）——真实出片编码器在渲染段另跑
        decoder: hwaccel.clone().unwrap_or_else(|| "软件解码".into()),
        encoder: if opts.drain == "file" {
            format!("{}·临时", encoders[0])
        } else {
            "无（-f null）".into()
        },
    });
    let _ = sink.add(ProcessEvent::Log {
        line: if opts.drain == "file" {
            "两阶段·分析：推理结果落盘 mask 缓存；编码器为探针（结束即删），最终出片在渲染段".to_string()
        } else {
            "两阶段·分析：推理结果落盘 mask 缓存；编码侧 -f null 直接丢弃输出（不产视频），最终出片在渲染段".to_string()
        },
    });
    let t0 = std::time::Instant::now();
    let mut last_err = String::new();
    for (i, enc) in encoders.iter().enumerate() {
        let store2 = match MaskStore::new(Path::new(&masks_dir)) {
            Ok(s) => s,
            Err(e) => return fail(e.to_string()),
        };
        let mut pos = 0u64; // 流内绝对位置（管线恒从帧 0 解码，无 seek）
        let mut az = az.take();
        let sink_p = sink.clone();
        let preview_style = parse_style(&opts.style, opts.strength).ok();
        let (dw, dh) = mosaic::preview_size(w, h);
        let transform: pipe::FrameTransform = Box::new(move |frames: &mut [&mut [u8]]| {
            let base = pos;
            pos += frames.len() as u64;
            // 已分析帧直接跳过：续跑不重付推理（帧号按流内位置对齐——
            // 曾因从 start 起写导致帧号与内容错位 + 全片重推）
            let skip = if base >= start { 0 } else { ((start - base) as usize).min(frames.len()) };
            if skip == frames.len() {
                return Ok(());
            }
            let az = az
                .as_mut()
                .ok_or_else(|| "分析器未就绪（无待分析帧不应到达）".to_string())?;
            for (fi, frame) in frames.iter_mut().enumerate().skip(skip) {
                let next = base + fi as u64;
                let instances = az.analyze_frame(frame)?;
                let mut merged = vec![0u8; w * h];
                for inst in &instances {
                    for (o, &m) in merged.iter_mut().zip(&inst.mask) {
                        *o |= m;
                    }
                }
                let records: Vec<InstanceRecord> = instances
                    .iter()
                    .map(|i| InstanceRecord {
                        id: i.id,
                        kind: i.kind,
                        score: i.score,
                        xyxy: i.xyxy,
                        mask: i.mask.clone(),
                    })
                    .collect();
                store2.save_mask(next, &merged).map_err(|e| e.to_string())?;
                store2.save_instances(next, &records).map_err(|e| e.to_string())?;
                store2
                    .save_meta(&automosaic_core::maskstore::MaskMeta {
                        width: w as u32,
                        height: h as u32,
                        frames: next + 1,
                    })
                    .map_err(|e| e.to_string())?;
                // 处理屏对照预览：每 4 个已分析帧一对（流式 8 帧一对是 10fps
                // 口径；档案级 0.1fps 下 8 帧≈80s 太稀，4 帧≈40s）
                if next % 4 == 0 {
                    if let Some(style) = preview_style.as_ref() {
                    let orig = media::nv12_to_rgba_scaled(frame, w, h, dw, dh);
                    automosaic_core::compose::apply(frame, w, h, &merged, style);
                    let proc = media::nv12_to_rgba_scaled(frame, w, h, dw, dh);
                        let _ = sink_p.add(ProcessEvent::PreviewPair {
                            frame_idx: next,
                            original: orig,
                            processed: proc,
                            width: dw as u32,
                            height: dh as u32,
                        });
                    }
                }
            }
            Ok(())
        });
        let result = pipe::run(
            Path::new(&input),
            &Path::new(&masks_dir).join("_unused.mp4"),
            PipelineOptions {
                hwaccel: hwaccel.clone(),
                encoder: enc.clone(),
                bitrate: "auto".into(),
                transform: Some(transform),
                batch_size: 1,
                cancel: Some(cancel.clone()),
                frame_format: media::FrameFormat::Nv12,
            },
            |p| {
                let _ = sink.add(ProcessEvent::Progress {
                    frames: p.frames,
                    decoded: p.decoded,
                    written: p.written,
                    total_frames: p.total_frames,
                    fps: p.fps,
                    eta_secs: p.eta_secs,
                });
            },
        );
        match result {
            Ok(stats) => {
                let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Finalizing });
                let _ = sink.add(ProcessEvent::Finished {
                    output: masks_dir.clone(),
                    frames: stats.frames,
                    elapsed_secs: t0.elapsed().as_secs_f64(),
                });
                return Ok(());
            }
            Err(pipe::PipelineError::Cancelled { frames }) => {
                let _ = sink.add(ProcessEvent::Cancelled { frames });
                return Ok(());
            }
            Err(pipe::PipelineError::EncoderFailed { .. }) if i + 1 < encoders.len() => {
                last_err = format!("编码器 {enc} 运行不可用");
                let _ = sink.add(ProcessEvent::Log {
                    line: format!("[回退] {last_err}，尝试 {} …", encoders[i + 1]),
                });
                continue;
            }
            Err(e) => return fail(e.to_string()),
        }
    }
    fail(last_err)
}

/// Archive 渲染：读 mask 缓存 + 复核补丁，纯合成+编码（无推理）。
pub fn archive_render(
    input: String,
    masks_dir: String,
    output: String,
    style: String,
    strength: u32,
    hwaccel: String,
    encoder: String,
    bitrate: String,
    sink: StreamSink<ProcessEvent>,
) -> Result<(), String> {
    use automosaic_core::maskstore::MaskStore;

    let cancel = cancel_flag();
    cancel.store(false, Ordering::Relaxed);
    let fail = |e: String| -> Result<(), String> {
        let _ = sink.add(ProcessEvent::Failed { error: e.clone() });
        Err(e)
    };
    let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Probing });
    let meta = match media::probe(Path::new(&input)) {
        Ok(m) => m,
        Err(e) => return fail(e.to_string()),
    };
    let (w, h) = (meta.width as usize, meta.height as usize);
    let store = match MaskStore::new(Path::new(&masks_dir)) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    let mmeta = match store.verify(meta.width, meta.height) {
        Ok(m) => m,
        Err(e) => return fail(e.to_string()),
    };
    let mask_style = match parse_style(&style, strength) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let patches = automosaic_core::maskstore::PatchStore::load(Path::new(&masks_dir));
    let _ = sink.add(ProcessEvent::Log {
        line: format!(
            "渲染：缓存 {}/{} 帧，复核补丁 {} 条",
            mmeta.frames,
            meta.total_frames.map(|t| t.to_string()).unwrap_or_else(|| "?".into()),
            patches.patches.len()
        ),
    });
    let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Inferring });
    let hwaccel = resolve_hwaccels(Path::new(&input), &hwaccel, &meta)
        .into_iter()
        .next()
        .flatten();
    let encoders = resolve_encoders(&encoder);
    let probe_out = Path::new(&masks_dir).join("_render_probe.mp4");
    let t0 = std::time::Instant::now();
    let mut last_err = String::new();
    for (i, enc) in encoders.iter().enumerate() {
        let store2 = match MaskStore::new(Path::new(&masks_dir)) {
            Ok(s) => s,
            Err(e) => return fail(e.to_string()),
        };
        let patches = patches.clone();
        let style2 = mask_style.clone();
        let mut idx = 0u64;
        let transform: pipe::FrameTransform = Box::new(move |frames: &mut [&mut [u8]]| {
            for frame in frames.iter_mut() {
                match store2.load_mask(idx, w, h) {
                    Ok(Some(mut mask)) => {
                        patches.apply(idx, &mut mask);
                        automosaic_core::compose::apply(frame, w, h, &mask, &style2);
                    }
                    Ok(None) => {} // 未分析帧原样（渲染段不猜测）
                    Err(e) => return Err(e.to_string()),
                }
                idx += 1;
            }
            Ok(())
        });
        let result = pipe::run(
            Path::new(&input),
            Path::new(&output),
            PipelineOptions {
                hwaccel: hwaccel.clone(),
                encoder: enc.clone(),
                bitrate: bitrate.clone(),
                transform: Some(transform),
                batch_size: 1,
                cancel: Some(cancel.clone()),
                frame_format: media::FrameFormat::Nv12,
            },
            |p| {
                let _ = sink.add(ProcessEvent::Progress {
                    frames: p.frames,
                    decoded: p.decoded,
                    written: p.written,
                    total_frames: p.total_frames,
                    fps: p.fps,
                    eta_secs: p.eta_secs,
                });
            },
        );
        let _ = std::fs::remove_file(&probe_out);
        match result {
            Ok(stats) => {
                let _ = sink.add(ProcessEvent::StageEnter { stage: ProcessStage::Finalizing });
                let _ = sink.add(ProcessEvent::Finished {
                    output: output.clone(),
                    frames: stats.frames,
                    elapsed_secs: t0.elapsed().as_secs_f64(),
                });
                return Ok(());
            }
            Err(pipe::PipelineError::Cancelled { frames }) => {
                let _ = sink.add(ProcessEvent::Cancelled { frames });
                return Ok(());
            }
            Err(pipe::PipelineError::EncoderFailed { .. }) if i + 1 < encoders.len() => {
                last_err = format!("编码器 {enc} 运行不可用");
                continue;
            }
            Err(e) => return fail(e.to_string()),
        }
    }
    fail(last_err)
}

// --------------------------------------------------------------------------- //
// 复核 UI API（关键帧刷子/加减点 → SAM 重提示 → 补丁落盘）
// --------------------------------------------------------------------------- //

/// 复核帧：原帧缩放 RGBA + 生效 mask（含补丁）平面 + 实例框列表。
pub struct ReviewFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// W×H，1=遮罩（缓存 mask + 补丁后）。
    pub mask: Vec<u8>,
    /// 该帧的 masklet 实例（id/kind/score/框）。
    pub instances: Vec<ReviewInstance>,
}

/// 复核实例条目。
pub struct ReviewInstance {
    pub id: u64,
    /// 0=person，1=孤立人脸。
    pub kind: u8,
    pub score: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// 取复核帧（frame_idx 处解码原帧 + 缓存 mask + 已存补丁 + 实例层）。
pub fn review_frame(
    input: String,
    masks_dir: String,
    frame_idx: u64,
) -> Result<ReviewFrame, String> {
    use automosaic_core::maskstore::MaskStore;

    let meta = media::probe(Path::new(&input)).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let t = frame_idx as f64 / meta.fps.max(0.001);
    let nv12 = media::decode_frame_at(Path::new(&input), t, &meta).map_err(|e| e.to_string())?;
    // 缩放到 720 宽供 UI（全分辨率 RGBA 传输过重）
    let (dw, dh) = mosaic::preview_size(w, h);
    let rgba = media::nv12_to_rgba_scaled(&nv12, w, h, dw, dh);
    let store = MaskStore::new(Path::new(&masks_dir)).map_err(|e| e.to_string())?;
    let mut mask = store.load_mask(frame_idx, w, h).map_err(|e| e.to_string())?.unwrap_or_else(|| vec![0u8; w * h]);
    let patches = automosaic_core::maskstore::PatchStore::load(Path::new(&masks_dir));
    patches.apply(frame_idx, &mut mask);
    let instances = store
        .load_instances(frame_idx, w, h)
        .map_err(|e| e.to_string())?
        .unwrap_or_default()
        .into_iter()
        .map(|r| ReviewInstance {
            id: r.id,
            kind: r.kind,
            score: r.score,
            x1: r.xyxy[0],
            y1: r.xyxy[1],
            x2: r.xyxy[2],
            y2: r.xyxy[3],
        })
        .collect();
    Ok(ReviewFrame { rgba, width: dw as u32, height: dh as u32, mask, instances })
}

/// 复核缓存元信息。
pub struct ReviewMeta {
    pub width: u32,
    pub height: u32,
    /// 已分析帧数（断点续跑进度/时间轴范围）。
    pub frames: u64,
    /// 视频总帧数。
    pub total_frames: Option<u64>,
    pub fps: f64,
    /// 已存补丁条数。
    pub patches: u32,
}

/// 取复核元信息（时间轴/进度用）。
pub fn review_meta(input: String, masks_dir: String) -> Result<ReviewMeta, String> {
    use automosaic_core::maskstore::MaskStore;

    let meta = media::probe(Path::new(&input)).map_err(|e| e.to_string())?;
    let store = MaskStore::new(Path::new(&masks_dir)).map_err(|e| e.to_string())?;
    let m = store.load_meta().map_err(|e| e.to_string())?;
    let patches = automosaic_core::maskstore::PatchStore::load(Path::new(&masks_dir));
    Ok(ReviewMeta {
        width: m.width,
        height: m.height,
        frames: m.frames,
        total_frames: meta.total_frames,
        fps: meta.fps,
        patches: patches.patches.len() as u32,
    })
}

/// SAM 点提示缓存：同帧迭代加减点时复用 encoder 嵌入（换帧才重编码）。
static SAM_CACHE: OnceLock<Mutex<Option<SamCache>>> = OnceLock::new();

struct SamCache {
    video: String,
    frame_idx: u64,
    sam: automosaic_core::sam2::Sam2,
    w: usize,
    h: usize,
}

/// 点提示 SAM 重提示（复核 UI 加/减点）：
/// `points` 为 [x, y, label]×N 扁平（label 1=前景 0=背景，全帧像素坐标），
/// 可选 `box`（[x1,y1,x2,y2]）。返回重提示 mask + IoU。
pub fn review_sam_prompt(
    input: String,
    frame_idx: u64,
    points: Vec<f32>,
    box_: Option<Vec<f32>>,
    sam_size: String,
) -> Result<SamPromptResult, String> {
    use automosaic_core::sam2::{PointPrompt, Sam2};

    let meta = media::probe(Path::new(&input)).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let cache = SAM_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    let need_reload = guard
        .as_ref()
        .map_or(true, |c| c.video != input || c.frame_idx != frame_idx);
    if need_reload {
        let (enc, dec) = match sam_size.as_str() {
            "tiny" => ("sam2.1-tiny-encoder.onnx", "sam2.1-tiny-decoder.onnx"),
            "large" => ("sam2.1-large-encoder.onnx", "sam2.1-large-decoder.onnx"),
            s => return Err(format!("未知 SAM 规格 {s}")),
        };
        let enc = model_store::resolve_model(enc);
        let dec = model_store::resolve_model(dec);
        if !enc.exists() || !dec.exists() {
            return Err(format!("缺少 SAM 模型（{}）", enc.display()));
        }
        let t = frame_idx as f64 / meta.fps.max(0.001);
        let nv12 = media::decode_frame_at(Path::new(&input), t, &meta).map_err(|e| e.to_string())?;
        let mut sam = Sam2::load(&enc, &dec, "cpu").map_err(|e| e.to_string())?;
        sam.set_frame(&nv12, w, h).map_err(|e| e.to_string())?;
        *guard = Some(SamCache { video: input, frame_idx, sam, w, h });
    }
    let c = guard.as_mut().expect("刚写入");
    let mut prompts: Vec<PointPrompt> = Vec::new();
    for chunk in points.chunks(3) {
        if chunk.len() == 3 {
            prompts.push(PointPrompt { x: chunk[0], y: chunk[1], label: chunk[2] as i32 });
        }
    }
    let bbox = box_.and_then(|b| {
        if b.len() == 4 { Some([b[0], b[1], b[2], b[3]]) } else { None }
    });
    let (mask, iou) = c
        .sam
        .prompt_points(&prompts, bbox, c.w, c.h)
        .map_err(|e| e.to_string())?;
    Ok(SamPromptResult { mask, iou })
}

/// SAM 重提示结果。
pub struct SamPromptResult {
    pub mask: Vec<u8>,
    pub iou: f32,
}

/// 保存笔刷补丁（frame 帧的 add/erase 区域平面，W×H 0/1）。
pub fn review_save_brush(
    masks_dir: String,
    frame_idx: u64,
    add: bool,
    mask: Vec<u8>,
) -> Result<(), String> {
    let mut ps = automosaic_core::maskstore::PatchStore::load(Path::new(&masks_dir));
    ps.push(
        Path::new(&masks_dir),
        automosaic_core::maskstore::Patch {
            frame: frame_idx,
            op: if add {
                automosaic_core::maskstore::PatchOp::Add
            } else {
                automosaic_core::maskstore::PatchOp::Erase
            },
            mask,
        },
    )
    .map_err(|e| e.to_string())
}

/// 撤销指定帧的全部补丁。
pub fn review_clear_frame(masks_dir: String, frame_idx: u64) -> Result<(), String> {
    let mut ps = automosaic_core::maskstore::PatchStore::load(Path::new(&masks_dir));
    ps.clear_frame(Path::new(&masks_dir), frame_idx).map_err(|e| e.to_string())
}
