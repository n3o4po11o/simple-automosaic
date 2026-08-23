# Simple AutoMosaic

跨平台（macOS / Windows / Linux）视频人物自动打马赛克软件。**Rust 核心 + Flutter 桌面 UI**，本地推理（ONNX：CoreML / DirectML / OpenVINO / CPU），FFmpeg 硬件编解码（VideoToolbox / NVDEC·NVENC / QSV / VAAPI / AMF）。

> 完整设计文档：**[docs/DESIGN.md](docs/DESIGN.md)**（架构、模型选型调研、五档质量预设、UI 原型、里程碑）
> 旧版 Python 实现已归档至 [archived/](archived/)，其三段流水线逻辑被本项目继承。

## 许可

本项目以 **[AGPL-3.0-or-later](./LICENSE)** 开源分发。内置的 Ultralytics YOLO 系列权重
（含 YOLO11Face）适用 AGPL-3.0（Ultralytics 主张覆盖训练产出的权重），ffmpeg 预编译二进制
为 GPL 构建（子进程调用、随附文本）——本应用开源分发即满足上述许可对"嵌入产品分发"的要求。
完整第三方清单见 **[NOTICES.md](./NOTICES.md)**；闭源商用需另行购买 Ultralytics 企业许可或
更换宽松许可模型栈（见 DESIGN §5.7）。

## 当前状态

**M1 主体完成**：

- [x] 设计文档（含四轮深度调研：模型 / Rust 推理运行时 / FFmpeg 硬件加速 / Flutter 集成）
- [x] Cargo workspace：`crates/automosaic-core`（管线库）+ `crates/automosaic-cli`（CLI）
- [x] 媒体层：ffprobe 探测、hwaccel/编码器枚举、NV12 rawvideo 管道（硬解→硬编）
- [x] **推理层（M1）**：ort 2.0-rc.13（macOS CoreML EP，失败自动落 CPU）、NV12→letterbox 融合采样、
      YOLO-seg 解码/NMS/proto mask 后处理、NV12 平面 mosaic/blur/solid 合成
- [x] **精度层（M3）**：人脸模型（yolov8n-face ONNX）+ IoU 跟踪（漏检保持 12 帧"宁多打不漏"）
      + mask 时序平滑（上一帧膨胀 3px 并集，消闪烁）；CLI 开关 `--no-face/--no-track/--no-smooth`
- [x] **UI（M2）**：frb 2.12 集成 + 暗色 M3 三屏（拖入 → 预览调参 → 处理进度/取消），macOS 构建通过
- [x] **性能层**：批推理基础设施（固定 batch=4 双 session，规避 CoreML 动态批问题）
      + 隔帧检测 `--detect-every N`（跟踪保持 + 人脸框沿用补间隔）
- [x] **Linux 验证**（distrobox/Fedora 43 容器）：
      构建+19 测试全绿（零代码修改）；软解软编直通 129fps、VAAPI 解码路径 124fps、
      全功能打码（ort CPU 推理）19.6fps，遮盖效果确认
- [x] **内置 ffmpeg**：每平台自带硬件加速预编译版（macOS=osxexperts 9.0/VideoToolbox，
      Windows/Linux=BtbN GPL/NVENC·QSV·AMF·VAAPI+libx264 兜底）；
      `tool_path` 解析（env > exe 旁 > bin/<plat>/ > PATH）；
      编码器运行期回退（`-encoders` 列表存在 ≠ 可用，nvenc 失败自动降级 libx264）
- [x] 端到端验证：1080p HEVC → VideoToolbox 硬解 → CoreML 推理 → mosaic → 硬编，
      输出帧 person/face 检出均降为 0（遮盖完整）；所有配置下 b1/b4 输出仅 0.08% 像素差

## 算法调试工作流（`debug run` / `debug sweep`）

不开 app 完成「跑管线 → 看数据 → 调参数 → 对比」的闭环：

- `debug run`：OUT_DIR 产出
  - `report.json` — 逐帧记录：persons/faces 检出（框+分数）、活跃 track
    （id / lost 漏检保持）、mask 像素覆盖、单帧推理耗时；汇总含 fps、
    平均检出、遮盖率 %、漏检保持帧 %
  - `annotated/*.png` — 真实打码效果 + mask 绿罩 + 按 8 色循环着色的
    track 框 + 人脸白框（`--annotate-every N` 或 `--annotate-at "1.5,3.0"`）
  - `out.mp4` — 处理后视频
- `debug sweep`：`--sweep key=v1,v2`（可重复，笛卡尔积）自动运行全部组合，
  输出对比表（fps/persons/faces/cov%/held%）+ `sweep.json` / `sweep.csv`
- sweep 可用键：`conf / strength / detect-every / batch / style / device / face / track / smooth`

## 性能（M1 Max，release，1080p HEVC，全功能=人体+人脸+跟踪+平滑）

| 配置 | fps |
|---|---|
| 直通（硬解→硬编，无推理） | ~150 |
| 仅人体 mosaic | 32.5 |
| 全功能，逐帧 | 19.6 |
| 全功能 + `--detect-every 2` | 28.9 |
| 全功能 + `--detect-every 3` | 32.4 |

### 五档质量预设（`--preset` / App 内选择器）

| 预设 | 人体模型 | 隔帧 | 实测 fps（M1 Max, CoreML） |
|---|---|---|---|
| `speed` 速度 | yolo26n 检测框+margin（免 mask） | 3 | 28.8 |
| `balanced` 均衡（默认） | yolo26n-seg @640 | 2 | 17.3 |
| `accurate` 准确 | yolo26s-seg @960 | 1 | 8.4 |
| `extreme` 极致 | yolo26x-seg @1280 | 1 | 1.5（离线档） |
| `archive` 极限·档案级 | ensemble 全链（分析→复核→渲染两阶段） | YOLO26x@1536 + GD-tiny + SAM2.1 + RetinaFace 滑窗 + OSNet | 0.1-0.5fps（离线档案级） |

人脸线：yolo11n/s-face-pose（YOLO11Face，20 通道与 yolov8n-face 同布局）；
e2e 验证四档输出 person 检出全部降零。预设可被显式参数覆写（`--conf/--batch/...`）。

### 跟踪（ByteTrack 完整形态）

- **Kalman 预测**（匀速模型，噪声随框尺寸缩放）：关联基于预测框，快速移动不丢轨
- **低分框二次关联**（BYTE 核心）：遮挡/模糊时 detector 额外产出 `[0.1, conf)` 的
  低分检测参与二次救援——低分不建新轨（防误检起轨）
- **漏检保持**：`max_lost` 帧内保留 mask，按 KF 速度自适应膨胀补位移条带
- **人脸级联 ROI**（设置屏三态开关：跟随预设/开/关；极致档默认开，CLI `--face-roi`）：
  person 头部区域裁剪放大后二次跑人脸模型（小脸有效分辨率随裁剪比例放大），
  与全帧结果 IoU 去重 + 几何合理性过滤（宽高比 + 脸高≤人体高 50%）
  A/B（大脸片段）召回 +5%，吞吐 -27%——小脸/远景场景收益更大；
  **俯视等特殊视角建议关闭**（"顶部 30%=头部"假设失效会引入误检多打码）
- A/B（clip5s 单人物）：指标持平无回归；推理 +15%（低分检测解码 mask 的精度换算力）；
  收益集中在遮挡/多人/模糊等困难场景

### 推理设备（macOS，`--device` / App 内选择器）

| 设备 | CoreML 计算单元 | 均衡档实测（M1 Max） |
|---|---|---|
| `auto`（默认） | CPU/GPU/NPU 自动调度 | **17.1 fps** |
| `ane` | CPU + 神经网络引擎 | 16.4 fps |
| `gpu` | CPU + GPU | 13.4 fps |
| `cpu` | ONNX Runtime CPU | ~8 fps |

小模型上自动调度（大量落 NPU）最快且能效最优；`gpu` 适合个别算子在 ANE 不兼容
回落 CPU 的图。CoreML 编译缓存按设备分目录（`~/.cache/automosaic/coreml/<device>/`），
切换设备首次需重新编译（约数秒）。

### 打包（全模型单形态）

| 形态 | 构建命令 | 体积 | 说明 |
|---|---|---|---|
| **full**（发布唯一形态，2026-08-22 起） | `BUNDLE_MODELS=full flutter build macos --release` | ~810MB | 全部 16 个 ONNX 入包（含极致档），离线可用 |
| **download**（开发默认） | `flutter build macos --release` | ~145MB | 只带 manifest.json；Xcode 直接构建/开发迭代用（应用内模型下载已移除，2026-08-22） |


### 手动下载模型（CLI / 补缺）

桌面安装包已内置四档全模型（离线可用）；CLI 独立分发或补缺时手动下载放置：
从 [Releases · models](https://github.com/n3o4po11o/simple-automosaic/releases/tag/models)
取 `models-standard.zip`（四档全模型）与可选的 `models-m5.zip`（archive 档组件，
均另有 `.tar.zst` 版），**解压到以下目录**（两包解到同一处，目录内直接是
`manifest.json` + `*.onnx`）：

| 平台 | 模型目录 |
|---|---|
| Windows | `%APPDATA%\Simple AutoMosaic\models` |
| Linux | `~/.local/share/Simple AutoMosaic/models` |
| macOS | `~/Library/Application Support/Simple AutoMosaic/models` |

- 也可用 `AUTOMOSAIC_MODELS_DIR` 环境变量指向任意目录（优先级最高）。
- 放置后校验：`automosaic-cli models verify`（按 manifest SHA256 逐文件核对）。
- `models-m5` 一键包内容 = GDino/SAM2.1（tiny+large）/RetinaFace/OSNet + 1536 主检；
  Windows portable 用户解压到 `%LOCALAPPDATA%\SimpleAutoMosaic\app\models`。

### 发布流程（安装包 + CI）

| 产物 | 内容 | 触发 |
|---|---|---|
| `simple-automosaic-macos-arm64.zip` + `.sha256` | 全模型离线可用（ditto zip，2026-08-22 起单形态） | `git tag v0.x.0 && git push --tags`；或手动 Run workflow（默认只构建到 Artifacts，勾选 publish + 填 tag 才发 Release） |
| `simple-automosaic-linux-x86_64.AppImage` + `.sha256` | 全模型离线可用（ubuntu:20.04 容器构建，glibc 2.31 基线） | 同上 |
| `simple-automosaic-windows-x64.zip` / `-portable.exe` + `.sha256` | zip=无模型轻量包；portable 单文件四档模型内置（首次运行释放到 `%LOCALAPPDATA%\SimpleAutoMosaic\app`，之后秒级启动） | 同上 |

```bash
# 首次发布顺序：
# 0. 生成公开分支（隐私清洗：剔除 AGENTS.md/sync-remote.sh，硬失败检查；单 commit 无历史）
bash scripts/prepare-public.sh && git push origin public:main
# 1. 发布安装包
git tag v0.1.0 && git push --tags        # Actions → release → 三平台产物直挂 Release
# 或本地打包：scripts/package_macos.sh   # 产物在 dist/（含 sha256 边车）
```

- **CI**（push/PR）：Rust 单测（workspace + FFI crate，文件依赖用例无 models/ 时自动跳过）
  + Flutter 静态分析（pin 3.44.8）
- **Linux 构建**（CI：`ubuntu:20.04` 容器，glibc 2.31 基线；本地任意 x86_64 Linux 同）：
  `bash scripts/build-linux-appimage.sh`
  出全模型 AppImage（`simple-automosaic-linux-x86_64.AppImage`，四档模型 ~672MB + 运行库，
  产物名不带版本号——release tag 即版本）。前置：Flutter + `mpv-devel`
  （media_kit 构建需要）+ gtk3-devel；ffmpeg 与 linuxdeploy 脚本自动拉取。
  VAAPI 硬编实测：RX 9070 XT + Mesa 26 → h264_vaapi 全管线 16.6fps（vs libx264 11.4）；
  旧 Mesa（<26，如 Fedora 43 容器）自动回退 libx264。
- **Windows 构建**（CI：windows-latest；本地同）：`pwsh scripts/build_windows.ps1`
  出两产物——zip（app+ffmpeg+运行库，无模型）/ portable 单文件（四档模型内置，
  NSIS 静默自释放到 `%LOCALAPPDATA%\SimpleAutoMosaic\app`，版本标记跳过重复解压）。
  M5 不随包：models release 的 models-m5 一键包自行放置。**Windows 构建尚未真机验证**。
- `.pt → ONNX` 转换需要 Python + PyTorch + ultralytics 环境（约 2.5GB），无法在桌面应用内执行，
  由 CI 承担。`scripts/export_models.sh` 从公开源拉 .pt（ModelScope 主源：
  [Ultralytics/YOLO26](https://www.modelscope.cn/models/Ultralytics/YOLO26)、YOLO11、
  [akanametov/yolo-face](https://github.com/akanametov/yolo-face)、
  [zjykzj/YOLO11Face](https://github.com/zjykzj/YOLO11Face)，sha 已核对一致）。
- 未签名/公证前，用户首次打开需右键 → 打开。
- `scripts/export_models.sh` 拉取 YOLO26 源权重时自动回退 ModelScope 镜像（国内网络友好）
- 已验证并放弃 FP16 导出：体积减半、精度无损，但 CoreML 吞吐 -22%（详见 DESIGN §6）

⚠️ **匿名化说明（e2e 实测）**：mosaic/solid 可使检测器完全失认（person 检出→0）；
**blur 在任何实用半径下都不能**（radius 64 仍残留 0.52——CNN 对模糊鲁棒），
blur 仅是观感选项。需要匿名化请用 mosaic/solid。

注：batch=4 批推理本身无吞吐增益（瓶颈在逐帧 letterbox 预处理/后处理而非 session 提交），
真正的杠杆是隔帧检测 + 跟踪保持；批模型保留作为后续预处理并行化的基础。
- [x] 五档质量预设与模型管理（manifest + SHA 校验 + 设置屏）
- [ ] 待办：ByteTrack 完整形态、Windows 适配、打包签名；清退应用内模型下载遗留代码（UI/FFI，2026-08-22 决策移除）

## 开发

```bash
# Rust 核心 + CLI
cargo build --release
cargo test

# 抓取内置 ffmpeg（必须：含各平台硬件加速的预编译版，LGPL 变体无 H.264 软编兜底故选 GPL）
./scripts/fetch_ffmpeg.sh

# 端到端回归（68 项：probe/transcode/process 参数矩阵/遮盖有效性/A-B/边缘用例/错误路径）
.venv/bin/python scripts/e2e_test.py

# 算法调试（免 GUI，自动化迭代）
./target/release/automosaic-cli debug run -i tests/clip5s.mp4 -o /tmp/dbg \
    --device cpu --annotate-every 20
./target/release/automosaic-cli debug sweep -i tests/clip5s.mp4 -o /tmp/swp \
    --device cpu --sweep conf=0.25,0.35,0.45 --sweep face=1,0

# 首次需导出 ONNX 模型（复用 archived 的 .venv）
./scripts/export_models.sh
# 注：模型查找顺序 = AUTOMOSAIC_MODELS_DIR 环境变量 → 用户数据目录
#     （~/.local/share/Simple AutoMosaic/models，macOS 为 ~/Library/Application Support）
#     → 可执行祖先链 models/（仓库内 CLI 自动用仓库 models/）。
#     CLI 独立分发时用 AUTOMOSAIC_MODELS_DIR 指向模型集即可，无需拷入用户目录。

# CLI
./target/release/automosaic-cli probe tests/clip5s.mp4
./target/release/automosaic-cli hwaccel
./target/release/automosaic-cli models list     # 五档预所需模型清点（缺失/批变体/ensemble 组件）
./target/release/automosaic-cli models verify   # 按 manifest SHA256 校验模型完整性
./target/release/automosaic-cli transcode -i tests/clip5s.mp4 -o /tmp/out.mp4   # 直通冒烟
./target/release/automosaic-cli process -i tests/clip5s.mp4 -o /tmp/out.mp4 \
    --preset balanced                                              # 五档预设（推荐）
./target/release/automosaic-cli process -i tests/clip5s.mp4 -o /tmp/out.mp4 \
    --style mosaic --strength 35 --conf 0.35                        # 显式参数（等价）

# Flutter 应用（三平台桌面壳，frb 集成待 M2）
cd app && flutter run -d macos
```

## 目录

```
├── docs/DESIGN.md        # 设计文档（单一事实来源）
├── crates/
│   ├── automosaic-core/  # 纯管线库：媒体/检测/跟踪/合成/作业（无 Flutter 依赖）
│   └── automosaic-cli/   # CLI：调试与无头场景复用全部管线
├── app/                  # Flutter 桌面应用
├── archived/             # 旧版 Python 实现（快照，不再维护）
└── tests/                # 测试视频 fixture
```
