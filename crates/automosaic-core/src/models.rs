//! 模型清单管理（DESIGN §8 模型分发的本地部分）：
//! `models/manifest.json` 的解析、模型路径解析（仓库根 / .app Resources / cwd）、
//! SHA256 完整性校验。预设（[`crate::preset`]）引用的模型按文件名在此解析。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::media::search_roots;

/// manifest.json 单个模型条目。
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub batch_file: Option<String>,
    pub imgsz: u32,
    pub sha256: String,
    /// 批推理伴生文件（-b4）的独立哈希（batch_file 存在时必需）。
    #[serde(default)]
    pub sha256_batch: Option<String>,
    pub size_mb: f64,
    /// 主下载源（GitHub Releases，目录 URL，文件名自动拼接）。
    #[serde(default)]
    pub url: Option<String>,
    /// 镜像下载源（ModelScope）。
    #[serde(default)]
    pub mirror_url: Option<String>,
    /// 主文件完整 URL（文件名 ≠ 下载名时用，如 HF 单文件直链；
    /// 优先于 url 拼接）。M5 组件模型（GD/SAM/Retina/OSNet）用。
    #[serde(default)]
    pub direct_url: Option<String>,
    #[serde(default)]
    pub direct_mirror: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub models: Vec<ManifestEntry>,
}

/// 用户级模型目录（下载目标 / 手动放置位置）：
/// macOS `~/Library/Application Support/Simple AutoMosaic/models`，
/// Linux `~/.local/share/Simple AutoMosaic/models`，Windows `%APPDATA%\Simple AutoMosaic\models`。
pub fn user_models_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local/share"))
                    .unwrap_or_else(std::env::temp_dir)
            })
    };
    base.join("Simple AutoMosaic").join("models")
}

/// 在搜索根下查找并加载 manifest.json；不存在返回 None（旧布局仍可用文件名直连）。
pub fn load_manifest() -> Option<Manifest> {
    let path = find_manifest()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn find_manifest() -> Option<PathBuf> {
    candidate_roots()
        .iter()
        .map(|r| r.join("manifest.json"))
        .find(|p| p.is_file())
}

/// 模型目录候选：环境变量 AUTOMOSAIC_MODELS_DIR（独立部署指向模型集，
/// 对齐 AUTOMOSAIC_FFMPEG_DIR 模式）→ 用户数据目录（手动放置，优先以覆盖
/// 打包副本）→ 各搜索根下的 `models/` 与 `Resources/models/`（.app 打包布局）。
fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    if let Some(dir) = std::env::var_os("AUTOMOSAIC_MODELS_DIR").filter(|d| !d.is_empty()) {
        roots.push(PathBuf::from(dir));
    }
    let user = user_models_dir();
    if !roots.contains(&user) {
        roots.push(user);
    }
    for root in search_roots() {
        let m = root.join("models");
        let r = root.join("Resources").join("models");
        if !roots.contains(&m) {
            roots.push(m);
        }
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    roots
}

/// 在清单中按文件名查条目。
impl Manifest {
    pub fn find(&self, file: &str) -> Option<&ManifestEntry> {
        self.models.iter().find(|m| m.file == file)
    }
}

/// 模型路径解析：原样 → 各模型目录候选（见 [`candidate_roots`]）。
/// 覆盖三种场景：flutter run（cwd=app/）、cargo test（cwd=crate/）、
/// Finder 双击 .app（cwd=/，靠 exe 祖先链走到仓库根/打包资源）。
pub fn resolve_model(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.exists() {
        return p;
    }
    let name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    candidate_roots()
        .iter()
        .map(|r| r.join(&name))
        .find(|c| c.exists())
        .unwrap_or(p)
}

/// 依次尝试候选文件名（如预设模型 → 旧模型回退链），返回第一个存在的路径。
pub fn resolve_first(candidates: &[&str]) -> Option<PathBuf> {
    candidates.iter().map(|c| resolve_model(c)).find(|p| p.exists())
}

/// 流式计算文件 SHA256（十六进制小写）。文件不存在返回 None。
pub fn sha256_of(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    use std::io::Read;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// 校验文件 SHA256 与清单是否一致。文件缺失返回 None（区别于校验失败）。
pub fn verify_sha256(path: &Path, expected: &str) -> Option<bool> {
    sha256_of(path).map(|actual| actual.eq_ignore_ascii_case(expected))
}

// --------------------------------------------------------------------------- //
// 模型下载（DESIGN §8 应用内下载：主源 GitHub Releases，镜像 ModelScope 回退）
// --------------------------------------------------------------------------- //

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("manifest 中无 {0} 或未配置下载地址")]
    NoSource(String),
    #[error("下载失败（{url}）：{err}")]
    Http { url: String, err: String },
    #[error("SHA256 校验失败：{path}")]
    BadSha { path: PathBuf },
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 单个文件下载：流式写入临时文件 → SHA 校验 → 原子改名落位。
/// `progress` 回调 (已下载字节, 总字节[未知则为 0])。
fn download_one(
    url: &str,
    dest: &Path,
    expected_sha: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), DownloadError> {
    use sha2::{Digest, Sha256};
    // 仅连接超时，不设整体超时（大模型在慢网络下耗时长）
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| DownloadError::Http { url: url.into(), err: e.to_string() })?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();

    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| DownloadError::Http { url: url.into(), err: e.to_string() })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    drop(file);
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownloadError::BadSha { path: dest.to_path_buf() });
    }
    std::fs::rename(&tmp, dest)?; // 同目录内改名，落位原子
    Ok(())
}

/// 下载一个 manifest 条目（主文件 + 批推理伴生文件）到用户模型目录。
/// 依次尝试主源与镜像；每个文件独立 SHA 校验，失败即中止（不留半成品）。
pub fn download_entry(
    entry: &ManifestEntry,
    progress: impl FnMut(&str, u64, u64),
) -> Result<Vec<PathBuf>, DownloadError> {
    download_entry_to(entry, &user_models_dir(), progress)
}

/// 同 [`download_entry`]，但下载到指定目录（测试用）。
pub fn download_entry_to(
    entry: &ManifestEntry,
    dir: &Path,
    mut progress: impl FnMut(&str, u64, u64),
) -> Result<Vec<PathBuf>, DownloadError> {
    let mut files: Vec<(&str, &str)> = vec![(&entry.file, &entry.sha256)];
    if let Some(b) = &entry.batch_file {
        let sha = entry.sha256_batch.as_deref().ok_or_else(|| {
            DownloadError::NoSource(format!("{}（manifest 缺少批文件 sha256）", b))
        })?;
        files.push((b.as_str(), sha));
    }

    let mut out = Vec::new();
    for (name, sha) in files {
        let mut urls: Vec<String> = Vec::new();
        // 完整直链优先（HF 单文件：下载名 ≠ 本地名）
        if name == entry.file {
            if let Some(u) = &entry.direct_url {
                urls.push(u.clone());
            }
            if let Some(u) = &entry.direct_mirror {
                urls.push(u.clone());
            }
        }
        [entry.url.as_ref(), entry.mirror_url.as_ref()]
            .into_iter()
            .flatten()
            .for_each(|u| urls.push(format!("{u}/{name}")));
        if urls.is_empty() {
            return Err(DownloadError::NoSource(entry.file.clone()));
        }
        let dest = dir.join(name);
        let mut last_err = None;
        let mut ok = false;
        for url in &urls {
            let pname = name.to_string();
            let p = &mut progress;
            match download_one(url, &dest, sha, move |d, t| p(&pname, d, t)) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if !ok {
            let _ = std::fs::remove_file(dest.with_extension("part"));
            return Err(last_err.unwrap_or(DownloadError::NoSource(name.to_string())));
        }
        out.push(dest);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI 检出无 models/（gitignore），文件依赖型测试优雅跳过。
    fn models_present() -> bool {
        Path::new("models/manifest.json").exists()
            || resolve_model("yolo11n-seg.onnx").exists()
    }

    #[test]
    fn resolve_model_finds_repo_models_dir() {
        if !models_present() {
            eprintln!("skip: 无 models/（CI 环境）");
            return;
        }
        // cargo test cwd = crate 目录，search_roots 会走到仓库根
        let p = resolve_model("yolo11n-seg.onnx");
        assert!(p.exists(), "应找到 {p:?}");
        assert!(p.to_string_lossy().contains("models"));
    }

    /// 极简本地 HTTP 服务器（单连接顺序响应，供下载器测试）。
    fn serve(paths: Vec<(String, Vec<u8>)>) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or_default().to_string();
                match paths.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    None => {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        (addr, handle)
    }

    fn entry_with(url: String, mirror: Option<String>, body: &[u8]) -> ManifestEntry {
        let mut e = ManifestEntry {
            file: "test-model.onnx".into(),
            batch_file: None,
            imgsz: 640,
            sha256: sha256_of(std::path::Path::new("/nonexistent")).unwrap_or_default(),
            sha256_batch: None,
            size_mb: 0.1,
            url: Some(url),
            mirror_url: mirror,
            direct_url: None,
            direct_mirror: None,
        };
        e.sha256 = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(body);
            format!("{:x}", h.finalize())
        };
        e
    }

    #[test]
    fn download_entry_verifies_sha_and_falls_back_to_mirror() {
        let body = b"hello-model-bytes".to_vec();
        // 主源 404（不存在路径），镜像存在 → 应回退成功
        let (addr, _server) = serve(vec![
            ("/mirror/test-model.onnx".into(), body.clone()),
        ]);
        let e = entry_with(
            format!("http://{addr}/primary"),
            Some(format!("http://{addr}/mirror")),
            &body,
        );
        let dir = std::env::temp_dir().join(format!("automosaic_test_{}", std::process::id()));
        let paths = download_entry_to(&e, &dir, |_, _, _| {}).unwrap();
        assert!(paths[0].exists());
        assert_eq!(std::fs::read(&paths[0]).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);

        // SHA 不匹配 → BadSha
        let (addr2, _server2) = serve(vec![("/ok/test-model.onnx".into(), b"corrupted".to_vec())]);
        let mut e2 = entry_with(format!("http://{addr2}/ok"), None, &body);
        e2.mirror_url = None;
        let err = download_entry_to(&e2, &dir, |_, _, _| {}).unwrap_err();
        assert!(matches!(err, DownloadError::BadSha { .. }), "{err:?}");
    }

    #[test]
    fn resolve_first_falls_back_in_order() {
        if !models_present() {
            eprintln!("skip: 无 models/（CI 环境）");
            return;
        }
        let p = resolve_first(&["definitely-missing.onnx", "yolo11n-seg.onnx"]);
        assert!(p.is_some());
        assert!(resolve_first(&["definitely-missing.onnx"]).is_none());
    }

    #[test]
    fn manifest_loads_and_finds_entries() {
        if !models_present() {
            eprintln!("skip: 无 models/（CI 环境）");
            return;
        }
        let m = load_manifest().expect("仓库内应能加载 models/manifest.json");
        let e = m.find("yolo26n-seg.onnx").expect("manifest 应含 yolo26n-seg");
        assert_eq!(e.imgsz, 640);
        assert!(m.find("yolo26x-seg.onnx").is_some());
    }

    #[test]
    fn sha256_verifies_against_manifest() {
        if !models_present() {
            eprintln!("skip: 无 models/（CI 环境）");
            return;
        }
        let m = load_manifest().unwrap();
        let e = m.find("yolo11n-seg.onnx").unwrap();
        let p = resolve_model("yolo11n-seg.onnx");
        assert_eq!(verify_sha256(&p, &e.sha256), Some(true));
        assert_eq!(verify_sha256(&p, "deadbeef"), Some(false));
        assert_eq!(verify_sha256(Path::new("/nonexistent.onnx"), &e.sha256), None);
    }
}
