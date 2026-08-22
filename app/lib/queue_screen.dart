import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import 'config_screen.dart';
import 'main.dart';
import 'prefs.dart';
import 'process_screen.dart';
import 'queue.dart';

/// 队列屏（DESIGN §7.2 队列侧栏的独立屏形态）：全部任务的状态列表 +
/// 运行中任务的迷你进度；处理详情在处理屏（两者经按钮互跳）。
class QueueScreen extends StatefulWidget {
  final AppSettings settings;
  final QueueController queue;
  const QueueScreen({super.key, required this.settings, required this.queue});
  @override
  State<QueueScreen> createState() => _QueueScreenState();
}

class _QueueScreenState extends State<QueueScreen> {
  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.queue,
      builder: (context, _) {
        final q = widget.queue;
        final current = q.current;
        final finished =
            q.jobs.where((j) => j.state != JobState.pending).length;
        return Scaffold(
          appBar: DraggableAppBar(
            title: Text(S.t('队列', 'Queue')),
            actions: [
              if (finished > 0)
                TextButton(
                  onPressed: q.clearFinished,
                  child: Text(S.t('清除已结束', 'Clear finished')),
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
              child: q.jobs.isEmpty
                  ? _empty()
                  : ListView(
                      padding: const EdgeInsets.all(24),
                      children: [
                        if (current != null) ...[
                          _runningCard(current),
                          const SizedBox(height: 16),
                        ],
                        for (final j in q.jobs) _jobTile(q, j),
                      ],
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
          const Icon(Icons.playlist_play, size: 56, color: Color(0xFF4CD964)),
          const SizedBox(height: 12),
          Text(S.t('队列为空', 'Queue is empty')),
          const SizedBox(height: 16),
          FilledButton(onPressed: _addJob, child: Text(S.t('添加任务', 'Add job'))),
          const SizedBox(height: 8),
          OutlinedButton(
            onPressed: () => Navigator.of(context).pop(),
            child: Text(S.t('返回', 'Back')),
          ),
        ],
      ),
    );
  }

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
  }

  void _openProcess() => Navigator.of(context).push(
        MaterialPageRoute(
          builder: (_) => ProcessScreen(
            settings: widget.settings,
            queue: widget.queue,
          ),
        ),
      );

  /// 运行中任务的迷你卡：进度条 + 吞吐 + 跳处理屏看对照预览。
  Widget _runningCard(QueueJob job) {
    final progress = (job.total != null && job.total! > BigInt.zero)
        ? (job.frames / job.total!).clamp(0.0, 1.0)
        : null;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const SizedBox(
                    height: 18, width: 18,
                    child: CircularProgressIndicator(strokeWidth: 2.5)),
                const SizedBox(width: 10),
                Expanded(
                  child: Text(job.name,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(fontWeight: FontWeight.w600)),
                ),
                TextButton.icon(
                  onPressed: _openProcess,
                  icon: const Icon(Icons.open_in_new, size: 16),
                  label: Text(S.t('处理详情', 'Details')),
                ),
              ],
            ),
            const SizedBox(height: 8),
            LinearProgressIndicator(
              value: progress,
              minHeight: 6,
              borderRadius: BorderRadius.circular(3),
            ),
            const SizedBox(height: 6),
            Text(
              '${job.frames}/${job.total?.toString() ?? '?'} ${S.t('帧', 'frames')} · '
              '${job.fps.toStringAsFixed(1)} fps · '
              '${S.t('剩余', 'ETA')} ${job.eta == null ? '?' : '${job.eta!.toStringAsFixed(0)}${S.t(' 秒', 's')}'}',
              style: TextStyle(color: Colors.grey.shade500, fontSize: 12),
            ),
          ],
        ),
      ),
    );
  }

  Widget _jobTile(QueueController q, QueueJob j) {
    final i = q.jobs.indexOf(j);
    return ListTile(
      dense: true,
      leading: _stateIcon(j.state),
      title: Text(j.name, overflow: TextOverflow.ellipsis),
      subtitle: Text(
        switch (j.state) {
          JobState.running => '${j.fps.toStringAsFixed(1)} fps · ${j.frames} ${S.t('帧', 'frames')}',
          JobState.finished =>
            '${j.frames} ${S.t('帧', 'frames')} · ${j.elapsed.toStringAsFixed(1)}${S.t(' 秒', 's')}',
          JobState.failed => j.error ?? S.t('失败', 'failed'),
          _ => '${S.t('输出', 'Output')} ${j.output.split('/').last}',
        },
        style: const TextStyle(fontSize: 11),
        overflow: TextOverflow.ellipsis,
      ),
      trailing: switch (j.state) {
        JobState.pending => IconButton(
            tooltip: S.t('移除', 'Remove'),
            icon: const Icon(Icons.close, size: 16),
            onPressed: () => q.removeAt(i),
          ),
        JobState.running => IconButton(
            tooltip: S.t('处理详情', 'Details'),
            icon: const Icon(Icons.open_in_new, size: 16),
            onPressed: _openProcess,
          ),
        JobState.finished => IconButton(
            tooltip: S.t('打开文件', 'Open file'),
            icon: const Icon(Icons.folder_open, size: 18),
            onPressed: () => revealFile(j.output),
          ),
        _ => null,
      },
    );
  }

  Widget _stateIcon(JobState s) => switch (s) {
        JobState.pending =>
          const Icon(Icons.schedule, size: 20, color: Colors.grey),
        JobState.running => const SizedBox(
            height: 16, width: 16, child: CircularProgressIndicator(strokeWidth: 2)),
        JobState.finished =>
          const Icon(Icons.check_circle, size: 20, color: Color(0xFF4CD964)),
        JobState.failed => Icon(Icons.error_outline,
            size: 20, color: Theme.of(context).colorScheme.error),
        JobState.cancelled =>
          const Icon(Icons.cancel_outlined, size: 20, color: Colors.orange),
      };
}
