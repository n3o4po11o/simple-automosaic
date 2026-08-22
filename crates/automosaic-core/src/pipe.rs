//! NV12 rawvideo 管线（M1）：解码 → [transform(推理+合成)] → 编码。
//!
//! 拓扑（继承 archived/ streaming.py 的已验证结构，DESIGN §2.1）：
//! ```text
//! [decode ffmpeg] stdout(NV12) → ch(32) → [transform 线程] → ch(32) → [encode ffmpeg] stdin
//!        │ stderr → drain 线程                                        │ stderr → drain 线程
//! ```
//! transform 为 None 时即直通（transcode 冒烟路径）。

use crate::media::{self, EncodeOptions, VideoMeta};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 有界通道容量（对应 Python 版 QUEUE_SIZE=32）。
const QUEUE_SIZE: usize = 32;
/// 进度回报间隔。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// 写帧线程轮询取消标志的周期（recv_timeout 空闲时的唤醒粒度）。
const WRITER_POLL: Duration = Duration::from_millis(100);
/// 取消后等编码器 EOF 收尾（写 moov）的上限，超时才强杀（DESIGN §7.4）。
const ENCODE_FINALIZE_TIMEOUT: Duration = Duration::from_secs(3);

/// 帧变换（原地修改一批 NV12 帧；批大小见 `PipelineOptions::batch_size`，
/// 流末尾最后一批可能不足额）。检测/合成的错误用 Err 中止管线。
pub type FrameTransform = Box<dyn FnMut(&mut [&mut [u8]]) -> Result<(), String> + Send>;

/// 取消标志：置 true 后管线在帧边界终止，双侧 ffmpeg 被 kill。
pub type CancelFlag = Arc<AtomicBool>;

pub struct PipelineOptions {
    /// None = 软解。
    pub hwaccel: Option<String>,
    pub encoder: String,
    /// 码率型编码器（videotoolbox/amf）的目标码率。
    pub bitrate: String,
    /// 每帧变换（推理+合成）；None = 直通。
    pub transform: Option<FrameTransform>,
    /// 批大小：transform 每次收到的帧数上限（最后一批可能不足）。
    pub batch_size: usize,
    /// 取消标志（调用方持有并可在任意时刻置位）。
    pub cancel: Option<CancelFlag>,
    /// 解码管道帧格式（DESIGN §3.2；MJPEG 为低配可选路径）。
    pub frame_format: media::FrameFormat,
}

#[derive(Debug, Clone)]
pub struct Progress {
    /// transform 完成帧数（处理进度口径）。
    pub frames: u64,
    /// 解码读出帧数（提取进度口径）。
    pub decoded: u64,
    /// 已写入编码器的帧数。
    pub written: u64,
    pub total_frames: Option<u64>,
    pub fps: f64,
    pub eta_secs: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("探测失败: {0}")]
    Probe(#[from] media::ProbeError),
    #[error("解码 ffmpeg 启动失败: {0}")]
    SpawnDecoder(std::io::Error),
    #[error("编码 ffmpeg 启动失败: {0}")]
    SpawnEncoder(std::io::Error),
    #[error("解码中断（已读 {frames} 帧）: {stderr}")]
    DecoderFailed { frames: u64, stderr: String },
    #[error("编码中断（已写 {frames} 帧）: {stderr}")]
    EncoderFailed { frames: u64, stderr: String },
    #[error("处理失败（帧 {frames}）: {reason}")]
    TransformFailed { frames: u64, reason: String },
    #[error("已取消（完成 {frames} 帧）")]
    Cancelled { frames: u64 },
    #[error("帧数不一致：解码 {decoded} 帧，编码 {encoded} 帧")]
    FrameCountMismatch { decoded: u64, encoded: u64 },
}

/// 排空子进程 stderr，防止其因管道写满而阻塞；JoinHandle 返回完整内容。
fn drain_stderr(child: &mut Child) -> std::thread::JoinHandle<String> {
    let mut stderr = child
        .stderr
        .take()
        .expect("stderr 必须 piped");
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    })
}

fn stderr_tail(s: &str) -> String {
    s.chars().rev().take(1000).collect::<String>().chars().rev().collect()
}

/// 运行结果：元数据 + 实际处理帧数。
///
/// 注意：`meta.total_frames` 来自容器元数据（nb_frames 或 duration×fps 估算），
/// 个别 VFR 重封装文件的 nb_frames 会虚报，实际帧数以 `frames` 为准。
pub struct PipelineStats {
    pub meta: VideoMeta,
    pub frames: u64,
}

/// 直通转码（无 transform）。
pub fn passthrough(
    input: &Path,
    output: &Path,
    hwaccel: Option<String>,
    encoder: String,
    bitrate: String,
    on_progress: impl FnMut(Progress),
) -> Result<PipelineStats, PipelineError> {
    run(
        input,
        output,
        PipelineOptions {
            hwaccel,
            encoder,
            bitrate,
            transform: None,
            batch_size: 1,
            cancel: None,
            frame_format: media::FrameFormat::Nv12,
        },
        on_progress,
    )
}

/// 完整管线：硬解 → NV12 管道 → [transform] → 硬编。
/// 音轨编码与 mp4/mov 容器不兼容（TrueHD 等 `-c:a copy` 会被 muxer 拒绝）时
/// 预先转为 AAC——按 probe 到的音轨编码确定性判断，无需运行期重试。
pub fn run(
    input: &Path,
    output: &Path,
    opts: PipelineOptions,
    mut on_progress: impl FnMut(Progress),
) -> Result<PipelineStats, PipelineError> {
    let meta = media::probe(input)?;
    let is_mp4 = output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("m4v") || e.eq_ignore_ascii_case("mov"));
    let audio_transcode = is_mp4
        && meta.audio_codecs.iter().any(|c| {
            ["truehd", "mlp", "dts"].iter().any(|bad| c.eq_ignore_ascii_case(bad))
        });
    // "auto" = 码率随分辨率档位缩放（1080p=6M 基准）；显式值原样透传
    let bitrate = if opts.bitrate.eq_ignore_ascii_case("auto") {
        media::auto_bitrate(meta.width, meta.height)
    } else {
        opts.bitrate.clone()
    };
    let enc_opts = EncodeOptions {
        width: meta.width,
        height: meta.height,
        fps: meta.fps,
        encoder: opts.encoder.clone(),
        audio_from: meta.has_audio.then(|| input.to_path_buf()),
        bitrate,
        audio_transcode,
    };

    let mut dec = Command::new(&media::tool_path("ffmpeg"))
        .args(match opts.frame_format {
            media::FrameFormat::Nv12 => media::decode_cmd(input, opts.hwaccel.as_deref()),
            media::FrameFormat::Mjpeg => media::decode_cmd_mjpeg(input, opts.hwaccel.as_deref()),
        })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(PipelineError::SpawnDecoder)?;
    // encoder == "null"：分析段空输出（-f null 帧丢弃，不落盘不编码）
    let enc_args = if opts.encoder == "null" {
        media::drain_null_cmd(&enc_opts)
    } else {
        media::encode_cmd(output, &enc_opts)
    };
    let mut enc = Command::new(&media::tool_path("ffmpeg"))
        .args(enc_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(PipelineError::SpawnEncoder)?;

    let dec_err = drain_stderr(&mut dec);
    let enc_err = drain_stderr(&mut enc);

    let frame_bytes = meta.width as usize * meta.height as usize * 3 / 2;
    let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE_SIZE);
    let (tx2, rx2) = sync_channel::<Vec<u8>>(QUEUE_SIZE);
    let frames_read = Arc::new(AtomicU64::new(0));
    let frames_done = Arc::new(AtomicU64::new(0)); // transform 完成（进度口径）
    let transform_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let reader_stop: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // 读帧线程：解码 stdout → 通道；EOF/中断即退出。
    // NV12 = 定长 read_exact；MJPEG = 定界器弹帧 → 解回 NV12（下游同构）。
    let reader = {
        let mut stdout = dec.stdout.take().expect("stdout 必须 piped");
        let frames_read = Arc::clone(&frames_read);
        let reader_stop = Arc::clone(&reader_stop);
        let frame_format = opts.frame_format;
        let (rw, rh) = (meta.width as usize, meta.height as usize);
        std::thread::spawn(move || {
            match frame_format {
                media::FrameFormat::Nv12 => {
                    let mut buf = vec![0u8; frame_bytes];
                    loop {
                        match stdout.read_exact(&mut buf) {
                            Ok(()) => {
                                if tx.send(buf.clone()).is_err() {
                                    // 下游先死（panic/错误）导致通道关闭——decoder 的
                                    // Broken pipe 是症状不是病因
                                    *reader_stop.lock().unwrap() =
                                        Some("下游先退出（transform/编码侧异常），读端被迫关闭".into());
                                    break;
                                }
                                frames_read.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                // EOF=流自然结束；UnexpectedEof=帧数据不完整（帧大小与
                                // 分辨率不匹配或解码器中途退出）
                                *reader_stop.lock().unwrap() =
                                    Some(format!("读帧中断: {e}（帧大小={frame_bytes} 字节，检查分辨率/旋转元数据）"));
                                break;
                            }
                        }
                    }
                }
                media::FrameFormat::Mjpeg => {
                    let mut scanner = media::JpegFrameScanner::new();
                    let mut jpegs: Vec<Vec<u8>> = Vec::new();
                    let mut chunk = vec![0u8; 1 << 16];
                    'mjpeg: loop {
                        match stdout.read(&mut chunk) {
                            Ok(0) => break, // 流自然结束
                            Ok(n) => {
                                scanner.push(&chunk[..n], &mut jpegs);
                                for j in jpegs.drain(..) {
                                    let frame = match media::jpeg_to_nv12(&j, rw, rh) {
                                        Ok(f) => f,
                                        Err(e) => {
                                            *reader_stop.lock().unwrap() = Some(e);
                                            break 'mjpeg;
                                        }
                                    };
                                    if tx.send(frame).is_err() {
                                        *reader_stop.lock().unwrap() =
                                            Some("下游先退出（transform/编码侧异常），读端被迫关闭".into());
                                        break 'mjpeg;
                                    }
                                    frames_read.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                *reader_stop.lock().unwrap() =
                                    Some(format!("读帧中断: {e}（MJPEG 管道）"));
                                break;
                            }
                        }
                    }
                }
            }
            drop(tx);
        })
    };

    // transform 线程：ch → 攒批 → 变换 → ch2。
    // panic 捕获：transform 内的 panic 若不拦截会静默杀死本线程，
    // 表现为"解码器 Broken pipe"（读端提前关管道）——必须归因为可诊断错误。
    let processor = {
        let mut transform = opts.transform;
        let batch_size = opts.batch_size.max(1);
        let frames_done = Arc::clone(&frames_done);
        let transform_error = Arc::clone(&transform_error);
        std::thread::spawn(move || {
            let mut buf: Vec<Vec<u8>> = Vec::with_capacity(batch_size);
            let run_batch = |buf: &mut Vec<Vec<u8>>, transform: &mut Option<FrameTransform>| {
                if buf.is_empty() {
                    return true;
                }
                if let Some(tf) = transform.as_mut() {
                    let mut refs: Vec<&mut [u8]> =
                        buf.iter_mut().map(|v| v.as_mut_slice()).collect();
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tf(&mut refs)));
                    match r {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            *transform_error.lock().unwrap() = Some(e);
                            return false;
                        }
                        Err(panic) => {
                            let msg: String = panic
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                                .unwrap_or_else(|| "未知 panic".into());
                            let bt = std::backtrace::Backtrace::force_capture().to_string();
                            *transform_error.lock().unwrap() =
                                Some(format!("PANIC: {msg}\n{bt}"));
                            return false;
                        }
                    }
                }
                frames_done.fetch_add(buf.len() as u64, Ordering::Relaxed);
                for frame in buf.drain(..) {
                    if tx2.send(frame).is_err() {
                        return false;
                    }
                }
                true
            };
            while let Ok(frame) = rx.recv() {
                buf.push(frame);
                if buf.len() >= batch_size && !run_batch(&mut buf, &mut transform) {
                    break;
                }
            }
            // 流结束：处理不足额的最后一批（tx2 随 run_batch 析构而关闭）
            if transform_error.lock().unwrap().is_none() {
                run_batch(&mut buf, &mut transform);
            }
        })
    };

    // 写帧线程：ch2 → 编码 stdin；通道关闭后关闭 stdin 让编码器收尾。
    // recv_timeout 轮询取消标志：取消时立即停止喂帧并关 stdin → 编码器读到
    // EOF 正常收尾（写 moov，半成品可播）。rawvideo 管道是数据输入，
    // ffmpeg 不会同时从其中读 'q' 命令——EOF 即等价的优雅停机信号。
    let frames_written = Arc::new(AtomicU64::new(0));
    let writer = {
        let mut stdin = enc.stdin.take().expect("stdin 必须 piped");
        let frames_written = Arc::clone(&frames_written);
        let cancel = opts.cancel.clone();
        std::thread::spawn(move || {
            loop {
                match rx2.recv_timeout(WRITER_POLL) {
                    Ok(frame) => {
                        if stdin.write_all(&frame).is_err() {
                            break; // 编码器先挂了
                        }
                        frames_written.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = stdin.flush();
            drop(stdin); // 关闭 → 编码器读到 EOF，正常收尾写 moov
        })
    };

    // 主线程：进度回报，写侧结束即完成（正常路径 = EOF → 通道逐级关闭 → 写侧收尾）。
    // 取消标志必须先于 writer.is_finished() 检查：写侧察觉取消退出后解码器仍
    // 存活（stdout 管道写满阻塞），若误判为正常完成会在 reader.join() 死等。
    let t0 = Instant::now();
    let mut cancelled = false;
    loop {
        if opts
            .cancel
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            cancelled = true;
            break;
        }
        if writer.is_finished() {
            break;
        }
        let frames = frames_done.load(Ordering::Relaxed);
        let elapsed = t0.elapsed().as_secs_f64();
        on_progress(Progress {
            frames,
            decoded: frames_read.load(Ordering::Relaxed),
            written: frames_written.load(Ordering::Relaxed),
            total_frames: meta.total_frames,
            fps: if elapsed > 0.0 { frames as f64 / elapsed } else { 0.0 },
            eta_secs: meta.total_frames.map(|t| {
                let fps = frames as f64 / elapsed.max(1e-9);
                if fps > 0.0 { (t.saturating_sub(frames)) as f64 / fps } else { 0.0 }
            }),
        });
        std::thread::sleep(PROGRESS_INTERVAL);
    }
    // 终拍：writer 完成即跳出循环，最后一帧的完成不再被播报（曾显示
    // 74/76 后直接跳"完成"——滞后一拍 + 估算 total 多 1 的叠加观感）
    {
        let frames = frames_done.load(Ordering::Relaxed);
        let elapsed = t0.elapsed().as_secs_f64();
        on_progress(Progress {
            frames,
            decoded: frames_read.load(Ordering::Relaxed),
            written: frames_written.load(Ordering::Relaxed),
            total_frames: meta.total_frames,
            fps: if elapsed > 0.0 { frames as f64 / elapsed } else { 0.0 },
            eta_secs: Some(0.0),
        });
    }

    // 取消：优雅收尾（DESIGN §7.4/G 类审计项）。写侧已察觉取消并关闭编码器
    // stdin → 编码器 EOF 后写 moov 正常退出；此处停掉解码器并等它收尾，
    // 3s 超时才强杀（极端场景：长音频 copy 追平耗时）。
    if cancelled {
        let _ = dec.kill();
        let _ = dec.wait();
        let deadline = Instant::now() + ENCODE_FINALIZE_TIMEOUT;
        while enc.try_wait().ok().flatten().is_none() {
            if Instant::now() >= deadline {
                let _ = enc.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = enc.wait();
        let _ = reader.join();
        let _ = processor.join();
        let _ = writer.join();
        let _ = dec_err.join();
        let _ = enc_err.join();
        return Err(PipelineError::Cancelled {
            frames: frames_done.load(Ordering::Relaxed),
        });
    }
    // 归因：transform 失败 → kill 双侧子进程后返回。
    if let Some(e) = transform_error.lock().unwrap().take() {
        let _ = dec.kill();
        let _ = enc.kill();
        let _ = dec.wait();
        let _ = enc.wait();
        return Err(PipelineError::TransformFailed {
            frames: frames_done.load(Ordering::Relaxed),
            reason: e,
        });
    }
    // 编码器失败会先体现为写侧提前退出（此时解码器可能还阻塞在 stdout 上，需 kill）。
    let _ = writer.join();
    let enc_status = enc.wait();
    if !enc_status.map(|s| s.success()).unwrap_or(false) {
        let _ = dec.kill();
        let _ = dec.wait();
        let _ = reader.join();
        let _ = processor.join();
        return Err(PipelineError::EncoderFailed {
            frames: frames_written.load(Ordering::Relaxed),
            stderr: stderr_tail(&enc_err.join().unwrap_or_default()),
        });
    }

    // 正常路径：读侧已随解码 EOF 结束。
    let _ = reader.join();
    let _ = processor.join();
    let dec_ok = dec.wait().map(|s| s.success()).unwrap_or(false);
    if !dec_ok {
        // decoder 的 Broken pipe 常是下游异常的症状；附上读端停止原因与
        // 退出码帮助定位真因（transform panic 已在上方单独归因）。
        let stop = reader_stop.lock().unwrap().clone().unwrap_or_default();
        let code = dec.try_wait().ok().flatten().and_then(|s| s.code()).unwrap_or(-1);
        return Err(PipelineError::DecoderFailed {
            frames: frames_read.load(Ordering::Relaxed),
            stderr: format!(
                "[读端] {stop}\n[退出码] {code}\n[decoder stderr 尾部]\n{}",
                stderr_tail(&dec_err.join().unwrap_or_default())
            ),
        });
    }

    let decoded = frames_read.load(Ordering::Relaxed);
    let encoded = frames_written.load(Ordering::Relaxed);
    if decoded != encoded {
        return Err(PipelineError::FrameCountMismatch { decoded, encoded });
    }

    Ok(PipelineStats { meta, frames: decoded })
}
