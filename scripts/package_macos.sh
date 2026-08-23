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
  # 注：曾在此预构建 pods 框架（Xcode 26 规划期模块解析），现已不必要——
  # flutter build 自带 pod install 与 pods 构建（全清缓存实测通过），
  # 且预构建在干净检出上因 pod 未 install 必然失败刷 8 条 warn
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
