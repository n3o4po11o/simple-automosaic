"""流水线并行处理：decode → infer → encode 三段通过队列并行。

架构：
  [decode 线程] ffmpeg 硬解→MJpeg pipe  ─► q_decode ─►
  [infer  线程] 批量 MPS 推理+合成        ─► q_encode ─►
  [encode 线程] ffmpeg 硬编 H.264 写 mp4

- 解码：`ffmpeg -hwaccel videotoolbox -i in -f image2pipe -vcodec mjpeg -`，
         stdout 是连续 MJPEG 字节流，按 JPEG 边界(FFD8..FFD9)切帧，cv2.imdecode。
- 编码：`ffmpeg -y -f image2pipe -vcodec mjpeg -framerate fps -i - [-i src音轨]
                 -c:v h264_videotoolbox -q:v 65 -c:a copy out.mp4`，stdin 逐帧喂 JPEG(q95)。
- 三线程 + 两队列(maxsize=QUEUE)；任一线程异常置 error Event，其余退出，主线程 raise。
"""

from __future__ import annotations

import logging
import queue
import subprocess
import threading
import time
from pathlib import Path

import cv2
import numpy as np

from automosaic.ffmpeg_io import has_audio, probe_framerate
from automosaic.masking import apply_mask, build_mask

log = logging.getLogger("automosaic")

QUEUE_SIZE = 32
JPEG_QUALITY = 95
JPG_PARAMS = [cv2.IMWRITE_JPEG_QUALITY, JPEG_QUALITY]
# 流结束哨兵
SENTINEL = None
# JPEG 起始/结束标记
SOI = b"\xff\xd8"
EOI = b"\xff\xd9"


# --------------------------------------------------------------------------- #
# 解码：读 MJPEG 流，逐帧 yield
# --------------------------------------------------------------------------- #
def _decode_loop(video: Path, hwaccel: bool, out_q: queue.Queue, error: threading.Event) -> None:
    try:
        cmd = ["ffmpeg", "-loglevel", "error", "-nostdin"]
        if hwaccel:
            cmd += ["-hwaccel", "videotoolbox"]
        cmd += [
            "-i", str(video),
            "-f", "image2pipe",
            "-vcodec", "mjpeg",
            "-vsync", "passthrough",
            "-",  # 输出到 stdout
        ]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        buf = bytearray()
        idx = 0
        while not error.is_set():
            chunk = proc.stdout.read(65536)
            if not chunk:
                break
            buf.extend(chunk)
            # 按 JPEG 边界切出所有完整帧
            while True:
                s = buf.find(SOI)
                if s < 0:
                    break
                e = buf.find(EOI, s + 2)
                if e < 0:
                    # 不完整，保留从 SOI 开始的内容
                    del buf[:s]
                    break
                jpg = bytes(buf[s:e + 2])
                del buf[:e + 2]
                arr = np.frombuffer(jpg, dtype=np.uint8)
                frame = cv2.imdecode(arr, cv2.IMREAD_COLOR)
                if frame is not None:
                    out_q.put((idx, frame))
                    idx += 1
        proc.stdout.close()
        rc = proc.wait()
        if rc not in (0, None) and not error.is_set():
            err = proc.stderr.read().decode("utf-8", "ignore")[-1000:]
            raise RuntimeError(f"ffmpeg 解码退出码 {rc}: {err}")
        log.info("解码完成: 共 %d 帧", idx)
    except Exception as e:
        log.exception("解码线程异常")
        error.set()
        raise
    finally:
        out_q.put(SENTINEL)


# --------------------------------------------------------------------------- #
# 推理：从 q_decode 取批，MPS 推理，把整批 (idx_list,frames,body,faces) 放 q_compose
# 合成在单独线程做（GPU 批量 或 CPU 逐帧），与 GPU 推理解耦
# --------------------------------------------------------------------------- #
def _infer_loop(
    in_q: queue.Queue,
    out_q: queue.Queue,
    error: threading.Event,
    body_det,
    face_det,
    *,
    batch: int,
    n_compose: int,
) -> None:
    try:
        buf: list[tuple[int, np.ndarray]] = []
        total = 0
        t0 = time.time()

        def flush():
            nonlocal total
            if not buf:
                return
            idxs = [i for i, _ in buf]
            frames = [f for _, f in buf]
            body_masks = body_det.detect(frames) if body_det else [None] * len(frames)
            face_boxes = face_det.detect(frames) if face_det else [[]] * len(frames)
            out_q.put((idxs, frames, body_masks, face_boxes))
            total += len(buf)

        for item in iter(in_q.get, SENTINEL):
            if error.is_set():
                break
            buf.append(item)
            if len(buf) >= batch:
                flush()
                buf.clear()
                dt = time.time() - t0
                if dt:
                    log.info("推理进度 %d 帧 (%.1f fps)", total, total / dt)
        if not error.is_set() and buf:
            flush()
            buf.clear()
        log.info("推理完成: 共 %d 帧", total)
    except Exception:
        log.exception("推理线程异常")
        error.set()
        raise
    finally:
        # n_compose 个哨兵：每个 compose worker 都需要收到一个才能退出
        for _ in range(n_compose):
            out_q.put(SENTINEL)


# --------------------------------------------------------------------------- #
# 合成 worker：从 q_compose 取整批，做 mask 合成后逐帧 put 到 q_encode
#   compose_device='gpu': masking_gpu.compose_batch（MPS 可分离高斯，批量）
#   compose_device='cpu': masking.apply_mask（CPU 包围盒，逐帧；可用多 worker）
# --------------------------------------------------------------------------- #
def _compose_loop(
    in_q: queue.Queue,
    out_q: queue.Queue,
    error: threading.Event,
    *,
    compose_device: str,
    face_expand: int,
    mask_style: str,
    blur_strength: int,
    feather: int,
) -> None:
    try:
        for item in iter(in_q.get, SENTINEL):
            if error.is_set():
                break
            idxs, frames, body_masks, face_boxes = item
            if compose_device == "gpu":
                from automosaic.masking_gpu import compose_batch
                outs = compose_batch(
                    frames, body_masks, face_boxes,
                    style=mask_style, blur_strength=blur_strength,
                    feather=feather, face_expand=face_expand,
                )
                for idx, out in zip(idxs, outs):
                    out_q.put((idx, out))
            else:  # cpu
                for idx, frame, body, faces in zip(idxs, frames, body_masks, face_boxes):
                    h, w = frame.shape[:2]
                    m = build_mask(h, w, body, faces, face_expand=face_expand)
                    out = apply_mask(
                        frame, m, style=mask_style,
                        blur_strength=blur_strength, feather=feather,
                    )
                    out_q.put((idx, out))
    except Exception:
        log.exception("合成线程异常")
        error.set()
        raise
    finally:
        out_q.put(SENTINEL)


# --------------------------------------------------------------------------- #
# 编码：从 q_encode 取帧，按 idx 顺序写入 ffmpeg stdin
# --------------------------------------------------------------------------- #
def _encode_loop(
    in_q: queue.Queue,
    error: threading.Event,
    proc: subprocess.Popen,
    n_sentinels: int,
) -> None:
    """从 q_encode 取帧，按 idx 顺序写入 ffmpeg stdin。

    n_sentinels: 期望收到的结束哨兵数（= compose worker 数）；收齐即结束。
    """
    try:
        pending: dict[int, np.ndarray] = {}
        next_idx = 0
        written = 0
        seen_sentinels = 0
        while seen_sentinels < n_sentinels:
            item = in_q.get()
            if item is SENTINEL:
                seen_sentinels += 1
                continue
            if error.is_set():
                break
            idx, frame = item
            pending[idx] = frame
            # 按顺序 flush 已就绪的帧（保证输出顺序与输入一致）
            while next_idx in pending:
                ok, jpg = cv2.imencode(".jpg", pending.pop(next_idx), JPG_PARAMS)
                if not ok:
                    raise RuntimeError(f"JPEG 编码失败: 帧 {next_idx}")
                proc.stdin.write(jpg.tobytes())
                next_idx += 1
                written += 1
        proc.stdin.close()
        rc = proc.wait()
        if rc not in (0, None) and not error.is_set():
            err = proc.stderr.read().decode("utf-8", "ignore")[-1000:]
            raise RuntimeError(f"ffmpeg 编码退出码 {rc}: {err}")
        log.info("编码完成: 共 %d 帧", written)
    except Exception as e:
        log.exception("编码线程异常")
        error.set()
        try:
            proc.stdin.close()
        except Exception:
            pass
        proc.kill()
        raise


def _build_encode_cmd(
    output: Path, fps: float, source: Path | None, *, hwaccel: bool,
    codec: str | None, q: int | None, crf: int | None,
) -> list[str]:
    """构建编码 ffmpeg 命令：stdin 喂 MJPEG，输出 mp4。"""
    from automosaic.ffmpeg_io import _has_encoder
    if codec is None:
        codec = "h264_videotoolbox" if hwaccel and _has_encoder("h264_videotoolbox") else "libx264"
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-nostdin",
           "-f", "image2pipe", "-vcodec", "mjpeg",
           "-framerate", f"{fps}", "-i", "-"]
    if source is not None and has_audio(source):
        cmd += ["-i", str(source), "-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy"]
    else:
        cmd += ["-map", "0:v:0"]
    cmd += ["-c:v", codec]
    if codec == "h264_videotoolbox":
        cmd += ["-q:v", str(q if q is not None else 65), "-pix_fmt", "yuv420p"]
    else:
        cmd += ["-crf", str(crf if crf is not None else 18),
                "-pix_fmt", "yuv420p", "-preset", "medium"]
    cmd += ["-movflags", "+faststart", str(output)]
    return cmd


def run_pipeline(
    input_video: Path,
    output_video: Path,
    *,
    device: str,
    batch: int,
    conf: float,
    face_expand: int,
    mask_style: str,
    blur_strength: int,
    feather: int,
    use_body: bool,
    use_face: bool,
    body_det,
    face_det,
    hwaccel: bool = True,
    codec: str | None = None,
    q: int | None = None,
    crf: int | None = None,
    workers: int = 4,
    compose_device: str = "gpu",
) -> None:
    """端到端流水线：decode → infer(GPU单线程) → compose → encode 并行。

    compose_device='gpu': 1 个 compose worker，MPS 批量可分离高斯（默认，最快）。
    compose_device='cpu': workers 个 compose worker，CPU 包围盒逐帧（兜底）。
    """
    output_video.parent.mkdir(parents=True, exist_ok=True)
    fps = probe_framerate(input_video)
    if compose_device == "gpu":
        n_compose = 1
        log.info("视频帧率: %.3f fps（流水线, compose=gpu 批量）", fps)
    else:
        n_compose = workers
        log.info("视频帧率: %.3f fps（流水线, compose=cpu %d workers）", fps, n_compose)

    cmd = _build_encode_cmd(
        output_video, fps, input_video,
        hwaccel=hwaccel, codec=codec, q=q, crf=crf,
    )
    enc_proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    q_decode = queue.Queue(maxsize=QUEUE_SIZE)
    q_compose = queue.Queue(maxsize=QUEUE_SIZE)
    q_encode = queue.Queue(maxsize=QUEUE_SIZE)
    error = threading.Event()

    threads = {
        "decode": threading.Thread(
            target=_decode_loop, args=(input_video, hwaccel, q_decode, error),
            name="decode"),
        "infer": threading.Thread(
            target=_infer_loop,
            args=(q_decode, q_compose, error, body_det, face_det),
            kwargs=dict(batch=batch, n_compose=n_compose),
            name="infer"),
        "encode": threading.Thread(
            target=_encode_loop, args=(q_encode, error, enc_proc, n_compose),
            name="encode"),
    }
    compose_threads = []
    for i in range(n_compose):
        t = threading.Thread(
            target=_compose_loop,
            args=(q_compose, q_encode, error),
            kwargs=dict(compose_device=compose_device, face_expand=face_expand,
                        mask_style=mask_style, blur_strength=blur_strength,
                        feather=feather),
            name=f"compose-{i}")
        compose_threads.append(t)
    threads_list = list(threads.values()) + compose_threads
    t0 = time.time()
    for t in threads_list:
        t.start()
    try:
        for t in threads_list:
            t.join()
    finally:
        if error.is_set():
            enc_proc.kill()
    if error.is_set():
        raise RuntimeError("流水线因错误终止（见上方日志）")
    log.info("全部完成，耗时 %.1fs", time.time() - t0)
