"""遮罩合成：人体轮廓 ∪ 人脸框 → 二值 mask → 应用到帧。

mask_style:
- 'blur' (默认): 对整帧高斯模糊，再用 mask 把"模糊版"贴回原图对应区域。
                 人物区域轻微模糊、背景保持清晰。
- 'solid':       在 mask 区域填纯色（默认黑色）。
feather>0: 对 mask 边缘羽化，使过渡更自然（可选）。
"""

from __future__ import annotations

import cv2
import numpy as np

MASK_COLOR = (0, 0, 0)  # BGR 纯黑（solid 模式用）


def expand_box(box: list[int], expand: int, h: int, w: int) -> tuple[int, int, int, int]:
    """人脸框四周外扩 expand 像素，并裁剪到画面内。"""
    x1, y1, x2, y2 = box
    x1 = max(0, x1 - expand)
    y1 = max(0, y1 - expand)
    x2 = min(w, x2 + expand)
    y2 = min(h, y2 + expand)
    return x1, y1, x2, y2


def build_mask(
    h: int, w: int,
    body: np.ndarray | None,
    faces: list[list[int]] | None,
    face_expand: int = 12,
) -> np.ndarray:
    """合并人体 mask 与人脸框，返回 bool mask（HxW）。"""
    mask = np.zeros((h, w), dtype=np.uint8)
    if body is not None and body.any():
        mask = np.where(body, 1, mask).astype(np.uint8)
    if faces:
        for box in faces:
            x1, y1, x2, y2 = expand_box(box, face_expand, h, w)
            mask[y1:y2, x1:x2] = 1
    return mask.astype(bool)


def apply_mask(
    frame: np.ndarray,
    mask: np.ndarray,
    *,
    style: str = "blur",
    blur_strength: int = 35,
    feather: int = 0,
) -> np.ndarray:
    """在帧上应用遮罩，返回新帧（不修改原图）。

    性能关键：只处理 mask 的包围盒区域（而非 1080p 全帧），
    在多人大场景下可加速数倍。

    - style='blur': 人物区域贴回高斯模糊版，背景清晰（轻微模糊）。
    - style='solid': mask 区域填 MASK_COLOR。
    - feather>0: 对 mask 边缘羽化（生成软 alpha）后混合。
    """
    if not mask.any():
        return frame  # 无需处理

    h, w = frame.shape[:2]
    # 算 mask 包围盒，加 padding（blur 核大小，避免子区域边缘可见接缝）
    ys, xs = np.where(mask)
    x1, x2 = int(xs.min()), int(xs.max())
    y1, y2 = int(ys.min()), int(ys.max())
    pad = max(int(blur_strength) if style == "blur" else (feather or 0), 0)
    x1 = max(0, x1 - pad); x2 = min(w, x2 + pad)
    y1 = max(0, y1 - pad); y2 = min(h, y2 + pad)
    if x2 <= x1 or y2 <= y1:
        return frame

    sub = frame[y1:y2, x1:x2]
    sub_mask = mask[y1:y2, x1:x2]

    # 软边缘 alpha（feather=0 时为硬边 0/1）
    if feather and feather > 0:
        alpha = (sub_mask.astype(np.float32) * 255.0)
        k = max(3, feather * 2 + 1)
        alpha = cv2.GaussianBlur(alpha, (k, k), 0)
        a = alpha[..., None] / 255.0
    else:
        a = sub_mask[..., None].astype(np.float32)  # 硬边

    if style == "blur":
        k = max(3, int(blur_strength) | 1)  # 高斯核必须正奇数
        blurred = cv2.GaussianBlur(sub, (k, k), 0)
        blended = (sub.astype(np.float32) * (1 - a) +
                   blurred.astype(np.float32) * a).astype(np.uint8)
    else:  # solid
        overlay = np.full_like(sub, MASK_COLOR, dtype=np.float32)
        blended = (sub.astype(np.float32) * (1 - a) + overlay * a).astype(np.uint8)

    out = frame.copy()
    out[y1:y2, x1:x2] = blended
    return out
