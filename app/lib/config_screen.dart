import 'dart:async';
import 'dart:io' show Platform;
import 'dart:ui' as ui;

import 'package:file_selector/file_selector.dart';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';

import 'main.dart';
import 'prefs.dart';
import 'process_screen.dart';
import 'queue.dart';
import 'src/rust/api/automosaic.dart';

/// 预览调参屏：播放器 + 右侧参数（预设/样式/强度/置信度/设备）。
class ConfigScreen extends StatefulWidget {
  final String path;
  final AppSettings settings;
  final QueueController queue;
  const ConfigScreen({
    super.key,
    required this.path,
    required this.settings,
    required this.queue,
  });

  @override
  State<ConfigScreen> createState() => _ConfigScreenState();
}

/// 右栏分段按钮的紧凑样式：11px 字号 + 8px 薄内边距 + shrinkWrap 去掉
/// 48px 最小点击区 + 压缩密度（330px 窄栏下默认样式换行、段内文字拆行）。
ButtonStyle _segmentCompactStyle() => const ButtonStyle(
      textStyle: WidgetStatePropertyAll(TextStyle(fontSize: 11)),
      // 水平密度不能低于 -1：VisualDensity 每单位 ±4px，-3 叠加 8px 内边距
      // 会变成负内边距把文字裁掉（"自动"只剩"自"）；垂直方向压密度安全
      padding: WidgetStatePropertyAll(
          EdgeInsets.symmetric(horizontal: 10, vertical: 0)),
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      minimumSize: WidgetStatePropertyAll(Size(0, 28)),
      visualDensity: VisualDensity(horizontal: -1, vertical: -3),
    );

class _ConfigScreenState extends State<ConfigScreen> {
  late final Player _player = Player();
  late final VideoController _controller = VideoController(_player);
  StreamSubscription? _durationSub;
  StreamSubscription? _positionSub;
  StreamSubscription? _playingSub;

  Duration _duration = Duration.zero;
  Duration _position = Duration.zero;
  bool _playing = false;
  bool _showProcessed = false;
  bool _showOverlay = true; // 检测框叠加（DESIGN §7.5）
  ui.Image? _processed;
  List<PreviewBox> _personBoxes = [];
  List<PreviewBox> _faceBoxes = [];
  String? _previewError;
  bool _previewBusy = false;
  Timer? _debounce;
  /// 音量跨配置屏/全屏控制栏共享（ValueNotifier）；_lastVolume 记忆静音前电平
  late final ValueNotifier<double> _volumeN;
  double _lastVolume = 100;
  /// 输出目录：null = 原目录输出（与视频同目录）；否则输出到该目录
  String? _outDir;

  late String _preset = widget.settings.preset;
  late String _style = widget.settings.style;
  late double _strength = widget.settings.strength;
  late double _conf = widget.settings.conf;
  // gpu/ane 是 CoreML 计算单元选项（仅 macOS 词表）；非 macOS 的历史遗留值
  // 归一到 auto，避免 SegmentedButton 无选中项
  late String _device = (!Platform.isMacOS &&
          const ['gpu', 'ane'].contains(widget.settings.device))
      ? 'auto'
      : widget.settings.device;
  List<PresetInfo> _presets = [];

  @override
  void initState() {
    super.initState();
    // 打开后不自动播放：加载元数据（时长/进度条），由用户点击播放
    _player.open(Media(widget.path), play: false);
    _volumeN = ValueNotifier(widget.settings.volume.clamp(0.0, 100.0));
    if (_volumeN.value > 0) _lastVolume = _volumeN.value;
    _player.setVolume(_volumeN.value);
    _durationSub = _player.stream.duration.listen((d) => setState(() => _duration = d));
    _playingSub = _player.stream.playing.listen((p) => setState(() => _playing = p));
    _positionSub = _player.stream.position.listen((p) {
      // mpv 载入初期会短暂上报负位置（demuxer 探测阶段）——忽略，
      // 避免污染进度与预览调度
      if (p.isNegative) return;
      setState(() => _position = p);
      _schedulePreviewRefresh();
    });
    _loadPresets();
  }

  Future<void> _loadPresets() async {
    final list = await listPresets(device: _device);
    if (mounted) setState(() => _presets = list);
  }

  @override
  void dispose() {
    _volumeN.dispose();
    _debounce?.cancel();
    _durationSub?.cancel();
    _positionSub?.cancel();
    _playingSub?.cancel();
    _player.dispose();
    _processed?.dispose();
    super.dispose();
  }

  void _persist() {
    widget.settings
      ..preset = _preset
      ..style = _style
      ..strength = _strength
      ..conf = _conf
      ..device = _device;
    widget.settings.save();
  }

  void _schedulePreviewRefresh() {
    if (!_showProcessed) return;
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 350), _refreshProcessed);
  }

  Future<void> _refreshProcessed() async {
    if (_previewBusy || _duration == Duration.zero) return;
    _previewBusy = true;
    try {
      final f = await previewFrame(
        input: widget.path,
        positionSecs: _position.inMilliseconds / 1000.0,
        conf: _conf,
        device: _device,
        preset: _preset,
        style: _style,
        strength: _strength.round(),
      );
      final img = await decodeRgba(f.rgba, f.width, f.height);
      if (mounted) {
        setState(() {
          _processed?.dispose();
          _processed = img;
          _personBoxes = f.persons;
          _faceBoxes = f.faces;
          _previewError = null;
        });
      }
    } catch (e) {
      if (mounted) setState(() => _previewError = e.toString());
    } finally {
      _previewBusy = false;
    }
  }

  String get _outputPath {
    final i = widget.path.lastIndexOf('.');
    final tag =
        {'speed': 'sp', 'balanced': 'bal', 'accurate': 'acc', 'extreme': 'ext', 'archive': 'arch'}[_preset];
    final ext = widget.settings.container;
    if (_outDir != null) {
      final name = widget.path.split('/').last;
      final stem = name.contains('.')
          ? name.substring(0, name.lastIndexOf('.'))
          : name;
      return '$_outDir/${stem}_mosaic_$tag.$ext';
    }
    final base = i > 0 ? widget.path.substring(0, i) : widget.path;
    return '${base}_mosaic_$tag.$ext';
  }

  QueueJob _makeJob() => QueueJob(
        input: widget.path,
        output: _outputPath,
        openAfterFinish: widget.settings.openAfterFinish,
        archive: _preset == 'archive',
        analyzeDrainFile: widget.settings.analyzeDrainFile,
        archiveSamSize: widget.settings.archiveSamSize,
        opts: ProcessOptions(
          preset: _preset,
          conf: _conf,
          device: _device,
          style: _style,
          strength: _strength.round(),
          modelPath: null,
          hwaccel: 'auto',
          encoder: widget.settings.encoder,
          bitrate: widget.settings.bitrate,
          face: widget.settings.face,
          faceExpand: widget.settings.faceExpand,
          faceRoi: widget.settings.faceRoi,
          track: widget.settings.track,
          maskSmooth: widget.settings.maskSmooth,
          maskEma: widget.settings.maskEma,
          landmarkExpand: widget.settings.landmarkExpand,
          detectEvery: widget.settings.detectEvery,
          batch: widget.settings.batch,
          tta: widget.settings.tta,
          gmc: widget.settings.gmc,
        ),
      );

  Future<void> _pickOutDir() async {
    final dir = await getDirectoryPath();
    if (dir != null && mounted) setState(() => _outDir = dir);
  }

  /// 开始处理：入队并跳转处理屏（空闲队列立即启动）。
  void _start() {
    _submit(pushToProcess: true);
  }

  /// 加入队列：留在本页（可继续添加其他视频），snackbar 提供入口。
  void _addToQueue() {
    _submit(pushToProcess: false);
  }

  void _submit({required bool pushToProcess}) {
    _persist();
    _player.pause();
    widget.queue.add(_makeJob());
    if (!pushToProcess) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(S.t('已加入队列：', 'Added to queue: ') + _outputPath.split('/').last),
          action: SnackBarAction(
            label: S.t('查看队列', 'View queue'),
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute(
                builder: (_) => ProcessScreen(
                  settings: widget.settings,
                  queue: widget.queue,
                ),
              ),
            ),
          ),
        ),
      );
      return;
    }
    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder: (_) => ProcessScreen(
          settings: widget.settings,
          queue: widget.queue,
        ),
      ),
    );
  }

  /// 自适应时长格式：<1h 用 MM:SS，≥1h 用 H:MM:SS（超长视频不再显示 90:00 这类）。
  String _fmt(Duration d) {
    if (d.isNegative) return '00:00';
    if (d.inHours > 0) {
      return '${d.inHours}:${(d.inMinutes % 60).toString().padLeft(2, '0')}:'
          '${(d.inSeconds % 60).toString().padLeft(2, '0')}';
    }
    return '${d.inMinutes.toString().padLeft(2, '0')}:${(d.inSeconds % 60).toString().padLeft(2, '0')}';
  }

  /// 显示用位置：时长已知时钳到时长内（载入初期 position 流的脏值可能超过
  /// 时长，如 6 秒视频显示 00:59）。
  Duration get _shownPosition => (_duration > Duration.zero && _position > _duration)
      ? _duration
      : _position;

  @override
  Widget build(BuildContext context) {
    final presetInfo = _presets.where((p) => p.id == _preset).firstOrNull;
    return Scaffold(
      appBar: DraggableAppBar(title: Text(widget.path.split('/').last)),
      body: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 左：播放器（原画面/修改后切换）+ 进度条
          Expanded(
            flex: 3,
            child: Card(
              margin: const EdgeInsets.all(16),
              child: Padding(
                padding: const EdgeInsets.all(12),
                child: Column(
                  children: [
                    Expanded(child: _viewArea()),
                    const SizedBox(height: 8),
                    _controls(),
                  ],
                ),
              ),
            ),
          ),
          // 右：参数
          SizedBox(
            width: 330,
            child: Card(
              margin: const EdgeInsets.fromLTRB(0, 16, 16, 16),
              child: ListView(
                padding: const EdgeInsets.all(20),
                children: [
                  _presetSection(presetInfo),
                  const SizedBox(height: 16),
                  Text(S.t('遮罩样式', 'Mask style'),
                      style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
                  const SizedBox(height: 8),
                  SegmentedButton<String>(
                    style: _segmentCompactStyle(),
                    showSelectedIcon: false,
                    segments: [
                      ButtonSegment(value: 'mosaic', label: Text(S.t('马赛克', 'Mosaic'), maxLines: 1)),
                      ButtonSegment(value: 'blur', label: Text(S.t('模糊', 'Blur'), maxLines: 1)),
                      ButtonSegment(value: 'solid', label: Text(S.t('纯黑', 'Solid'), maxLines: 1)),
                    ],
                    selected: {_style},
                    onSelectionChanged: (s) {
                      setState(() => _style = s.first);
                      _schedulePreviewRefresh();
                    },
                  ),
                  if (_style == 'blur') ...[
                    const SizedBox(height: 8),
                    _blurWarning(),
                  ],
                  const SizedBox(height: 20),
                  Text(S.t('强度（${_strength.round()}）', 'Strength (${_strength.round()})'),
                      style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
                  Slider(
                    value: _strength,
                    min: 4,
                    max: 96,
                    divisions: 23,
                    onChanged: (v) {
                      setState(() => _strength = v);
                      _schedulePreviewRefresh();
                    },
                  ),
                  Text(S.t('置信度（${_conf.toStringAsFixed(2)}）', 'Confidence (${_conf.toStringAsFixed(2)})'),
                      style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
                  Slider(
                    value: _conf,
                    min: 0.1,
                    max: 0.8,
                    divisions: 14,
                    onChanged: (v) {
                      setState(() => _conf = v);
                      _schedulePreviewRefresh();
                    },
                  ),
                  Text(S.t('推理设备', 'Device'),
                      style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
                  const SizedBox(height: 8),
                  SegmentedButton<String>(
                    style: _segmentCompactStyle(),
                    showSelectedIcon: false,
                    segments: [
                      ButtonSegment(value: 'auto', label: Text(S.t('自动', 'Auto'), maxLines: 1)),
                      // gpu/ane 为 CoreML 计算单元选项，仅 macOS；Linux 设备词表
                      // = auto/webgpu（WebGPU EP）/cpu，Windows auto=DirectML
                      if (Platform.isMacOS) ...[
                        const ButtonSegment(value: 'gpu', label: Text('GPU', maxLines: 1)),
                        const ButtonSegment(value: 'ane', label: Text('NPU', maxLines: 1)),
                      ],
                      const ButtonSegment(value: 'cpu', label: Text('CPU', maxLines: 1)),
                      if (Platform.isLinux)
                        const ButtonSegment(value: 'webgpu', label: Text('WebGPU', maxLines: 1)),
                    ],
                    selected: {_device},
                    onSelectionChanged: (s) {
                      setState(() => _device = s.first);
                      _loadPresets();
                      _schedulePreviewRefresh();
                    },
                  ),
                  const SizedBox(height: 4),
                  Text(
                    switch (_device) {
                      'gpu' => S.t('CoreML：CPU + GPU（算子兼容性最稳）',
                          'CoreML: CPU + GPU (most compatible)'),
                      'ane' => S.t('CoreML：CPU + 神经网络引擎（能效最优）',
                          'CoreML: CPU + Neural Engine (best efficiency)'),
                      'cpu' => Platform.isLinux
                          ? S.t('ONNX Runtime CPU（不使用 WebGPU）',
                              'ONNX Runtime CPU (no WebGPU)')
                          : S.t('ONNX Runtime CPU（不使用 CoreML）',
                              'ONNX Runtime CPU (no CoreML)'),
                      'webgpu' => S.t(
                          'WebGPU（Dawn/Vulkan，实验）：大模型有加速，'
                          '小模型与 CPU 持平；输出与 CPU 逐比特一致',
                          'WebGPU (Dawn/Vulkan, experimental): faster for '
                          'large models, on par for small; bit-identical output'),
                      _ => Platform.isLinux
                          ? S.t(
                              'WebGPU（Dawn/Vulkan）：GPU 加速，初始化失败自动回退 CPU',
                              'WebGPU (Dawn/Vulkan): GPU accelerated, '
                              'auto-falls back to CPU on init failure')
                          : S.t('CoreML：CPU/GPU/NPU 自动调度（推荐）',
                              'CoreML: CPU/GPU/NPU auto (recommended)'),
                    },
                    style: TextStyle(color: Colors.grey.shade500, fontSize: 11),
                  ),
                  const SizedBox(height: 20),
                  _enhancements(),
                  const SizedBox(height: 24),
                  // 输出目录双态：原目录输出（默认）/ 选目录输出；当前态高亮，
                  // 实际输出路径实时反映在下方路径行
                  Builder(builder: (context) {
                    final custom = _outDir != null;
                    final activeStyle = FilledButton.styleFrom(
                      textStyle: const TextStyle(fontSize: 12),
                      visualDensity: VisualDensity.compact,
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                    );
                    final inactiveStyle = OutlinedButton.styleFrom(
                      textStyle: const TextStyle(fontSize: 12),
                      visualDensity: VisualDensity.compact,
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                    );
                    final dirName = custom
                        ? (_outDir!.split('/').last.isEmpty
                            ? S.t('输出目录', 'Output folder')
                            : _outDir!.split('/').last)
                        : S.t('输出目录', 'Output folder');
                    return Row(
                      children: [
                        Expanded(
                          child: Tooltip(
                            message: custom ? _outDir! : S.t('选择输出目录', 'Choose output folder'),
                            child: custom
                                ? FilledButton.icon(
                                    style: activeStyle,
                                    onPressed: _pickOutDir,
                                    icon: const Icon(Icons.drive_file_move_outline, size: 15),
                                    label: Text(dirName, maxLines: 1, overflow: TextOverflow.ellipsis),
                                  )
                                : OutlinedButton.icon(
                                    style: inactiveStyle,
                                    onPressed: _pickOutDir,
                                    icon: const Icon(Icons.drive_file_move_outline, size: 15),
                                    label: Text(S.t('输出目录', 'Output folder'), maxLines: 1, overflow: TextOverflow.ellipsis),
                                  ),
                          ),
                        ),
                        const SizedBox(width: 8),
                        Expanded(
                          child: !custom
                              ? FilledButton.icon(
                                  style: activeStyle,
                                  onPressed: () {},
                                  icon: const Icon(Icons.folder, size: 15),
                                  label: Text(S.t('原目录输出', 'Same as input'), maxLines: 1, overflow: TextOverflow.ellipsis),
                                )
                              : OutlinedButton.icon(
                                  style: inactiveStyle,
                                  onPressed: () => setState(() => _outDir = null),
                                  icon: const Icon(Icons.folder, size: 15),
                                  label: Text(S.t('原目录输出', 'Same as input'), maxLines: 1, overflow: TextOverflow.ellipsis),
                                ),
                        ),
                      ],
                    );
                  }),
                  const SizedBox(height: 8),
                  FilledButton.icon(
                    onPressed: _start,
                    icon: const Icon(Icons.play_arrow),
                    label: Text(_preset == 'archive'
                        ? S.t('开始分析（两阶段）', 'Analyze (two-phase)')
                        : S.t('开始处理', 'Start processing')),
                  ),
                  const SizedBox(height: 8),
                  OutlinedButton.icon(
                    onPressed: _addToQueue,
                    icon: const Icon(Icons.playlist_add),
                    label: Text(S.t('加入队列', 'Add to queue')),
                  ),
                  const SizedBox(height: 8),
                  Tooltip(
                    message: _outputPath,
                    child: Text(
                      S.t('输出 → ', 'Output → ') + _outputPath,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(color: Colors.grey.shade500, fontSize: 12),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// 增强选项（按片生效，随任务入队；全部默认开，A/B 与特殊场景用）。
  /// 2026-08-21 由设置屏迁入——导入视频后即可调整，无需事前去设置。
  Widget _enhancements() {
    final s = widget.settings;
    void set(void Function(AppSettings x) mut) {
      setState(() => mut(s));
      s.save();
    }

    // 悬停 300ms 出通俗说明：为什么开、有什么好处、什么情况才关
    Widget tile(String title, bool value, String tip, void Function(bool) onChanged) {
      return Tooltip(
        message: tip,
        waitDuration: const Duration(milliseconds: 300),
        constraints: const BoxConstraints(maxWidth: 320),
        child: SwitchListTile(
          dense: true,
          contentPadding: EdgeInsets.zero,
          title: Text(title, style: const TextStyle(fontSize: 13)),
          value: value,
          onChanged: onChanged,
        ),
      );
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(S.t('增强选项', 'Enhancements'),
            style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
        tile(
          S.t('目标跟踪（ByteTrack）', 'Object tracking (ByteTrack)'),
          s.track,
          S.t(
          '开启后同一个人在连续画面中被认作同一身份：偶尔回头、被遮挡或'
          '某几帧没检测到时，马赛克仍跟着人走，不闪烁、不漏打。关闭后每帧'
          '独立判断，快速运动或遮挡时会闪烁、漏打。日常建议始终开启，'
          '关闭仅用于对比调试。',
          'The same person is recognized across frames: brief look-aways, '
          'occlusions, or missed detections keep the mosaic following the '
          'person — no flicker, no gaps. Off: each frame is judged alone, so '
          'fast motion or occlusion flickers and misses coverage. Keep on for '
          'daily use; disable only for A/B testing.'),
          (v) => set((x) => x.track = v),
        ),
        tile(
          S.t('mask 时序平滑', 'Mask smoothing'),
          s.maskSmooth,
          S.t(
          '把上一帧的遮罩范围略微扩大后与当前帧合并：遮挡边缘的抖动、'
          '"一帧有一帧没有"的闪烁会被消除。零性能开销，关闭仅在排查'
          '检测问题时有意义。',
          'Dilates the previous mask and merges it with the current frame: '
          'edge jitter and on-off flicker are removed. Zero performance cost; '
          'only disable when debugging detections.'),
          (v) => set((x) => x.maskSmooth = v),
        ),
        tile(
          S.t('per-ID mask EMA', 'Per-person mask EMA'),
          s.maskEma,
          S.t(
          '按人物身份对遮罩做时序平均：同一个人的遮罩边缘逐帧稳定，不再'
          '"呼吸"般忽大忽小；多人场景每人独立平滑、互不影响。关闭则遮罩'
          '严格跟随每帧检测结果，边缘会随检测轻微抖动。',
          'Averages masks per person over time: edges stay stable instead of '
          '"breathing"; each person is smoothed independently. Off: masks '
          'strictly follow each detection and may jitter slightly.'),
          (v) => set((x) => x.maskEma = v),
        ),
        tile(
          S.t('landmark 外扩（抗转头）', 'Landmark expansion'),
          s.landmarkExpand,
          S.t(
          '利用检测到的双眼位置自适应调整人脸遮罩：正脸时标准大小，转头'
          '侧脸时自动加宽，盖住侧脸轮廓和头发。关闭则用固定外扩值，'
          '转头幅度大时可能盖不全。',
          'Uses detected eye positions to size the face mask: standard when '
          'facing the camera, automatically wider on head turns to cover the '
          'profile and hair. Off: fixed padding may miss strong head turns.'),
          (v) => set((x) => x.landmarkExpand = v),
        ),
        tile(
          S.t('全局运动补偿（GMC）', 'Global motion comp. (GMC)'),
          s.gmc,
          S.t(
          '镜头本身平移（手持、跟拍）时估计相机运动，让检测框和遮罩跟着'
          '画面走：运动镜头下跟踪不丢、人物移出画面后遮罩不残留原地。'
          '固定机位拍摄无副作用，可以一直开着。',
          'When the camera pans (handheld, follow shots), motion is estimated '
          'so boxes and masks follow the scene: tracking holds and masks do '
          'not linger after people leave the frame. No effect on static '
          'shots — safe to keep on.'),
          (v) => set((x) => x.gmc = v),
        ),
        // 两行排布：第一行"翻转TTA"，第二行自动/开/关（全宽，同遮罩样式）
        Tooltip(
          message: S.t(
              '每帧额外做一次水平翻转推理再合并结果：能捡回正着看容易'
              '漏掉的目标，打码更不容易漏，代价是推理时间翻倍。'
              '"自动"= 极致档开启、其余档关闭；追求速度选"关"，'
              '追求不漏选"开"。',
              'Runs an extra horizontally-flipped inference per frame and '
              'merges results: catches easy-to-miss targets, fewer gaps, at '
              '2x inference time. Auto = on for the Extreme preset; Off for '
              'speed, On for coverage.'),
          waitDuration: const Duration(milliseconds: 300),
          constraints: const BoxConstraints(maxWidth: 320),
          child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(S.t('翻转TTA', 'Flip TTA'), style: const TextStyle(fontSize: 13)),
              const SizedBox(height: 6),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<int>(
                  style: _segmentCompactStyle(),
                    showSelectedIcon: false,
                  segments: [
                    ButtonSegment(value: 0, label: Text(S.t('自动', 'Auto'), maxLines: 1)),
                    ButtonSegment(value: 1, label: Text(S.t('开', 'On'), maxLines: 1)),
                    ButtonSegment(value: 2, label: Text(S.t('关', 'Off'), maxLines: 1)),
                  ],
                  selected: {s.tta},
                  onSelectionChanged: (v) => set((x) => x.tta = v.first),
                ),
              ),
            ],
          ),
        ),
        )
      ],
    );
  }

  /// 模型与后端明细（PresetDetail 结构化展示）：图标 + 标签 + 值的紧凑
  /// 行，与处理屏任务信息同一控件语言；值超宽省略号 + tooltip。
  Widget _detailRows(String presetLabel, PresetDetail d) {
    final c = Theme.of(context).colorScheme;
    Widget row(IconData icon, String label, String value, {String? tip}) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 2),
        child: Row(
          children: [
            Icon(icon, size: 13, color: c.primary),
            const SizedBox(width: 6),
            SizedBox(
              width: 32,
              child: Text(label,
                  style: TextStyle(fontSize: 10.5, color: c.onSurfaceVariant)),
            ),
            const SizedBox(width: 4),
            Expanded(
              child: Tooltip(
                message: tip ?? value,
                child: Text(value,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                        fontSize: 11.5, fontWeight: FontWeight.w500)),
              ),
            ),
          ],
        ),
      );
    }

    final face = d.faceModel == null
        ? S.t('未启用', 'off')
        : '${d.faceModel} · ${d.faceSizeMb.toStringAsFixed(1)}MB'
            '${d.faceBatch ? ' · b4' : ''}';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        row(Icons.memory, S.t('人体', 'Body'),
            '${d.bodyModel} · ${d.bodySizeMb.toStringAsFixed(1)}MB'
            '${d.bodyBatch ? ' · b4' : ''}'),
        row(Icons.face, S.t('人脸', 'Face'), face),
        row(
          Icons.timelapse,
          S.t('推理', 'Infer'),
          '${S.rust(presetLabel)} · ${d.detectEvery <= 1 ? S.t('逐帧', 'every frame') : S.t('隔 ${d.detectEvery} 帧', 'every ${d.detectEvery} frames')}'
          ' · conf ${d.conf.toStringAsFixed(2)}',
        ),
        row(Icons.developer_board, S.t('后端', 'Device'), S.rust(d.backendDesc)),
      ],
    );
  }

  Widget _presetSection(PresetInfo? info) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(S.t('质量预设', 'Preset'),
            style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
        const SizedBox(height: 8),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final p in _presets)
              ChoiceChip(
                label: Text(S.rust(p.label)),
                selected: _preset == p.id,
                // 五档均可选择：模型缺失时下方卡片给出指引（与其他档一致，
                // 报错在启动分析时明确抛出）
                onSelected: (sel) {
                  if (sel) {
                    setState(() {
                      _preset = p.id;
                      _conf = p.conf; // 联动预设默认置信度
                    });
                    _schedulePreviewRefresh();
                  }
                },
              ),
          ],
        ),
        const SizedBox(height: 10),
        Container(
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(8),
            color: Theme.of(context).colorScheme.surfaceContainerLow,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(children: [
                Icon(
                  info == null
                      ? Icons.hourglass_empty
                      : (info.available ? Icons.check_circle : Icons.error_outline),
                  size: 16,
                  color: info == null
                      ? Colors.grey
                      : (info.available ? const Color(0xFF4CD964) : Theme.of(context).colorScheme.error),
                ),
                const SizedBox(width: 6),
                Text(S.t('模型与后端', 'Model & backend'),
                    style: Theme.of(context).textTheme.titleSmall),
              ]),
              const SizedBox(height: 4),
              if (info == null)
                Text(S.t('查询中…', 'Loading…'),
                    style: TextStyle(
                        fontSize: 11,
                        color: Theme.of(context).colorScheme.onSurfaceVariant))
              else if (!info.available || info.detail == null)
                // 缺模型指引 / 未实现说明（desc 只承载不可用态）
                Text(info.desc,
                    style: TextStyle(
                        fontSize: 11, height: 1.5,
                        color: Theme.of(context).colorScheme.onSurfaceVariant))
              else
                _detailRows(info.label, info.detail!),
            ],
          ),
        ),
      ],
    );
  }

  /// blur 仅观感、不能可靠匿名化的警示（DESIGN §6 实测修正）。
  Widget _blurWarning() {
    return Container(
      padding: const EdgeInsets.all(8),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(6),
        color: const Color(0xFF33270E),
        border: Border.all(color: const Color(0xFF8A6D1A)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Icon(Icons.warning_amber_rounded, size: 16, color: Color(0xFFE0B44C)),
          SizedBox(width: 6),
          Expanded(
            child: Text(
              S.t('模糊无法可靠匿名化：检测器对模糊图像仍然鲁棒。需要匿名化请使用马赛克或纯黑。',
                  'Blur alone does not anonymize: detectors stay robust to '
                  'blurred images. Use mosaic or solid when anonymity matters.'),
              style: TextStyle(fontSize: 11, color: Color(0xFFE0B44C), height: 1.4),
            ),
          ),
        ],
      ),
    );
  }

  Widget _viewArea() {
    if (_showProcessed) {
      if (_previewError != null) {
        return Center(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Text(S.t('预览失败：', 'Preview failed: ') + (_previewError ?? ''),
                style: const TextStyle(fontSize: 12)),
          ),
        );
      }
      if (_processed == null) {
        return Center(child: Text(S.t('正在生成修改后画面…', 'Generating masked preview…')));
      }
      return Column(
        children: [
          Expanded(
            child: ClipRRect(
              borderRadius: BorderRadius.circular(8),
              child: FittedBox(
                fit: BoxFit.contain,
                child: Stack(
                  children: [
                    RawImage(image: _processed),
                    // 检测框叠加（DESIGN §7.5：person 绿框 / 人脸橙框 + 双眼点）
                    if (_showOverlay)
                      CustomPaint(
                        size: Size(_processed!.width.toDouble(),
                            _processed!.height.toDouble()),
                        painter: _DetectionPainter(
                            persons: _personBoxes, faces: _faceBoxes),
                      ),
                  ],
                ),
              ),
            ),
          ),
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Text(S.t('显示检测框', 'Show detections'),
                  style: TextStyle(color: Colors.grey.shade400, fontSize: 12)),
              Switch(
                value: _showOverlay,
                onChanged: (v) => setState(() => _showOverlay = v),
              ),
            ],
          ),
        ],
      );
    }
    // 去掉 media_kit 内置控件（其进度条/播放键/音量与应用控制条重复）；
    // 交互全部由下方 _controls 承载
    return ClipRRect(
      borderRadius: BorderRadius.circular(8),
      child: Video(controller: _controller, controls: NoVideoControls),
    );
  }

  Widget _controls() {
    final posMs = _position.inMilliseconds.toDouble();
    final durMs = _duration.inMilliseconds.toDouble().clamp(1, double.infinity);
    return Column(
      children: [
        Row(
          children: [
            IconButton(
              onPressed: () => _playing ? _player.pause() : _player.play(),
              icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
            ),
            Text('${_fmt(_shownPosition)} / ${_fmt(_duration)}',
                style: TextStyle(color: Colors.grey.shade400, fontSize: 12)),
            const Spacer(),
            _volumeControl(),
            const SizedBox(width: 4),
            IconButton(
              tooltip: S.t('全屏（双击画面或 Esc 退出）', 'Fullscreen (double-click or Esc to exit)'),
              onPressed: _enterFullscreen,
              icon: const Icon(Icons.fullscreen),
            ),
            const SizedBox(width: 8),
            SegmentedButton<bool>(
              segments: [
                ButtonSegment(value: false, label: Text(S.t('原画面', 'Original'))),
                ButtonSegment(value: true, label: Text(S.t('修改后画面', 'Masked'))),
              ],
              selected: {_showProcessed},
              onSelectionChanged: (s) {
                setState(() => _showProcessed = s.first);
                if (_showProcessed) {
                  _refreshProcessed();
                }
              },
            ),
          ],
        ),
        SliderTheme(
          data: SliderThemeData(
            trackHeight: 4,
            thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 7),
            overlayShape: const RoundSliderOverlayShape(overlayRadius: 14),
          ),
          child: Slider(
            value: posMs.clamp(0.0, durMs).toDouble(),
            max: durMs.toDouble(),
            onChanged: (v) => _player.seek(Duration(milliseconds: v.toInt())),
          ),
        ),
      ],
    );
  }

  /// 音量：图标点按静音/恢复 + 窄滑杆（拖动实时生效，松手持久化）。
  Widget _volumeControl() {
    return _VolumeControl(
      volume: _volumeN,
      onChanged: _applyVolume,
      onCommitted: _commitVolume,
    );
  }

  void _applyVolume(double v) {
    _volumeN.value = v.clamp(0.0, 100.0);
    if (_volumeN.value > 0) _lastVolume = _volumeN.value;
    _player.setVolume(_volumeN.value);
  }

  void _commitVolume(double v) {
    widget.settings.volume = v;
    widget.settings.save();
  }

  double get _unmuteLevel => _lastVolume;

  /// 窗口内全屏：视频铺满应用窗口的播放路由（带控制栏）。不改变播放
  /// 状态——暂停进则保持暂停（空格/播放键起播）。
  Future<void> _enterFullscreen() async {
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        fullscreenDialog: true,
        builder: (_) => _FullscreenView(
          controller: _controller,
          volume: _volumeN,
          onVolume: _applyVolume,
          onVolumeCommitted: _commitVolume,
          unmuteLevel: () => _unmuteLevel,
        ),
      ),
    );
  }
}

/// 音量控件（调参屏控制条与全屏控制栏共用）：静音/恢复图标 + 窄滑杆。
class _VolumeControl extends StatelessWidget {
  final ValueNotifier<double> volume;
  final void Function(double) onChanged;
  final void Function(double) onCommitted;
  final double Function()? unmuteLevel;

  const _VolumeControl({
    required this.volume,
    required this.onChanged,
    required this.onCommitted,
    this.unmuteLevel,
  });

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<double>(
      valueListenable: volume,
      builder: (context, v, _) {
        return Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            IconButton(
              tooltip: v > 0 ? S.t('静音', 'Mute') : S.t('取消静音', 'Unmute'),
              visualDensity: VisualDensity.compact,
              onPressed: () => onChanged(v > 0 ? 0 : (unmuteLevel?.call() ?? 100)),
              icon: Icon(v > 0 ? Icons.volume_up : Icons.volume_off, size: 20),
            ),
            SizedBox(
              width: 84,
              child: SliderTheme(
                data: const SliderThemeData(
                  trackHeight: 3,
                  thumbShape: RoundSliderThumbShape(enabledThumbRadius: 6),
                  overlayShape: RoundSliderOverlayShape(overlayRadius: 12),
                ),
                child: Slider(
                  value: v.clamp(0.0, 100.0),
                  min: 0,
                  max: 100,
                  onChanged: onChanged,
                  onChangeEnd: onCommitted,
                ),
              ),
            ),
          ],
        );
      },
    );
  }
}

/// 全屏播放（真·窗口全屏）：windowManager 原生全屏 + 本路由承载播放与
/// 控制栏（播放/进度/时间/音量/退出）。Esc / 双击画面 / 右上按钮退出。
class _FullscreenView extends StatefulWidget {
  final VideoController controller;
  final ValueNotifier<double> volume;
  final void Function(double) onVolume;
  final void Function(double) onVolumeCommitted;
  final double Function()? unmuteLevel;

  const _FullscreenView({
    required this.controller,
    required this.volume,
    required this.onVolume,
    required this.onVolumeCommitted,
    this.unmuteLevel,
  });

  @override
  State<_FullscreenView> createState() => _FullscreenViewState();
}

class _FullscreenViewState extends State<_FullscreenView> {
  Player get _player => widget.controller.player;
  StreamSubscription? _posSub;
  StreamSubscription? _durSub;
  StreamSubscription? _playSub;
  Duration _position = Duration.zero;
  Duration _duration = Duration.zero;
  bool _playing = false;

  @override
  void initState() {
    super.initState();
    // duration/position 事件在视频加载时已发过，此处晚订阅收不到——
    // 用 player.state 的同步快照做初值，流只负责后续增量
    _position = _player.state.position;
    _duration = _player.state.duration;
    _playing = _player.state.playing;
    _posSub = _player.stream.position.listen((p) {
      if (!p.isNegative && mounted) setState(() => _position = p);
    });
    _durSub = _player.stream.duration.listen((d) {
      if (mounted) setState(() => _duration = d);
    });
    _playSub = _player.stream.playing.listen((p) {
      if (mounted) setState(() => _playing = p);
    });
  }

  @override
  void dispose() {
    _posSub?.cancel();
    _durSub?.cancel();
    _playSub?.cancel();
    super.dispose();
  }

  void _exit() {
    Navigator.of(context).pop();
  }

  String _fmt(Duration d) {
    if (d.isNegative) return '00:00';
    if (d.inHours > 0) {
      return '${d.inHours}:${(d.inMinutes % 60).toString().padLeft(2, '0')}:'
          '${(d.inSeconds % 60).toString().padLeft(2, '0')}';
    }
    return '${d.inMinutes.toString().padLeft(2, '0')}:${(d.inSeconds % 60).toString().padLeft(2, '0')}';
  }

  @override
  Widget build(BuildContext context) {
    final posMs = _position.inMilliseconds.toDouble();
    final durMs = _duration.inMilliseconds.toDouble().clamp(1, double.infinity);
    return Scaffold(
      backgroundColor: Colors.black,
      // Focus(autofocus) 是关键：CallbackShortcuts 需要焦点链上的节点接收
      // 按键，否则 Esc 无人消费（上一版不生效的原因）
      body: Focus(
        autofocus: true,
        child: CallbackShortcuts(
          bindings: {
            const SingleActivator(LogicalKeyboardKey.escape): _exit,
            const SingleActivator(LogicalKeyboardKey.space): () =>
                _playing ? _player.pause() : _player.play(),
          },
          child: GestureDetector(
            onDoubleTap: _exit,
            child: Stack(
              fit: StackFit.expand,
              children: [
                Video(
                  controller: widget.controller,
                  controls: NoVideoControls,
                  fit: BoxFit.contain,
                ),
                // 底部控制栏（渐变浮层）
                Align(
                  alignment: Alignment.bottomCenter,
                  child: IgnorePointer(
                    ignoring: false,
                    child: Container(
                      decoration: const BoxDecoration(
                        gradient: LinearGradient(
                          begin: Alignment.bottomCenter,
                          end: Alignment.topCenter,
                          colors: [Colors.black87, Colors.transparent],
                        ),
                      ),
                      padding: const EdgeInsets.fromLTRB(24, 32, 24, 12),
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          SliderTheme(
                            data: const SliderThemeData(
                              trackHeight: 4,
                              thumbShape:
                                  RoundSliderThumbShape(enabledThumbRadius: 7),
                              overlayShape:
                                  RoundSliderOverlayShape(overlayRadius: 14),
                            ),
                            child: Slider(
                              value: posMs.clamp(0.0, durMs).toDouble(),
                              max: durMs.toDouble(),
                              onChanged: (v) => _player
                                  .seek(Duration(milliseconds: v.toInt())),
                            ),
                          ),
                          Row(
                            children: [
                              IconButton(
                                tooltip: _playing ? S.t('暂停', 'Pause') : S.t('播放', 'Play'),
                                onPressed: () => _playing
                                    ? _player.pause()
                                    : _player.play(),
                                icon: Icon(
                                    _playing ? Icons.pause : Icons.play_arrow,
                                    color: Colors.white),
                              ),
                              Text(
                                '${_fmt(_position)} / ${_fmt(_duration)}',
                                style: const TextStyle(
                                    color: Colors.white70, fontSize: 12),
                              ),
                              const Spacer(),
                              _VolumeControl(
                                volume: widget.volume,
                                onChanged: widget.onVolume,
                                onCommitted: widget.onVolumeCommitted,
                                unmuteLevel: widget.unmuteLevel,
                              ),
                              IconButton(
                                tooltip: S.t('退出全屏', 'Exit fullscreen'),
                                onPressed: _exit,
                                icon: const Icon(Icons.fullscreen_exit,
                                    color: Colors.white),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// 检测框叠加层（DESIGN §7.5）：person 绿框（半透明填充+描边）、人脸橙框、
/// 双眼 landmark 点。坐标为原始视频像素，画布 size 由 Stack 拉伸到图像尺寸。
class _DetectionPainter extends CustomPainter {
  final List<PreviewBox> persons;
  final List<PreviewBox> faces;

  _DetectionPainter({required this.persons, required this.faces});

  @override
  void paint(Canvas canvas, Size size) {
    final personPaint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = size.width / 480
      ..color = const Color(0xCC4CD964);
    final personFill = Paint()
      ..style = PaintingStyle.fill
      ..color = const Color(0x144CD964);
    final facePaint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = size.width / 640
      ..color = const Color(0xE6FF9800);
    final eyePaint = Paint()
      ..style = PaintingStyle.fill
      ..color = const Color(0xE6FF9800);

    for (final b in persons) {
      final r = Rect.fromLTRB(b.x1, b.y1, b.x2, b.y2);
      canvas.drawRect(r, personFill);
      canvas.drawRect(r, personPaint);
    }
    final eyeR = size.width / 480;
    for (final b in faces) {
      canvas.drawRect(Rect.fromLTRB(b.x1, b.y1, b.x2, b.y2), facePaint);
      final eyes = b.eyes;
      if (eyes != null && eyes.length == 4) {
        canvas.drawCircle(Offset(eyes[0], eyes[1]), eyeR, eyePaint);
        canvas.drawCircle(Offset(eyes[2], eyes[3]), eyeR, eyePaint);
      }
    }
  }

  @override
  bool shouldRepaint(_DetectionPainter old) =>
      old.persons != persons || old.faces != faces;
}
