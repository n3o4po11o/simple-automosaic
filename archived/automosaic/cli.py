"""命令行入口：解析参数并编排 pipeline。"""

from __future__ import annotations

import argparse
import logging
import sys
from pathlib import Path

from automosaic import __version__
from automosaic.detectors import BODY_MODEL_DEFAULT, FACE_MODEL_DEFAULT
from automosaic.ffmpeg_io import check_ffmpeg
from automosaic.pipeline import run


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="automosaic",
        description="M1 Max GPU(MPS) 加速的视频人脸/人体纯黑遮罩工具。",
    )
    p.add_argument("-i", "--input", required=True, type=Path, help="输入视频")
    p.add_argument("-o", "--output", required=True, type=Path, help="输出视频")
    p.add_argument("--device", default="mps", choices=["mps", "cpu", "cuda"],
                   help="推理设备（M1 Max 用 mps）")
    p.add_argument("--batch", type=int, default=16, help="推理批大小")
    p.add_argument("--conf", type=float, default=0.35, help="置信度阈值")
    p.add_argument("--face-expand", type=int, default=12,
                   help="人脸框四周外扩像素")
    p.add_argument("--feather", type=int, default=0,
                   help="遮罩边缘羽化像素（0=硬边）")
    p.add_argument("--mode", default="streaming", choices=["streaming", "batch"],
                   help="处理模式：streaming=三段流水线并行(默认,边解边推边编)；"
                        "batch=全量解帧后串行处理(兜底)")
    p.add_argument("--mask-style", default="blur", choices=["blur", "solid"],
                   help="遮罩样式：blur=轻微模糊(默认)；solid=纯色黑遮罩")
    p.add_argument("--blur-strength", type=int, default=35,
                   help="模糊核大小（仅 blur 样式，默认 35，越大越模糊）")
    p.add_argument("--workers", type=int, default=4,
                   help="CPU 端 mask 合成并行线程数（仅 --compose-device=cpu，默认 4）")
    p.add_argument("--compose-device", default="gpu", choices=["gpu", "cpu"],
                   help="mask 合成设备：gpu=MPS 批量可分离高斯(默认,最快)；"
                        "cpu=多线程包围盒(兜底)")
    p.add_argument("--body/--no-body", dest="use_body", default=True,
                   help="是否检测人体（默认开）")
    p.add_argument("--face/--no-face", dest="use_face", default=True,
                   help="是否检测人脸（默认开）")
    p.add_argument("--body-model", default=BODY_MODEL_DEFAULT,
                   help="人体分割权重")
    p.add_argument("--face-model", default=FACE_MODEL_DEFAULT,
                   help="人脸检测权重")
    p.add_argument("--keep-frames", action="store_true",
                   help="保留中间帧目录（默认清理）")
    p.add_argument("--frames-dir", type=Path, default=None,
                   help="中间帧目录（默认输入旁 _automosaic_frames）")
    p.add_argument("--hwaccel/--no-hwaccel", dest="hwaccel", default=True,
                   help="macOS VideoToolbox 硬解/硬编（默认开启，mac 推荐）")
    p.add_argument("--codec", default=None,
                   help="输出视频编码器（默认 h264_videotoolbox 或 libx264）")
    p.add_argument("--crf", type=int, default=None,
                   help="libx264 质量（0-51，默认 18）")
    p.add_argument("--q", type=int, default=None,
                   help="VideoToolbox 质量（0-100，默认 65）")
    p.add_argument("-v", "--verbose", action="store_true", help="详细日志")
    p.add_argument("-V", "--version", action="version", version=f"%(prog)s {__version__}")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )

    check_ffmpeg()

    run(
        input_video=args.input,
        output_video=args.output,
        device=args.device,
        batch=args.batch,
        conf=args.conf,
        face_expand=args.face_expand,
        feather=args.feather,
        use_body=args.use_body,
        use_face=args.use_face,
        body_model=args.body_model,
        face_model=args.face_model,
        keep_frames=args.keep_frames,
        frames_root=args.frames_dir,
        hwaccel=args.hwaccel,
        codec=args.codec,
        crf=args.crf,
        q=args.q,
        mode=args.mode,
        mask_style=args.mask_style,
        blur_strength=args.blur_strength,
        workers=args.workers,
        compose_device=args.compose_device,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
