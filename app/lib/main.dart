import 'dart:async';
import 'dart:io' show Platform;
import 'dart:ui' show PlatformDispatcher;

import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:media_kit/media_kit.dart';
import 'package:window_manager/window_manager.dart';

import 'config_screen.dart';
import 'prefs.dart';
import 'queue.dart';
import 'queue_screen.dart';
import 'settings_screen.dart';
import 'src/rust/frb_generated.dart';

/// 全局单例：设置（持久化）与任务队列（跨屏共享）。
final AppSettings appSettings = AppSettings();
final QueueController appQueue = QueueController();

// --------------------------------------------------------------------------- //
// 轻量双语（DESIGN §7.1 多语言）：中文为源文案，英文成对提供；
/// `S.t('设置', 'Settings')`。语言在 main 按设置/系统初始化，外观组可切。
/// 主题同理从设置读取（system/light/dark）。
class S {
  static bool _en = false;
  static String t(String zh, String en) => _en ? en : zh;

  /// 按设置值（system/zh/en）解析；system 跟随系统语言。
  static void init(String setting) {
    final lang = setting == 'system'
        ? PlatformDispatcher.instance.locale.languageCode
        : setting;
    _en = lang.toLowerCase().startsWith('en');
  }

  /// Rust 侧返回的固定文案（预设名/后端描述/解码标注等数据字段）英译；
  /// 未命中的文案原样返回（新后端/新预设自动兜底为中文）。
  static String rust(String zh) => t(zh, switch (zh) {
        '速度' => 'Speed',
        '均衡' => 'Balanced',
        '准确' => 'Accurate',
        '极致' => 'Extreme',
        '极限·档案级' => 'Archive',
        '自动' => 'Auto',
        'GPU（CoreML CPU+GPU）' => 'GPU (CoreML CPU+GPU)',
        'NPU（CoreML CPU+神经引擎）' => 'NPU (CoreML CPU+Neural Engine)',
        'DirectML（DX12）' => 'DirectML (DX12)',
        'CoreML（CPU/GPU/NPU 自动调度）' => 'CoreML (CPU/GPU/NPU auto)',
        'CoreML（CPU+GPU）' => 'CoreML (CPU+GPU)',
        'CoreML（CPU+NPU）' => 'CoreML (CPU+NPU)',
        'CPU（ONNX Runtime）' => 'CPU (ONNX Runtime)',
        'WebGPU（Dawn/Vulkan，实验）' => 'WebGPU (Dawn/Vulkan, experimental)',
        'OpenVINO（Intel CPU/GPU）' => 'OpenVINO (Intel CPU/GPU)',
        '软件解码' => 'Software decode',
        _ => zh,
      });
}

Future<void> main() async {
  // shared_preferences 等插件在 runApp 前走平台通道，必须先初始化 binding，
  // 否则 main 抛异常中断 → 窗口黑屏（打包产物实测）
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  MediaKit.ensureInitialized();
  await appSettings.load();
  S.init(appSettings.language);
  // 自绘标题栏（DESIGN §7.1）：隐藏系统标题栏；macOS 交通灯仍悬浮可用，
  // Linux 无系统装饰需自绘窗口控件（WindowControls）。
  await windowManager.ensureInitialized();
  // 注：不设 backgroundColor: Colors.transparent——透明窗口（isOpaque=NO）
  // 在原生全屏下会失去不透明表面的同步呈现路径，视频画面易撕裂；应用本身
  // 恒为不透明深色背景，无需透明窗口
  const opts = WindowOptions(
    titleBarStyle: TitleBarStyle.hidden,
    minimumSize: Size(960, 620),
  );
  await windowManager.waitUntilReadyToShow(opts, () async {
    final s = appSettings;
    if (s.winWidth != null && s.winHeight != null) {
      await windowManager.setBounds(Rect.fromLTWH(
        s.winLeft ?? 100, s.winTop ?? 100, s.winWidth!, s.winHeight!,
      ));
    }
    if (s.winMaximized) {
      await windowManager.maximize();
    }
    await windowManager.show();
  });
  await appQueue.restore(); // 恢复上次未完成的待办任务（§7.1 队列持久化）
  runApp(const AutomosaicApp());
}

// --------------------------------------------------------------------------- //
// 主题与入口
// --------------------------------------------------------------------------- //

class AutomosaicApp extends StatelessWidget {
  const AutomosaicApp({super.key});
  @override
  Widget build(BuildContext context) {
    // 亮/暗双主题（DESIGN §7.1 跟随系统亮暗 + 外观组覆写）；设置变更经
    // AppSettings.notifyListeners 触发本层重建
    return ListenableBuilder(
      listenable: appSettings,
      builder: (context, _) {
        final dark = ThemeData(
          useMaterial3: true,
          colorScheme: ColorScheme.fromSeed(
            seedColor: const Color(0xFF4CD964),
            brightness: Brightness.dark,
          ),
          scaffoldBackgroundColor: const Color(0xFF14161A),
          cardTheme: const CardThemeData(
            color: Color(0xFF1C1F26),
            elevation: 0,
            margin: EdgeInsets.zero,
          ),
        );
        final light = ThemeData(
          useMaterial3: true,
          colorScheme: ColorScheme.fromSeed(
            seedColor: const Color(0xFF2E9E44),
            brightness: Brightness.light,
          ),
          scaffoldBackgroundColor: const Color(0xFFF6F7F9),
          cardTheme: const CardThemeData(
            color: Colors.white,
            elevation: 0,
            margin: EdgeInsets.zero,
          ),
        );
        return MaterialApp(
          title: 'Simple AutoMosaic',
          debugShowCheckedModeBanner: false,
          theme: light,
          darkTheme: dark,
          themeMode: switch (appSettings.themeMode) {
            'light' => ThemeMode.light,
            'dark' => ThemeMode.dark,
            _ => ThemeMode.system,
          },
          home: const HomeScreen(),
        );
      },
    );
  }
}

/// 可拖动窗口的 AppBar（自绘标题栏，DESIGN §7.1）：标题区即拖拽区；
/// macOS 交通灯悬浮在左上，自动让位（返回按钮右移 / 无按钮时占位）。
class DraggableAppBar extends StatelessWidget implements PreferredSizeWidget {
  final Widget? title;
  final List<Widget>? actions;
  final bool hasBack;
  final Color? backgroundColor;

  const DraggableAppBar({
    super.key,
    this.title,
    this.actions,
    this.hasBack = true,
    this.backgroundColor,
  });

  @override
  Size get preferredSize => const Size.fromHeight(kToolbarHeight);

  @override
  Widget build(BuildContext context) {
    final inset = Platform.isMacOS;
    return DragToMoveArea(
      child: AppBar(
        title: title,
        // Linux：TitleBarStyle.hidden 连 GTK 装饰（min/max/close）一起移除，
        // 窗口控件自绘在标题栏尾部；macOS 有悬浮交通灯无需自绘
        actions: [...?actions, if (Platform.isLinux) const WindowControls()],
        backgroundColor: backgroundColor,
        automaticallyImplyLeading: false,
        // macOS 交通灯悬浮占左上 ~72px：加宽 leading 槽位并把返回按钮右移
        // （仅加 Padding 不加 leadingWidth 会把按钮压成零宽——不可见不可点）
        leadingWidth: inset ? (hasBack ? 72 + kToolbarHeight : 80) : null,
        leading: inset
            ? (hasBack
                ? const Padding(
                    padding: EdgeInsets.only(left: 72),
                    child: BackButton(),
                  )
                : const SizedBox(width: 80))
            : (hasBack ? const BackButton() : null),
      ),
    );
  }
}

/// Linux 自绘窗口控件（min/max/close，挂在 DraggableAppBar 尾部）：
/// TitleBarStyle.hidden 在 GTK 上移除全部装饰，不像 macOS 保留交通灯。
/// close 经 setPreventClose 走 onWindowClose（保存几何后再 destroy）。
class WindowControls extends StatefulWidget {
  const WindowControls({super.key});

  @override
  State<WindowControls> createState() => _WindowControlsState();
}

class _WindowControlsState extends State<WindowControls> with WindowListener {
  bool _maximized = false;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
    windowManager
        .isMaximized()
        .then((v) { if (mounted) setState(() => _maximized = v); });
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    super.dispose();
  }

  @override
  void onWindowMaximize() => setState(() => _maximized = true);

  @override
  void onWindowUnmaximize() => setState(() => _maximized = false);

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        IconButton(
          icon: const Icon(Icons.horizontal_rule_rounded, size: 18),
          tooltip: S.t('最小化', 'Minimize'),
          onPressed: windowManager.minimize,
        ),
        IconButton(
          icon: Icon(
            _maximized ? Icons.filter_none_rounded : Icons.crop_square_rounded,
            size: 16,
          ),
          tooltip: S.t(_maximized ? '还原' : '最大化',
              _maximized ? 'Restore' : 'Maximize'),
          onPressed: () async {
            if (await windowManager.isMaximized()) {
              windowManager.unmaximize();
            } else {
              windowManager.maximize();
            }
          },
        ),
        IconButton(
          icon: const Icon(Icons.close_rounded, size: 18),
          tooltip: S.t('关闭', 'Close'),
          onPressed: windowManager.close,
        ),
      ],
    );
  }
}

// --------------------------------------------------------------------------- //
// 1. 拖入屏
// --------------------------------------------------------------------------- //

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});
  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> with WindowListener {
  bool _dragging = false;
  String? _error;
  Timer? _boundsDebounce;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
    windowManager.setPreventClose(true); // 关闭前保存窗口几何
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    _boundsDebounce?.cancel();
    super.dispose();
  }

  Future<void> _saveBounds() async {
    final size = await windowManager.getSize();
    final pos = await windowManager.getPosition();
    final maximized = await windowManager.isMaximized();
    await appSettings.saveWindowBounds(
      left: pos.dx, top: pos.dy, width: size.width, height: size.height,
      maximized: maximized,
    );
  }

  @override
  void onWindowMoved() => _scheduleBoundsSave();
  @override
  void onWindowResized() => _scheduleBoundsSave();

  void _scheduleBoundsSave() {
    _boundsDebounce?.cancel();
    _boundsDebounce = Timer(const Duration(milliseconds: 600), _saveBounds);
  }

  @override
  void onWindowClose() async {
    _boundsDebounce?.cancel();
    await _saveBounds();
    await windowManager.destroy();
  }

  Future<void> _open(String path) async {
    await appSettings.addRecentFile(path);
    if (!mounted) return;
    setState(() {}); // 刷新"最近:"栏
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ConfigScreen(path: path, settings: appSettings, queue: appQueue),
      ),
    );
  }

  Future<void> _pick() async {
    const type = XTypeGroup(extensions: ['mp4', 'mov', 'mkv', 'webm', 'avi', 'm4v']);
    final file = await openFile(acceptedTypeGroups: [type]);
    if (file != null) _open(file.path);
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      appBar: DraggableAppBar(
        hasBack: false,
        backgroundColor: Colors.transparent,
        actions: [
          ListenableBuilder(
            listenable: appQueue,
            builder: (context, _) => IconButton(
              tooltip: S.t('队列（${appQueue.jobs.length}）', 'Queue (${appQueue.jobs.length})'),
              onPressed: () => Navigator.of(context).push(
                MaterialPageRoute(
                  builder: (_) => QueueScreen(settings: appSettings, queue: appQueue),
                ),
              ),
              icon: Badge(
                isLabelVisible: appQueue.jobs.isNotEmpty,
                label: Text('${appQueue.jobs.length}'),
                child: const Icon(Icons.playlist_play),
              ),
            ),
          ),
          IconButton(
            tooltip: S.t('设置', 'Settings'),
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute(builder: (_) => SettingsScreen(settings: appSettings)),
            ),
            icon: const Icon(Icons.settings_outlined),
          ),
          const SizedBox(width: 8),
        ],
      ),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 640),
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Icon(Icons.blur_on, size: 56, color: scheme.primary),
                const SizedBox(height: 12),
                Text('Simple AutoMosaic', style: Theme.of(context).textTheme.headlineMedium),
                const SizedBox(height: 32),
                DropTarget(
                  onDragEntered: (_) => setState(() => _dragging = true),
                  onDragExited: (_) => setState(() => _dragging = false),
                  onDragDone: (details) {
                    setState(() => _dragging = false);
                    final f = details.files.firstOrNull;
                    if (f != null) _open(f.path);
                  },
                  child: AnimatedContainer(
                    duration: const Duration(milliseconds: 150),
                    width: double.infinity,
                    padding: const EdgeInsets.symmetric(vertical: 56),
                    decoration: BoxDecoration(
                      borderRadius: BorderRadius.circular(16),
                      border: Border.all(
                        color: _dragging
                            ? scheme.primary
                            : scheme.outlineVariant,
                        width: 2,
                      ),
                      color: _dragging
                          ? scheme.primaryContainer
                          : scheme.surfaceContainerLow,
                    ),
                    child: Column(
                      children: [
                        Icon(
                          _dragging ? Icons.download_done : Icons.movie_outlined,
                          size: 40,
                          color: _dragging ? scheme.primary : Colors.grey,
                        ),
                        const SizedBox(height: 12),
                        Text(_dragging ? S.t('松开以打开', 'Drop to open') : S.t('拖入视频文件', 'Drop a video here')),
                        const SizedBox(height: 4),
                        TextButton(onPressed: _pick, child: Text(S.t('或点击选择文件', 'or click to browse'))),
                      ],
                    ),
                  ),
                ),
                if (appSettings.recentFiles.isNotEmpty) ...[
                  const SizedBox(height: 24),
                  Align(
                    alignment: Alignment.centerLeft,
                    child: Wrap(
                      spacing: 8,
                      runSpacing: 8,
                      children: [
                        for (final path in appSettings.recentFiles)
                          InputChip(
                            label: Text(path.split('/').last),
                            tooltip: path,
                            visualDensity: VisualDensity.compact,
                            onPressed: () => _open(path),
                            onDeleted: () async {
                              await appSettings.removeRecentFile(path);
                              if (context.mounted) setState(() {});
                            },
                          ),
                      ],
                    ),
                  ),
                ],
                if (_error != null) ...[
                  const SizedBox(height: 16),
                  Text(_error!, style: TextStyle(color: scheme.error)),
                ],
                const SizedBox(height: 24),
                Text(
                  S.t('本地推理 · 视频人物自动打马 · 数据不出本机',
                      'On-device inference · automatic person masking · nothing leaves your machine'),
                  style: TextStyle(color: Colors.grey.shade500),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
