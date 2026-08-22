//! automosaic-cli：核心库的命令行入口（M1）。
//!
//! 子命令：probe / hwaccel / models（模型清点与校验）/ transcode（直通冒烟）/
//! process（检测+打码全管线）/ analyze + render（archive 两阶段）/
//! queue（批处理）/ debug（调试运行与参数扫描）。

mod debug;

use automosaic_core::compose::MaskStyle;
use automosaic_core::detect::{Detector, FaceDetector};
use automosaic_core::media;
use automosaic_core::models as model_store;
use automosaic_core::mosaic;
use automosaic_core::pipe;
use automosaic_core::preset::QualityPreset;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(
    name = "automosaic-cli",
    // 版本与 app 对齐：构建期由 build.rs 从 app/pubspec.yaml 注入（scripts/version.sh 单一事实源）
    version = env!("AUTOMOSAIC_VERSION"),
    about = "AutoMosaic Studio 核心 CLI（M1：NV12 管线 + YOLO-seg 推理）"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 探测视频元数据（分辨率/帧率/总帧数/音轨）
    Probe { input: PathBuf },
    /// 列出本机 ffmpeg 可用的 hwaccel 与硬件编码器
    Hwaccel,
    /// 模型管理：清点五档预所需模型（list）/ 按 manifest 校验 SHA256（verify）
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// NV12 直通转码（M1-media：验证设计 §3.2 管道，无推理）
    Transcode {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// auto = 按平台候选链取第一个可用；none = 软解；或显式名（videotoolbox/cuda/...）
        #[arg(long, default_value = "auto")]
        hwaccel: String,
        /// auto = 按平台候选链取第一个可用；或显式名（h264_videotoolbox/libx264/...）
        #[arg(long, default_value = "auto")]
        encoder: String,
        /// 目标码率；auto = 按分辨率档位缩放（1080p 基准 6M）
        #[arg(long, default_value = "auto")]
        bitrate: String,
    },
    /// 两阶段·分析（DESIGN §5.6 M5 骨架）：逐帧 mask RLE 落盘，可中断续跑
    Analyze {
        #[arg(short, long)]
        input: PathBuf,
        /// mask 缓存目录（自动创建；已有缓存则续跑）
        #[arg(short, long)]
        masks: PathBuf,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        model: Option<PathBuf>,
        #[arg(long)]
        conf: Option<f32>,
        #[arg(long, default_value = "auto")]
        device: String,
        #[arg(long)]
        face_model: Option<PathBuf>,
        #[arg(long)]
        no_face: bool,
        #[arg(long)]
        face_expand: Option<u32>,
        #[arg(long)]
        batch: Option<u32>,
        #[arg(long)]
        detect_every: Option<u32>,
        #[arg(long)]
        face_roi: bool,
        #[arg(long)]
        tta: bool,
        #[arg(long = "no-tta")]
        no_tta: bool,
        #[arg(long)]
        gmc: bool,
        /// archive 档 SAM2.1 精修模型规格：large（默认，档案级）/ tiny（开发调试）
        #[arg(long, default_value = "large")]
        sam_size: String,
        /// 分析段编码侧输出处理：null（默认，-f null 帧直接丢弃不落盘）/
        /// file（真编码写探针视频后删除——调试管线用）
        #[arg(long, default_value = "null")]
        drain: String,
        #[arg(long, default_value = "auto")]
        hwaccel: String,
        #[arg(long, default_value = "auto")]
        encoder: String,
    },
    /// 两阶段·渲染：读 mask 缓存纯合成+编码（不推理；样式/强度可改）
    Render {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// analyze 产出的 mask 缓存目录
        #[arg(short, long)]
        masks: PathBuf,
        #[arg(long, default_value = "mosaic")]
        style: String,
        #[arg(long, default_value_t = 35)]
        strength: u32,
        #[arg(long, default_value = "auto")]
        hwaccel: String,
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long, default_value = "auto")]
        bitrate: String,
    },
    /// 算法调试：跑管线并输出逐帧检测/跟踪/覆盖率报告（免 GUI 迭代）
    Debug {
        #[command(subcommand)]
        cmd: DebugCmd,
    },
    /// 检测人体并打码（M3：YOLO-seg person + 人脸 + IoU 跟踪 + 时序平滑）
    Process {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// 质量预设：speed / balanced / accurate / extreme / archive
        /// （展开为默认模型与参数，与下方显式参数二选一；显式参数优先。
        /// archive 档为两阶段语义，此处会拒绝——用 analyze + render）
        #[arg(long)]
        preset: Option<String>,
        /// ONNX 模型路径（默认取预设；无预设时 yolo11n-seg）
        #[arg(long)]
        model: Option<PathBuf>,
        /// 置信度阈值（默认取预设）
        #[arg(long)]
        conf: Option<f32>,
        /// 推理设备：auto（macOS=CoreML 全单元） / gpu（CoreML CPU+GPU） / ane（CoreML CPU+NPU） / cpu
        #[arg(long, default_value = "auto")]
        device: String,
        /// 遮罩样式：mosaic / blur / solid
        #[arg(long, default_value = "mosaic")]
        style: String,
        /// mosaic=格边长；blur=模糊半径
        #[arg(long, default_value_t = 35)]
        strength: u32,
        #[arg(long, default_value = "auto")]
        hwaccel: String,
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long, default_value = "auto")]
        bitrate: String,
        /// 人脸模型路径（默认取预设；--no-face 关闭人脸）
        #[arg(long)]
        face_model: Option<PathBuf>,
        /// 人脸框四周外扩像素（默认取预设）
        #[arg(long)]
        face_expand: Option<u32>,
        #[arg(long)]
        no_face: bool,
        /// 关闭跟踪（逐帧独立检测）
        #[arg(long)]
        no_track: bool,
        /// 关闭 mask 时序平滑
        #[arg(long)]
        no_smooth: bool,
        /// 关闭 landmark 外扩（眼距自适应；关则固定 face-expand）
        #[arg(long)]
        no_landmark_expand: bool,
        /// 关闭 per-ID mask EMA
        #[arg(long)]
        no_mask_ema: bool,
        /// 批推理大小（需存在 *-b{N}.onnx 固定批模型；不存在自动回退逐帧）
        #[arg(long)]
        batch: Option<u32>,
        /// 隔帧检测间隔：每 N 帧推理一次，中间帧用跟踪保持的遮罩
        #[arg(long)]
        detect_every: Option<u32>,
        /// 人脸级联 ROI：对 person 头部裁剪放大二次推理（小脸召回；极致档默认开）
        #[arg(long)]
        face_roi: bool,
        /// 翻转 TTA 增强：额外跑一次水平翻转推理合并结果（+召回，推理 ×2；极致档默认开）
        #[arg(long)]
        tta: bool,
        /// 关闭翻转 TTA（覆写预设默认）
        #[arg(long = "no-tta")]
        no_tta: bool,
        /// 相位相关全局运动补偿（运动镜头：预测框平移 + 保持帧遮罩跟随；静机位自动零位移）
        #[arg(long)]
        gmc: bool,
        /// 自适应降档（opt-in）：推理跟不上实时时先撤批 session、再逐步隔帧
        /// （上限 3）——低配机器的吞吐保底；默认关（全档逐帧是既定画质决策）
        #[arg(long)]
        adaptive: bool,
        /// 关闭 OC-SORT 观测中心重更新（丢失后重关联的回滚重放；关则标准 KF 更新）
        #[arg(long)]
        no_ocru: bool,
        /// 解码管道帧格式：nv12（默认） / mjpeg（低配可选：管道带宽 ~1/20，
        /// 代价是 JPEG 编解码 CPU 与轻微画质损失，DESIGN §3.2）
        #[arg(long, default_value = "nv12")]
        pipe: String,
    },
    /// 批处理队列（DESIGN §2.2 JobManager 的 CLI 消费面）：多视频串行处理，
    /// 单个失败不中断后续（状态机在 core::job）
    Queue {
        /// 输入视频（可多个）
        #[arg(short, long = "input", num_args = 1.., required = true)]
        inputs: Vec<PathBuf>,
        /// 输出目录（自动创建；产物 = <原文件名>_mosaic.mp4）
        #[arg(short, long)]
        out_dir: PathBuf,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long, default_value = "auto")]
        device: String,
        #[arg(long)]
        conf: Option<f32>,
        #[arg(long, default_value = "mosaic")]
        style: String,
        #[arg(long, default_value_t = 35)]
        strength: u32,
        #[arg(long, default_value = "auto")]
        hwaccel: String,
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long, default_value = "auto")]
        bitrate: String,
        #[arg(long)]
        no_face: bool,
    },
}

/// process 子命令的预设/显式参数合并结果。
#[derive(Debug, Clone)]
struct EffectiveParams {
    model: PathBuf,
    face_model: Option<PathBuf>,
    conf: f32,
    face_expand: u32,
    batch: u32,
    detect_every: u32,
    face_roi: bool,
    tta: bool,
    preset_label: String,
}

/// --preset 与显式参数合并：显式参数优先，缺省取预设展开值。
#[allow(clippy::too_many_arguments)]
fn merge_params(
    preset: &Option<String>,
    model: &Option<PathBuf>,
    face_model: &Option<PathBuf>,
    conf: &Option<f32>,
    face_expand: &Option<u32>,
    batch: &Option<u32>,
    detect_every: &Option<u32>,
    no_face: bool,
    face_roi_flag: bool,
    tta_flag: Option<bool>,
) -> Result<EffectiveParams, String> {
    let (pp, label) = match preset.as_deref().map(QualityPreset::from_id) {
        Some(Some(p)) => (Some(p.params()?), format!("预设={}", p.label())),
        Some(None) => {
            return Err(format!(
                "未知预设 {}（可选 speed/balanced/accurate/extreme；archive 档走两阶段 analyze → render）",
                preset.as_deref().unwrap_or_default()
            ))
        }
        None => (None, "无预设（显式参数）".to_string()),
    };
    // 预设人脸模型缺失时回退到旧模型（模型管理未下载全的场景）
    let preset_face = pp.as_ref().map(|p| {
        model_store::resolve_first(&[&p.face_model, "yolo11n-face-pose.onnx", "yolov8n-face.onnx"])
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.face_model.clone())
    });
    // 预设人体模型：解析到 models/ 实际路径；缺失给出明确指引
    let preset_body = match &pp {
        Some(p) => {
            let resolved = model_store::resolve_model(&p.body_model);
            if !resolved.exists() {
                return Err(format!(
                    "预设[{}]缺少人体模型 {}（运行 scripts/export_models.sh 导出或在设置中下载）",
                    label.trim_start_matches("预设="),
                    p.body_model
                ));
            }
            resolved
        }
        None => PathBuf::from("models/yolo11n-seg.onnx"),
    };
    Ok(EffectiveParams {
        model: model.clone().unwrap_or(preset_body),
        face_model: if no_face {
            None
        } else {
            Some(
                face_model.clone().unwrap_or_else(|| {
                    preset_face.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("models/yolov8n-face.onnx"))
                }),
            )
        },
        conf: conf.unwrap_or_else(|| pp.as_ref().map_or(0.35, |p| p.conf)),
        face_expand: face_expand.unwrap_or_else(|| pp.as_ref().map_or(12, |p| p.face_expand)),
        batch: batch.unwrap_or_else(|| pp.as_ref().map_or(4, |p| p.batch)),
        detect_every: detect_every.unwrap_or_else(|| pp.as_ref().map_or(1, |p| p.detect_every)),
        face_roi: face_roi_flag || pp.as_ref().is_some_and(|p| p.face_roi),
        tta: tta_flag.unwrap_or_else(|| pp.as_ref().is_some_and(|p| p.tta)),
        preset_label: label,
    })
}

#[derive(Debug, clap::Subcommand)]
enum ModelsCmd {
    /// 清点五档预所需模型（body/人脸回退链/批变体/archive ensemble 组件）的存在状态
    List,
    /// 按 manifest SHA256 校验模型完整性；--file 校验单个，缺省校验全部
    Verify {
        #[arg(long)]
        file: Option<String>,
    },
}

/// `models` 子命令：CLI 侧模型管理（对齐 GUI 设置屏的清点/SHA 校验）。
/// 查找规则与运行期一致：AUTOMOSAIC_MODELS_DIR → 用户数据目录 →
/// 可执行祖先链 models/（core::models::candidate_roots）。
fn models_cmd(cmd: ModelsCmd) -> Result<(), String> {
    match cmd {
        ModelsCmd::List => {
            if model_store::load_manifest().is_none() {
                println!("（未找到 manifest.json，按文件名直接探测）");
            }
            let mut ready = 0;
            for p in QualityPreset::ALL {
                let params = p.params()?;
                let mut missing = 0;
                println!("\n预设 {}（{}）", p.label(), p.id());

                let body = model_store::resolve_model(&params.body_model);
                model_row(&params.body_model, &body, &mut missing);

                // 人脸线（archive 档由 retina 组件承担，无 yolo-face）
                if !params.face_model.is_empty() {
                    match model_store::resolve_first(&[
                        &params.face_model,
                        "yolo11n-face-pose.onnx",
                        "yolov8n-face.onnx",
                    ]) {
                        Some(path) => {
                            let shown = path.file_name().unwrap_or_default().to_string_lossy();
                            if shown != params.face_model {
                                println!("  ✓ {shown:<44}（回退自 {}）", params.face_model);
                            } else {
                                println!("  ✓ {shown:<44} → {}", path.display());
                            }
                        }
                        None => model_row(&params.face_model, Path::new(""), &mut missing),
                    }
                }

                // 批推理变体（可选：缺失自动回退逐帧）
                if params.batch > 1 {
                    let stem = Path::new(&params.body_model)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();
                    let bfile = format!("{}-b{}.onnx", stem, params.batch);
                    let b = model_store::resolve_model(&bfile);
                    if b.is_file() {
                        println!("  ✓ {bfile:<44} → {}（批变体）", b.display());
                    } else {
                        println!("  – {bfile:<44}（批变体缺失，回退逐帧）");
                    }
                }

                // archive ensemble 组件（全部必需）
                if let Some(a) = &params.archive {
                    for (role, name) in [
                        ("开放词汇检测", a.gd),
                        ("SAM encoder", a.sam_encoder),
                        ("SAM decoder", a.sam_decoder),
                        ("滑窗人脸", a.retina),
                        ("外观关联", a.reid),
                    ] {
                        let path = model_store::resolve_model(name);
                        model_row(&format!("{name}（{role}）"), &path, &mut missing);
                    }
                    println!("  注：--sam-size tiny 另需 sam2.1-tiny-{{encoder,decoder}}.onnx（开发调试可选）");
                }

                if missing == 0 {
                    ready += 1;
                    println!("  ⇒ 齐备");
                } else {
                    println!("  ⇒ 缺 {missing} 个必需要件");
                }
            }
            println!("\n汇总：{}/{} 预设可用", ready, QualityPreset::ALL.len());
            Ok(())
        }
        ModelsCmd::Verify { file } => {
            let manifest = model_store::load_manifest()
                .ok_or("未找到 manifest.json（应位于模型目录内）")?;
            let entries = match &file {
                Some(f) => vec![manifest
                    .find(f)
                    .ok_or_else(|| format!("manifest 中无 {f}"))?
                    .clone()],
                None => manifest.models.clone(),
            };
            let (mut ok, mut fail, mut miss) = (0, 0, 0);
            for e in &entries {
                let path = model_store::resolve_model(&e.file);
                match model_store::verify_sha256(&path, &e.sha256) {
                    Some(true) => {
                        ok += 1;
                        println!("✓ {}", e.file);
                    }
                    Some(false) => {
                        fail += 1;
                        println!("✗ {} 哈希不符（{}）", e.file, path.display());
                    }
                    None => {
                        miss += 1;
                        println!("– {} 缺失（{}）", e.file, path.display());
                    }
                }
            }
            println!("\n校验：{ok} 通过 / {fail} 损坏 / {miss} 缺失（共 {} 条）", entries.len());
            if fail > 0 {
                Err("存在损坏模型".into())
            } else {
                Ok(())
            }
        }
    }
}

/// 清点行：存在打 ✓ 与解析路径，缺失打 ✗ 计数。
fn model_row(name: &str, path: &Path, missing: &mut usize) {
    if path.is_file() {
        println!("  ✓ {name:<44} → {}", path.display());
    } else {
        *missing += 1;
        println!("  ✗ {name:<44}（未找到）");
    }
}

#[derive(Debug, clap::Subcommand)]
enum DebugCmd {
    /// 单次调试运行：OUT_DIR 产出 out.mp4 + report.json + annotated/*.png
    Run {
        #[command(flatten)]
        common: DebugArgs,
        /// 每 N 帧导出一张标注帧（缺省不导出）
        #[arg(long)]
        annotate_every: Option<u32>,
        /// 在指定秒数处导出标注帧（逗号分隔，如 "1.5,3.0"）
        #[arg(long)]
        annotate_at: Option<String>,
    },
    /// 参数扫描：--sweep key=v1,v2（可重复），笛卡尔积运行并汇总对比表
    Sweep {
        #[command(flatten)]
        common: DebugArgs,
        #[arg(long = "sweep", required = true)]
        sweeps: Vec<String>,
    },
}

#[derive(Debug, clap::Args)]
struct DebugArgs {
    #[arg(short, long)]
    input: PathBuf,
    /// 调试产物目录（report.json / out.mp4 / annotated/）
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, default_value = "models/yolo11n-seg.onnx")]
    model: PathBuf,
    #[arg(long, default_value = "models/yolov8n-face.onnx")]
    face_model: PathBuf,
    #[arg(long, default_value_t = 0.35)]
    conf: f32,
    #[arg(long, default_value = "auto")]
    device: String,
    #[arg(long, default_value = "mosaic")]
    style: String,
    #[arg(long, default_value_t = 35)]
    strength: u32,
    #[arg(long, default_value = "auto")]
    hwaccel: String,
    #[arg(long, default_value = "auto")]
    encoder: String,
    #[arg(long, default_value = "auto")]
    bitrate: String,
    #[arg(long, default_value_t = 4)]
    batch: u32,
    #[arg(long, default_value_t = 1)]
    detect_every: u32,
    #[arg(long)]
    no_face: bool,
    #[arg(long)]
    no_track: bool,
    #[arg(long)]
    no_smooth: bool,
    #[arg(long)]
    no_landmark_expand: bool,
    #[arg(long)]
    no_mask_ema: bool,
    /// 翻转 TTA（额外一次水平翻转推理合并，+召回 ×2 代价）
    #[arg(long)]
    tta: bool,
    /// 相位相关全局运动补偿（sweep 键 gmc）
    #[arg(long)]
    gmc: bool,
    /// OC-SORT 观测中心重更新（sweep 键 ocru；默认开，--no-ocru 关）
    #[arg(long = "no-ocru")]
    no_ocru: bool,
}

impl DebugArgs {
    fn to_config(&self) -> debug::DebugConfig {
        debug::DebugConfig {
            input: self.input.clone(),
            out_dir: self.output.clone(),
            model: self.model.clone(),
            face_model: self.face_model.clone(),
            conf: self.conf,
            device: self.device.clone(),
            style: self.style.clone(),
            strength: self.strength,
            hwaccel: self.hwaccel.clone(),
            encoder: self.encoder.clone(),
            bitrate: self.bitrate.clone(),
            batch: self.batch,
            detect_every: self.detect_every,
            face: !self.no_face,
            landmark_expand: !self.no_landmark_expand,
            mask_ema: !self.no_mask_ema,
            face_roi: false, // debug 默认关（A/B 用 --sweep face-roi=1 开）
            track: !self.no_track,
            smooth: !self.no_smooth,
            tta: self.tta,
            gmc: self.gmc,
            ocru: !self.no_ocru,
            annotate_every: None,
            annotate_at: vec![],
        }
    }
}

/// hwaccel 选择 → 实际值。auto = 候选链逐个对真实流做 1s 冒烟解码
/// （DESIGN §3.3：设备/驱动级硬失败在启动期剔除，不再白付首次任务失败；
/// 结果按 流规格 缓存），全败则软解。
pub(crate) fn resolve_hwaccel(input: &Path, choice: &str) -> Option<String> {
    match choice {
        "none" => None,
        "auto" => {
            let Some(meta) = media::probe(input).ok() else { return None };
            media::decode_chain()
                .into_iter()
                .flatten()
                .find(|c| media::hwaccel_usable(input, c, &meta))
                .map(String::from)
        }
        name => Some(name.to_string()),
    }
}

/// 编码器选择 → 有序候选列表（auto = 候选链 ∩ 本机能力；libx264 兜底保证非空）。
/// 运行期失败时按序降级（`-encoders` 列表存在 ≠ 运行时可用，如容器内无 libcuda
/// 的 nvenc）。
pub(crate) fn resolve_encoders(choice: &str) -> Vec<String> {
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Probe { input } => probe_cmd(&input),
        Cmd::Hwaccel => {
            let bin = media::tool_path("ffmpeg");
            println!("ffmpeg: {}", bin.display());
            println!("GPU vendor: {}", media::gpu_vendor().unwrap_or("(未识别)"));
            let hw = media::list_hwaccels();
            println!("hwaccels: {}", hw.join(", "));
            for e in media::encoder_chain() {
                println!(
                    "encoder  {e:<20} {}",
                    if media::has_encoder(e) { "✓" } else { "✗" }
                );
            }
            Ok(())
        }
        Cmd::Transcode { input, output, hwaccel, encoder, bitrate } => {
            transcode_cmd(&input, &output, &hwaccel, &encoder, &bitrate)
        }
        Cmd::Models { cmd } => models_cmd(cmd),
        Cmd::Analyze {
            input, masks, preset, model, conf, device, face_model, no_face, face_expand,
            batch, detect_every, face_roi, tta, no_tta, gmc, sam_size, drain, hwaccel, encoder,
        } => analyze_cmd(
            &input, &masks, &preset, &model, conf, &device, &face_model, no_face,
            face_expand, batch, detect_every, face_roi,
            if tta { Some(true) } else if no_tta { Some(false) } else { None },
            gmc, &sam_size, &drain, &hwaccel, &encoder,
        ),
        Cmd::Render { input, output, masks, style, strength, hwaccel, encoder, bitrate } => {
            render_cmd(&input, &output, &masks, &style, strength, &hwaccel, &encoder, &bitrate)
        }
        Cmd::Debug { cmd } => debug_cmd(cmd),
        Cmd::Queue { inputs, out_dir, preset, device, conf, style, strength, hwaccel, encoder, bitrate, no_face } => {
            queue_cmd(&inputs, &out_dir, &preset, &device, conf, &style, strength, &hwaccel, &encoder, &bitrate, no_face)
        }
        Cmd::Process {
            input, output, preset, model, conf, device, style, strength, hwaccel, encoder, bitrate,
            face_model, face_expand, no_face, no_track, no_smooth, no_landmark_expand, no_mask_ema, batch, detect_every, face_roi, tta, no_tta, gmc, adaptive, no_ocru, pipe,
        } => process_cmd(
            &input,
            &output,
            &preset,
            &model,
            conf,
            &device,
            &style,
            strength,
            &hwaccel,
            &encoder,
            &bitrate,
            &face_model,
            face_expand,
            no_face,
            no_track,
            no_smooth,
            batch,
            detect_every,
            face_roi,
            no_landmark_expand,
            no_mask_ema,
            if tta { Some(true) } else if no_tta { Some(false) } else { None },
            gmc,
            adaptive,
            no_ocru,
            &pipe,
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 两阶段·分析：mask 组装状态机（MosaicPipeline）逐帧落盘（DESIGN §5.6 骨架）。
/// `--preset archive` 走 M5 极限·档案级管线（ensemble + SAM2.1 精修 +
/// RetinaFace 滑窗 + masklet 关联，[`automosaic_core::archive`]）。
/// 断点续跑：mask 目录已有缓存时跳到下一未分析帧（tracker 状态冷启动，见
/// MosaicPipeline::set_frame_idx 的语义说明）。
#[allow(clippy::too_many_arguments)]
fn analyze_cmd(
    input: &PathBuf,
    masks_dir: &PathBuf,
    preset: &Option<String>,
    model: &Option<PathBuf>,
    conf: Option<f32>,
    device: &str,
    face_model: &Option<PathBuf>,
    no_face: bool,
    face_expand: Option<u32>,
    batch: Option<u32>,
    detect_every: Option<u32>,
    face_roi: bool,
    tta: Option<bool>,
    gmc: bool,
    sam_size: &str,
    drain: &str,
    hwaccel: &str,
    encoder: &str,
) -> Result<(), String> {
    // M5 archive：独立管线（ensemble 模型组/逐帧精修/masklet 实例层），
    // 其余参数（model/face_model/batch 等）对流式档有效，此处不消费
    if preset.as_deref() == Some("archive") {
        return analyze_archive_cmd(input, masks_dir, device, conf, tta, sam_size, drain, hwaccel, encoder);
    }
    let eff = merge_params(
        preset, model, face_model, &conf, &face_expand, &batch, &detect_every, no_face, face_roi, tta,
    )?;
    let meta = media::probe(input).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let store = model_store_dir(masks_dir);

    // 断点续跑：已有同尺寸缓存则从下一帧继续，否则重头
    let start = match store.load_meta() {
        Ok(m) if m.width == meta.width && m.height == meta.height => {
            let s = store.analyzed_frames();
            println!("续跑：缓存已有 {s} 帧（meta frames={}）", m.frames);
            s
        }
        _ => {
            store
                .save_meta(&automosaic_core::maskstore::MaskMeta {
                    width: meta.width,
                    height: meta.height,
                    frames: 0,
                })
                .map_err(|e| e.to_string())?;
            0
        }
    };
    if let Some(total) = meta.total_frames
        && start >= total
    {
        println!("缓存已完整（{start} 帧），无需继续");
        return Ok(());
    }

    let batch_model = |p: &Path| -> PathBuf {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        p.with_file_name(format!("{stem}-b{}.onnx", eff.batch))
    };
    let batch_n = eff.batch.max(1) as usize;

    println!(
        "[分析] {} → {}  模型={} 起始帧={start}",
        input.display(),
        masks_dir.display(),
        eff.model.display()
    );
    let hw = resolve_hwaccel(input, hwaccel);
    let encoders = resolve_encoders(encoder);

    let transform = || -> Result<pipe::FrameTransform, String> {
        let mut det = Detector::load(&eff.model, device, eff.conf).map_err(|e| e.to_string())?;
        det.low_conf = Some(automosaic_core::track::BYTE_LOW_CONF);
        det.tta = eff.tta;
        if batch_n > 1 && batch_model(&eff.model).exists() {
            det.enable_batch(&batch_model(&eff.model), batch_n).map_err(|e| e.to_string())?;
        }
        let mut face = if let Some(fm) = &eff.face_model {
            let mut fd = FaceDetector::load(fm, device, (eff.conf - 0.1).max(0.1))
                .map_err(|e| e.to_string())?;
            if batch_n > 1 && batch_model(fm).exists() {
                fd.enable_batch(&batch_model(fm), batch_n).map_err(|e| e.to_string())?;
            }
            Some(fd)
        } else {
            None
        };
        let det: Arc<Mutex<dyn automosaic_core::detect::DetectorBackend>> =
            Arc::new(Mutex::new(det));
        let face: Option<Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>>> =
            face.take().map(|fd| {
                let f: Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>> =
                    Arc::new(Mutex::new(fd));
                f
            });
        let mut pipe = mosaic::MosaicPipeline::new(
            det,
            face,
            mosaic::MosaicOptions {
                conf: eff.conf,
                face: eff.face_model.is_some(),
                face_roi: eff.face_roi,
                landmark_expand: true,
                mask_ema: true,
                face_expand: eff.face_expand,
                track: true,
                smooth: true,
                gmc,
                ocru: true,
                detect_every: eff.detect_every,
                fps: meta.fps as f32,
                adaptive_skip_max: 0, // 两阶段分析离线跑，不做实时妥协
                style: MaskStyle::Solid, // 未用于组装（仅占位）
            },
            w,
            h,
        );
        pipe.set_frame_idx(start);
        let store = model_store_dir(masks_dir);
        let mut pos = 0u64; // 流内绝对位置（管线恒从帧 0 解码，无 seek）
        Ok(Box::new(move |frames: &mut [&mut [u8]]| {
            let base = pos;
            pos += frames.len() as u64;
            // 已分析帧直接跳过：续跑不重付推理（曾因索引错位把帧 0 的 mask
            // 写到 start 处——帧号与内容错位 + 白付全片推理）
            let skip = if base >= start { 0 } else { ((start - base) as usize).min(frames.len()) };
            if skip == frames.len() {
                return Ok(());
            }
            let refs: Vec<&[u8]> = frames[skip..].iter().map(|f| &**f).collect();
            let results = pipe.masks_of(&refs)?;
            drop(refs);
            for (i, (mask, _)) in results.iter().enumerate() {
                let next = base + (skip + i) as u64;
                store.save_mask(next, mask).map_err(|e| format!("mask 落盘失败: {e}"))?;
                store
                    .save_meta(&automosaic_core::maskstore::MaskMeta {
                        width: w as u32,
                        height: h as u32,
                        frames: next + 1,
                    })
                    .map_err(|e| format!("meta 更新失败: {e}"))?;
            }
            Ok(())
        }))
    };

    // pipe::run 需要编码侧；分析阶段直通编码到临时探针文件后删除
    let probe_out = masks_dir.join("_analyze_probe.mp4");
    let t0 = std::time::Instant::now();
    let stats = run_with_encoder_fallback(&encoders, |enc| {
        let transform = transform()
            .map_err(|e| pipe::PipelineError::TransformFailed { frames: 0, reason: e })?;
        pipe::run(
            input,
            &probe_out,
            pipe::PipelineOptions {
                hwaccel: hw.clone(),
                encoder: enc,
                bitrate: "auto".into(),
                transform: Some(transform),
                // 与流式 process 同批大小：批/单帧 session 的推理数值微差
                // 会经 tracker 状态放大为 mask 差异（两阶段 A/B 实测）
                batch_size: if batch_n > 1 && batch_model(&eff.model).exists() { batch_n } else { 1 },
                cancel: None,
                frame_format: media::FrameFormat::Nv12, // 两阶段 e2e 等价性基于 NV12
            },
            make_progress_printer(),
        )
    })?;
    let _ = std::fs::remove_file(&probe_out);
    let added = stats.frames.saturating_sub(start);
    if added == 0 {
        // 估算 total_frames 常比实际多 1（duration×fps 取整），跑到头即完整
        println!("缓存已完整：0 新增（总 {} 帧）→ {}", stats.frames, masks_dir.display());
    } else {
        println!(
            "分析完成：新增 {added}/{} 帧（总 {}），{:.1}s → {}",
            stats.frames,
            stats.frames,
            t0.elapsed().as_secs_f64(),
            masks_dir.display()
        );
    }
    Ok(())
}

/// M5 极限·档案级分析：ensemble（YOLO26x@1536 + Grounding DINO）→ WBF →
/// SAM2.1 精修 → RetinaFace 滑窗 → masklet 关联；逐帧落盘 .mask（合并层）
/// + .inst（实例层，复核 UI 的编辑单元）。0.1-0.5fps 预期，长任务断点续跑。
#[allow(clippy::too_many_arguments)]
fn analyze_archive_cmd(
    input: &PathBuf,
    masks_dir: &PathBuf,
    device: &str,
    conf: Option<f32>,
    tta: Option<bool>,
    sam_size: &str,
    drain: &str,
    hwaccel: &str,
    encoder: &str,
) -> Result<(), String> {
    use automosaic_core::archive::{ArchiveAnalyzer, ArchiveModelPaths, ArchiveOptions};
    use automosaic_core::maskstore::InstanceRecord;

    let pp = QualityPreset::Archive.params()?;
    let refs = pp.archive.expect("Archive 预设含模型组");
    let (sam_enc, sam_dec) = match sam_size {
        "tiny" => ("sam2.1-tiny-encoder.onnx", "sam2.1-tiny-decoder.onnx"),
        "large" => (refs.sam_encoder, refs.sam_decoder),
        s => return Err(format!("未知 SAM 规格 {s}（可选 large/tiny）")),
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
    let need = [
        ("YOLO26x@1536（主检）", &paths.yolo),
        ("Grounding DINO（开放词汇第二路）", &paths.gd),
        ("SAM2.1 encoder（mask 精修）", &paths.sam_encoder),
        ("SAM2.1 decoder", &paths.sam_decoder),
        ("RetinaFace（滑窗人脸）", &paths.retina),
    ];
    for (name, p) in &need {
        if !p.exists() {
            return Err(format!(
                "Archive 档缺少{name}: {}（运行 scripts/fetch_m5_models.sh 或在应用内下载）",
                p.display()
            ));
        }
    }
    if paths.reid.is_none() {
        eprintln!("[提示] 无 OSNet ReID 模型，masklet 关联退化为纯 IoU");
    }

    let meta = media::probe(input).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let store = model_store_dir(masks_dir);
    let start = match store.load_meta() {
        Ok(m) if m.width == meta.width && m.height == meta.height => {
            let s = store.analyzed_frames();
            println!("续跑：缓存已有 {s} 帧（masklet ID 从续跑点重新编起——复核编辑按帧补丁存储，不受影响）");
            s
        }
        _ => {
            store
                .save_meta(&automosaic_core::maskstore::MaskMeta {
                    width: meta.width,
                    height: meta.height,
                    frames: 0,
                })
                .map_err(|e| e.to_string())?;
            0
        }
    };
    if let Some(total) = meta.total_frames
        && start >= total
    {
        println!("缓存已完整（{start} 帧），无需继续");
        return Ok(());
    }

    println!(
        "[Archive 分析] {} → {}  ensemble=yolo26x@1536+GD-tiny  SAM={}  起始帧={start}",
        input.display(),
        masks_dir.display(),
        paths.sam_encoder.display()
    );
    let hw = resolve_hwaccel(input, hwaccel);
    let encoders = resolve_drain(drain, encoder);
    let opts = ArchiveOptions { conf: conf.unwrap_or(pp.conf), tta: tta.unwrap_or(true), ..Default::default() };

    let transform = || -> Result<pipe::FrameTransform, String> {
        // 懒加载：首个变换调用才构建 analyzer——续跑"缓存已完整"路径不白付
        // 五件套模型加载（GD fp16 编译 + x@1536 加载数十秒）
        let mut az: Option<ArchiveAnalyzer> = None;
        let paths = paths.clone();
        let opts = opts.clone();
        let device = device.to_string();
        let store = model_store_dir(masks_dir);
        let mut pos = 0u64; // 流内绝对位置（管线恒从帧 0 解码，无 seek）
        Ok(Box::new(move |frames: &mut [&mut [u8]]| {
            let base = pos;
            pos += frames.len() as u64;
            // 已分析帧直接跳过：续跑不重付推理（帧号按流内位置对齐）
            let skip = if base >= start { 0 } else { ((start - base) as usize).min(frames.len()) };
            if skip == frames.len() {
                return Ok(());
            }
            if az.is_none() {
                eprintln!();
                az = Some(ArchiveAnalyzer::new_with_progress(&paths, opts.clone(), &device, w, h, |stage| {
                    eprintln!("  加载模型：{stage} …");
                })?);
            }
            let az = az.as_mut().expect("刚构建");
            for (i, frame) in frames.iter_mut().enumerate().skip(skip) {
                let next = base + i as u64;
                let instances = az.analyze_frame(frame)?;
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
                // 合并层 = 全实例并集（render 消费；复核补丁在此之上叠加）
                let mut merged = vec![0u8; w * h];
                for inst in &instances {
                    for (o, &m) in merged.iter_mut().zip(&inst.mask) {
                        *o |= m;
                    }
                }
                store.save_mask(next, &merged).map_err(|e| format!("mask 落盘失败: {e}"))?;
                store.save_instances(next, &records).map_err(|e| format!("实例落盘失败: {e}"))?;
                store
                    .save_meta(&automosaic_core::maskstore::MaskMeta {
                        width: w as u32,
                        height: h as u32,
                        frames: next + 1,
                    })
                    .map_err(|e| format!("meta 更新失败: {e}"))?;
            }
            Ok(())
        }))
    };

    // pipe::run 需要编码侧；分析阶段直通编码到临时探针文件后删除（同 analyze_cmd）
    let probe_out = masks_dir.join("_analyze_probe.mp4");
    let t0 = std::time::Instant::now();
    let stats = run_with_encoder_fallback(&encoders, |enc| {
        let transform = transform()
            .map_err(|e| pipe::PipelineError::TransformFailed { frames: 0, reason: e })?;
        pipe::run(
            input,
            &probe_out,
            pipe::PipelineOptions {
                hwaccel: hw.clone(),
                encoder: enc,
                bitrate: "auto".into(),
                transform: Some(transform),
                batch_size: 1, // 逐帧精修，无批
                cancel: None,
                frame_format: media::FrameFormat::Nv12,
            },
            make_progress_printer(),
        )
    })?;
    let _ = std::fs::remove_file(&probe_out);
    let added = stats.frames.saturating_sub(start);
    if added == 0 {
        println!("缓存已完整：0 新增（总 {} 帧）→ {}", stats.frames, masks_dir.display());
    } else {
        println!(
            "Archive 分析完成：新增 {added}/{} 帧（总 {}），{:.1}s（{:.2}fps）→ {}",
            stats.frames,
            stats.frames,
            t0.elapsed().as_secs_f64(),
            added as f64 / t0.elapsed().as_secs_f64().max(1e-9),
            masks_dir.display()
        );
    }
    Ok(())
}

/// 两阶段·渲染：读 mask 缓存纯合成+编码（无推理；样式/强度可任意改）。
/// 存在复核补丁（patches.bin，M5 复核 UI 产物）时在缓存 mask 之上应用。
#[allow(clippy::too_many_arguments)]
fn render_cmd(
    input: &PathBuf,
    output: &PathBuf,
    masks_dir: &PathBuf,
    style: &str,
    strength: u32,
    hwaccel: &str,
    encoder: &str,
    bitrate: &str,
) -> Result<(), String> {
    let meta = media::probe(input).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);
    let store = model_store_dir(masks_dir);
    let mmeta = store.verify(meta.width, meta.height).map_err(|e| e.to_string())?;
    let mask_style = match style {
        "mosaic" => MaskStyle::Mosaic { cell: strength.clamp(2, 128) as usize },
        "blur" => MaskStyle::Blur { radius: strength.clamp(1, 64) as usize },
        "solid" => MaskStyle::Solid,
        s => return Err(format!("未知样式 {s}（可选 mosaic/blur/solid）")),
    };
    println!(
        "[渲染] 缓存 {}/{} 帧 · 样式={mask_style:?}  {} → {}",
        mmeta.frames,
        meta.total_frames.map(|t| t.to_string()).unwrap_or_else(|| "?".into()),
        input.display(),
        output.display()
    );
    let hw = resolve_hwaccel(input, hwaccel);
    let encoders = resolve_encoders(encoder);
    // 复核补丁（M5）：存在 patches.bin 时在缓存 mask 之上应用（add |= / erase &= !）
    let patches = automosaic_core::maskstore::PatchStore::load(masks_dir);
    if !patches.patches.is_empty() {
        println!("复核补丁：{} 条（{} 帧）", patches.patches.len(), {
            let mut fs: Vec<u64> = patches.patches.iter().map(|p| p.frame).collect();
            fs.sort_unstable();
            fs.dedup();
            fs.len()
        });
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    let missing = std::sync::Arc::new(AtomicU64::new(0));
    let t0 = std::time::Instant::now();
    let stats = run_with_encoder_fallback(&encoders, |enc| {
        let store = model_store_dir(masks_dir);
        let mut idx = 0u64;
        let style = mask_style.clone();
        let patches = patches.clone();
        let missing = Arc::clone(&missing);
        let transform: pipe::FrameTransform = Box::new(move |frames: &mut [&mut [u8]]| {
            for frame in frames.iter_mut() {
                match store.load_mask(idx, w, h) {
                    Ok(Some(mut mask)) => {
                        patches.apply(idx, &mut mask);
                        automosaic_core::compose::apply(frame, w, h, &mask, &style)
                    }
                    // 未分析帧：原样（渲染段不猜测）
                    Ok(None) => {
                        missing.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => return Err(e.to_string()),
                }
                idx += 1;
            }
            Ok(())
        });
        pipe::run(
            input,
            output,
            pipe::PipelineOptions {
                hwaccel: hw.clone(),
                encoder: enc,
                bitrate: bitrate.to_string(),
                transform: Some(transform),
                batch_size: 1,
                cancel: None,
                frame_format: media::FrameFormat::Nv12,
            },
            make_progress_printer(),
        )
    })?;
    eprintln!();
    let missing_warned = missing.load(Ordering::Relaxed);
    if missing_warned > 0 {
        eprintln!("警告：{missing_warned} 帧无缓存（未分析），已原样输出");
    }
    println!(
        "渲染完成：{} 帧，{:.1}s（无推理）→ {}",
        stats.frames,
        t0.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

/// mask 缓存目录的 &MaskStore 便捷构造（内部 new）。
fn model_store_dir(dir: &PathBuf) -> automosaic_core::maskstore::MaskStore {
    automosaic_core::maskstore::MaskStore::new(dir).expect("mask 目录创建失败")
}

fn debug_cmd(cmd: DebugCmd) -> Result<(), String> {
    match cmd {
        DebugCmd::Run { common, annotate_every, annotate_at } => {
            let mut cfg = common.to_config();
            cfg.annotate_every = annotate_every;
            if let Some(at) = annotate_at {
                cfg.annotate_at = at
                    .split(',')
                    .filter_map(|t| t.trim().parse::<f64>().ok())
                    .collect();
            }
            let r = debug::run(&cfg)?;
            println!(
                "完成: {} 帧，{:.1}fps（推理合计 {:.0}ms）| 平均 persons={:.2} faces={:.2} 遮盖={:.1}% 漏检保持帧={:.1}%",
                r.frames, r.fps, r.infer_ms_total, r.mean_persons, r.mean_faces, r.mask_cov_pct, r.held_pct
            );
            println!("报告: {}", cfg.out_dir.join("report.json").display());
            Ok(())
        }
        DebugCmd::Sweep { common, sweeps } => {
            let cfg = common.to_config();
            let parsed: Vec<(String, Vec<String>)> = sweeps
                .iter()
                .map(|s| {
                    let (k, v) = s
                        .split_once('=')
                        .ok_or_else(|| format!("--sweep 格式应为 key=v1,v2，得到 {s}"))?;
                    Ok((
                        k.trim().to_string(),
                        v.split(',').map(|x| x.trim().to_string()).collect(),
                    ))
                })
                .collect::<Result<_, String>>()?;
            debug::sweep(&cfg, &parsed)
        }
    }
}

fn probe_cmd(input: &PathBuf) -> Result<(), String> {
    let m = media::probe(input).map_err(|e| e.to_string())?;
    println!(
        "{}: {}×{} @ {:.3}fps, codec={}, pix_fmt={}, frames={}, duration={:?}s, audio={}{},",
        input.display(),
        m.width,
        m.height,
        m.fps,
        m.codec,
        m.pix_fmt.as_deref().unwrap_or("?"),
        m.total_frames.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
        m.duration_secs,
        m.has_audio,
        if m.rotation != 0.0 { format!(", rotation={}°", m.rotation) } else { String::new() }
    );
    Ok(())
}

fn transcode_cmd(
    input: &PathBuf,
    output: &PathBuf,
    hwaccel: &str,
    encoder: &str,
    bitrate: &str,
) -> Result<(), String> {
    let hw = resolve_hwaccel(input, hwaccel);
    let encoders = resolve_encoders(encoder);
    println!(
        "hwaccel={}  encoder 候选=[{}]  {} → {}",
        hw.as_deref().unwrap_or("(软解)"),
        encoders.join(", "),
        input.display(),
        output.display()
    );
    let t0 = std::time::Instant::now();
    let stats = run_with_encoder_fallback(&encoders, |enc| {
        pipe::passthrough(input, output, hw.clone(), enc, bitrate.to_string(), make_progress_printer())
    })?;
    eprintln!();
    println!(
        "完成：{} 帧，{:.1}s（管线吞吐 {:.1}fps）→ {}",
        stats.frames,
        t0.elapsed().as_secs_f64(),
        stats.frames as f64 / t0.elapsed().as_secs_f64().max(1e-9),
        output.display()
    );
    Ok(())
}

/// 分析段输出处理选择：null（默认，-f null 帧丢弃）或 file（真编码探针，调试用）。
pub(crate) fn resolve_drain(drain: &str, encoder: &str) -> Vec<String> {
    match drain {
        "null" => vec!["null".into()],
        "file" => resolve_encoders(encoder),
        d => {
            eprintln!("未知 --drain {d}（可选 null/file），按 null 处理");
            vec!["null".into()]
        }
    }
}

/// 依序尝试编码器：EncoderFailed（运行期不可用）时降级到下一个候选。
pub(crate) fn run_with_encoder_fallback<T>(
    encoders: &[String],
    mut run: impl FnMut(String) -> Result<T, pipe::PipelineError>,
) -> Result<T, String> {
    let mut last: Option<String> = None;
    for (i, enc) in encoders.iter().enumerate() {
        match run(enc.clone()) {
            Ok(v) => return Ok(v),
            Err(pipe::PipelineError::EncoderFailed { .. }) if i + 1 < encoders.len() => {
                eprintln!("\n[回退] 编码器 {enc} 运行不可用，尝试 {} …", encoders[i + 1]);
            }
            Err(e) => return Err(e.to_string()),
        }
        last = Some(format!("编码器 {enc} 运行不可用"));
    }
    Err(last.unwrap_or_else(|| "无可用编码器".into()))
}

#[allow(clippy::too_many_arguments)]
fn process_cmd(
    input: &PathBuf,
    output: &PathBuf,
    preset: &Option<String>,
    model: &Option<PathBuf>,
    conf: Option<f32>,
    device: &str,
    style: &str,
    strength: u32,
    hwaccel: &str,
    encoder: &str,
    bitrate: &str,
    face_model: &Option<PathBuf>,
    face_expand: Option<u32>,
    no_face: bool,
    no_track: bool,
    no_smooth: bool,
    batch: Option<u32>,
    detect_every: Option<u32>,
    face_roi: bool,
    landmark_expand: bool,
    mask_ema: bool,
    tta: Option<bool>,
    gmc: bool,
    adaptive: bool,
    no_ocru: bool,
    pipe: &str,
) -> Result<(), String> {
    // Archive 档是两阶段语义（分析→复核→渲染），流式 process 不承载
    if preset.as_deref() == Some("archive") {
        return Err(
            "archive 档走两阶段：automosaic-cli analyze --preset archive → （可选复核）→ render \
             （DESIGN §5.6：数小时分析可断点续跑，复核编辑后纯合成渲染）"
                .into(),
        );
    }
    let eff = merge_params(
        preset, model, face_model, &conf, &face_expand, &batch, &detect_every, no_face, face_roi, tta,
    )?;
    let meta = media::probe(input).map_err(|e| e.to_string())?;
    let (w, h) = (meta.width as usize, meta.height as usize);

    // 固定批模型：与单帧模型同目录、同词干加 -b{N} 后缀
    let batch_model = |p: &Path| -> PathBuf {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        p.with_file_name(format!("{stem}-b{}.onnx", eff.batch))
    };
    let batch_n = eff.batch.max(1) as usize;

    let mask_style = match style {
        "mosaic" => MaskStyle::Mosaic { cell: strength.clamp(2, 128) as usize },
        "blur" => MaskStyle::Blur { radius: strength.clamp(1, 64) as usize },
        "solid" => MaskStyle::Solid,
        s => return Err(format!("未知样式 {s}（可选 mosaic/blur/solid）")),
    };
    let pipe_format = match pipe {
        "nv12" => media::FrameFormat::Nv12,
        "mjpeg" => media::FrameFormat::Mjpeg,
        other => return Err(format!("未知管道格式 {other}（可选 nv12/mjpeg）")),
    };
    let hw = resolve_hwaccel(input, hwaccel);
    let encoders = resolve_encoders(encoder);
    let body_b4 = batch_n > 1 && batch_model(&eff.model).exists();
    let face_b4 =
        batch_n > 1 && eff.face_model.as_ref().map_or(false, |f| batch_model(f).exists());
    println!(
        "[{}] 模型={} device={} conf={} 样式={:?}({strength})  人脸={} 跟踪={} 平滑={} TTA={} 批={}(b4模型:{}/{}) 隔帧={}  {}×{} {} 帧",
        eff.preset_label,
        eff.model.display(),
        if device == "cpu" { "cpu" } else { "auto" },
        eff.conf,
        mask_style,
        eff.face_model.is_some(),
        !no_track,
        !no_smooth,
        eff.tta,
        batch_n,
        body_b4,
        face_b4,
        eff.detect_every,
        w,
        h,
        meta.total_frames.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
    );
    println!(
        "hwaccel={}  encoder 候选=[{}]  {} → {}",
        hw.as_deref().unwrap_or("(软解)"),
        encoders.join(", "),
        input.display(),
        output.display()
    );

    // transform 构建器：编码器回退重试时重建（模型重载，CoreML 编译有缓存）
    let make_transform = || -> Result<pipe::FrameTransform, String> {
        let mut det = Detector::load(&eff.model, device, eff.conf).map_err(|e| e.to_string())?;
        det.low_conf = Some(automosaic_core::track::BYTE_LOW_CONF); // BYTE 二段救援
        det.tta = eff.tta; // 翻转 TTA（极致档默认开，--no-tta 覆写）
        if body_b4 {
            det.enable_batch(&batch_model(&eff.model), batch_n).map_err(|e| e.to_string())?;
        }
        let mut face = if let Some(fm) = &eff.face_model {
            let mut fd = FaceDetector::load(fm, device, (eff.conf - 0.1).max(0.1))
                .map_err(|e| e.to_string())?;
            if face_b4 {
                fd.enable_batch(&batch_model(fm), batch_n).map_err(|e| e.to_string())?;
            }
            Some(fd)
        } else {
            None
        };
        let det: Arc<Mutex<dyn automosaic_core::detect::DetectorBackend>> =
            Arc::new(Mutex::new(det));
        let face: Option<Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>>> =
            face.take().map(|fd| {
                let f: Arc<Mutex<dyn automosaic_core::detect::FaceDetectorBackend>> =
                    Arc::new(Mutex::new(fd));
                f
            });
        Ok(mosaic::build(
            det,
            face,
            mosaic::MosaicOptions {
                conf: eff.conf,
                face: eff.face_model.is_some(),
                face_roi: eff.face_roi,
                landmark_expand,
                mask_ema,
                face_expand: eff.face_expand,
                track: !no_track,
                smooth: !no_smooth,
                    gmc,
                    ocru: !no_ocru,
                    detect_every: eff.detect_every,
                    fps: meta.fps as f32,
                    adaptive_skip_max: if adaptive { 3 } else { 0 },
                    style: mask_style.clone(),
            },
            w,
            h,
            None,
        ))
    };

    let t0 = std::time::Instant::now();
    let mut load_secs = 0.0f64;
    let stats = run_with_encoder_fallback(&encoders, |enc| {
        // 注：CoreML EP 每个 session 首次加载约 2-3s（无磁盘缓存），记录为模型加载时间
        let tl = std::time::Instant::now();
        let transform = make_transform()
            .map_err(|e| pipe::PipelineError::TransformFailed { frames: 0, reason: e })?;
        load_secs = tl.elapsed().as_secs_f64();
        eprintln!("模型加载 {load_secs:.1}s");
        pipe::run(
            input,
            output,
            pipe::PipelineOptions {
                hwaccel: hw.clone(),
                encoder: enc,
                bitrate: bitrate.to_string(),
                transform: Some(transform),
                batch_size: if body_b4 { batch_n } else { 1 },
                cancel: None,
                frame_format: pipe_format,
            },
            make_progress_printer(),
        )
    })?;
    eprintln!();
    let total = t0.elapsed().as_secs_f64();
    let proc_fps = stats.frames as f64 / (total - load_secs).max(1e-9);
    println!(
        "完成：{} 帧，总 {total:.1}s（模型加载 {load_secs:.1}s，处理 {:.1}fps）→ {}",
        stats.frames,
        proc_fps,
        output.display()
    );
    Ok(())
}

/// 批处理队列：core JobManager 驱动串行执行；单作业失败记录后继续。
#[allow(clippy::too_many_arguments)]
fn queue_cmd(
    inputs: &[PathBuf],
    out_dir: &PathBuf,
    preset: &Option<String>,
    device: &str,
    conf: Option<f32>,
    style: &str,
    strength: u32,
    hwaccel: &str,
    encoder: &str,
    bitrate: &str,
    no_face: bool,
) -> Result<(), String> {
    use automosaic_core::job::{JobManager, JobState};

    std::fs::create_dir_all(out_dir).map_err(|e| format!("输出目录创建失败: {e}"))?;
    let total = inputs.len();
    let mut jm = JobManager::new();
    for _ in inputs {
        jm.enqueue();
    }
    let mut failed = 0usize;
    while let Some((id, _cancel)) = jm.start_next() {
        let input = &inputs[id as usize];
        let output = out_dir.join(format!(
            "{}_mosaic.mp4",
            input.file_stem().and_then(|s| s.to_str()).unwrap_or("out")
        ));
        println!(
            "\n[队列 {}/{}] {} → {}",
            id as usize + 1,
            total,
            input.display(),
            output.display()
        );
        let t0 = std::time::Instant::now();
        // 复用单文件 process（预设解析/探测/编码回退全套语义一致）
        let r = process_cmd(
            input,
            &output,
            preset,
            &None,          // model：取预设
            conf,
            device,
            style,
            strength,
            hwaccel,
            encoder,
            bitrate,
            &None,          // face_model：取预设
            None,           // face_expand：取预设
            no_face,
            false,          // no_track
            false,          // no_smooth
            None,           // batch：取预设
            None,           // detect_every：取预设
            false,          // face_roi：取预设
            true,           // landmark_expand
            true,           // mask_ema
            None,           // tta：取预设
            false,          // gmc
            false,          // adaptive
            false,          // no_ocru
            "nv12",
        );
        let state = match r {
            Ok(()) => JobState::Done { frames: 0 },
            Err(e) => {
                failed += 1;
                eprintln!("[队列] 作业 {} 失败: {e}", id + 1);
                JobState::Failed { error: e }
            }
        };
        jm.finish(id, state);
        println!("[队列] 用时 {:.1}s", t0.elapsed().as_secs_f64());
    }
    let done = total - failed;
    println!(
        "\n队列完成：{done}/{total} 成功{}",
        if failed > 0 { format!("，{failed} 失败") } else { String::new() }
    );
    if failed > 0 {
        return Err(format!("{failed} 个作业失败"));
    }
    Ok(())
}

fn make_progress_printer() -> impl FnMut(pipe::Progress) {
    |p: pipe::Progress| {
        let total = p.total_frames.map(|t| t.to_string()).unwrap_or_else(|| "?".into());
        let eta = p
            .eta_secs
            .map(|e| format!("{:.0}s", e))
            .unwrap_or_else(|| "?".into());
        eprint!("\r帧 {}/{}  {:6.1}fps  eta {:>6}", p.frames, total, p.fps, eta);
    }
}
