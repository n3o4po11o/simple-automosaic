//! Grounding DINO 开放词汇人体检测（DESIGN §5.6 管线 A 步骤 1b：ensemble 第二路）。
//!
//! 模型资产：onnx-community/grounding-dino-tiny-ONNX（fp16 变体，IDEA-Research
//! 权重 Apache-2.0）。GD-base 自导出需 transformers 工具链（设计文档 §5.6 降级链
//! 明示 tiny 为现成替代）；fp16 输入接口仍为 f32（onnx-community 导出惯例）。
//!
//! 输入（2026-08-21 真实帧验证的规格）：
//! - `pixel_values [1,3,800,800]`：长边 800 letterbox（右下零填充，填充值=归一化后 0）
//!   → RGB /255 → ImageNet mean/std；`pixel_mask` 有效区 1。
//! - 文本 "person."（Transformers.js 惯例：小写+句点）BERT 分词离线预计算：
//!   `[CLS]=101, "person"=2711, "."=1012, [SEP]=102` + 0 填充到 16。
//! 输出：`logits [1,900,256]`、`pred_boxes [1,900,4]`（cxcywh，**原帧归一化坐标**
//! ——pixel_mask 标记有效区，模型预测以原图为基准，squash/letterbox 两变体
//! 实测同框可证）。score = sigmoid(max(logits[q, 1..=2]))（词 token 位）。

use std::path::Path;

use crate::detect::DetectError;

/// GD-tiny 输入边长。
const GD_SIZE: usize = 800;
/// 文本序列长度（"person." 4 token + 0 填充）。
const TEXT_LEN: usize = 16;
/// "person." 的 BERT-base-uncased 分词（2026-08-21 tokenizer.json 核对）。
const TEXT_IDS: [i64; TEXT_LEN] = [101, 2711, 1012, 102, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// 有效 token 数（attention mask 的 1 长度）。
const TEXT_VALID: usize = 4;

fn gd_mean() -> [f32; 3] {
    [0.485, 0.456, 0.406]
}

fn gd_std() -> [f32; 3] {
    [0.229, 0.224, 0.225]
}

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

/// NV12 → GD 输入：长边 800 等比缩放 + 右下零填充（归一化后 0）
/// → RGB /255 → ImageNet mean/std → CHW f32。返回 (张量, 有效宽, 有效高)。
fn nv12_to_gd_input(nv12: &[u8], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let scale = GD_SIZE as f32 / w.max(h) as f32;
    let new_w = (w as f32 * scale).round() as usize;
    let new_h = (h as f32 * scale).round() as usize;
    let plane = GD_SIZE * GD_SIZE;
    let mut out = vec![0f32; 3 * plane];
    let (mean, std) = (gd_mean(), gd_std());
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

            let i = oy * GD_SIZE + ox;
            out[i] = (r / 255.0 - mean[0]) / std[0];
            out[plane + i] = (g / 255.0 - mean[1]) / std[1];
            out[2 * plane + i] = (b / 255.0 - mean[2]) / std[2];
        }
    }
    (out, new_w, new_h)
}

/// 开放词汇 "person" 检测器（ensemble 第二路）。
pub struct GroundingDino {
    session: ort::session::Session,
    pub conf: f32,
}

/// 单个 person 假设（原帧像素 xyxy）。
#[derive(Debug, Clone, Copy)]
pub struct GdBox {
    pub xyxy: [f32; 4],
    pub score: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum GdError {
    #[error("模型文件不存在: {0}（设置中下载或 scripts/fetch_m5_models.sh）")]
    ModelNotFound(std::path::PathBuf),
    #[error("ort 错误: {0}")]
    Ort(#[from] ort::Error),
    #[error("{0}")]
    Load(#[from] DetectError),
    #[error("输出形状异常: logits {0:?}")]
    BadShape(Vec<i64>),
}

impl GroundingDino {
    pub fn load(model: &Path, device: &str, conf: f32) -> Result<Self, GdError> {
        if !model.exists() {
            return Err(GdError::ModelNotFound(model.to_path_buf()));
        }
        Ok(Self { session: commit(device, model)?, conf })
    }

    /// 检测 person 框（原帧像素坐标）。
    pub fn detect_persons(&mut self, nv12: &[u8], w: usize, h: usize) -> Result<Vec<GdBox>, GdError> {
        let (pixel_values, vw, vh) = nv12_to_gd_input(nv12, w, h);
        let mut pixel_mask = vec![0i64; GD_SIZE * GD_SIZE];
        for y in 0..vh {
            pixel_mask[y * GD_SIZE..y * GD_SIZE + vw].fill(1);
        }
        let attn: Vec<i64> = (0..TEXT_LEN).map(|i| (i < TEXT_VALID) as i64).collect();
        let outputs = self.session.run(ort::inputs! {
            "pixel_values" => ort::value::Tensor::from_array((
                [1i64, 3, GD_SIZE as i64, GD_SIZE as i64],
                pixel_values,
            ))?,
            "input_ids" => ort::value::Tensor::from_array(([1i64, TEXT_LEN as i64], TEXT_IDS.to_vec()))?,
            "token_type_ids" => ort::value::Tensor::from_array(([1i64, TEXT_LEN as i64], vec![0i64; TEXT_LEN]))?,
            "attention_mask" => ort::value::Tensor::from_array(([1i64, TEXT_LEN as i64], attn))?,
            "pixel_mask" => ort::value::Tensor::from_array(([1i64, GD_SIZE as i64, GD_SIZE as i64], pixel_mask))?,
        })?;
        let (ls, lt) = outputs["logits"].try_extract_tensor::<f32>()?;
        let (bs, bt) = outputs["pred_boxes"].try_extract_tensor::<f32>()?;
        if ls.len() != 3 || bs.len() != 3 || bs[2] != 4 {
            return Err(GdError::BadShape(ls.to_vec()));
        }
        let (q, _) = (ls[1] as usize, ls[2] as usize);
        let mut out = Vec::new();
        for i in 0..q {
            // 词 token 位 1..=2（"person"、"."）取 max → sigmoid
            let a = lt[i * ls[2] as usize + 1];
            let b = lt[i * ls[2] as usize + 2];
            let score = 1.0 / (1.0 + (-a.max(b)).exp());
            if score < self.conf {
                continue;
            }
            // pred_boxes = cxcywh（原帧归一化）→ xyxy 像素
            let o = i * 4;
            let (cx, cy, bw, bh) = (bt[o], bt[o + 1], bt[o + 2], bt[o + 3]);
            out.push(GdBox {
                xyxy: [
                    ((cx - bw * 0.5) * w as f32).clamp(0.0, w as f32),
                    ((cy - bh * 0.5) * h as f32).clamp(0.0, h as f32),
                    ((cx + bw * 0.5) * w as f32).clamp(0.0, w as f32),
                    ((cy + bh * 0.5) * h as f32).clamp(0.0, h as f32),
                ],
                score,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实模型回归锚点：clip5s 首帧 GD person = score≈0.89、框 (455,237)-(848,1039)
    ///（2026-08-21 Python 对照验证的数值锚点）。
    #[test]
    fn gd_detects_person_on_real_frame() {
        let model = crate::models::resolve_model("grounding-dino-tiny.onnx");
        let video = std::path::Path::new("../../tests/clip5s.mp4");
        if !model.exists() || !video.exists() {
            eprintln!("skip: 无 GD 模型或测试视频（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut gd = GroundingDino::load(&model, "cpu", 0.35).expect("加载 GD");
        let dets = gd.detect_persons(&nv12, w, h).expect("推理");
        assert!(!dets.is_empty(), "clip5s 首帧应检出 person");
        let top = dets.iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap()).unwrap();
        assert!(top.score > 0.6, "首假设分数 {}", top.score);
        let (x1, y1, x2, y2) = (top.xyxy[0], top.xyxy[1], top.xyxy[2], top.xyxy[3]);
        // Python 锚点 (455,237,844,1035)：±40px 容差（fp16 导出与预处理微差）
        assert!((x1 - 455.0).abs() < 40.0, "x1={x1}");
        assert!((y1 - 237.0).abs() < 40.0, "y1={y1}");
        assert!((x2 - 844.0).abs() < 40.0, "x2={x2}");
        assert!((y2 - 1035.0).abs() < 40.0, "y2={y2}");
    }
}
