#!/usr/bin/env bash
# 从 GitHub Release（tag=models）拉取已导出的 ONNX——平台发布构建的模型
# 来源。导出（.pt→ONNX，需 Python+torch+ultralytics）只在 publish-models
# 工作流做一次并发布到该 Release；模型不变时各平台 job 仅下载+校验+打包，
# 不再各自经历一遍导出管线（Linux 老 glibc 装 torch 等问题随之消灭）。
#
# 用法: bash scripts/fetch_models_release.sh [--all]
#   默认：四档全模型 + 中间档/回退（speed/balanced/accurate/extreme 所需）
#   --all：追加 archive 档 M5 ensemble 组件与 1536 主检（Windows 五档形态）
# 依赖：curl + sha256sum（macOS 用 shasum）；校验基准为 release 侧的
#   manifest.json（随资产一同下载）；下载源可用 MODELS_BASE 覆盖。
set -euo pipefail
cd "$(dirname "$0")/.."

ALL=0
[ "${1:-}" = "--all" ] && ALL=1
BASE="${MODELS_BASE:-https://github.com/n3o4po11o/simple-automosaic/releases/download/models}"

# M5 ensemble 组件 + 1536（与 preset.rs ArchiveModelRefs / build_windows.ps1 对齐）
M5_FILES="grounding-dino-tiny.onnx sam2.1-large-encoder.onnx sam2.1-large-decoder.onnx \
sam2.1-tiny-encoder.onnx sam2.1-tiny-decoder.onnx retinaface-r34.onnx \
osnet-x025-msmt17.onnx yolo26x-seg-1536.onnx"

# manifest 以 release 侧为唯一事实源（CI 导出非字节确定，repo 内副本会
# 滞后于已发布资产）；下载失败=release 未发布，给出可行动指引
mkdir -p models
curl -fsSL --retry 3 --retry-delay 10 -o models/manifest.json "$BASE/manifest.json" || {
  echo "错误：models release 缺 manifest.json——先推 models-* 标签或 Actions → publish-models 运行" >&2
  exit 1
}

# manifest 为本仓库生成的规则 JSON：file/batch_file 与 sha256/sha256_batch
# 字段成对相邻，awk 状态机提取两对（批变体 -b4 是独立文件，打包必需）
pairs="$(awk '
  /"file":/ {gsub(/[",]/, "", $2); f = $2}
  /"batch_file":/ {gsub(/[",]/, "", $2); bf = ($2 == "null") ? "" : $2}
  /"sha256":/ {gsub(/[",]/, "", $2); if (f != "") {print f, $2; f = ""}}
  /"sha256_batch":/ {gsub(/[",]/, "", $2); if (bf != "") {print bf, $2; bf = ""}}
' models/manifest.json)"
[ -n "$pairs" ] || { echo "manifest 解析失败（无 file/sha256 对）" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum "$1" | awk '{print $1}'; }
else
  sha() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

want() { # $1=文件名 → 是否需要下载
  if [ "$ALL" = 1 ]; then return 0; fi
  case " $M5_FILES " in *" $1 "*) return 1;; esac
  return 0
}

total=0
echo "$pairs" | while read -r f expect; do
  [ -n "$f" ] || continue
  want "$f" || continue
  if [ -f "models/$f" ] && [ "$(sha "models/$f")" = "$expect" ]; then
    echo "已有 ${f}（sha 一致）"
    continue
  fi
  echo "下载 $f …"
  ok=0
  for i in 1 2 3 4 5; do
    if curl -fsSL --retry 3 --retry-delay 10 -o "models/$f" "$BASE/$f"; then ok=1; break; fi
    echo "  下载失败（第 $i/5 轮），15s 后重试" >&2
    sleep 15
  done
  if [ "$ok" != 1 ]; then
    echo "错误：$f 下载失败——models release 是否已发布？（Actions → publish-models → Run workflow，或推 models-* 标签触发）" >&2
    exit 1
  fi
  got="$(sha "models/$f")"
  if [ "$got" != "$expect" ]; then
    echo "错误：$f SHA256 不符（期望 $expect，得到 $got）" >&2
    exit 1
  fi
done
echo "完成：models/ 已就绪（$( [ "$ALL" = 1 ] && echo 五档全量 || echo 四档)）"