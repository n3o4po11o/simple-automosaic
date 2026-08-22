# AutoMosaic Studio — 跨平台视频人物自动打马赛克软件设计文档

> 技术栈：Rust（核心管线）+ Flutter（桌面 UI）｜推理：ONNX（CoreML / DirectML / OpenVINO / Vulkan(实验)）｜编解码：FFmpeg 硬件加速（VideoToolbox / NVDEC·NVENC / QSV / VAAPI / AMF）
>
> 平台：macOS 13+（Apple Silicon 优先）/ Windows 10+ / Linux（x86_64 主流发行版）
>
> 设计日期：2026-08　｜　基于本仓库 Python 版 automosaic 的已验证逻辑重构

---

## 0. 决策总览（TL;DR）

| 决策点 | 结论 | 一句话理由 |
|---|---|---|
| 进程架构 | Flutter UI 进程内嵌 Rust cdylib（flutter_rust_bridge v2），FFmpeg 以**子进程**运行 | ffmpeg 天然崩溃隔离；frb StreamSink 提供类型安全的进度流；帧数据零拷贝回传 |
| 推理运行时 | **ort（ONNX Runtime Rust 绑定）为主**，tract 纯 Rust 兜底 | 唯一同时覆盖 CoreML(macOS) / DirectML(Windows) / OpenVINO(Intel) / CPU 的生产级方案 |
| "Metal + Vulkan + OpenVINO" 落地 | 推理：macOS=CoreML EP(Metal/ANE)、Windows=DirectML、Intel=OpenVINO EP(可选)、Linux=WebGPU EP(Vulkan，auto 默认/失败落 CPU)；**遮罩合成=wgpu compute（真正跑 Metal/Vulkan/DX12）** | ORT **没有原生 Vulkan EP**（2026-08 现状，调研确认）；合成走 wgpu 可真跨三平台 GPU |
| 默认模型（均衡档） | 人体：YOLO26n-seg（或 yolo11n-seg）｜人脸：YOLO11Face n-pose（带 5 点 landmark）｜跟踪：ByteTrack | n 档 mask AP 33.9 vs yolo11n-seg 30.0，参数更少；人脸 Hard 集 81→85 升级路径 |
| 质量分档 | 五档预设：速度 / 均衡 / 准确 / 极致（YOLO26x-seg @1280）/**极限·档案级**（多检测器 ensemble + WBF 融合 + SAM2.1-large 逐帧精修 + 滑窗人脸 + 人工复核 UI） | 极限档 accuracy-first，0.1-0.5fps 可接受；采用「分析→复核→渲染」两阶段架构（§5.6） |
| 管道帧格式 | **NV12 rawvideo** 双向管道（替换现版 MJPEG） | 消除 JPEG 编解码 CPU 开销与画质损失；NV12 是所有硬件编解码器原生格式；1080p30 单向仅 93 MB/s |
| 管线拓扑 | 继承 Python 版三段流水线：decode → infer(+track) → compose → encode，有界队列 + 哨兵 + 有序重组 | 已在 M1 Max 验证 51fps（Python）；Rust 下预期 2-3× |
| 精度核心杠杆 | ① 跟踪 + 漏检补偿（OC-SORT/ByteTrack）② 隔帧检测+插值 ③ mask 时序并集/EMA ④ imgsz 960/1280 ⑤ FP16 | 前 3 项零推理成本，观感提升最大 |
| UI | Flutter 3.4x + Material 3 自定义暗色主题 + window_manager 自绘标题栏 + Riverpod 3 | 工具类应用暗色优先；向导式「拖入→预览调参→处理→完成」 |
| 进度协议 | Rust `PipelineEvent` 枚举 → frb StreamSink → Dart StreamProvider | 阶段/帧数/fps/ETA/日志全部结构化、编译期类型安全 |
| 分发 | macOS: dmg+notarize｜Windows: MSIX（ffmpeg 用 BtbN **LGPL** shared 构建）｜Linux: AppImage（优先系统 ffmpeg） | 规避 GPL；模型运行时下载到应用数据目录 + SHA 校验 |

---

## 0.5 实现进度（2026-08-20 更新·三；2026-08-21 补录 H 组 + 批量实现十项：trait 插件化/DirectML/OpenVINO(feature)/OCR+ORU/自适应降档/MJPEG/JobManager/YuNet/自绘标题栏/双主题多语言；**2026-08-21 晚：M5 极限·档案级档全量落地**——WBF/ensemble/SAM2.1/RetinaFace 滑窗/OSNet 外观关联/复核 UI；**2026-08-22：Linux 客户端收口**——WebGPU auto 默认（9070XT 物理机真机验证）/自绘窗口控件/设备选项平台化/CLI $ORIGIN rpath）

**已实现**（30+ 提交，macOS 全链路 + Linux 全链路，测试基线：**120 core 单测**（2026-08-21 M5 后；原 82）+ 2 集成测试（取消收尾/冒烟探测）+ 2 FFI 单测 + e2e release/debug 双轮）：

| 模块 | 内容 |
|---|---|
| 媒体层 | ffprobe 已移除：探测改 ffmpeg `-i` stderr 解析（少内置 52MB 二进制，**音轨编码全量枚举**）；NV12 rawvideo 双向管道；hwaccel/编码器枚举 + 平台候选链（**GPU vendor 重排**：sysfs PCI id 枚举，N 卡 cuda/nvenc 优先）+ **启动期真实流冒烟探测**（候选 hwaccel 各解码 1s 到 `-f null`，设备/驱动级硬失败启动期剔除，结果按 流规格 进程内缓存——不再白付首次任务失败）；**编码器运行期回退**（`-encoders` 在列 ≠ 可用，如容器内无 libcuda 的 nvenc → 自动降级 libx264）；**多音轨/字幕/章节/容器元数据全量保留**（`-map 1:a?` 全轨；mp4/mov 字幕 mov_text、mkv 直接 copy；`-map_chapters 1 -map_metadata 1` 取自源文件——旧版仅取第一条音轨）；**码率档位缩放**（`--bitrate auto`=默认：长边 ≤720p/1080p/1440p/4K+ → 3/6/10/20M）；**h264_amf 质量参数臂**（原落空）；**videotoolbox `-realtime 1`** 降编码功耗 | §3.2/3.3/3.4 |
| 内置 ffmpeg | 三平台预编译 GPL 版入包（macOS=osxexperts 9.0/VideoToolbox，Win/Linux=BtbN/NVENC·QSV·AMF·VAAPI+libx264）；`tool_path` 资源解析（env → exe 祖先链 10 级含 .app Resources → PATH）；**打包单形态 full**（2026-08-22 起，download 安装包移除）：`BUNDLE_MODELS=full` 全部模型入包 ~810MB；`download`（仅 manifest）保留为 Xcode 直接构建的开发默认；~~应用内模型下载~~（**2026-08-22 决策移除**：产物单形态全模型入包后无下载需求；UI/FFI 遗留代码待清退——原 ureq 流式 + SHA256 + 主源/镜像回退链路）；导出脚本拉 .pt 权重自动回退 ModelScope；许可文件（LICENSE/NOTICES/GPL 文本）随包分发 |
| 推理层 | ort 2.0-rc.13，macOS CoreML EP + **持久编译缓存（按设备分目录，模型加载 8.2s→0.6s）**；**计算单元可选**（auto=全单元 / gpu=CPU+GPU / ane=CPU+NPU / cpu，`MLComputeUnits` 透传）；NV12→letterbox 单遍融合采样；**双布局解码**：yolo11 anchor `[N,116,8400]`（NMS）与 yolo26 e2e `[N,300,38]`（免 NMS，xyxy+score+class+coeffs）按输出形状自动识别；**可变 imgsz**（输入/proto 尺寸从 session 探测，640/960/1280 全通）；固定 batch=4 双 session 批推理（批 session 继承设备选择）；**翻转 TTA**（2026-08-20：letterbox 源列镜像采样零拷贝翻转趟 + 框/mask 帧坐标镜像回映射 + 两趟按分数贪心 NMS 合并；极致档默认开，CLI --tta/--no-tta，debug sweep 键 tta；FFI 经预设参数自动继承）；**WebGPU EP**（2026-08-20：Linux x86_64 Dawn/Vulkan 实验灰度，--device webgpu；RADV 实测 accurate 档 +29%、输出与 CPU 逐比特一致；2026-08-21：auto 默认启用，与 macOS/Windows auto 语义对齐，cpu 显式退出） |
| 精度层 | **ByteTrack 完整形态**：匀速 Kalman（2×2 分维、噪声随框尺寸缩放）预测框关联 + 低分框二次关联（`BYTE_LOW_CONF=0.1`，Detector `low_conf` 产出低分检测；遮挡/模糊救援、低分不起轨）+ 速度自适应保持帧膨胀（速度来自 KF 状态）；**人脸级联 ROI**（`detect_boxes_roi`：person 头部裁剪放大二次推理 + `merge_faces` 去重 + `filter_implausible_faces` 几何过滤[宽高比 0.5..1.8 + 脸高≤人体高 50%——俯视视角下"顶部 30%=头部"假设失效时的误检防线]；**用户三态开关**（UI 设置/FFI `face_roi: u8` 0=跟随预设[极致档开] 1=开 2=关，俯视建议关）+ CLI `--face-roi`；A/B 大脸片段召回 +5%，代价 ~-27% 吞吐）；人脸模型（yolo11n/s-face-pose）+ person 关联过滤 gate_faces；mask 时序平滑；**丢失 track 遮罩渐隐**（保持期后半段 fade 线性 0→1，按框尺寸腐蚀回缩——人物离场不硬切；重新匹配即复位）；**GMC 全局运动补偿**（2026-08-20：相位相关逐帧估计相机平移，KF 预测框平移关联 + 保持帧 mask 位移跟随，峰值显著性门限防静机位噪声；--gmc/UI 开关/sweep 键 gmc）；**残影修复**（2026-08-20 Linux 实测反馈：① 漏检保持的冻结 mask 改为按 KF 速度逐帧外推平移——码跟着人走而非原地冻结；② 贴画框边缘的丢失 track 3 帧快速衰减（离场启发式，画面内部丢失仍 12 帧防遮挡）——合成"人物移出画面"片 A/B 验证：残影段 12 帧 ~80k px 冻结 → 5 帧内归零） |
> **2026-08-20 用户决策：全档逐帧检测**（速度/均衡原为隔 3/2 帧；隔帧的时序滞后是拖影根源——两轮残影修复后仍余留观感问题，吞吐换画质。隔帧机制保留为高级覆写：CLI `--detect-every` / UI 滑杆）。下表 fps 为隔帧时代实测，逐帧后吞吐约减半。
> **2026-08-21 Linux 客户端实测验证通过**：AppImage 重打包（含残影修复×2 + 全档逐帧）后用户确认拖影问题消除。

| **五档质量预设（新）** | core `preset.rs`：速度（yolo26n 检测框+margin）/ 均衡（yolo26n-seg）/ 准确（yolo26s-seg@960）/ 极致（yolo26x-seg@1280 + 级联 ROI + **翻转 TTA**）/ 极限（M5 占位报错），**全部逐帧检测**；CLI `--preset`（显式参数覆写）；隔帧时代 M1 Max 实测 28.8/17.3/8.4/1.5fps，输出 person 检出全降零；**中间档 yolo26m-seg@960 / yolo26l-seg@1280 已导出入 manifest**（2026-08-20，无预设消费，--model 显式选用） |
| **模型管理（新）** | `models/manifest.json`（file/batch_file/imgsz/sha256/size，`scripts/export_models.sh` 自动生成）；core `models.rs` 清单解析 + 搜索根路径解析（仓库/.app Resources）+ SHA256 流式校验；FFI `list_models()/verify_model()` |
| **共用变换（新）** | core `mosaic.rs`：检测→gate→跟踪→平滑→合成单实现（此前 CLI/FFI 两份且 FFI 缺隔帧/保持膨胀/gate），CLI 与 FFI 统一接入；预览对照帧经 `PreviewSink` 回调从 core 输出 |
| UI（M2 主体） | frb 2.12；media_kit 播放器 + 修改后画面所见即所得（**导入后不自动播放**，用户手动起播）；处理页分阶段计数 + 左右对照预览；**预设选择器（可用性标识 + conf 联动）**；**推理设备四选**（自动/GPU/NPU/CPU，透传 CoreML 计算单元）；**多任务顺序队列**（QueueController：逐个执行/取消当前/移除待办/清除已结束，**待办持久化到 shared_preferences（JSON），重启恢复继续跑**——原设计提议 hive，为免新增依赖改用现役方案）；**设置屏**（人脸开关/级联 ROI 三态/**「增强选项」组四开关：ByteTrack 跟踪/mask 时序平滑/per-ID EMA/landmark 外扩（全部持久化，CLI 对应 --no-track/--no-smooth/--no-mask-ema/--no-landmark-expand，debug sweep 键 track/smooth/ema/landmark）**/完成后打开文件/高级参数：隔帧/批/人脸外扩覆写/模型管理 + SHA 校验）；**「视频参数」组（2026-08-20）**：编码器（平台候选链下拉）/目标码率（auto=档位缩放或固定值）/输出容器（MP4/MKV——MKV 字幕原样保留）；**「加速器」组（2026-08-20）**：list_backends() 数据驱动（自动/GPU/NPU/CPU + 描述与可用标识）；**增强选项组补翻转 TTA 三态开关**（自动=跟随预设/开/关）；**处理屏阶段行**（ProcessEvent::StageEnter：探测→推理→收尾）；**预览检测框叠加**（2026-08-20：person 绿框/人脸橙框/双眼点 CustomPainter + 开关，preview_frame 直接检测首帧语义与流式一致）；**日志面板**（ProcessEvent::Log）；**blur 非匿名化警示**；**拖入屏「最近:」文件栏**（去重置顶 8 条，chip 可删，点击直开）；shared_preferences 持久化；lib 拆为 5 文件（main/config/process/settings/queue+prefs）；**产品显示名 Simple AutoMosaic**（CFBundleName/CFBundleDisplayName/标题/dmg 卷名），文件名与进程名 simple-automosaic；**设备选项平台化（2026-08-22）**：gpu/ane（CoreML 计算单元）仅 macOS 显示，Linux=自动/CPU/WebGPU（auto 默认 WebGPU），描述文案按平台措辞（不出现跨平台无关的 CoreML 字样）、非 macOS 历史遗留 gpu/ane 值归一 auto |
| **UI 打磨（2026-08-21 会话·预览/处理/队列三屏用户实测迭代）** | **播放器**：去 media_kit 内置控件（其进度条/播放键/音量与应用控制条重复），控制条补**音量滑杆**（ValueNotifier 跨屏共享 + 持久化 + 静音记忆）与**全屏按钮**——窗口内全屏（视频铺满应用窗口的黑底路由 + 底部渐变控制栏：播放/进度/时间/音量/退出；空格播放暂停、Esc/双击退出、player.state 快照做初值、进入不自动播放；曾实现 windowManager 原生真全屏，实测撕裂后按用户决策回退）；**处理屏**：任务信息结构化——新 FFI 事件 `ProcessEvent::JobMeta`（预设/模型/人脸/检测节奏/分辨率/**解码 hwaccel/编码器**/后端/模型加载耗时，编码器回退随重发更新）3×3 图标药丸网格展示，纯文本日志退役；预览对改 Expanded 弹性占位一屏看全（去滚动），队列列表移出；**队列独立成屏**（QueueScreen：任务列表 + 运行中迷你进度卡 + 处理详情互跳；首页队列按钮改指，处理屏顶栏留入口）；**调参屏**：增强选项组由设置屏迁入（导入视频后按片调整、随任务入队）；右栏紧凑化（区块标题 13px、分段按钮紧凑样式 11px + shrinkWrap + showSelectedIcon:false）；**输出目录双态按钮**（系统目录选择器/原目录输出，输出路径实时联动）；「模型与后端」改 PresetDetail 结构化明细行；**设置屏**：编码器/码率下拉改圆角药丸菜单；**参数悬停通俗说明**（2026-08-21 晚补）：三组参数面板（调参屏增强选项、设置屏「处理」「高级参数」）每个选项悬停 300ms 弹"为什么开/有什么好处/什么情况才关"结构的通俗解释（17 条，双语），气泡限宽 320px | §7.2/7.3/7.5 |
| 格式兼容（2026-08-20） | 旋转元数据（probe 解析 display-matrix ±90 交换宽高，输出为显示方向）；TrueHD 等 mp4 不兼容音轨预检自动转 AAC；10-bit HEVC / ProRes 422 HQ / 4K60 实测通过——全部固化为 e2e 用例 |
| 增强开关化 | **全部增强项用户可控**（2026-08-21 起位于**调参屏「增强选项」组**——导入视频后按片调整：ByteTrack 跟踪/mask 平滑/per-ID EMA/landmark 外扩/GMC/翻转 TTA 三态；设置屏保留级联 ROI 三态、人脸开关、隔帧/批/外扩滑杆）——无隐式强制项；CLI 对应 --no-* flag，debug sweep 全键覆盖 |
| **两阶段 analyze/render（M5 骨架，2026-08-20）** | core `maskstore`（逐帧 RLE 原子落盘 [tmp+rename+fsync]、meta.json 校验、断点续跑起点=最大帧号+1）+ `MosaicPipeline`（mask 组装状态机从流式 build 抽出共用：检测/gate/跟踪/平滑/GMC 一套逻辑）+ CLI `analyze`/`render`（渲染段纯合成无推理，样式/强度可改）；e2e：两阶段 vs 流式逐帧等价（absdiff=0）+ 续跑幂等。复核 UI（关键帧刷子）与 ensemble/SAM2.1/RetinaFace 属 M5 后续 | §5.6/§2.1 |
| 调试/工程化 | `debug run/sweep`（逐帧报告 + 标注帧 + 参数扫描，键含 track/smooth/ema/landmark）；`scripts/e2e_test.py` release+debug 双轮 106 项（含格式兼容矩阵）；Linux distrobox 构建验证 |
| 关键修复史 | **M5 会话（2026-08-21）**：App 复核入口埋进只渲染运行中任务的死代码路径（完成态 UI 从未上屏——复核屏从未可见的直接原因）；archive 档输出命名 tag 漏项致 `_mosaic_null.mp4`（Dart null 插值）；**Debug 构建 Rust 零优化致分析慢 ~8×**（cargokit 仅非 Debug 配置传 `--release`——日常用 Release 包）；档案分析预览漏接 PairSink；进度终拍缺失 + total 估算多 1 的"差 N 帧完成"观感；CoreML EP 跑 SAM2.1-large 每帧上下文泄漏（见 B 组）。人脸 NMS 跨坐标系 bug；MaskSmoother 累积；App Sandbox 必须关闭；debug_assert 只对真实前置条件断言；抽帧位置钳制；**导出脚本 b1/b4 同路径覆盖 bug（人脸模型 .pt 在 models/ 内时 b1 被覆写）**；**打包 .app 黑屏 = 插件在 runApp 前访问平台通道未初始化 binding（SharedPreferences.getInstance 需 WidgetsFlutterBinding.ensureInitialized）**；**载入期时间显示 00:59/00:06 = mpv 上报负位置 + Dart `%` 欧几里得取模（-1%60=59），修复=流层忽略负值 + 格式化防御**；**旋转元数据视频输出内容错乱 = ffmpeg 对 rawvideo 管道自动旋转而 probe 未交换宽高（帧按错误行宽解释），修复=解析 display-matrix 并交换**；**M3 SegmentedButton 选中段默认前导✓图标挤裁标签（紧凑宽度下"自动"裁成"自"，showSelectedIcon:false）**；**VisualDensity 每单位 ±4px——水平 -3 密度叠加 8px 内边距变负值把文字挤出**；**透明窗口（backgroundColor: transparent）在原生全屏失去同步呈现路径致视频撕裂（已改不透明窗口）**；**CallbackShortcuts 需 Focus(autofocus) 锚定焦点链否则 Esc 无人消费** |

**未实现清单（2026-08-20 全文对账审计，按类别；H 组为 2026-08-21 代码对账补录）**：

**A. 平台与分发**
| 项 | 说明 | 设计出处 |
|---|---|---|
| Windows 全链路 | 真机构建、media_kit_libs_windows_video、MSIX；h264_amf 目前无专属质量参数分支（落空臂） | §8 |
| Linux release 未进 CI | release.yml 仅 macOS job；build-linux-appimage.sh 已备未接入；deb 包未做 | §8 |
| 自动更新 | auto_updater（Sparkle/WinSparkle）+ Linux 免进 | §8 |
| ~~签名公证~~ | 不适用（无 Apple 开发者账号），分发说明右键打开 | §8 |

**B. M5 极限·档案级档（✅ 2026-08-21 全量落地；实测 clip5s 75 帧 @0.15fps——准确落进设计的 0.1-0.5fps 区间）**
| 项 | 状态 |
|---|---|
| ~~YOLO26x@1536 + Grounding DINO ensemble → WBF 融合~~ | ✅ core `wbf.rs`（加权框融合：双确认全分/单路稀释、votes 确认度、同源 NMS 去重——单测 6 项含权重偏置/去重边界）+ `gdino.rs`（GD-tiny fp16：800² letterbox + pixel_mask + "person." BERT 预计算分词 [101,2711,1012,102]，logits 词位 sigmoid 解码，归一化框直映射原帧；真实帧回归锚点 ±40px）+ `archive.rs` 主线（双检测器并行 → WBF(0.55, 召回优先 votes=2 或低分保留) → SAM 精修）。GD-base 自导出按设计降级链用 tiny（transformers 工具链不在位） | §5.6 |
| ~~SAM2.1-large 逐帧 box-prompting 精修~~ | ✅ `sam2.rs`（**⚠️ macOS 上 SAM 恒走 CPU EP**（`cfg` 门控，Linux 正常透传 device——RDNA/WebGPU 待实测）：2026-08-21 实测 SAM2.1-large 走 CoreML EP（auto/gpu 均然）存在每帧上下文泄漏[stderr 刷 "Context leak detected"]+ 病理性慢[首帧 ~35 分钟]，内存随帧单调膨胀至 swap 写满盘；CPU EP 实测稳定、RSS 走平——CoreML EP 对该大 transformer 的实现问题，tiny 109MB 无恙。**实测速度（CLI release）**：large ≈7.8s/帧、tiny ≈1.9s/帧（设置屏「极限档 SAM 精修模型」large/tiny 可选，任务快照；mask 缓存与规格绑定），vietanhdev ONNX：encoder 1024² ResizeLongestSide+右下零填充+图外 mean/std 归一化（双变体实测确认）→ image_embed+双 high_res_feats；decoder **num_labels 维批量**——一帧 N 框一次 decoder 调用，multimask 3 候选取 argmax-IoU；box=双角点 label 2/3 编码；低分辨率 256→帧坐标最近邻映射）。SAM IoU<0.5 回退链：YOLO proto mask → 框+margin。点提示 API（复核用）同源 | §5.6 |
| ~~RetinaFace-R50 滑窗人脸 + 多尺度~~ | ✅ `retinaface.rs`（**资产为 yakhyo retinaface_r34.onnx**：biubug6 MIT 移植、动态尺寸、原理解码——归一化锚框 ceil 特征图 + 方差 0.1/0.2 + BGR-104/117/123 预处理，全部 2026-08-21 真实帧双图验证；**R50 官方权重仅 Google Drive 分发、ternaus 镜像实测偏弱（conf 0.46 vs r34 的 0.998），R34 为可得最优**，R50 待可靠镜像后同接口替换）。滑窗：原生 1280² tile 25% overlap（小脸）+ 半尺度全帧（大脸）+ 跨 tile NMS(0.4)；宿主 person 框内的脸 landmark 外扩并入该 masklet；孤立小脸补框喂 SAM2 精修为独立实例 | §5.6 |
| ~~BoT-SORT 外观关联~~ | ✅ `reid.rs`（OSNet-x0.25 MSMT17 ONNX，BoT-SORT 同源 512 维嵌入，固定批 16 复用取首行）+ masklet greedy 关联（0.6·IoU + 0.4·外观余弦，阈值 0.30，丢失容忍 8 帧注销；**档案级不做漏检编造**——SAM2 传播仅校验的设计决策） | §5.3/§5.6 |
| ~~复核 UI（关键帧刷子）~~ | ✅ maskstore 实例层（`frame_*.inst`：id/kind/score/框/逐实例 RLE）+ 补丁层（`patches.bin`：add/erase 逐帧原子落盘，渲染段纯合成应用）+ FFI review 系列（review_frame/meta/sam_prompt（同帧嵌入缓存，迭代加点秒回）/save_brush/clear_frame）+ App **ReviewScreen**（时间轴导航/红色叠加预览/笔刷加擦/点提示 SAM 重提示绿预览→差分 materialize 成 add/erase 补丁/实例框 id 标注/一键渲染）+ `archive_render`（FFI 渲染入口）。CLI 侧 render 自动应用补丁 | §5.6 |
| ~~masklet 缓存落盘/断点续跑/两阶段任务流~~ | ✅ 2026-08-20 骨架 + **2026-08-21 修复续跑索引错位**（管线无 seek 恒从帧 0 流送——曾把帧 0 的 mask 写到 start 处：帧号与内容错位 + 白付全片推理；修复=transform 按流内绝对位置跳过已分析帧，续跑 0.8s 秒回 vs 修复前全量 8 分钟；原 balanced analyze 同 bug 同修）。App 集成：archive 预设"开始分析（两阶段）"→ 队列 archiveAnalyze → 完成态"复核"入口 → ReviewScreen → 渲染出片 | §5.6/§2.1 |
| 体验与输出处理 | **分析段 `-f null` 空输出**（2026-08-21：旧实现真编码写探针视频后删除——长片 GB 级白写；media `drain_null_cmd` + pipe 识别 encoder="null"，CLI `--drain null|file` 默认 null，App 恒 null）；**档案级分析对照预览**（2026-08-21：archive transform 补接 PreviewPair——每 4 个已分析帧按用户样式合成一对，0.1fps 下 ~40s 一对；曾因漏接 PairSink 导致处理屏永远"等待预览帧"）；**App 收口**（处理屏空闲态显示最近已结束任务卡片——曾因只渲染运行中任务致完成态 UI（复核/渲染入口）不可达；「渲染输出」直渲主按钮；JobMeta 解码器=真实 hwaccel、编码器=无（-f null）/临时；进度终拍 + Finished 真实帧数回填——曾显示 74/76 即"完成"[末拍滞后 + total 估算多 1]） | §5.6 |
| 模型资产 | 8 个新 manifest 条目（yolo26x-seg-1536 本地导出 252MB / GD-tiny fp16 360MB / SAM2.1 large 829MB+tiny 120MB / retinaface-r34 85MB / osnet 0.9MB）+ `scripts/fetch_m5_models.sh`（下载/解压/导出/SHA 校验一键）+ models.rs `direct_url`（HF 单文件直链，下载名≠本地名场景） | §8 |

**C. 精度增强（M3 尾巴）**
| 项 | 说明 | 出处 |
|---|---|---|
| ~~OC-SORT / BoT-SORT GMC~~ | ✅ 2026-08-20 完成：相位相关全局位移估计（128² 下采样 Y + 2D FFT 互功率谱 + 亚像素峰值，峰值显著性门限防静机位噪声）+ KF 预测框平移关联 + 漏检保持帧 mask 累积位移跟随；CLI --gmc / UI 开关 / sweep 键 gmc | §5.3 |
| ~~TTA 翻转增强~~ | ✅ 2026-08-20 完成：letterbox 镜像采样翻转趟 + 结果镜像合并 NMS；极致档默认开（--tta/--no-tta 覆写，sweep 键 tta，UI 三态开关已接） | §6 |
| ~~丢失 track 遮罩 0.5s 渐隐~~ | ✅ 2026-08-20 完成：保持期后半段 fade 线性 0→1 + 按框尺寸腐蚀回缩（erode_region），重新匹配即复位 | §6 |
| ~~yolo26 m/l 中间档模型导出~~ | ✅ 2026-08-20 完成：m-seg@960（90.4MB）/ l-seg@1280（107.5MB）+ b4 批变体入 manifest，tiny 片实测 2.1/1.4fps | §5.1 |
| ~~速度档 YuNet 人脸~~ | ✅ 2026-08-21 完成：OpenCV Zoo 2023mar（MIT，232KB）入 manifest（直连上游源下载）；detect.rs 人脸模型双布局自动识别（YuNet 12 输出 = cls/obj/bbox/kps × stride 8/16/32）；解码与 OpenCV FaceDetectorYN 官方实现数值对照一致（分数/框/五点逐位）——预处理为**原始 BGR 0..255 + 直接拉伸**（实测 letterbox+零填充使该帧分数 0.51→0.23，模型对训练分布敏感）；速度档默认接入（缺失回退 yolo11n-face-pose），真帧回归测试固化 | §5.2 |
| ~~OC-SORT 观测中心重更新~~ | ✅ 2026-08-21 完成：OCR 关联救援（两段 IoU 失败后用**最后观测框**与剩余高分检测再试——漏检期间 KF 按陈旧速度把预测框跑远的关联兜底）+ ORU 重更新（回滚到最后观测、以 (z−last)/gap 差分速度重放匀速假设，防"丢得越久恢复后速度污染越重"→拖影）；`--no-ocru` / sweep 键 ocru；**BoT-SORT 外观关联仍属 M5**（需 ReID 模型资产） | §5.3 |

**D. 性能与效率**
| 项 | 说明 | 出处 |
|---|---|---|
| wgpu GPU 合成（ComposeWgpu） | **2026-08-21 评估后缓议**：`ComposeBackend` trait 与 `mosaic::build_with_composer` 注入点已落地（mock 测试覆盖）；但帧数据在系统内存（NV12 管道）时 GPU 需上传+回写 ~6.2MB@1080p（≥1ms）——CPU 实测全帧 5.96ms、实际 mask 覆盖 <20% 画面即 ~1.2ms，往返不偿失；唯 4K+多人有净收益，但彼时瓶颈在推理（1-5fps）合成耗时是噪声。结论：与 v2 进程内零拷贝（hwframes 常驻 GPU）联动实施才有意义 | §4.3 |
| ~~compose CPU SIMD~~ | ✅ 2026-08-20 实测收口：标量块求和已被 LLVM 自动向量化（1080p 全帧 5.96ms，基准固化为 bench_pixelate_1080p）；手写 NEON vpadalq 实测全帧 +15% 但实际 mask 只覆盖人物区域（<20% 画面），真实收益 <0.2ms/帧——不引入，复杂度不值 | §6 |
| ~~自适应批/隔帧~~ | ✅ 2026-08-21 完成：core `AdaptiveTuner`——2s 决策窗，推理确为瓶颈（busy 占比 >0.7，排除解码/编码拖累误判）且吞吐 <0.85×fps 连续两窗才动作；降档阶梯 = 先撤批 session → detect_every 逐级 +1 至上限，只升不降防振荡；CLI `--adaptive`（**默认关**：全档逐帧是 2026-08-20 用户决策，自适应为低配机器显式 opt-in）；单测覆盖阶梯/防误触/禁用三态 | §6 |
| ~~大模型运行期自适应降档~~ | ✅ 2026-08-21 完成（与上同一调节器）：撤批 = 第一档（对应"batch 8→2"语义，当前预设批=4），隔帧上限 3；`DetectorBackend::try_reduce_batch` trait 钩子 | §6.8 |
| ~~videotoolbox `-realtime 1` 降功耗参数~~ | ✅ 2026-08-20 完成（encode_cmd videotoolbox 臂） | §6 |
| ffmpeg 进程内零拷贝（rusty_ffmpeg hwframes） | 4K60+ 场景，v2 路线 | §3.1 |
| INT8（仅 backbone/neck） | proto 系数量化会劣化 mask 边缘 | §6 |

**E. 推理后端**
| 项 | 说明 | 出处 |
|---|---|---|
| DirectML（Windows） | ◐ 2026-08-21 代码完成：cargo 按平台启用 `directml` feature + `commit_session` Windows 臂（auto 默认即走 DirectML，EP 失败 recover 落 CPU）+ backend_desc + FFI 枚举条目；**真机验证待 Windows 适配**（与 §A Windows 全链路同批） | §4.2 |
| OpenVINO EP（Intel） | ◐ 2026-08-21 代码完成但**编译期 feature 门控**（默认关）：pyke 预编译 ORT 运行时无 "openvino+webgpu" 组合构建（Linux 服务器实测，链接期拒绝下载）——`--features ort-openvino` 启用（自链 libonnxruntime 或上游补齐后），`--device openvino` 语义已就绪 | §4.2 |
| ~~WebGPU EP（Vulkan 灰度）~~ | ✅ 2026-08-20 完成（Linux x86_64 feature "webgpu"，Dawn/Vulkan 显式指定）：RX 9070 XT/RADV 容器实测——speed(n@640) 15.6 vs CPU 16.2fps（打平略慢）、balanced(n-seg) 持平、**accurate(s-seg@960) 10.6 vs 8.2fps（+29%，模型越大优势越大）**；输出与 CPU **逐比特一致**（PSNR=inf）。CLI --device webgpu + UI 设备选项（仅 Linux 显示）；2026-08-21 起 auto 默认即 WebGPU（极限档 SAM2.1-large CPU ~7.8s/帧，默认 CPU 不再合理；EP 失败自动落 CPU 不变）；2026-08-22 9070XT 物理机真机验证通过（auto=WebGPU：accurate 档 14.3 vs 4.1fps ≈ 2.9×）。⚠️ 打包需带 libwebgpu_dawn.so（ort-sys 下载于 target/，AppImage 闭包收集时注意） | §4.2 |
| tract 纯 Rust 兜底 | 未做（ORT 加载失败仍直接报错）。2026-08-21 评估：yolo26 e2e 头含 topk/gather 等算子，tract 兼容性存疑且 pyke 预编译运行时随包分发使"dylib 加载失败"场景实际罕见——优先级下调，待真出现分发故障再投入 | §4.2 |
| ~~DetectorBackend/ComposeBackend trait 抽象~~ | ✅ 2026-08-21 完成：`DetectorBackend`（含 `try_reduce_batch` 降档钩子）/`FaceDetectorBackend`/`ComposeBackend` 三 trait，mosaic 管线全面改走 dyn 对象（mock 后端单测验证可插拔），`build_with_composer` 合成注入点；未来 tract/ncnn/wgpu 后端零管线改动接入；~~list_backends() API~~ ✅ 2026-08-20 | §4.3 |

**F. UI/UX 细节**
| 项 | 说明 | 出处 |
|---|---|---|
| ~~window_manager 自绘标题栏/记住窗口状态/跟随系统亮暗~~ | ✅ 2026-08-21 完成：`TitleBarStyle.hidden` + `DraggableAppBar`（全屏接入，macOS 交通灯自动让位）；窗口几何（位置/尺寸/最大化）持久化到 shared_preferences（移动/缩放去抖 600ms + 关闭时保存），启动恢复；themeMode 跟随系统；**2026-08-22 Linux 收口**：hidden 在 GTK 上连装饰（min/max/close）一并移除而 macOS 保留交通灯 → `WindowControls` 自绘三键挂 DraggableAppBar 尾部（仅 Linux；最大化图标随窗口事件切换；close 走 setPreventClose→几何保存→destroy 链路），物理机真机验证通过 | §7.1 |
| ~~版本号展示与生成机制~~ | ✅ 2026-08-22：单一事实源 = `app/pubspec.yaml` version 字段；`scripts/version.sh`（show / bump patch·minor·major，同步 workspace Cargo 版本）；CLI `--version` 构建期自 pubspec 注入；设置屏「关于」卡片经 package_info_plus 展示版本 | §7.1 |
| ~~CLI 模型管理~~ | ✅ 2026-08-22：`models list`（五档要件清点——body/人脸回退链/批变体可选/archive ensemble 组件，缺什么一目了然）+ `models verify`（manifest SHA256 校验，与 GUI 设置屏对齐）；`AUTOMOSAIC_MODELS_DIR` 环境变量最高优先（对齐 AUTOMOSAIC_FFMPEG_DIR 模式，独立部署指向模型集）；`--preset` 文案补 archive 两阶段指引 | §5.6/§8 |
| ~~亮色主题/多语言~~ | ✅ 2026-08-21 全量收口：Material 3 亮/暗双主题全 App 生效；`S.t(zh, en)` 轻量双语 + **`S.rust()` Rust 数据文案映射垫片**（预设名/后端描述/"软件解码"标注等数据字段的英译表，未命中原样兜底——不改 Rust 接口）；**四屏全部界面文案英文化**（调参/处理/队列/设置，含悬停说明与 Rust 数据标签逐一清点，语言名"中文"按惯例保留母语）；外观组（主题/语言）即时切换 | §7.1 |
| ~~设置屏「外观」组~~ | ✅ 2026-08-21 完成（语言/主题入口，见上） | §7.3 |
| ~~设置屏「视频参数」组~~ | ✅ 2026-08-20 完成：编码器（平台候选链下拉）/码率（auto 档位缩放或固定值）/输出容器（MP4/MKV） | §7.3 |
| ~~预览叠加层~~ | ✅ 2026-08-20 完成：preview_frame 返回 person 框/人脸框/双眼点（FFI PreviewBox + codegen），预览画面 CustomPainter 叠加（绿 person 框 + 橙人脸框 + 眼点）+「显示检测框」开关；置信度拖动重绘已有（350ms 防抖重推理） | §7.5 |
| ~~最近文件列表~~ | ✅ 2026-08-20 完成：拖入屏「最近:」chip 栏（去重置顶 8 条，可删） | §7.3 |
| ~~队列持久化~~ | ✅ 2026-08-20 完成：待办任务 JSON 持久化到 shared_preferences，重启恢复继续跑（原设计提议 hive，改用现役依赖免新增） | §7.1 |
| ~~Stage/StageEnter 结构化进度事件~~ | ✅ 2026-08-20 完成：ProcessEvent::StageEnter（Probing/Inferring/Finalizing——流式管线为任务级状态机边界）+ 处理屏阶段行；分阶段计数器（提取/处理/编码）并存 | §7.4 |
| ~~core job.rs JobManager~~ | ✅ 2026-08-21 完成：`core::job` 状态机（Queued→Running→Done/Failed/Cancelled，单向 + 幂等回写防线；Queued 直接取消 / Running 经取消标志由执行方回写）+ 单测；CLI `queue` 子命令为首个消费面（多视频串行批处理、单作业失败不中断后续）；FFI 迁移 Dart 队列时可复用 | §2.2 |

**G. 编解码细节（2026-08-20 审计发现；7 项中 5 项当日完成）**
| 项 | 说明 | 出处 |
|---|---|---|
| ~~取消时 ffmpeg 优雅收尾~~ | ✅ 2026-08-20 完成：写侧察觉取消即关编码器 stdin → 编码器 EOF 正常收尾写 moov（半成品可播，集成测试覆盖；rawvideo 数据管道上 ffmpeg 不读 'q' 命令，EOF 即等价优雅停机），3s 超时才 kill | §7.4 |
| ~~解码侧启动期冒烟探测~~ | ✅ 2026-08-20 完成：候选 hwaccel 对真实流各解码 1s 到 `-f null`，硬失败启动期剔除，结果按 流规格 进程内缓存 | §3.3 |
| ~~GPU vendor 枚举排序~~ | ✅ 2026-08-20 完成：Linux 读 `/sys/class/drm/card*/device/vendor`（PCI id 十六进制解析），macOS 恒 Apple；候选链按 vendor 重排（N 卡 cuda/nvenc 优先、Win 的 I 卡 qsv/A 卡 amf 优先、混布保守默认）；`hwaccel` 子命令展示。AMD 真机验证（曾因十进制 parse bug 恒未识别，回归测试固化）；Windows DXGI 枚举随 Windows 适配一并做 | §3.3 |
| ~~h264_amf 质量参数分支~~ | ✅ 2026-08-20 完成：`-quality balanced -b:v`（amf 系编码器全覆盖） | §3.4 |
| ~~码率随分辨率档位缩放~~ | ✅ 2026-08-20 完成：`--bitrate auto`（默认）按长边分档 3/6/10/20M，显式值透传 | §3.4 |
| ~~多音轨/字幕/章节保留~~ | ✅ 2026-08-20 完成：`-map 1:a?` 全轨 + 字幕（mp4/mov→mov_text，mkv→copy）+ `-map_chapters 1 -map_metadata 1`；TrueHD 预检改为全轨任一命中（旧版只看首轨会漏判） | §3.2 |
| ~~MJPEG 低配管道选项~~ | ✅ 2026-08-21 完成：`--pipe mjpeg`（解码侧 `-f mjpeg -q:v 2`，带宽 ~1/20 起）；读侧 `JpegFrameScanner` 增量定界（SOI/EOI 扫描跨块安全，熵编码段 0xFF 必跟填充/RST）+ jpeg-decoder → `rgb_to_nv12`（BT.601 limited，往返单测 ≤6）→ 帧下游与 NV12 完全同构；macOS/Linux 双端全管线冒烟通过；两阶段 analyze/render 保持 NV12（e2e 等价性基线不动） | §3.2 |

**H. 功能对账补录（2026-08-21 代码核实：设计正文有要求、A–G 审计遗漏项）**

| 项 | 说明 | 出处 |
|---|---|---|
| 保留指定人脸 | 预览调参屏「遮住他人时保留指定人脸」复选框（人脸排除/白名单）——全库无 keep-out/exclude/whitelist 实现 | §7.3 |
| 羽化（feather）参数 | Python 版 `--feather` 继承项（遮罩边缘羽化滑杆）——crates/app 全库无 feather | 附A |
| batch 全量落盘兜底路径 | §1 约定 Python 版 batch 模式「保留为 `--fallback` 路径」——CLI 子命令仅 Probe/Hwaccel/Transcode/Analyze/Render/Debug/Process，无 batch 模式（现有 `run_with_encoder_fallback` 是编码器回退，另一回事） | §1/附A |
| 「实时试算此段」+ 预估 fps/时长 | 预览调参屏的片段试算按钮与「预估 51 fps · 全片约 2 分 10 秒」行——config_screen 无试算/预估逻辑 | §7.3 |
| 低置信人脸兜底 | face conf 低于阈值但落在 person 头部区（pose 验证）时仍打码——现有低分救援仅 ByteTrack 框级（BYTE_LOW_CONF=0.1），无人脸版 | §6 |
| AV1 默认 dav1d 软解 + 提示 | §3.3 注：探测到 AV1 默认软解并提示（VideoToolbox AV1 不可靠）——全 core 无 AV1 特判 | §3.3 |
| 设置屏散项 | 「自导入 ONNX+输入规格」「跟踪器参数」「日志级别」三个设置项——settings_screen 均无 | §7.3 |

**开发节奏（2026-08-19 定）**：mac 端优先完成全部功能/性能开发（M3 剩余、预处理并行化、M5……），之后再统一验证 Linux。Windows 在 mac 收口后评估。

**实测结论修正**：blur 在任何实用半径下都不能使检测器失认（radius 64 残留 0.52——CNN 对模糊鲁棒），blur 仅为观感选项，匿名化需用 mosaic/solid（§6 补充）。

## 1. 从现有 Python 版继承什么

Python 版（本仓库）已验证的架构资产，全部保留并升级：

| Python 版现状 | Rust 版设计 | 变化 |
|---|---|---|
| 三段流水线 streaming.py：decode→infer→compose→encode 四线程 + 三队列(32) + 哨兵错误传播 | 同拓扑，tokio task / std::thread + `crossbeam` 有界通道 | ✅ 继承，错误传播改为 `CancellationToken` + `broadcast` 事件 |
| MJPEG pipe（JPEG q95，CPU 编解码 2 次） | **NV12 rawvideo pipe** | 🔧 升级：画质无损、省 CPU；带宽 1080p30 单向 93MB/s，4K30 373MB/s 可行 |
| yolo11n-seg(person) + yolov8n-face 双模型并行 | YOLO26n-seg + YOLO11Face(n-pose, 带 5 点) | 🔧 升级模型 + **新增跟踪层** |
| mask 合成：CPU 包围盒 Gaussian（51fps）/ MPS 批量可分离 Gaussian | CPU SIMD 包围盒版（默认）+ **wgpu compute 批量版**（可选） | 🔧 wgpu = Metal/Vulkan/DX12 真跨平台 GPU |
| 无跟踪，逐帧独立检测（闪烁、漏检无补偿） | ByteTrack/OC-SORT + 时序平滑 + 隔帧检测插值 | ✨ 新增，最大精度杠杆 |
| `-hwaccel videotoolbox` 固定 | 启动期**硬件探测 + 平台回退链** | 🔧 全平台 hwaccel |
| 无 UI、日志即进度 | Flutter 完整 GUI：预览、调参、进度、队列 | ✨ 新增 |
| batch 模式（全量落盘兜底） | 保留为 `--fallback` 路径 | ✅ 继承 |

---

## 2. 总体架构

### 2.1 进程与线程拓扑

```
┌─────────────────────────── Flutter UI 进程 ────────────────────────────┐
│  Dart(Riverpod) ◄──Stream<PipelineEvent>── flutter_rust_bridge v2     │
│      │                        ▲                                        │
│      │ FFI 调用（作业管理/参数/取消）   Rust cdylib（libautomosaic_core）  │
│      ▼                        │                                        │
└──────┬────────────────────────┼────────────────────────────────────────┘
       │ spawn/pipe             │
       ▼                        ▼
┌─ ffmpeg(硬解) ─┐   ┌──────────────── Rust 核心线程池 ────────────────┐   ┌─ ffmpeg(硬编) ─┐
│ -hwaccel …     │   │ [probe]  ffprobe → 元数据/总帧数/音轨           │   │ -c:v h264_…    │
│ -f rawvideo    ├──►│ [decode-reader] 读 NV12 管道 → 有界队列(32)     │   │ -f rawvideo    │
│ -pix_fmt nv12  │   │ [infer]  攒批(8)→预处理→ort→后处理→跟踪 → 队列   ├──►│ -pix_fmt nv12  │
└────────────────┘   │ [compose] mask 合成（CPU SIMD / wgpu） → 队列   │   │ + 音轨 copy    │
                     │ [encode-writer] 有序重组 → 写编码管道 stdin      │   └────────────────┘
                     │ [events]  PipelineEvent → StreamSink(broadcast) │
                     └─────────────────────────────────────────────────┘
```

要点：

- **ffmpeg 双子进程**（解码侧/编码侧）与 Python 版一致：任一 ffmpeg 崩溃仅终止当前作业，UI 存活；通过 stderr 尾部 + 退出码归因。
- Rust 核心**同时编译为 lib（frb 用）与 bin（CLI 用）**：CLI 形态直接复用全部管线（调试、CI、无头服务器），这是架构上的逃生舱。
- 有序重组：compose 输出按帧号重排后写入编码 stdin（继承 `_encode_loop` 的 pending/next_idx 逻辑）。
- 取消：UI → `cancel_job(id)` → `CancellationToken` → 各 stage 退出 → 编码器 stdin 关闭（EOF）优雅收尾写 moov（3s 超时后 kill）。注：rawvideo 数据管道上 ffmpeg 不读 `q` 交互命令，EOF 是等价的优雅停机信号（2026-08-20 实测定案）。
- **两种执行模式**：① 流式（1-4 档默认）：上图三段流水线一次跑完；② **两阶段离线（极限·档案级档，§5.6，✅ 2026-08-21）**：分析（ensemble 逐帧推理 → masklet 实例 RLE 缓存落盘，按帧断点可续跑）→ 复核（UI 关键帧刷子/点提示 SAM 修补，补丁逐帧 materialize 落盘）→ 渲染（纯合成+编码，无推理，自动应用补丁）。同一套 compose/encode 代码复用，推理段与渲染段解耦。

### 2.2 Cargo workspace 结构

```
automosaic/
├── crates/
│   ├── automosaic-core/        # 纯管线库，不依赖 Flutter：CLI/测试可直接用
│   │   ├── src/
│   │   │   ├── pipeline/       #   三段流水线、通道、哨兵、有序重组
│   │   │   ├── media/          #   ffmpeg 子进程封装：probe/decode/encode/hwaccel 探测
│   │   │   ├── detect/         #   DetectorBackend trait + ort 实现 + 预处理/后处理/NMS
│   │   │   ├── track/          #   ByteTrack / OC-SORT（自实现或包 mot-rs）
│   │   │   ├── compose/        #   ComposeBackend trait + CPU(SIMD) / wgpu 实现
│   │   │   ├── smooth/         #   时序平滑：mask 并集/EMA、漏检补偿
│   │   │   ├── job.rs          #   JobManager：队列、状态机、事件总线
│   │   │   └── events.rs       #   PipelineEvent / Stage / JobError 枚举
│   │   └── tests/              #   集成测试：小视频端到端 + 黄金帧比对
│   ├── automosaic-ffi/         # frb 胶水层：api/ 模块暴露给 Dart（薄）
│   └── automosaic-cli/         # bin：python -m automosaic 的等价 CLI
├── app/                        # Flutter 应用
│   ├── lib/
│   │   ├── features/           # home(拖放) / preview(调参) / process(进度) / queue / settings
│   │   ├── rust/               # frb 生成绑定
│   │   └── core/               # 主题、Riverpod providers
│   └── …
├── models/                     # ONNX 权重清单（manifest.json：url+sha256+输入规格）
└── scripts/                    # 模型导出（python ultralytics export）、ffmpeg 构建脚本
```

核心原则：**frb 胶水层只做翻译**，所有逻辑在 `automosaic-core`，可独立测试、可 CLI 化。

---

## 3. 视频管线：FFmpeg 硬件加速（调研结论）

### 3.1 集成方式选型

| 方案 | 结论 |
|---|---|
| **ffmpeg-sidecar（子进程管道）** | ✅ **v1 采用**。活跃（2.5.2，2026-05）、零 unsafe、崩溃隔离、任何 `-hwaccel` 参数即插即用、LGPL 分发最省心。不足（pix_fmt 常量、异步）自行封装百行内解决，或直接用 `std::process`/tokio 自管 |
| rusty_ffmpeg / rsmpeg（进程内 libav） | ⏳ v2 性能路线。rusty_ffmpeg 跟版 FFmpeg 8.1、全量 hwaccel API；可做 hwframes 零拷贝，但 unsafe 面大。**用 trait 把 decode/encode 边界抽象，两条路线可共存** |
| ffmpeg-next | ❌ 维护模式，safe 层不暴露 `hw_device_ctx`（Discussion #120），硬解必须钻 sys 层，无优势 |
| GStreamer | ❌ 重依赖、插件管理/打包复杂；其「自动选 hwaccel」思想被本设计的探测回退链吸收 |

### 3.2 帧格式：NV12 rawvideo 双向管道

```
解码侧 stdout:
  ffmpeg -hwaccel <auto> -i IN -f rawvideo -pix_fmt nv12 -        (帧由 ffmpeg 内部从 GPU 下载)

编码侧 stdin:
  ffmpeg -y -f rawvideo -pix_fmt nv12 -s {W}x{H} -r {fps} -i - \
         [-i IN -map 0:v:0 -map 1:a:0? -c:a copy] \
         -c:v <hw-encoder> <质量参数> -movflags +faststart OUT
```

- 带宽：1080p30 双向 187 MB/s、4K30 双向 746 MB/s——OS 管道吞吐（1-6 GB/s）内可行；4K60+ 再考虑进程内零拷贝路线。
- NV12 是 VideoToolbox/NVENC/QSV/VAAPI 共同原生格式，**进出编码器零像素格式转换**。
- RGB 转换（推理需要）只在**待推理的 letterbox 子图**上做（CPU SIMD 或 wgpu shader），背景像素不经过任何转换。
- 为什么弃用 MJPEG（现 Python 方案）：每帧 2 次 JPEG 编解码 CPU 开销 + q95 有损（马赛克边缘可观测）；仅保留为低配设备可选项。

### 3.3 各平台 hwaccel 矩阵与回退链

**启动期探测**（结果缓存，设置页可视化）：
1. ffprobe → 编码格式/profile/pix_fmt/帧数（`nb_frames` 缺失时 `-count_frames` 估计或按时长×fps）；
2. `ffmpeg -hwaccels` / `-encoders` → 编译期支持；
3. 对每个候选 `-init_hw_device` + 1-2s 真实流冒烟解码（`-f null -`）→ 驱动/硬件真实可用性；
4. GPU vendor 枚举辅助排序（Linux: `lspci`/`/sys/class/drm`；Windows: DXGI；macOS: `system_profiler`）。

**回退链**（`-hwaccel` 框架本身支持"流不支持硬件→自动软解"，但设备初始化失败会直接报错，故需进程级重试）：

```
macOS:          videotoolbox → 软解
Win + NVIDIA:   cuda(nvdec) → d3d11va → 软解        编码: h264_nvenc → libx264(或openh264)
Win + Intel:    qsv → d3d11va → 软解                 编码: h264_qsv → libx264
Win + AMD:      d3d11va → 软解                       编码: h264_amf → libx264
Linux + Intel:  vaapi(/dev/dri/renderD128) → 软解    编码: h264_vaapi → libx264
Linux + AMD:    vaapi → 软解                          编码: h264_vaapi → libx264
Linux + NVIDIA: cuda → 软解                           编码: h264_nvenc → libx264
```

注：AV1 解码在 macOS VideoToolbox 上仍不可靠（M3/M4 才有硬件，ffmpeg patch 未全部落地）——探测到 AV1 时默认 dav1d 软解并提示。

注：**AMD RDNA4（RX 9070 系）的 H.264/HEVC 硬件编码依赖 Mesa 版本**（2026-08 物理机实测）：Mesa 26.0+（vainfo 显示 H264/HEVC EncSlice 全开）正常，**Mesa 25.3（Fedora 43 容器）未暴露** H264/HEVC 编码入口（仅 AV1 EncSlice）——容器内开发测试会误判"无 H264 硬编"。Linux 编码链 `h264_vaapi → h264_nvenc → libx264` 在物理机直接命中 h264_vaapi（均衡档 16.6fps vs libx264 11.4fps，+46%）；旧 Mesa 用户自动运行期回退 libx264。AppImage 运行时 libva 随包、VA 驱动用宿主（与图形驱动配套），跨小版本加载宿主驱动实测可用。`--encoder av1_vaapi` 仍是 RDNA4 的备选硬编出口。

### 3.4 硬件编码器质量参数（内置预设）

| 编码器 | 推荐参数 | 备注 |
|---|---|---|
| `h264_videotoolbox` | `-b:v 6M -maxrate 7M -bufsize 12M -realtime 1 -allow_sw 1 -tag:v avc1` | `-q:v` 行为不稳，用码率模式更可靠；`allow_sw` 兜底软编；码率随分辨率档位缩放 |
| `h264_nvenc` / `hevc_nvenc` | `-preset p4 -rc vbr -cq 23 -b:v 0`（离线档 `p6/p7 -cq 20 -tune hq`） | **必须 `-b:v 0`** 否则默认 2M 封顶 |
| `h264_qsv` | `-global_quality 22 -look_ahead 1 -look_ahead_depth 40 -extbrc 1` | ICQ 语义近似 CRF |
| `h264_vaapi` | `-vaapi_device /dev/dri/renderD128 -vf format=nv12,hwupload -rc_mode CQP -global_quality 22` | 无 CRF；ICQ 仅 Intel 且有 bug |
| `h264_amf` | `-quality balanced -b:v 6M` | |
| 软编兜底 | `libx264 -crf 20 -preset veryfast`（LGPL 构建无 libx264 时用 openh264） | |

---

## 4. 推理引擎：「Metal + Vulkan + OpenVINO」的落地现实

### 4.1 关键调研结论（2026-08）

- **ONNX Runtime 没有原生 Vulkan EP**。WebGPU EP（rc.10+，基于 Dawn）可在 Win(DX12)/mac(Metal)/Linux(Vulkan) 跑，但官方标注**实验性**。Vulkan 推理的其它路径：ncnn（Vulkan 最成熟，但 Rust 绑定 2024 年起停更，需自维护 `ncnn-sys`）。
- **ort 2.0.0-rc.13**（包 ONNX Runtime 1.28）覆盖：CoreML（含 ANE）/ DirectML / CUDA / OpenVINO / XNNPACK / WebGPU(实验) / CPU，`load-dynamic` + rpath 分发模式成熟。⚠️ 2.0 长期 RC，锁定 `=2.0.0-rc.13`。
- **tract 0.23.4**：纯 Rust、零原生依赖单二进制；CPU 性能与 ORT 相当（M1 上略快）；**Metal 后端仍在补基础算子**（2026-08 仍在合 pooling/resize），不能当 macOS GPU 主力 → 用作 CPU 兜底/受限环境。
- candle（ONNX 弱、无 Vulkan）、wonnx（**2025-05 已归档**）、tch-rs（无 ONNX、体积失控）→ 排除。
- Ultralytics 官方已发布 `ultralytics-inference` Rust crate（基于 ort，含 letterbox/NMS/mask/keypoint 全套后处理，EP 覆盖与我们的选型一致）——⚠️ AGPL-3.0，闭源商用不可直接依赖；**本项目自研后处理**（NMS ~100 行 + mask 原型图解码，参考其实现思路与 ort 官方 yolo 示例）。

### 4.2 后端矩阵（最终设计）

| 平台 | 推理 EP（ort） | 遮罩合成 | 说明 |
|---|---|---|---|
| macOS | **CoreML**（`CPUAndNeuralEngine`，可吃 ANE+Metal）→ CPU | wgpu(Metal) 或 CPU | "Metal" 需求由 CoreML(Metal GPU/ANE) + wgpu 合成双路径满足 |
| Windows | **DirectML**（任意 DX12 GPU，零驱动依赖）→ CUDA(N卡可选) → CPU | wgpu(DX12) 或 CPU | DirectML 覆盖 N/A/I 全系；NVIDIA 高级选项开放 CUDA EP（引导下载大运行库） |
| Linux | **WebGPU(Vulkan，auto 默认)** → OpenVINO(Intel, 显式可选) → CPU | **wgpu(Vulkan)** 或 CPU | 合成必然走 Vulkan → "Vulkan 加速"产品化兑现；推理 Vulkan 走 WebGPU EP（2026-08-21 起 auto 默认，EP 失败 ort 自动落 CPU） |
| 任意 | tract（纯 Rust CPU）兜底 | CPU | ORT dylib 加载失败时自动降级，保证"永远能跑" |

### 4.3 抽象层设计（为 Vulkan 留后门）

```rust
pub trait DetectorBackend: Send {
    fn name(&self) -> &'static str;                      // "coreml" / "directml" / "openvino" / "cpu" / "webgpu"
    fn load(models: &ModelManifest) -> Result<Self>;      // 编译期注册 EP；FP16 优先
    fn infer_batch(&mut self, batch: &[LetterboxedRgb]) -> Result<Vec<RawDetections>>;
}

pub trait ComposeBackend: Send {
    fn compose(&mut self, frame: &mut FrameNv12, masks: &[InstanceMask], style: MaskStyle);
}
// 实现：ComposeCpu（SIMD + 包围盒，默认）/ ComposeWgpu（可分离高斯 compute shader，批量）
```

- 未来接入 ncnn/Vulkan 原生后端 = 新增一个 `DetectorBackend` 实现，管线零改动。
- 后端枚举通过 frb 暴露给 UI 的「加速器」下拉框（`list_backends() -> Vec<BackendInfo>`，含可用性与显存信息）。

### 4.4 预处理 / 后处理（自研，规避 AGPL）

- **预处理**：NV12 → letterbox 到 640/960/1280（保持纵横比、114 灰边）；仅对 letterbox 子图做 YUV→RGB；批内张量 `[N,3,S,S]` f32（FP16 后端则 f16）。SIMD（`wide`/手写 avx2+neon）或 wgpu shader。
- **后处理**：decode（含 e2e 免 NMS 的 YOLO26 直接取 topk）、标准 NMS（IoU 0.45；seg 用 mask NMS）、proto×coeff 的 mask 原型图上采样裁剪、pose 关键点解码。参考 ort yolo 示例与 AndreyGermanov/yolov8_onnx_rust_segmentation 思路自实现，MIT 许可。

---

## 5. 模型选型（深度调研结论）

### 5.1 人体线（COCO person 实例分割）

| 模型 | n 档 mask AP / 参数 / FLOPs | CPU ONNX(n) | 评价 |
|---|---|---|---|
| **YOLO26n-seg**（2025-09） | **33.9 / 2.7M / 9.1B** | **53.3ms** | ✅ 默认。免 NMS 端到端头（ONNX 图干净、Vulkan 友好）、小目标更好（STAL）、CPU 更快 |
| yolo11n-seg | 30.0 / 2.9M / 10.4B | ~80ms | ✅ 保守备选（现用模型，生态最成熟） |
| YOLO26s-seg | 40.0 / 10.4M / 34.2B | — | ✅ 准确档（box 47.3） |
| YOLO26m-seg | 44.1 / 23.6M / 121.5B | — | 准确档与极致档之间的高性价比中间档（box 52.5） |
| YOLO26l-seg | 45.5 / 28.0M / 139.8B | — | 同上（box 54.4）；比 m 仅 +5B FLOPs，量级接近时优先 l |
| **YOLO26x-seg** | **47.0 / 62.8M / 313.5B** | — | ✅ **极致档（算力充裕场景）**。mask 47.0 为全系最高（box 56.5）；仅推荐 GPU（CUDA/CoreML/DirectML）|
| YOLOv12 | +1.2 box AP | 慢（attention 需 FA kernel） | ❌ Vulkan/核显算子风险高 |
| RT-DETR / D-FINE / RF-DETR | 检测强（RF-DETR-Nano 48.0 AP） | — | ⚠️ 无实例分割/无 n 档；仅作 license 敏感时的框检测备选（Apache 2.0） |
| RTMDet-Ins | ~52.8 AP 顶格 | — | ⚠️ 需 MMDeploy 工具链，不选 |

**大模型档（m/l/x）的算力现实**（估算值，FP16、fused 模型指标）：

| 模型 | @640 每帧 FLOPs | @1280 每帧 FLOPs | M1 Max(CoreML) 估 | RTX 4090(CUDA) 估 | 适用 |
|---|---|---|---|---|---|
| YOLO26m-seg | 121.5B | ~486B | 25-40 fps | 100+ fps | 离线准实时 |
| YOLO26l-seg | 139.8B | ~559B | 20-35 fps | 100+ fps | 离线准实时 |
| YOLO26x-seg | 313.5B | ~1.25T | **8-15 fps** | **40-80 fps @640；20-40 fps @1280** | 离线极致档（不限时） |

- 极致档定位是**离线导出**（"处理时慢慢跑，出片质量最高"），实时预览自动降到均衡档模型——预览与出片用不同模型是设计内行为。
- x 档推理时建议：batch 降到 2-4（显存占用约 2-4GB）、隔帧检测关闭（算力充裕时全帧检测召回最高）、CUDA EP 显式指定（N 卡）。
- FP16 下 x 档权重约 125MB，模型管理页按需下载，不进默认安装包。

### 5.2 人脸线

| 模型 | Easy/Medium/Hard AP | Landmark | License | 评价 |
|---|---|---|---|---|
| **YOLO11Face n-pose** | 94.6 / 92.6 / **81.0** | 5 点 | Apache 2.0（声明） | ✅ 默认。现 yolov8n-face 的平滑升级，同管线 |
| **YOLO11Face s-pose** | 95.7 / 94.2 / **85.2** | 5 点 | 同上 | ✅ 准确档；landmark 用于转头场景按眼距外扩 |
| SCRFD-2.5G/10G | 93.8→95.2 / 78.6→83.1 | 5 点 | **非商业**（insightface 权重） | ⚠️ 备选，商用禁 |
| YuNet | 中（小脸弱） | 5 点 | MIT（OpenCV Zoo 官方 ONNX） | ✅ 速度档兜底（75K 参数，CPU 亚毫秒） |
| RetinaFace-R50 | Hard 91.8 天花板 | 5 点 | 非商业 | ❌ 太重 |

### 5.3 跟踪（新增层，马赛克稳定性的核心）

- **ByteTrack**（默认）：纯运动+IoU 关联，无 ReID 模型；Ultralytics 实现是纯 NumPy/SciPy，~几百行，Rust 自实现或用 `mot-rs`/`jamtrack-rs`（后者含 OC-SORT/BoT-SORT）。
- **OC-SORT**（可选）：专治漏检/遮挡（observation-centric 重更新），运动镜头开此项。
- 跟踪带来的三个直接收益：
  1. **ID 稳定**：同一人持续同一遮罩，消除逐帧闪烁；
  2. **漏检补偿**：1-2 帧漏检用 Kalman 预测补框（"宁可多打不可漏"）；
  3. **隔帧检测**：每 2-4 帧推理一次，中间帧用跟踪状态外推 mask 仿射跟随 → 吞吐 ×2-4。

### 5.4 高级档（可选，离线精修）：SAM2.1 传播

- sam2.1-tiny(38.9M)/small(46M)：memory bank 机制天然适合「首帧 prompt → 后续传播」；A100 91FPS 但 **M1/核显仅 10-25FPS** → 只进"离线精修"档（处理时可选，不进实时预览）。
- Apple 官方 CoreML FP16 包（apple/coreml-sam2.1-small）是 macOS 最佳起点；ONNX 为社区级支持（这是它不进主路径的主因）。
- 管线模式（DEVA 式）：yolo-seg 每 N 帧重检测纠偏 + SAM2.1 中间帧传播 → 电影级稳定 mask。**列为 M4 里程碑，默认档不用。**

### 5.5 五档推荐组合（App 内「质量预设」直接对应）

> 实现状态（2026-08-21）：五档全部落地（core `preset.rs` + CLI `--preset` + App 预设选择器）。速度=yolo26n 检测-only@640 + YuNet 人脸；均衡=yolo26n-seg@640 + yolo11n-face；准确=yolo26s-seg@960；极致=yolo26x-seg@1280（级联 ROI + TTA）；**极限·档案级=两阶段 ensemble 管线（§5.6，2026-08-21）——yolo26x-seg@1536 + GD-tiny + SAM2.1-large + retinaface-r34 滑窗 + OSNet 关联，App 内"开始分析→复核→渲染"任务流**。全档逐帧检测（2026-08-20 决策）。

| 预设 | 人体 | 人脸 | 跟踪 | imgsz | 目标 |
|---|---|---|---|---|---|
| **速度**（预览/低配） | yolo26n 检测框+margin（免 mask） | YuNet | ByteTrack + 隔 3 帧检测 | 640 | CPU 亦可实时 |
| **均衡**（默认） | YOLO26n-seg | YOLO11Face n-pose | ByteTrack + 隔 2 帧 + mask 并集平滑 | 640-960 | 核显/M1 25-60fps |
| **准确**（离线导出） | YOLO26s-seg | YOLO11Face s-pose | OC-SORT + 全帧检测 + EMA | 960-1280 | 不限时，高召回 |
| **极致**（算力充裕/离线导出） | **YOLO26x-seg**（l/m 为中间选项） | YOLO11Face s-pose + 人脸级联 ROI @1280 | OC-SORT + 全帧检测 + EMA + TTA | **1280** | 不限时，高召回；GPU(CUDA/CoreML) 推荐；M4 后可叠加 SAM2.1 传播 |
| **极限·档案级**（accuracy-first） | **ensemble**：YOLO26x-seg @1536 + Grounding DINO-base → WBF → **SAM2.1-large 逐帧精修** | RetinaFace-R50(MIT) @1280/1920 **滑窗** + 多尺度 | IoU+外观嵌入 masklet 关联；传播仅作一致性校验 | 1280-1536 | **0.1-0.5fps 可接受**，一切以精度为准；两阶段「分析→复核→渲染」（§5.6） |

极致档说明：人脸线 YOLO11Face **只发布 n/s 两档**（无更大模型），极致档的人脸增益来自「s-pose 顶格 + 1280 高分辨率推理 + person 头部级联 ROI 裁剪二次检测」三件套，而非更大的人脸权重；人体线 x 档（mask 47.0）+ 1280 分辨率是遮挡/小人群场景召回的主要来源。

### 5.6 极限精度档（档案级，accuracy-first）——完整设计

定位：0.1–0.5 fps 完全可接受（每帧数秒），一切以 person mask 精度与人脸召回为准，面向"一次处理、长期存档"场景。

**调研结论（2026-08）**：

- 闭集单模型天花板：MaskDINO Swin-L（mask AP **54.7**，开权重第一）> Mask2Former Swin-L（50.1；注意"57.8"是全景分割 PQ 口径）> EoMT-L@1280（DINOv3 版 49.9，MIT，HF 实现）。但 **MaskDINO/Mask2Former 均无官方 ONNX**（detectron2 生态、MSDeformAttn 自定义算子，自导出成本高）；EoMT 导出最干净。
- **社区公认的精度天花板是 Grounded-SAM2 组合范式**：多检测器出框（ensemble + WBF 融合）→ **SAM2.1-large（224M）逐帧 box-prompting** 出精细 mask → 帧间关联。SAM2.1 的边界质量来自 SA-V 人工级数据引擎，是公开模型 mask 保真度的上限。SAM2.1-large 有现成 Apache-2.0 ONNX 权重（vietanhdev/segment-anything-2.1-onnx-models）。
- SAM2 **视频传播模式（memory attention/encoder）无官方 ONNX**（社区拆件 + 图外状态），且单路径传播累积漂移（SAM2Long 证据）→ 极限档以**逐帧 prompting 为准**，传播仅作一致性校验与人工复核标记。
- 人脸：WIDER FACE 榜已饱和，换模型收益 ~1 AP；**推理策略收益远大于换模型**——SAHI 滑窗小目标最高 **+25% AP**、假阳性 ÷10。干净许可的高精度权重：biubug6 RetinaFace-R50（MIT；OpenVINO OMZ 有现成 IR）。

**管线 A（主推）**：

```
每帧（离线，可中断续跑）:
 1. 检测并行 2 路:
    YOLO26x-seg @1536（box+粗mask，复用已有资产）
    Grounding DINO-base（prompt="person"，文本 embedding 离线预计算；
                        tiny 档有现成 ONNX，base 用 HF optimum 自导）
 2. 框级 WBF 融合（含 multi-scale + flip TTA 副本，+2~3% mAP；Rust 自实现，~百行）
 3. Mask 精修: SAM2.1-large encoder 1 次/帧 + 每个 WBF 框 1 次 decoder
    （重叠 mask IoU 去重；低置信框保留——召回优先）
 4. 人脸: RetinaFace-R50 @1280/1920 滑窗（20-25% overlap）+ 多尺度合并
    （孤立小人脸补框喂 SAM2 精修；人脸归并到所属 person masklet）
 5. 时序: IoU + 外观嵌入关联成 masklet；
    SAM2 memory 传播仅用于一致性校验（IoU < 阈值 → 标记人工复核候选帧）
 6. 全部 mask 缓存落盘（RLE + 元数据）→ UI 人工修补关键帧
    → 仅重算受影响 masklet → 渲染
```

- 每帧耗时预估：RTX 4090 ≈ **0.8–2 s**；M1 Max ≈ **3–6 s**；纯 CPU 不推荐（>30s/帧）。
- 模型清单：YOLO26x-seg（已有）+ GD-base（Apache-2.0，自导）+ SAM2.1-large ONNX（Apache-2.0，现成）+ RetinaFace-R50（MIT，现成 IR）。
- 管线 B（极简备选）：EoMT-L@1280 单模型（MIT、HF 权重、导出最干净）+ SAM2.1-large 精修 + RetinaFace 滑窗——模型最少、许可最净、维护面小，召回上限比 A 低 1–3 AP，作为 A 中 GD 自导受阻时的降级路线。

**两阶段架构**（极限档专用，区别于 1-4 档的流式管线）：

```
[分析阶段]  逐帧全管线推理 → masklet 缓存落盘（按帧断点，可暂停/续跑/跨会话）
      ↓
[复核阶段]  UI：masklet 时间轴 + 关键帧刷子/加减点修补（DaVinci Resolve Magic Mask 先例）
            修改仅触发受影响 masklet 段重算
      ↓
[渲染阶段]  纯 compose+encode（无推理），全速跑完
```

分析阶段长达数小时（0.1-0.5fps × 全片）是预期行为——进度 UI 显示 ETA 与已完成帧断点；mask 缓存使长任务可中断续跑、复核可反复迭代而不必重推全片。

**ort 跑大 transformer 的工程注意**：输入尺寸固定（1024/1280，规避动态形状 IO binding 坑）；大模型（SAM2.1-L FP16 ~450MB）注意 arena 内存控制；FP16 下 LayerNorm/GroupNorm 数值敏感，归一化层保 FP32（混合精度导出）；CoreML EP 走 GPU 而非 ANE（transformer 在 ANE 回退严重）；优先 HF transformers 重实现导出的图（纯标准算子），避开 detectron2 自定义 op。

### 5.7 License 汇总（重要）

> **分发决定（2026-08-19）：本项目以 AGPL-3.0-or-later 开源分发**（仓库根 LICENSE + NOTICES.md，
> 许可文件随 .app Resources 分发）。该选择同时满足 Ultralytics 对权重"嵌入产品分发"的开源要求
> 与 ffmpeg GPL 构建的随附要求；闭源商用路径保留为企业许可或 D-FINE+YuNet+SAM2.1 换栈。

- Ultralytics 全部权重（YOLO11/26/v8/v12）：**AGPL-3.0** → 本项目开源（**已选定 AGPL-3.0-or-later**）可直接用；**闭源商用需企业许可**（一份覆盖含 YOLO26 全系）或换 D-FINE(人体框)+YuNet(人脸)+SAM2.1(mask) 全 Apache/MIT 组合。
- YOLO11Face：repo 声明 Apache 2.0 但基于 ultralytics 训练，有传导争议——本项目按 AGPL 对待（见 NOTICES.md）。
- 极限档组件：SAM2.1（Apache-2.0）、Grounding DINO 1.0 开放权重（Apache-2.0）、EoMT（MIT）、biubug6 RetinaFace 权重（MIT）——均可商用；⚠️ insightface 预训练人脸权重**非商用**、Grounding DINO 1.5/2.0 是闭源 API 不可本地部署。
- FFmpeg：子进程调用可执行文件 + LGPL 构建（BtbN LGPL shared / 自建）→ 闭源友好；**不要**打包 homebrew(GPL) dylib。当前打包实为 **GPL 构建**（LGPL 变体无 H.264 软编兜底）——开源分发下合规，闭源分发需换回 LGPL 构建或接受 GPL 文本随附。
- ONNX Runtime：MIT + 遥测声明（隐私敏感需在文档披露或禁用）；ort/tract/wgpu/ffmpeg-sidecar：MIT/Apache。

---

## 6. 精度 / 性能 / 效率优化清单（本地推理前提下）

### 精度（按性价比排序）

> ⚠️ 实测修正（2026-08）：**blur 不是匿名化手段**——半径扫描 25→64 全部残留 person 检出（0.83→0.52，CNN 对模糊鲁棒）；mosaic/solid 才能使检测器完全失认。blur 仅作观感选项保留，UI 需加警示。

1. **跟踪 + 漏检补偿**（零推理成本）：Kalman 预测填补 1-2 帧漏检；`lost` 状态的 track 保留遮罩 0.5s 渐隐。
2. **mask 时序平滑**（零成本）：上一帧 mask 膨胀 3-5px 与本帧取并集 + per-ID EMA(α≈0.7)，消除闪烁。
3. **人脸级联 ROI**（省算力+提召回）：person 头部区域裁剪后跑人脸模型 → 小脸有效分辨率翻倍。
4. **imgsz 提升**：960/1280 对远景小人是最大杠杆（FLOPs 平方增长，靠隔帧检测对冲）。
5. **landmark 外扩**：按眼距/鼻位置比例外扩（抗转头）替代固定像素外扩。
6. **低置信人脸兜底**：face conf 低于阈值但落在 person 头部区（pose 验证）时仍打码。
7. TTA 翻转增强（+0.3~0.8 AP，2× 代价）：仅准确档离线处理开。

### 性能

1. 三段流水线并行（吞吐≈最慢段，已验证）+ Rust 零 GIL。
2. **隔帧检测 + 跟踪插值**：有效吞吐 ×2-4。
3. ~~FP16 全平台默认~~ **实测修正（2026-08-19）**：ultralytics `half=True` 导出保持 f32 输入/输出边界（内部 f16 + Cast 节点），与现有代码零改动兼容、精度无损、CPU 仅 +3%、体积减半——但 **CoreML 上反而慢 22%**（均衡档 17.3→13.4fps，Cast 边界恶化分段调度，NPU 模式同样）。**保持 FP32 导出**；若后续要压体积，优先真 f16 边界（Rust 侧 letterbox 直出 f16）重测。INT8 仅量化 backbone/neck（proto 系数量化劣化 mask 边缘），留作后续。
4. 批推理（默认 batch=8，动态 batch ONNX）摊薄 EP 提交开销。
5. 合成：CPU SIMD 包围盒版（默认，现 Python 版 51fps → Rust 预期 150fps+）/ wgpu 批量可分离高斯（多人场景）。
6. NV12 管道省 2 次 JPEG 编解码；letterbox 只转换待推理子图。
7. 编解码全程硬件（§3 回退链）。
8. **大模型档（m/l/x）策略**：推理显存/耗时超阈值时自适应 batch 8→2、隔帧自动开启（用户选择极致档且 GPU 充裕时保持全帧）；**实时预览与出片解耦**——预览固定用均衡档模型，出片用所选大模型，预览流畅度不受出片模型影响。

### 效率（能耗/资源）

- 自适应批大小与隔帧间隔（根据推理耗时动态调，目标"跟上解码"）；
- 硬件编码 `-realtime 1` 类参数降低编码功耗；
- 处理中 UI 预览帧主动降采样（720p JPEG 流）不与主管线抢带宽。

---

## 7. Flutter UI 设计

### 7.1 技术栈

| 层 | 选型 |
|---|---|
| 框架/设计 | Flutter 3.4x stable + **Material 3 自定义暗色主题**（暗色优先，工具类应用一致性；fluent_ui/macos_ui 后期可选） |
| 窗口 | window_manager：隐藏标题栏自绘 + DragToMoveArea + 记住窗口状态 + 跟随系统亮暗 |
| 状态 | Riverpod 3（`StreamProvider` 接管线事件流 + `select` 细粒度订阅防整页重建） |
| Rust 绑定 | flutter_rust_bridge 2.12（codegen 锁版本与 runtime 一致） |
| 持久化 | shared_preferences（设置）+ hive（预设/队列） |
| 拖放 | desktop_drop |
| 预览帧 | frb 零拷贝 Uint8List → ui.Image + CustomPainter 叠框；实时预览 = 720p JPEG 帧流（30fps 可行）；播放器（可选）media_kit |

### 7.2 信息架构（四屏向导 + 队列）

```
[1 拖入] → [2 预览调参] → [3 处理进度] → [4 完成]
                    └──────── 队列侧栏（多任务） ────────┘
              设置（模型/加速器/编码/外观）  全局常驻
```

### 7.3 界面原型

**主窗口 — 拖入屏（暗色、自绘标题栏）**

```
┌──────────────────────────────────────────────────────────────┐
│ ● AutoMosaic Studio            [队列 2] [设置] [─ □ ✕]        │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│        ┌────────────────────────────────────────┐            │
│        │                                        │            │
│        │     ⬇  拖入视频，或点击选择文件          │            │
│        │        MP4 / MOV / MKV / WebM …        │            │
│        │                                        │            │
│        └────────────────────────────────────────┘            │
│   最近:  vacation.mp4   meeting-0412.mp4   street-4k.mov     │
│   GPU: Apple M2 Max · CoreML ✓   模型: 均衡档 已就绪 ✓        │
└──────────────────────────────────────────────────────────────┘
```

**预览调参屏**（左帧预览 + 右参数；Rust 抽 5 个关键帧跑检测，框叠加可交互）

```
┌──────────────────────────────────────────────────────────────┐
│ ← vacation.mp4  00:03:21 · 1080p60 · H.264 · 有音轨           │
├───────────────────────────────────────────────┬──────────────┤
│  ┌─────────────────────────────────────────┐  │ 检测目标      │
│  │                                         │  │ ☑ 人体(轮廓)  │
│  │      [人体mask轮廓·绿色半透明]            │  │ ☑ 人脸(框)    │
│  │      (人脸框·橙色, 5点landmark)           │  │ ☐ 遮住他人时  │
│  │          ⏵ 关键帧 2/5 ──────○──          │  │  保留指定人脸  │
│  └─────────────────────────────────────────┘  │              │
│  [▶ 实时试算此段]                               │ 遮罩样式      │
│                                               │ ◉ 高斯模糊    │
│  检出: 3 人 · 3 脸   置信度: 35%  ──────●─────  │ ○ 像素马赛克  │
│                                               │ ○ 纯色遮罩    │
│  预估: 51 fps · 全片约 2 分 10 秒               │ 强度 ▓▓▓░░ 35 │
│                                               │ 羽化 ▓░░░░ 8  │
│                                               │              │
│                                               │ 质量预设      │
│                                               │ [速度|均衡|准确|极致|极限]│
├───────────────────────────────────────────────┴──────────────┤
│                        [ 加入队列 ]   [ 立即处理 ⌘↵ ]          │
└──────────────────────────────────────────────────────────────┘
```

**处理进度屏**（阶段 Stepper + 分段吞吐 + 总进度 + 可展开日志 + 实时预览小窗）

```
┌──────────────────────────────────────────────────────────────┐
│ 正在处理: vacation.mp4                              [ 取消 ]  │
├──────────────────────────────────────────────────────────────┤
│  探测 ✓ → 解码 ⚡ → 推理+跟踪 ⚡ → 合成 ⚡ → 编码 ⚡ → 完成 …   │
│  ─────────────────────────●──────────────────────────  63%   │
│  帧 57,204 / 91,240      51.3 fps   预计剩余 1 分 48 秒       │
│  检出 4 人 · 4 脸   GPU: CoreML(ANE)   编码: h264_videotoolbox │
│  ┌───────────────────────┐   ┌──────────────────────────────┐ │
│  │  (实时预览·已打码帧     │   │ ▸ 日志                       │ │
│  │   720p 30fps)         │   │ 14:02:11 infer 51200 帧 51fps│ │
│  └───────────────────────┘   │ 14:02:15 encode 码率 6.1Mbps │ │
│                              └──────────────────────────────┘ │
├──────────────────────────────────────────────────────────────┤
│ 队列: 1. vacation.mp4 ⚡63%   2. meeting.mp4 ⏸等待   [暂停][停止] │
└──────────────────────────────────────────────────────────────┘
```

**设置屏**（分组卡片）：外观（主题/语言）｜模型管理（五档预设、下载/更新、自导入 ONNX+输入规格、SHA 校验）｜加速器（`list_backends()` 驱动的下拉：CoreML/DirectML/CUDA/OpenVINO/WebGPU实验/CPU/tract兜底 + 每项探测状态与测试按钮）｜视频（hwaccel 自动/手动、编码器、码率策略、容器）｜高级（批大小、隔帧间隔、跟踪器参数、日志级别）。

### 7.4 进度与事件协议（frb StreamSink）

```rust
// Rust（frb 自动翻译为 Dart 枚举/类，编译期类型安全）
pub enum Stage { Probing, Decoding, Inferring, Composing, Encoding, Muxing, Done }

pub enum PipelineEvent {
    Probed      { total_frames: u64, fps: f32, w: u32, h: u32, has_audio: bool },
    StageEnter  { stage: Stage },
    FrameTick   { decoded: u64, processed: u64, total: u64,     // UI 100ms 节流合并
                  infer_fps: f32, pipeline_fps: f32, eta_secs: f32,
                  persons: u32, faces: u32 },
    PreviewFrame{ jpeg: Vec<u8> },                              // 零拷贝 Uint8List
    Log         { level: LogLevel, line: String },
    Finished    { output_path: String, elapsed_secs: f32 },
    Failed      { error: JobError, ffmpeg_tail: Option<String> },
    Cancelled,
}

pub enum JobError { FileNotFound, DecodeFailed(String), ModelLoadFailed(String),
                    BackendUnavailable(String), EncoderFailed(String), IoError(String) }
```

- UI 侧：`pipelineEventProvider = StreamProvider` + `select` 按 event 类型细粒度订阅（进度条只听 FrameTick、日志面板只听 Log，高频事件 provider 层 100ms 批量）。
- 取消：`cancel_job(job_id)` → CancellationToken 级联；写侧关编码器 stdin（EOF）→ 编码器正常收尾写 moov（半成品可播），3s 超时 kill；队列可「停止当前/跳过下一个」。

### 7.5 预览方案（两层）

1. **配置期静态预览**（必须）：Rust 均匀抽 5 关键帧 → 解码缩至 720p → 跑当前参数检测 → 帧数据（零拷贝 Uint8List）+ 归一化框返回 → Dart `ui.Image` + CustomPainter 绘制（mask 轮廓半透明多边形、人脸框、landmark 点）。拖动置信度/强度滑杆 → 重新绘制（检测缓存不重跑，>100ms 防抖后才重推理）。
2. **处理期实时预览**（可选开关）：compose 后帧降采样 720p JPEG（q80）经 `PreviewFrame` 事件推送（节流至 30fps），Dart 解码绘制。不引入 Texture widget（v1 不需要 4K 低延迟预览；未来需要再上 Rust 直写 GL/Metal 纹理）。

---

## 8. 打包分发

| 平台 | 产物 | FFmpeg | 要点 | 现状（2026-08-19） |
|---|---|---|---|---|
| macOS | .app → .dmg | ~~自建 LGPL~~ → **osxexperts 9.0 GPL 预编译**（VideoToolbox） | codesign + notarization；~~universal2~~（暂 arm64-only：pyke 无 x86_64-apple-darwin 预编译，Podfile 已锁 ARCHS） | ✅ dmg 可用（未签名）；ffmpeg+模型已入 .app Resources；App Sandbox 已关闭（spawn 架构必需）；2026-08-22 起 download 形态移除，产物更名 simple-automosaic-<版本>.dmg（原 AutoMosaic-Studio-*） |
| Windows | MSIX（+ 便携 zip） | ~~BtbN LGPL~~ → **BtbN GPL 静态**（含 nvenc/amf/d3d11va/libvpl + libx264 兜底） | CUDA EP 运行库做可选下载 | ✗ 未在真机验证（fetch 脚本已有分支）；media_kit 需补 libs 包 |
| Linux | AppImage + deb | ~~系统包~~ → **BtbN GPL 静态**（VAAPI/NVDEC/QSV + libx264） | 老发行版容器内构建保 glibc 兼容 | ✅（用户实测口径）**AppImage 双形态可用**（`scripts/build-linux-appimage.sh`：闭包收集+自检+libmpv/libwebgpu_dawn 显式入包——dawn 在 cargokit 构建目录，ldd 闭包解析不到，2026-08-20 真机自检暴露后修复；单形态 full ~2.6GB[含极致档全模型集]，Xvfb 冒烟通过；2026-08-21 含拖影修复+全档逐帧的重打包经用户 Linux 真机验证功能正常；2026-08-22 重打包（WebGPU auto 默认/自绘窗口控件/设备选项平台化）经用户物理机验证通过；CLI 二进制注入 $ORIGIN rpath 免手动 LD_LIBRARY_PATH；2026-08-22 起 download 形态移除、产物更名 simple-automosaic-<版本>-linux-<arch>.AppImage）；Flutter Linux 构建需 mpv-devel；VAAPI 编码链修正（RDNA4 无 H264 硬编→libx264 兜底，av1_vaapi 显式可用）；deb 未做 |

> GPL 取代 LGPL 的原因：LGPL 变体不含任何 H.264 软件编码器，硬编不可用的机器无兜底（实测容器内 nvenc 缺 libcuda 场景）。

- **命名口径（2026-08-22 统一）**：构建产物文件名统一 `simple-automosaic`——dmg `simple-automosaic-<版本>.dmg`、AppImage `simple-automosaic-<版本>-linux-<arch>.AppImage`（desktop/icon 同名）、Windows exe/RC 元数据；显示名 `Simple AutoMosaic`（标题/CFBundleName/ProductName/dmg 卷名）；bundle id 三平台统一 `dev.automosaic.simpleAutomosaic`（Linux APPLICATION_ID 原 typo `simple_automosAic` 一并修复；CLI 维持 `automosaic-cli`，workspace crate 名口径，含 automosaic-core/automosaic-cli）。

- **模型分发**（2026-08-22 再修订）：安装包全模型入包（单形态 full，离线可用）；**应用内模型下载功能移除**（SHA256 校验/主源失败切镜像等下载链路一并废弃，UI/FFI 遗留代码待清退）。
- 自动更新：auto_updater（Sparkle/WinSparkle）+ Linux 免进；体积优化：release profile `lto`/`strip`/`panic=abort`，`--analyze-size` 监控。
- CI：GitHub Actions 三平台矩阵（subosito/flutter-action + Rust toolchain + cargo 缓存）。

---

## 9. 里程碑

| 阶段 | 内容 | 验收标准 | 状态（2026-08-19） |
|---|---|---|---|
| **M0**（脚手架） | workspace 搭建、frb integrate、CLI 骨架、CI 三平台构建 | `automosaic-cli --help` 三平台可跑 | ✅ 完成（CI 除外） |
| **M1**（核心管线，CLI 形态） | NV12 双向管道、CPU 推理（ort CPU）、NMS/mask 后处理、CPU SIMD 合成、进度事件、探测回退链、取消 | tests/clip5s.mp4 端到端出片；M 系列 CPU 路径 ≥ Python 版 51fps | ✅ 完成（合成仍为标量实现，SIMD 列入 M4） |
| **M2**（GPU + UI） | CoreML/DirectML EP、wgpu 合成、Flutter 四屏 UI、预览、队列、设置、模型下载 | 三平台硬解硬编全通；UI 完整流程可用 | ◐ CoreML+UI 主体完成（播放器/对照预览/取消/**队列/设置/预设选择器**）；DirectML 未测、wgpu 未做、模型下载未做（manifest+校验已有） |
| **M3**（精度） | ByteTrack/OC-SORT、时序平滑、隔帧检测、人脸级联 ROI、landmark 外扩、四档预设（含极致档 YOLO26x-seg/l/m） | 漏检率/闪烁主观评测显著优于 M1；文档化 A/B（同片对比 Python 版） | **M3 基本完成**：ByteTrack 完整形态（Kalman 预测框关联 + 低分二次救援）+ 人脸级联 ROI（几何过滤 + 三态开关）+ **landmark 外扩**（yolo11-face-pose 双眼坐标解码，眼距缩短量×0.6 补水平外扩抗转头，无 landmark 回退固定值）+ **per-ID EMA**（α=0.7 定点实现，mask 二值化输出）+ 时序平滑/隔帧检测/人脸 gate/四档预设 + **丢失 track 遮罩渐隐**（2026-08-20：保持期后半段腐蚀回缩，离场不硬切）+ **翻转 TTA**（2026-08-20：极致档默认开）；2026-08-21 补：YuNet 速度档 + OC-SORT OCR/ORU（BoT-SORT 外观关联属 M5） |
| **M4**（打磨） | OpenVINO EP、WebGPU EP 灰度、INT8、打包签名公证（不适用）、自动更新 | 发布 1.0 | ◐ 打包（2026-08-22 起单形态 full）/CI/发布流水线/格式兼容/增强开关化已做；WebGPU ✅、OpenVINO ◐（feature 门控，见 §0.5-E）/INT8（FP32 决策维持）/自动更新未做（详见 §0.5 清单 D/E/A） | ◐ 内置 ffmpeg+dmg 已做（未签名）；OpenVINO/WebGPU/INT8/公证/更新未做；**新增完成**：CoreML 编译缓存、调试工具、双轮 e2e |
| **M5**（极限·档案级档） | §5.6 管线 A：GD-base 自导出、SAM2.1-large 逐帧 prompting、WBF、RetinaFace 滑窗、masklet 缓存与断点续跑、复核 UI（关键帧刷子/加减点 + 段级重算）、两阶段任务流 | 同片 A/B：极限档漏检率显著低于极致档；数小时分析任务可中断续跑 | ✅ **2026-08-21 全量落地**（管线 A 全链：YOLO26x@1536+GD-tiny ensemble → WBF → SAM2.1 精修 → RetinaFace 滑窗 → OSNet 外观 masklet 关联；复核 UI 刷子/点提示/补丁；两阶段 App 任务流；实测 0.15fps 落进设计区间）。**偏差记录**：GD 用 tiny 而非 base（自导出工具链缺，设计内降级链）；RetinaFace 用 R34 而非 R50（官方 R50 仅 GDrive、镜像实测偏弱——同接口可替换）；SAM2 传播一致性校验与段级重算未做（档案级不编造观测的取舍，补丁为逐帧 materialize 语义） |

---

## 10. 风险与缓解

| 风险 | 等级 | 缓解 |
|---|---|---|
| ort 2.0 长期 RC、API 破坏性变更 | 中 | 锁定 `=2.0.0-rc.13`；`DetectorBackend` trait 隔离；tract 兜底 |
| WebGPU/Vulkan 推理实验性（Linux 非 Intel 无 GPU 加速） | 中 | auto 默认 WebGPU（9070XT/RADV 实测输出逐比特一致；原"默认 CPU"决策 2026-08-21 撤销——极限档大模型 CPU 不堪用），EP 失败 ort 自动落 CPU、cpu 显式退出；ncnn 后端留 trait 后门 |
| CoreML/DirectML EP 对动态 batch 的边角问题 | 中 | 导出时固定 batch 维度网格（1/2/4/8）按需切换；探测期跑真实冒烟 |
| Ultralytics AGPL 传导（若闭源商用） | 高（仅商用） | 开源发布（本设计默认）或换 D-FINE+YuNet+SAM2.1 全 Apache/MIT 组合 |
| FFmpeg hwaccel 驱动差异（10bit HEVC、AV1） | 中 | 流级探测 + 自动软解回退 + UI 提示实际使用路径 |
| 4K60+ 管道带宽触顶 | 低 | v2 路线：rusty_ffmpeg 进程内 hwframes 零拷贝（trait 已预留） |
| SAM2.1 视频传播无官方 ONNX（极限档） | 中 | 已按"逐帧 prompting 为准、传播仅作校验"设计，规避 memory 组件自研；确需传播时走社区拆件方案（M5+ 评估） |
| Grounding DINO-base 需自导出（极限档） | 中 | 降级链：GD-tiny（现成 ONNX）→ 管线 B（EoMT-L，MIT、HF 现成） |
| 极限档单任务时长数小时 | 中 | masklet 缓存按帧断点 + 续跑 + 复核不重推全片；进度 UI 明示 ETA；CPU 机器提示不适用 |
| 大 transformer 在 CoreML ANE 回退 / DirectML 算子边角 | 中 | CoreML 指定 GPU 而非 ANE；FP16 混合精度导出（归一化层 FP32）；发布前逐 EP 冒烟 |
| macOS 公证/Windows 签名 CI 卡点 | 低 | 按社区成熟方案（临时 keychain + notarytool）提前搭建 |
| ORT 遥测隐私条款 | 低 | 文档披露 + 禁用开关 |

---

## 附 A：与 Python 版参数映射

| Python CLI 参数 | Rust 版对应 |
|---|---|
| `--conf` | 预览屏置信度滑杆（body/face 分离阈值，face=conf-0.1 规则保留） |
| `--mask-style / --blur-strength / --feather / --face-expand` | 遮罩样式卡片（blur/solid/新增 mosaic 像素块样式） |
| `--batch / --workers / --compose-device` | 高级设置（自动为默认） |
| `--mode streaming/batch` | 流水线固定 streaming；batch 兜底路径保留在 CLI |
| `--hwaccel / --codec / --q / --crf` | 视频设置（自动策略 + 专家模式覆写） |

## 附 B：调研信息源（摘要）

- 模型：Ultralytics 官方文档（YOLO26/YOLO11/segment/pose/track）、arXiv 2509.25164(YOLO26)/2502.12524(v12)/2410.13842(D-FINE)、zjykzj/YOLO11Face、insightface SCRFD、OpenCV Zoo YuNet、facebookresearch/sam2、jamtrack-rs / mot-rs
- 运行时：pykeio/ort（docs: linking/execution-providers）、sonos/tract（0.23.4，Metal 后端进展）、huggingface/candle、webonnx/wonnx（已归档）、Tencent/ncnn、intel/openvino-rs、crates.io `ultralytics-inference`
- FFmpeg：trac wiki HWAccelIntro/QuickSync、ffmpeg-sidecar、rusty_ffmpeg、rsmpeg、BtbN/FFmpeg-Builds、NVIDIA Video Codec SDK 文档、Frigate hwaccel 文档
- Flutter：flutter_rust_bridge 官方文档（Stream/codec/集成）、media_kit、Riverpod 3、window_manager、Spotube/HandBrake/Buzz UI 参考
- 极限精度档：facebookresearch/Mask2Former(MODEL_ZOO)、IDEA-Research/MaskDINO、tue-mps/EoMT（CVPR'25）、vietanhdev/segment-anything-2.1-onnx-models（SAM2.1 全系 ONNX）、onnx-community/grounding-dino-tiny-ONNX、idea-research/grounded-sam-2、biubug6/Pytorch_Retinaface（+ OMZ 现成 IR）、ZFTurbo/weighted-boxes-fusion（WBF）、SAHI(arXiv:2202.06934)、SAM2Long(arXiv:2410.16268)、TIER IV Grounded-SAM2 TensorRT 实测、DaVinci Resolve Magic Mask（复核 UI 先例）
