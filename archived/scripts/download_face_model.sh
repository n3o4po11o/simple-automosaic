#!/usr/bin/env bash
# 下载 YOLOv8-face 人脸检测权重到 ./models/。
# 下载 YOLOv8-face 人脸检测权重到 ./models/。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODELS="$ROOT/models"
mkdir -p "$MODELS"

# 来源：akanametov/yolo-face（专门做成可被 ultralytics YOLO() 直接加载的格式）
URL="https://github.com/akanametov/yolo-face/releases/download/1.0.0/yolov8n-face.pt"
OUT="$MODELS/yolov8n-face.pt"

if [[ -f "$OUT" ]]; then
  echo "[skip] 已存在: $OUT"
  exit 0
fi

echo "[download] $URL"
if command -v curl >/dev/null 2>&1; then
  curl -L --fail -o "$OUT" "$URL"
else
  wget -O "$OUT" "$URL"
fi

echo "[done] $OUT"
ls -lh "$OUT"
