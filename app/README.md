# Simple AutoMosaic — 桌面应用（Flutter）

跨平台桌面壳（macOS / Windows / Linux），核心管线在 [rust/](rust/)（flutter_rust_bridge 2.x FFI，
实现见仓库根 `crates/automosaic-core`）。六屏流程：拖入 → 配置 → 处理 → 队列 → 复核 → 设置。

## 开发

```bash
flutter pub get
flutter run -d macos          # 或 -d linux / -d windows
flutter analyze
```

FFI 接口改动后重新生成桥接代码：

```bash
cd rust && flutter_rust_bridge_codegen generate
```

Linux 构建需 `mpv-devel`（media_kit 的 CMake 用 pkg-config 找 mpv）；
内置 ffmpeg 由根目录 `scripts/fetch_ffmpeg.sh` 拉取，打包脚本随产物分发。
