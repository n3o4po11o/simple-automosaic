import 'dart:async';
import 'dart:ui' as ui;

import 'package:collection/collection.dart';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import 'config_screen.dart';
import 'main.dart';
import 'prefs.dart';
import 'review_screen.dart';
import 'queue.dart';
import 'queue_screen.dart';
import 'src/rust/api/automosaic.dart'
    show
        ProcessEvent_Failed,
        ProcessEvent_Finished,
        ProcessEvent_JobMeta,
        ProcessStage,
        archiveRender;

/// 处理进度屏：当前任务大卡（分阶段进度/对照预览/日志）+ 队列列表，
/// 多任务由 QueueController 顺序驱动（DESIGN §7.2/§7.4）。
class ProcessScreen extends StatefulWidget {
  final AppSettings settings;
  final QueueController queue;
  const ProcessScreen({super.key, required this.settings, required this.queue});

  @override
  State<ProcessScreen> createState() => _ProcessScreenState();
}

class _ProcessScreenState extends State<ProcessScreen> {
  bool _showLogs = false;
  /// archive 任务完成后的直渲状态（免去进复核屏的跳转）。
  bool _renderingArchive = false;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.queue,
      builder: (context, _) {
        final q = widget.queue;
        final current = q.current;
        return Scaffold(
          appBar: DraggableAppBar(
            title: Text(current == null
                ? S.t('处理', 'Processing')
                : S.t('正在处理：', 'Processing: ') + current.name),
            actions: [
              ListenableBuilder(
                listenable: Listenable.merge([widget.queue]),
                builder: (context, _) => IconButton(
                  tooltip: S.t('队列（${q.jobs.length}）', 'Queue (${q.jobs.length})'),
                  onPressed: () => Navigator.of(context).push(
                    MaterialPageRoute(
                      builder: (_) => QueueScreen(
                        settings: widget.settings,
                        queue: widget.queue,
                      ),
                    ),
                  ),
                  icon: Badge(
                    isLabelVisible: q.jobs.isNotEmpty,
                    label: Text('${q.jobs.length}'),
                    child: const Icon(Icons.playlist_play),
                  ),
                ),
              ),
              IconButton(
                tooltip: S.t('添加任务', 'Add job'),
                onPressed: _addJob,
                icon: const Icon(Icons.add),
              ),
              const SizedBox(width: 8),
            ],
          ),
          body: Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 880),
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: q.jobs.isEmpty
                    ? _empty()
                    // 无运行任务时显示最近一个已结束任务：完成态行承载
                    // archive 的渲染输出/复核入口（曾因只显示运行中任务，
                    // 完成态 UI 从未渲染——复核入口不可达，2026-08-21 修复）；
                    // 全部待办时回退显示首个待办（状态行"等待中"）
                    : _currentCard(current ??
                        q.jobs.lastWhereOrNull((j) => j.state != JobState.pending) ??
                        q.jobs.first),
              ),
            ),
          ),
        );
      },
    );
  }

  Widget _empty() {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Icon(Icons.movie_filter_outlined, size: 56, color: Color(0xFF4CD964)),
          const SizedBox(height: 12),
          Text(S.t('队列为空', 'Queue is empty')),
          const SizedBox(height: 16),
          FilledButton(onPressed: _addJob, child: Text(S.t('添加任务', 'Add job'))),
          const SizedBox(height: 8),
          OutlinedButton(onPressed: _openQueue, child: Text(S.t('查看队列', 'View queue'))),
        ],
      ),
    );
  }

  void _openQueue() => Navigator.of(context).push(
        MaterialPageRoute(
          builder: (_) => QueueScreen(
            settings: widget.settings,
            queue: widget.queue,
          ),
        ),
      );

  Future<void> _addJob() async {
    const type = XTypeGroup(extensions: ['mp4', 'mov', 'mkv', 'webm', 'avi', 'm4v']);
    final file = await openFile(acceptedTypeGroups: [type]);
    if (!mounted || file == null) return;
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ConfigScreen(
          path: file.path,
          settings: widget.settings,
          queue: widget.queue,
        ),
      ),
    );
    // ConfigScreen pushReplacement 到本屏；从队列点 + 进来的则自然返回本屏
  }


  // ---- 当前任务大卡 ----

  Widget _currentCard(QueueJob job) {
    final progress =
        (job.total != null && job.total! > BigInt.zero)
            ? (job.frames / job.total!).clamp(0.0, 1.0)
            : null;
    // 一屏看全：固定信息紧凑排布，预览对 Expanded 弹性占位（随剩余空间
    // 自动缩放，无需滚动）；队列已移至独立队列屏
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          children: [
            _statusHeader(job),
            if (job.meta != null) ...[
              const SizedBox(height: 10),
              _metaTags(job.meta!),
            ],
            const SizedBox(height: 14),
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceEvenly,
              children: [
                _stage(S.t('提取', 'Decode'), job.decoded.toString(), Icons.movie_filter_outlined),
                _stage(S.t('处理', 'Infer'), '${job.frames}/${job.total?.toString() ?? '?'}', Icons.auto_awesome),
                _stage(S.t('编码', 'Encode'), job.written.toString(), Icons.save_outlined),
              ],
            ),
            const SizedBox(height: 12),
            LinearProgressIndicator(
              value: progress,
              minHeight: 8,
              borderRadius: BorderRadius.circular(4),
            ),
            const SizedBox(height: 8),
            Text(
              '${job.fps.toStringAsFixed(1)} fps · ${S.t('剩余', 'ETA')} ${job.eta == null ? '?' : '${job.eta!.toStringAsFixed(0)}${S.t(' 秒', 's')}'}',
              style: TextStyle(color: Colors.grey.shade400, fontSize: 12),
            ),
            const SizedBox(height: 12),
            Expanded(child: _previewPair(job)),
            const SizedBox(height: 12),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                OutlinedButton(
                  onPressed: job.state == JobState.running
                      ? widget.queue.cancelCurrent
                      : null,
                  child: Text(S.t('取消', 'Cancel')),
                ),
                const SizedBox(width: 8),
                OutlinedButton(
                  onPressed: () => revealFile(job.output),
                  child: Text(S.t('打开所在文件夹', 'Reveal in Finder')),
                ),
              ],
            ),
            if (job.logs.isNotEmpty) ...[
              const SizedBox(height: 8),
              _logPanel(job),
            ],
          ],
        ),
      ),
    );
  }

  /// 任务信息网格（ProcessEvent::JobMeta 的结构化展示）：3×3 等宽等高
  /// 图标药丸——Expanded 单元保证列对齐、行等距；图标 + 值与全 App 控件
  /// 同语言，值超宽省略号截断、tooltip 给完整含义。后端药丸只显示短名
  /// （如 CoreML），完整描述在 tooltip。
  Widget _metaTags(ProcessEvent_JobMeta m) {
    final c = Theme.of(context).colorScheme;
    String stem(String f) => f.endsWith('.onnx') ? f.substring(0, f.length - 5) : f;
    Widget tag(IconData icon, String value, {String? tooltip}) => Container(
          height: 30,
          alignment: Alignment.center,
          padding: const EdgeInsets.symmetric(horizontal: 8),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(8),
            color: c.surfaceContainerLow,
            border: Border.all(color: c.outlineVariant),
          ),
          child: Tooltip(
            message: tooltip ?? value,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(icon, size: 13, color: c.primary),
                const SizedBox(width: 5),
                Flexible(
                  child: Text(
                    value,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                        fontSize: 11.5, fontWeight: FontWeight.w500),
                  ),
                ),
              ],
            ),
          ),
        );
    final detect = m.detectEvery <= 1
        ? '${S.t('逐帧 · 批 ', 'every frame · batch ')}${m.batch}'
        : '${S.t('隔 ${m.detectEvery} 帧 · 批 ', 'every ${m.detectEvery} frames · batch ')}${m.batch}';
    Widget row(List<Widget> cells) => Row(
          children: [
            for (final (i, cell) in cells.indexed) ...[
              if (i > 0) const SizedBox(width: 6),
              Expanded(child: cell),
            ],
          ],
        );
    return Column(
      children: [
        row([
          tag(Icons.tune, S.rust(m.presetLabel), tooltip: S.t('质量预设', 'Preset')),
          tag(Icons.memory, stem(m.bodyModel), tooltip: S.t('人体模型', 'Body model')),
          tag(
            Icons.face,
            m.face ? (m.faceModel != null ? stem(m.faceModel!) : S.t('开', 'on')) : S.t('关', 'off'),
            tooltip: m.face ? S.t('人脸模型', 'Face model') : S.t('人脸检测关闭', 'Face detection off'),
          ),
        ]),
        const SizedBox(height: 6),
        row([
          tag(Icons.input, S.rust(m.decoder),
              tooltip: S.t('视频解码（hwaccel）', 'Video decode (hwaccel)')),
          tag(Icons.output, m.encoder, tooltip: S.t('视频编码器', 'Video encoder')),
          tag(Icons.aspect_ratio,
              '${m.width}×${m.height} · ${m.totalFrames ?? '?'} ${S.t('帧', 'frames')}',
              tooltip: S.t('分辨率与总帧数', 'Resolution and frame count')),
        ]),
        const SizedBox(height: 6),
        row([
          tag(Icons.timelapse, detect, tooltip: S.t('检测间隔与批大小', 'Detection interval and batch')),
          tag(Icons.developer_board, S.rust(m.deviceDesc).split('（').first,
              tooltip: S.t('推理后端：', 'Backend: ') + S.rust(m.deviceDesc)),
          tag(Icons.hourglass_bottom, '${m.modelLoadSecs.toStringAsFixed(1)}s',
              tooltip: S.t('模型加载耗时', 'Model load time')),
        ]),
      ],
    );
  }

  Widget _statusHeader(QueueJob job) {
    return switch (job.state) {
      JobState.running => Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            const SizedBox(
                height: 22,
                width: 22,
                child: CircularProgressIndicator(strokeWidth: 3)),
            const SizedBox(width: 10),
            Text(S.t('处理中', 'Processing') + _stageLabel(job)),
          ],
        ),
      JobState.finished => Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            const Icon(Icons.check_circle, color: Color(0xFF4CD964), size: 26),
            const SizedBox(width: 8),
            Text('${job.archive ? S.t('分析完成：', 'Analysis done: ') : S.t('完成：', 'Done: ')}${job.frames} ${S.t('帧', 'frames')} · ${job.elapsed.toStringAsFixed(1)}${S.t(' 秒', 's')}'),
            // 两阶段档：直渲出片（主路径）；需要修补再进复核
            if (job.archive) ...[
              const SizedBox(width: 12),
              FilledButton.icon(
                icon: _renderingArchive
                    ? const SizedBox(
                        width: 14,
                        height: 14,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.movie, size: 16),
                label: Text(S.t('渲染输出', 'Render')),
                onPressed: _renderingArchive ? null : () => _renderArchive(job),
              ),
              TextButton.icon(
                icon: const Icon(Icons.brush, size: 16),
                label: Text(S.t('复核', 'Review')),
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute(
                    builder: (_) => ReviewScreen(
                      input: job.input,
                      masksDir: job.masksDir,
                      output: job.output,
                      settings: widget.settings,
                    ),
                  ),
                ),
              ),
            ],
          ],
        ),
      JobState.failed => Text(S.t('处理失败：', 'Failed: ') + (job.error ?? ''),
          style: TextStyle(color: Theme.of(context).colorScheme.error, fontSize: 13)),
      JobState.cancelled =>
          Text('${S.t('已取消（完成 ', 'Cancelled (')}${job.frames}${S.t(' 帧）', ' frames)')}'),
      JobState.pending => Text(S.t('等待中', 'Pending')),
    };
  }

  /// 直接渲染 archive 任务的成片（复用缓存，纯合成，秒级）。
  Future<void> _renderArchive(QueueJob job) async {
    if (_renderingArchive) return;
    setState(() {
      _renderingArchive = true;
    });
    try {
      await archiveRender(
        input: job.input,
        masksDir: job.masksDir,
        output: job.output,
        style: job.opts.style,
        strength: job.opts.strength.round(),
        hwaccel: 'auto',
        encoder: widget.settings.encoder,
        bitrate: widget.settings.bitrate,
      ).drain((e) {
        if (e is ProcessEvent_Failed) {
          ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(content: Text('渲染失败: ${e.error}')));
        } else if (e is ProcessEvent_Finished) {
          if (widget.settings.openAfterFinish) unawaited(revealFile(job.output));
          ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(content: Text('渲染完成: ${job.output.split('/').last}')));
        }
      });
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('渲染失败: $e')));
      }
    }
    if (mounted) setState(() => _renderingArchive = false);
  }

  /// 阶段后缀（ProcessEvent::StageEnter 的最新值；流式管线各段并发，
  /// 这里展示任务级状态机：探测 → 推理 → 收尾）。
  String _stageLabel(QueueJob job) => switch (job.stage) {
        ProcessStage.probing => S.t(' · 探测元数据', ' · probing'),
        ProcessStage.inferring => S.t(' · 推理中', ' · inferring'),
        ProcessStage.finalizing => S.t(' · 编码收尾', ' · finalizing'),
        null => '',
      };

  /// 日志面板（可展开，ProcessEvent::Log）。
  Widget _logPanel(QueueJob job) {
    return Column(
      children: [
        TextButton.icon(
          onPressed: () => setState(() => _showLogs = !_showLogs),
          icon: Icon(_showLogs ? Icons.expand_less : Icons.expand_more, size: 18),
          label: Text(_showLogs
              ? S.t('收起日志', 'Hide logs')
              : S.t('日志（${job.logs.length}）', 'Logs (${job.logs.length})')),
        ),
        if (_showLogs)
          Container(
            width: double.infinity,
            constraints: const BoxConstraints(maxHeight: 140),
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(8),
              color: Theme.of(context).colorScheme.surfaceContainerLow,
            ),
            child: ListView(
              children: [
                for (final line in job.logs)
                  Text(line,
                      style: const TextStyle(fontSize: 10.5, fontFamily: 'monospace', color: Color(0xFF9AA3AD), height: 1.5)),
              ],
            ),
          ),
      ],
    );
  }

  Widget _stage(String label, String value, IconData icon) {
    return Column(
      children: [
        Icon(icon, size: 20, color: const Color(0xFF4CD964)),
        const SizedBox(height: 4),
        Text(label, style: TextStyle(fontSize: 11, color: Colors.grey.shade500)),
        Text(value, style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600)),
      ],
    );
  }

  Widget _previewPair(QueueJob job) {
    if (job.origImg == null || job.procImg == null) {
      return Center(
        child: Text(S.t('等待预览帧…', 'Waiting for preview frames…'),
            style: TextStyle(color: Theme.of(context).colorScheme.onSurfaceVariant)),
      );
    }
    return Row(
      children: [
        Expanded(child: _labeled(
            '${S.t('原画面（帧 ', 'Original (frame ')}${job.previewIdx}${S.t('）', ')')}',
            job.origImg!)),
        const SizedBox(width: 8),
        Expanded(child: _labeled(S.t('处理后画面', 'Masked'), job.procImg!)),
      ],
    );
  }

  /// 图像在剩余高度内自适应（Expanded + FittedBox contain），标签贴底。
  Widget _labeled(String label, ui.Image img) {
    return Column(
      children: [
        Expanded(
          child: ClipRRect(
            borderRadius: BorderRadius.circular(6),
            child: FittedBox(fit: BoxFit.contain, child: RawImage(image: img)),
          ),
        ),
        const SizedBox(height: 4),
        Text(label, style: TextStyle(fontSize: 10, color: Colors.grey.shade500)),
      ],
    );
  }

}
