#!/usr/bin/env bash
# 把 models/*.onnx + manifest.json 发布到 GitHub Releases（tag=models）——
# 即应用内下载的公开主源（境外访问）。
#
# 前置（一次性）：
#   1. 仓库已推送到 GitHub（git remote origin 存在——manifest 主源地址由它自动推导）
#   2. gh 已安装并登录：brew install gh && gh auth login
#
# 用法：scripts/publish_models.sh          # 首次创建 release 并上传
#       scripts/publish_models.sh --force  # 已存在时覆盖上传（模型更新后）
set -euo pipefail
cd "$(dirname "$0")/.."

command -v gh >/dev/null 2>&1 || {
  echo "未安装 gh CLI：brew install gh && gh auth login" >&2
  exit 1
}
git remote get-url origin >/dev/null 2>&1 || {
  echo "未配置 git remote origin——manifest 主源地址无法推导，请先推送仓库" >&2
  exit 1
}

# 重生成 manifest（主源地址自动从 origin 推导，含最新 sha256）
bash scripts/export_models.sh --manifest-only

FILES=(models/*.onnx models/manifest.json)
echo "发布 ${#FILES[@]} 个文件到 GitHub Releases tag=models …"

if gh release view models >/dev/null 2>&1; then
  gh release upload models "${FILES[@]}" --clobber
  echo "已覆盖上传（release models 已存在）"
else
  gh release create models "${FILES[@]}" \
    --title "模型分发源" \
    --notes "应用内模型下载主源（境外）。国内镜像：ModelScope（见 manifest mirror_url）。
文件由 scripts/export_models.sh 生成（FP32，b1+b4 变体）；SHA256 见 manifest.json。"
fi

# 验证：抽查一个文件的公开直链可达
REPO_SLUG=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
if [ -n "$REPO_SLUG" ]; then
  URL="https://github.com/$REPO_SLUG/releases/download/models/manifest.json"
  if curl -fsI --max-time 15 "$URL" | head -1 | grep -q "200\|302"; then
    echo "✓ 公开直链可达：$URL"
  else
    echo "⚠ 直链暂不可达（release 生效可能需要几秒）：$URL"
  fi
fi
echo "完成。国内镜像：在 ModelScope 创建模型仓后上传同名文件，并用 MODEL_DL_MS=<地址> 重跑 --manifest-only"
