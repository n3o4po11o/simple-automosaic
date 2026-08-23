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

# ---- 2) 内置 ffmpeg ----
# 优先发行版 ffmpeg（动态链接 libva → 运行期用宿主 libva/驱动，VAAPI 跨
# 发行版可用）；BtbN 预编译静态烘焙 libva 2.7（API 1.7）且 DRI 驱动路径
# 写死 Debian 布局——非 Debian 系宿主（Fedora: /usr/lib64/dri）VAAPI 直接
# 初始化失败（真机三验：libva "Trying /usr/lib/x86_64-linux-gnu/dri" 后
# va_openDriver -1）。BtbN 仅作无 apt 环境的兜底。
if [ ! -x "$ROOT/bin/linux-$ARCH/ffmpeg" ]; then
  log "拉取 BtbN shared 预编译 ffmpeg（9.x 现代参数 + 动态 libva）"
  (cd "$ROOT" && bash scripts/fetch_ffmpeg.sh)
fi
FFMPEG="$ROOT/bin/linux-$ARCH/ffmpeg"
# shared 变体 rpath 不含 $ORIGIN，版本自检带同目录 LD（运行期 AppRun 已设）
LD_LIBRARY_PATH="$ROOT/bin/linux-$ARCH" "$FFMPEG" -version | head -1

# ---- 3) Flutter Linux Release 构建（cargokit 随之编译 Rust dylib）----
cd "$ROOT/app"
flutter config --enable-linux-desktop >/dev/null 2>&1 || true
flutter pub get >/dev/null
flutter build linux --release
BUNDLE="build/linux/$FLUTTER_ARCH/release/bundle"
BIN="$BUNDLE/simple-automosaic"
test -x "$BIN" || { echo "构建产物缺失：$BIN"; exit 1; }
log "Flutter bundle 完成：$BUNDLE"

# ---- 5) 图标（无设计稿时生成品牌色马赛克占位）----
ICON="$ROOT/scripts/simple-automosaic-512.png"
if [ ! -f "$ICON" ]; then
  log "生成占位图标"
  "$FFMPEG" -y -loglevel error -f lavfi -i "color=c=0x1C1F26:s=512x512,drawgrid=w=64:h=64:c=0x4CD964@0.9:t=8" \
    -frames:v 1 "$ICON"
fi

# 宿主仅承担 GUI 桌面必自带的栈（GTK3/glib/X11/Wayland/系统服务——
# ABI 稳定且桌面发行版全带）与 glibc/libstdc++；其余依赖全部随包。
# 编解码族（jpeg/tiff/webp/ogg/vorbis/flac/opus/speex/theora/sndfile/soxr）
# 曾在白名单——但 soname 跨发行版系分裂（libjpeg：Debian .8 / Fedora .62
# 等），Fedora 系宿主缺 Debian soname 直接加载失败（真机二验：libjpeg.so.8
# not found）；改为全部随包（~10MB，自洽）。
# libva/libvdpau 属驱动栈：必须宿主提供（DRI 驱动路径由宿主 libva 自知，
# 各发行版布局不同；随包的 Debian libva 在 Fedora 系宿主必然找不到驱动，
# 且其老 API 缺 vaMapBuffer2 会反噬宿主程序——真机四验实证）。
is_host_only() {
  case "$1" in
    ld-linux*|libc.so*|libm.so*|libmvec*|libdl*|libpthread*|librt*|libresolv*|libanl*|libnsl*|libutil.so*|libgcc_s*|libstdc++*|libGL*|libEGL*|libglapi*|libgbm*|libdrm*|libz.so*|libzstd*|liblzma*|liblz4*|libexpat*|libpcre2*|libmd*|libacl*|libattr*|libcap*|libselinux*|libuuid*|libblkid*|libmount*|libseccomp*|libkeyutils*|libdbus*|libsystemd*|libudev*|libglib*|libgobject*|libgio*|libgmodule*|libgthread*|libpango*|libcairo*|libgdk*|libgtk*|libharfbuzz*|libfreetype*|libfontconfig*|libpixman*|libpng*|libgraphite2*|libjbig*|liblcms*|libX11*|libXau*|libXdmcp*|libXext*|libXrender*|libxcb*|libxkbcommon*|libwayland*|libICE*|libSM*|libnuma*|libpipewire*|libspa*|libjack*|libpulse*|libasound*|libva*|libvdpau*) return 0 ;;
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
  # <root>/ffmpeg、<root>/models/、<root>/Resources/models/）；shared 构建的
  # so 同目录放置（rpath=$ORIGIN），libva/libvdpau 被白名单排除→宿主提供
  cp "$FFMPEG" "$APPDIR/usr/bin/ffmpeg"
  # shared 变体的 so 放 bundle lib 目录（闭包收集/打包自检/AppRun 的
  # LD_LIBRARY_PATH 三方一致；libva/libvdpau 剔除→宿主提供）
  mkdir -p "$APPDIR/usr/bin/lib"
  for so in "$ROOT/bin/linux-$ARCH"/*.so.*; do
    [ -e "$so" ] || continue
    case "$(basename "$so")" in libva*|libvdpau*) continue ;; esac
    cp -Ln "$so" "$APPDIR/usr/bin/lib/"
  done
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
    for target in "$APPDIR/usr/bin/simple-automosaic" "$bundle_lib"/*.so* \
                  "$bundle_lib"/gdk-pixbuf-2.0/*/loaders/*.so "$APPDIR/usr/bin/ffmpeg"; do
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

  # 容器内无 FUSE：解包运行 AppImage 工具。不再用 linuxdeploy——其依赖
  # 部署会在 samba 私有库（libutil-tdb-private-samba.so 等 dlopen 内部解析）
  # 上必败，而该功能本已被自有闭包+白名单替代；desktop/icon/AppRun 的
  # 摆放自助完成（appimagetool 只需 AppDir 根部三件 + AppRun 可执行）
  export APPIMAGE_EXTRACT_AND_RUN=1
  install -D -m 755 "$ROOT/scripts/AppRun-linux" "$APPDIR/AppRun"
  install -D -m 644 "$APPDIR/dev.automosaic.simpleAutomosaic.desktop" \
    "$APPDIR/usr/share/applications/dev.automosaic.simpleAutomosaic.desktop"
  for sz in 128 256; do
    install -D -m 644 "$ICON" \
      "$APPDIR/usr/share/icons/hicolor/${sz}x${sz}/apps/simple-automosaic.png"
  done

  # appimagetool 打包清理后的 AppDir（工具缓存目录；下载含 5 轮重试）
  local TOOLS="$ROOT/.tools"
  mkdir -p "$TOOLS"
  local AT="$TOOLS/appimagetool-$ARCH.AppImage"
  if [ ! -f "$AT" ]; then
    log "下载 appimagetool"
    fetch_retry "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage" "$AT"
    chmod +x "$AT"
  fi
  (cd "$ROOT" && ARCH="$ARCH" "$AT" "$APPDIR" "$FINAL" >/dev/null)

  rm -rf "$APPDIR"
  (cd "$ROOT/dist" && shasum -a 256 "$(basename "$FINAL")" | tee "$(basename "$FINAL").sha256")
  log "产物：$FINAL（$(du -h "$FINAL" | cut -f1)）"
}

mkdir -p "$ROOT/dist"; rm -f "$ROOT/dist"/*linux*.AppImage "$ROOT/dist"/*linux*.sha256

log "== 打包（全模型离线可用）=="
make_appimage "$ROOT/dist/simple-automosaic-linux-${ARCH}.AppImage"

log "完成：dist/simple-automosaic-linux-${ARCH}.AppImage + .sha256"
