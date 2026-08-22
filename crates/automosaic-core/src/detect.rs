//! YOLO-seg（ONNX/ort）推理与后处理（DESIGN §4.4，M1）。
//!
//! - 预处理：NV12 → letterbox(S) CHW f32，单遍融合采样（BT.601 + 双线性 Y + 最近邻 UV），
//!   不经过全帧 RGB 中间表示。
//! - 后处理：输出解码（export 已含 DFL/sigmoid，box 为 S 像素坐标）、person 类过滤、
//!   NMS、proto×coeffs 的 mask 重建（先在 P 空间二值化再最近邻放大，比逐像素放大省 ~15× 算力）。
//!
//! 模型布局（加载时按 session 形状自动识别，见 [`ModelIo`]）：
//! - yolo11-seg（anchor 铺排）：input `images [1,3,640,640]`；
//!   output0 `[1,116,8400]`（4 box + 80 cls + 32 mask coeffs）；output1 proto `[1,32,160,160]`
//! - yolo26（e2e 免 NMS）：output0 `[1,300,4+1+1(+32)]` = xyxy + score + class + coeffs；
//!   检测-only 变体无 output1（mask 用检测框 + margin）

use std::path::{Path, PathBuf};

use rayon::prelude::*;

/// CoreML 编译产物缓存目录（持久化，避免每 session 首载 ~3s 的编译）。
/// 按设备分目录：计算单元不同的编译产物互不覆盖。
#[cfg(target_os = "macos")]
fn coreml_cache_dir(device: &str) -> PathBuf {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache/automosaic/coreml")
        .join(device);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// device 字符串 → CoreML 计算单元。
/// auto = 全单元（CoreML 按算子分段自行调度 CPU/GPU/ANE）；
/// gpu = 仅 CPU+GPU；ane = 仅 CPU+NPU；cpu = 不启用 CoreML EP。
#[cfg(target_os = "macos")]
fn coreml_compute_units(device: &str) -> ort::ep::coreml::ComputeUnits {
    use ort::ep::coreml::ComputeUnits;
    match device {
        "gpu" => ComputeUnits::CPUAndGPU,
        "ane" => ComputeUnits::CPUAndNeuralEngine,
        _ => ComputeUnits::All,
    }
}

/// 推理后端的人读描述（UI 展示用；CoreML 内部调度无法逐算子查询，故描述配置而非实测）。
pub fn backend_desc(device: &str) -> String {
    #[cfg(target_os = "macos")]
    if device != "cpu" {
        let units = match device {
            "gpu" => "CPU+GPU",
            "ane" => "CPU+NPU",
            _ => "CPU/GPU/NPU 自动调度",
        };
        return format!("CoreML（{units}）");
    }
    // Linux：auto 与显式 webgpu 同径（2026-08-21 起 auto 默认 WebGPU）
    #[cfg(target_os = "linux")]
    if device != "cpu" && device != "openvino" {
        return "WebGPU（Dawn/Vulkan，实验）".into();
    }
    if device == "webgpu" {
        return "WebGPU（Dawn/Vulkan，实验）".into();
    }
    if device == "directml" {
        return "DirectML（DX12）".into();
    }
    if device == "openvino" {
        return "OpenVINO（Intel CPU/GPU）".into();
    }
    let _ = device;
    "CPU（ONNX Runtime）".to_string()
}

/// 构建带 CoreML 编译缓存与指定计算单元的 session（macOS 非 CPU 设备用）。
fn commit_session(device: &str, model: &Path) -> Result<ort::session::Session, DetectError> {
    let mut b = ort::session::Session::builder()?;
    #[cfg(target_os = "macos")]
    if device != "cpu" {
        b = b
            .with_execution_providers(
                [ort::ep::CoreML::default()
                    .with_compute_units(coreml_compute_units(device))
                    .with_model_cache_dir(coreml_cache_dir(device).display().to_string())
                    .build()],
            )
            .unwrap_or_else(|e| e.recover());
    }
    // WebGPU EP（Dawn/Vulkan）：Linux 推理加速出口（DESIGN §4.2）。auto 默认
    // 启用（2026-08-21 起，与 macOS=CoreML/Windows=DirectML 的 auto 语义对齐；
    // 极限档 SAM2.1-large 在 CPU 上 ~7.8s/帧，默认 CPU 不再合理）；EP 初始化
    // 失败由 ort 自动落 CPU（"永远能跑"），cpu 显式退出，openvino 走专用臂。
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
    // DirectML EP（Windows 任意 DX12 GPU，N/A/I 全系覆盖且零驱动依赖，DESIGN
    // §4.2 Windows 默认）：auto 与显式 directml 均走此臂。EP 初始化失败由
    // ort 自动落 CPU（"永远能跑"）。
    #[cfg(target_os = "windows")]
    if device != "cpu" {
        b = b
            .with_execution_providers([ort::ep::DirectML::default().build()])
            .unwrap_or_else(|e| e.recover());
    }
    // OpenVINO EP（Intel CPU/GPU/NPU；DESIGN §4.2 可选项）：显式 --device
    // openvino 才启用（默认 CPU）。pyke 预编译运行时无 openvino+webgpu 组合
    // 构建，故默认关闭、经 cargo feature `ort-openvino` 编译期启用（见
    // core Cargo.toml）——未启用时本臂不编译，openvino 等价 CPU。
    #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "ort-openvino"))]
    if device == "openvino" {
        b = b
            .with_execution_providers([ort::ep::OpenVINO::default().build()])
            .unwrap_or_else(|e| e.recover());
    }
    Ok(b.commit_from_file(model)?)
}

/// 默认模型输入边长（yolo11 系与人脸线导出尺寸）。
pub const INPUT_SIZE: usize = 640;
/// yolo11-seg anchor 铺排输出行数（4 box + 80 cls + 32 mask coeffs）。
const ROWS: usize = 116;
const PROTO_DIM: usize = 32;
const COCO_PERSON: usize = 0;
const NMS_IOU: f32 = 0.45;
const MAX_DET: usize = 300;
const MASK_THR: f32 = 0.5;
/// 无 mask 头模型（yolo26 检测-only）的检测框外扩比例。
const BOX_MARGIN: f32 = 0.05;

/// output0 的铺排方式（按形状识别：anchor `[b,行,锚数]` 行数 ≪ 锚数；
/// e2e `[b,topk,行]` topk ≫ 行数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputLayout {
    /// yolo11 系铺排：cxycwh + 逐类分数，需 NMS。
    Anchor,
    /// yolo26 e2e 免 NMS：xyxy + score + class (+ mask coeffs)。
    E2E,
}

/// 从 session 形状探测出的模型 IO 参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelIo {
    pub layout: OutputLayout,
    /// 输入边长 S（letterbox 目标）。
    pub input_size: usize,
    /// proto 边长 P；None = 检测-only 模型（无 output1，mask 用框+margin）。
    pub proto_size: Option<usize>,
}

/// 探测 session 的输入/输出形状 → [`ModelIo`]。
fn probe_io(session: &ort::session::Session) -> Result<ModelIo, DetectError> {
    let dims_of = |o: &ort::value::Outlet| -> Option<Vec<i64>> {
        let shape = o.dtype().tensor_shape()?;
        Some(shape.iter().copied().collect())
    };
    let in_dims = session
        .inputs()
        .first()
        .and_then(|o| dims_of(o))
        .ok_or(DetectError::BadModel)?;
    if in_dims.len() != 4 || in_dims[1] != 3 || in_dims[2] <= 0 || in_dims[2] != in_dims[3] {
        return Err(DetectError::BadModel);
    }
    let input_size = in_dims[2] as usize;

    let out0 = session
        .outputs()
        .iter()
        .find(|o| o.name() == "output0")
        .and_then(|o| dims_of(o))
        .ok_or(DetectError::BadModel)?;
    if out0.len() != 3 || out0[1] <= 0 || out0[2] <= 0 {
        return Err(DetectError::BadModel);
    }
    let layout = if out0[1] > out0[2] { OutputLayout::E2E } else { OutputLayout::Anchor };

    let proto_size = session
        .outputs()
        .iter()
        .find(|o| o.name() == "output1")
        .and_then(|o| dims_of(o))
        .filter(|d| d.len() == 4 && d[1] == PROTO_DIM as i64 && d[2] == d[3] && d[2] > 0)
        .map(|d| d[2] as usize);

    Ok(ModelIo { layout, input_size, proto_size })
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("模型文件不存在: {0}（可用 scripts/export_models.sh 生成）")]
    ModelNotFound(PathBuf),
    #[error("ort 错误: {0}")]
    Ort(#[from] ort::Error),
    #[error("模型输入/输出形状无法识别（期望 images [N,3,S,S] + output0/output1）")]
    BadModel,
    #[error("模型输出形状异常：output0 期望 [1,{ROWS},N]，实际 {actual:?}")]
    BadOutput { actual: Vec<i64> },
}

#[derive(Debug, Clone)]
pub struct Detection {
    /// letterbox(640) 空间的 xyxy。
    pub xyxy: [f32; 4],
    pub score: f32,
    coeffs: [f32; PROTO_DIM],
}

pub struct Detector {
    session: ort::session::Session,
    /// 固定 batch=N 的第二 session（CoreML 对动态 batch 支持差，用固定形状规避）。
    batch: Option<(usize, ort::session::Session)>,
    pub conf: f32,
    /// 低分下限（ByteTrack 二段关联用）：设为 Some(lo) 时解码返回 score ≥ lo
    /// 的全部检测（含 conf 以下），由跟踪器按 conf 切分两段。None = 仅 ≥ conf。
    pub low_conf: Option<f32>,
    /// 翻转 TTA（DESIGN §6 精度 #7）：每帧额外跑一次水平翻转推理，两趟结果
    /// 按分数贪心 NMS 合并（+0.3~0.8 AP 召回，推理 ×2，离线档开）。
    pub tta: bool,
    io: ModelIo,
    /// 加载设备（批 session 继承同设备与计算单元）。
    device: String,
}

/// 单个 person 实例：frame 坐标框 + 全分辨率 mask（跟踪/漏检补偿需要按 ID 持有）。
#[derive(Clone)]
pub struct PersonInstance {
    pub score: f32,
    /// 原始分辨率 xyxy。
    pub xyxy: [f32; 4],
    pub mask: Vec<u8>,
}

// --------------------------------------------------------------------------- //
// 后端抽象（DESIGN §4.3 DetectorBackend）：管线层（mosaic/CLI/FFI）只依赖
// trait，具体推理引擎（ort 系 CoreML/WebGPU/DirectML/OpenVINO/CPU；未来的
// tract 纯 Rust 兜底、ncnn Vulkan）各自实现，接入零管线改动。
// NV12 进 / 实例出——letterbox 预处理与后处理属于后端实现细节。
// --------------------------------------------------------------------------- //

/// 人体检测后端。
pub trait DetectorBackend: Send {
    /// 后端名（"coreml"/"webgpu"/"directml"/"openvino"/"cpu"…）。
    fn backend_name(&self) -> &str;
    /// 批量检测 person 实例（框为原始分辨率坐标，mask 为逐实例 W×H）。
    fn detect_person_instances_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<PersonInstance>>, String>;
    /// 自适应降档第一档（DESIGN §6.8"batch 8→2"）：撤销固定批 session、
    /// 回退逐帧推理（省显存与延迟尖峰，吞吐略降）。返回是否实际降档；
    /// 无批概念的后端默认无操作。
    fn try_reduce_batch(&mut self) -> bool {
        false
    }
}

impl DetectorBackend for Detector {
    fn backend_name(&self) -> &str {
        &self.device
    }

    fn detect_person_instances_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<PersonInstance>>, String> {
        Detector::detect_person_instances_batch(self, frames, w, h).map_err(|e| e.to_string())
    }

    fn try_reduce_batch(&mut self) -> bool {
        if self.batch.is_some() {
            self.batch = None;
            true
        } else {
            false
        }
    }
}

/// 人脸检测后端。
pub trait FaceDetectorBackend: Send {
    fn backend_name(&self) -> &str;
    /// 批量检测人脸框（原始分辨率坐标）。
    fn detect_boxes_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<FaceBox>>, String>;
    /// 级联 ROI 人脸检测（框已映射回全帧坐标）。
    fn detect_boxes_roi(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
        roi: [f32; 4],
    ) -> Result<Vec<FaceBox>, String>;
}

impl FaceDetectorBackend for FaceDetector {
    fn backend_name(&self) -> &str {
        &self.device
    }

    fn detect_boxes_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<FaceBox>>, String> {
        FaceDetector::detect_boxes_batch(self, frames, w, h).map_err(|e| e.to_string())
    }

    fn detect_boxes_roi(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
        roi: [f32; 4],
    ) -> Result<Vec<FaceBox>, String> {
        FaceDetector::detect_boxes_roi(self, nv12, w, h, roi).map_err(|e| e.to_string())
    }
}

impl Detector {
    /// 加载模型。device: "auto"（macOS=CoreML 全单元 / Windows=DirectML /
    /// Linux=WebGPU，2026-08-21 起三平台 auto 均默认平台 EP）
    /// /"gpu"（CPU+GPU）/"ane"（CPU+NPU）/"cpu"；Windows 另有 "directml"；
    /// Linux x86_64 另有 "webgpu"（显式，与 auto 同径）与 "openvino"（Intel）。
    /// EP 初始化失败时 ort 自动落到 CPU EP（"永远能跑"）。
    pub fn load(model: &Path, device: &str, conf: f32) -> Result<Self, DetectError> {
        if !model.exists() {
            return Err(DetectError::ModelNotFound(model.to_path_buf()));
        }
        let session = commit_session(device, model)?;
        let io = probe_io(&session)?;
        Ok(Self { session, batch: None, conf, low_conf: None, tta: false, io, device: device.to_string() })
    }

    /// 加载固定 batch=N 的第二模型；`frames.len() == N` 时走批量 session。
    pub fn enable_batch(&mut self, model: &Path, n: usize) -> Result<(), DetectError> {
        if !model.exists() {
            return Err(DetectError::ModelNotFound(model.to_path_buf()));
        }
        let session = commit_session(&self.device, model)?;
        if probe_io(&session)? != self.io {
            return Err(DetectError::BadModel);
        }
        self.batch = Some((n, session));
        Ok(())
    }

    pub fn batch_n(&self) -> Option<usize> {
        self.batch.as_ref().map(|(n, _)| *n)
    }

    /// 对一帧 NV12 检测 person 实例（框为原始分辨率坐标，mask 为逐实例）。
    pub fn detect_person_instances(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
    ) -> Result<Vec<PersonInstance>, DetectError> {
        Ok(self
            .detect_person_instances_batch(&[nv12], w, h)?
            .pop()
            .unwrap_or_default())
    }

    /// 批量检测：`frames.len()` 恰好等于 batch session 的 N 时走批量推理，
    /// 否则逐帧走 batch=1 主 session。`tta` 开启时翻转趟复用同一 session。
    pub fn detect_person_instances_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<PersonInstance>>, DetectError> {
        // 解码过滤下限：low_conf 生效时返回 [lo, conf) 的低分检测供 BYTE 二段救援
        let conf = self.low_conf.map_or(self.conf, |lo| lo.min(self.conf));
        let io = self.io;
        let mut run = |flip: bool| -> Result<Vec<Vec<PersonInstance>>, DetectError> {
            if let Some((n, sess)) = self.batch.as_mut()
                && *n == frames.len()
            {
                return infer_person_session(sess, io, conf, frames, w, h, flip);
            }
            let mut out = Vec::with_capacity(frames.len());
            for f in frames {
                out.push(
                    infer_person_session(&mut self.session, io, conf, &[f], w, h, flip)?
                        .pop()
                        .unwrap_or_default(),
                );
            }
            Ok(out)
        };
        let mut out = run(false)?;
        if self.tta {
            let flipped = run(true)?;
            for (o, f) in out.iter_mut().zip(flipped) {
                *o = merge_instances(std::mem::take(o), f);
            }
        }
        Ok(out)
    }

    /// 模型 IO 参数（输入尺寸/布局，暴露给上层展示与缓存 key）。
    pub fn io(&self) -> ModelIo {
        self.io
    }

    /// 对一帧 NV12 检测 person，返回合并 mask（W×H，1=遮罩区域）。
    pub fn detect_person_mask(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
    ) -> Result<Vec<u8>, DetectError> {
        let instances = self.detect_person_instances(nv12, w, h)?;
        let mut out = vec![0u8; w * h];
        for inst in &instances {
            for (o, m) in out.iter_mut().zip(&inst.mask) {
                *o |= *m;
            }
        }
        Ok(out)
    }
}

/// 在指定 session 上跑一批帧（session 的固定 batch 必须等于 frames.len()）。
/// `flip` = TTA 翻转趟：输入按源列镜像采样，输出（框/mask）再镜像回真实坐标。
fn infer_person_session(
    session: &mut ort::session::Session,
    io: ModelIo,
    conf: f32,
    frames: &[&[u8]],
    w: usize,
    h: usize,
    flip: bool,
) -> Result<Vec<Vec<PersonInstance>>, DetectError> {
    let size = io.input_size;
    let b = frames.len();
    // letterbox 是 CPU 热点（1080p→640 每帧 ~3-5ms），批内按帧并行
    let chunks: Vec<Vec<f32>> = frames
        .par_iter()
        .map(|f| nv12_to_letterbox_chw(f, w, h, size, flip))
        .collect();
    let mut input = Vec::with_capacity(b * 3 * size * size);
    for c in &chunks {
        input.extend_from_slice(c);
    }
    let outputs = session.run(ort::inputs! {
        "images" => ort::value::Tensor::from_array((
            [b as i64, 3, size as i64, size as i64],
            input,
        ))?,
    })?;
    let (s0, o0) = outputs["output0"].try_extract_tensor::<f32>()?;
    if s0.len() != 3 || s0[0] != b as i64 {
        return Err(DetectError::BadOutput { actual: s0.to_vec() });
    }
    let (stride, rows) = match io.layout {
        OutputLayout::Anchor => {
            if s0[1] != ROWS as i64 {
                return Err(DetectError::BadOutput { actual: s0.to_vec() });
            }
            (ROWS, s0[2] as usize) // 铺排：o0 按行主序，单帧占 ROWS*n
        }
        OutputLayout::E2E => {
            if s0[2] < 6 || s0[2] > 6 + PROTO_DIM as i64 {
                return Err(DetectError::BadOutput { actual: s0.to_vec() });
            }
            (s0[2] as usize, s0[1] as usize) // e2e：行主序 topk 行，单帧占 topk*stride
        }
    };

    let (proto_per, proto_t) = match io.proto_size {
        Some(psize) => {
            let (s1, t) = outputs["output1"].try_extract_tensor::<f32>()?;
            if s1.len() != 4
                || s1[0] != b as i64
                || s1[1] != PROTO_DIM as i64
                || s1[2] != psize as i64
            {
                return Err(DetectError::BadOutput { actual: s1.to_vec() });
            }
            (PROTO_DIM * psize * psize, Some(t))
        }
        None => (0, None),
    };

    let frame0 = stride * rows;
    let (scale, pad_x, pad_y, new_w, new_h) = letterbox_params(w, h, size);

    let mut out = Vec::with_capacity(b);
    for i in 0..b {
        let dets = match io.layout {
            OutputLayout::Anchor => nms(
                decode_person_anchor(&o0[i * frame0..], rows, conf)?,
                NMS_IOU,
                MAX_DET,
            ),
            OutputLayout::E2E => decode_person_e2e(&o0[i * frame0..], rows, stride, conf),
        };
        // mask 解码（proto×coeffs + 上采样 + 反 letterbox）按检测并行
        let instances: Vec<PersonInstance> = dets
            .par_iter()
            .map(|det| {
                let xyxy = unletterbox_box(det.xyxy, scale, pad_x, pad_y, w, h, flip);
                let mask = match (proto_t, io.proto_size) {
                    (Some(t), Some(psize)) => {
                        let ms = det_mask(det, &t[i * proto_per..], size, psize);
                        let mut mask = vec![0u8; w * h];
                        unletterbox_into(&ms, scale, pad_x, pad_y, new_w, new_h, size, w, h, flip, &mut mask);
                        mask
                    }
                    // 检测-only：框 + 5% margin 的矩形遮罩
                    _ => rect_mask(xyxy, BOX_MARGIN, w, h),
                };
                PersonInstance { score: det.score, xyxy, mask }
            })
            .collect();
        out.push(instances);
    }
    Ok(out)
}

/// TTA 两趟（原图 + 翻转）结果合并：按分数降序贪心 NMS 去重，
/// 保留高分假设的框与 mask（同目标两趟框几乎重合，IoU ≫ 阈值）。
fn merge_instances(a: Vec<PersonInstance>, b: Vec<PersonInstance>) -> Vec<PersonInstance> {
    let mut all = a;
    all.extend(b);
    all.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<PersonInstance> = Vec::new();
    for inst in all {
        if kept.len() >= MAX_DET {
            break;
        }
        if kept.iter().all(|k| box_iou(&k.xyxy, &inst.xyxy) <= NMS_IOU) {
            kept.push(inst);
        }
    }
    kept
}

/// letterbox 空间 box → 原始分辨率后按比例外扩的实心矩形 mask。
fn rect_mask(xyxy: [f32; 4], margin: f32, w: usize, h: usize) -> Vec<u8> {
    let (mw, mh) = ((xyxy[2] - xyxy[0]) * margin, (xyxy[3] - xyxy[1]) * margin);
    let x1 = (xyxy[0] - mw).max(0.0) as usize;
    let y1 = (xyxy[1] - mh).max(0.0) as usize;
    let x2 = ((xyxy[2] + mw).ceil() as usize).min(w).max(x1 + 1);
    let y2 = ((xyxy[3] + mh).ceil() as usize).min(h).max(y1 + 1);
    let mut mask = vec![0u8; w * h];
    for y in y1..y2 {
        mask[y * w + x1..y * w + x2].fill(1);
    }
    mask
}

// --------------------------------------------------------------------------- //
// 预处理
// --------------------------------------------------------------------------- //

/// letterbox 参数（与 ultralytics 对齐：等比缩放居中，pad 用整数除法分配）。
/// 返回 (scale, pad_x, pad_y, new_w, new_h)；有效区域为
/// [pad_x, pad_x+new_w) × [pad_y, pad_y+new_h)。
pub fn letterbox_params(w: usize, h: usize, size: usize) -> (f32, usize, usize, usize, usize) {
    let scale = size as f32 / w.max(h) as f32;
    let new_w = (w as f32 * scale).round() as usize;
    let new_h = (h as f32 * scale).round() as usize;
    (scale, (size - new_w) / 2, (size - new_h) / 2, new_w, new_h)
}

/// NV12 → letterbox(size) CHW f32（/255）。单遍融合：逐输出像素双线性采 Y、
/// 最近邻采 UV，BT.601 limited 转 RGB；letterbox 填充 114。
/// `flip` = 源图水平镜像（TTA 翻转趟；填充对称，等价于翻转后再 letterbox）。
pub fn nv12_to_letterbox_chw(
    nv12: &[u8],
    w: usize,
    h: usize,
    size: usize,
    flip: bool,
) -> Vec<f32> {
    let (scale, pad_x, pad_y, new_w, new_h) = letterbox_params(w, h, size);
    let plane = size * size;
    let mut out = vec![114.0f32 / 255.0; 3 * plane];
    let y_plane = &nv12[..w * h];
    let uv = &nv12[w * h..];
    let (chw, chh) = (w / 2, h / 2);

    for oy in 0..new_h {
        // 源坐标（像素中心对齐）
        let sy = (oy as f32 + 0.5) / scale - 0.5;
        let y0 = sy.floor().max(0.0) as usize;
        let iy0 = y0.min(h - 1);
        let iy1 = (y0 + 1).min(h - 1);
        let fy = (sy - y0 as f32).clamp(0.0, 1.0);
        let cy = (((sy + 0.5) * 0.5) as usize).min(chh - 1);

        for ox in 0..new_w {
            let mut sx = (ox as f32 + 0.5) / scale - 0.5;
            if flip {
                sx = w as f32 - 1.0 - sx;
            }
            let x0 = sx.floor().max(0.0) as usize;
            let ix0 = x0.min(w - 1);
            let ix1 = (x0 + 1).min(w - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);
            let cx = (((sx + 0.5) * 0.5) as usize).min(chw - 1);

            // Y 双线性
            let y00 = y_plane[iy0 * w + ix0] as f32;
            let y10 = y_plane[iy0 * w + ix1] as f32;
            let y01 = y_plane[iy1 * w + ix0] as f32;
            let y11 = y_plane[iy1 * w + ix1] as f32;
            let yv = y00 * (1.0 - fx) * (1.0 - fy)
                + y10 * fx * (1.0 - fy)
                + y01 * (1.0 - fx) * fy
                + y11 * fx * fy;

            // UV 最近邻
            let u = uv[cy * w + cx * 2] as f32;
            let v = uv[cy * w + cx * 2 + 1] as f32;

            // BT.601 limited range → RGB
            let yy = (yv - 16.0) * 1.1644;
            let r = (yy + 1.5960 * (v - 128.0)).clamp(0.0, 255.0);
            let g = (yy - 0.3917 * (u - 128.0) - 0.8130 * (v - 128.0)).clamp(0.0, 255.0);
            let b = (yy + 2.0172 * (u - 128.0)).clamp(0.0, 255.0);

            let dst = (oy + pad_y) * size + (ox + pad_x);
            out[dst] = r / 255.0;
            out[plane + dst] = g / 255.0;
            out[2 * plane + dst] = b / 255.0;
        }
    }
    out
}

// --------------------------------------------------------------------------- //
// 后处理
// --------------------------------------------------------------------------- //

/// output0 [1,116,N] → person Detection 列表（box 已是输入空间 xyxy）。
#[cfg(test)]
fn decode_person(o0: &[f32], shape: &[i64], conf: f32) -> Result<Vec<Detection>, DetectError> {
    if shape.len() != 3 || shape[0] != 1 || shape[1] != ROWS as i64 {
        return Err(DetectError::BadOutput {
            actual: shape.to_vec(),
        });
    }
    decode_person_anchor(o0, shape[2] as usize, conf)
}

/// anchor 铺排单帧切片解码：o0 为该帧的 ROWS×n 连续数据。
fn decode_person_anchor(o0: &[f32], n: usize, conf: f32) -> Result<Vec<Detection>, DetectError> {
    // 批量路径传入的是开区间切片（&o0[i*frame0..]），长度 ≥ 单帧即可；
    // 只读前 ROWS*n 个元素
    debug_assert!(o0.len() >= ROWS * n, "切片不足一帧: {} < {}", o0.len(), ROWS * n);
    let mut dets = Vec::new();
    for c in 0..n {
        let score = o0[(4 + COCO_PERSON) * n + c];
        if score < conf {
            continue;
        }
        let cx = o0[c];
        let cy = o0[n + c];
        let bw = o0[2 * n + c];
        let bh = o0[3 * n + c];
        let mut coeffs = [0f32; PROTO_DIM];
        for (k, co) in coeffs.iter_mut().enumerate() {
            *co = o0[(84 + k) * n + c];
        }
        dets.push(Detection {
            xyxy: [cx - bw * 0.5, cy - bh * 0.5, cx + bw * 0.5, cy + bh * 0.5],
            score,
            coeffs,
        });
    }
    Ok(dets)
}

/// e2e 单帧切片解码：topk 行 × stride 列（stride = 6 检测-only / 38 seg），
/// 每行 [x1,y1,x2,y2,score,class, coeffs?]。导出头已去重，免 NMS。
fn decode_person_e2e(o0: &[f32], rows: usize, stride: usize, conf: f32) -> Vec<Detection> {
    debug_assert!(o0.len() >= rows * stride);
    let mut dets = Vec::new();
    for r in 0..rows {
        let b = r * stride;
        if o0[b + 5] != COCO_PERSON as f32 {
            continue;
        }
        let score = o0[b + 4];
        if score < conf {
            continue;
        }
        let mut coeffs = [0f32; PROTO_DIM];
        if stride > 6 {
            coeffs[..stride - 6].copy_from_slice(&o0[b + 6..b + stride]);
        }
        dets.push(Detection {
            xyxy: [o0[b], o0[b + 1], o0[b + 2], o0[b + 3]],
            score,
            coeffs,
        });
    }
    dets
}

fn box_iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    if area_a + area_b <= 0.0 { 0.0 } else { inter / (area_a + area_b - inter) }
}

/// 按分数降序的贪心 NMS。
fn nms(mut dets: Vec<Detection>, iou_thr: f32, max_det: usize) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<Detection> = Vec::new();
    for det in dets {
        if kept.len() >= max_det {
            break;
        }
        if kept.iter().all(|k| box_iou(&k.xyxy, &det.xyxy) <= iou_thr) {
            kept.push(det);
        }
    }
    kept
}

/// 单个检测的 mask：proto×coeffs → sigmoid → proto 空间二值化（裁剪到 box）
/// → 最近邻放大到 box 的输入空间尺寸 → 落位画布。
fn det_mask(det: &Detection, proto: &[f32], size: usize, psize: usize) -> Vec<u8> {
    let mut canvas = vec![0u8; size * size];
    let [x1, y1, x2, y2] = det.xyxy.map(|v| v.clamp(0.0, size as f32));
    let (x1u, y1u) = (x1 as usize, y1 as usize);
    let (bw, bh) = (
        ((x2 - x1).round() as usize).clamp(1, size - x1u),
        ((y2 - y1).round() as usize).clamp(1, size - y1u),
    );
    if x1u >= size || y1u >= size || bw == 0 || bh == 0 {
        return canvas;
    }

    // box 映射到 proto 空间并裁剪
    let s = psize as f32 / size as f32;
    let bx1 = ((x1 * s).floor() as usize).min(psize - 1);
    let by1 = ((y1 * s).floor() as usize).min(psize - 1);
    let bcw = (((x2 * s).ceil() as usize).saturating_sub(bx1)).clamp(1, psize - bx1);
    let bch = (((y2 * s).ceil() as usize).saturating_sub(by1)).clamp(1, psize - by1);

    // proto 空间二值化 crop
    let mut crop = vec![0u8; bcw * bch];
    for (i, px) in crop.iter_mut().enumerate() {
        let (ly, lx) = (i / bcw, i % bcw);
        let idx = (by1 + ly) * psize + bx1 + lx;
        let mut acc = 0f32;
        for j in 0..PROTO_DIM {
            acc += det.coeffs[j] * proto[j * psize * psize + idx];
        }
        *px = (1.0 / (1.0 + (-acc).exp()) > MASK_THR) as u8;
    }

    // 最近邻放大落位
    for py in 0..bh {
        let sy = py * bch / bh;
        let row = (y1u + py) * size + x1u;
        for px in 0..bw {
            let sx = px * bcw / bw;
            if crop[sy * bcw + sx] == 1 {
                canvas[row + px] = 1;
            }
        }
    }
    canvas
}

/// letterbox(size) 空间 box → 原始分辨率坐标（clamp 到帧内）。
/// `flip` = TTA 翻转趟：映射后再水平镜像（x → w-1-x）。
fn unletterbox_box(
    xyxy: [f32; 4],
    scale: f32,
    pad_x: usize,
    pad_y: usize,
    w: usize,
    h: usize,
    flip: bool,
) -> [f32; 4] {
    let b = [
        ((xyxy[0] - pad_x as f32) / scale).clamp(0.0, w as f32),
        ((xyxy[1] - pad_y as f32) / scale).clamp(0.0, h as f32),
        ((xyxy[2] - pad_x as f32) / scale).clamp(0.0, w as f32),
        ((xyxy[3] - pad_y as f32) / scale).clamp(0.0, h as f32),
    ];
    if !flip {
        b
    } else {
        [
            (w as f32 - 1.0 - b[2]).clamp(0.0, w as f32),
            b[1],
            (w as f32 - 1.0 - b[0]).clamp(0.0, w as f32),
            b[3],
        ]
    }
}

/// mask（letterbox 空间，size×size）最近邻映射回原始分辨率，并入 out（并集）。
/// `flip` = 翻转趟的 mask 落位时水平镜像（写 x → w-1-x）。
#[allow(clippy::too_many_arguments)]
fn unletterbox_into(
    m: &[u8],
    scale: f32,
    pad_x: usize,
    pad_y: usize,
    new_w: usize,
    new_h: usize,
    size: usize,
    w: usize,
    h: usize,
    flip: bool,
    out: &mut [u8],
) {
    for y in 0..h {
        let my = (((y as f32) * scale) as usize).min(new_h - 1) + pad_y;
        for x in 0..w {
            let mx = (((x as f32) * scale) as usize).min(new_w - 1) + pad_x;
            if m[my * size + mx] == 1 {
                let dx = if flip { w - 1 - x } else { x };
                out[y * w + dx] = 1;
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// 人脸检测器（yolov8n-face：output0 [1,20,8400] = 4 box + 1 conf + 5×3 关键点）
// --------------------------------------------------------------------------- //

/// 人脸检测器（yolov8n-face / yolo11n-face-pose：output0 [1,20,8400] =
/// 4 box + 1 conf + 5×3 关键点；YuNet（OpenCV Zoo）：cls/obj/bbox/kps ×
/// stride{8,16,32} 共 12 输出——速度档兜底，75K 参数 CPU 亚毫秒，DESIGN §5.2）。
pub struct FaceDetector {
    session: ort::session::Session,
    batch: Option<(usize, ort::session::Session)>,
    pub conf: f32,
    input_size: usize,
    /// 加载设备（批 session 继承同设备与计算单元）。
    device: String,
    /// 输出布局（按输出名探测：存在 cls_8 即 YuNet）。
    layout: FaceLayout,
    /// 输入张量名（yolo 系 "images"、YuNet "input"）。
    input_name: String,
}

/// 人脸模型输出布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaceLayout {
    /// yolo 系铺排：output0 [1,20,8400]（4 box + 1 conf + 5×3 关键点）。
    YoloAnchor,
    /// YuNet 2023mar：12 输出（cls/obj/bbox/kps × stride 8/16/32），
    /// 固定输入 1×3×640×640、原始 BGR 0..255、零填充。
    YuNet,
}

#[derive(Debug, Clone)]
pub struct FaceBox {
    /// 原始分辨率 xyxy。
    pub xyxy: [f32; 4],
    pub score: f32,
    /// 双眼坐标（原始分辨率；yolo11-face-pose 的 5 点 landmark 前两点，
    /// 置信 < 0.5 或模型无 landmark 时为 None——用于转头场景的眼距自适应外扩）。
    pub eyes: Option<([f32; 2], [f32; 2])>,
}

impl FaceDetector {
    /// 加载人脸模型（yolo 系或 YuNet，按输出名自动识别）。device 语义同
    /// [`Detector::load`]。YuNet 输入固定 batch=1，不支持批 session。
    pub fn load(model: &Path, device: &str, conf: f32) -> Result<Self, DetectError> {
        if !model.exists() {
            return Err(DetectError::ModelNotFound(model.to_path_buf()));
        }
        let session = commit_session(device, model)?;
        // 输入尺寸直接取自输入维度（YuNet 无 output0，probe_io 不适用；
        // 动态维度（-1）时用 yolo 系惯例 640）
        let in_dims = session
            .inputs()
            .first()
            .and_then(|o| o.dtype().tensor_shape())
            .map(|s| s.iter().copied().collect::<Vec<_>>());
        let input_size = match in_dims {
            Some(d) if d.len() == 4 && d[2] > 0 && d[2] == d[3] => d[2] as usize,
            _ => INPUT_SIZE,
        };
        let input_name = session
            .inputs()
            .first()
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| "images".into());
        let layout = if session.outputs().iter().any(|o| o.name() == "cls_8") {
            FaceLayout::YuNet
        } else {
            FaceLayout::YoloAnchor
        };
        Ok(Self { session, batch: None, conf, input_size, device: device.to_string(), layout, input_name })
    }

    /// 加载固定 batch=N 的第二模型（仅 yolo 系；YuNet 固定 batch=1）。
    pub fn enable_batch(&mut self, model: &Path, n: usize) -> Result<(), DetectError> {
        if self.layout == FaceLayout::YuNet {
            return Err(DetectError::BadModel);
        }
        if !model.exists() {
            return Err(DetectError::ModelNotFound(model.to_path_buf()));
        }
        self.batch = Some((n, commit_session(&self.device, model)?));
        Ok(())
    }

    /// 检测人脸框（原始分辨率坐标）。
    pub fn detect_boxes(&mut self, nv12: &[u8], w: usize, h: usize) -> Result<Vec<FaceBox>, DetectError> {
        Ok(self
            .detect_boxes_batch(&[nv12], w, h)?
            .pop()
            .unwrap_or_default())
    }

    /// 级联 ROI 人脸检测（DESIGN §6 精度清单 #3）：对 `roi`（全帧坐标 xyxy）
    /// 裁剪出子 NV12 后 letterbox 放大到模型输入尺寸推理——远景小脸的有效
    /// 分辨率随裁剪比例放大（如 1080p 里 200px 高的人 → 头部 60px 裁剪后
    /// 放大到 640，等效 10× 采样）。返回框已映射回全帧坐标。
    pub fn detect_boxes_roi(
        &mut self,
        nv12: &[u8],
        w: usize,
        h: usize,
        roi: [f32; 4],
    ) -> Result<Vec<FaceBox>, DetectError> {
        let (cw, ch, ox, oy, crop) = crop_nv12(nv12, w, h, roi);
        if cw < 8 || ch < 8 {
            return Ok(vec![]);
        }
        let faces = self.detect_boxes(&crop, cw, ch)?;
        Ok(faces
            .into_iter()
            .map(|f| FaceBox {
                xyxy: [
                    f.xyxy[0] + ox as f32,
                    f.xyxy[1] + oy as f32,
                    f.xyxy[2] + ox as f32,
                    f.xyxy[3] + oy as f32,
                ],
                score: f.score,
                eyes: f.eyes.map(|(l, r)| {
                    ([l[0] + ox as f32, l[1] + oy as f32], [r[0] + ox as f32, r[1] + oy as f32])
                }),
            })
            .collect())
    }

    /// 批量检测（语义同 [`Detector::detect_person_instances_batch`]）。
    /// YuNet 输入固定 batch=1，恒走逐帧路径。
    pub fn detect_boxes_batch(
        &mut self,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<FaceBox>>, DetectError> {
        let conf = self.conf;
        let size = self.input_size;
        if self.layout == FaceLayout::YuNet {
            let mut out = Vec::with_capacity(frames.len());
            for f in frames {
                out.push(Self::infer_yunet_session(
                    &mut self.session,
                    &self.input_name,
                    conf,
                    size,
                    f,
                    w,
                    h,
                )?);
            }
            return Ok(out);
        }
        if let Some((n, sess)) = self.batch.as_mut()
            && *n == frames.len()
        {
            return Self::infer_face_session(sess, &self.input_name, conf, size, frames, w, h);
        }
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            out.push(
                Self::infer_face_session(&mut self.session, &self.input_name, conf, size, &[f], w, h)?
                    .pop()
                    .unwrap_or_default(),
            );
        }
        Ok(out)
    }

    fn infer_face_session(
        session: &mut ort::session::Session,
        input_name: &str,
        conf: f32,
        size: usize,
        frames: &[&[u8]],
        w: usize,
        h: usize,
    ) -> Result<Vec<Vec<FaceBox>>, DetectError> {
        const FACE_ROWS: usize = 20;
        let b = frames.len();
        let mut input = Vec::with_capacity(b * 3 * size * size);
        for f in frames {
            input.extend_from_slice(&nv12_to_letterbox_chw(f, w, h, size, false));
        }
        let value: ort::session::SessionInputValue =
            ort::value::Tensor::from_array(([b as i64, 3, size as i64, size as i64], input))?
                .into();
        let outputs = session
            .run(vec![(std::borrow::Cow::Borrowed(input_name), value)])?;
        let (s0, o0) = outputs["output0"].try_extract_tensor::<f32>()?;
        if s0.len() != 3 || s0[0] != b as i64 || s0[1] != FACE_ROWS as i64 {
            return Err(DetectError::BadOutput { actual: s0.to_vec() });
        }
        let n = s0[2] as usize;
        let frame0 = FACE_ROWS * n;
        let (scale, pad_x, pad_y, _, _) = letterbox_params(w, h, size);

        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            out.push(decode_face_frame(&o0[i * frame0..], n, conf, scale, pad_x, pad_y, w, h, true));
        }
        Ok(out)
    }

    /// YuNet 单帧推理（DESIGN §5.2 速度档人脸）：BGR 原值 letterbox → 12 输出
    /// → 逐 stride 解码 → NMS。解码公式与 OpenCV FaceDetectorYN 对齐
    /// （2026-08-21 数值对照验证，框/五点/分数逐位一致）：
    /// - 网格行列：row = i/cols、col = i%cols（cols = size/stride）
    /// - 框：cx=(col+dx)·s、cy=(row+dy)·s、w=exp(dw)·s、h=exp(dh)·s
    /// - 关键点：kps_n = (δx+col)·s、(δy+row)·s；前两点为右/左眼
    /// - 分数：√(clamp(cls)·clamp(obj))
    fn infer_yunet_session(
        session: &mut ort::session::Session,
        input_name: &str,
        conf: f32,
        size: usize,
        nv12: &[u8],
        w: usize,
        h: usize,
    ) -> Result<Vec<FaceBox>, DetectError> {
        let input = nv12_to_yunet_chw(nv12, w, h, size);
        let value: ort::session::SessionInputValue =
            ort::value::Tensor::from_array(([1i64, 3, size as i64, size as i64], input))?.into();
        let outputs = session.run(vec![(std::borrow::Cow::Borrowed(input_name), value)])?;

        let mut cands: Vec<FaceBox> = Vec::new();
        for s in [8usize, 16, 32] {
            let cols = size / s;
            let n = cols * cols;
            let get = |name: String| -> Result<(Vec<i64>, Vec<f32>), DetectError> {
                let (shape, t) = outputs[name.as_str()].try_extract_tensor::<f32>()?;
                Ok((shape.to_vec(), t.to_vec()))
            };
            let (cs, ct) = get(format!("cls_{s}"))?;
            let (os, ot) = get(format!("obj_{s}"))?;
            let (bs, bt) = get(format!("bbox_{s}"))?;
            let (ks, kt) = get(format!("kps_{s}"))?;
            let ok_shape = |sh: &[i64], last: usize| {
                sh.len() == 3 && sh[0] == 1 && sh[1] == n as i64 && sh[2] == last as i64
            };
            if !ok_shape(&cs, 1) || !ok_shape(&os, 1) || !ok_shape(&bs, 4) || !ok_shape(&ks, 10) {
                return Err(DetectError::BadOutput { actual: cs });
            }
            for i in 0..n {
                let score = (ct[i].clamp(0.0, 1.0) * ot[i].clamp(0.0, 1.0)).sqrt();
                if score < conf {
                    continue;
                }
                let (row, col) = (i / cols, i % cols);
                let (sf, cf, rf) = (s as f32, col as f32, row as f32);
                let b = i * 4;
                let cx = (cf + bt[b]) * sf;
                let cy = (rf + bt[b + 1]) * sf;
                let bw = bt[b + 2].exp() * sf;
                let bh = bt[b + 3].exp() * sf;
                let k = i * 10;
                let pt = |j: usize| [(kt[k + 2 * j] + cf) * sf, (kt[k + 2 * j + 1] + rf) * sf];
                cands.push(FaceBox {
                    xyxy: [cx - bw * 0.5, cy - bh * 0.5, cx + bw * 0.5, cy + bh * 0.5],
                    score,
                    eyes: Some((pt(0), pt(1))),
                });
            }
        }
        // NMS（拉伸空间）+ 帧坐标映射
        cands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let mut kept: Vec<FaceBox> = Vec::new();
        for c in cands {
            if kept.len() >= MAX_DET {
                break;
            }
            if kept.iter().all(|k| box_iou(&k.xyxy, &c.xyxy) <= 0.3) {
                kept.push(c);
            }
        }
        // 拉伸坐标 → 帧坐标（各向异性：x、y 独立缩放，无 padding）
        let to_frame = |v: f32, in_dim: usize, out_dim: usize| {
            (v * out_dim as f32 / in_dim as f32).clamp(0.0, out_dim as f32)
        };
        let un = |p: [f32; 2]| [to_frame(p[0], size, w), to_frame(p[1], size, h)];
        Ok(kept
            .into_iter()
            .map(|d| FaceBox {
                xyxy: [
                    to_frame(d.xyxy[0], size, w),
                    to_frame(d.xyxy[1], size, h),
                    to_frame(d.xyxy[2], size, w),
                    to_frame(d.xyxy[3], size, h),
                ],
                score: d.score,
                eyes: d.eyes.map(|(l, r)| (un(l), un(r))),
            })
            .collect())
    }
}

/// 单帧人脸解码 + NMS（IoU 0.3）+ 帧坐标映射。
fn decode_face_frame(
    o0: &[f32],
    n: usize,
    conf: f32,
    scale: f32,
    pad_x: usize,
    pad_y: usize,
    w: usize,
    h: usize,
    has_landmarks: bool,
) -> Vec<FaceBox> {
    let mut dets = Vec::new();
    for c in 0..n {
        let score = o0[4 * n + c]; // 单类：第 5 行即人脸置信度
        if score < conf {
            continue;
        }
        let (cx, cy) = (o0[c], o0[n + c]);
        let (bw, bh) = (o0[2 * n + c], o0[3 * n + c]);
        // 5 点 landmark（yolo-face 布局：通道 5+3i 起 (x,y,conf)×5，前两点为双眼）
        let eyes = has_landmarks.then(|| {
            let pt = |i: usize| {
                let (x, y, v) = (o0[(5 + 3 * i) * n + c], o0[(6 + 3 * i) * n + c], o0[(7 + 3 * i) * n + c]);
                (x, y, v)
            };
            let l = pt(0);
            let r = pt(1);
            (l.2 >= 0.5 && r.2 >= 0.5).then(|| ([l.0, l.1], [r.0, r.1]))
        })
        .flatten();
        dets.push(FaceBox {
            xyxy: [cx - bw * 0.5, cy - bh * 0.5, cx + bw * 0.5, cy + bh * 0.5],
            score,
            eyes,
        });
    }
    dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    // NMS 必须在 letterbox 原始空间内完成（曾因与已映射坐标混算 IoU 导致
    // 同一张脸保留 3~8 个重复框、遮盖面积虚大）
    let mut kept_raw: Vec<FaceBox> = Vec::new();
    for d in dets {
        if kept_raw.len() >= MAX_DET {
            break;
        }
        if kept_raw.iter().all(|k| box_iou(&k.xyxy, &d.xyxy) <= 0.3) {
            kept_raw.push(d);
        }
    }
    kept_raw
        .into_iter()
        .map(|d| {
            let eyes = d.eyes.map(|(l, r)| {
                let un = |p: [f32; 2]| {
                    [
                        ((p[0] - pad_x as f32) / scale).clamp(0.0, w as f32),
                        ((p[1] - pad_y as f32) / scale).clamp(0.0, h as f32),
                    ]
                };
                (un(l), un(r))
            });
            FaceBox {
                xyxy: unletterbox_box(d.xyxy, scale, pad_x, pad_y, w, h, false),
                score: d.score,
                eyes,
            }
        })
        .collect()
}

/// NV12 → YuNet 输入：**直接拉伸**到 size×size 的 BGR 0..255 CHW（无 padding、
/// 各向异性缩放——与 OpenCV FaceDetectorYN 的 setInputSize 语义一致；实测
/// letterbox+零填充使该帧人脸分数 0.51→0.23，此模型对训练分布敏感）。
/// 采样核与 [`nv12_to_letterbox_chw`] 相同（双线性 Y + 最近邻 UV + BT.601）。
fn nv12_to_yunet_chw(nv12: &[u8], w: usize, h: usize, size: usize) -> Vec<f32> {
    let plane = size * size;
    let mut out = vec![0f32; 3 * plane];
    let y_plane = &nv12[..w * h];
    let uv = &nv12[w * h..];
    let (chw, chh) = (w / 2, h / 2);
    let (sx, sy) = (w as f32 / size as f32, h as f32 / size as f32);

    for oy in 0..size {
        let fy = (oy as f32 + 0.5) * sy - 0.5;
        let y0 = fy.floor().max(0.0) as usize;
        let iy0 = y0.min(h - 1);
        let iy1 = (y0 + 1).min(h - 1);
        let dy = (fy - y0 as f32).clamp(0.0, 1.0);
        let cy = (((fy + 0.5) * 0.5) as usize).min(chh - 1);
        for ox in 0..size {
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
            let i = oy * size + ox;
            out[i] = b;
            out[plane + i] = g;
            out[2 * plane + i] = r;
        }
    }
    out
}

/// 人脸外扩（landmark 自适应）：正脸眼距 ≈ 0.35×框宽，转头时缩短——
/// 差额按 0.6 系数补到水平外扩（盖住侧脸轮廓/头发），垂直保持基础值。
/// 无 landmark（或置信不足）时退回固定基础外扩。
pub fn face_expand_xy(fb: &FaceBox, base: usize, landmark_expand: bool) -> (usize, usize) {
    if !landmark_expand {
        return (base, base);
    }
    match fb.eyes {
        Some((l, r)) => {
            let eye_dist = ((r[0] - l[0]).powi(2) + (r[1] - l[1]).powi(2)).sqrt();
            let bw = (fb.xyxy[2] - fb.xyxy[0]).max(1.0);
            let shrink = (0.35 * bw - eye_dist).clamp(0.0, 0.35 * bw);
            (base + (shrink * 0.6) as usize, base)
        }
        None => (base, base),
    }
}

/// 人脸框与 person 关联过滤（消灭 person 外的误检脸，如海报/屏幕/物品）：
/// 保留条件 = 框中心落在任一 person 框（四周外扩 8%）内，
/// 或分数 ≥ standalone_thr（人体未检出时的特写场景兜底）。
pub fn gate_faces(
    faces: Vec<FaceBox>,
    person_boxes: &[[f32; 4]],
    standalone_thr: f32,
) -> Vec<FaceBox> {
    faces
        .into_iter()
        .filter(|f| {
            if f.score >= standalone_thr {
                return true;
            }
            let (cx, cy) = ((f.xyxy[0] + f.xyxy[2]) * 0.5, (f.xyxy[1] + f.xyxy[3]) * 0.5);
            person_boxes.iter().any(|p| {
                let (ex, ey) = ((p[2] - p[0]) * 0.08, (p[3] - p[1]) * 0.08);
                cx >= p[0] - ex && cx <= p[2] + ex && cy >= p[1] - ey && cy <= p[3] + ey
            })
        })
        .collect()
}

/// 合并全帧与级联 ROI 的人脸结果：IoU > `thr` 视为同一张脸，保留高分。
pub fn merge_faces(a: Vec<FaceBox>, b: Vec<FaceBox>, thr: f32) -> Vec<FaceBox> {
    let mut all = a;
    all.extend(b);
    all.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<FaceBox> = Vec::new();
    for f in all {
        if kept.iter().all(|k| box_iou(&k.xyxy, &f.xyxy) <= thr) {
            kept.push(f);
        }
    }
    kept
}

/// 脸部几何合理性过滤（俯视/级联 ROI 误检防线）：
/// - 宽高比 w/h ∈ [0.5, 1.8]（人脸不会是细长条或横幅）；
/// - 落在 person 框内的脸，高度 ≤ 该 person 高度的 50%（真人脸 ≈ 人体高的
///   8%~30%；超过一半基本是衣物/地面纹理误检——俯视视角下级联 ROI 的
///   "顶部 30%"假设失效时的高发模式）；
/// - 不在任何 person 框内的脸（特写高分兜底路径）只做宽高比检查。
pub fn filter_implausible_faces(faces: Vec<FaceBox>, person_boxes: &[[f32; 4]]) -> Vec<FaceBox> {
    faces
        .into_iter()
        .filter(|f| {
            let (fw, fh) = (f.xyxy[2] - f.xyxy[0], f.xyxy[3] - f.xyxy[1]);
            if fw <= 0.0 || fh <= 0.0 {
                return false;
            }
            let ar = fw / fh;
            if !(0.5..=1.8).contains(&ar) {
                return false;
            }
            let (cx, cy) = ((f.xyxy[0] + f.xyxy[2]) * 0.5, (f.xyxy[1] + f.xyxy[3]) * 0.5);
            let host = person_boxes.iter().find(|p| {
                cx >= p[0] && cx <= p[2] && cy >= p[1] && cy <= p[3]
            });
            match host {
                Some(p) => {
                    let ph = p[3] - p[1];
                    ph <= 0.0 || fh / ph <= 0.5
                }
                None => true, // 特写：无宿主人体，只做宽高比检查
            }
        })
        .collect()
}

/// 全帧坐标 ROI → 紧凑子 NV12（行复制；宽高对齐偶数，UV 半采样随之对齐）。
/// 返回 (crop_w, crop_h, 原点 x, 原点 y, 数据)；roi 越界部分被钳制。
pub(crate) fn crop_nv12(nv12: &[u8], w: usize, h: usize, roi: [f32; 4]) -> (usize, usize, usize, usize, Vec<u8>) {
    let x1 = (roi[0].max(0.0).floor() as usize).min(w.saturating_sub(2)) & !1;
    let y1 = (roi[1].max(0.0).floor() as usize).min(h.saturating_sub(2));
    let x2 = ((roi[2].ceil() as usize).min(w)).max(x1 + 2) & !1;
    let y2 = ((roi[3].ceil() as usize).min(h)).max(y1 + 2);
    let (cw, ch) = (x2 - x1, y2 - y1);
    let mut out = vec![0u8; cw * ch * 3 / 2];
    // Y 平面（stride w → cw）
    for (dy, y) in (y1..y2).enumerate() {
        let src = y * w + x1;
        out[dy * cw..dy * cw + cw].copy_from_slice(&nv12[src..src + cw]);
    }
    // UV 平面（行数/列数减半）
    let (uv_y1, uv_rows) = (y1 / 2, ch / 2);
    let uv_off = cw * ch;
    for (dy, uy) in (uv_y1..uv_y1 + uv_rows).enumerate() {
        let src = w * h + uy * w + x1; // UV 行 stride 与 Y 相同（packed NV12）
        let dst = uv_off + dy * cw;
        out[dst..dst + cw].copy_from_slice(&nv12[src..src + cw]);
    }
    (cw, ch, x1, y1, out)
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_params_center() {
        // 16:9 → 640×360 居中，上下 pad 140
        let (scale, pad_x, pad_y, nw, nh) = letterbox_params(1920, 1080, 640);
        assert!((scale - 640.0 / 1920.0).abs() < 1e-6);
        assert_eq!((nw, nh, pad_x, pad_y), (640, 360, 0, 140));
    }

    #[test]
    fn nv12_white_frame_produces_near_white_rgb() {
        // 全白 Y=235, U=V=128（limited range 白）→ RGB ≈ 255
        let (w, h) = (64, 64);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        nv12[..w * h].fill(235);
        let chw = nv12_to_letterbox_chw(&nv12, w, h, 64, false);
        let plane = 64 * 64;
        for i in 0..plane {
            assert!(chw[i] > 0.9, "R[{}]={}", i, chw[i]);
            assert!(chw[plane + i] > 0.9);
            assert!(chw[2 * plane + i] > 0.9);
        }
    }

    #[test]
    fn flipped_letterbox_mirrors_columns() {
        // 无缩放方形（scale=1 无 pad）：翻转趟的输出 = 正常趟的列反转
        let (w, h, size) = (64, 64, 64);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                nv12[y * w + x] = (x * 4) as u8; // 水平渐变（左右不对称）
            }
        }
        let normal = nv12_to_letterbox_chw(&nv12, w, h, size, false);
        let flipped = nv12_to_letterbox_chw(&nv12, w, h, size, true);
        let plane = size * size;
        for y in 0..size {
            for x in 0..size {
                let (a, b) = (normal[y * size + x], flipped[y * size + (size - 1 - x)]);
                assert!((a - b).abs() < 1e-4, "({y},{x}): {a} vs {b}");
            }
        }
    }

    #[test]
    fn unletterbox_flip_mirrors_box_and_mask() {
        // scale=1 无 pad：翻转映射 x → w-1-x
        let b = unletterbox_box([10.0, 20.0, 110.0, 220.0], 1.0, 0, 0, 640, 480, true);
        assert_eq!(b, [529.0, 20.0, 629.0, 220.0]); // 640-1-110, 640-1-10
        // mask：左半 1 → 翻转后落在右半
        let (w, h) = (16, 4);
        let mut m = vec![0u8; w * w];
        for y in 0..w {
            for x in 0..4 {
                m[y * w + x] = 1;
            }
        }
        let mut out = vec![0u8; w * h];
        unletterbox_into(&m, 1.0, 0, 0, w, w, w, w, h, true, &mut out);
        assert_eq!(out[0 * w + 12], 1, "左半遮罩应镜像到右半");
        assert_eq!(out[0 * w + 3], 0);
        assert_eq!(out[(h - 1) * w + 15], 1);
    }

    #[test]
    fn merge_instances_dedups_and_keeps_high_score() {
        let mk = |x: f32, s: f32| PersonInstance { score: s, xyxy: [x, 0.0, x + 100.0, 100.0], mask: vec![(s * 10.0) as u8] };
        // 两趟同目标（IoU≈0.82）+ 翻转趟独有的第二个目标
        let merged = merge_instances(vec![mk(0.0, 0.9)], vec![mk(5.0, 0.7), mk(300.0, 0.6)]);
        assert_eq!(merged.len(), 2, "重合目标去重，独有目标保留");
        assert_eq!(merged[0].score, 0.9, "保留高分假设");
        assert_eq!(merged[1].xyxy[0], 300.0);
    }

    fn make_output0(score: f32, n: usize) -> (Vec<i64>, Vec<f32>) {
        // 一个 anchor：box 中心 (320,320) 大小 100，person 分数 score
        let mut o0 = vec![0f32; ROWS * n];
        let c = 5;
        o0[c] = 320.0;
        o0[n + c] = 320.0;
        o0[2 * n + c] = 100.0;
        o0[3 * n + c] = 100.0;
        o0[4 * n + c] = score;
        (vec![1, ROWS as i64, n as i64], o0)
    }

    #[test]
    fn decode_filters_by_conf_and_decodes_box() {
        let (shape, o0) = make_output0(0.8, 10);
        let dets = decode_person(&o0, &shape, 0.5).unwrap();
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert_eq!(d.xyxy, [270.0, 270.0, 370.0, 370.0]);

        let (shape2, o02) = make_output0(0.3, 10);
        assert!(decode_person(&o02, &shape2, 0.5).unwrap().is_empty());
    }

    #[test]
    fn nms_suppresses_overlap() {
        let mk = |x: f32, s: f32| Detection {
            xyxy: [x, 0.0, x + 100.0, 100.0],
            score: s,
            coeffs: [0.0; PROTO_DIM],
        };
        let kept = nms(vec![mk(0.0, 0.9), mk(10.0, 0.8), mk(200.0, 0.7)], 0.45, 300);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].xyxy[0], 0.0);
        assert_eq!(kept[1].xyxy[0], 200.0);
    }

    #[test]
    fn decode_e2e_filters_class_and_conf() {
        // 3 行：person 高分 / 非 person 高分 / person 低分；stride=38（seg）
        let stride = 4 + 1 + 1 + PROTO_DIM;
        let mut o0 = vec![0f32; 3 * stride];
        for (r, &(score, class)) in [(0.9f32, 0.0f32), (0.95, 45.0), (0.2, 0.0)].iter().enumerate() {
            let b = r * stride;
            o0[b..b + 4].copy_from_slice(&[10.0, 20.0, 110.0, 220.0]);
            o0[b + 4] = score;
            o0[b + 5] = class;
            o0[b + 6] = 1.5; // 首个 coeff
        }
        let dets = decode_person_e2e(&o0, 3, stride, 0.35);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].xyxy, [10.0, 20.0, 110.0, 220.0]);
        assert_eq!(dets[0].coeffs[0], 1.5);

        // 检测-only stride=6（无 coeffs）
        let mut o6 = vec![0f32; stride]; // 长度 ≥ rows*6 即可
        o6[..4].copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        o6[4] = 0.8;
        o6[5] = 0.0;
        let dets = decode_person_e2e(&o6, 1, 6, 0.35);
        assert_eq!(dets.len(), 1);
        assert_eq!(dets[0].coeffs, [0.0; PROTO_DIM]);
    }

    #[test]
    fn rect_mask_inflates_by_margin() {
        let m = rect_mask([100.0, 100.0, 200.0, 200.0], 0.05, 640, 480);
        // 5% of 100px = 5px 外扩 → [95, 205)（f32 下 100*0.05 恰为 5.0）
        assert_eq!(m[100 * 640 + 100], 1);
        assert_eq!(m[95 * 640 + 95], 1);
        assert_eq!(m[105 * 640 + 204], 1);
        assert_eq!(m[105 * 640 + 205], 0); // 排他边界
        assert_eq!(m[90 * 640 + 90], 0); // 外扩之外
        // 面积 = 110×110
        let area: usize = m.iter().map(|&v| v as usize).sum();
        assert_eq!(area, 110 * 110);
    }

    #[test]
    fn letterbox_params_1280() {
        let (scale, pad_x, pad_y, nw, nh) = letterbox_params(1920, 1080, 1280);
        assert!((scale - 1280.0 / 1920.0).abs() < 1e-6);
        assert_eq!((nw, nh, pad_x, pad_y), (1280, 720, 0, 280));
    }

    #[test]
    fn gate_faces_filters_outside_person() {
        let faces = vec![
            FaceBox { xyxy: [100.0, 100.0, 150.0, 150.0], score: 0.30, eyes: None }, // person 内 → 保留
            FaceBox { xyxy: [800.0, 800.0, 850.0, 850.0], score: 0.55, eyes: None }, // person 外 → 丢弃
            FaceBox { xyxy: [900.0, 100.0, 960.0, 160.0], score: 0.75, eyes: None }, // person 外但高分 → 保留（特写）
        ];
        let persons = [[80.0, 80.0, 400.0, 600.0]];
        let kept = gate_faces(faces, &persons, 0.6);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|f| f.xyxy[0] != 800.0));
    }

    #[test]
    fn crop_nv12_extracts_aligned_subframe() {
        // 8×8 灰阶帧，裁 (2,2)-(6,6)：Y 值应为原帧对应子块
        let (w, h) = (8, 8);
        let mut nv12 = vec![0u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                nv12[y * w + x] = (y * 8 + x) as u8;
            }
        }
        let (cw, ch, ox, oy, crop) = crop_nv12(&nv12, w, h, [2.3, 2.9, 6.7, 6.2]);
        assert_eq!((cw, ch, ox, oy), (4, 5, 2, 2)); // x2=6.7→7(偶), y2=6.2→7
        assert_eq!(crop.len(), cw * ch * 3 / 2);
        for dy in 0..ch {
            for dx in 0..cw {
                assert_eq!(crop[dy * cw + dx], nv12[(oy + dy) * w + ox + dx]);
            }
        }
        // UV 平面（半分辨率）同样对应
        let uv_off = cw * ch;
        for dy in 0..ch / 2 {
            for dx in 0..cw {
                assert_eq!(
                    crop[uv_off + dy * cw + dx],
                    nv12[w * h + (oy / 2 + dy) * w + ox + dx]
                );
            }
        }
    }

    #[test]
    fn crop_nv12_clamps_out_of_bounds_roi() {
        let (w, h) = (16, 16);
        let nv12 = vec![7u8; w * h * 3 / 2];
        // 越界 ROI 被钳到帧内，不 panic
        let (cw, ch, ox, oy, crop) = crop_nv12(&nv12, w, h, [-50.0, -50.0, 999.0, 999.0]);
        assert_eq!((cw, ch, ox, oy), (w, h, 0, 0));
        assert_eq!(crop.len(), w * h * 3 / 2);
    }

    #[test]
    fn filter_implausible_faces_rejects_bad_geometry() {
        let person = [[100.0, 100.0, 200.0, 1000.0]]; // 900 高的人
        let mk = |x: f32, y: f32, w: f32, h: f32| FaceBox { xyxy: [x, y, x + w, y + h], score: 0.8, eyes: None };
        let faces = vec![
            mk(120.0, 110.0, 60.0, 80.0),   // 正常脸：比例 0.75、高/人高=0.09 → 保留
            mk(110.0, 300.0, 80.0, 100.0),  // person 内正常比例 → 保留
            mk(110.0, 400.0, 80.0, 500.0),  // 脸高超人体一半（500/900=0.56）→ 拒绝
            mk(110.0, 200.0, 90.0, 10.0),   // 细长条：比例 9 → 拒绝
            mk(110.0, 200.0, 10.0, 90.0),   // 竖长条：比例 0.11 → 拒绝
        ];
        // 在 person 框外的高分特写脸（standalone）：仅宽高比检查 → 保留
        let mut all = faces;
        all.push(mk(800.0, 800.0, 80.0, 100.0));
        let kept = filter_implausible_faces(all, &person);
        assert_eq!(kept.len(), 3, "保留：正常脸、中等宽脸、无宿主特写脸");
        assert!(kept.iter().all(|f| {
            let (fw, fh) = (f.xyxy[2] - f.xyxy[0], f.xyxy[3] - f.xyxy[1]);
            (0.5..=1.8).contains(&(fw / fh))
        }));
    }

    #[test]
    fn merge_faces_dedups_by_iou_keeps_high_score() {
        let a = vec![FaceBox { xyxy: [100.0, 100.0, 150.0, 150.0], score: 0.9, eyes: None }];
        let b = vec![
            FaceBox { xyxy: [102.0, 101.0, 151.0, 149.0], score: 0.6, eyes: None }, // IoU>0.6 → 去重留 0.9
            FaceBox { xyxy: [400.0, 400.0, 450.0, 450.0], score: 0.5, eyes: None }, // 不同位置 → 保留
        ];
        let merged = merge_faces(a, b, 0.6);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|f| f.score == 0.9));
        assert!(merged.iter().any(|f| f.score == 0.5));
    }

    #[test]
    fn face_expand_adapts_to_eye_distance() {
        let mk = |eyes: Option<([f32; 2], [f32; 2])>| FaceBox {
            xyxy: [100.0, 100.0, 200.0, 220.0], // 框宽 100
            score: 0.8,
            eyes,
        };
        // 正脸：眼距 35（≈0.35×框宽）→ 无补扩
        let (ex, ey) = face_expand_xy(&mk(Some(([110.0, 140.0], [145.0, 140.0]))), 12, true);
        assert_eq!((ex, ey), (12, 12));
        // 侧脸：眼距 ≈5.1 → 缩短 ≈29.9 → 水平补 29.9×0.6≈17.9 → 12+17=29
        let (ex, ey) = face_expand_xy(&mk(Some(([110.0, 140.0], [115.0, 141.0]))), 12, true);
        assert_eq!((ex, ey), (29, 12));
        // 无 landmark → 固定
        assert_eq!(face_expand_xy(&mk(None), 12, true), (12, 12));
        // 开关关闭 → 即使侧脸也固定
        assert_eq!(
            face_expand_xy(&mk(Some(([110.0, 140.0], [115.0, 141.0]))), 12, false),
            (12, 12)
        );
    }

    #[test]
    fn yunet_detects_face_on_real_frame() {
        // YuNet 解码路径的真实帧验证（2026-08-21 与 OpenCV FaceDetectorYN
        // 官方解码数值对照后的回归锚点：clip5s 首帧有 1 张 ~0.5 分的人脸）
        let model = crate::models::resolve_model("face_detection_yunet_2023mar.onnx");
        let video = std::path::Path::new("../../tests/clip5s.mp4");
        if !model.exists() || !video.exists() {
            eprintln!("skip: 无 YuNet 模型或测试视频（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut fd = FaceDetector::load(&model, "cpu", 0.4).expect("加载 YuNet");
        assert_eq!(fd.input_size, 640);
        let faces = fd.detect_boxes(&nv12, w, h).expect("推理");
        assert!(!faces.is_empty(), "clip5s 首帧应检出人脸（官方解码对照分 0.51）");
        let f = &faces[0];
        assert!(f.score >= 0.4, "分数 {}", f.score);
        let (fw, fh) = (f.xyxy[2] - f.xyxy[0], f.xyxy[3] - f.xyxy[1]);
        assert!(fw > 20.0 && fh > 20.0, "真实人脸尺寸，得 {:?}", f.xyxy);
        let eyes = f.eyes.expect("YuNet 恒有双眼点");
        let (el, er) = (eyes.0, eyes.1);
        assert!(el[0] >= 0.0 && el[0] <= w as f32 && er[1] >= 0.0 && er[1] <= h as f32);
        // 双眼应在框内（landmark 外扩的可用性前提）
        assert!(el[0] >= f.xyxy[0] && el[0] <= f.xyxy[2] && el[1] >= f.xyxy[1] && el[1] <= f.xyxy[3]);
        assert!(er[0] >= f.xyxy[0] && er[0] <= f.xyxy[2]);
    }

    #[test]
    fn gate_faces_empty_persons_keeps_high_score_only() {
        let faces = vec![
            FaceBox { xyxy: [0.0, 0.0, 50.0, 50.0], score: 0.40, eyes: None },
            FaceBox { xyxy: [60.0, 0.0, 110.0, 50.0], score: 0.70, eyes: None },
        ];
        let kept = gate_faces(faces, &[], 0.6);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].score, 0.70);
    }

    #[test]
    fn mask_from_uniform_proto_matches_box_region() {
        // proto 全 1，coeffs 全 1 → sigmoid(32)=1 → box 区域全遮罩
        let det = Detection {
            xyxy: [100.0, 100.0, 200.0, 200.0],
            score: 0.9,
            coeffs: [1.0; PROTO_DIM],
        };
        let psize = 240; // yolo26s-seg @960 的 proto 尺寸
        let proto = vec![1.0f32; PROTO_DIM * psize * psize];
        let m = det_mask(&det, &proto, 960, psize);
        assert_eq!(m[100 * 960 + 100], 1);
        assert_eq!(m[150 * 960 + 150], 1);
        assert_eq!(m[50 * 960 + 50], 0); // box 外
    }

    #[test]
    fn unletterbox_maps_back() {
        // 640×640 全 1 → 原始分辨率全 1（scale=1 无 pad）
        let m = vec![1u8; INPUT_SIZE * INPUT_SIZE];
        let mut out = vec![0u8; 640 * 200];
        unletterbox_into(&m, 1.0, 0, 0, 640, 640, INPUT_SIZE, 640, 200, false, &mut out);
        assert!(out.iter().all(|&v| v == 1));
    }
}
