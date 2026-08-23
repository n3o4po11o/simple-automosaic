import 'dart:async';
import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'prefs.dart';
import 'src/rust/api/automosaic.dart';

/// 单个队列任务的运行状态。
enum JobState { pending, running, finished, failed, cancelled }

/// 队列任务：输入/输出/参数 + 运行态（进度、日志、预览帧）。
class QueueJob {
  final String input;
  final String output;
  final ProcessOptions opts;
  final bool openAfterFinish;
  /// M5 两阶段档：output 语义改为"最终渲染产物"，先跑 archiveAnalyze
  /// 落盘 mask 缓存（masksDir），完成经复核屏手动渲染。
  final bool archive;
  /// 分析段输出处理快照（设置屏"分析段写探针视频"；false=-f null）。
  final bool analyzeDrainFile;
  /// 极限档 SAM 规格快照（large/tiny，建任务时取设置）。
  final String archiveSamSize;

  JobState state = JobState.pending;
  /// 任务级阶段（ProcessEvent::StageEnter 的最新值；null = 尚未开始）。
  ProcessStage? stage;
  BigInt frames = BigInt.zero;
  BigInt decoded = BigInt.zero;
  BigInt written = BigInt.zero;
  BigInt? total;
  double fps = 0;
  double? eta;
  double elapsed = 0;
  String? error;
  /// 任务元数据（ProcessEvent::JobMeta；处理屏"任务信息"键值展示）。
  ProcessEvent_JobMeta? meta;
  final List<String> logs = [];
  ui.Image? origImg;
  ui.Image? procImg;
  BigInt previewIdx = BigInt.zero;
  bool decodingPreview = false;

  QueueJob({
    required this.input,
    required this.output,
    required this.opts,
    required this.openAfterFinish,
    this.archive = false,
    this.analyzeDrainFile = false,
    this.archiveSamSize = 'large',
  });

  String get name => input.split('/').last;

  /// 两阶段 mask 缓存目录（output 去扩展名 + _mosaic_masks）。
  String get masksDir {
    final i = output.lastIndexOf('.');
    final base = i > 0 ? output.substring(0, i) : output;
    return '${base}_masks';
  }
}

/// 多任务顺序队列（DESIGN §7.2/§7.4）：一次跑一个任务，完成/失败/取消后自动
/// 启动下一个；FFI 侧为单任务全局取消，顺序执行天然兼容。
/// 待办任务持久化到 shared_preferences（DESIGN §7.1 队列持久化；原设计提议
/// hive，此处用已有的 shared_preferences 存 JSON，免引入新依赖）——重启后
/// 恢复为待办并继续顺序执行；运行态（进度/日志/预览）不持久化。
class QueueController extends ChangeNotifier {
  static const _pendingKey = 'pendingQueueJobs';

  final List<QueueJob> jobs = [];
  StreamSubscription<ProcessEvent>? _sub;
  bool _starting = false;

  QueueJob? get current =>
      jobs.where((j) => j.state == JobState.running).firstOrNull;

  bool get isBusy => current != null;

  /// 应用启动时恢复上次未完成的待办任务。
  Future<void> restore() async {
    final p = await SharedPreferences.getInstance();
    final raw = p.getString(_pendingKey);
    if (raw == null) return;
    try {
      final list = (jsonDecode(raw) as List)
          .map((e) => _jobFromJson(e as Map<String, dynamic>))
          .whereType<QueueJob>()
          .toList();
      if (list.isEmpty) return;
      jobs.addAll(list);
      notifyListeners();
      unawaited(_startNext());
    } catch (_) {
      // 旧版本/损坏数据：静默丢弃，不阻塞启动
    }
  }

  Future<void> _persistPending() async {
    final p = await SharedPreferences.getInstance();
    final pending = jobs
        .where((j) => j.state == JobState.pending)
        .map(_jobToJson)
        .toList();
    await p.setString(_pendingKey, jsonEncode(pending));
  }

  Map<String, dynamic> _jobToJson(QueueJob j) => {
        'input': j.input,
        'output': j.output,
        'openAfterFinish': j.openAfterFinish,
        'archive': j.archive,
        'analyzeDrainFile': j.analyzeDrainFile,
        'archiveSamSize': j.archiveSamSize,
        'opts': {
          'preset': j.opts.preset,
          'conf': j.opts.conf,
          'device': j.opts.device,
          'style': j.opts.style,
          'strength': j.opts.strength,
          'modelPath': j.opts.modelPath,
          'hwaccel': j.opts.hwaccel,
          'encoder': j.opts.encoder,
          'bitrate': j.opts.bitrate,
          'face': j.opts.face,
          'faceExpand': j.opts.faceExpand,
          'detectEvery': j.opts.detectEvery,
          'faceRoi': j.opts.faceRoi,
          'track': j.opts.track,
          'maskSmooth': j.opts.maskSmooth,
          'maskEma': j.opts.maskEma,
          'landmarkExpand': j.opts.landmarkExpand,
          'batch': j.opts.batch,
          'tta': j.opts.tta,
          'gmc': j.opts.gmc,
        },
      };

  QueueJob? _jobFromJson(Map<String, dynamic> m) {
    final o = m['opts'];
    if (o is! Map<String, dynamic>) return null;
    return QueueJob(
      input: m['input'] as String,
      output: m['output'] as String,
      openAfterFinish: m['openAfterFinish'] as bool? ?? true,
      archive: m['archive'] as bool? ?? false,
      analyzeDrainFile: m['analyzeDrainFile'] as bool? ?? false,
      archiveSamSize: m['archiveSamSize'] as String? ?? 'large',
      opts: ProcessOptions(
        preset: o['preset'] as String? ?? 'balanced',
        conf: (o['conf'] as num?)?.toDouble() ?? 0.35,
        device: o['device'] as String? ?? 'auto',
        style: o['style'] as String? ?? 'mosaic',
        strength: o['strength'] as int? ?? 35,
        modelPath: o['modelPath'] as String?,
        hwaccel: o['hwaccel'] as String? ?? 'auto',
        encoder: o['encoder'] as String? ?? 'auto',
        bitrate: o['bitrate'] as String? ?? 'auto',
        face: o['face'] as bool? ?? true,
        faceExpand: o['faceExpand'] as int? ?? 0,
        detectEvery: o['detectEvery'] as int? ?? 0,
        faceRoi: o['faceRoi'] as int? ?? 0,
        track: o['track'] as bool? ?? true,
        maskSmooth: o['maskSmooth'] as bool? ?? true,
        maskEma: o['maskEma'] as bool? ?? true,
        landmarkExpand: o['landmarkExpand'] as bool? ?? true,
        batch: o['batch'] as int? ?? 0,
        tta: o['tta'] as int? ?? 0,
        gmc: o['gmc'] as bool? ?? false,
      ),
    );
  }

  void add(QueueJob job) {
    jobs.add(job);
    notifyListeners();
    unawaited(_persistPending());
    unawaited(_startNext());
  }

  void removeAt(int i) {
    final j = jobs[i];
    if (j.state == JobState.running) return; // 运行中不可移除，先取消
    jobs.removeAt(i);
    notifyListeners();
    unawaited(_persistPending());
  }

  void clearFinished() {
    jobs.removeWhere(
        (j) => j.state != JobState.running && j.state != JobState.pending);
    notifyListeners();
    unawaited(_persistPending());
  }

  /// 取消当前任务（幂等；完成后自动进入下一个）。
  void cancelCurrent() => cancelProcess();

  Future<void> _startNext() async {
    if (_starting || isBusy) return;
    final next = jobs.where((j) => j.state == JobState.pending).firstOrNull;
    if (next == null) return;
    _starting = true;
    next.state = JobState.running;
    notifyListeners();
    try {
      final stream = next.archive
          ? archiveAnalyze(
              input: next.input,
              masksDir: next.masksDir,
              opts: ArchiveAnalyzeOptions(
                device: next.opts.device,
                samSize: next.archiveSamSize,
                conf: next.opts.conf,
                // 0=跟随预设（archive 默认开）/1=开/2=关
                tta: next.opts.tta != 2,
                hwaccel: next.opts.hwaccel,
                encoder: next.opts.encoder,
                // 对照预览合成样式（仅处理屏展示，不影响 mask 缓存）
                style: next.opts.style,
                strength: next.opts.strength.round(),
                // 输出处理：建任务时快照的设置屏调试开关（true=真编码探针）
                drain: next.analyzeDrainFile ? 'file' : 'null',
              ),
            )
          : processVideo(
              input: next.input,
              output: next.output,
              opts: next.opts,
            );
      final done = Completer<void>();
      _sub = stream.listen(
        (e) => _onEvent(next, e, done),
        onError: (Object err) {
          next.state = JobState.failed;
          next.error = err.toString();
          done.complete();
        },
        onDone: done.complete,
      );
      await done.future;
    } finally {
      _starting = false;
      notifyListeners();
      unawaited(_persistPending()); // 任务已离开待办态，更新持久化列表
      unawaited(_startNext());
    }
  }

  void _onEvent(QueueJob job, ProcessEvent e, Completer<void> done) {
    switch (e) {
      case ProcessEvent_StageEnter(:final stage):
        job.stage = stage;
      case ProcessEvent_JobMeta():
        job.meta = e;
      case ProcessEvent_Progress(
          :final frames,
          :final decoded,
          :final written,
          :final totalFrames,
          :final fps,
          :final etaSecs):
        job.frames = frames;
        job.decoded = decoded;
        job.written = written;
        job.total = totalFrames;
        job.fps = fps;
        job.eta = etaSecs;
      case ProcessEvent_Log(:final line):
        job.logs.add(line);
      case ProcessEvent_PreviewPair(
          :final frameIdx,
          :final original,
          :final processed,
          :final width,
          :final height):
        _decodePair(job, original, processed, width, height, frameIdx);
      case ProcessEvent_Finished(:final frames, :final elapsedSecs):
        job.state = JobState.finished;
        job.elapsed = elapsedSecs;
        // 真实帧数回填：进度末拍可能滞后一拍、total 为估算值（时长×fps
        // 取整常多 1）——完成态以此为准，消除"还差 N 帧就完成"的观感
        job.frames = frames;
        job.total = frames;
        _sub?.cancel();
        // 两阶段档的产物是 mask 缓存（复核后渲染），不直接打开
        if (job.openAfterFinish && !job.archive) {
          unawaited(revealFile(job.output));
        }
        done.complete();
      case ProcessEvent_Failed(:final error):
        job.state = JobState.failed;
        job.error = error;
        _sub?.cancel();
        done.complete();
      case ProcessEvent_Cancelled():
        job.state = JobState.cancelled;
        _sub?.cancel();
        done.complete();
    }
    notifyListeners();
  }

  Future<void> _decodePair(
      QueueJob job, Uint8List orig, Uint8List proc, int w, int h, BigInt idx) async {
    if (job.decodingPreview) return; // 丢弃过快的帧，避免排队积压
    job.decodingPreview = true;
    try {
      final a = await decodeRgba(orig, w, h);
      final b = await decodeRgba(proc, w, h);
      job.origImg?.dispose();
      job.procImg?.dispose();
      job.origImg = a;
      job.procImg = b;
      job.previewIdx = idx;
      notifyListeners();
    } finally {
      job.decodingPreview = false;
    }
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }
}

Future<ui.Image> decodeRgba(Uint8List data, int w, int h) {
  final c = Completer<ui.Image>();
  ui.decodeImageFromPixels(data, w, h, ui.PixelFormat.rgba8888, c.complete);
  return c.future;
}
