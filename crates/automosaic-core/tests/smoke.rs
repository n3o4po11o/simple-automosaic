//! 解码侧冒烟探测（DESIGN §3.3 步骤 3）：候选 hwaccel 对真实流做 1s 试解码，
//! 硬失败（设备/驱动/构建缺失）在启动期剔除。依赖 tests/clip5s.mp4 与 ffmpeg。

use std::path::PathBuf;

use automosaic_core::media;

fn test_clip() -> Option<PathBuf> {
    let clip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/clip5s.mp4");
    clip.exists().then_some(clip)
}

fn ffmpeg_available() -> bool {
    if media::tool_path("ffmpeg").is_file() {
        return true;
    }
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join("ffmpeg").is_file())
}

#[test]
fn bogus_hwaccel_is_rejected_and_cached() {
    let (Some(clip), true) = (test_clip(), ffmpeg_available()) else {
        eprintln!("skip: 无测试片或 ffmpeg");
        return;
    };
    let meta = media::probe(&clip).expect("测试片可探测");
    // 不存在的 hwaccel：ffmpeg 立即硬失败
    assert!(
        !media::hwaccel_usable(&clip, "definitely_not_an_hwaccel", &meta),
        "伪造的 hwaccel 应被冒烟剔除"
    );
    // 二次调用走缓存，结果一致
    assert!(!media::hwaccel_usable(&clip, "definitely_not_an_hwaccel", &meta));
}

#[test]
fn platform_first_candidate_usable_on_mac() {
    // macOS 首选 videotoolbox 对 H.264/HEVC 真实流可用（硬编机器必有）。
    // 其他平台候选可用性依容器/驱动而异，不做强断言（Linux 容器内无 GPU 时
    // vaapi 冒烟失败→自动软解，正是该机制的设计行为）。
    #[cfg(target_os = "macos")]
    {
        let (Some(clip), true) = (test_clip(), ffmpeg_available()) else {
            eprintln!("skip: 无测试片或 ffmpeg");
            return;
        };
        let meta = media::probe(&clip).expect("测试片可探测");
        assert!(
            media::hwaccel_usable(&clip, "videotoolbox", &meta),
            "macOS 上 videotoolbox 应通过冒烟"
        );
    }
}
