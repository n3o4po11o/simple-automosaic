"""处理管线：streaming（默认，三段并行）与 batch（全量落盘，兜底）两种模式。"""

from __future__ import annotations

import logging
import time
from pathlib import Path

import cv2
import numpy as np

from automosaic.detectors import BodyDetector, FaceDetector
from automosaic.ffmpeg_io import (
    encode_video,
    extract_frames,
    probe_framerate,
)
from automosaic.masking import apply_mask, build_mask

log = logging.getLogger("automosaic")


def _load_batch(paths: list[Path]) -> list[np.ndarray]:
    """按文件名顺序读取一批 PNG 为 BGR ndarray。"""
    frames = []
    for p in paths:
        img = cv2.imread(str(p), cv2.IMREAD_COLOR)
        if img is None:
            raise RuntimeError(f"无法读取帧: {p}")
        frames.append(img)
    return frames


def run(
    input_video: Path,
    output_video: Path,
    *,
    device: str = "mps",
    batch: int = 16,
    conf: float = 0.35,
    face_expand: int = 12,
    feather: int = 0,
    use_body: bool = True,
    use_face: bool = True,
    body_model: str | None = None,
    face_model: str | None = None,
    keep_frames: bool = False,
    frames_root: Path | None = None,
    hwaccel: bool = True,
    codec: str | None = None,
    crf: int | None = None,
    q: int | None = None,
    mode: str = "streaming",
    mask_style: str = "blur",
    blur_strength: int = 35,
    workers: int = 4,
    compose_device: str = "gpu",
) -> None:
    """端到端处理。按 mode 派发：
    - 'streaming' (默认): decode→infer→encode 三段流水线并行（边解边推边编），无中间文件。
    - 'batch': 全量解帧落盘 → 推理 → 重编码（兼容旧路径）。
    """
    if not input_video.exists():
        raise FileNotFoundError(f"输入视频不存在: {input_video}")

    # 构造检测器（两种模式共用）
    body_det = BodyDetector(weights=body_model, device=device, conf=conf) if use_body else None
    face_det = FaceDetector(
        weights=face_model, device=device, conf=max(0.1, conf - 0.1)
    ) if use_face else None

    if mode == "streaming":
        from automosaic.streaming import run_pipeline
        run_pipeline(
            input_video, output_video,
            device=device, batch=batch, conf=conf, face_expand=face_expand,
            mask_style=mask_style, blur_strength=blur_strength, feather=feather,
            use_body=use_body, use_face=use_face,
            body_det=body_det, face_det=face_det,
            hwaccel=hwaccel, codec=codec, q=q, crf=crf,
            workers=workers, compose_device=compose_device,
        )
    else:
        run_batch(
            input_video, output_video,
            body_det=body_det, face_det=face_det,
            device=device, batch=batch, conf=conf, face_expand=face_expand,
            feather=feather, mask_style=mask_style, blur_strength=blur_strength,
            keep_frames=keep_frames, frames_root=frames_root,
            hwaccel=hwaccel, codec=codec, crf=crf, q=q,
        )


def run_batch(
    input_video: Path,
    output_video: Path,
    *,
    body_det,
    face_det,
    device: str = "mps",
    batch: int = 16,
    conf: float = 0.35,
    face_expand: int = 12,
    feather: int = 0,
    mask_style: str = "blur",
    blur_strength: int = 35,
    keep_frames: bool = False,
    frames_root: Path | None = None,
    hwaccel: bool = True,
    codec: str | None = None,
    crf: int | None = None,
    q: int | None = None,
) -> None:
    """batch 模式：全量解帧 → 批推理 → 重编码（兼容旧路径，兜底用）。"""
    if not input_video.exists():
        raise FileNotFoundError(f"输入视频不存在: {input_video}")

    fps = probe_framerate(input_video)
    log.info("视频帧率: %.3f fps", fps)

    work = frames_root or input_video.parent / "_automosaic_frames"
    work.mkdir(parents=True, exist_ok=True)
    frames_dir = work  # 原地处理：解帧后直接覆盖写回

    n = extract_frames(input_video, frames_dir, hwaccel=hwaccel)
    log.info("解帧完成: %d 帧 → %s (hwaccel=%s)", n, frames_dir, hwaccel)

    paths = sorted(frames_dir.glob("*.png"))
    total = len(paths)
    t0 = time.time()
    processed = 0

    for i in range(0, total, batch):
        chunk = paths[i:i + batch]
        frames = _load_batch(chunk)

        body_masks = body_det.detect(frames) if body_det else [None] * len(frames)
        face_boxes = face_det.detect(frames) if face_det else [[]] * len(frames)

        for path, frame, body, faces in zip(chunk, frames, body_masks, face_boxes):
            h, w = frame.shape[:2]
            mask = build_mask(h, w, body, faces, face_expand=face_expand)
            out = apply_mask(frame, mask, style=mask_style,
                             blur_strength=blur_strength, feather=feather)
            cv2.imwrite(str(path), out)
            processed += 1

        dt = time.time() - t0
        rate = processed / dt if dt else 0
        log.info("进度 %d/%d (%.1f fps 处理)", processed, total, rate)

    log.info("推理与合成完成，开始编码...")
    encode_video(
        frames_dir, output_video, fps, source=input_video,
        hwaccel=hwaccel, codec=codec, crf=crf, q=q,
    )
    log.info("输出: %s", output_video)

    if not keep_frames:
        import shutil
        shutil.rmtree(frames_dir, ignore_errors=True)
        log.info("已清理中间帧目录")
    else:
        log.info("保留中间帧: %s", frames_dir)

    log.info("全部完成，耗时 %.1fs", time.time() - t0)
