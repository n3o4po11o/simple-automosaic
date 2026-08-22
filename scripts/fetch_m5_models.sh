#!/usr/bin/env bash
# M5 极限·档案级档模型资产获取（DESIGN §5.6 管线 A）：
#   - yolo26x-seg @1536：本地 .pt 导出（ultralytics，同 export_models.sh 惯例）
#   - grounding-dino-tiny：HF onnx-community fp16 变体（GD-base 自导出需
#     transformers 工具链，设计文档 §5.6 降级链明示 tiny 为现成替代）
#   - SAM2.1（tiny 开发调试 + large 档案级默认）：HF vietanhdev zip → 解压
#   - retinaface-r34：yakhyo/retinaface-pytorch GitHub release（biubug6 MIT 移植；
#     R50 官方权重仅 Google Drive 分发、镜像实测偏弱，R34 为验证过的可得最优）
#   - osnet-x025-msmt17：HF anriha（BoT-SORT 同源 ReID）
# 下载后按 models/manifest.json 逐条 SHA256 校验。
set -euo pipefail
cd "$(dirname "$0")/.."

HF="https://huggingface.co"
MODELS=models

fetch() { # $1=目标 $2=URL（存在即跳过）
  local f="$1"
  [ -f "$MODELS/$f" ] && { echo "已有 $f"; return 0; }
  echo "下载 $f …"
  mkdir -p "$MODELS"
  curl -fSL --retry 3 -o "$MODELS/$f" "$2"
}

sam_zip() { # $1=变体名 tiny|large $2=zip 文件名
  local v="$1" z="$2"
  if [ ! -f "$MODELS/sam2.1-$v-encoder.onnx" ]; then
    echo "下载并解压 SAM2.1-$v …"
    tmp=$(mktemp -d)
    curl -fSL --retry 3 -o "$tmp/s.zip" "$HF/vietanhdev/segment-anything-2.1-onnx-models/resolve/main/$z"
    unzip -o -q "$tmp/s.zip" -d "$tmp/sam"
    enc=$(find "$tmp/sam" -name "*encoder*.onnx" | head -1)
    dec=$(find "$tmp/sam" -name "*decoder*.onnx" | head -1)
    mv "$enc" "$MODELS/sam2.1-$v-encoder.onnx"
    mv "$dec" "$MODELS/sam2.1-$v-decoder.onnx"
    rm -rf "$tmp"
  fi
}

# YOLO26x-seg @1536（Archive 主检；.pt 已由 export_models.sh 拉取）
if [ ! -f "$MODELS/yolo26x-seg-1536.onnx" ] && [ -f yolo26x-seg.pt ]; then
  PY="${AUTOMOSAIC_PY:-.venv/bin/python}"
  echo "导出 yolo26x-seg @1536（批=1，两阶段档不消费 -b4）…"
  "$PY" - <<'EOF'
from ultralytics import YOLO
import shutil
p = YOLO('yolo26x-seg.pt').export(format='onnx', imgsz=1536, batch=1, opset=17, simplify=True, dynamic=False)
shutil.move(p, 'models/yolo26x-seg-1536.onnx')
EOF
fi

fetch grounding-dino-tiny.onnx "$HF/onnx-community/grounding-dino-tiny-ONNX/resolve/main/onnx/model_fp16.onnx"
sam_zip tiny  sam2.1_hiera_tiny_20260221.zip
sam_zip large sam2.1_hiera_large_20260221.zip
fetch retinaface-r34.onnx "https://github.com/yakhyo/retinaface-pytorch/releases/download/v0.0.1/retinaface_r34.onnx"
fetch osnet-x025-msmt17.onnx "$HF/anriha/osnet_x0_25_msmt17/resolve/main/osnet_x0_25_msmt17.onnx"

# manifest SHA 校验
PY="${AUTOMOSAIC_PY:-python3}"
"$PY" - <<'EOF'
import json, hashlib, sys, os
m = json.load(open("models/manifest.json"))
want = {e["file"]: e["sha256"] for e in m["models"] if e["file"] in {
    "yolo26x-seg-1536.onnx", "grounding-dino-tiny.onnx",
    "sam2.1-tiny-encoder.onnx", "sam2.1-tiny-decoder.onnx",
    "sam2.1-large-encoder.onnx", "sam2.1-large-decoder.onnx",
    "retinaface-r34.onnx", "osnet-x025-msmt17.onnx"}}
bad = 0
for f, sha in want.items():
    p = os.path.join("models", f)
    if not os.path.exists(p):
        print(f"缺失: {f}"); bad += 1; continue
    h = hashlib.sha256(open(p, "rb").read()).hexdigest()
    ok = h == sha
    print(("✓" if ok else "✗ SHA 不符") + f" {f}")
    bad += 0 if ok else 1
sys.exit(1 if bad else 0)
EOF
echo "M5 模型资产就绪（Archive 档可用）"
