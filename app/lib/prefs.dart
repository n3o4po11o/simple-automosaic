import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// 应用设置（shared_preferences 持久化）。字段含义见 DESIGN §7.3 设置屏。
/// ChangeNotifier：主题/语言等全局项变更后 MaterialApp 层重建。
class AppSettings extends ChangeNotifier {
  String preset = 'balanced';
  String device = 'auto';
  String style = 'mosaic';
  double strength = 35;
  double conf = 0.35;
  bool face = true;
  int detectEvery = 0; // 0 = 取预设默认
  int batch = 0; // 0 = 取预设默认
  int faceExpand = 0; // 0 = 取预设默认
  int faceRoi = 0; // 人脸级联 ROI：0=跟随预设 1=开 2=关
  int tta = 0; // 翻转 TTA：0=跟随预设（极致档开）1=开 2=关
  // 增强选项（全部默认开；关 A/B 或特殊场景用）
  bool track = true; // ByteTrack 跟踪
  bool maskSmooth = true; // mask 时序平滑
  bool maskEma = true; // per-ID mask EMA
  bool landmarkExpand = true; // landmark 外扩
  bool gmc = false; // 相位相关全局运动补偿（运动镜头）
  bool openAfterFinish = true;
  /// 两阶段分析段输出处理：false = -f null（默认，帧丢弃不落盘）；
  /// true = 真编码写探针视频后删除（调试管线用）。
  bool analyzeDrainFile = false;
  /// 极限档 SAM2.1 规格：large（档案级默认，精度最高）/ tiny（快 ~4×）。
  String archiveSamSize = 'large';
  /// 播放器音量（0..100，调参屏滑杆；持久化记忆上次值）
  double volume = 100;
  // 视频参数（DESIGN §7.3 设置屏「视频」组）
  String encoder = 'auto'; // auto / h264_videotoolbox / libx264 / …
  String bitrate = 'auto'; // auto = 按分辨率档位缩放；或显式 "6M" 等
  String container = 'mp4'; // 输出容器：mp4 / mkv
  // 拖入屏"最近:"栏（去重置顶，最多 8 条；DESIGN §7.3）
  List<String> recentFiles = [];
  // 外观（DESIGN §7.3 设置屏「外观」组）：system / light / dark 与 system / zh / en
  String themeMode = 'system';
  String language = 'system';
  // 窗口状态（window_manager 记住窗口位置与尺寸；DESIGN §7.1）
  double? winLeft, winTop, winWidth, winHeight;
  bool winMaximized = false;

  Future<void> load() async {
    final p = await SharedPreferences.getInstance();
    preset = p.getString('preset') ?? preset;
    device = p.getString('device') ?? device;
    style = p.getString('style') ?? style;
    strength = p.getDouble('strength') ?? strength;
    conf = p.getDouble('conf') ?? conf;
    face = p.getBool('face') ?? face;
    detectEvery = p.getInt('detectEvery') ?? detectEvery;
    batch = p.getInt('batch') ?? batch;
    faceExpand = p.getInt('faceExpand') ?? faceExpand;
    faceRoi = p.getInt('faceRoi') ?? faceRoi;
    tta = p.getInt('tta') ?? tta;
    track = p.getBool('track') ?? track;
    maskSmooth = p.getBool('maskSmooth') ?? maskSmooth;
    maskEma = p.getBool('maskEma') ?? maskEma;
    landmarkExpand = p.getBool('landmarkExpand') ?? landmarkExpand;
    gmc = p.getBool('gmc') ?? gmc;
    openAfterFinish = p.getBool('openAfterFinish') ?? openAfterFinish;
    analyzeDrainFile = p.getBool('analyzeDrainFile') ?? analyzeDrainFile;
    archiveSamSize = p.getString('archiveSamSize') ?? archiveSamSize;
    encoder = p.getString('encoder') ?? encoder;
    bitrate = p.getString('bitrate') ?? bitrate;
    container = p.getString('container') ?? container;
    volume = p.getDouble('volume') ?? volume;
    recentFiles = p.getStringList('recentFiles') ?? [];
    themeMode = p.getString('themeMode') ?? themeMode;
    language = p.getString('language') ?? language;
    winLeft = p.getDouble('winLeft');
    winTop = p.getDouble('winTop');
    winWidth = p.getDouble('winWidth');
    winHeight = p.getDouble('winHeight');
    winMaximized = p.getBool('winMaximized') ?? false;
  }

  /// 记录最近打开的文件（去重置顶、上限 8 条）。
  Future<void> addRecentFile(String path) async {
    recentFiles = [path, ...recentFiles.where((p) => p != path)].take(8).toList();
    final p = await SharedPreferences.getInstance();
    await p.setStringList('recentFiles', recentFiles);
  }

  /// 从"最近"栏移除一条。
  Future<void> removeRecentFile(String path) async {
    recentFiles = recentFiles.where((p) => p != path).toList();
    final p = await SharedPreferences.getInstance();
    await p.setStringList('recentFiles', recentFiles);
  }

  Future<void> save() async {
    final p = await SharedPreferences.getInstance();
    await p.setString('preset', preset);
    await p.setString('device', device);
    await p.setString('style', style);
    await p.setDouble('strength', strength);
    await p.setDouble('conf', conf);
    await p.setBool('face', face);
    await p.setInt('detectEvery', detectEvery);
    await p.setInt('batch', batch);
    await p.setInt('faceExpand', faceExpand);
    await p.setInt('faceRoi', faceRoi);
    await p.setInt('tta', tta);
    await p.setBool('track', track);
    await p.setBool('maskSmooth', maskSmooth);
    await p.setBool('maskEma', maskEma);
    await p.setBool('landmarkExpand', landmarkExpand);
    await p.setBool('gmc', gmc);
    await p.setBool('openAfterFinish', openAfterFinish);
    await p.setBool('analyzeDrainFile', analyzeDrainFile);
    await p.setString('archiveSamSize', archiveSamSize);
    await p.setString('encoder', encoder);
    await p.setString('bitrate', bitrate);
    await p.setString('container', container);
    await p.setDouble('volume', volume);
    await p.setString('themeMode', themeMode);
    await p.setString('language', language);
    notifyListeners();
  }

  /// 保存窗口几何（window_manager 回调；去抖后调用，不触发 UI 重建）。
  Future<void> saveWindowBounds({
    required double left, required double top,
    required double width, required double height,
    required bool maximized,
  }) async {
    winLeft = left; winTop = top; winWidth = width; winHeight = height;
    winMaximized = maximized;
    final p = await SharedPreferences.getInstance();
    await p.setDouble('winLeft', left);
    await p.setDouble('winTop', top);
    await p.setDouble('winWidth', width);
    await p.setDouble('winHeight', height);
    await p.setBool('winMaximized', maximized);
  }
}

/// 用系统应用打开/显示文件（macOS=Finder 定位）。
Future<void> revealFile(String path) async {
  try {
    if (Platform.isMacOS) {
      await Process.run('open', ['-R', path]);
    } else if (Platform.isWindows) {
      await Process.run('explorer', ['/select,', path]);
    } else {
      await Process.run('xdg-open', [File(path).parent.path]);
    }
  } catch (_) {
    // 打开失败不影响主流程
  }
}
