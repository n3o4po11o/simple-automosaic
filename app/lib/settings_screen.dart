import 'dart:io';

import 'package:flutter/material.dart';
import 'package:package_info_plus/package_info_plus.dart';

import 'main.dart';
import 'prefs.dart';
import 'src/rust/api/automosaic.dart';

/// 设置屏（DESIGN §7.3）：外观（主题/语言）+ 高级参数（隔帧/批/人脸外扩）
/// + 行为开关 + 视频参数（编码器/码率/输出容器）+ 模型管理。
class SettingsScreen extends StatefulWidget {
  final AppSettings settings;
  const SettingsScreen({super.key, required this.settings});

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  List<ModelInfo> _models = [];
  List<BackendInfo> _backends = [];
  final Map<String, String> _verifyResults = {}; // file → 校验结果文案
  PackageInfo? _packageInfo; // 关于卡片的版本号（pubspec 单一事实源）

  @override
  void initState() {
    super.initState();
    _loadModels();
    _loadBackends();
    PackageInfo.fromPlatform().then((info) {
      if (mounted) setState(() => _packageInfo = info);
    });
  }

  Future<void> _loadBackends() async {
    try {
      final backends = await listBackends();
      if (mounted) setState(() => _backends = backends);
    } catch (_) {
      // 后端枚举失败不阻塞设置屏
    }
  }

  Future<void> _loadModels() async {
    final models = await listModels();
    if (mounted) setState(() => _models = models);
  }

  Future<void> _verify(String file) async {
    setState(() => _verifyResults[file] = S.t('校验中…', 'Verifying…'));
    try {
      final ok = await verifyModel(file: file);
      setState(() => _verifyResults[file] = ok
          ? S.t('SHA256 校验通过', 'SHA256 verified')
          : S.t('校验失败：文件已损坏', 'Verify failed: file corrupted'));
    } catch (e) {
      setState(() => _verifyResults[file] = S.t('校验失败：', 'Verify failed: ') + e.toString());
    }
  }

  void _set(void Function(AppSettings s) mut) {
    setState(() => mut(widget.settings));
    widget.settings.save();
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.settings;
    return Scaffold(
      appBar: DraggableAppBar(title: Text(S.t('设置', 'Settings'))),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 640),
          child: ListView(
            padding: const EdgeInsets.all(16),
            children: [
              _card(S.t('外观', 'Appearance'), [
                ListTile(
                  title: Text(S.t('主题', 'Theme')),
                  subtitle: Text(
                    switch (s.themeMode) {
                      'light' => S.t('亮色', 'Light'),
                      'dark' => S.t('暗色', 'Dark'),
                      _ => S.t('跟随系统亮暗', 'Follow system'),
                    },
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: SegmentedButton<String>(
                    segments: [
                      ButtonSegment(value: 'system', label: Text(S.t('系统', 'System'))),
                      ButtonSegment(value: 'light', label: Text(S.t('亮', 'Light'))),
                      ButtonSegment(value: 'dark', label: Text(S.t('暗', 'Dark'))),
                    ],
                    selected: {s.themeMode},
                    onSelectionChanged: (v) => _set((x) => x.themeMode = v.first),
                  ),
                ),
                ListTile(
                  title: Text(S.t('语言', 'Language')),
                  subtitle: Text(
                    switch (s.language) {
                      'zh' => '中文',
                      'en' => 'English',
                      _ => S.t('跟随系统语言', 'Follow system'),
                    },
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: SegmentedButton<String>(
                    segments: [
                      ButtonSegment(value: 'system', label: Text(S.t('系统', 'System'))),
                      ButtonSegment(value: 'zh', label: const Text('中文')),
                      ButtonSegment(value: 'en', label: const Text('English')),
                    ],
                    selected: {s.language},
                    onSelectionChanged: (v) {
                      _set((x) => x.language = v.first);
                      S.init(v.first); // 立即生效（后续界面文案按新语言渲染）
                    },
                  ),
                ),
              ]),
              _card(S.t('处理', 'Processing'), [
                Tooltip(
                  message: S.t(
                      '检测人脸并单独打码：人脸是最需要保护的识别特征，'
                      '单独的小码比只靠人体整体遮盖更可靠、更清晰。'
                      '关闭则仅按人体轮廓整体遮盖。',
                      'Detects faces and masks them separately: faces are the '
                      'key identifying feature — a dedicated small mosaic is '
                      'more reliable than body coverage alone. Off masks only '
                      'the body silhouette.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: SwitchListTile(
                    title: Text(S.t('同时给人脸打马', 'Also mask faces')),
                    subtitle: Text(S.t('关闭后仅遮盖人体', 'Off: body silhouette only')),
                    value: s.face,
                    onChanged: (v) => _set((x) => x.face = v),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '对每个检测到的人的头部区域裁剪放大后单独跑一次'
                      '人脸模型：远景小人脸的有效分辨率成倍提升，小脸召回明显'
                      '更高，代价是每人每次检测多一跑人脸模型。俯视等特殊视角下'
                      '"头顶部"假设可能失效引入误打，此时可强制关。'
                      '"自动"= 极致档开启、其余关闭。',
                      "Crops each person's head region, enlarges it, and runs "
                      'the face model again: small or distant faces get '
                      'multiplied resolution with notably higher recall, at '
                      'one extra face pass per person. In top-down views the '
                      'head-zone assumption can misfire — force off then. '
                      'Auto = on for Extreme.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: ListTile(
                  title: Text(S.t('人脸级联 ROI', 'Face cascade ROI')),
                  subtitle: Text(
                    switch (s.faceRoi) {
                      1 => S.t('强制开启：小脸/远景召回更高', 'Force on: higher recall for small/far faces'),
                      2 => S.t('强制关闭：俯视等特殊视角建议关（头部区域假设失效会多打马）',
                          'Force off: for top-down views (head-zone assumption fails)'),
                      _ => S.t('跟随预设（极致档开启，其余关闭）', 'Follow preset (on for Extreme)'),
                    },
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: SegmentedButton<int>(
                    segments: [
                      ButtonSegment(value: 0, label: Text(S.t('自动', 'Auto'))),
                      ButtonSegment(value: 1, label: Text(S.t('开', 'On'))),
                      ButtonSegment(value: 2, label: Text(S.t('关', 'Off'))),
                    ],
                    selected: {s.faceRoi},
                    onSelectionChanged: (v) => _set((x) => x.faceRoi = v.first),
                  ),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '极限·档案级分析阶段的编码侧输出处理。默认 -f null：帧直接丢弃、'
                      '不编码不落盘（分析的产物是遮罩缓存，编码只为保持管道排空防堵塞）。'
                      '打开后改为真编码并写一个临时探针视频、分析结束即删除——'
                      '仅排查管线/编码问题时需要，会多耗编码功耗与磁盘。',
                      'How the analyze stage drains its encoder side for the '
                      'Archive preset. Default -f null: frames are discarded '
                      'with no encoding or disk writes (analysis output is the '
                      'mask cache; encoding exists only to keep the pipeline '
                      'draining). When on, encodes a temporary probe video and '
                      'deletes it afterwards — only for debugging pipeline or '
                      'encoder issues; costs extra power and disk.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: SwitchListTile(
                    title: Text(S.t('分析段写探针视频', 'Analyze: write probe video')),
                    subtitle: Text(S.t(
                        '默认 -f null 丢弃输出（不落盘）；开=真编码临时视频后删除（调试）',
                        'Default -f null (discard, no files); on = encode temp probe video (debug)')),
                    value: s.analyzeDrainFile,
                    onChanged: (v) => _set((x) => x.analyzeDrainFile = v),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '任务完成时自动在访达中定位并高亮输出视频，'
                      '省去手动去输出目录找文件。',
                      'Automatically reveals and highlights the output video '
                      'in Finder when the task finishes — no manual search.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: SwitchListTile(
                    title: Text(S.t('完成后打开输出文件', 'Reveal output when done')),
                    subtitle: Text(S.t('任务完成时在访达中定位输出', 'Locate the output in Finder when finished')),
                    value: s.openAfterFinish,
                    onChanged: (v) => _set((x) => x.openAfterFinish = v),
                  ),
                ),
              ]),
              _card(S.t('高级参数（0 = 跟随预设）', 'Advanced (0 = follow preset)'), [
                Tooltip(
                  message: S.t(
                      '极限·档案级档的 mask 精修模型规格。large：精度最高'
                      '（档案级定位），但在 Mac 上约 8 秒/帧；tiny：精度略低，'
                      '快约 4 倍（约 2 秒/帧），短/快验场景推荐。分析的 mask '
                      '缓存与规格绑定——换规格需重新分析。',
                      'SAM2.1 model size for the Archive preset. large: best '
                      'mask fidelity (archival) but ~8s/frame on Mac; tiny: '
                      'slightly lower fidelity, ~4x faster (~2s/frame) — '
                      'recommended for quick runs. Mask caches are bound to '
                      'the size; switching requires re-analysis.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: ListTile(
                    title: Text(S.t('极限档 SAM 精修模型', 'Archive SAM refiner')),
                    subtitle: Text(
                      s.archiveSamSize == 'large'
                          ? S.t('large：精度最高，~8 秒/帧', 'large: best fidelity, ~8s/frame')
                          : S.t('tiny：快约 4 倍，~2 秒/帧', 'tiny: ~4x faster, ~2s/frame'),
                      style: const TextStyle(fontSize: 12),
                    ),
                    trailing: SegmentedButton<String>(
                      segments: [
                        ButtonSegment(value: 'large', label: Text(S.t('large', 'large'))),
                        ButtonSegment(value: 'tiny', label: Text(S.t('tiny', 'tiny'))),
                      ],
                      selected: {s.archiveSamSize},
                      onSelectionChanged: (v) => _set((x) => x.archiveSamSize = v.first),
                    ),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '每 N 帧做一次检测，中间帧用跟踪结果推算遮罩位置：'
                      '数值越大跑得越快，但快速运动的目标遮罩会滞后（拖影）。'
                      '0 = 跟随预设（当前预设均为逐帧检测）；低端机器跑不动时'
                      '可调到 2-3 换速度。',
                      'Detects every N frames; in-between frames use tracking '
                      'to move the mask: higher N is faster but fast-moving '
                      'subjects lag (smearing). 0 = follow preset (all '
                      'presets detect every frame); use 2-3 only on slow '
                      'machines.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: ListTile(
                  title: Text(S.t('隔帧检测间隔', 'Detection interval')),
                  subtitle: Text(s.detectEvery == 0
                      ? S.t('自动（预设默认）', 'Auto (preset default)')
                      : S.t('每 ${s.detectEvery} 帧检测一次', 'Every ${s.detectEvery} frames')),
                  trailing: SizedBox(
                    width: 220,
                    child: Slider(
                      value: s.detectEvery.toDouble(),
                      min: 0,
                      max: 4,
                      divisions: 4,
                      label: s.detectEvery == 0 ? S.t('自动', 'Auto') : '${s.detectEvery}',
                      onChanged: (v) => _set((x) => x.detectEvery = v.round()),
                    ),
                  ),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '一次推理同时处理多帧，摊薄每次调用的固定开销，'
                      '整体跑得更快。需要存在对应的 -b4 批模型文件（否则自动'
                      '退回逐帧）。0 = 跟随预设（批 4）；显存吃紧的特大模型'
                      '可降到 1。',
                      'Processes multiple frames per inference to amortize '
                      'fixed overhead — faster overall. Requires the matching '
                      '-b4 batch model (otherwise falls back to per-frame). '
                      '0 = follow preset (batch 4); drop to 1 for huge models '
                      'short on memory.'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: ListTile(
                  title: Text(S.t('批推理大小', 'Batch size')),
                  subtitle: Text(s.batch == 0
                      ? S.t('自动（预设默认）', 'Auto (preset default)')
                      : 'batch = ${s.batch}'),
                  trailing: SizedBox(
                    width: 220,
                    child: Slider(
                      value: s.batch.toDouble(),
                      min: 0,
                      max: 4,
                      divisions: 2,
                      label: s.batch == 0 ? S.t('自动', 'Auto') : '${s.batch}',
                      onChanged: (v) => _set((x) => x.batch = v.round()),
                    ),
                  ),
                  ),
                ),
                Tooltip(
                  message: S.t(
                      '在检测到的人脸框四周额外扩大的像素数，用于盖住'
                      '发际、下巴和检测框的轻微误差：12px 适合大多数人；'
                      '远景小人脸可适当减小、特写大脸可加大。0 = 跟随预设'
                      '（12px）。',
                      'Extra pixels padded around each detected face to cover '
                      'hairline, chin, and small detection errors: 12px suits '
                      'most people; smaller for distant faces, larger for '
                      'close-ups. 0 = follow preset (12px).'),
                  waitDuration: const Duration(milliseconds: 300),
                  constraints: const BoxConstraints(maxWidth: 320),
                  child: ListTile(
                  title: Text(S.t('人脸框外扩', 'Face padding')),
                  subtitle: Text(s.faceExpand == 0
                      ? S.t('自动（预设默认 12px）', 'Auto (preset default 12px)')
                      : '${s.faceExpand} px'),
                  trailing: SizedBox(
                    width: 220,
                    child: Slider(
                      value: s.faceExpand.toDouble(),
                      min: 0,
                      max: 32,
                      divisions: 8,
                      label: s.faceExpand == 0 ? S.t('自动', 'Auto') : '${s.faceExpand}px',
                      onChanged: (v) => _set((x) => x.faceExpand = v.round()),
                    ),
                  ),
                  ),
                ),
              ]),
              _card(S.t('视频参数', 'Video'), [
                ListTile(
                  title: Text(S.t('视频编码器', 'Video encoder')),
                  subtitle: Text(
                    s.encoder == 'auto'
                        ? S.t('自动（按平台候选链 + 运行期回退）',
                            'Auto (platform chain + runtime fallback)')
                        : s.encoder,
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: _pillMenu(
                    value: _validEncoder(s.encoder),
                    options: {
                      for (final e in _encoderChoices())
                        e: e == 'auto' ? S.t('自动', 'Auto') : e,
                    },
                    onSelected: (v) => _set((x) => x.encoder = v),
                  ),
                ),
                ListTile(
                  title: Text(S.t('目标码率', 'Bitrate')),
                  subtitle: Text(
                    s.bitrate == 'auto'
                        ? S.t('自动（按分辨率档位：1080p=6M、4K=20M）',
                            'Auto (by resolution: 1080p=6M, 4K=20M)')
                        : S.t('${s.bitrate}（固定）', '${s.bitrate} (fixed)'),
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: _pillMenu(
                    value: _bitrateChoices.contains(s.bitrate) ? s.bitrate : 'auto',
                    options: {
                      for (final b in _bitrateChoices)
                        b: b == 'auto' ? S.t('自动', 'Auto') : b,
                    },
                    onSelected: (v) => _set((x) => x.bitrate = v),
                  ),
                ),
                ListTile(
                  title: Text(S.t('输出容器', 'Container')),
                  subtitle: Text(
                    switch (s.container) {
                      'mkv' => S.t('MKV（字幕原样保留；兼容性最广）',
                          'MKV (subtitles kept as-is; widest compat)'),
                      _ => S.t('MP4（兼容性最好；字幕转 mov_text）',
                          'MP4 (best compat; subtitles to mov_text)'),
                    },
                    style: const TextStyle(fontSize: 12),
                  ),
                  trailing: SegmentedButton<String>(
                    segments: const [
                      ButtonSegment(value: 'mp4', label: Text('MP4')),
                      ButtonSegment(value: 'mkv', label: Text('MKV')),
                    ],
                    selected: {s.container},
                    onSelectionChanged: (v) => _set((x) => x.container = v.first),
                  ),
                ),
              ]),
              _card(S.t('加速器', 'Backends'), [
                if (_backends.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(16),
                    child: Text(S.t('枚举中…', 'Enumerating…'),
                        style: const TextStyle(color: Color(0xFF9AA3AD), fontSize: 12)),
                  ),
                for (final b in _backends)
                  ListTile(
                    leading: Icon(
                      b.available ? Icons.check_circle : Icons.cancel_outlined,
                      size: 20,
                      color: b.available ? const Color(0xFF4CD964) : Colors.grey,
                    ),
                    title: Text(S.rust(b.label), style: const TextStyle(fontSize: 13)),
                    subtitle: Text(S.rust(b.desc), style: const TextStyle(fontSize: 11)),
                    dense: true,
                  ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 4, 16, 12),
                  child: Text(
                    // CoreML 调度不可查询的说明仅 macOS 相关；其余平台简洁口径
                    S.t('设备在预览调参屏选择；此处为后端枚举（list_backends，DESIGN §4.3）。\n可用性为配置口径${Platform.isMacOS
                            ? '（CoreML 内部调度无法逐算子查询）'
                            : '（EP 初始化失败自动落 CPU）'}.',
                        'Device is chosen on the preview screen; this lists backends (list_backends, DESIGN §4.3).\nAvailability is configuration-level${Platform.isMacOS
                                ? ' (per-operator CoreML scheduling is not queryable)'
                                : ' (falls back to CPU on EP init failure)'}.'),
                    style: TextStyle(color: Color(0xFF9AA3AD), fontSize: 11, height: 1.5),
                  ),
                ),
              ]),
              _card(S.t('模型管理', 'Models'), [
                if (_models.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(16),
                    child: Text(S.t('未找到 models/manifest.json（模型应位于应用 Resources 或仓库 models/ 目录）',
                        'models/manifest.json not found (models should be in '
                        'app Resources or the repo models/ directory)'),
                        style: TextStyle(color: Color(0xFF9AA3AD), fontSize: 12)),
                  ),
                for (final m in _models)
                  ListTile(
                    leading: Icon(
                      m.present ? Icons.check_circle : Icons.error_outline,
                      size: 20,
                      color: m.present ? const Color(0xFF4CD964) : Colors.grey,
                    ),
                    title: Text(m.file, style: const TextStyle(fontSize: 13)),
                    subtitle: Text(
                      '@${m.imgsz} · ${m.sizeMb.toStringAsFixed(1)}MB'
                      '${m.batchPresent ? S.t(' · 批推理 b4', ' · batch b4') : ''}'
                      '${m.present ? '' : S.t(' · 缺失', ' · missing')}'
                      '${_verifyResults[m.file] != null ? '\n${_verifyResults[m.file]}' : ''}',
                      style: const TextStyle(fontSize: 11, height: 1.4),
                    ),
                    isThreeLine: _verifyResults[m.file] != null,
                    dense: true,
                    trailing: m.present
                        ? TextButton(
                            onPressed: () => _verify(m.file),
                            child: Text(S.t('校验', 'Verify')),
                          )
                        : null,
                  ),
              ]),
              _card(S.t('关于', 'About'), [
                ListTile(
                  dense: true,
                  leading: const Icon(Icons.info_outline, size: 20),
                  title: Text(
                      'Simple AutoMosaic v${_packageInfo?.version ?? "…"}',
                      style: const TextStyle(fontSize: 13)),
                  subtitle: Text(
                      S.t('版本号与发布产物一致（app/pubspec.yaml，scripts/version.sh 维护）',
                          'Version matches release artifacts (app/pubspec.yaml, '
                          'maintained by scripts/version.sh)'),
                      style: TextStyle(color: Colors.grey.shade500, fontSize: 11)),
                ),
              ]),
            ],
          ),
        ),
      ),
    );
  }

  /// 值选择菜单：圆角药丸（当前值 + 展开箭头），点击弹出 PopupMenu——
  /// 比原生 DropdownButton 的下划线样式自然、与卡片语言一致。
  Widget _pillMenu({
    required String value,
    required Map<String, String> options,
    required void Function(String) onSelected,
  }) {
    final c = Theme.of(context).colorScheme;
    return PopupMenuButton<String>(
      initialValue: value,
      position: PopupMenuPosition.under,
      constraints: const BoxConstraints(minWidth: 200),
      onSelected: onSelected,
      itemBuilder: (_) => [
        for (final e in options.entries)
          PopupMenuItem(
            value: e.key,
            child: Text(e.value, style: const TextStyle(fontSize: 13)),
          ),
      ],
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
        decoration: BoxDecoration(
          borderRadius: BorderRadius.circular(8),
          border: Border.all(color: c.outlineVariant),
          color: c.surfaceContainerLow,
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(options[value] ?? value, style: const TextStyle(fontSize: 13)),
            const SizedBox(width: 4),
            Icon(Icons.expand_more, size: 16, color: c.onSurfaceVariant),
          ],
        ),
      ),
    );
  }

  Widget _card(String title, List<Widget> children) {
    return Card(
      margin: const EdgeInsets.only(bottom: 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 14, 16, 4),
            child: Text(title, style: Theme.of(context).textTheme.titleMedium),
          ),
          ...children,
        ],
      ),
    );
  }

  /// 平台编码器候选（与 core media::encoder_chain 对齐；非法持久化值回退 auto）。
  static List<String> _encoderChoices() {
    if (Platform.isMacOS) return const ['auto', 'h264_videotoolbox', 'libx264'];
    if (Platform.isWindows) {
      return const ['auto', 'h264_nvenc', 'h264_qsv', 'h264_amf', 'libx264'];
    }
    return const ['auto', 'h264_vaapi', 'h264_nvenc', 'libx264'];
  }

  static String _validEncoder(String e) =>
      _encoderChoices().contains(e) ? e : 'auto';

  static const _bitrateChoices = ['auto', '3M', '6M', '10M', '20M', '30M'];
}
