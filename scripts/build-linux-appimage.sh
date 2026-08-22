#!/usr/bin/env bash
# Linux AppImage 构建（DESIGN §8；2026-08-22 起单形态——download 移除仅产 full，
# 产物命名统一 simple-automosaic 口径，与二进制/desktop/icon 同源；不带版本号，
# release tag 即版本——与 macOS zip / Windows 产物三平台统一）：
#   simple-automosaic-linux-<arch>.AppImage  全模型离线可用
# 附 .sha256 边车。
#
# 前置：
#   - Flutter SDK（PATH 中，或 $HOME/flutter；均无则按 stable 克隆到 .flutter-sdk）
#   - mpv-devel（media_kit 构建需要 mpv.pc）+ gtk3-devel/clang/cmake/ninja
#   - bin/linux-<arch>/ffmpeg（缺则自动调 scripts/fetch_ffmpeg.sh，BtbN GPL）
#   - models/（全模型入包，scripts/export_models.sh 生成）
#
# 模式源自 simple-automosaic 项目 build-linux-appimage.sh 的实战结论：
#   - Rust dylib 由 Dart 裸名 dlopen → AppRun 必须设 LD_LIBRARY_PATH
#   - libmpv 运行时 dlopen（ldd 闭包不可见）→ 显式入包
#   - 闭包统一收集 + 宿主仅承担桌面自带栈 + 打包自检大声失败
#   - 容器内无 FUSE → APPIMAGE_EXTRACT_AND_RUN=1
# 用法:  bash scripts/build-linux-appimage.sh [项目根]
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
ARCH="$(uname -m)"
[ "$ARCH" = "x86_64" ] || { echo "仅支持 x86_64（当前 ${ARCH}）"; exit 1; }
FLUTTER_ARCH="x64"

log() { printf '\033[1;32m[appimage]\033[0m %s\n' "$*"; }

fetch_retry() {
  local url="$1" dest="$2" i rc
  for i in 1 2 3 4 5; do
    if curl -fsSL --retry 3 --retry-delay 10 -o "$dest" "$url"; then
      return 0
    fi
    rc=$?
    log "下载失败（exit $rc，第 $i/5 轮），15s 后重试：$url"
    sleep 15
  done
  return 1
}

# ---- 1) Flutter SDK ----
if ! command -v flutter >/dev/null 2>&1 && [ -x "$HOME/flutter/bin/flutter" ]; then
  export PATH="$HOME/flutter/bin:$PATH"
fi
if ! command -v flutter >/dev/null 2>&1; then
  FLUTTER_DIR="$ROOT/.flutter-sdk"
  if [ ! -x "$FLUTTER_DIR/bin/flutter" ]; then
    log "克隆 Flutter stable → $FLUTTER_DIR"
    git clone --depth 1 -b stable https://github.com/flutter/flutter.git "$FLUTTER_DIR"
  fi
  export PATH="$FLUTTER_DIR/bin:$PATH"
fi
flutter --version 2>/dev/null | head -1

# ---- 2) 内置 ffmpeg（BtbN GPL：VAAPI/NVDEC/QSV + libx264 兜底）----
if [ ! -x "$ROOT/bin/linux-$ARCH/ffmpeg" ]; then
  log "拉取内置 ffmpeg（fetch_ffmpeg.sh）"
  (cd "$ROOT" && bash scripts/fetch_ffmpeg.sh)
fi
FFMPEG="$ROOT/bin/linux-$ARCH/ffmpeg"
"$FFMPEG" -version | head -1

# ---- 3) Flutter Linux Release 构建（cargokit 随之编译 Rust dylib）----
cd "$ROOT/app"
flutter config --enable-linux-desktop >/dev/null 2>&1 || true
flutter pub get >/dev/null
flutter build linux --release
BUNDLE="build/linux/$FLUTTER_ARCH/release/bundle"
BIN="$BUNDLE/simple-automosaic"
test -x "$BIN" || { echo "构建产物缺失：$BIN"; exit 1; }
log "Flutter bundle 完成：$BUNDLE"

# ---- 4) linuxdeploy ----
TOOLS="$ROOT/.tools"; mkdir -p "$TOOLS"
LD="$TOOLS/linuxdeploy-$ARCH.AppImage"
if [ ! -f "$LD" ]; then
  log "下载 linuxdeploy"
  fetch_retry "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage" "$LD"
  chmod +x "$LD"
fi

# ---- 5) 图标（无设计稿时生成品牌色马赛克占位）----
ICON="$ROOT/scripts/simple-automosaic-512.png"
if [ ! -f "$ICON" ]; then
  log "生成占位图标"
  "$FFMPEG" -y -loglevel error -f lavfi -i "color=c=0x1C1F26:s=512x512,drawgrid=w=64:h=64:c=0x4CD964@0.9:t=8" \
    -frames:v 1 "$ICON"
fi

# 宿主仅承担 GUI 桌面必自带的栈（GTK3/glib/X11/Wayland/音频/系统服务——
# ABI 稳定且桌面发行版全带）与 glibc/libstdc++；其余依赖全部随包。
is_host_only() {
  case "$1" in
    ld-linux*|libc.so*|libm.so*|libmvec*|libdl*|libpthread*|librt*|libresolv*|libanl*|libnsl*|libutil*|libcrypt*|libgcc_s*|libstdc++*|libGL*|libEGL*|libglapi*|libgbm*|libdrm*|libz.so*|libzstd*|liblzma*|liblz4*|libexpat*|libffi*|libpcre2*|libmd*|libacl*|libattr*|libcap*|libselinux*|libuuid*|libblkid*|libmount*|libseccomp*|libkeyutils*|libdbus*|libsystemd*|libudev*|libglib*|libgobject*|libgio*|libgmodule*|libgthread*|libpango*|libcairo*|libgdk*|libgtk*|libharfbuzz*|libfreetype*|libfontconfig*|libpixman*|libpng*|libjpeg*|libtiff*|libwebp*|libgraphite2*|libjbig*|liblcms*|libX11*|libXau*|libXdmcp*|libXext*|libXrender*|libxcb*|libxkbcommon*|libwayland*|libICE*|libSM*|libogg*|libvorbis*|libflac*|libopus*|libspeex*|libtheora*|libsndfile*|libsoxr*|libnuma*|libpipewire*|libspa*|libjack*|libpulse*|libasound*) return 0 ;;
    *) return 1 ;;
  esac
}

make_appimage() { # $1 = 输出 AppImage 路径
  local FINAL="$1"
  local APPDIR="$ROOT/dist/AppDir"
  rm -rf "$APPDIR"
  mkdir -p "$APPDIR/usr/bin"
  # Flutter bundle 结构：可执行文件与 lib/、data/ 必须保持同级
  cp -r "$BUNDLE/." "$APPDIR/usr/bin/"

  # ffmpeg 与模型放可执行文件同级（tool_path/candidate_roots 按 exe 祖先解析：
  # <root>/ffmpeg、<root>/models/、<root>/Resources/models/）
  cp "$FFMPEG" "$APPDIR/usr/bin/ffmpeg"
  mkdir -p "$APPDIR/usr/bin/models"
  cp -f "$ROOT/models/"*.onnx "$APPDIR/usr/bin/models/"
  [ -f "$ROOT/models/manifest.json" ] && cp -f "$ROOT/models/manifest.json" "$APPDIR/usr/bin/models/"
  # 许可随包（AGPL/GPL 合规）
  for f in LICENSE NOTICES.md; do
    [ -f "$ROOT/$f" ] && cp -f "$ROOT/$f" "$APPDIR/usr/bin/"
  done
  if [ -d "$ROOT/LICENSES" ]; then
    mkdir -p "$APPDIR/usr/bin/LICENSES"
    cp -f "$ROOT/LICENSES/"*.txt "$APPDIR/usr/bin/LICENSES/" 2>/dev/null || true
  fi

  # libmpv 本体入包（media_kit 运行时 dlopen，ldd 闭包看不见）：优先用
  # media_kit_libs_linux 随 bundle 自带的；缺则从系统 ldconfig 补
  local bundle_lib="$APPDIR/usr/bin/lib"
  if ! ls "$bundle_lib"/libmpv.so* >/dev/null 2>&1; then
    local MPV_LIB="$({ ldconfig -p 2>/dev/null || /sbin/ldconfig -p; } | awk '/libmpv\.so\.2 \(/{print $NF; exit}')"
    if [ -n "$MPV_LIB" ] && [ -f "$MPV_LIB" ]; then
      log "补入系统 libmpv（bundle 未自带）"
      cp -Ln "$MPV_LIB" "$bundle_lib/"
    fi
  fi

  # WebGPU EP 的 Dawn 运行库（ort-sys 产物，位于 cargokit 构建目录——
  # 不在 ldd 搜索路径，闭包解析不到，须显式入包；仅 Linux x86_64 的
  # webgpu feature 构建存在，缺失则跳过（无 WebGPU 的旧构建）
  local CARGOKIT_REL
  CARGOKIT_REL="$(dirname "$BUNDLE")/plugins/rust_lib_automosaic_studio/cargokit_build/x86_64-unknown-linux-gnu/release"
  local DAWN="$(ls "$CARGOKIT_REL/libwebgpu_dawn.so" 2>/dev/null | head -1)"
  if [ -n "$DAWN" ]; then
    log "补入 libwebgpu_dawn（WebGPU EP）"
    cp -Ln "$DAWN" "$bundle_lib/"
  fi

  # 统一闭包收集：主程序 + bundle 全部库（含 dlopen 系）的依赖，
  # 除 is_host_only（桌面自带栈）外一律随包
  local round added so base target
  for round in 1 2 3 4 5; do
    added=0
    for target in "$APPDIR/usr/bin/simple-automosaic" "$bundle_lib"/*.so* "$APPDIR/usr/bin/ffmpeg"; do
      [ -f "$target" ] || continue
      while IFS= read -r so; do
        [ -f "$so" ] || continue
        base="$(basename "$so")"
        is_host_only "$base" && continue
        if [ ! -e "$bundle_lib/$base" ]; then
          cp -Ln "$so" "$bundle_lib/$base" && added=1
        fi
      done < <(LD_LIBRARY_PATH="$bundle_lib" ldd "$target" 2>/dev/null | awk '/=> \// {print $3}')
    done
    [ "$added" -eq 0 ] && break
  done
  log "闭包收集完成"

  # 打包自检：bundle 内每个目标的依赖必须全部可解析，且从系统路径解析的
  # 必须属于 is_host_only——大声失败好过静默带病
  local failed=0 f line dep_name dep_path
  for f in "$APPDIR/usr/bin/simple-automosaic" "$bundle_lib"/*.so* "$APPDIR/usr/bin/ffmpeg"; do
    [ -f "$f" ] || continue
    while IFS= read -r line; do
      dep_name="$(printf '%s\n' "$line" | awk '{print $1}')"
      dep_path="$(printf '%s\n' "$line" | awk '{print $3}')"
      if [ "$dep_path" = "not" ]; then
        echo "错误：$(basename "$f") 依赖未解析：$line" >&2
        failed=1
      elif ! [[ "$dep_path" == "$bundle_lib"* ]] && ! is_host_only "$dep_name"; then
        echo "错误：$(basename "$f") 依赖 $dep_name 未随包（解析自 $dep_path）" >&2
        failed=1
      fi
    done < <(LD_LIBRARY_PATH="$bundle_lib" ldd "$f" 2>/dev/null | awk '/=>/ {print}')
  done
  [ "$failed" -ne 0 ] && exit 1
  log "打包自检：依赖闭包校验通过"

  # 文件名须与 APPLICATION_ID 一致：Wayland app_id / X11 WM_CLASS 按此匹配
  # desktop 条目（任务栏图标与分组），Icon= 则是 hicolor 主题名
  cat > "$APPDIR/dev.automosaic.simpleAutomosaic.desktop" <<EOF
[Desktop Entry]
Name=Simple AutoMosaic
Comment=Video person auto-mosaic (local inference)
Exec=simple-automosaic
Icon=simple-automosaic
Terminal=false
Type=Application
Categories=AudioVideo;Video;Utility;
StartupWMClass=dev.automosaic.simpleAutomosaic
EOF
  cp "$ICON" "$APPDIR/simple-automosaic.png"

  # 容器内无 FUSE：解包运行 AppImage 工具
  export APPIMAGE_EXTRACT_AND_RUN=1
  (cd "$ROOT" && "$LD" --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/simple-automosaic" \
    --desktop-file "$APPDIR/dev.automosaic.simpleAutomosaic.desktop" \
    --icon-file "$APPDIR/simple-automosaic.png" \
    --custom-apprun "$ROOT/scripts/AppRun-linux" \
    --output appimage >/dev/null)

  rm -f "$FINAL"
  OUT="$(ls -t "$ROOT"/*.AppImage 2>/dev/null | head -1)"
  [ -n "$OUT" ] && mv "$OUT" "$FINAL"
  rm -rf "$APPDIR"
  (cd "$ROOT/dist" && shasum -a 256 "$(basename "$FINAL")" | tee "$(basename "$FINAL").sha256")
  log "产物：$FINAL（$(du -h "$FINAL" | cut -f1)）"
}

mkdir -p "$ROOT/dist"; rm -f "$ROOT/dist"/*linux*.AppImage "$ROOT/dist"/*linux*.sha256

log "== 打包（全模型离线可用）=="
make_appimage "$ROOT/dist/simple-automosaic-linux-${ARCH}.AppImage"

log "完成：dist/simple-automosaic-linux-${ARCH}.AppImage + .sha256"
