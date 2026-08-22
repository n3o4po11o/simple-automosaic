# automosaic（已归档：Python v0 版本）

> **此目录为旧版 Python 实现的快照存档**，仅供参考，不再维护。新的 Rust + Flutter 跨平台版本正在根目录开发中，设计文档见 `docs/DESIGN.md`。本版的已验证逻辑（三段流水线、队列解耦、有序重组、遮罩合成策略）已被新设计继承。

在 Apple Silicon（M1 Max 等）上通过 **GPU（MPS）** 加速，对视频中的人脸与人体打**轻微模糊遮罩**。人体遮罩贴合身形（YOLO 实例分割轮廓），人脸使用专用 YOLO-face 模型补强。采用 **decode→infer→encode 三段流水线并行**，边解帧边推理边编码。

## 工作原理

```
[decode] ffmpeg VideoToolbox 硬解 ──MJpeg pipe──► [infer] YOLO-seg(person)+YOLO-face MPS批推理+模糊合成 ──► [encode] ffmpeg VideoToolbox 硬编
                  │                                                      │                                                              │
                  ▼                                                      ▼                                                              ▼
            队列(max32) ◄────────────────────────────────────────── 队列(max32) ◄──────────────────────────────────────────────────── ffmpeg stdin → output.mp4
                                            （三线程并行，无中间落盘文件）
```

- **人体**：`yolo11n-seg.pt`（COCO `person` 类实例分割）→ 输出人体轮廓 mask，贴合身形。
- **人脸**：`yolov8n-face.pt`（专用）→ 人脸框，带小幅膨胀，覆盖特写/仰拍/部分入镜场景。
- **遮罩（默认 `blur`）**：人体多边形 ∪ 人脸框 → 二值 mask → 对该区域做高斯模糊（背景保持清晰，人物轻微模糊）；可选 `solid` 纯黑遮罩。
- **流水线并行**：解码（VideoToolbox 硬解）、推理（MPS）、编码（VideoToolbox 硬编）三段同时进行，吞吐≈三者最慢一段，而非三者之和。中间帧走 MJpeg 管道（JPG 质量 95），不落盘。

## 安装

```bash
# 需要 Python 3.11（系统默认 3.14 跑不了 torch，故用 uv 指定 3.11）
uv venv --python 3.11 .venv
source .venv/bin/activate          # 或用 uv run 跳过激活
uv pip install -r requirements.txt

# 下载权重（首次）
./scripts/download_face_model.sh
# yolo11n-seg.pt 会在首次运行时由 ultralytics 自动下载
```

## 使用

```bash
python -m automosaic --input in.mp4 --output out.mp4
```

常用参数：

| 参数 | 默认 | 说明 |
|------|------|------|
| `--input` | 必填 | 输入视频 |
| `--output` | 必填 | 输出视频 |
| `--device` | `mps` | `mps` / `cpu` |
| `--batch` | `16` | 推理批大小 |
| `--conf` | `0.35` | 置信度阈值 |
| `--face-expand` | `12` | 人脸框外扩像素 |
| `--feather` | `0` | 遮罩边缘羽化像素（0=硬边） |
| `--mode` | `streaming` | `streaming`(三段并行,默认) / `batch`(全量解帧串行,兜底) |
| `--mask-style` | `blur` | `blur`(轻微模糊,默认) / `solid`(纯黑遮罩) |
| `--blur-strength` | `35` | 模糊核大小（仅 blur 样式，越大越模糊） |
| `--compose-device` | `cpu` | mask 合成设备：`cpu`=多线程(默认,推荐) / `gpu`=MPS批量(与推理抢GPU) |
| `--workers` | `4` | CPU mask 合成并行线程数（仅 --compose-device=cpu） |
| `--body/--no-body` | on | 是否检测人体 |
| `--face/--no-face` | on | 是否检测人脸 |
| `--keep-frames` | off | 保留中间帧目录（默认清理） |
| `--hwaccel/--no-hwaccel` | on | macOS VideoToolbox 硬解/硬编（mac 推荐） |
| `--codec` | `h264_videotoolbox` | 输出编码器（设 `libx264` 用软编） |
| `--crf` | `18` | libx264 质量 |
| `--q` | `65` | VideoToolbox 质量 |

示例：

```bash
# 默认：人体+人脸 纯黑遮罩，MPS 加速
python -m automosaic -i in.mp4 -o out.mp4

# 只打马赛克在脸上，关闭人体
python -m automosaic -i in.mp4 -o out.mp4 --no-body

# 用 CPU（无 GPU 环境调试）
python -m automosaic -i in.mp4 -o out.mp4 --device cpu
```

## 性能

在 M1 Max（32 核 GPU）上，`yolo11n-seg` + `yolov8n-face` 默认走 MPS。瓶颈通常在 PNG 解帧/编码与磁盘 IO，而非推理。可通过增大 `--batch` 提升吞吐。

## 设计：可替换模型

`automosaic/detectors.py` 定义统一接口，换模型只需实现：

```python
class BodyDetector:
    def detect(self, frames: list[np.ndarray]) -> list[np.ndarray]:  # 返回每帧的 person mask
class FaceDetector:
    def detect(self, frames: list[np.ndarray]) -> list[list[list[int]]]:  # 返回每帧的人脸框
```

例如把 YOLO 换成 MediaPipe Pose/Face，只需替换这两个类的内部实现。

## v1 范围外（后续可扩展）

- 流式解码（不全量落盘解帧）
- 多进程拆分 / 跨帧跟踪（ByteTrack/BoT-SORT）做时序平滑
- Web UI
- 马赛克/模糊等其他遮罩样式（当前仅纯色黑）

## 目录

```
automosaic/
├── automosaic/
│   ├── cli.py          # 命令行入口
│   ├── ffmpeg_io.py    # 解帧 / 重编码
│   ├── detectors.py    # YOLO 封装（可替换）
│   ├── masking.py      # 遮罩合成
│   └── pipeline.py     # 批推理管线
└── scripts/
    └── download_face_model.sh
```
