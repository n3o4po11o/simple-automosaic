//! SAM2.1 逐帧 box/point prompting 精修（DESIGN §5.6 管线 A 步骤 3）。
//!
//! 模型资产：vietanhdev/segment-anything-2.1-onnx-models（Apache-2.0 源权重）。
//! - encoder `image [1,3,1024,1024]` → `image_embed [1,256,64,64]` +
//!   `high_res_feats_0 [1,32,256,256]` + `high_res_feats_1 [1,64,128,128]`
//!   （图外归一化：ResizeLongestSide(1024) 右下零填充 + pixel mean/std——
//!   官方导出语义，2026-08-21 真实帧双变体对照确认 mean/std 置信度更高）。
//! - decoder 多提示批：`num_labels` 维 = 提示数（box 编码为两角点 label 2/3），
//!   输出 `masks [N,3,256,256]`（raw logits）+ `iou_predictions [N,3]`，
//!   取 IoU 最高的候选（multimask argmax）。
//! 低分辨率 mask（256 域，1024 空间）最近邻映射回原始分辨率。

use std::path::Path;

use crate::detect::DetectError;

/// SAM 输入边长（官方 ResizeLongestSide 目标）。
const SAM_SIZE: usize = 1024;

fn sam_mean() -> [f32; 3] {
    [123.675, 116.28, 103.53]
}

fn sam_std() -> [f32; 3] {
    [58.395, 57.12, 57.375]
}

fn commit(device: &str, model: &Path) -> Result<ort::session::Session, DetectError> {
    // 与 detect.rs 的 commit_session 同语义（CoreML 缓存/EP 链），此处独立一份
    // 避免把内部函数 pub 化；EP 失败由 ort 自动落 CPU。
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

/// NV12 → SAM 输入：ResizeLongestSide(1024)（等比缩放 + 右下零填充）
/// → RGB → pixel mean/std 归一化 → CHW f32。返回 (输入张量, 缩放系数)。
fn nv12_to_sam_input(nv12: &[u8], w: usize, h: usize) -> (Vec<f32>, f32) {
    let scale = SAM_SIZE as f32 / w.max(h) as f32;
    let new_w = (w as f32 * scale).round() as usize;
    let new_h = (h as f32 * scale).round() as usize;
    let plane = SAM_SIZE * SAM_SIZE;
    let mut out = vec![0f32; 3 * plane];
    let (mean, std) = (sam_mean(), sam_std());
    let y_plane = &nv12[..w * h];
    let uv = &nv12[w * h..];
    let (chw, chh) = (w / 2, h / 2);

    for oy in 0..new_h {
        let sy = (oy as f32 + 0.5) / scale - 0.5;
        let y0 = sy.floor().max(0.0) as usize;
        let iy0 = y0.min(h - 1);
        let iy1 = (y0 + 1).min(h - 1);
        let fy = (sy - y0 as f32).clamp(0.0, 1.0);
        let cy = (((sy + 0.5) * 0.5) as usize).min(chh - 1);
        for ox in 0..new_w {
            let sx = (ox as f32 + 0.5) / scale - 0.5;
            let x0 = sx.floor().max(0.0) as usize;
            let ix0 = x0.min(w - 1);
            let ix1 = (x0 + 1).min(w - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);
            let cx = (((sx + 0.5) * 0.5) as usize).min(chw - 1);

            let y00 = y_plane[iy0 * w + ix0] as f32;
            let y10 = y_plane[iy0 * w + ix1] as f32;
            let y01 = y_plane[iy1 * w + ix0] as f32;
            let y11 = y_plane[iy1 * w + ix1] as f32;
            let yv = y00 * (1.0 - fx) * (1.0 - fy)
                + y10 * fx * (1.0 - fy)
                + y01 * (1.0 - fx) * fy
                + y11 * fx * fy;
            let u = uv[cy * w + cx * 2] as f32;
            let v = uv[cy * w + cx * 2 + 1] as f32;
            let yy = (yv - 16.0) * 1.1644;
            let r = (yy + 1.5960 * (v - 128.0)).clamp(0.0, 255.0);
            let g = (yy - 0.3917 * (u - 128.0) - 0.8130 * (v - 128.0)).clamp(0.0, 255.0);
            let b = (yy + 2.0172 * (u - 128.0)).clamp(0.0, 255.0);

            let i = oy * SAM_SIZE + ox;
            out[i] = (r - mean[0]) / std[0];
            out[plane + i] = (g - mean[1]) / std[1];
            out[2 * plane + i] = (b - mean[2]) / std[2];
        }
    }
    (out, scale)
}

/// 256 低分辨率 mask（logits，阈值 0）→ 原始分辨率 W×H 二值。
/// 低分辨率域与 1024 输入同原点等比（右下填充），最近邻采样。
pub fn map_low_res_to_frame(low_res: &[f32], scale: f32, w: usize, h: usize, out: &mut [u8]) {
    let lr_w = (((w as f32 * scale).round() as usize) * 256 / SAM_SIZE).max(1);
    let lr_h = (((h as f32 * scale).round() as usize) * 256 / SAM_SIZE).max(1);
    for y in 0..h {
        let sy = y * lr_h / h;
        for x in 0..w {
            let sx = x * lr_w / w;
            out[y * w + x] = (low_res[sy * 256 + sx] > 0.0) as u8;
        }
    }
}

/// SAM2.1 精修器：encoder 每帧一次，decoder 按提示批推理。
pub struct Sam2 {
    encoder: ort::session::Session,
    decoder: ort::session::Session,
    /// 当前帧缓存（set_frame 后有效）。
    embed: Option<Embedding>,
    /// set_frame 时的缩放系数（坐标/低分辨率 mask 映射用）。
    pub scale: f32,
}

struct Embedding {
    image_embed: Vec<f32>,
    embed_shape: [i64; 4],
    hr0: Vec<f32>,
    hr0_shape: [i64; 4],
    hr1: Vec<f32>,
    hr1_shape: [i64; 4],
}

/// 点提示（复核 UI 用）：label 1=前景 0=背景；box 两角点 label 2/3。
#[derive(Debug, Clone, Copy)]
pub struct PointPrompt {
    pub x: f32,
    pub y: f32,
    pub label: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum Sam2Error {
    #[error("SAM 模型文件不存在: {0}")]
    ModelNotFound(std::path::PathBuf),
    #[error("SAM 推理错误: {0}")]
    Ort(#[from] ort::Error),
    #[error("{0}")]
    Load(#[from] DetectError),
    #[error("输出形状异常: masks {0:?}")]
    BadShape(Vec<i64>),
    #[error("尚未调用 set_frame（无图像嵌入缓存）")]
    NoFrame,
}

impl Sam2 {
    /// 加载 SAM2.1。⚠️ **恒走 CPU EP（无视 device 参数）**：2026-08-21 实测
    /// SAM2.1-large 走 CoreML EP（auto/gpu 均然）存在每帧上下文泄漏
    /// （stderr 刷 "Context leak detected"）+ 病理性慢（首帧 ~35 分钟），
    /// 内存随帧单调膨胀直至 swap 写满盘；tiny（109MB）则无恙——CoreML EP
    /// 对该大 transformer 的实现问题。macOS 恒回退 CPU EP（实测 3-6s/帧
    /// large，档案级预算内）；Linux 上无此问题，device 正常透传
    /// （webgpu=RDNA 实验路径，2026-08-21 起）。macOS 恒回退 CPU，
    /// 参数仅 Linux 消费。
    #[cfg_attr(target_os = "macos", allow(unused_variables))]
    pub fn load(encoder: &Path, decoder: &Path, device: &str) -> Result<Self, Sam2Error> {
        if !encoder.exists() {
            return Err(Sam2Error::ModelNotFound(encoder.to_path_buf()));
        }
        if !decoder.exists() {
            return Err(Sam2Error::ModelNotFound(decoder.to_path_buf()));
        }
        #[cfg(target_os = "macos")]
        let device = "cpu";
        Ok(Self {
            encoder: commit(device, encoder)?,
            decoder: commit(device, decoder)?,
            embed: None,
            scale: 1.0,
        })
    }

    /// 缓存是否已就绪（当前帧已编码）。
    pub fn frame_ready(&self) -> bool {
        self.embed.is_some()
    }

    /// 编码当前帧（每帧一次；后续 refine_boxes/prompt_points 复用缓存）。
    pub fn set_frame(&mut self, nv12: &[u8], w: usize, h: usize) -> Result<(), Sam2Error> {
        let (input, scale) = nv12_to_sam_input(nv12, w, h);
        let outputs = self.encoder.run(ort::inputs! {
            "image" => ort::value::Tensor::from_array((
                [1i64, 3, SAM_SIZE as i64, SAM_SIZE as i64],
                input,
            ))?,
        })?;
        let ex = |name: &str| -> Result<(Vec<f32>, [i64; 4]), Sam2Error> {
            let (s, t) = outputs[name].try_extract_tensor::<f32>()?;
            if s.len() != 4 {
                return Err(Sam2Error::NoFrame);
            }
            Ok((t.to_vec(), [s[0], s[1], s[2], s[3]]))
        };
        let (image_embed, embed_shape) = ex("image_embed")?;
        let (hr0, hr0_shape) = ex("high_res_feats_0")?;
        let (hr1, hr1_shape) = ex("high_res_feats_1")?;
        self.embed = Some(Embedding { image_embed, embed_shape, hr0, hr0_shape, hr1, hr1_shape });
        self.scale = scale;
        Ok(())
    }

    /// 批量 box prompting（num_labels 维批量，每框一次 decoder）：
    /// 返回每框 (W×H 二值 mask, argmax 候选的 IoU 预测)。
    pub fn refine_boxes(
        &mut self,
        boxes: &[[f32; 4]],
        w: usize,
        h: usize,
    ) -> Result<Vec<(Vec<u8>, f32)>, Sam2Error> {
        if boxes.is_empty() {
            return Ok(vec![]);
        }
        let s = self.scale;
        // box → [n,2,2] 两角点（label 2/3）
        let mut coords_n = Vec::with_capacity(boxes.len() * 4);
        for b in boxes {
            coords_n.extend_from_slice(&[b[0] * s, b[1] * s, b[2] * s, b[3] * s]);
        }
        let labels_n: Vec<f32> = boxes.iter().flat_map(|_| [2.0f32, 3.0f32]).collect();
        let n = boxes.len() as i64;
        let (lows, ious) = self.decode(n, &coords_n, &labels_n, 2)?;
        let mut out = Vec::with_capacity(boxes.len());
        for (i, low) in lows.iter().enumerate() {
            let mut mask = vec![0u8; w * h];
            map_low_res_to_frame(low, s, w, h, &mut mask);
            out.push((mask, ious[i]));
        }
        Ok(out)
    }

    /// 单框便捷封装：返回 (W×H mask, IoU 预测)。
    pub fn refine_box(&mut self, xyxy: [f32; 4], w: usize, h: usize) -> Result<(Vec<u8>, f32), Sam2Error> {
        Ok(self
            .refine_boxes(&[xyxy], w, h)?
            .pop()
            .expect("非空输入必有输出"))
    }

    /// 自由点提示（复核 UI 加/减点）：可选附加 box。点为原始分辨率像素。
    pub fn prompt_points(
        &mut self,
        points: &[PointPrompt],
        box_xyxy: Option<[f32; 4]>,
        w: usize,
        h: usize,
    ) -> Result<(Vec<u8>, f32), Sam2Error> {
        let s = self.scale;
        let mut coords = Vec::new();
        let mut labels = Vec::new();
        for p in points {
            coords.extend_from_slice(&[p.x * s, p.y * s]);
            labels.push(p.label as f32);
        }
        if let Some(b) = box_xyxy {
            coords.extend_from_slice(&[b[0] * s, b[1] * s, b[2] * s, b[3] * s]);
            labels.extend_from_slice(&[2.0f32, 3.0f32]);
        }
        let (lows, ious) = self.decode(1, &coords, &labels, coords.len() / 2)?;
        let mut mask = vec![0u8; w * h];
        map_low_res_to_frame(&lows[0], s, w, h, &mut mask);
        Ok((mask, ious[0]))
    }

    /// decoder 批推理。返回 (每组 argmax-IoU 候选的低分辨率 logits, 该组最优 IoU)。
    /// `points_per_group` = 每提示组的点数（shape 信息）。
    fn decode(
        &mut self,
        n: i64,
        coords: &[f32],
        labels: &[f32],
        points_per_group: usize,
    ) -> Result<(Vec<Vec<f32>>, Vec<f32>), Sam2Error> {
        let emb = self.embed.as_ref().ok_or(Sam2Error::NoFrame)?;
        debug_assert_eq!(coords.len(), n as usize * points_per_group * 2);
        debug_assert_eq!(labels.len(), n as usize * points_per_group);
        let masks_in = vec![0f32; (n * 256 * 256) as usize];
        let outputs = self.decoder.run(ort::inputs! {
            "image_embed" => ort::value::Tensor::from_array((emb.embed_shape, emb.image_embed.clone()))?,
            "high_res_feats_0" => ort::value::Tensor::from_array((emb.hr0_shape, emb.hr0.clone()))?,
            "high_res_feats_1" => ort::value::Tensor::from_array((emb.hr1_shape, emb.hr1.clone()))?,
            "point_coords" => ort::value::Tensor::from_array((
                [n, points_per_group as i64, 2],
                coords.to_vec(),
            ))?,
            "point_labels" => ort::value::Tensor::from_array((
                [n, points_per_group as i64],
                labels.to_vec(),
            ))?,
            "mask_input" => ort::value::Tensor::from_array(([n, 1, 256, 256], masks_in))?,
            "has_mask_input" => ort::value::Tensor::from_array(([n], vec![0f32; n as usize]))?,
        })?;
        let (ms, mt) = outputs["masks"].try_extract_tensor::<f32>()?;
        let (_, it) = outputs["iou_predictions"].try_extract_tensor::<f32>()?;
        if ms.len() != 4 || ms[0] != n {
            return Err(Sam2Error::BadShape(ms.to_vec()));
        }
        let cand = ms[1] as usize; // 3 候选
        let plane = 256 * 256;
        let mut lows = Vec::with_capacity(n as usize);
        let mut best_ious = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let mut best = 0usize;
            for c in 1..cand {
                if it[i * cand + c] > it[i * cand + best] {
                    best = c;
                }
            }
            best_ious.push(it[i * cand + best]);
            lows.push(mt[i * cand * plane + best * plane..i * cand * plane + (best + 1) * plane].to_vec());
        }
        Ok((lows, best_ious))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sam_input_scales_longest_side_and_pads() {
        // 16×8 灰阶帧 → 1024 域：有效区 1024×512，底部为 0 填充
        let (w, h) = (16, 8);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                nv12[y * w + x] = (x * 16) as u8;
            }
        }
        let (input, scale) = nv12_to_sam_input(&nv12, w, h);
        assert!((scale - 64.0).abs() < 1e-6, "scale {}", scale);
        let plane = SAM_SIZE * SAM_SIZE;
        assert!(input[..SAM_SIZE].iter().any(|&v| v != 0.0));
        assert!(input[512 * SAM_SIZE..plane].iter().all(|&v| v == 0.0), "右下零填充");
    }

    #[test]
    fn map_low_res_maps_valid_region() {
        // 1024 帧 @scale=1 → 256 域 1:4 降采样；左上 1/4（128×128）为正 →
        // 帧 (0..512)² 为 1（512/4=128 边界），之外为 0
        let mut low = vec![0f32; 256 * 256];
        for y in 0..128 {
            for x in 0..128 {
                low[y * 256 + x] = 5.0;
            }
        }
        let (w, h) = (1024usize, 1024);
        let mut out = vec![0u8; w * h];
        map_low_res_to_frame(&low, 1.0, w, h, &mut out);
        assert_eq!(out[0], 1);
        assert_eq!(out[511 * w + 511], 1, "sy=127 仍在 1/4 区域");
        assert_eq!(out[512 * w + 0], 0, "sy=128 出界");
        assert_eq!(out[0 * w + 512], 0);
        assert_eq!(out[1023 * w + 1023], 0);
    }

    /// 真实模型回归锚点（SAM2.1 tiny）：已知 clip5s 首帧 + GD person 框
    /// → 精修 mask 非空且 IoU 预测合理、主体落在提示框内。
    #[test]
    fn sam_refines_person_box_on_real_frame() {
        let enc = crate::models::resolve_model("sam2.1-tiny-encoder.onnx");
        let dec = crate::models::resolve_model("sam2.1-tiny-decoder.onnx");
        let video = std::path::Path::new("../../tests/clip5s.mp4");
        if !enc.exists() || !dec.exists() || !video.exists() {
            eprintln!("skip: 无 SAM 模型或测试视频（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut sam = Sam2::load(&enc, &dec, "cpu").expect("加载 SAM");
        sam.set_frame(&nv12, w, h).expect("编码");
        // GD 验证过的 person 框（2026-08-21 数值锚点：score 0.89）
        let box_ = [455.0, 237.0, 848.0, 1039.0];
        let (mask, iou) = sam.refine_box(box_, w, h).expect("精修");
        assert!(iou > 0.5, "IoU 预测 {iou}");
        let area = mask.iter().map(|&v| v as usize).sum::<usize>();
        assert!(area > 10_000, "人物 mask 面积 {area}");
        // mask 主体应落在提示框内（SAM 以框为提示，>80% 像素在框内）
        let in_box = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)).filter(|&(x, y)| mask[y * w + x] == 1))
            .filter(|&(x, y)| {
                (x as f32) >= box_[0] && (x as f32) <= box_[2] && (y as f32) >= box_[1] && (y as f32) <= box_[3]
            })
            .count();
        assert!(in_box as f32 / area as f32 > 0.8, "框内占比 {in_box}/{area}");
    }
}
