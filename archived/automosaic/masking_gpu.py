"""GPU(MPS) 批量 mask 合成：可分离高斯卷积 + alpha 混合。

相比 CPU 版（masking.py），把高斯模糊和混合整体放到 GPU 上批量做，
单批 16 帧 1080p 约 134ms（CPU 包围盒版约 561ms，~4x 加速）。

接口：compose_batch(frames, body_masks, faces_per_frame, ...) -> list[np.ndarray]
  - frames: list[BGR ndarray]
  - body_masks: 每帧一个 bool ndarray(HxW) 或 None（与 CPU 版 build_mask 输入一致）
  - faces_per_frame: 每帧一组 [x1,y1,x2,y2]
  - 返回处理后的 BGR ndarray 列表
"""

from __future__ import annotations

import logging

import cv2
import numpy as np

log = logging.getLogger("automosaic")

MASK_COLOR = (0, 0, 0)  # BGR 纯黑（solid 模式）


def _gauss_kernel1d(k: int, sigma: float | None, device: str):
    """构造 1D 高斯核（与 OpenCV GaussianBlur 相同的 sigma 默认规则）。"""
    import torch
    if sigma is None or sigma <= 0:
        sigma = 0.3 * ((k - 1) * 0.5 - 1) + 0.8
    x = torch.arange(k, device=device).float() - (k - 1) / 2
    g = torch.exp(-(x * x) / (2 * sigma * sigma))
    return g / g.sum()


def _masks_to_tensor(body_masks, faces_per_frame, h: int, w: int, device, expand: int):
    """把每帧的 body mask(numpy) + 人脸框 聚合成一个 float tensor [B,1,H,W]。"""
    import torch
    out = torch.zeros((len(body_masks), 1, h, w), dtype=torch.float32, device=device)
    for i, (body, faces) in enumerate(zip(body_masks, faces_per_frame)):
        if body is not None and getattr(body, "any", lambda: False)():
            out[i, 0] = torch.from_numpy(body.astype(np.float32)).to(device)
        if faces:
            # face 数量少，逐框置 1（带 expand 外扩）
            for box in faces:
                x1, y1, x2, y2 = box
                x1 = max(0, int(x1) - expand)
                y1 = max(0, int(y1) - expand)
                x2 = min(w, int(x2) + expand)
                y2 = min(h, int(y2) + expand)
                if x2 > x1 and y2 > y1:
                    out[i, 0, y1:y2, x1:x2] = 1.0
    return out


def compose_batch(
    frames: list[np.ndarray],
    body_masks,
    faces_per_frame,
    *,
    style: str = "blur",
    blur_strength: int = 35,
    feather: int = 0,
    face_expand: int = 12,
    device: str = "mps",
):
    """批量 GPU 合成。返回处理后的 BGR ndarray 列表。

    - style='blur': 可分离高斯模糊 + mask 混合（背景清晰）
    - style='solid': mask 区域填黑
    - feather>0: mask 边缘高斯羽化
    """
    import torch
    if not frames:
        return []
    h, w = frames[0].shape[:2]
    B = len(frames)

    # 1) frame -> tensor [B,3,H,W] float32
    stack = np.stack(frames).astype(np.float32)            # [B,H,W,3]
    ft = torch.from_numpy(stack).to(device)
    ft = ft.permute(0, 3, 1, 2).contiguous()                # [B,3,H,W]

    # 2) mask -> tensor [B,1,H,W]
    mask = _masks_to_tensor(body_masks, faces_per_frame, h, w, device, face_expand)

    # 3) feather: 对 mask 做小核高斯模糊
    if feather and feather > 0:
        fk = max(3, feather * 2 + 1)
        g1 = _gauss_kernel1d(fk, None, device)
        gh = g1.view(1, 1, 1, fk)
        gv = g1.view(1, 1, fk, 1)
        p = fk // 2
        tmp = torch.nn.functional.conv2d(mask, gh, padding=(0, p))
        mask = torch.nn.functional.conv2d(tmp, gv, padding=(p, 0))

    # 4) 合成
    if style == "blur":
        k = max(3, int(blur_strength) | 1)
        g1 = _gauss_kernel1d(k, None, device)
        gh = g1.view(1, 1, 1, k).expand(3, -1, -1, -1)      # groups=3
        gv = g1.view(1, 1, k, 1).expand(3, -1, -1, -1)
        p = k // 2
        tmp = torch.nn.functional.conv2d(ft, gh, padding=(0, p), groups=3)
        blurred = torch.nn.functional.conv2d(tmp, gv, padding=(p, 0), groups=3)
        out = ft * (1 - mask) + blurred * mask
    else:  # solid
        color = torch.tensor(MASK_COLOR, dtype=torch.float32, device=device
                             ).view(1, 3, 1, 1)
        out = ft * (1 - mask) + color * mask

    # 5) tensor -> numpy [B,H,W,3] uint8
    out = out.permute(0, 2, 3, 1).contiguous().clamp(0, 255).to(torch.uint8)
    arr = out.cpu().numpy()
    return [arr[i] for i in range(B)]
