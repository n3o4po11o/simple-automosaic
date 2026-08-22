#!/usr/bin/env bash
# 打包 macOS 发布产物（DESIGN §8；2026-08-22 起单形态——download 移除仅产 full）。
# 产物口径与 Linux/Windows 三平台统一（不带版本号，release tag 即版本）：
#   simple-automosaic-macos-arm64.zip  全模型离线可用
# zip 用 ditto 打包（保留 bundle 元数据/资源派生/权限；hdiutil dmg 已弃用——
# zip 在 CI 上更简单可靠，且与 .app 内 672MB 模型的压缩率相当）。
# 附 .sha256 边车（内容 `<哈希>  <文件名>`）。
#
# 前置：models/ 已由 scripts/export_models.sh 生成；
#       bin/darwin-arm64/ffmpeg 已由 scripts/fetch_ffmpeg.sh 拉取。
# 产物：dist/simple-automosaic-macos-arm64.zip（本地与 CI release 工作流共用本脚本）。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="app/build/macos/Build/Products/Release/simple-automosaic.app"
OUT="dist"
ZIP="$OUT/simple-automosaic-macos-arm64.zip"
mkdir -p "$OUT"; rm -f "$OUT"/*.zip "$OUT"/*.sha256 "$OUT"/*.dmg

build_app() { # 环境变量 BUNDLE_MODELS 透传给 Xcode 打包阶段
  # 清洁构建（CI 检出）需先预构建 pods 框架：Runner 不直接依赖 pods 目标，
  # Xcode 26 的规划期模块解析要求产物已存在（本项目 CI 实测教训）
  for s in desktop_drop file_selector_macos media_kit_libs_macos_video \
           media_kit_native_event_loop media_kit_video package_info_plus \
           shared_preferences_foundation wakelock_plus; do
    (cd app/macos && xcodebuild -workspace Runner.xcworkspace -scheme "$s" \
      -configuration Release -derivedDataPath "$PWD/../build/macos" \
      -destination generic/platform=macOS \
      OBJROOT="$PWD/../build/macos/Build/Intermediates.noindex" \
      SYMROOT="$PWD/../build/macos/Build/Products" \
      COMPILER_INDEX_STORE_ENABLE=NO build >/dev/null 2>&1) \
      || echo "warn: pods 预构建 $s 失败（增量构建可忽略）"
  done
  (cd app && flutter build macos --release)
}

make_zip() {
  rm -f "$ZIP"
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
  (cd "$OUT" && shasum -a 256 "$(basename "$ZIP")" | tee "$(basename "$ZIP").sha256")
  du -h "$ZIP" | awk -v t="$(basename "$ZIP")" '{print "产物:", t, "("$1")"}'
}

echo "== 打包（全模型离线可用）=="
BUNDLE_MODELS=full build_app
make_zip

echo "完成：dist/simple-automosaic-macos-arm64.zip + .sha256"
