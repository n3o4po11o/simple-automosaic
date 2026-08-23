//! OSNet 行人重识别外观嵌入（DESIGN §5.6 步骤 5 / §5.3 BoT-SORT 外观关联）。
//!
//! 模型资产：anriha/osnet-x025-msmt17.onnx（torchreid 官方 osnet_x0_25 MSMT17
//! 训练权重的 ONNX 转换；MSMT17 为最大行人 ReID 数据集，BoT-SORT 默认同源）。
//!
//! 输入 `[16,3,256,128]` 固定批（跟踪场景的批推理导出）：RGB /255 →
//! ImageNet mean/std，letterbox 拉伸到 256×128（torchreid transforms.Resize 语义）。
//! 输出 `[16,512]` 嵌入向量——L2 归一化后余弦相似度做关联代价。
//! 单实例嵌入 = 复制 16 份取首行（固定形状模型的固定开销，<1ms）。

use std::path::Path;

use crate::detect::{crop_nv12, DetectError};

const BATCH: usize = 16;
const IN_H: usize = 256;
const IN_W: usize = 128;
const EMB: usize = 512;

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

/// OSNet ReID 嵌入器（可选组件：缺模型时 archive 管线退化为纯 IoU 关联）。
pub struct ReId {
    session: ort::session::Session,
}

#[derive(Debug, thiserror::Error)]
pub enum ReIdError {
    #[error("模型文件不存在: {0}")]
    ModelNotFound(std::path::PathBuf),
    #[error("ort 错误: {0}")]
    Ort(#[from] ort::Error),
    #[error("{0}")]
    Load(#[from] DetectError),
}

impl ReId {
    pub fn load(model: &Path, device: &str) -> Result<Self, ReIdError> {
        if !model.exists() {
            return Err(ReIdError::ModelNotFound(model.to_path_buf()));
        }
        Ok(Self { session: commit(device, model)? })
    }

    /// 裁剪 person 框并嵌入（L2 归一化 512 维）。
    pub fn embed(&mut self, nv12: &[u8], w: usize, h: usize, xyxy: [f32; 4]) -> Result<[f32; EMB], ReIdError> {
        let (cw, ch, _ox, _oy, crop) = crop_nv12(nv12, w, h, xyxy);
        if cw < 8 || ch < 8 {
            return Ok([0.0; EMB]);
        }
        // NV12 crop → RGB 256×128（拉伸；torchreid Resize 语义）
        let plane = IN_W * IN_H;
        let mut input = vec![0f32; BATCH * 3 * plane];
        let y_plane = &crop[..cw * ch];
        let uv = &crop[cw * ch..];
        let (chw, chh) = (cw / 2, ch / 2);
        let (sx, sy) = (cw as f32 / IN_W as f32, ch as f32 / IN_H as f32);
        let (mean, std) = ([0.485f32, 0.456, 0.406], [0.229f32, 0.224, 0.225]);
        for oy in 0..IN_H {
            let fy = (oy as f32 + 0.5) * sy - 0.5;
            let y0 = fy.floor().max(0.0) as usize;
            let iy0 = y0.min(ch - 1);
            let iy1 = (y0 + 1).min(ch - 1);
            let dy = (fy - y0 as f32).clamp(0.0, 1.0);
            let cy = (((fy + 0.5) * 0.5) as usize).min(chh - 1);
            for ox in 0..IN_W {
                let fx = (ox as f32 + 0.5) * sx - 0.5;
                let x0 = fx.floor().max(0.0) as usize;
                let ix0 = x0.min(cw - 1);
                let ix1 = (x0 + 1).min(cw - 1);
                let dx = (fx - x0 as f32).clamp(0.0, 1.0);
                let cx = (((fx + 0.5) * 0.5) as usize).min(chw - 1);

                let y00 = y_plane[iy0 * cw + ix0] as f32;
                let y10 = y_plane[iy0 * cw + ix1] as f32;
                let y01 = y_plane[iy1 * cw + ix0] as f32;
                let y11 = y_plane[iy1 * cw + ix1] as f32;
                let yv = y00 * (1.0 - dx) * (1.0 - dy)
                    + y10 * dx * (1.0 - dy)
                    + y01 * (1.0 - dx) * dy
                    + y11 * dx * dy;
                let u = uv[cy * cw + cx * 2] as f32;
                let v = uv[cy * cw + cx * 2 + 1] as f32;
                let yy = (yv - 16.0) * 1.1644;
                let r = (yy + 1.5960 * (v - 128.0)).clamp(0.0, 255.0) / 255.0;
                let g = (yy - 0.3917 * (u - 128.0) - 0.8130 * (v - 128.0)).clamp(0.0, 255.0) / 255.0;
                let b = (yy + 2.0172 * (u - 128.0)).clamp(0.0, 255.0) / 255.0;

                let i = oy * IN_W + ox;
                input[i] = (r - mean[0]) / std[0];
                input[plane + i] = (g - mean[1]) / std[1];
                input[2 * plane + i] = (b - mean[2]) / std[2];
            }
        }
        // 固定批 16：同一 crop 复制（首行即结果）
        let batched: Vec<f32> = input.iter().copied().cycle().take(BATCH * 3 * plane).collect();
        let outputs = self.session.run(ort::inputs! {
            "input" => ort::value::Tensor::from_array((
                [BATCH as i64, 3, IN_H as i64, IN_W as i64],
                batched,
            ))?,
        })?;
        let (_, t) = outputs["output"].try_extract_tensor::<f32>()?;
        let mut emb = [0f32; EMB];
        let row = &t[..EMB];
        let norm = row.iter().map(|&v| v * v).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for (e, &v) in emb.iter_mut().zip(row) {
                *e = v / norm;
            }
        }
        Ok(emb)
    }
}

/// 余弦相似度（输入应为 L2 归一化向量；未归一化时结果钳制到 [-1,1]）。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (na, nb) = (
        a.iter().map(|&v| v * v).sum::<f32>().sqrt(),
        b.iter().map(|&v| v * v).sum::<f32>().sqrt(),
    );
    if na < 1e-6 || nb < 1e-6 {
        return 0.0;
    }
    let dot = a.iter().zip(b).map(|(&x, &y)| x * y).sum::<f32>();
    (dot / (na * nb)).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_basics() {
        let a = [1.0f32, 0.0];
        assert!((cosine(&a, &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine(&a, &[0.0, 1.0]) - 0.0).abs() < 1e-6);
        assert!((cosine(&a, &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine(&a, &[0.0; 2]), 0.0, "零向量安全");
    }

    /// 真实模型回归锚点：同一人的两次裁剪（小幅平移）相似度 > 不同区域。
    #[test]
    fn embed_same_person_more_similar_than_background() {
        let model = crate::models::resolve_model("osnet-x025-msmt17.onnx");
        let video = std::path::Path::new("../../tests/clip5s.mp4");
        if !model.exists() || !video.exists() {
            eprintln!("skip: 无 ReID 模型或测试视频（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut reid = ReId::load(&model, "cpu").expect("加载");
        let person = [455.0, 237.0, 848.0, 1039.0]; // GD 验证过的 person
        let person_shift = [465.0, 247.0, 858.0, 1049.0]; // +10px 平移
        let bg = [1200.0, 800.0, 1700.0, 1070.0]; // 背景区域
        let e1 = reid.embed(&nv12, w, h, person).expect("e1");
        let e2 = reid.embed(&nv12, w, h, person_shift).expect("e2");
        let e3 = reid.embed(&nv12, w, h, bg).expect("e3");
        let same = cosine(&e1, &e2);
        let diff = cosine(&e1, &e3);
        assert!(same > diff, "同人平移 sim={same} 应 > 背景 sim={diff}");
        assert!(same > 0.5, "同人相似度 {same}");
    }
}
