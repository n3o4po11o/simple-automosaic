# 第三方组件与模型声明（NOTICES）

本软件（AutoMosaic Studio）以 **GNU AGPL-3.0-or-later** 分发（见 [LICENSE](./LICENSE)）。
分发物中包含以下第三方组件与模型权重，各自适用其原始许可：

## 模型权重（models/，打包于应用 Resources/models/）

| 权重 | 来源 | 许可 |
|---|---|---|
| yolo11n-seg / yolo26n / yolo26n-seg / yolo26s-seg / yolo26x-seg | [ultralytics/assets](https://github.com/ultralytics/assets/releases)，经 `scripts/export_models.sh` 导出 ONNX（b1/b4 变体） | **AGPL-3.0**（Ultralytics 主张覆盖训练产出的权重；本应用开源分发以此合规） |
| yolov8n-face | derron/yolov8-face 社区权重（ultralytics 框架训练） | AGPL-3.0（同上，传导主张） |
| yolo11n-face-pose / yolo11s-face-pose | [zjykzj/YOLO11Face](https://github.com/zjykzj/YOLO11Face) v1.0.0 | 仓库声明 Apache-2.0；因基于 ultralytics 训练存在 AGPL 传导争议，按 AGPL 对待 |

> SHA256 清单见 `models/manifest.json`（`scripts/export_models.sh` 自动生成）。

## 可执行文件（应用 Resources/）

| 组件 | 来源 | 许可 |
|---|---|---|
| ffmpeg / ffprobe（macOS） | osxexperts ffmpeg 9.0 预编译（VideoToolbox） | **GPL-3.0**（文本见 [LICENSES/GPL-3.0.txt](./LICENSES/GPL-3.0.txt)；以独立子进程调用，未链接） |
| ffmpeg / ffprobe（Windows/Linux 打包） | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) GPL 静态版 | GPL-3.0 |
| Mpv.framework | 随 media_kit_libs_macos_video 分发 | LGPL-2.1-or-later（mpv） |

## Rust 依赖（crates/、app/rust/）

| 组件 | 许可 |
|---|---|
| ort（ONNX Runtime Rust 绑定）+ ONNX Runtime 二进制 | MIT（ORT 含官方遥测声明，见 <https://github.com/microsoft/onnx-runtime>） |
| ort 生态（ndarray 等）/ thiserror / serde / sha2 / clap | MIT 或 Apache-2.0 |
| flutter_rust_bridge | MIT |

## Flutter 依赖（app/）

| 组件 | 许可 |
|---|---|
| Flutter SDK / Dart | BSD-3-Clause |
| media_kit / media_kit_video / media_kit_native_event_loop | MIT |
| desktop_drop / file_selector / shared_preferences / freezed | MIT / BSD / Apache-2.0 |

## 合规要点

- 本应用与 Ultralytics AGPL 权重作为整体分发，故本应用自身以 AGPL-3.0 开源
  （对应 Ultralytics 许可 FAQ 对“嵌入产品分发”的要求）。
- ffmpeg 以子进程方式调用且不修改，分发其二进制时随附 GPL-3.0 文本与来源；
  如需获取对应源码，见上表来源仓库。
- ONNX Runtime 遥测：默认构建的 ORT 含使用遥测上报（可经环境变量禁用），
  详见官方文档；本应用不额外收集任何用户数据。
