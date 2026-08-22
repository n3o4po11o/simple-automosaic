#!/usr/bin/env bash
# 生成可公开的发布分支：从 git 索引导出被跟踪文件（git archive，天然不含
# 构建产物/模型/未跟踪的视频 fixture），剔除开发私有内容后压缩为单一初始
# commit——历史中的内网 IP / 用户路径等隐私不随发布外泄。本地 main 与完整
# 历史不受影响。
#
# 清洗项：
#   - 剔除 AGENTS.md（开发决策记录，含内网环境）
#   - 剔除 scripts/sync-remote.sh（LAN 同步脚本，含内网主机/路径）
#   - 硬失败检查：导出树不得含内网 IP / 内网用户名 / 用户家目录路径 / 视频文件
#   - （README 内网引用已在 main 中性化，无需脚本清洗）
#
# 注意：导出的是已提交内容，先在 main 提交最新改动再跑本脚本。
#
# 用法: bash scripts/prepare-public.sh [分支名，默认 public]
# 之后: git push <remote> public:main   # 推到 GitHub 的 main
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BRANCH="${1:-public}"

# 公开 commit 的作者身份（可用 PUBLIC_NAME/PUBLIC_EMAIL 覆盖）
PUBLIC_NAME="${PUBLIC_NAME:-n3o4po11o}"
PUBLIC_EMAIL="${PUBLIC_EMAIL:-n3o4po11o@gmail.com}"

cd "$ROOT"

# 发布树中排除的开发/私有内容（本地保留，仅不进公开分支）
EXCLUDE=(
  AGENTS.md
  scripts/sync-remote.sh
)

if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
  echo "分支 ${BRANCH} 已存在（可 git branch -D 删除后重建）" >&2
  exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "工作树有未提交改动——先提交到 main 再生成发布分支" >&2
  exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# git archive 只导出被跟踪文件（遵守 gitignore，无构建产物/模型/视频缓存）
mkdir -p "$WORK/tree"
git archive HEAD | tar -x -C "$WORK/tree"
for f in "${EXCLUDE[@]}"; do
  rm -f "$WORK/tree/$f"
done

# 硬失败：清洗后不得再含内网 IP / 家目录路径 / 视频文件（检测模式本身保持
# 通用——不得出现具体内网值，否则检查器成为泄露源）
LEAKS="$(grep -rIl -E '192\.168\.|/Users/[a-z]/' "$WORK/tree" 2>/dev/null || true)"
VIDEOS="$(find "$WORK/tree" -type f \( -name '*.mp4' -o -name '*.mov' -o -name '*.avi' -o -name '*.mkv' \) || true)"
if [ -n "$LEAKS" ] || [ -n "$VIDEOS" ]; then
  [ -n "$LEAKS" ] && { echo "错误：发布树仍含内网引用："; echo "$LEAKS"; }
  [ -n "$VIDEOS" ] && { echo "错误：发布树含视频文件："; echo "$VIDEOS"; }
  echo "先在 main 清理后再导出" >&2
  exit 1
fi

echo "发布树内容："
(cd "$WORK/tree" && ls)

git -C "$WORK/tree" init -q -b "$BRANCH"
# -f 必需：导出树自带 .gitignore（如 models/ 规则），add -A 会把"被跟踪
# 但被忽略"的文件（models/manifest.json）静默丢掉
git -C "$WORK/tree" add -Af
git -C "$WORK/tree" -c user.name="$PUBLIC_NAME" -c user.email="$PUBLIC_EMAIL" \
  commit -q -m "Simple AutoMosaic: 跨平台视频人物自动打马赛克（Rust + Flutter）"

# 把孤儿分支接回本仓库
git fetch "$WORK/tree" "$BRANCH:$BRANCH"

echo
echo "完成：分支 ${BRANCH}（单一初始 commit，已排除 ${EXCLUDE[*]}，隐私检查通过）"
echo "发布：git push <remote> ${BRANCH}:main"
git log --oneline "$BRANCH" | head -3
