#!/usr/bin/env bash
# 下载内置用的 ffmpeg/ffprobe 预编译二进制到 bin/<platform>/。
# 探测已改为 ffmpeg -i 解析（media.rs），ffprobe 不再需要（app 展示性元数据走 media_kit）。
# 每个平台的构建都必须带该平台硬件加速（DESIGN §8）：
#   macOS   : VideoToolbox（原生构建默认启用）
#   Windows : NVENC/NVDEC + QSV(libvpl) + AMF + D3D11VA（BtbN GPL）
#   Linux   : VAAPI + NVDEC(cuda) + QSV（BtbN GPL 静态，glibc>=2.28）
# 统一选 GPL 变体：包含 libx264 软编兜底（LGPL 变体没有任何 H.264 软件编码器，
# 硬编不可用的机器会无路可退）。GPL 二进制以独立子进程分发，随包附源码链接即可。
# 运行时解析顺序见 automosaic-core/src/media.rs::tool_path。
#
# 可用环境变量：
#   FFMPEG_TAG  BtbN 版本标签（默认 master-latest；锁版本用 n9.0-latest 等）
set -euo pipefail
cd "$(dirname "$0")/.."

OS="$(uname -s)"
ARCH="$(uname -m)"
TAG="${FFMPEG_TAG:-master-latest}"

fetch() { # fetch <url> <out>
  echo "下载 $1"
  curl -fL --retry 3 -o "$2" "$1"
}

case "$OS:$ARCH" in
  Darwin:arm64)
    PLATFORM=bin/darwin-arm64
    mkdir -p "$PLATFORM" /tmp/automosaic-ffmpeg
    fetch https://www.osxexperts.net/ffmpeg9arm.zip /tmp/automosaic-ffmpeg/ffmpeg.zip
    unzip -oq /tmp/automosaic-ffmpeg/ffmpeg.zip -d "$PLATFORM"
    # 个人站构建带 quarantine，dev 使用需去除；打进 .app 随 App 签名则无此问题
    xattr -dr com.apple.quarantine "$PLATFORM" 2>/dev/null || true
    ;;
  Darwin:x86_64)
    PLATFORM=bin/darwin-x86_64
    mkdir -p "$PLATFORM"
    fetch https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip "$PLATFORM/ffmpeg.zip"
    unzip -oq "$PLATFORM/ffmpeg.zip" -d "$PLATFORM" && rm "$PLATFORM/ffmpeg.zip"
    xattr -dr com.apple.quarantine "$PLATFORM" 2>/dev/null || true
    ;;
  Linux:x86_64 | Linux:aarch64)
    [ "$ARCH" = x86_64 ] && SUFFIX=linux64 || SUFFIX=linuxarm64
    PLATFORM=bin/linux-$ARCH
    mkdir -p "$PLATFORM" /tmp/automosaic-ffmpeg
    fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-$TAG-$SUFFIX-gpl.tar.xz" /tmp/automosaic-ffmpeg/ffmpeg.tar.xz
    tar -xJf /tmp/automosaic-ffmpeg/ffmpeg.tar.xz -C /tmp/automosaic-ffmpeg
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/bin/ffmpeg "$PLATFORM/"
    ;;
  MINGW*:* | MSYS*:* | CYGWIN*:*)
    PLATFORM=bin/windows-x86_64
    mkdir -p "$PLATFORM" /tmp/automosaic-ffmpeg
    fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-$TAG-win64-gpl.zip" /tmp/automosaic-ffmpeg/ffmpeg.zip
    unzip -oq /tmp/automosaic-ffmpeg/ffmpeg.zip -d /tmp/automosaic-ffmpeg
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/bin/ffmpeg.exe "$PLATFORM/"
    ;;
  *)
    echo "不支持的平台: $OS $ARCH（请手动放置 ffmpeg/ffprobe 到 bin/ 下并调整 tool_path）" >&2
    exit 1
    ;;
esac

chmod +x "$PLATFORM"/ffmpeg* 2>/dev/null || true
echo "== $PLATFORM =="
"$PLATFORM/ffmpeg" -version | head -1
echo "-- hwaccels --"
"$PLATFORM/ffmpeg" -hide_banner -hwaccels 2>/dev/null | tail -n +2 | tr '\n' ' '
echo
echo "-- libx264（软编兜底必须存在） --"
"$PLATFORM/ffmpeg" -hide_banner -encoders 2>/dev/null | grep -c libx264
rm -rf /tmp/automosaic-ffmpeg
echo "完成。注意：bin/ 不入 git（.gitignore），打包时随应用分发。"
