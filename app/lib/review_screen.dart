/// M5 复核屏（DESIGN §5.6 复核阶段）：masklet 时间轴 + 关键帧刷子/加减点修补。
///
/// 数据流：
/// - review_meta/review_frame：缓存进度 + 原帧（720 RGBA）+ 生效 mask（含补丁）
///   + 实例框（masklet id 标注）；
/// - 笔刷：手势涂抹 add/erase 区域 → review_save_brush 落盘补丁；
/// - 点提示：前景/背景点（±框）→ review_sam_prompt SAM 重提示 → 预览 →
///   接受时与当前 mask 差分 materialize 成 add/erase 两条补丁（渲染段无推理）；
/// - 渲染：archive_render 纯合成出片（进度条 + 完成打开）。
library;
import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'main.dart' show S;
import 'prefs.dart';
import 'queue.dart' show decodeRgba;
import 'src/rust/api/automosaic.dart' as rust;

/// 笔刷模式。
enum _BrushMode { off, add, erase }

/// 点提示模式。
enum _PointMode { off, foreground, background }

class ReviewScreen extends StatefulWidget {
  const ReviewScreen({
    super.key,
    required this.input,
    required this.masksDir,
    required this.output,
    required this.settings,
  });

  final String input;
  final String masksDir;
  final String output;
  final AppSettings settings;

  @override
  State<ReviewScreen> createState() => _ReviewScreenState();
}

class _ReviewScreenState extends State<ReviewScreen> {
  rust.ReviewMeta? _meta;
  int _frame = 0;
  ui.Image? _image;
  Uint8List? _mask; // 全分辨率 0/1
  List<rust.ReviewInstance> _instances = const [];
  bool _loading = false;
  bool _rendering = false;

  _BrushMode _brush = _BrushMode.off;
  _PointMode _point = _PointMode.off;
  final List<double> _points = []; // [x, y, label] 扁平（全分辨率）
  Uint8List? _samPreview; // SAM 重提示预览 mask
  double _samIou = 0;
  /// 笔刷笔画（预览平面，全分辨率 0/1，未保存）。
  Uint8List? _stroke;
  double _strokeRadius = 24;

  bool get _editing =>
      _brush != _BrushMode.off || _point != _PointMode.off;

  @override
  void initState() {
    super.initState();
    _loadMeta();
  }

  Future<void> _loadMeta() async {
    try {
      final m = await rust.reviewMeta(input: widget.input, masksDir: widget.masksDir);
      if (!mounted) return;
      setState(() => _meta = m);
      _loadFrame(0);
    } catch (e) {
      _snack('读取缓存失败: $e');
    }
  }

  Future<void> _loadFrame(int idx) async {
    if (_loading) return;
    _loading = true;
    setState(() => _frame = idx);
    try {
      final f = await rust.reviewFrame(
        input: widget.input,
        masksDir: widget.masksDir,
        frameIdx: BigInt.from(idx),
      );
      final img = await decodeRgba(f.rgba, f.width, f.height);
      if (!mounted) return;
      setState(() {
        _image?.dispose();
        _image = img;
        _mask = f.mask;
        _instances = f.instances;
        _points.clear();
        _samPreview = null;
        _stroke = null;
      });
    } catch (e) {
      _snack('读取帧失败: $e');
    } finally {
      _loading = false;
    }
  }

  void _snack(String msg) {
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(msg)));
    }
  }

  // --------------------------------------------------------------------- //
  // 笔刷
  // --------------------------------------------------------------------- //

  Uint8List _ensureStroke() {
    if (_stroke == null || _stroke!.length != _mask!.length) {
      _stroke = Uint8List(_mask!.length);
    }
    return _stroke!;
  }

  void _paintStroke(Offset pos, double previewW) {
    if (_mask == null || _image == null) return;
    // 预览坐标 → 全分辨率
    final scale = _fullW / previewW;
    final cx = (pos.dx * scale).round();
    final cy = (pos.dy * scale).round();
    final r = (_strokeRadius * scale).round();
    final stroke = _ensureStroke();
    for (var y = (cy - r).clamp(0, _fullH - 1); y <= (cy + r).clamp(0, _fullH - 1); y++) {
      for (var x = (cx - r).clamp(0, _fullW - 1); x <= (cx + r).clamp(0, _fullW - 1); x++) {
        final dx = x - cx, dy = y - cy;
        if (dx * dx + dy * dy <= r * r) {
          stroke[y * _fullW + x] = 1;
        }
      }
    }
    setState(() {}); // 触发叠加层重绘
  }

  Future<void> _saveStroke() async {
    if (_stroke == null || _stroke!.every((v) => v == 0)) return;
    final add = _brush == _BrushMode.add;
    try {
      await rust.reviewSaveBrush(
        masksDir: widget.masksDir,
        frameIdx: BigInt.from(_frame),
        add: add,
        mask: _stroke!,
      );
      setState(() => _stroke = null);
      await _loadFrame(_frame); // 刷新生效 mask
      _snack(add ? '已添加遮罩补丁' : '已保存擦除补丁');
    } catch (e) {
      _snack('保存失败: $e');
    }
  }

  // --------------------------------------------------------------------- //
  // 点提示 SAM
  // --------------------------------------------------------------------- //

  Future<void> _addPoint(Offset pos, double previewW) async {
    if (_mask == null || _image == null) return;
    final scale = _fullW / previewW;
    final x = pos.dx * scale;
    final y = pos.dy * scale;
    final label = _point == _PointMode.foreground ? 1.0 : 0.0;
    _points.addAll([x, y, label]);
    setState(() {}); // 画点
    // 重新提示（SAM 缓存同帧嵌入，第二次起 <1s）
    try {
      final r = await rust.reviewSamPrompt(
        input: widget.input,
        frameIdx: BigInt.from(_frame),
        points: _points,
        box: null,
        samSize: 'large',
      );
      if (!mounted) return;
      setState(() {
        _samPreview = r.mask;
        _samIou = r.iou;
      });
    } catch (e) {
      _snack('SAM 重提示失败: $e');
    }
  }

  /// 接受 SAM 结果：与当前生效 mask 差分 → add/erase 两条补丁（materialize）。
  Future<void> _acceptSam() async {
    if (_samPreview == null || _mask == null) return;
    final addDiff = Uint8List(_mask!.length);
    final eraseDiff = Uint8List(_mask!.length);
    for (var i = 0; i < _mask!.length; i++) {
      final want = _samPreview![i];
      final have = _mask![i];
      if (want == 1 && have == 0) addDiff[i] = 1;
      if (want == 0 && have == 1) eraseDiff[i] = 1;
    }
    try {
      final hasAdd = addDiff.any((v) => v == 1);
      final hasErase = eraseDiff.any((v) => v == 1);
      if (hasAdd) {
        await rust.reviewSaveBrush(
          masksDir: widget.masksDir,
          frameIdx: BigInt.from(_frame),
          add: true,
          mask: addDiff,
        );
      }
      if (hasErase) {
        await rust.reviewSaveBrush(
          masksDir: widget.masksDir,
          frameIdx: BigInt.from(_frame),
          add: false,
          mask: eraseDiff,
        );
      }
      setState(() {
        _samPreview = null;
        _points.clear();
      });
      await _loadFrame(_frame);
      _snack('SAM 修补已保存（IoU ${_samIou.toStringAsFixed(2)}）');
    } catch (e) {
      _snack('保存失败: $e');
    }
  }

  Future<void> _clearFramePatches() async {
    try {
      await rust.reviewClearFrame(
        masksDir: widget.masksDir,
        frameIdx: BigInt.from(_frame),
      );
      await _loadFrame(_frame);
      _snack('已撤销该帧全部补丁');
    } catch (e) {
      _snack('撤销失败: $e');
    }
  }

  // --------------------------------------------------------------------- //
  // 渲染
  // --------------------------------------------------------------------- //

  Future<void> _render() async {
    if (_rendering) return;
    setState(() => _rendering = true);
    final out = widget.output;
    var failed = false;
    try {
      await rust.archiveRender(
        input: widget.input,
        masksDir: widget.masksDir,
        output: out,
        style: widget.settings.style,
        strength: widget.settings.strength.round(),
        hwaccel: 'auto',
        encoder: widget.settings.encoder,
        bitrate: widget.settings.bitrate,
      ).drain((e) {
        if (e is rust.ProcessEvent_Failed) {
          failed = true;
          _snack('渲染失败: ${e.error}');
        } else if (e is rust.ProcessEvent_Finished) {
          if (widget.settings.openAfterFinish) unawaited(revealFile(out));
          _snack('渲染完成: ${out.split('/').last}');
        }
      });
    } catch (e) {
      _snack('渲染失败: $e');
    }
    if (mounted) setState(() => _rendering = false);
    final _ = failed;
  }

  int get _fullW => _meta?.width ?? 0;
  int get _fullH => _meta?.height ?? 0;

  // --------------------------------------------------------------------- //
  // UI
  // --------------------------------------------------------------------- //

  @override
  Widget build(BuildContext context) {
    final total = _meta == null ? 0 : _meta!.frames.toInt();
    return Scaffold(
      appBar: AppBar(
        title: Text(S.t('复核 · ', 'Review · ') + widget.input.split('/').last),
        actions: [
          IconButton(
            tooltip: S.t('撤销本帧补丁', 'Undo patches on this frame'),
            onPressed: _clearFramePatches,
            icon: const Icon(Icons.undo),
          ),
        ],
      ),
      body: Column(
        children: [
          Expanded(child: _viewer()),
          _toolbar(),
          _timeline(total),
        ],
      ),
    );
  }

  Widget _viewer() {
    if (_image == null || _mask == null) {
      return const Center(child: CircularProgressIndicator());
    }
    return LayoutBuilder(builder: (context, box) {
      // 图像letterbox 适配
      final iw = _image!.width.toDouble();
      final ih = _image!.height.toDouble();
      final scale = box.maxWidth / iw;
      final dispH = ih * scale;
      return Center(
        child: SizedBox(
          width: box.maxWidth,
          height: dispH,
          child: GestureDetector(
            onPanUpdate: _brush == _BrushMode.off && _point == _PointMode.off
                ? null
                : (d) {
                    if (_brush != _BrushMode.off) {
                      _paintStroke(d.localPosition, box.maxWidth);
                    }
                  },
            onTapUp: _point == _PointMode.off
                ? null
                : (d) => _addPoint(d.localPosition, box.maxWidth),
            child: CustomPaint(
              painter: _ReviewPainter(
                image: _image!,
                mask: _mask!,
                maskW: _fullW,
                instances: _instances,
                stroke: _stroke,
                strokeAdd: _brush == _BrushMode.add,
                points: _points,
                samPreview: _samPreview,
                previewW: box.maxWidth,
              ),
              size: Size(box.maxWidth, dispH),
            ),
          ),
        ),
      );
    });
  }

  Widget _toolbar() {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      child: Wrap(
        spacing: 8,
        runSpacing: 4,
        alignment: WrapAlignment.center,
        children: [
          ChoiceChip(
            label: Text(S.t('笔刷·加', 'Brush·Add')),
            selected: _brush == _BrushMode.add,
            onSelected: (v) => setState(() {
              _brush = v ? _BrushMode.add : _BrushMode.off;
              _stroke = null;
            }),
          ),
          ChoiceChip(
            label: Text(S.t('笔刷·擦', 'Brush·Erase')),
            selected: _brush == _BrushMode.erase,
            onSelected: (v) => setState(() {
              _brush = v ? _BrushMode.erase : _BrushMode.off;
              _stroke = null;
            }),
          ),
          if (_brush != _BrushMode.off) ...[
            Slider(
              value: _strokeRadius,
              min: 6,
              max: 80,
              onChanged: (v) => setState(() => _strokeRadius = v),
            ),
            FilledButton.tonal(
              onPressed: _saveStroke,
              child: Text(S.t('保存笔画', 'Save stroke')),
            ),
          ],
          ChoiceChip(
            label: Text(S.t('点·前景', 'Point·FG')),
            selected: _point == _PointMode.foreground,
            onSelected: (v) => setState(() {
              _point = v ? _PointMode.foreground : _PointMode.off;
              _points.clear();
              _samPreview = null;
            }),
          ),
          ChoiceChip(
            label: Text(S.t('点·背景', 'Point·BG')),
            selected: _point == _PointMode.background,
            onSelected: (v) => setState(() {
              _point = v ? _PointMode.background : _PointMode.off;
              _points.clear();
              _samPreview = null;
            }),
          ),
          if (_samPreview != null)
            FilledButton(
              onPressed: _acceptSam,
              child: Text(S.t('接受 SAM（IoU ${_samIou.toStringAsFixed(2)}）',
                  'Accept SAM (IoU ${_samIou.toStringAsFixed(2)})')),
            ),
          if (_samPreview != null)
            TextButton(
              onPressed: () => setState(() {
                _samPreview = null;
                _points.clear();
              }),
              child: Text(S.t('丢弃', 'Discard')),
            ),
        ],
      ),
    );
  }

  Widget _timeline(int total) {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
        child: Row(
          children: [
            Text('$_frame/${total > 0 ? total - 1 : 0}'),
            Expanded(
              child: Slider(
                value: total > 1 ? _frame.clamp(0, total - 1).toDouble() : 0,
                max: (total > 1 ? total - 1 : 1).toDouble(),
                onChanged: total > 1
                    ? (v) {
                        if (!_editing) _loadFrame(v.round());
                      }
                    : null,
              ),
            ),
            SizedBox(
              width: 200,
              child: FilledButton.icon(
                onPressed: _rendering ? null : _render,
                icon: _rendering
                    ? const SizedBox(
                        width: 14,
                        height: 14,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.movie),
                label: Text(S.t('渲染输出', 'Render')),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 复核叠加绘制：原图 + 红色半透明 mask + 笔画 + SAM 预览 + 点 + 实例框。
class _ReviewPainter extends CustomPainter {
  _ReviewPainter({
    required this.image,
    required this.mask,
    required this.maskW,
    required this.instances,
    this.stroke,
    required this.strokeAdd,
    required this.points,
    this.samPreview,
    required this.previewW,
  });

  final ui.Image image;
  final Uint8List mask;
  final int maskW;
  final List<rust.ReviewInstance> instances;
  final Uint8List? stroke;
  final bool strokeAdd;
  final List<double> points;
  final Uint8List? samPreview;
  final double previewW;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawImageRect(
      image,
      Rect.fromLTWH(0, 0, image.width.toDouble(), image.height.toDouble()),
      Offset.zero & size,
      Paint(),
    );
    final scale = previewW / image.width;
    final fullW = maskW;
    final fullH = mask.isNotEmpty ? mask.length ~/ fullW : 0;
    if (fullW == 0 || fullH == 0) return;

    // 生效 mask（红 40%）
    _paintPlane(canvas, size, mask, fullW, fullH, scale, const Color(0x66FF2020));
    // SAM 预览（绿 50%）
    if (samPreview != null) {
      _paintPlane(canvas, size, samPreview!, fullW, fullH, scale, const Color(0x8800CC66));
    }
    // 笔画（橙）
    if (stroke != null) {
      _paintPlane(canvas, size, stroke!, fullW, fullH, scale,
          strokeAdd ? const Color(0x99FF9800) : const Color(0x992196F3));
    }
    // 点提示
    for (var i = 0; i + 2 < points.length; i += 3) {
      final p = Paint()..color = points[i + 2] == 1 ? Colors.cyan : Colors.pinkAccent;
      canvas.drawCircle(Offset(points[i] * scale, points[i + 1] * scale), 6, p);
      canvas.drawCircle(
        Offset(points[i] * scale, points[i + 1] * scale),
        8,
        Paint()
          ..color = Colors.white
          ..style = PaintingStyle.stroke
          ..strokeWidth = 2,
      );
    }
    // 实例框 + id 标签
    final boxPaint = Paint()
      ..color = Colors.amber
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5;
    final tp = TextPainter(textDirection: TextDirection.ltr);
    for (final inst in instances) {
      final r = Rect.fromLTRB(
        inst.x1 * scale,
        inst.y1 * scale,
        inst.x2 * scale,
        inst.y2 * scale,
      );
      canvas.drawRect(r, boxPaint);
      tp.text = TextSpan(
        text: '#${inst.id}',
        style: const TextStyle(color: Colors.amber, fontSize: 10),
      );
      tp.layout();
      tp.paint(canvas, r.topLeft);
    }
  }

  void _paintPlane(Canvas canvas, Size size, Uint8List plane, int fullW, int fullH,
      double scale, Color color) {
    final paint = Paint()..color = color;
    // 下采样 4px 绘制矩形条带（全像素 drawRect 过慢）
    for (var y = 0; y < fullH; y += 3) {
      var runStart = -1;
      for (var x = 0; x <= fullW; x += 3) {
        final on = x < fullW && plane[y * fullW + x] == 1;
        if (on && runStart < 0) runStart = x;
        if (!on && runStart >= 0) {
          canvas.drawRect(
            Rect.fromLTRB(runStart * scale, y * scale, x * scale, (y + 3) * scale),
            paint,
          );
          runStart = -1;
        }
      }
    }
    final _ = size; //（size 未直接使用，矩形已按 scale 换算）
  }

  @override
  bool shouldRepaint(covariant _ReviewPainter old) =>
      old.mask != mask ||
      old.stroke != stroke ||
      old.samPreview != samPreview ||
      old.points.length != points.length ||
      old.instances.length != instances.length;
}
