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

fetch() { # fetch <url> <out>——5 轮重试（GitHub 大档在 CI 共享出口 IP 上常见中断）
  local i rc
  for i in 1 2 3 4 5; do
    echo "下载 $1（第 $i/5 轮）"
    if curl -fL --retry 3 --retry-delay 10 -o "$2" "$1"; then return 0; fi
    rc=$?
    echo "下载失败 exit=$rc，15s 后重试" >&2
    rm -f "$2"
    sleep 15
  done
  echo "错误：下载最终失败：$1" >&2
  return 1
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
    # shared 变体：动态链接 libva（tar 内带的 libva.so.2 由打包白名单跳过，
    # 运行期用宿主 libva → DRI 驱动路径随宿主正确，VAAPI 跨发行版可用；
    # static 变体静态烘焙 libva 2.7 且驱动路径写死 Debian 布局，非 Debian
    # 系宿主 VAAPI 必败（2026-08-22 真机实证）
    [ "$ARCH" = x86_64 ] && SUFFIX=linux64 || SUFFIX=linuxarm64
    PLATFORM=bin/linux-$ARCH
    mkdir -p "$PLATFORM" /tmp/automosaic-ffmpeg
    fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-$TAG-$SUFFIX-gpl-shared.tar.xz" /tmp/automosaic-ffmpeg/ffmpeg.tar.xz
    # 半截 tar 校验：解压失败明确报错而非幽灵 127
    tar -xJf /tmp/automosaic-ffmpeg/ffmpeg.tar.xz -C /tmp/automosaic-ffmpeg || {
      echo "错误：ffmpeg tar 解压失败（下载不完整？）" >&2; exit 1; }
    # ffmpeg + 全部 so（rpath=$ORIGIN 同目录解析；libva*/libvdpau* 若在内，
    # 由 AppImage 打包白名单跳过→宿主提供；probe 走 ffmpeg -i，无 ffprobe 依赖）
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/bin/ffmpeg "$PLATFORM/"
    # shared 变体的 so 在 tar 的 lib/ 下（rpath=$ORIGIN，需与二进制同目录）
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/lib/*.so.* "$PLATFORM/" 2>/dev/null || true
    ;;
  MINGW*:* | MSYS*:* | CYGWIN*:*)
    # shared 变体：静态单 exe ~145MB 在 NSIS 打包时 mmap 中途崩（10700K
    # 测试机 ICE#12345 实测，CI 环境偶发同类）；shared 的 exe 数百 KB +
    # DLL 平铺同目录（Windows 按 exe 目录加载），portable 亦缩 ~100MB
    PLATFORM=bin/windows-x86_64
    mkdir -p "$PLATFORM" /tmp/automosaic-ffmpeg
    fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-$TAG-win64-gpl-shared.zip" /tmp/automosaic-ffmpeg/ffmpeg.zip
    unzip -oq /tmp/automosaic-ffmpeg/ffmpeg.zip -d /tmp/automosaic-ffmpeg
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/bin/ffmpeg.exe "$PLATFORM/"
    cp /tmp/automosaic-ffmpeg/ffmpeg-*/bin/*.dll "$PLATFORM/"
    # 官方 onnxruntime（OpenVINO EP，版本对齐 ort rc.13 的 api-27（1.27.x））：
    # Windows 推理走 load-dynamic，dll 由打包随应用分发（exe 旁标准搜索
    # 命中）；pyke 预编译无 openvino 变体故取官方包
    fetch "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-x64-1.27.0.zip" /tmp/automosaic-ffmpeg/ort.zip
    unzip -oq /tmp/automosaic-ffmpeg/ort.zip -d /tmp/automosaic-ffmpeg/ort
    cp /tmp/automosaic-ffmpeg/ort/onnxruntime-win-x64-*/lib/onnxruntime.dll "$PLATFORM/"
    cp /tmp/automosaic-ffmpeg/ort/onnxruntime-win-x64-*/lib/onnxruntime_providers_shared.dll "$PLATFORM/"
    ;;
  *)
    echo "不支持的平台: $OS $ARCH（请手动放置 ffmpeg/ffprobe 到 bin/ 下并调整 tool_path）" >&2
    exit 1
    ;;
esac

chmod +x "$PLATFORM"/ffmpeg* 2>/dev/null || true
echo "== $PLATFORM =="
# shared 变体（Linux）的 so 与二进制同目录但 rpath 不含 $ORIGIN——
# 自检统一带同目录 LD（运行期由 AppRun 的 LD_LIBRARY_PATH 覆盖）
if ls "$PLATFORM"/*.so.* >/dev/null 2>&1; then
  export LD_LIBRARY_PATH="$PLATFORM${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
"$PLATFORM/ffmpeg" -version | head -1
echo "-- hwaccels --"
"$PLATFORM/ffmpeg" -hide_banner -hwaccels 2>/dev/null | tail -n +2 | tr '\n' ' '
echo
echo "-- libx264（软编兜底必须存在） --"
"$PLATFORM/ffmpeg" -hide_banner -encoders 2>/dev/null | grep -c libx264
rm -rf /tmp/automosaic-ffmpeg
echo "完成。注意：bin/ 不入 git（.gitignore），打包时随应用分发。"
