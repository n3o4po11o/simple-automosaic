//! 取消优雅收尾（DESIGN §7.4 / §0.5-G 审计项）：取消的半成品 mp4 必须含
//! moov 可播——旧版直接 kill 双侧子进程，半成品不可播。
//!
//! 依赖仓库根 tests/clip5s.mp4 与可用的 ffmpeg（内置 bin/ 或 PATH），
//! 缺失时跳过（CI 检出无二进制的场景）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use automosaic_core::media;
use automosaic_core::pipe::{self, PipelineError, PipelineOptions};

fn test_clip() -> Option<PathBuf> {
    let clip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/clip5s.mp4");
    clip.exists().then_some(clip)
}

fn ffmpeg_available() -> bool {
    let p = media::tool_path("ffmpeg");
    p.is_file() || which_ffmpeg()
}

/// PATH 上是否有 ffmpeg（tool_path 找不到文件时回退 PATH 名）。
fn which_ffmpeg() -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join("ffmpeg").is_file())
}

#[test]
fn cancelled_output_still_playable() {
    let (Some(clip), true) = (test_clip(), ffmpeg_available()) else {
        eprintln!("skip: 无测试片或 ffmpeg");
        return;
    };
    let out = std::env::temp_dir().join("automosaic_cancel_test.mp4");
    let _ = std::fs::remove_file(&out);

    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    // transform 注入延迟拉长管线，确保取消发生在中途而非收尾竞态
    let slow: pipe::FrameTransform = Box::new(|frames: &mut [&mut [u8]]| {
        std::thread::sleep(Duration::from_millis(30 * frames.len() as u64));
        Ok(())
    });
    let result = pipe::run(
        &clip,
        &out,
        PipelineOptions {
            hwaccel: None,
            encoder: "libx264".into(),
            bitrate: "auto".into(),
            transform: Some(slow),
            batch_size: 1,
            cancel: Some(cancel),
            frame_format: Default::default(),
        },
        |p| {
            if p.frames > 0 {
                flag.store(true, Ordering::Relaxed); // 首个进度点后取消
            }
        },
    );
    assert!(
        matches!(&result, Err(PipelineError::Cancelled { frames }) if *frames > 0),
        "应在处理中途取消，得 {}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
    // 核心断言：编码器 EOF 收尾写了 moov → 半成品可被探测解析
    let meta = media::probe(&out).expect("取消的半成品应可播（moov 完整）");
    assert!(meta.width > 0 && meta.height > 0);
}
