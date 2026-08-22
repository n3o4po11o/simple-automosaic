#!/usr/bin/env bash
# 版本号机制（DESIGN §0.4 版本行）：单一事实源 = app/pubspec.yaml 的 version 字段，
# 发布产物命名（dmg/AppImage）、设置屏「关于」卡片、CLI --version 全部由此派生。
#   - CLI 经 crates/automosaic-cli/build.rs 构建期读取 pubspec 注入
#   - 设置屏经 package_info_plus 运行期读取（同为 pubspec）
# bump 会同步根 Cargo.toml 的 workspace 版本（避免 CLI 与 app 版本漂移）。
# 用法：
#   scripts/version.sh                              # 打印当前版本
#   scripts/version.sh bump patch|minor|major       # 递增并同步
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PUBSPEC="$ROOT/app/pubspec.yaml"
CARGO="$ROOT/Cargo.toml"

cur() { grep '^version:' "$PUBSPEC" | awk '{print $2}' | cut -d+ -f1; }

bump() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$(cur)"
  case "$1" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
    *) echo "用法: $0 [show | bump patch|minor|major]" >&2; exit 2 ;;
  esac
  local v="$major.$minor.$patch"
  sed -i.bak -E "s|^version: .*|version: $v|" "$PUBSPEC" && rm -f "$PUBSPEC.bak"
  # 只动第一个 version = "..."（[workspace.package] 的版本字段）
  sed -i.bak -E "0,/^version = /s|^version = .*|version = \"$v\"|" "$CARGO" && rm -f "$CARGO.bak"
  echo "$v"
}

case "${1:-show}" in
  show) cur ;;
  bump) bump "${2:?bump 需要参数 patch|minor|major}" ;;
  *) echo "用法: $0 [show | bump patch|minor|major]" >&2; exit 2 ;;
esac
