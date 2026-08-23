//! 五档质量预设（DESIGN §5.5）：预设 → 模型与管线参数的映射。
//!
//! 模型文件名对应 `models/manifest.json` 条目；实际路径解析与可用性检查见
//! [`crate::models`]。极限·档案级（Archive，M5）展开为 ensemble 模型组
//! （YOLO26x@1536 + Grounding DINO + SAM2.1 + RetinaFace + OSNet），管线本体
//! 在 [`crate::archive`]，消费入口为两阶段 analyze/render。

/// 质量预设档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QualityPreset {
    /// 速度：yolo26n 检测框 + margin（免 mask 头）。全档逐帧检测
    /// （2026-08-20 用户决策：隔帧的时序滞后是拖影根源，吞吐换画质；
    /// 隔帧机制保留为高级覆写）。
    Speed,
    /// 均衡（默认）：yolo26n-seg + mask 平滑，全帧检测。
    Balanced,
    /// 准确：yolo26s-seg @960 全帧检测。
    Accurate,
    /// 极致：yolo26x-seg @1280 全帧检测（GPU 推理推荐）。
    Extreme,
    /// 极限·档案级：ensemble + SAM2.1 精修（M5，DESIGN §5.6）。
    /// 两阶段：analyze（[`crate::archive`]）→ 复核 → render。
    Archive,
}

impl QualityPreset {
    pub const ALL: [QualityPreset; 5] =
        [Self::Speed, Self::Balanced, Self::Accurate, Self::Extreme, Self::Archive];

    /// 稳定 id（FFI/CLI/持久化用）。
    pub fn id(&self) -> &'static str {
        match self {
            Self::Speed => "speed",
            Self::Balanced => "balanced",
            Self::Accurate => "accurate",
            Self::Extreme => "extreme",
            Self::Archive => "archive",
        }
    }

    /// 人读名称。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Speed => "速度",
            Self::Balanced => "均衡",
            Self::Accurate => "准确",
            Self::Extreme => "极致",
            Self::Archive => "极限·档案级",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.id() == s)
    }

    /// 预设对应的管线参数。Archive 档展开为 ensemble 模型组
    /// （body_model 为主检；archive 字段含其余组件；face_model 留空——
    /// 该档人脸线由 retina 组件承担，不走 yolo-face）。
    pub fn params(&self) -> Result<PresetParams, String> {
        match self {
            Self::Speed => Ok(PresetParams {
                body_model: "yolo26n.onnx".into(),
                // YuNet（OpenCV Zoo，MIT）：75K 参数 CPU 亚毫秒（DESIGN §5.2 速度
                // 档兜底）；缺失时 resolve_first 回退 yolo11n-face-pose/yolov8n-face
                face_model: "face_detection_yunet_2023mar.onnx".into(),
                conf: 0.35,
                ..Default::default()
            }),
            Self::Balanced => Ok(PresetParams {
                body_model: "yolo26n-seg.onnx".into(),
                face_model: "yolo11n-face-pose.onnx".into(),
                conf: 0.35,
                ..Default::default()
            }),
            Self::Accurate => Ok(PresetParams {
                body_model: "yolo26s-seg.onnx".into(),
                face_model: "yolo11s-face-pose.onnx".into(),
                conf: 0.30,
                detect_every: 1,
                ..Default::default()
            }),
            Self::Extreme => Ok(PresetParams {
                body_model: "yolo26x-seg.onnx".into(),
                face_model: "yolo11s-face-pose.onnx".into(),
                conf: 0.30,
                detect_every: 1,
                // 极致档三件套：头部级联 ROI + 翻转 TTA（DESIGN §5.5）
                face_roi: true,
                tta: true,
                ..Default::default()
            }),
            Self::Archive => Ok(PresetParams {
                body_model: "yolo26x-seg-1536.onnx".into(),
                face_model: String::new(), // 人脸线 = retina 组件
                conf: 0.25,
                detect_every: 1,
                tta: true,
                archive: Some(ArchiveModelRefs::default()),
                ..Default::default()
            }),
        }
    }
}

/// Archive 档（M5）的 ensemble 模型组引用（文件名，经 [`crate::models::resolve_model`] 解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveModelRefs {
    /// 开放词汇第二路检测器（"person." 文本提示）。
    pub gd: &'static str,
    /// mask 精修 encoder（large 为档案级默认；tiny/small 开发调试）。
    pub sam_encoder: &'static str,
    pub sam_decoder: &'static str,
    /// 滑窗人脸线。
    pub retina: &'static str,
    /// 外观关联嵌入（缺失自动退化纯 IoU）。
    pub reid: &'static str,
}

impl Default for ArchiveModelRefs {
    fn default() -> Self {
        Self {
            gd: "grounding-dino-tiny.onnx",
            sam_encoder: "sam2.1-large-encoder.onnx",
            sam_decoder: "sam2.1-large-decoder.onnx",
            retina: "retinaface-r34.onnx",
            reid: "osnet-x025-msmt17.onnx",
        }
    }
}

/// 预设展开后的管线参数（FFI/CLI 的批/隔帧/人脸开关等以此为准）。
#[derive(Debug, Clone)]
pub struct PresetParams {
    /// 人体模型文件名（models/ 下）。
    pub body_model: String,
    /// 人脸模型文件名；找不到时逐级回退（见 [`crate::models::resolve_first`]）。
    pub face_model: String,
    pub conf: f32,
    /// 隔帧检测间隔：每 N 帧推理一次。
    pub detect_every: u32,
    /// 批推理大小（需存在 -b{N} 固定批模型，否则自动逐帧）。
    pub batch: u32,
    /// 是否启用人脸检测。
    pub face: bool,
    /// 人脸框四周外扩像素。
    pub face_expand: u32,
    /// 是否启用 IoU 跟踪（隔帧档必须开：中间帧靠跟踪保持）。
    pub track: bool,
    /// 是否启用 mask 时序平滑。
    pub smooth: bool,
    /// 人脸级联 ROI（极致档默认开：person 头部裁剪放大二次推理，小脸召回）。
    pub face_roi: bool,
    /// 翻转 TTA（极致档默认开：+0.3~0.8 AP 召回，推理 ×2）。
    pub tta: bool,
    /// Archive（M5）ensemble 模型组；其他档为 None。
    pub archive: Option<ArchiveModelRefs>,
}

impl Default for PresetParams {
    fn default() -> Self {
        Self {
            body_model: String::new(),
            face_model: String::new(),
            conf: 0.35,
            detect_every: 1,
            batch: 4,
            face: true,
            face_expand: 12,
            track: true,
            smooth: true,
            face_roi: false,
            tta: false,
            archive: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_roundtrip() {
        for p in QualityPreset::ALL {
            assert_eq!(QualityPreset::from_id(p.id()), Some(p));
        }
        assert_eq!(QualityPreset::from_id("bogus"), None);
    }

    #[test]
    fn speed_uses_box_model_full_frame() {
        let p = QualityPreset::Speed.params().unwrap();
        assert_eq!(p.body_model, "yolo26n.onnx");
        assert_eq!(p.face_model, "face_detection_yunet_2023mar.onnx", "速度档人脸 = YuNet");
        assert_eq!(p.detect_every, 1, "全档逐帧（2026-08-20 决策）");
        assert!(p.track);
    }

    #[test]
    fn balanced_is_n_seg_full_frame() {
        let p = QualityPreset::Balanced.params().unwrap();
        assert_eq!(p.body_model, "yolo26n-seg.onnx");
        assert_eq!(p.detect_every, 1, "全档逐帧（2026-08-20 决策）");
        assert!(p.smooth);
    }

    #[test]
    fn accurate_extreme_full_frame() {
        assert_eq!(QualityPreset::Accurate.params().unwrap().detect_every, 1);
        assert_eq!(QualityPreset::Extreme.params().unwrap().detect_every, 1);
    }

    #[test]
    fn tta_only_on_extreme() {
        assert!(!QualityPreset::Speed.params().unwrap().tta);
        assert!(!QualityPreset::Balanced.params().unwrap().tta);
        assert!(!QualityPreset::Accurate.params().unwrap().tta);
        assert!(QualityPreset::Extreme.params().unwrap().tta);
    }

    #[test]
    fn archive_expands_ensemble_models() {
        let p = QualityPreset::Archive.params().unwrap();
        assert_eq!(p.body_model, "yolo26x-seg-1536.onnx");
        assert!(p.face_model.is_empty(), "Archive 人脸线 = retina 组件");
        let a = p.archive.expect("ensemble 模型组");
        assert_eq!(a.gd, "grounding-dino-tiny.onnx");
        assert_eq!(a.sam_encoder, "sam2.1-large-encoder.onnx");
        assert_eq!(a.retina, "retinaface-r34.onnx");
        assert_eq!(a.reid, "osnet-x025-msmt17.onnx");
        assert!(p.tta);
        // 其他档不受影响
        assert!(QualityPreset::Balanced.params().unwrap().archive.is_none());
    }
}
