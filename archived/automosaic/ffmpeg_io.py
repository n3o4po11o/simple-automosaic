"""ffmpeg 解帧与重编码工具。

设计要点：
- 解帧：`ffmpeg -i in -vsync passthrough tmp/%06d.png`，保留原始帧率与时序。
- 重编码：以原始帧率导入处理后的 PNG，从原视频拷贝音轨，编码为 H.264 yuv420p（最大兼容性）。
- 用 ffprobe 探测帧率（r_frame_rate）。
所有 ffmpeg/ffprobe 调用走 subprocess，失败时抛 RuntimeError。
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

PNG_GLOB = "%06d.png"
FIRST_FRAME = "%06d.png" % 1  # "000001.png"


def _run(cmd: list[str]) -> str:
    """运行命令并返回 stdout；失败抛 RuntimeError（含 stderr）。"""
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"命令失败 ({proc.returncode}): {' '.join(cmd)}\nstderr:\n{proc.stderr[-2000:]}"
        )
    return proc.stdout


def check_ffmpeg() -> None:
    """确保 ffmpeg/ffprobe 可用。"""
    if not shutil.which("ffmpeg"):
        raise RuntimeError("未找到 ffmpeg，请先安装：brew install ffmpeg")
    if not shutil.which("ffprobe"):
        raise RuntimeError("未找到 ffprobe，应随 ffmpeg 一同安装")


def probe_framerate(video: Path) -> float:
    """探测视频帧率（r_frame_rate，如 "30/1" → 30.0）。"""
    out = _run([
        "ffprobe", "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=r_frame_rate",
        "-of", "default=nokey=1:noprint_wrappers=1",
        str(video),
    ]).strip()
    m = re.match(r"(\d+)(?:/(\d+))?", out)
    if not m:
        raise RuntimeError(f"无法解析帧率: {out!r}")
    num = int(m.group(1))
    den = int(m.group(2)) if m.group(2) else 1
    if den == 0:
        raise RuntimeError(f"帧率分母为 0: {out!r}")
    return num / den


def has_audio(video: Path) -> bool:
    """视频是否含音轨。"""
    out = _run([
        "ffprobe", "-v", "error",
        "-select_streams", "a",
        "-show_entries", "stream=codec_type",
        "-of", "default=nokey=1:noprint_wrappers=1",
        str(video),
    ]).strip()
    return out == "audio"


def extract_frames(video: Path, frames_dir: Path, hwaccel: bool = True) -> int:
    """将视频解帧为 PNG，返回帧数。

    - hwaccel=True: 用 macOS VideoToolbox 硬解（HEVC/H.264 提速明显）。
    """
    frames_dir.mkdir(parents=True, exist_ok=True)
    cmd = ["ffmpeg", "-y"]
    if hwaccel:
        # 优先 VideoToolbox 硬件解码；失败时 ffmpeg 会自动回退到软解
        cmd += ["-hwaccel", "videotoolbox"]
    cmd += [
        "-i", str(video),
        "-vsync", "passthrough",  # 保留每帧时间戳，避免重复/丢帧
        str(frames_dir / PNG_GLOB),
    ]
    _run(cmd)
    count = len(list(frames_dir.glob("*.png")))
    if count == 0:
        raise RuntimeError(f"解帧失败：{frames_dir} 下无 PNG")
    return count


def encode_video(
    frames_dir: Path,
    output: Path,
    framerate: float,
    source: Path | None = None,
    *,
    hwaccel: bool = True,
    codec: str | None = None,
    crf: int | None = None,
    q: int | None = None,
) -> None:
    """将处理后的 PNG 帧重编码为视频。

    - 以 framerate 导入 PNG 序列。
    - 若 source 含音轨，则从源拷贝音轨（-c:a copy）。
    - hwaccel=True: 用 macOS VideoToolbox 硬编（h264_videotoolbox），显著提速；
      失败/不可用时回退 libx264。
    - codec/crf/q: 显式覆盖编码器与质量参数（见下方默认逻辑）。
    """
    output.parent.mkdir(parents=True, exist_ok=True)

    # 决定编码器：默认 VideoToolbox 硬编 H.264，否则 libx264
    if codec is None:
        codec = "h264_videotoolbox" if hwaccel and _has_encoder("h264_videotoolbox") else "libx264"

    cmd = [
        "ffmpeg", "-y",
        "-framerate", f"{framerate}",
        "-i", str(frames_dir / PNG_GLOB),
    ]
    if source is not None and has_audio(source):
        # 把原视频作为第二个输入，仅取其音轨
        cmd += ["-i", str(source), "-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy"]
    else:
        cmd += ["-map", "0:v:0"]

    cmd += ["-c:v", codec]
    if codec == "h264_videotoolbox":
        # VideoToolbox 用 -q:v（质量，0-100，约 50-70 为佳）而非 crf
        cmd += ["-q:v", str(q if q is not None else 65), "-pix_fmt", "yuv420p"]
    else:  # libx264
        cmd += ["-crf", str(crf if crf is not None else 18), "-pix_fmt", "yuv420p",
                "-preset", "medium"]
    cmd += ["-movflags", "+faststart", str(output)]
    _run(cmd)


def _has_encoder(name: str) -> bool:
    """查询 ffmpeg 是否支持某编码器。"""
    try:
        out = subprocess.run(
            ["ffmpeg", "-hide_banner", "-encoders"],
            capture_output=True, text=True, check=True,
        ).stdout
        return any(line.strip().startswith("V") and name in line for line in out.splitlines())
    except Exception:
        return False
