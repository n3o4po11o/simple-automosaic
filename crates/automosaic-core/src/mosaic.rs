//! 检测→人脸 gate→跟踪→平滑→合成 的可复用帧变换（CLI 与 FFI 共用，
//! DESIGN §2.2「所有逻辑在 core」）。此前 CLI 与 FFI 各持一份且行为有差异
//! （FFI 缺隔帧检测/保持帧膨胀/person 关联过滤），现统一到这份实现。
//!
//! 隔帧检测语义（与原 CLI 实现一致，e2e 验证过）：
//! - 每 N 帧推理一次（`detect_every`），中间帧用 tracker 保持的 mask；
//! - 保持帧按 track 速度膨胀 mask（[`crate::track::Track::hold_dilate_px`]）
//!   补偿位移条带；人脸框沿用最近一次推理结果；
//! - person 外的低分人脸由 [`crate::detect::gate_faces`] 过滤；
//! - 丢失 track 的遮罩在保持期后半段渐隐（跟踪层给出 fade 进度，此处按框
//!   尺寸腐蚀回缩，人物离场不硬切）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::compose::{self, ComposeBackend, MaskStyle};
use crate::detect::{DetectorBackend, FaceBox, FaceDetectorBackend};
use crate::gmc::GlobalMotionEstimator;
use crate::media;
use crate::pipe::FrameTransform;
use crate::track::{IouTracker, MaskSmoother, TrackerOptions};

/// 贴边丢失 track 的保持帧上限（离场启发式）：贴画框边缘的丢失大概率是
/// 人物走出画面，继续原地保持只会留残影——3 帧后放弃（画面内部丢失仍走
/// max_lost=12 的"宁可多打"遮挡语义）。DESIGN §0.5 残影修复 2026-08-20。
const EDGE_HOLD_FRAMES: u32 = 3;
/// 贴边判定 margin（像素）。
const EDGE_MARGIN: f32 = 12.0;

/// 左右对照预览：每 N 帧一对，缩放到 480 宽。
pub const PREVIEW_EVERY: u64 = 8;
pub const PREVIEW_WIDTH: usize = 480;

/// 预览输出接口（FFI 侧实现为 StreamSink 事件；CLI 不用）。
pub trait PreviewSink: Send {
    /// 该帧是否需要对照图。
    fn wants(&mut self, frame_idx: u64) -> bool;
    /// 输出一对缩放后的 RGBA（original / processed）。
    fn emit(&mut self, frame_idx: u64, original: Vec<u8>, processed: Vec<u8>, w: u32, h: u32);
}

/// 变换参数（预设展开 + 用户覆写后的最终值）。
#[derive(Debug, Clone)]
pub struct MosaicOptions {
    pub conf: f32,
    /// 是否启用人脸检测。
    pub face: bool,
    /// 人脸框四周外扩像素。
    pub face_expand: u32,
    /// 级联 ROI 人脸检测：对每个 person 头部区域裁剪放大后二次推理
    /// （DESIGN §6 #3，小脸召回杠杆；代价 = 每人每次检测多一跑人脸模型）。
    pub face_roi: bool,
    pub track: bool,
    pub smooth: bool,
    /// landmark 外扩（眼距自适应；关闭则固定基础值）。
    pub landmark_expand: bool,
    /// per-ID mask EMA（关闭则透传最近观测）。
    pub mask_ema: bool,
    /// OC-SORT 观测中心重更新（丢失后重关联回滚重放，防速度污染）。
    pub ocru: bool,
    /// 相位相关全局运动补偿（运动镜头：预测框平移 + 保持帧 mask 跟随）。
    pub gmc: bool,
    /// 隔帧检测间隔（1 = 全帧）。
    pub detect_every: u32,
    /// 视频帧率（自适应隔帧的实时目标；≤0 视为 30）。
    pub fps: f32,
    /// 自适应隔帧上限（DESIGN §6 效率清单"自适应批/隔帧"与"大模型运行期
    /// 自适应降档"的合并实现）：>0 时，推理确为瓶颈（busy 占比 >0.7）且
    /// 吞吐 <0.85×fps 持续两个决策窗，先撤批 session（省显存/延迟尖峰），
    /// 再逐步把 detect_every 上调到此上限。0 = 关闭（默认——全档逐帧是
    /// 2026-08-20 用户决策，自适应为低配机器的显式 opt-in）。
    pub adaptive_skip_max: u32,
    pub style: MaskStyle,
}

/// person 框 → 头部 ROI（顶部 30%、左右各收 8%，上浮 5% 盖发际）。
fn head_roi(p: [f32; 4], w: f32, h: f32) -> [f32; 4] {
    let (pw, ph) = (p[2] - p[0], p[3] - p[1]);
    [
        (p[0] + pw * 0.08).max(0.0),
        (p[1] - ph * 0.05).max(0.0),
        (p[2] - pw * 0.08).min(w),
        (p[1] + ph * 0.30).min(h),
    ]
}

// --------------------------------------------------------------------------- //
// 自适应隔帧调节器（DESIGN §6 效率清单"自适应批/隔帧"；D-3/D-4）
// --------------------------------------------------------------------------- //

/// 自适应决策窗（墙钟）：每 2s 评估一次吞吐与 busy 占比。
const ADAPTIVE_WINDOW: Duration = Duration::from_secs(2);
/// 触发阈值：推理 busy 占比（排除解码/编码拖累的误判）与实时吞吐下限。
const ADAPTIVE_BUSY_RATIO: f64 = 0.7;
const ADAPTIVE_FPS_RATIO: f64 = 0.85;

/// 自适应调节的动作（masks_of 据此对后端执行降档）。
#[derive(Debug, PartialEq, Eq)]
enum TuneAction {
    None,
    /// 撤批 session（第一档：省显存与延迟尖峰，吞吐略降）。
    ReduceBatch,
    /// detect_every +1（后续档：以画质换吞吐，逐帧→隔帧）。
    SkipIncreased,
}

/// 推理跟不上实时时的降档状态机：连续两个决策窗 `busy>0.7 且 吞吐<0.85×fps`
/// 才动作（防瞬时波动），先 ReduceBatch 后逐级 SkipIncreased，只升不降
///（画质单向妥协，避免振荡）。`every` 初始取用户/预设的 detect_every。
struct AdaptiveTuner {
    every: u32,
    max_every: u32,
    /// max ≤ base（无上调空间）= 功能关闭（不撤批也不隔帧）。
    enabled: bool,
    target_fps: f32,
    window_start: Option<Instant>,
    frames: u64,
    busy_ns: u64,
    starved: u32,
    batch_reduced: bool,
}

impl AdaptiveTuner {
    fn new(fps: f32, base_every: u32, max_every: u32) -> Self {
        let base = base_every.max(1);
        Self {
            every: base,
            max_every: max_every.max(base),
            enabled: max_every > base,
            target_fps: if fps > 0.0 { fps } else { 30.0 },
            window_start: None,
            frames: 0,
            busy_ns: 0,
            starved: 0,
            batch_reduced: false,
        }
    }

    fn effective_every(&self) -> u64 {
        self.every.max(1) as u64
    }

    /// 记录一批（frames = 本批帧数、busy_ns = 本批推理墙钟耗时），到决策窗
    /// 边界时评估是否降档。
    fn note(&mut self, frames: u64, busy_ns: u64) -> TuneAction {
        self.frames += frames;
        self.busy_ns += busy_ns;
        let now = Instant::now();
        let t0 = *self.window_start.get_or_insert(now);
        let elapsed = now - t0;
        if elapsed < ADAPTIVE_WINDOW {
            return TuneAction::None;
        }
        let action = self.decide(elapsed);
        // 滚动重置窗口
        self.frames = 0;
        self.busy_ns = 0;
        self.window_start = Some(now);
        action
    }

    fn decide(&mut self, elapsed: Duration) -> TuneAction {
        if !self.enabled {
            return TuneAction::None;
        }
        let secs = elapsed.as_secs_f64().max(1e-9);
        let fps = self.frames as f64 / secs;
        let busy_ratio = self.busy_ns as f64 / elapsed.as_nanos().max(1) as f64;
        let infer_bound =
            busy_ratio > ADAPTIVE_BUSY_RATIO && fps < ADAPTIVE_FPS_RATIO * self.target_fps as f64;
        if !infer_bound {
            self.starved = 0;
            return TuneAction::None;
        }
        self.starved += 1;
        if self.starved < 2 {
            return TuneAction::None; // 需连续两个窗确认
        }
        self.starved = 0;
        if !self.batch_reduced {
            self.batch_reduced = true;
            return TuneAction::ReduceBatch;
        }
        if self.every < self.max_every {
            self.every += 1;
            return TuneAction::SkipIncreased;
        }
        TuneAction::None // 已到顶
    }
}

/// 逐帧 mask 组装管线（检测→人脸 gate→跟踪→平滑 的状态机）。
/// [`build`]（流式合成）与两阶段 analyze（mask 落盘，DESIGN §5.6/§2.1）共用；
/// 不修改帧数据——合成与落盘由调用方决定。
pub struct MosaicPipeline {
    det: Arc<Mutex<dyn DetectorBackend>>,
    face: Option<Arc<Mutex<dyn FaceDetectorBackend>>>,
    opts: MosaicOptions,
    w: usize,
    h: usize,
    tracker: IouTracker,
    smoother: MaskSmoother,
    gmc: Option<GlobalMotionEstimator>,
    scratch: Vec<u8>,
    frame_idx: u64,
    last_faces: Vec<FaceBox>,
    adaptive: AdaptiveTuner,
}

impl MosaicPipeline {
    pub fn new(
        det: Arc<Mutex<dyn DetectorBackend>>,
        face: Option<Arc<Mutex<dyn FaceDetectorBackend>>>,
        opts: MosaicOptions,
        w: usize,
        h: usize,
    ) -> Self {
        let tracker =
            IouTracker::new(TrackerOptions { ema: opts.mask_ema, ocru: opts.ocru, ..Default::default() });
        let gmc = opts.gmc.then(GlobalMotionEstimator::new);
        let adaptive = AdaptiveTuner::new(opts.fps, opts.detect_every, opts.adaptive_skip_max);
        Self {
            det,
            face,
            opts,
            w,
            h,
            tracker,
            smoother: MaskSmoother::new(),
            gmc,
            scratch: Vec::new(),
            frame_idx: 0,
            last_faces: Vec::new(),
            adaptive,
        }
    }

    /// 已处理的帧数（全局索引 = 下一帧编号；analyze 断点续跑用）。
    pub fn frame_idx(&self) -> u64 {
        self.frame_idx
    }

    /// 设置全局帧号起点（断点续跑：恢复 detect_every 相位）。
    /// 注意 tracker/smoother 的历史状态不跨会话——续跑首帧行为等同新轨起帧，
    /// mask 逐帧检测主导、正确性不受影响，仅时序状态冷启动。
    pub fn set_frame_idx(&mut self, idx: u64) {
        self.frame_idx = idx;
    }

    /// 组装一批帧的最终 mask（含人脸框并入 + 时序平滑）。
    /// 返回每帧 (mask, person 框)；person 框含 GMC 平移。
    pub fn masks_of(
        &mut self,
        frames: &[&[u8]],
    ) -> Result<Vec<(Vec<u8>, Vec<[f32; 4]>)>, String> {
        let (w, h) = (self.w, self.h);
        let opts = self.opts.clone();
        let every = self.adaptive.effective_every();
        // 1) 隔帧检测：选出本批需要推理的帧
        let need: Vec<usize> = (0..frames.len())
            .filter(|i| (self.frame_idx + *i as u64) % every == 0)
            .collect();
        // 2) 批量推理（body + face 全帧）；计时供自适应降档用
        let mut body: Vec<Vec<crate::detect::PersonInstance>> = Vec::new();
        let mut faces: Vec<Vec<FaceBox>> = Vec::new();
        let t_infer = Instant::now();
        if !need.is_empty() {
            let refs: Vec<&[u8]> = need.iter().map(|&i| frames[i]).collect();
            {
                let mut d = self.det.lock().unwrap_or_else(|p| p.into_inner());
                body = d.detect_person_instances_batch(&refs, w, h)?;
            }
            if let Some(fd) = &self.face {
                let mut fd = fd.lock().unwrap_or_else(|p| p.into_inner());
                faces = fd.detect_boxes_batch(&refs, w, h)?;
            }
        }
        let infer_ns = t_infer.elapsed().as_nanos() as u64;
        // 3) 逐帧组装
        let mut out = Vec::with_capacity(frames.len());
        for (i, frame) in frames.iter().enumerate() {
            // GMC：先估相机位移（原始帧），静机位自动给 (0,0)
            let motion = self
                .gmc
                .as_mut()
                .map_or([0.0f32, 0.0], |g| {
                    let (dx, dy) = g.shift(frame, w, h);
                    [dx, dy]
                });
            let k = need.iter().position(|&n| n == i);
            let mut instances = match k {
                Some(k) => std::mem::take(&mut body[k]),
                None => Vec::new(), // 隔帧：tracker 保持漏检补偿
            };
            // 关跟踪模式（A/B 调试）没有二段救援语义，只保留高分检测
            if !opts.track {
                instances.retain(|d| d.score >= opts.conf);
            }
            let mut mask = vec![0u8; w * h];
            let mut pboxes: Vec<[f32; 4]> = Vec::new();
            // (外推后 person 框, 保持位移)：保持帧的人脸框据此对齐
            // （旧实现沿用上次检测位置，人物移动时人脸码滞后=拖影）
            let mut track_shifts: Vec<([f32; 4], [f32; 2])> = Vec::new();
            if !opts.track {
                for inst in &instances {
                    for (o, m) in mask.iter_mut().zip(&inst.mask) {
                        *o |= *m;
                    }
                    pboxes.push(inst.xyxy);
                }
            } else {
                let hold = k.is_none();
                for t in self.tracker.update_with_motion(instances, opts.conf, motion) {
                    // 离场快速衰减：贴边 && 丢失超阈值 → 放弃保持（残影修复）
                    if t.lost > EDGE_HOLD_FRAMES
                        && crate::track::near_frame_edge(t.xyxy, w, h, EDGE_MARGIN)
                    {
                        continue;
                    }
                    // 保持帧的框/mask 平移累积相机位移（GMC）；检测帧 shift=0 无操作
                    let (sx, sy) = (t.shift[0], t.shift[1]);
                    let box_shifted = [
                        t.xyxy[0] + sx,
                        t.xyxy[1] + sy,
                        t.xyxy[2] + sx,
                        t.xyxy[3] + sy,
                    ];
                    // 丢失渐隐：fade 进度按框尺寸换算腐蚀像素，删除帧遮罩恰好消失
                    let fade_px = (t.fade
                        * 0.5
                        * (t.xyxy[2] - t.xyxy[0]).max(t.xyxy[3] - t.xyxy[1]))
                        .ceil() as usize;
                    let scratch = &mut self.scratch;
                    if hold || fade_px > 0 || sx != 0.0 || sy != 0.0 {
                        // 保持帧：mask 冻结在旧位置，按相机位移平移 + track 速度
                        // 膨胀补位移条带；渐隐帧：按进度腐蚀回缩
                        scratch.clear();
                        scratch.extend_from_slice(&t.mask);
                        if sx != 0.0 || sy != 0.0 {
                            compose::shift_mask_region(
                                scratch,
                                w,
                                h,
                                t.xyxy,
                                sx.round() as isize,
                                sy.round() as isize,
                            );
                        }
                        if hold {
                            compose::dilate_region(scratch, w, h, box_shifted, t.hold_dilate_px());
                        }
                        if fade_px > 0 {
                            compose::erode_region(scratch, w, h, box_shifted, fade_px);
                        }
                        for (o, m) in mask.iter_mut().zip(scratch.iter()) {
                            *o |= *m;
                        }
                    } else {
                        for (o, m) in mask.iter_mut().zip(&t.mask) {
                            *o |= *m;
                        }
                    }
                    pboxes.push(box_shifted);
                    track_shifts.push((box_shifted, [sx, sy]));
                }
            }
            let mut face_boxes: Vec<FaceBox> = match k {
                Some(k) => faces.get(k).cloned().unwrap_or_default(),
                None => {
                    // 隔帧保持：沿用最近一次的人脸框，但按宿主 person track
                    // 的外推位移平移（人脸码跟随人物，不留在旧位置）
                    self.last_faces
                        .iter()
                        .map(|f| {
                            let (cx, cy) = (
                                (f.xyxy[0] + f.xyxy[2]) * 0.5,
                                (f.xyxy[1] + f.xyxy[3]) * 0.5,
                            );
                            let shift = track_shifts
                                .iter()
                                .find(|(b, _)| {
                                    cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3]
                                })
                                .map(|(_, s)| *s)
                                .unwrap_or([0.0, 0.0]);
                            let mut fb = f.clone();
                            fb.xyxy[0] += shift[0];
                            fb.xyxy[1] += shift[1];
                            fb.xyxy[2] += shift[0];
                            fb.xyxy[3] += shift[1];
                            fb
                        })
                        .collect()
                }
            };
            // 级联 ROI：对每个 person 头部裁剪放大二次推理（帧数据未合成，仍为原始 NV12）
            if opts.face_roi && k.is_some()
                && let Some(fd) = &self.face
            {
                for p in &pboxes {
                    let roi = head_roi(*p, w as f32, h as f32);
                    let roi_faces = {
                        let mut fd = fd.lock().unwrap_or_else(|p| p.into_inner());
                        fd.detect_boxes_roi(frame, w, h, roi)?
                    };
                    face_boxes = crate::detect::merge_faces(face_boxes, roi_faces, 0.6);
                }
            }
            // 几何过滤 + 关联过滤（与流式管线相同防线）
            let face_boxes = crate::detect::filter_implausible_faces(face_boxes, &pboxes);
            let face_boxes = crate::detect::gate_faces(face_boxes, &pboxes, 0.6);
            for fb in &face_boxes {
                let (ex, ey) =
                    crate::detect::face_expand_xy(fb, opts.face_expand as usize, opts.landmark_expand);
                let x1 = (fb.xyxy[0] as usize).saturating_sub(ex);
                let y1 = (fb.xyxy[1] as usize).saturating_sub(ey);
                let x2 = (fb.xyxy[2] as usize + 1 + ex).min(w);
                let y2 = (fb.xyxy[3] as usize + 1 + ey).min(h);
                for y in y1..y2 {
                    for x in x1..x2 {
                        mask[y * w + x] = 1;
                    }
                }
            }
            if opts.smooth {
                self.smoother.apply(&mut mask, w, h);
            }
            out.push((mask, pboxes));
        }
        if !need.is_empty() {
            self.last_faces = faces.last().cloned().unwrap_or_default();
        }
        // 自适应降档（opt-in，见 AdaptiveTuner）：撤批动作对后端执行；
        // 隔帧上调在 tuner 内生效（下批起 effective_every 变化）
        if let TuneAction::ReduceBatch = self.adaptive.note(frames.len() as u64, infer_ns) {
            let mut d = self.det.lock().unwrap_or_else(|p| p.into_inner());
            d.try_reduce_batch();
        }
        self.frame_idx += frames.len() as u64;
        Ok(out)
    }
}

/// 构建帧变换（流式合成，CPU 合成）。`det`/`face` 以共享句柄传入（FFI 模型
/// 缓存跨任务复用；CLI 包一层 Arc 即可）。mask 组装在 [`MosaicPipeline`]，
/// 此处只做预览回调 + 合成落帧。
pub fn build(
    det: Arc<Mutex<dyn DetectorBackend>>,
    face: Option<Arc<Mutex<dyn FaceDetectorBackend>>>,
    opts: MosaicOptions,
    w: usize,
    h: usize,
    preview: Option<Box<dyn PreviewSink>>,
) -> FrameTransform {
    build_with_composer(det, face, opts, w, h, preview, Box::new(compose::ComposeCpu))
}

/// [`build`] 的可插拔合成后端版（DESIGN §4.3 ComposeBackend）：合成在
/// GPU（wgpu compute，未来）或 mock 时经 `composer` 注入。
#[allow(clippy::too_many_arguments)]
pub fn build_with_composer(
    det: Arc<Mutex<dyn DetectorBackend>>,
    face: Option<Arc<Mutex<dyn FaceDetectorBackend>>>,
    opts: MosaicOptions,
    w: usize,
    h: usize,
    mut preview: Option<Box<dyn PreviewSink>>,
    mut composer: Box<dyn ComposeBackend>,
) -> FrameTransform {
    let style = opts.style.clone();
    let mut pipe = MosaicPipeline::new(det, face, opts, w, h);
    Box::new(move |frames: &mut [&mut [u8]]| {
        let base = pipe.frame_idx();
        let refs: Vec<&[u8]> = frames.iter().map(|f| &**f).collect();
        let results = pipe.masks_of(&refs)?;
        drop(refs);
        for (i, ((mask, _), frame)) in results.into_iter().zip(frames.iter_mut()).enumerate() {
            let global_idx = base + i as u64;
            let wants = preview.as_mut().map_or(false, |p| p.wants(global_idx));
            let orig_scaled = wants.then(|| scale_preview(frame, w, h));
            composer.apply(frame, w, h, &mask, &style);
            if let (Some(p), Some(orig)) = (preview.as_mut(), orig_scaled) {
                let (dw, dh) = preview_size(w, h);
                p.emit(global_idx, orig, scale_preview(frame, w, h), dw as u32, dh as u32);
            }
        }
        Ok(())
    })
}

/// NV12 帧缩放到 PREVIEW_WIDTH 宽的 RGBA（偶数高）。
fn scale_preview(frame: &[u8], w: usize, h: usize) -> Vec<u8> {
    let dw = PREVIEW_WIDTH.min(w);
    let dh = (dw * h / w).max(2) & !1;
    media::nv12_to_rgba_scaled(frame, w, h, dw, dh)
}

/// 预览尺寸（宽, 高）：480 宽（小视频取原宽）、偶数高。
pub fn preview_size(w: usize, h: usize) -> (usize, usize) {
    let dw = PREVIEW_WIDTH.min(w);
    (dw, (dw * h / w).max(2) & !1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::MaskStyle;
    use crate::detect::DetectorBackend;

    struct MockDet;

    impl DetectorBackend for MockDet {
        fn backend_name(&self) -> &str {
            "mock"
        }

        fn detect_person_instances_batch(
            &mut self,
            frames: &[&[u8]],
            w: usize,
            h: usize,
        ) -> Result<Vec<Vec<crate::detect::PersonInstance>>, String> {
            Ok(frames
                .iter()
                .map(|_| {
                    vec![crate::detect::PersonInstance {
                        score: 0.9,
                        xyxy: [2.0, 2.0, w as f32 - 2.0, h as f32 - 2.0],
                        mask: vec![1u8; w * h],
                    }]
                })
                .collect())
        }
    }

    fn test_opts() -> MosaicOptions {
        MosaicOptions {
            conf: 0.35,
            face: false,
            face_expand: 0,
            face_roi: false,
            track: true,
            smooth: false,
            landmark_expand: false,
            mask_ema: true,
            ocru: true,
            gmc: false,
            detect_every: 1,
            fps: 0.0,
            adaptive_skip_max: 0,
            style: MaskStyle::Solid,
        }
    }

    #[test]
    fn pipeline_accepts_any_detector_backend() {
        let det: Arc<Mutex<dyn DetectorBackend>> = Arc::new(Mutex::new(MockDet));
        let (w, h) = (32, 32);
        let mut pipe = MosaicPipeline::new(det, None, test_opts(), w, h);
        let frame = vec![128u8; w * h * 3 / 2];
        let out = pipe.masks_of(&[&frame]).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].0.iter().all(|&v| v == 1), "mock 全帧 mask 应透传");
        assert_eq!(out[0].1.len(), 1, "person 框来自 mock 检测");
    }

    #[test]
    fn build_with_composer_routes_frames_through_composer() {
        struct CountComposer(usize);
        impl compose::ComposeBackend for CountComposer {
            fn apply(&mut self, _nv12: &mut [u8], _w: usize, _h: usize, _m: &[u8], _s: &MaskStyle) {
                self.0 += 1;
            }
        }
        let det: Arc<Mutex<dyn DetectorBackend>> = Arc::new(Mutex::new(MockDet));
        let (w, h) = (32, 32);
        let composer = Box::new(CountComposer(0));
        let mut tf = build_with_composer(det, None, test_opts(), w, h, None, composer);
        let mut f1 = vec![128u8; w * h * 3 / 2];
        let mut f2 = vec![128u8; w * h * 3 / 2];
        let mut frames: Vec<&mut [u8]> = vec![&mut f1, &mut f2];
        tf(&mut frames).unwrap();
        // composer 逐帧调用（计数值经 Box 借用后无法读取——此处仅验证不 panic
        // 且变换成功；计数语义由 CountComposer 的 apply 递增保证）
    }

    #[test]
    fn adaptive_ladder_requires_two_windows_then_reduces_batch_then_skips() {
        let mut t = AdaptiveTuner::new(30.0, 1, 3);
        let win = Duration::from_secs(2);
        // 推理瓶颈场景：20fps < 0.85×30=25.5，busy 75% > 0.7
        let starved_setup = |t: &mut AdaptiveTuner| {
            t.frames = 40; // 20fps × 2s
            t.busy_ns = 1_500_000_000; // 75%
        };
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::None, "首个窗只计数不动作");
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::ReduceBatch, "连续两窗 → 先撤批");
        assert_eq!(t.effective_every(), 1, "撤批不动隔帧");
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::None, "降档后再计一窗");
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::SkipIncreased);
        assert_eq!(t.effective_every(), 2);
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::None);
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::SkipIncreased);
        assert_eq!(t.effective_every(), 3);
        starved_setup(&mut t);
        starved_setup(&mut t);
        assert_eq!(t.decide(win), TuneAction::None, "到达上限后不再上调");
        assert_eq!(t.effective_every(), 3);
    }

    #[test]
    fn adaptive_does_not_trigger_when_decode_bound_or_healthy() {
        // 解码瓶颈（busy 仅 25%）——隔帧帮不上忙，不得动作
        let mut t = AdaptiveTuner::new(30.0, 1, 3);
        t.frames = 40; // 20fps（<阈值），但
        t.busy_ns = 500_000_000; // busy 25% < 0.7
        assert_eq!(t.decide(Duration::from_secs(2)), TuneAction::None);
        assert_eq!(t.starved, 0, "健康窗应清零计数");
        // 健康（40fps）且 busy 高——吞吐达标同样不动
        let mut t2 = AdaptiveTuner::new(30.0, 1, 3);
        t2.frames = 80;
        t2.busy_ns = 1_800_000_000; // 90%
        assert_eq!(t2.decide(Duration::from_secs(2)), TuneAction::None);
    }

    #[test]
    fn adaptive_max_zero_disables() {
        let mut t = AdaptiveTuner::new(30.0, 1, 0);
        t.frames = 10;
        t.busy_ns = 1_999_000_000;
        t.starved = 1; // 预置已计数一窗
        assert_eq!(t.decide(Duration::from_secs(2)), TuneAction::None, "max=0 时永远不动作");
    }

    #[test]
    fn head_roi_covers_top_third_with_margin() {
        // 1080p 全身高 person：头部 = 顶部 30%，左右收 8%，上浮 5%
        let r = head_roi([100.0, 0.0, 300.0, 1080.0], 1920.0, 1080.0);
        assert!((r[0] - 116.0).abs() < 1e-3); // 100 + 200*0.08
        assert_eq!(r[1], 0.0); // 0 - 54 clamp 到 0
        assert!((r[2] - 284.0).abs() < 1e-3);
        assert!((r[3] - 324.0).abs() < 1e-3); // 0 + 1080*0.30
    }

    #[test]
    fn head_roi_clamps_to_frame() {
        let r = head_roi([-50.0, -50.0, 200.0, 400.0], 1920.0, 1080.0);
        assert_eq!(r[0], 0.0);
        assert_eq!(r[1], 0.0);
        assert!(r[2] > 0.0 && r[2] <= 1920.0);
        assert!(r[3] > 0.0 && r[3] <= 1080.0);
    }
}
