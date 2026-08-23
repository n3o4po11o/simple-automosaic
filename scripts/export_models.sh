#!/usr/bin/env bash
# 导出 ONNX 模型（供 automosaic-core 推理），对应 DESIGN §5.5 五档质量预设。
# 每个模型导出两个变体：batch=1（主）与 batch=4（批推理，固定形状规避 CoreML 动态批问题）。
# 依赖：.venv（uv 管理，ultralytics ≥8.4）。
#
# ⚠️ 固定 FP32 导出，不要加 half=True：实测（2026-08-19，DESIGN §6）ultralytics 的
# FP16 导出在 CoreML 上反而慢 22%（Cast 边界恶化分段调度），体积减半不值得。
#
# 输出布局（detect.rs 按输出形状自动选择解码路径）：
#   yolo11-seg: output0 [N,116,8400]（anchor 铺排 + NMS）
#   yolo26:     output0 [N,300,4+1+1+32]（e2e 免 NMS，xyxy + score + class + coeffs）
#
# 注意：ultralytics 会把导出写到源 .pt 同目录同名 .onnx——先导出后立即改名，
# 否则第二次导出（b4）会覆盖第一次（b1）。
set -euo pipefail
cd "$(dirname "$0")/.."

# --manifest-only：跳过导出，仅重生成 manifest.json（改下载源后用）
MANIFEST_ONLY=0
[ "${1:-}" = "--manifest-only" ] && MANIFEST_ONLY=1

# CI 环境用 AUTOMOSAIC_PY=python3 覆盖（无 .venv）
PY="${AUTOMOSAIC_PY:-.venv/bin/python}"
if ! command -v "$PY" >/dev/null 2>&1 && [ ! -x "$PY" ]; then
  echo "未找到 $PY（请安装 ultralytics 后手动导出，或设 AUTOMOSAIC_PY）" >&2
  exit 1
fi

if [ "$MANIFEST_ONLY" = "0" ]; then
# 源权重下载：全部来自公开 URL；ModelScope 为主源（国内/官方镜像），GitHub 回退。
# .pt 与本仓库开发时使用的文件逐字节一致（sha 已核对），保证 CI 导出与本地一致。
fetch_url() { # $1=目标 $2=ModelScope或首选 $3=回退
  local f="$1"
  [ -f "$f" ] && return 0
  echo "下载 $f …"
  mkdir -p "$(dirname "$f")"
  curl -fSL --retry 2 -o "$f" "$2" || curl -fSL --retry 2 -o "$f" "$3"
}
MS26="https://www.modelscope.cn/models/Ultralytics/YOLO26/resolve/master"
GH26="https://github.com/ultralytics/assets/releases/download/v8.4.0"
MS11="https://www.modelscope.cn/models/Ultralytics/YOLO11/resolve/master"
AKFACE="https://github.com/akanametov/yolo-face/releases/download/1.0.0"
ZJFACE="https://github.com/zjykzj/YOLO11Face/releases/download/v1.0.0"
fetch_url yolo26n.pt            "$MS26/yolo26n.pt"            "$GH26/yolo26n.pt"
# 中间档（DESIGN §5.1：m=准确与极致之间高性价比，l=接近极致；无预设消费，
# --model models/yolo26m-seg.onnx 显式选用）
fetch_url yolo26m-seg.pt        "$MS26/yolo26m-seg.pt"        "$GH26/yolo26m-seg.pt"
fetch_url yolo26l-seg.pt        "$MS26/yolo26l-seg.pt"        "$GH26/yolo26l-seg.pt"
fetch_url yolo26n-seg.pt        "$MS26/yolo26n-seg.pt"        "$GH26/yolo26n-seg.pt"
fetch_url yolo26s-seg.pt        "$MS26/yolo26s-seg.pt"        "$GH26/yolo26s-seg.pt"
fetch_url yolo26x-seg.pt        "$MS26/yolo26x-seg.pt"        "$GH26/yolo26x-seg.pt"
fetch_url yolo11n-seg.pt        "$MS11/yolo11n-seg.pt"        "$GH26/yolo11n-seg.pt"
fetch_url models/yolov8n-face.pt     "$AKFACE/yolov8n-face.pt"     "$AKFACE/yolov8n-face.pt"
fetch_url models/yolo11n-face-pose.pt "$ZJFACE/yolo11n-pose_widerface.pt" "$ZJFACE/yolo11n-pose_widerface.pt"
fetch_url models/yolo11s-face-pose.pt "$ZJFACE/yolo11s-pose_widerface.pt" "$ZJFACE/yolo11s-pose_widerface.pt"
# 速度档人脸兜底：YuNet（OpenCV Zoo 官方 ONNX，MIT；非 ultralytics 导出，直连下载）
fetch_url models/face_detection_yunet_2023mar.onnx \
  "https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx" \
  "https://www.modelscope.cn/models/n3o4po11o/simple-automosaic-models/resolve/master/face_detection_yunet_2023mar.onnx"

mkdir -p models
"$PY" - <<'EOF'
from ultralytics import YOLO
import shutil, os

def export(src, dst, imgsz):
    stem = dst[:-len('.onnx')]
    # b4 先导：ultralytics 总是写到源 .pt 旁的同名 .onnx——若 .pt 在 models/ 内，
    # b1/b4 会写同一路径互相覆盖；先把 b4 挪到 -b4，b1 最后落位
    p4 = YOLO(src).export(format='onnx', imgsz=imgsz, batch=4, opset=17, simplify=True, dynamic=False)
    shutil.move(p4, f'{stem}-b4.onnx')
    p = YOLO(src).export(format='onnx', imgsz=imgsz, batch=1, opset=17, simplify=True, dynamic=False)
    if os.path.abspath(p) != os.path.abspath(dst):
        shutil.move(p, dst)
    print(f"{dst} (b1+b4, imgsz={imgsz})")

# 速度档：yolo26n 检测框 + margin（无 mask 头，output0 [N,300,6]）
export('yolo26n.pt', 'models/yolo26n.onnx', 640)
# 均衡档：yolo26n-seg
export('yolo26n-seg.pt', 'models/yolo26n-seg.onnx', 640)
# 准确档：yolo26s-seg @960
export('yolo26s-seg.pt', 'models/yolo26s-seg.onnx', 960)
# 极致档：yolo26x-seg @1280（仅 GPU 推理推荐，约 2-4GB 显存）
export('yolo26x-seg.pt', 'models/yolo26x-seg.onnx', 1280)
# 中间档：m @960（准确档成本）、l @1280（接近极致成本）
export('yolo26m-seg.pt', 'models/yolo26m-seg.onnx', 960)
export('yolo26l-seg.pt', 'models/yolo26l-seg.onnx', 1280)

# 人脸线（YOLO11Face n/s-pose，20 通道与 yolov8n-face 同布局，可 drop-in）。
# 权重来自 zjykzj/YOLO11Face releases；失败不影响人体线。
try:
    export('models/yolo11n-face-pose.pt', 'models/yolo11n-face-pose.onnx', 640)
    export('models/yolo11s-face-pose.pt', 'models/yolo11s-face-pose.onnx', 640)
    print("人脸线: yolo11n/s-face-pose")
except Exception as e:
    print(f"YOLO11Face 导出失败（保留 yolov8n-face）: {e}")

# 遗留：yolo11n-seg / yolov8n-face（yolo11 解码路径回归测试用 + 保守备选）
if os.path.exists('yolo11n-seg.pt'):
    export('yolo11n-seg.pt', 'models/yolo11n-seg.onnx', 640)
if os.path.exists('models/yolov8n-face.pt'):
    export('models/yolov8n-face.pt', 'models/yolov8n-face.onnx', 640)
EOF
fi  # MANIFEST_ONLY=0 结束

# 生成 models/manifest.json（文件名 + sha256 + imgsz + 下载源清单，供模型管理器/下载器）
"$PY" - <<'EOF'
import hashlib, json, os, re, subprocess

# 应用内下载源（目录级 URL，文件名自动拼接）：
# 主源（境外/公开）= 本仓库 GitHub Releases 的 models tag，地址自动从 git remote origin
# 推导；镜像（国内）= ModelScope 模型仓。两者均可用环境变量覆盖：
#   MODEL_DL_GH / MODEL_DL_MS
def gh_base():
    env = os.environ.get('MODEL_DL_GH')
    if env:
        return env
    try:
        url = subprocess.run(['git', 'remote', 'get-url', 'origin'],
                             capture_output=True, text=True, check=True).stdout.strip()
    except Exception:
        url = ''
    m = re.match(r'(?:git@github\.com:|https?://github\.com/)([^/]+/[^/]+?)(?:\.git)?/?$', url)
    if m:
        return f'https://github.com/{m.group(1)}/releases/download/models'
    # 无 remote 且未覆盖 → 本仓库发布地址（n3o4po11o/simple-automosaic）
    return 'https://github.com/n3o4po11o/simple-automosaic/releases/download/models'

GH = gh_base()
MS = os.environ.get('MODEL_DL_MS',
    'https://www.modelscope.cn/models/n3o4po11o/simple-automosaic-models/resolve/master')
print(f'下载源: 主源={GH}')
print(f'        镜像={MS}')

def sha256(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()

IMGSZ = {
    'yolo26n': 640, 'yolo26n-seg': 640, 'yolo26s-seg': 960, 'yolo26x-seg': 1280,
    'yolo26m-seg': 960, 'yolo26l-seg': 1280,
    'yolo11n-seg': 640,
    'yolo11n-face-pose': 640, 'yolo11s-face-pose': 640, 'yolov8n-face': 640,
    'face_detection_yunet_2023mar': 640,
}
# 非自导出模型（直连上游源，不用本仓库 Release）
SPECIAL_URL = {
    'face_detection_yunet_2023mar': {
        'url': 'https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet',
        'mirror_url': MS,
    },
}
# 既有条目打底：M5 ensemble 组件（gdino/sam2/retinaface/osnet 等，条目由
# fetch_m5_models.sh 流程/手工维护，含 direct_url/direct_mirror 字段——本生成
# 器不产出这些字段）原样保留，避免 CI 全新 checkout 上再生成时丢失
merged = {}
if os.path.exists('models/manifest.json'):
    with open('models/manifest.json') as f:
        for e in json.load(f).get('models', []):
            merged[e['file']] = e

for f in sorted(os.listdir('models')):
    if not f.endswith('.onnx'):
        continue
    stem = f[:-len('.onnx')]
    if stem.endswith('-b4'):
        continue
    b4 = f'{stem}-b4.onnx'
    has_b4 = os.path.exists(f'models/{b4}')
    special = SPECIAL_URL.get(stem, {})
    entry = {
        'file': f,
        'batch_file': b4 if has_b4 else None,
        'imgsz': IMGSZ.get(stem, 640),
        'sha256': sha256(f'models/{f}'),
        'sha256_batch': sha256(f'models/{b4}') if has_b4 else None,
        'size_mb': round(os.path.getsize(f'models/{f}') / 1048576, 1),
        'url': special.get('url', GH),
        'mirror_url': special.get('mirror_url', MS),
    }
    # 自导出族（IMGSZ 已知）刷新条目（sha/批变体跟随重导出）；其余沿用既有
    if stem in IMGSZ or f not in merged:
        merged[f] = entry

# 沿用条目里的"裸 base"下载源（本生成器写法特征：不以文件名结尾）跟随当前
# 配置刷新——仓库迁移后旧地址不残留；上游全文件 URL（direct_url 等）不动
def refresh_base(u, new_base, marker):
    if isinstance(u, str) and marker in u and u.endswith('/resolve/master'):
        return new_base
    if isinstance(u, str) and u.endswith('/releases/download/models'):
        return new_base
    return u
for e in merged.values():
    e['url'] = refresh_base(e.get('url'), GH, 'modelscope.cn/models/')
    e['mirror_url'] = refresh_base(e.get('mirror_url'), MS, 'modelscope.cn/models/')
    e['direct_mirror'] = refresh_base(e.get('direct_mirror'), MS, 'modelscope.cn/models/')
entries = sorted(merged.values(), key=lambda e: e['file'])
with open('models/manifest.json', 'w') as f:
    json.dump({'models': entries}, f, indent=2)
print(f"manifest.json: {len(entries)} 个模型（含下载源）")
EOF
echo "完成: models/*.onnx + manifest.json"
