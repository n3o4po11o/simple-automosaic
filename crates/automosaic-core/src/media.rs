//! 媒体探测与 ffmpeg 命令构建（DESIGN §3）。
//!
//! - `probe`: ffprobe JSON → [`VideoMeta`]（后续 Probed 事件与管线参数的数据源）
//! - `list_hwaccels` / `has_encoder`: 编译期能力枚举（完整"真实流冒烟探测"见 M2）
//! - `decode_chain` / `encoder_chain`: 平台候选链（DESIGN §3.3 回退链的静态部分）
//! - `decode_cmd` / `encode_cmd`: NV12 rawvideo 双向管道命令（DESIGN §3.2）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// 子进程构造统一入口：Windows GUI 应用下 spawn 默认弹控制台窗口
/// （ffmpeg 探测/冒烟/管线/reg 查询，真机实测一片 cmd 闪现）——统一
/// CREATE_NO_WINDOW 隐藏。
#[allow(dead_code)]
pub(crate) fn spawn_command(program: &std::path::Path) -> Command {
    let mut c = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

// --------------------------------------------------------------------------- //
// ffmpeg/ffprobe 二进制解析（内置优先，DESIGN §8）
// --------------------------------------------------------------------------- //

/// 内置二进制目录名（与 scripts/fetch_ffmpeg.sh 的布局一致）。
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUNDLED_PLATFORM: &str = "darwin-arm64";
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const BUNDLED_PLATFORM: &str = "darwin-x86_64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BUNDLED_PLATFORM: &str = "linux-x86_64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BUNDLED_PLATFORM: &str = "linux-aarch64";
#[cfg(target_os = "windows")]
const BUNDLED_PLATFORM: &str = "windows-x86_64";
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const BUNDLED_PLATFORM: &str = "other";

/// 搜索根目录：可执行文件的各级祖先（最多 10 级——macOS .app 到仓库根约 9 级）
/// + cwd 及其上级（最多 3 级）。去重保序。
pub fn search_roots() -> Vec<PathBuf> {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    roots_from(exe.as_deref(), cwd.as_deref())
}

fn roots_from(exe: Option<&Path>, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |p: Option<&Path>| {
        if let Some(p) = p && !roots.iter().any(|r| r == p) {
            roots.push(p.to_path_buf());
        }
    };
    // exe 祖先（.app/Contents/MacOS/exe → 上溯约 9 级到仓库根）
    let mut dir = exe.and_then(|e| e.parent());
    for _ in 0..10 {
        match dir {
            Some(d) => {
                push(Some(d));
                dir = d.parent();
            }
            None => break,
        }
    }
    // cwd 祖先（repo 根 / app/ / crates/* 运行场景；Finder 启动 cwd=/ 时无效）
    let mut dir = cwd;
    for _ in 0..3 {
        match dir {
            Some(d) => {
                push(Some(d));
                dir = d.parent();
            }
            None => break,
        }
    }
    roots
}

/// 解析 ffmpeg/ffprobe 可执行文件路径（`tool` 为不带扩展名的名字）。
/// 优先级：环境变量 AUTOMOSAIC_FFMPEG_DIR → 各搜索根下的
/// `<根>/<name>` 与 `<根>/bin/<platform>/<name>` → 系统 PATH。
pub fn tool_path(tool: &str) -> PathBuf {
    let name = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };

    let mut cands: Vec<PathBuf> = vec![];
    if let Ok(dir) = std::env::var("AUTOMOSAIC_FFMPEG_DIR") {
        cands.push(Path::new(&dir).join(&name));
    }
    let bundled = Path::new("bin").join(BUNDLED_PLATFORM).join(&name);
    for root in search_roots() {
        cands.push(root.join(&name));
        cands.push(root.join(&bundled));
        // 打包布局：.app/Contents/Resources/（Contents 是 exe 的祖先）
        cands.push(root.join("Resources").join(&name));
    }
    cands.push(PathBuf::from(&name)); // PATH 兜底
    cands.into_iter().find(|c| c.is_file()).unwrap_or_else(|| PathBuf::from(name))
}

// --------------------------------------------------------------------------- //
// ffprobe 探测
// --------------------------------------------------------------------------- //

/// 视频元数据。
#[derive(Debug, Clone)]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub pix_fmt: Option<String>,
    /// 容器精确值不可得时按 duration × fps 估算。
    pub total_frames: Option<u64>,
    pub duration_secs: Option<f64>,
    pub has_audio: bool,
    /// 全部音轨的编码名（"Audio: " 行逐条解析；TrueHD 等与 mp4 不兼容的编码
    /// 会触发 AAC 转码——多音轨时任一不兼容即转，旧版只看第一条会漏判）。
    pub audio_codecs: Vec<String>,
    /// 显示旋转（度，来自容器 display matrix；±90 时解码管道输出已自动旋转，
    /// width/height 已交换为显示尺寸）。
    pub rotation: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("执行 {bin} 失败: {source}\n（未找到内置 ffmpeg？在仓库根运行 scripts/fetch_ffmpeg.sh 生成 bin/；\n 已尝试的搜索根见 media::search_roots）")]
    Spawn { bin: String, #[source] source: std::io::Error },
    #[error("无法解析视频（退出码 {code}）: {stderr}")]
    Inspect { code: i32, stderr: String },
    #[error("无视频流: {0}")]
    NoVideoStream(PathBuf),
}

/// 解析 ffmpeg -i 的 stderr 提取元数据。
/// （ffprobe 已从依赖中移除：探测复用 ffmpeg 本体，少内置一个 ~52MB 二进制；
///   app 侧的展示性元数据由 media_kit/libmpv 播放流提供。）
///
/// 典型输出：
/// ```text
/// Input #0, mov,mp4,..., from 'in.mp4':
///   Duration: 00:00:05.07, start: 0.000000, bitrate: 1089 kb/s
///   Stream #0:0[0x1](und): Video: hevc (Main 10) (hvc1 / 0x31763763), yuv420p10le(pc),
///       1920x1080, 1088 kb/s, 15 fps, 15 tbr, 16k tbn (default)
///   Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6D703461), 44100 Hz, stereo, fltp
/// At least one output file must be specified   ← 退出码 1 属预期
/// ```
pub fn probe(path: &Path) -> Result<VideoMeta, ProbeError> {
    let bin = tool_path("ffmpeg");
    let out = spawn_command(&bin)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(path)
        .output()
        .map_err(|source| ProbeError::Spawn {
            bin: bin.to_string_lossy().into_owned(),
            source,
        })?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    meta_from_ffmpeg_stderr(&stderr, path, out.status.code())
}

fn meta_from_ffmpeg_stderr(
    stderr: &str,
    path: &Path,
    code: Option<i32>,
) -> Result<VideoMeta, ProbeError> {
    let vline = stderr
        .lines()
        .find(|l| l.contains(": Video:"))
        .ok_or_else(|| ProbeError::Inspect {
            code: code.unwrap_or(-1),
            stderr: tail_of(stderr, 600),
        })?;
    let (width, height) = dims_in(vline).ok_or_else(|| ProbeError::NoVideoStream(path.to_path_buf()))?;

    let duration_secs = stderr.lines().find_map(|l| {
        let p = l.find("Duration:")?;
        let seg = l[p + 9..].split(',').next()?.trim();
        let mut it = seg.split(':');
        let h: f64 = it.next()?.parse().ok()?;
        let m: f64 = it.next()?.parse().ok()?;
        let s: f64 = it.next()?.parse().ok()?;
        Some(h * 3600.0 + m * 60.0 + s)
    });

    let fps = number_before(vline, "fps")
        .or_else(|| number_before(vline, "tbr"))
        .filter(|f| *f > 0.0 && *f < 1000.0)
        .unwrap_or(30.0);

    let codec = vline
        .split(": Video:")
        .nth(1)
        .unwrap_or("")
        .trim()
        .split(|c| c == ' ' || c == '(')
        .next()
        .unwrap_or("")
        .to_string();
    let pix_fmt = vline
        .split([',', ' '])
        .find(|t| t.starts_with("yuv") || t.starts_with("nv12") || t.starts_with("rgb"))
        .map(|t| t.split('(').next().unwrap_or(t).to_string());

    // Display Matrix: rotation of -90.00 degrees（容器旋转元数据）。
    // ffmpeg 解码到 rawvideo 时会自动应用旋转——±90 下管道帧是"显示方向"，
    // 交换 width/height 使 transform/编码/字节数一致（nan 等异常视为 0）。
    let rotation = stderr
        .lines()
        .find(|l| l.contains("Display Matrix: rotation of"))
        .and_then(|l| {
            let p = l.find("rotation of")? + "rotation of".len();
            l[p..].split("degrees").next()?.trim().parse::<f32>().ok()
        })
        .unwrap_or(0.0);
    let rotated90 = {
        let r = rotation.round().rem_euclid(180.0);
        (r - 90.0).abs() < 45.0
    };
    let (width, height) = if rotated90 { (height, width) } else { (width, height) };

    let total_frames = duration_secs.map(|d| (d * fps).round() as u64);
    Ok(VideoMeta {
        width,
        height,
        fps,
        codec,
        pix_fmt,
        total_frames,
        duration_secs,
        has_audio: stderr.lines().any(|l| l.contains(": Audio:")),
        audio_codecs: stderr
            .lines()
            .filter(|l| l.contains(": Audio:"))
            .filter_map(|l| {
                let p = l.find(": Audio:")? + ": Audio:".len();
                l[p..]
                    .split(|c| c == ' ' || c == '(' || c == ',')
                    .find(|t| !t.is_empty())
                    .map(|t| t.to_string())
            })
            .collect(),
        rotation,
    })
}

/// 行内 "1920x1080" 形式的尺寸（排除十六进制 tag 如 "0x31763763"——
/// 要求两侧均为 ≥2 位十进制且在合理范围）。
fn dims_in(line: &str) -> Option<(u32, u32)> {
    for (i, _) in line.match_indices('x') {
        let before: String = line[..i]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let after: String = line[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let (Ok(w), Ok(h)) = (before.parse::<u32>(), after.parse::<u32>()) {
            if (2..=8192).contains(&w) && (2..=8192).contains(&h) {
                return Some((w, h));
            }
        }
    }
    None
}

/// 单位（"fps"/"tbr"）前的数字：", 15 fps" → 15。
fn number_before(s: &str, unit: &str) -> Option<f64> {
    let i = s.find(unit)?;
    let digits: String = s[..i]
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let digits = digits.trim_matches('.');
    if digits.is_empty() || digits == "." {
        return None;
    }
    digits.parse().ok()
}

fn tail_of(s: &str, n: usize) -> String {
    s.chars().rev().take(n).collect::<String>().chars().rev().collect()
}

// --------------------------------------------------------------------------- //
// hwaccel / 编码器枚举（编译期能力；驱动级冒烟探测在后续里程碑）
// --------------------------------------------------------------------------- //

/// `ffmpeg -hwaccels` 输出解析（首行是表头 "Hardware acceleration methods:"）。
pub fn list_hwaccels() -> Vec<String> {
    let Ok(out) = spawn_command(&tool_path("ffmpeg"))
        .args(["-hide_banner", "-hwaccels"])
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// `ffmpeg -encoders` 中是否存在指定编码器。
pub fn has_encoder(name: &str) -> bool {
    let Ok(out) = spawn_command(&tool_path("ffmpeg"))
        .args(["-hide_banner", "-encoders"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name))
}

// --------------------------------------------------------------------------- //
// 解码侧冒烟探测（DESIGN §3.3 步骤 3：真实流 1s 试解码）
// --------------------------------------------------------------------------- //

/// 冒烟运行的墙钟上限（硬失败立即退出；正常 1s 流解码远小于此）。
const SMOKE_TIMEOUT: Duration = Duration::from_secs(5);

/// 候选 hwaccel 对真实流的冒烟解码：解码前 1 秒到 `-f null`。
/// 设备/驱动/构建级硬失败（如 mac 上的 cuda、无 libcuda 容器内的 nvdec）
/// 立即非零退出——据此在启动期选定候选，避免首次任务白付一次运行期失败；
/// 流级不兼容由 `-hwaccel` 框架内部回退软解（视为可用）。
/// 结果按 (hwaccel, codec, 分辨率) 进程内缓存，同规格视频不重复付费。
pub fn hwaccel_usable(input: &Path, hwaccel: &str, meta: &VideoMeta) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<(String, String, u32, u32), bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (hwaccel.to_string(), meta.codec.clone(), meta.width, meta.height);
    if let Some(&v) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return v;
    }
    let v = smoke_decode_run(input, hwaccel);
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, v);
    v
}

/// 跑一次冒烟 ffmpeg（带墙钟超时；超时视为不可用并 kill）。
fn smoke_decode_run(input: &Path, hwaccel: &str) -> bool {
    let Ok(mut child) = spawn_command(&tool_path("ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-hwaccel", hwaccel])
        .arg("-t")
        .arg("1")
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + SMOKE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
}

// --------------------------------------------------------------------------- //
// GPU vendor 枚举（DESIGN §3.3 步骤 4：候选链按实际硬件排序）
// --------------------------------------------------------------------------- //

/// GPU 厂商。Linux 读 `/sys/class/drm/card*/device/vendor`（PCI id，无外部
/// 依赖）；macOS 恒 Apple（统一显存，链无需重排）；Windows 读注册表显卡
/// 类的 DriverDesc（reg query 全版本可用，厂商名为 ASCII 前缀不受控制台
/// 代码页影响；纯 Intel 如 10700K UHD 630 → qsv 优先链，2026-08-23 真机
/// 首验前默认链把 nvenc 排前导致纯 Intel 机选中即运行期失败）。
/// 多卡混布时独显优先（nvidia > amd > intel）。
pub fn gpu_vendor() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some("apple")
    }
    #[cfg(target_os = "linux")]
    {
        let ids = linux_pci_vendor_ids("/sys/class/drm");
        vendor_from_ids(&ids)
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(out) = spawn_command(std::path::Path::new("reg"))
            .args([
                "query",
                r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}",
                "/s",
                "/v",
                "DriverDesc",
            ])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            let has = |k: &str| text.contains(k);
            if has("nvidia") {
                return Some("nvidia");
            }
            if has("radeon") || has("amd") {
                return Some("amd");
            }
            if has("intel") {
                return Some("intel");
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// 读 DRM 卡的 PCI vendor id 列表（"0x8086\n0x1002"）。仅 Linux 消费
/// （gpu_vendor 的 sysfs 路径）。
#[cfg(target_os = "linux")]
fn linux_pci_vendor_ids(sysfs: &str) -> Vec<u32> {
    let mut ids = vec![];
    if let Ok(cards) = std::fs::read_dir(std::path::Path::new(sysfs)) {
        for card in cards.flatten() {
            let name = card.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("card") || name.contains('-') {
                continue; // 跳过 card0-HDMI-A-1 之类的连接器条目
            }
            if let Ok(s) = std::fs::read_to_string(card.path().join("device/vendor")) {
                if let Some(id) = parse_pci_vendor_id(&s) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// sysfs 的 vendor 文件内容（"0x1002\n"）→ PCI id（十六进制）。
/// （曾按十进制 parse 导致 0x1002→1002 恒不匹配——真机才暴露）
#[cfg(target_os = "linux")]
fn parse_pci_vendor_id(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(target_os = "linux")]
const PCI_NVIDIA: u32 = 0x10de;
#[cfg(target_os = "linux")]
const PCI_AMD: u32 = 0x1002;
#[cfg(target_os = "linux")]
const PCI_INTEL: u32 = 0x8086;

#[cfg(target_os = "linux")]
fn vendor_from_ids(ids: &[u32]) -> Option<&'static str> {
    let mut seen: Vec<&str> = vec![];
    for &id in ids {
        let v = match id {
            PCI_NVIDIA => "nvidia",
            PCI_AMD => "amd",
            PCI_INTEL => "intel",
            _ => continue,
        };
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    match seen.as_slice() {
        [only] => Some(only),
        _ => None, // 无独显直连（iGPU 不在 DRM 列表）或多厂商混布 → 默认链
    }
}

/// 解码 hwaccel 候选链（DESIGN §3.3；vendor 已知时按实际硬件排序）。
/// 从优到劣，最后一项 None 表示软解。
pub fn decode_chain() -> Vec<Option<&'static str>> {
    decode_chain_for(gpu_vendor())
}

/// `decode_chain` 的可注入版（测试用）：NVIDIA 机器 cuda 优先（vaapi 在
/// 无 Intel/AMD 显卡时不可用，排后靠冒烟剔除）；其余保持 vaapi 优先。
pub fn decode_chain_for(vendor: Option<&str>) -> Vec<Option<&'static str>> {
    #[cfg(target_os = "macos")]
    {
        let _ = vendor;
        vec![Some("videotoolbox"), None]
    }
    #[cfg(target_os = "windows")]
    {
        match vendor {
            Some("intel") => vec![Some("qsv"), Some("d3d11va"), Some("cuda"), None],
            _ => vec![Some("cuda"), Some("d3d11va"), Some("qsv"), None],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match vendor {
            Some("nvidia") => vec![Some("cuda"), Some("vaapi"), Some("qsv"), None],
            Some("intel") => vec![Some("vaapi"), Some("qsv"), None],
            _ => vec![Some("vaapi"), Some("cuda"), Some("qsv"), None],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = vendor;
        vec![None]
    }
}

/// H.264 编码器候选链（DESIGN §3.4；vendor 已知时按实际硬件排序）。
pub fn encoder_chain() -> Vec<&'static str> {
    encoder_chain_for(gpu_vendor())
}

/// `encoder_chain` 的可注入版（测试用）：N 卡 nvenc 优先、I 卡 qsv 优先、
/// A 卡 amf 优先（Windows）；Linux 的 N 卡 nvenc 提到 vaapi 前。
pub fn encoder_chain_for(vendor: Option<&str>) -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    {
        let _ = vendor;
        vec!["h264_videotoolbox", "libx264"]
    }
    #[cfg(target_os = "windows")]
    {
        match vendor {
            Some("nvidia") => vec!["h264_nvenc", "h264_qsv", "h264_amf", "libx264"],
            Some("intel") => vec!["h264_qsv", "h264_nvenc", "h264_amf", "libx264"],
            Some("amd") => vec!["h264_amf", "h264_nvenc", "h264_qsv", "libx264"],
            _ => vec!["h264_nvenc", "h264_qsv", "h264_amf", "libx264"],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match vendor {
            Some("nvidia") => vec!["h264_nvenc", "h264_vaapi", "libx264"],
            _ => vec!["h264_vaapi", "h264_nvenc", "libx264"],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = vendor;
        vec!["libx264"]
    }
}

// --------------------------------------------------------------------------- //
// ffmpeg 命令构建（NV12 rawvideo 双向管道，DESIGN §3.2）
// --------------------------------------------------------------------------- //

/// VAAPI 渲染节点：取 /dev/dri/ 下第一个 renderD*（多卡时按序），无则用惯例路径
/// （后续由运行期回退兜底；非 Linux 平台仅用于显式指定 vaapi 编码器的场景）。
fn vaapi_render_device() -> String {
    use std::fs;
    if let Ok(entries) = fs::read_dir("/dev/dri") {
        let mut nodes: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("renderD"))
            .collect();
        nodes.sort();
        if let Some(first) = nodes.first() {
            return format!("/dev/dri/{first}");
        }
    }
    "/dev/dri/renderD128".to_string()
}

/// 解码管道帧格式（DESIGN §3.2）：NV12 rawvideo 主路径；MJPEG 为低配设备
/// 可选路径——管道带宽 ~1/20 起（q95 1080p30 约 2-5MB/s vs NV12 93MB/s），
/// 代价是解码侧 JPEG 编码 + 读侧 JPEG 解码的 CPU 与轻微画质损失。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameFormat {
    /// rawvideo NV12 双向管道（默认）。
    #[default]
    Nv12,
    /// MJPEG：ffmpeg 输出 q≈95 JPEG，读侧解回 NV12 后走同一 transform。
    Mjpeg,
}

/// NV12 解码侧参数（不含程序名）：`[-hwaccel X] -i IN -f rawvideo -pix_fmt nv12 -`
/// （帧由 ffmpeg 内部从 GPU 下载为 NV12 后经 stdout 输出）。
/// 注：M0 用 String 参数，非 UTF-8 路径有损；后续版本改 OsString。
pub fn decode_cmd(input: &Path, hwaccel: Option<&str>) -> Vec<String> {
    let mut c: Vec<String> = ["-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Some(hw) = hwaccel {
        c.push("-hwaccel".into());
        c.push(hw.into());
        // qsv 解码帧留在 GPU（qsv 帧）时 rawvideo+nv12 无法 auto-scale
        // （"Impossible to convert" 即解码回退链触发）——强制下载为 nv12
        // sw 帧（10700K 真机验证；其余 hwaccel 忽略此参数或无副作用）
        if hw == "qsv" {
            c.extend(["-hwaccel_output_format", "nv12"].map(String::from));
        }
    }
    c.push("-i".into());
    c.push(input.to_string_lossy().into_owned());
    c.extend(["-map", "0:v:0"].map(String::from));
    // -vsync 在 FFmpeg 8 已移除，等价物是 -fps_mode passthrough（保帧不重采样）
    c.extend(
        ["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "nv12", "-"]
            .map(String::from),
    );
    c
}

/// MJPEG 解码侧参数（DESIGN §3.2 低配可选路径）：帧压成 q≈95 JPEG（-q:v 2）
/// 输出，读侧由 [`JpegFrameScanner`] 定界并解回 NV12。旋转元数据同样被
/// ffmpeg 应用（输出为显示方向，与 NV12 路径一致）。
pub fn decode_cmd_mjpeg(input: &Path, hwaccel: Option<&str>) -> Vec<String> {
    let mut c: Vec<String> = ["-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Some(hw) = hwaccel {
        c.push("-hwaccel".into());
        c.push(hw.into());
        if hw == "qsv" {
            c.extend(["-hwaccel_output_format", "nv12"].map(String::from));
        }
    }
    c.push("-i".into());
    c.push(input.to_string_lossy().into_owned());
    c.extend(
        ["-map", "0:v:0", "-fps_mode", "passthrough", "-f", "mjpeg", "-q:v", "2", "-"]
            .map(String::from),
    );
    c
}

// --------------------------------------------------------------------------- //
// MJPEG 读侧：变长帧定界 + JPEG→NV12（帧下游与 NV12 路径完全同构）
// --------------------------------------------------------------------------- //

/// JPEG 帧首/帧尾标记（SOI/EOI）。熵编码数据中 0xFF 后必跟 0x00（位填充）
/// 或 RSTn 标记（0xFFD0-D7），故扫描 EOI 无歧义。
const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];

/// MJPEG 字节流的增量定界器：喂入任意大小的字节块，弹出完整 JPEG 帧
/// （SOI..EOI 含端点）；不完整的尾帧留在内部缓冲等待后续数据。
pub struct JpegFrameScanner {
    buf: Vec<u8>,
}

impl JpegFrameScanner {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 喂入一个字节块；弹出的完整帧按序追加到 `out`。
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<Vec<u8>>) {
        self.buf.extend_from_slice(chunk);
        loop {
            let Some(soi) = find_pair(&self.buf, 0, &SOI) else {
                // 无帧首：丢弃全部，但保留可能是 SOI 前半的尾字节 0xFF
                let keep = usize::from(self.buf.last() == Some(&0xFF));
                let cut = self.buf.len() - keep;
                self.buf.drain(..cut);
                return;
            };
            let Some(eoi) = find_pair(&self.buf, soi + 2, &EOI) else {
                // 半帧：丢弃帧首前的杂散字节，等更多数据
                if soi > 0 {
                    self.buf.drain(..soi);
                }
                return;
            };
            let frame = self.buf[soi..eoi + 2].to_vec();
            self.buf.drain(..eoi + 2);
            out.push(frame);
        }
    }
}

impl Default for JpegFrameScanner {
    fn default() -> Self {
        Self::new()
    }
}

fn find_pair(buf: &[u8], from: usize, pair: &[u8; 2]) -> Option<usize> {
    if buf.len() < 2 || from + 1 >= buf.len() {
        return None;
    }
    (from..buf.len() - 1).find(|&i| buf[i] == pair[0] && buf[i + 1] == pair[1])
}

/// 解一帧 JPEG → NV12。尺寸与视频不符（流规格变化）报错。
/// JPEG 经 RGB 往返（BT.601 limited 量化误差 ≤1，远小于 q95 压缩损失）。
pub fn jpeg_to_nv12(jpeg: &[u8], w: usize, h: usize) -> Result<Vec<u8>, String> {
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    let pixels = dec.decode().map_err(|e| format!("JPEG 解码失败: {e}"))?;
    let info = dec.info().ok_or("JPEG 缺少头信息")?;
    if info.width as usize != w || info.height as usize != h {
        return Err(format!(
            "JPEG 帧 {}×{} 与视频 {}×{} 不符",
            info.width, info.height, w, h
        ));
    }
    if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return Err(format!("期望 RGB24 输出，得到 {:?}", info.pixel_format));
    }
    Ok(rgb_to_nv12(&pixels, w, h))
}

/// 交错 RGB24 → NV12（BT.601 limited range；UV 为 2×2 块均值，
/// 与 nv12_to_rgba 的逆变换配对）。w/h 须为偶数（NV12 4:2:0）。
pub fn rgb_to_nv12(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    debug_assert!(w % 2 == 0 && h % 2 == 0, "NV12 需偶数尺寸：{w}×{h}");
    let mut out = vec![0u8; w * h * 3 / 2];
    for i in 0..w * h {
        let (r, g, b) = (rgb[i * 3] as f32, rgb[i * 3 + 1] as f32, rgb[i * 3 + 2] as f32);
        out[i] = (16.0 + 0.2568 * r + 0.5041 * g + 0.0979 * b).round().clamp(0.0, 255.0) as u8;
    }
    let (cw, uv_off) = (w / 2, w * h);
    let uv = &mut out[uv_off..];
    for cy in 0..h / 2 {
        for cx in 0..cw {
            let (mut su, mut sv) = (0.0f32, 0.0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((cy * 2 + dy) * w + cx * 2 + dx) * 3;
                    let (r, g, b) = (rgb[i] as f32, rgb[i + 1] as f32, rgb[i + 2] as f32);
                    su += -0.1482 * r - 0.2910 * g + 0.4392 * b;
                    sv += 0.4392 * r - 0.3678 * g - 0.0714 * b;
                }
            }
            let k = (cy * cw + cx) * 2;
            uv[k] = (128.0 + su * 0.25).round().clamp(0.0, 255.0) as u8;
            uv[k + 1] = (128.0 + sv * 0.25).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// 码率随分辨率档位缩放（DESIGN §3.4"码率随分辨率档位缩放"）：1080p 为 6M 基准。
/// 按长边分档（旋转与宏块对齐不敏感：1088×1920 仍算 1080p）。
/// 仅在用户未显式给出码率（"auto"）时使用。
pub fn auto_bitrate(w: u32, h: u32) -> String {
    match w.max(h) {
        ..=1280 => "3M",  // ≤720p 类（长边 1280）
        ..=1920 => "6M",  // ≤1080p 类
        ..=2560 => "10M", // ≤1440p 类
        _ => "20M",       // 4K+
    }
    .to_string()
}

/// 编码侧选项。
pub struct EncodeOptions {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// 编码器名（如 h264_videotoolbox / h264_nvenc / libx264）。
    pub encoder: String,
    /// 源视频含音轨时传其路径，音轨以 `-c:a copy` 混入。
    pub audio_from: Option<PathBuf>,
    /// 码率型编码器（videotoolbox/amf）的目标码率（"auto" 由 pipe::run 先行解析）。
    pub bitrate: String,
    /// 音轨兜底转码：copy 失败（如 TrueHD in MP4 experimental）时改 AAC 192k
    /// （作用于全部音轨——按轨选编码需逐流枚举，留给后续）。
    pub audio_transcode: bool,
}

/// 编码侧参数（不含程序名）：`-f rawvideo -pixel_format nv12 -video_size WxH -framerate FPS -i - ...`
/// （rawvideo 解复用器的输入选项名是 `-pixel_format`/`-video_size`/`-framerate`，
///  与输出侧的 `-pix_fmt`/`-s`/`-r` 不同）。
/// 分析段空输出命令（`-f null`）：帧直接丢弃、不编码不落盘、无音频第二输入。
/// 两阶段 analyze 的产物是 mask 缓存——编码侧只需保持管道排空（背压防死锁），
/// 旧实现"真编码 + 写探针文件 + 删除"对长片是 GB 级白写（2026-08-21 优化）。
pub fn drain_null_cmd(o: &EncodeOptions) -> Vec<String> {
    vec![
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pixel_format".into(),
        "nv12".into(),
        "-video_size".into(),
        format!("{}x{}", o.width, o.height),
        "-framerate".into(),
        format!("{:.6}", o.fps),
        "-i".into(),
        "-".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ]
}

pub fn encode_cmd(output: &Path, o: &EncodeOptions) -> Vec<String> {
    let mut c: Vec<String> = vec![
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pixel_format".into(),
        "nv12".into(),
        "-video_size".into(),
        format!("{}x{}", o.width, o.height),
        "-framerate".into(),
        format!("{:.6}", o.fps),
        "-i".into(),
        "-".into(),
    ];

    if let Some(src) = &o.audio_from {
        c.push("-i".into());
        c.push(src.to_string_lossy().into_owned());
    }
    c.push("-map".into());
    c.push("0:v:0".into());

    c.push("-c:v".into());
    c.push(o.encoder.clone());
    // 各编码器质量参数（DESIGN §3.4 的 M0 子集；完整档位在 M2 设置项中开放）
    match o.encoder.as_str() {
        "h264_videotoolbox" => {
            // -realtime 1：编码器实时调度提示，降低编码功耗（DESIGN §6 效率清单）
            c.extend(
                ["-b:v", &o.bitrate, "-realtime", "1", "-allow_sw", "1", "-tag:v", "avc1"]
                    .map(String::from),
            );
        }
        e if e.contains("nvenc") => {
            // -b:v 0 是必需的，否则默认 2M 封顶
            c.extend(
                ["-preset", "p4", "-rc", "vbr", "-cq", "23", "-b:v", "0"]
                    .map(String::from),
            );
        }
        e if e.ends_with("qsv") => {
            // QSV（Intel）：rawvideo stdin 为 sw 帧，须显式建 QSV 设备并
            // hwupload 上 GPU（对齐 vaapi 分支模式；缺此链时 sw 帧直喂
            // h264_qsv 在多驱动上报 "no device" 即运行期回退——纯 Intel
            // 真机全链 qsv→nvenc→amf→libx264 连环回退的根因）。
            // look_ahead/extbrc 在 Gen9.5（UHD 630）驱动兼容性差，弃用
            // 设备参数作前缀直接拼接（insert(0) 已两次写反：正序循环插入、
            // 倒序数组再 .rev()，均产出 "-filter_hw_device qsv=hw" 即刻败——
            // 真机 stderr 两次定位；形状由 encode_cmd_qsv_device_prefix_order
            // 单元测试锁死）
            let mut prefix: Vec<String> =
                ["-init_hw_device", "qsv=hw", "-filter_hw_device", "hw"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            prefix.extend(c);
            c = prefix;
            c.extend(
                ["-global_quality", "22", "-vf", "format=nv12,hwupload"]
                    .map(String::from),
            );
        }
        e if e.ends_with("vaapi") => {
            // VAAPI（AMD/Intel）：无 CRF，CQP 全局质量；设备须在输入前创建，
            // 帧经 hwupload 上 GPU。设备缺失/不支持时运行期回退下一候选。
            let dev = vaapi_render_device();
            c.insert(0, dev);
            c.insert(0, "-vaapi_device".into());
            c.extend(
                ["-rc_mode", "CQP", "-global_quality", "22", "-vf", "format=nv12|vaapi,hwupload"]
                    .map(String::from),
            );
        }
        e if e.ends_with("amf") => {
            c.extend(["-quality", "balanced", "-b:v", &o.bitrate].map(String::from));
        }
        "libx264" => {
            c.extend(["-crf", "20", "-preset", "veryfast"].map(String::from));
        }
        _ => {}
    }
    // 音轨/字幕/章节全量保留（DESIGN §3.2：旧版仅 -map 1:a:0? 会丢多音轨、
    // 字幕与章节）。输入 1 = 源文件（输入 0 是 rawvideo 管道，无这些流）。
    if o.audio_from.is_some() {
        c.extend(["-map", "1:a?"].map(String::from));
        if o.audio_transcode {
            c.extend(["-c:a", "aac", "-b:a", "192k"].map(String::from));
        } else {
            c.extend(["-c:a", "copy"].map(String::from));
        }
        // 字幕：mp4/mov 用 mov_text 转封装（原生编码器必在）；mkv 直接 copy；
        // 其余容器不映射（webm 只收 webvtt 等，copy 会直接失败）
        let ext = output
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "mp4" | "m4v" | "mov" => {
                c.extend(["-map", "1:s?", "-c:s", "mov_text"].map(String::from));
            }
            "mkv" => {
                c.extend(["-map", "1:s?", "-c:s", "copy"].map(String::from));
            }
            _ => {}
        }
        // 章节与容器元数据（标题等）来自源文件；默认取输入 0（管道，恒为空）
        c.extend(["-map_chapters", "1", "-map_metadata", "1"].map(String::from));
    }
    // faststart 仅 mp4/mov 族有效（mkv 无此概念，旧版 ffmpeg 对未知 muxer 选项报错）
    let mp4ish = output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"));
    if mp4ish {
        c.push("-movflags".into());
        c.push("+faststart".into());
    }
    c.push(output.to_string_lossy().into_owned());
    c
}

// --------------------------------------------------------------------------- //
// 预览辅助（UI 关键帧）
// --------------------------------------------------------------------------- //

/// NV12 → RGBA（BT.601 limited range），用于 UI 预览帧。
pub fn nv12_to_rgba(nv12: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let uv = &nv12[w * h..];
    for y in 0..h {
        for x in 0..w {
            let yy = (nv12[y * w + x] as f32 - 16.0) * 1.1644;
            let u = uv[(y / 2) * w + (x / 2) * 2] as f32 - 128.0;
            let v = uv[(y / 2) * w + (x / 2) * 2 + 1] as f32 - 128.0;
            let i = (y * w + x) * 4;
            out[i] = (yy + 1.5960 * v).clamp(0.0, 255.0) as u8;
            out[i + 1] = (yy - 0.3917 * u - 0.8130 * v).clamp(0.0, 255.0) as u8;
            out[i + 2] = (yy + 2.0172 * u).clamp(0.0, 255.0) as u8;
            out[i + 3] = 255;
        }
    }
    out
}

/// 抽取指定时间位置的单帧（NV12）。
/// 位置会钳制到最后一片帧区间的中点（末帧实际时间戳 = (n-1)/fps，
/// 播放器给出的 duration 末尾 0.0x s 内没有帧，`-ss` 到那里会取到 0 字节）；
/// 若仍取不到（元数据误差），回退 0.25s 重试一次。
pub fn decode_frame_at(path: &Path, pos_secs: f64, meta: &VideoMeta) -> Result<Vec<u8>, ProbeError> {
    let frame_bytes = meta.width as usize * meta.height as usize * 3 / 2;
    let bin = tool_path("ffmpeg");
    let pos = clamp_seek_pos(pos_secs, meta);
    let mut attempt = pos;
    for _ in 0..2 {
        let out = spawn_command(&bin)
            .args(["-loglevel", "error", "-nostdin", "-ss"])
            .arg(format!("{attempt:.3}"))
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "nv12", "-"])
            .output()
            .map_err(|source| ProbeError::Spawn {
                bin: bin.to_string_lossy().into_owned(),
                source,
            })?;
        if out.status.success() && out.stdout.len() >= frame_bytes {
            return Ok(out.stdout[..frame_bytes].to_vec());
        }
        if !out.status.success() {
            return Err(ProbeError::Inspect {
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().into(),
            });
        }
        attempt = (attempt - 0.25).max(0.0); // 空输出：元数据偏差，回退重试
    }
    Err(ProbeError::Inspect {
        code: 0,
        stderr: format!(
            "抽帧数据不足（含回退重试）：pos={pos_secs:.3}s（钳制后 {pos:.3}s）"
        ),
    })
}

/// 钳制抽帧位置到最后一片帧区间中点：duration 末尾的半帧余量内无帧。
fn clamp_seek_pos(pos_secs: f64, meta: &VideoMeta) -> f64 {
    let Some(dur) = meta.duration_secs else { return pos_secs.max(0.0) };
    let half_frame = 0.5 / meta.fps.max(1.0);
    // 末帧时间戳 ≈ dur - half_frame；再保守 1 帧余量
    pos_secs.clamp(0.0, (dur - half_frame * 3.0).max(0.0))
}

/// NV12 → 指定尺寸的 RGBA（最近邻采样，BT.601）；用于处理中的对照预览小图。
pub fn nv12_to_rgba_scaled(nv12: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 4];
    let uv = &nv12[w * h..];
    for dy in 0..dh {
        let sy = dy * h / dh.max(1);
        for dx in 0..dw {
            let sx = dx * w / dw.max(1);
            let yy = (nv12[sy * w + sx] as f32 - 16.0) * 1.1644;
            let u = uv[(sy / 2) * w + (sx / 2) * 2] as f32 - 128.0;
            let v = uv[(sy / 2) * w + (sx / 2) * 2 + 1] as f32 - 128.0;
            let i = (dy * dw + dx) * 4;
            out[i] = (yy + 1.5960 * v).clamp(0.0, 255.0) as u8;
            out[i + 1] = (yy - 0.3917 * u - 0.8130 * v).clamp(0.0, 255.0) as u8;
            out[i + 2] = (yy + 2.0172 * u).clamp(0.0, 255.0) as u8;
            out[i + 3] = 255;
        }
    }
    out
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_reach_repo_root_from_app_bundle_layout() {
        // macOS .app：exe 在 Contents/MacOS 下，仓库根需上溯 9 级
        let exe = Path::new("/repo/app/build/prod/Debug/App.app/Contents/MacOS/App");
        let roots = roots_from(Some(exe), Some(Path::new("/")));
        assert!(
            roots.iter().any(|r| r.ends_with("/repo")),
            "祖先链应到达仓库根: {roots:?}"
        );
        assert!(roots.iter().any(|r| r.ends_with("Contents/MacOS")));
    }

    #[test]
    fn roots_include_cwd_parents() {
        let roots = roots_from(None, Some(Path::new("/repo/app")));
        assert!(roots.iter().any(|r| r.ends_with("/repo/app")));
        assert!(roots.iter().any(|r| r.ends_with("/repo")));
    }

    #[test]
    fn roots_dedup() {
        let roots = roots_from(None, None);
        assert!(roots.is_empty());
    }

    use super::*;

    const MP4_HEVC_STDERR: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'in.mp4':\n  Duration: 00:00:05.07, start: 0.000000, bitrate: 1089 kb/s\n  Stream #0:0[0x1](und): Video: hevc (Main 10) (hvc1 / 0x31763763), yuv420p10le(pc), 1920x1080, 1088 kb/s, 15 fps, 15 tbr, 16k tbn (default)\n  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6D703461), 44100 Hz, stereo, fltp, 61 kb/s (default)\nAt least one output file must be specified\n";

    #[test]
    fn parses_ffmpeg_stderr_mp4() {
        let m = meta_from_ffmpeg_stderr(MP4_HEVC_STDERR, Path::new("in.mp4"), Some(1)).unwrap();
        assert_eq!((m.width, m.height), (1920, 1080));
        assert_eq!(m.fps, 15.0);
        assert_eq!(m.codec, "hevc");
        assert_eq!(m.pix_fmt.as_deref(), Some("yuv420p10le"));
        assert!((m.duration_secs.unwrap() - 5.07).abs() < 0.01);
        assert_eq!(m.total_frames, Some(76)); // 5.07×15=76.05
        assert!(m.has_audio);
    }

    #[test]
    fn parses_tbr_only_and_no_audio() {
        // fps 缺失时回退 tbr；无 Audio 行 → has_audio=false
        let s = "Input #0, matroska,webm, from 'in.mkv':\n  Duration: 00:00:10.00, start: 0.000000\n  Stream #0:0: Video: h264 (High), yuv420p(progressive), 3840x2160, 30 tbr, 1k tbn\n";
        let m = meta_from_ffmpeg_stderr(s, Path::new("in.mkv"), Some(1)).unwrap();
        assert_eq!(m.fps, 30.0);
        assert!(!m.has_audio);
        assert_eq!(m.total_frames, Some(300));
    }

    #[test]
    fn rejects_stderr_without_video() {
        let s = "in.mp4: No such file or directory\n";
        assert!(meta_from_ffmpeg_stderr(s, Path::new("in.mp4"), Some(1)).is_err());
    }

    #[test]
    fn scaled_rgba_matches_full_at_same_size_and_shrinks() {
        let (w, h) = (64, 64);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        nv12[..w * h].fill(235);
        let full = nv12_to_rgba(&nv12, w, h);
        let same = nv12_to_rgba_scaled(&nv12, w, h, w, h);
        assert_eq!(full, same, "同尺寸应与全尺寸转换一致");
        let small = nv12_to_rgba_scaled(&nv12, w, h, 16, 16);
        assert_eq!(small.len(), 16 * 16 * 4);
        assert!(small.iter().step_by(4).all(|&r| r > 200), "白帧 R 通道应接近 255");
    }

    #[test]
    fn clamp_seek_pos_bounds() {
        let meta = |dur: Option<f64>, fps: f64| VideoMeta {
            width: 1920, height: 1080, fps, codec: "h264".into(), pix_fmt: None,
            total_frames: None, duration_secs: dur, has_audio: false,
            audio_codecs: vec![],
            rotation: 0.0,
        };
        // 75 帧 @15fps，duration 5.0：末帧 ≈4.933，上限 = 5.0 - 0.1 = 4.9
        let m = meta(Some(5.0), 15.0);
        assert_eq!(clamp_seek_pos(4.999, &m), 4.9);
        assert_eq!(clamp_seek_pos(2.0, &m), 2.0);
        assert_eq!(clamp_seek_pos(-1.0, &m), 0.0);
        // 极短视频不越界为负
        let m2 = meta(Some(0.1), 15.0);
        assert_eq!(clamp_seek_pos(0.09, &m2), 0.0);
        // 无时长信息：原样（非负）
        let m3 = meta(None, 30.0);
        assert_eq!(clamp_seek_pos(3.5, &m3), 3.5);
    }

    #[test]
    fn dims_and_number_parsers() {
        assert_eq!(dims_in("1920x1080, 1088 kb/s"), Some((1920, 1080)));
        assert_eq!(dims_in("(hvc1 / 0x31763763), yuv420p"), None);
        assert_eq!(number_before(", 29.97 fps, 30 tbr", "fps"), Some(29.97));
        assert_eq!(number_before("30 tbr", "tbr"), Some(30.0));
        assert_eq!(number_before("no number tbr", "tbr"), None);
    }

    #[test]
    fn decode_cmd_shapes_correctly() {
        let c = decode_cmd(Path::new("in.mp4"), Some("videotoolbox"));
        let joined = c.join(" ");
        assert!(joined.starts_with("-loglevel error -nostdin -hwaccel videotoolbox -i in.mp4"));
        assert!(joined.ends_with("-fps_mode passthrough -f rawvideo -pix_fmt nv12 -"));
    }

    #[test]
    fn decode_cmd_mjpeg_shapes_correctly() {
        let c = decode_cmd_mjpeg(Path::new("in.mp4"), None);
        let joined = c.join(" ");
        assert!(joined.starts_with("-loglevel error -nostdin -i in.mp4"));
        assert!(joined.ends_with("-fps_mode passthrough -f mjpeg -q:v 2 -"));
    }

    #[test]
    fn jpeg_scanner_frames_across_chunk_boundaries() {
        let mut sc = JpegFrameScanner::new();
        let mut out = vec![];
        // 杂散前缀 + 尾字节 0xFF 跨块（SOI 前半）
        sc.push(b"junk\x00\xff", &mut out);
        assert!(out.is_empty(), "无完整帧");
        // SOI 后半 + 载荷 + EOI
        let f1 = [0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9];
        sc.push(&f1[1..], &mut out);
        assert_eq!(out.len(), 1, "0xFF 跨块拼接后应成帧");
        assert_eq!(out[0], f1);
        // 半帧跨块 + 尾部紧跟第二帧（粘包）
        let mut out2 = vec![];
        sc.push(&[0xFF, 0xD8, 9], &mut out2);
        assert!(out2.is_empty());
        sc.push(&[8, 0xFF, 0xD9, 0xFF, 0xD8, 7, 0xFF, 0xD9], &mut out2);
        assert_eq!(out2.len(), 2, "补齐半帧 + 紧跟的第二帧");
        assert_eq!(out2[0], [0xFF, 0xD8, 9, 8, 0xFF, 0xD9]);
        assert_eq!(out2[1], [0xFF, 0xD8, 7, 0xFF, 0xD9]);
    }

    #[test]
    fn rgb_to_nv12_roundtrip_within_tolerance() {
        // BT.601 limited 往返（rgb→nv12→rgba）：均匀色精确（Y=235 白）、
        // 平滑渐变误差 ≤6（4:2:0 色度子采样的块内混合，非转换误差）
        let (w, h) = (8, 8);
        // 1) 整幅纯白：U=V=128、Y=235，往返应近无损
        let white = vec![255u8; w * h * 3];
        let nv12 = rgb_to_nv12(&white, w, h);
        assert_eq!(nv12[0], 235, "BT.601 limited 白 = Y235");
        assert!(nv12[w * h..].iter().all(|&v| v == 128), "无色度");
        let rgba = nv12_to_rgba(&nv12, w, h);
        assert!(rgba.chunks(4).all(|p| p[0] >= 253 && p[1] >= 253 && p[2] >= 253));
        // 2) 平滑渐变（小步长：2×2 块内色度摆幅 ≤4 → 子采样混合误差有限）
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = (x * 5) as u8;
                rgb[i + 1] = (y * 5) as u8;
                rgb[i + 2] = (60 + (x + y) * 2) as u8;
            }
        }
        let nv12 = rgb_to_nv12(&rgb, w, h);
        assert_eq!(nv12.len(), w * h * 3 / 2);
        let rgba = nv12_to_rgba(&nv12, w, h);
        for i in 0..w * h {
            for c in 0..3 {
                let d = (rgba[i * 4 + c] as i32 - rgb[i * 3 + c] as i32).abs();
                assert!(d <= 6, "往返误差过大: i={i} c={c} d={d}");
            }
        }
    }

    #[test]
    fn probe_parses_rotation_and_swaps_dims() {
        let stderr = "Input #0, mov,mp4, from 'in.mp4':\n  Duration: 00:00:05.00\n  Stream #0:0(und): Video: hevc (Main), yuv420p, 1920x1080, 15 fps\n  Stream #0:1(und): Audio: truehd (mlpa), 44100 Hz\n      Display Matrix: rotation of -90.00 degrees\n";
        let m = meta_from_ffmpeg_stderr(stderr, Path::new("in.mp4"), Some(0)).unwrap();
        assert_eq!((m.width, m.height), (1080, 1920), "±90 应交换宽高");
        assert!((m.rotation + 90.0).abs() < 0.01);
        assert_eq!(m.audio_codecs, vec!["truehd".to_string()]);
    }

    #[test]
    fn probe_no_rotation_keeps_dims() {
        let stderr = "Input #0, from 'in.mp4':\n  Stream #0:0(und): Video: h264, 1920x1080, 30 fps\n  Stream #0:1(und): Audio: aac, 44100 Hz\n";
        let m = meta_from_ffmpeg_stderr(stderr, Path::new("in.mp4"), Some(0)).unwrap();
        assert_eq!((m.width, m.height), (1920, 1080));
        assert_eq!(m.rotation, 0.0);
        assert_eq!(m.audio_codecs, vec!["aac".to_string()]);
    }

    #[test]
    fn parses_all_audio_codecs() {
        // 多音轨：aac + truehd 混布 → 两条都解析（任一不兼容即触发 AAC 转码）
        let s = "Input #0, mov,mp4, from 'in.mp4':\n  Duration: 00:00:05.00\n  Stream #0:0(und): Video: h264, 1920x1080, 30 fps\n  Stream #0:1(und): Audio: aac (LC), 44100 Hz\n  Stream #0:2(und): Audio: truehd (mlpa), 48000 Hz\n";
        let m = meta_from_ffmpeg_stderr(s, Path::new("in.mp4"), Some(0)).unwrap();
        assert_eq!(m.audio_codecs, vec!["aac".to_string(), "truehd".to_string()]);
    }

    #[test]
    fn encode_cmd_audio_transcode_uses_aac() {
        let o = EncodeOptions {
            width: 640, height: 360, fps: 30.0,
            encoder: "libx264".into(),
            audio_from: Some(PathBuf::from("in.mp4")),
            bitrate: "6M".into(),
            audio_transcode: true,
        };
        let joined = encode_cmd(Path::new("out.mp4"), &o).join(" ");
        assert!(joined.contains("-map 1:a? -c:a aac -b:a 192k"));
        assert!(!joined.contains("-c:a copy"));
    }

    #[test]
    fn encode_cmd_shapes_correctly() {
        let o = EncodeOptions {
            width: 1280,
            height: 720,
            fps: 29.970029,
            encoder: "h264_videotoolbox".into(),
            audio_from: Some(PathBuf::from("in.mp4")),
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let joined = encode_cmd(Path::new("out.mp4"), &o).join(" ");
        assert!(joined.contains("-pixel_format nv12 -video_size 1280x720"));
        assert!(joined.contains("-framerate 29.970029 -i -"));
        assert!(joined.contains("-c:v h264_videotoolbox"));
        assert!(joined.contains("-realtime 1"), "VideoToolbox 应带实时调度提示");
        assert!(joined.contains("-map 1:a? -c:a copy"), "全部音轨 copy");
        assert!(joined.contains("-map 1:s? -c:s mov_text"), "mp4 字幕转 mov_text");
        assert!(joined.contains("-map_chapters 1 -map_metadata 1"), "章节与元数据取自源文件");
        assert!(joined.ends_with("-movflags +faststart out.mp4"));
    }

    #[test]
    fn encode_cmd_qsv_device_prefix_order() {
        // 回归锚：设备前缀顺序曾两次写反（insert(0) 心智负担），真机 stderr
        // "Invalid filter device qsv=hw" 定位——前四参必须严格为此序
        let o = EncodeOptions {
            width: 640, height: 360, fps: 30.0,
            encoder: "h264_qsv".into(),
            audio_from: None,
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let c = encode_cmd(Path::new("out.mp4"), &o);
        assert_eq!(&c[..4], &["-init_hw_device", "qsv=hw", "-filter_hw_device", "hw"]);
        assert!(c.windows(2).any(|w| w == ["-vf", "format=nv12,hwupload"]));
        assert!(c.contains(&"-global_quality".to_string()));
    }

    #[test]
    fn encode_cmd_amf_has_quality_params() {
        // 回归：amf 此前落空臂（Windows AMD 拿不到质量参数）
        let o = EncodeOptions {
            width: 1920, height: 1080, fps: 30.0,
            encoder: "h264_amf".into(),
            audio_from: None,
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let joined = encode_cmd(Path::new("out.mp4"), &o).join(" ");
        assert!(joined.contains("-quality balanced -b:v 6M"));
    }

    #[test]
    fn encode_cmd_mkv_subs_copy_and_no_movflags() {
        let o = EncodeOptions {
            width: 640, height: 360, fps: 30.0,
            encoder: "libx264".into(),
            audio_from: Some(PathBuf::from("in.mkv")),
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let joined = encode_cmd(Path::new("out.mkv"), &o).join(" ");
        assert!(joined.contains("-map 1:s? -c:s copy"), "mkv 字幕直接 copy");
        assert!(!joined.contains("movflags"), "mkv 无 faststart 概念");
        assert!(joined.ends_with("out.mkv"));
    }

    #[test]
    fn encode_cmd_no_audio_source_maps_nothing_extra() {
        let o = EncodeOptions {
            width: 640, height: 360, fps: 30.0,
            encoder: "libx264".into(),
            audio_from: None,
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let joined = encode_cmd(Path::new("out.mp4"), &o).join(" ");
        assert!(!joined.contains("-map 1:"));
        assert!(!joined.contains("map_chapters"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parses_sysfs_vendor_string() {
        assert_eq!(parse_pci_vendor_id("0x1002\n"), Some(0x1002));
        assert_eq!(parse_pci_vendor_id("0x10de"), Some(0x10de));
        assert_eq!(parse_pci_vendor_id("0x8086\n"), Some(0x8086));
        assert_eq!(parse_pci_vendor_id("garbage"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn vendor_from_pci_ids() {
        assert_eq!(vendor_from_ids(&[0x1002]), Some("amd"));
        assert_eq!(vendor_from_ids(&[0x10de, 0x10de]), Some("nvidia"));
        assert_eq!(vendor_from_ids(&[0x8086]), Some("intel"));
        assert_eq!(vendor_from_ids(&[]), None, "无 DRM 卡（iGPU 直连场景缺失）");
        assert_eq!(
            vendor_from_ids(&[0x10de, 0x1002]),
            None,
            "混布多厂商 → 保守默认链"
        );
        assert_eq!(vendor_from_ids(&[0x1234, 0x5678]), None, "未知厂商忽略");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_chains_reorder_by_vendor() {
        let d = decode_chain_for(Some("nvidia"));
        assert_eq!(d.first(), Some(&Some("cuda")), "N 卡解码 cuda 优先");
        let e = encoder_chain_for(Some("nvidia"));
        assert_eq!(e.first(), Some(&"h264_nvenc"), "N 卡编码 nvenc 优先");
        // AMD/未知：vaapi 优先（默认保守链）
        assert_eq!(decode_chain_for(Some("amd")).first(), Some(&Some("vaapi")));
        assert_eq!(encoder_chain_for(None).first(), Some(&"h264_vaapi"));
        // Intel：无 cuda 噪声项
        let di = decode_chain_for(Some("intel"));
        assert!(di.contains(&Some("qsv")) && !di.contains(&Some("cuda")));
    }

    #[test]
    fn auto_bitrate_scales_by_resolution_tier() {
        assert_eq!(auto_bitrate(1280, 720), "3M");
        assert_eq!(auto_bitrate(1920, 1080), "6M");
        assert_eq!(auto_bitrate(1088, 1920), "6M"); // 竖屏 1080p
        assert_eq!(auto_bitrate(2560, 1440), "10M");
        assert_eq!(auto_bitrate(3840, 2160), "20M");
        assert_eq!(auto_bitrate(640, 360), "3M");
    }

    #[test]
    fn encode_cmd_nvenc_includes_zero_bitrate() {
        let o = EncodeOptions {
            width: 640,
            height: 360,
            fps: 30.0,
            encoder: "h264_nvenc".into(),
            audio_from: None,
            bitrate: "6M".into(),
            audio_transcode: false,
        };
        let joined = encode_cmd(Path::new("out.mp4"), &o).join(" ");
        assert!(joined.contains("-rc vbr -cq 23 -b:v 0"));
        assert!(!joined.contains("-map 1:a"));
    }
}
