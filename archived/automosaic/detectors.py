"""检测器封装：统一接口，底层可替换。

当前实现：
- BodyDetector: YOLO11-seg，取 COCO `person` 类实例分割轮廓。
- FaceDetector: yolov8n-face.pt（社区权重），取人脸 bbox。

两者都通过 `device='mps'` 在 M1 Max GPU 上加速。
换模型时只需保持 detect() 的返回签名不变即可（见 README）。
"""

from __future__ import annotations

import logging
from pathlib import Path

import numpy as np

log = logging.getLogger("automosaic")

# COCO 中 person 的类别 id
COCO_PERSON = 0

# 默认权重名/路径
BODY_MODEL_DEFAULT = "yolo11n-seg.pt"
FACE_MODEL_DEFAULT = str(Path(__file__).resolve().parent.parent / "models" / "yolov8n-face.pt")


def _import_yolo():
    """延迟导入 ultralytics，便于在没有依赖的环境里 --help 也能跑。"""
    try:
        from ultralytics import YOLO
        return YOLO
    except ImportError as e:  # pragma: no cover
        raise RuntimeError(
            "未安装 ultralytics，请先安装依赖：uv pip install -r requirements.txt"
        ) from e


class BodyDetector:
    """人体实例分割检测器（贴合身形的轮廓 mask）。"""

    def __init__(self, weights: str = BODY_MODEL_DEFAULT, device: str = "mps", conf: float = 0.35):
        YOLO = _import_yolo()
        # ultralytics 会在本地缺失时自动下载 yolo11n-seg.pt
        self.model = YOLO(weights)
        self.device = device
        self.conf = conf
        self.names = self.model.names
        log.info("BodyDetector 加载完成: %s (device=%s)", weights, device)

    def detect(self, frames: list[np.ndarray]) -> list[np.ndarray]:
        """对一批帧推理，返回每帧一个 bool mask（HxW，True=人体区域）。

        ultralytics 的 model.predict 接受 numpy/BGR 或 RGB；为稳妥起见我们传 BGR（与 cv2 一致），
        ultralytics 内部会转换。masks.data 为 [N, h, w] 的 tensor，坐标已是缩放后的尺寸，
        需 resize 回原图。这里用 result.masks.xy（每个实例的轮廓点，原图坐标）来填充，最稳。
        """
        if not frames:
            return []
        results = self.model.predict(
            source=frames, device=self.device, conf=self.conf,
            classes=[COCO_PERSON], verbose=False, retina_masks=True,
        )
        masks: list[np.ndarray] = []
        for r, frame in zip(results, frames):
            h, w = frame.shape[:2]
            m = np.zeros((h, w), dtype=bool)
            masks_xy = getattr(r, "masks", None)
            if masks_xy is not None and hasattr(masks_xy, "xy"):
                for poly in masks_xy.xy:  # poly: (K,2) 原图坐标
                    if poly is None or len(poly) < 3:
                        continue
                    _fill_polygon(m, poly)
            masks.append(m)
        return masks


class FaceDetector:
    """人脸检测器（YOLO-face 专用模型）。"""

    def __init__(self, weights: str = FACE_MODEL_DEFAULT, device: str = "mps", conf: float = 0.25):
        YOLO = _import_yolo()
        if not Path(weights).exists():
            raise FileNotFoundError(
                f"人脸权重不存在: {weights}\n请先运行 ./scripts/download_face_model.sh"
            )
        self.model = YOLO(weights)
        self.device = device
        self.conf = conf
        log.info("FaceDetector 加载完成: %s (device=%s)", weights, device)

    def detect(self, frames: list[np.ndarray]) -> list[list[list[int]]]:
        """对一批帧推理，返回每帧一组人脸框 [x1,y1,x2,y2]（原图像素坐标，int）。

        yolov8n-face.pt 的输出仍带 boxes（含关键点时忽略 keypoints，仅取 box）。
        """
        if not frames:
            return []
        results = self.model.predict(
            source=frames, device=self.device, conf=self.conf, verbose=False,
        )
        out: list[list[list[int]]] = []
        for r in results:
            boxes = []
            b = getattr(r, "boxes", None)
            if b is not None:
                # xyxy tensor [N,4]
                xyxy = getattr(b, "xyxy", None)
                if xyxy is not None and len(xyxy):
                    arr = xyxy.cpu().numpy().astype(int)
                    for row in arr:
                        boxes.append([int(row[0]), int(row[1]), int(row[2]), int(row[3])])
            out.append(boxes)
        return out


def _fill_polygon(mask: np.ndarray, poly: np.ndarray) -> None:
    """在 bool mask 上用多边形填充 True 区域（用 cv2）。"""
    import cv2
    pts = np.asarray(poly, dtype=np.int32).reshape(-1, 1, 2)
    cv2.fillPoly(mask.view(np.uint8), [pts], 1)  # view 成 uint8 才能 fillPoly
    # 重新解析 bool（原地）
    # 注意：view 共享内存，fillPoly 写入的就是 mask 的字节
