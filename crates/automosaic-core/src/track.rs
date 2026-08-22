//! ByteTrack 完整形态跟踪 + 漏检保持 + mask 时序平滑（M3，DESIGN §5.3）。
//!
//! - **Kalman 预测**（匀速模型，位置+尺寸各一维独立的 2×2 滤波）：每帧
//!   predict 推进状态，关联基于**预测框**而非旧框——快速移动的匹配更稳；
//!   速度估计来自滤波状态（取代旧的相邻帧差分）。
//! - **低分框二次关联**（BYTE 核心）：高分检测先与 track 关联，未匹配的
//!   track 再用低分检测（`low_conf..conf` 区间，遮挡/模糊时唯一可见的输出）
//!   二次救援；低分检测不创建新 track（压制误检起轨）。
//! - **OC-SORT 观测中心救援**（OCR + ORU，可选）：漏检期间 KF 按陈旧速度
//!   盲预测，预测框可能跑得比人远——两段 IoU 关联失败后，用**最后观测框**
//!   与剩余高分检测再试一次（OCR），命中则回滚重放滤波器（ORU，观测差分
//!   速度替代盲预测速度），防止"丢得越久、恢复后速度被污染越重"。
//! - 漏检保持：`max_lost` 帧内保留最后 mask，按 KF 速度自适应膨胀补位移条带
//!   （见 [`Track::hold_dilate_px`]）。
//! - mask 时序平滑：上一帧原始 mask 膨胀 3px 与本帧取并集（零成本消闪烁）。

use crate::detect::PersonInstance;

/// ByteTrack 低分检测下限（BYTE 论文默认 ~0.1）：Detector 设为 `low_conf` 后
/// 解码额外返回 [此值, conf) 的检测，供二段关联救援遮挡/模糊目标。
pub const BYTE_LOW_CONF: f32 = 0.1;

fn box_iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let aa = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let ab = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    if aa + ab <= 0.0 { 0.0 } else { inter / (aa + ab - inter) }
}

// --------------------------------------------------------------------------- //
// 匀速 Kalman（每维 [位置, 速度] 独立 2×2，观测位置）
// --------------------------------------------------------------------------- //

/// 单维（位置 p + 速度 v）的匀速 Kalman：
/// 预测 p' = p + v；观测 z 仅含位置。协方差 2×2 含交叉项（速度由位置残差学习）。
#[derive(Debug, Clone, Copy)]
struct KalmanDim {
    p: f32,
    v: f32,
    cov_pp: f32,
    cov_pv: f32,
    cov_vv: f32,
    q_p: f32, // 过程噪声（位置）
    q_v: f32, // 过程噪声（速度）
    r: f32,   // 观测噪声
}

impl KalmanDim {
    fn new(p: f32, scale: f32) -> Self {
        Self {
            p,
            v: 0.0,
            cov_pp: 10.0 * scale * scale,
            cov_vv: 100.0 * scale * scale,
            cov_pv: 0.0,
            q_p: (0.02 * scale) * (0.02 * scale),
            q_v: (0.01 * scale) * (0.01 * scale),
            r: (0.01 * scale) * (0.01 * scale),
        }
    }

    fn predict(&mut self) {
        // F = [[1,1],[0,1]]；P' = F P Fᵀ + Q
        self.p += self.v;
        let (pp, pv, vv) = (self.cov_pp, self.cov_pv, self.cov_vv);
        self.cov_pp = pp + 2.0 * pv + vv + self.q_p;
        self.cov_pv = pv + vv;
        self.cov_vv = vv + self.q_v;
    }

    fn update(&mut self, z: f32) {
        // H = [1,0]；标准标量观测更新
        let s = self.cov_pp + self.r;
        let k_p = self.cov_pp / s;
        let k_v = self.cov_pv / s;
        let innov = z - self.p;
        self.p += k_p * innov;
        self.v += k_v * innov;
        let (pp, pv, vv) = (self.cov_pp, self.cov_pv, self.cov_vv);
        self.cov_pp = (1.0 - k_p) * pp;
        self.cov_pv = (1.0 - k_p) * pv;
        self.cov_vv = vv - k_v * pv;
    }
}

/// 框级 Kalman：cx/cy/w/h 四个独立 [`KalmanDim`]（scale 取框均值尺寸，
/// 噪声随框大小缩放——大框的定位抖动按比例更大）。
#[derive(Debug, Clone)]
struct BoxKalman {
    cx: KalmanDim,
    cy: KalmanDim,
    w: KalmanDim,
    h: KalmanDim,
}

impl BoxKalman {
    fn new(xyxy: [f32; 4]) -> Self {
        let (w, h) = ((xyxy[2] - xyxy[0]).max(1.0), (xyxy[3] - xyxy[1]).max(1.0));
        let scale = (w + h) * 0.5;
        Self {
            cx: KalmanDim::new((xyxy[0] + xyxy[2]) * 0.5, scale),
            cy: KalmanDim::new((xyxy[1] + xyxy[3]) * 0.5, scale),
            w: KalmanDim::new(w, scale),
            h: KalmanDim::new(h, scale),
        }
    }

    /// 推进一帧，返回预测框（xyxy）。
    fn predict(&mut self) -> [f32; 4] {
        for d in [&mut self.cx, &mut self.cy, &mut self.w, &mut self.h] {
            d.predict();
        }
        self.box_of()
    }

    fn update(&mut self, xyxy: [f32; 4]) {
        let (w, h) = ((xyxy[2] - xyxy[0]).max(1.0), (xyxy[3] - xyxy[1]).max(1.0));
        self.cx.update((xyxy[0] + xyxy[2]) * 0.5);
        self.cy.update((xyxy[1] + xyxy[3]) * 0.5);
        self.w.update(w);
        self.h.update(h);
    }

    fn box_of(&self) -> [f32; 4] {
        let (hw, hh) = (self.w.p * 0.5, self.h.p * 0.5);
        [self.cx.p - hw, self.cy.p - hh, self.cx.p + hw, self.cy.p + hh]
    }

    /// OC-SORT 观测中心重更新（ORU，DESIGN §5.3）：丢失 `gap` 帧后重新关联时，
    /// 标准 KF 更新会把 gap 步盲预测的漂移当作观测残差一次性吸收——丢得越久，
    /// 恢复后的速度估计被污染越重（错误速度 → 保持帧外推方向错 → 拖影）。
    /// 改为回滚到最后观测状态 `last_obs`，以 (z − last)/gap 为间隙平均速度
    /// 重放匀速假设（协方差重置为初生不确定性并按 gap 步增长），再对新观测
    /// 做标准更新——滤波器状态由观测序列主导而非盲预测（observation-centric）。
    fn reupdate_after_gap(&mut self, last_obs: [f32; 4], z: [f32; 4], gap: u32) {
        let dims = |b: [f32; 4]| {
            (
                (b[0] + b[2]) * 0.5,
                (b[1] + b[3]) * 0.5,
                (b[2] - b[0]).max(1.0),
                (b[3] - b[1]).max(1.0),
            )
        };
        let (lcx, lcy, lw, lh) = dims(last_obs);
        let (zcx, zcy, zw, zh) = dims(z);
        let gap = gap.max(1) as f32;
        let scale = (lw + lh) * 0.5;
        let with_v = |p: f32, v: f32| {
            let mut d = KalmanDim::new(p, scale);
            d.v = v;
            d
        };
        let mut k = BoxKalman {
            cx: with_v(lcx, (zcx - lcx) / gap),
            cy: with_v(lcy, (zcy - lcy) / gap),
            w: with_v(lw, (zw - lw) / gap),
            h: with_v(lh, (zh - lh) / gap),
        };
        for _ in 0..gap as usize {
            k.predict();
        }
        k.update(z);
        *self = k;
    }

    /// 平移位置状态（GMC 相机运动补偿）：速度项不动——相机平移不应被
    /// 速度滤波吸收（停机后会产生反向漂移，OC-SORT 的观测中心论据）。
    fn translate(&mut self, dx: f32, dy: f32) {
        self.cx.p += dx;
        self.cy.p += dy;
    }
}

/// 一个被跟踪的 person（ID + 最近观测框 + 最近 mask + 丢失计数 + KF）。
pub struct Track {
    pub id: u64,
    /// 最近一次匹配的观测框（漏检期间冻结；关联用 KF 预测框）。
    pub xyxy: [f32; 4],
    pub score: f32,
    /// W×H，1=遮罩 = per-ID EMA（α=0.7）的二值化输出；漏检期间冻结。
    pub mask: Vec<u8>,
    /// 连续未匹配的帧数。
    pub lost: u32,
    /// 丢失遮罩渐隐进度（0=全强度；1=收缩殆尽，DESIGN §6"0.5s 渐隐"）：
    /// 保持期后半段线性上升，track 删除帧遮罩恰好消失，避免硬切。
    pub fade: f32,
    /// 漏检期间累积的相机位移（GMC；保持帧 mask/框按此平移跟随镜头）。
    pub shift: [f32; 2],
    kf: BoxKalman,
    /// EMA 灰度域（0..255），mask 的未二值化来源。
    ema: Vec<u8>,
}

/// per-ID mask 指数滑动平均：ema = α·cur + (1-α)·ema（定点 179/77 ≈ 0.7/0.3）。
/// 二值化输出（≥128）作为 track mask——单帧抖动被时序吸收。
fn ema_blend(ema: &mut Vec<u8>, cur: &[u8], out_bin: &mut Vec<u8>, enabled: bool) {
    if !enabled {
        // 关闭 EMA：mask 直接透传最近观测（保持 0/1 域）
        out_bin.clear();
        out_bin.extend(cur.iter().map(|&v| v.min(1)));
        return;
    }
    if ema.len() != cur.len() {
        ema.clear();
        ema.extend(cur.iter().map(|&v| v.saturating_mul(255)));
    } else {
        for (e, &c) in ema.iter_mut().zip(cur) {
            let cur255 = (c.min(1) as u32) * 255; // mask 域为 0/1，钳制任意输入
            *e = ((cur255 * 179 + (*e as u32) * 77) / 256) as u8;
        }
    }
    out_bin.clear();
    out_bin.extend(ema.iter().map(|&e| (e >= 128) as u8));
}

impl Track {
    /// 框中心速度（像素/帧），来自 Kalman 滤波状态。
    pub fn velocity(&self) -> (f32, f32) {
        (self.kf.cx.v, self.kf.cy.v)
    }

    /// 保持帧的自适应膨胀像素：0.5×|v|×(lost+1)，夹在 [6, 24]。
    /// mask 已按速度外推平移（跟住目标），膨胀只需覆盖速度估计误差
    /// （KF 收敛后 ~20-30%）与检测间隔内的形变——旧值 |v|×(lost+1)
    /// 上限 48px 是纯冻结时代"补位移条带"的语义，外推后叠加会形成
    /// 明显拖影光环（2026-08-20 均衡档残余拖影，隔帧档特有）
    pub fn hold_dilate_px(&self) -> usize {
        let (vx, vy) = self.velocity();
        let d = ((vx.abs() + vy.abs()) * 0.5 * (self.lost as f32 + 1.0)).round() as usize;
        d.clamp(6, 24)
    }
}

#[derive(Debug, Clone)]
pub struct TrackerOptions {
    /// 一段关联（高分检测）IoU 阈值。
    pub iou_thr: f32,
    /// 二段关联（低分救援）IoU 阈值（高于一段，避免误吸收）。
    pub low_iou_thr: f32,
    /// 漏检保持帧数（超过则删除 track）。
    pub max_lost: u32,
    /// per-ID mask EMA（α=0.7）；关闭则 mask 透传最近观测（二值化）。
    pub ema: bool,
    /// OC-SORT 观测中心重更新（ORU）：丢失后重关联时回滚重放，防速度污染
    /// （DESIGN §5.3；关闭则用标准 KF 更新）。
    pub ocru: bool,
}

impl Default for TrackerOptions {
    fn default() -> Self {
        Self { iou_thr: 0.3, low_iou_thr: 0.5, max_lost: 12, ema: true, ocru: true } // 12 帧 ≈ 30fps 下 0.4s
    }
}

/// 框是否贴近画面边缘（离场启发式）：人物"走出画面"的最后观测必然
/// 贴边，此时继续保持遮罩只会留下原地残影（2026-08-20 Linux 实测反馈）；
/// 画面内部的丢失更可能是遮挡，保持"宁可多打"语义。
pub fn near_frame_edge(xyxy: [f32; 4], w: usize, h: usize, margin: f32) -> bool {
    let (wf, hf) = (w as f32, h as f32);
    xyxy[0] < margin || xyxy[1] < margin || xyxy[2] > wf - margin || xyxy[3] > hf - margin
}

/// 丢失遮罩渐隐进度（DESIGN §6 精度 #1 的"0.5s 渐隐"落地）：
/// 保持期**后半段**线性 0→1（前半段全强度——1-2 帧短漏检的补偿优先，
/// "宁可多打不可漏"）；track 删除帧（lost = max_lost）恰好到 1。
/// max_lost < 4 时关闭（保持期太短，渐隐没有意义）。
fn fade_progress(lost: u32, max_lost: u32) -> f32 {
    let start = max_lost / 2;
    if max_lost < 4 || lost <= start {
        0.0
    } else {
        ((lost - start) as f32 / (max_lost - start) as f32).min(1.0)
    }
}

/// ByteTrack 形态跟踪器（原 IouTracker 升级，名称保留以稳定调用方）。
pub struct IouTracker {
    opts: TrackerOptions,
    tracks: Vec<Track>,
    next_id: u64,
}

impl IouTracker {
    pub fn new(opts: TrackerOptions) -> Self {
        Self { opts, tracks: vec![], next_id: 0 }
    }

    /// 用当前帧的检测更新跟踪；`dets` 为**未按高分阈值过滤**的检测
    /// （≥ 低分下限，由 Detector 的 `low_conf` 产出），`high_conf` 为
    /// 一段/二段的切分线（即用户置信度）。返回应当打码的活跃 track
    /// （含漏检保持期内的，`lost <= max_lost`）。
    pub fn update(&mut self, dets: Vec<PersonInstance>, high_conf: f32) -> Vec<&Track> {
        self.update_with_motion(dets, high_conf, [0.0, 0.0])
    }

    /// 带全局运动补偿的更新（DESIGN §5.3 GMC）：`motion` 为本帧相机位移
    /// （相位相关估计），KF 预测框与保持位移据此平移——镜头平移下大位移
    /// 目标仍可关联，漏检保持的冻结 mask 跟随镜头。
    pub fn update_with_motion(
        &mut self,
        dets: Vec<PersonInstance>,
        high_conf: f32,
        motion: [f32; 2],
    ) -> Vec<&Track> {
        // 0) 全部 track 先做 KF 预测（漏检帧也在推进状态——含本调用），
        //    再按相机位移平移（关联与保持均在新帧坐标系）
        let preds: Vec<[f32; 4]> = self
            .tracks
            .iter_mut()
            .map(|t| {
                let _ = t.kf.predict();
                t.kf.translate(motion[0], motion[1]);
                t.kf.box_of()
            })
            .collect();

        let mut dets = dets;
        dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let (high_idx, low_idx): (Vec<usize>, Vec<usize>) = (0..dets.len())
            .partition(|&i| dets[i].score >= high_conf);

        // 关联（贪心：按 stage 顺序，检测已按分数降序；IoU 对 KF 预测框）
        let mut track_taken = vec![false; self.tracks.len()];
        let mut assign: Vec<Option<usize>> = vec![None; dets.len()];
        let mut match_stage = |idxs: &[usize], thr: f32, track_taken: &mut [bool]| {
            for &di in idxs {
                let d = &dets[di];
                let mut best = (None, 0.0f32);
                for (ti, _) in self.tracks.iter().enumerate() {
                    if track_taken[ti] {
                        continue;
                    }
                    let iou = box_iou(&preds[ti], &d.xyxy);
                    if iou > best.1 && iou >= thr {
                        best = (Some(ti), iou);
                    }
                }
                if let Some(ti) = best.0 {
                    track_taken[ti] = true;
                    assign[di] = Some(ti);
                }
            }
        };
        // 1) 一段：高分 × track
        match_stage(&high_idx, self.opts.iou_thr, &mut track_taken);
        // 2) 二段：低分 × 一段未匹配的 track（BYTE 救援；不创建新轨）
        match_stage(&low_idx, self.opts.low_iou_thr, &mut track_taken);
        // 2.5) OCR 观测中心救援（OC-SORT，DESIGN §5.3）：两段后仍未匹配的
        //      track 用**最后观测框**（而非按陈旧速度外推的 KF 预测框——漏检
        //      期间人停住/变向时它会跑得比人远）与剩余高分检测再关联一次。
        //      命中在应用阶段走 ORU 重更新（回滚重放，观测差分速度）。
        if self.opts.ocru {
            for ti in 0..self.tracks.len() {
                if track_taken[ti] {
                    continue;
                }
                let mut best: Option<(usize, f32)> = None;
                for &di in &high_idx {
                    if assign[di].is_some() {
                        continue;
                    }
                    let iou = box_iou(&self.tracks[ti].xyxy, &dets[di].xyxy);
                    if iou >= self.opts.iou_thr && best.map_or(true, |(_, b)| iou > b) {
                        best = Some((di, iou));
                    }
                }
                if let Some((di, _)) = best {
                    track_taken[ti] = true;
                    assign[di] = Some(ti);
                }
            }
        }

        // 3) 应用：匹配 → KF 更新（丢失后重关联走 ORU）；高分未匹配 → 新
        //    track；低分未匹配 → 丢弃
        for (di, d) in dets.into_iter().enumerate() {
            match assign[di] {
                Some(ti) => {
                    let t = &mut self.tracks[ti];
                    if t.lost > 0 && self.opts.ocru {
                        t.kf.reupdate_after_gap(t.xyxy, d.xyxy, t.lost + 1);
                    } else {
                        t.kf.update(d.xyxy);
                    }
                    t.xyxy = d.xyxy;
                    t.score = d.score;
                    let mut bin = std::mem::take(&mut t.mask);
                    ema_blend(&mut t.ema, &d.mask, &mut bin, self.opts.ema);
                    t.mask = bin;
                    t.lost = 0;
                    t.fade = 0.0;
                    t.shift = [0.0, 0.0];
                }
                None => {
                    if d.score >= high_conf {
                        let mut ema = Vec::new();
                        let mut mask = Vec::new();
                        ema_blend(&mut ema, &d.mask, &mut mask, self.opts.ema);
                        self.tracks.push(Track {
                            id: self.next_id,
                            xyxy: d.xyxy,
                            score: d.score,
                            mask,
                            lost: 0,
                            fade: 0.0,
                            shift: [0.0, 0.0],
                            kf: BoxKalman::new(d.xyxy),
                            ema,
                        });
                        self.next_id += 1;
                        track_taken.push(true);
                    }
                }
            }
        }
        // 4) 未匹配 track → 丢失计数；超期删除。
        // 保持位移累积 = 相机位移（GMC）+ **目标自身速度（KF 外推）**——
        // 漏检/隔帧保持的冻结 mask 按速度逐帧累加平移跟随目标，而非原地冻结
        // （人物移走后旧位置的"残影/拖影"即源于纯冻结 + 膨胀，2026-08-20
        // Linux 实测反馈；速度来自滤波状态，静止目标 ≈0 无副作用）
        for (ti, t) in self.tracks.iter_mut().enumerate() {
            if !track_taken[ti] {
                t.lost += 1;
                t.fade = fade_progress(t.lost, self.opts.max_lost);
                let (vx, vy) = t.velocity();
                t.shift[0] += motion[0] + vx;
                t.shift[1] += motion[1] + vy;
            }
        }
        self.tracks.retain(|t| t.lost <= self.opts.max_lost);

        self.tracks.iter().collect()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

// --------------------------------------------------------------------------- //
// mask 时序平滑
// --------------------------------------------------------------------------- //

/// 上一帧【原始】mask 膨胀 3px 后与本帧取并集（消除逐帧闪烁与瞬时漏检）。
/// 注意 prev 必须存"并入历史前"的原始 mask——若存并入后的结果会无限累积
/// （人物轨迹永久保留且逐帧增肥，导致大面积背景被打码；预览单帧不触发，
/// 仅连续处理时暴露）。
pub struct MaskSmoother {
    prev: Option<Vec<u8>>,
    buf: Vec<u8>,
}

impl MaskSmoother {
    pub fn new() -> Self {
        Self { prev: None, buf: vec![] }
    }

    /// 就地平滑 `cur`（W×H）。
    pub fn apply(&mut self, cur: &mut [u8], w: usize, h: usize) {
        let raw = cur.to_vec(); // 本帧原始 mask（未并入历史）
        if let Some(prev) = &self.prev {
            self.buf.clear();
            self.buf.extend_from_slice(prev);
            crate::compose::dilate3(&mut self.buf, w, h);
            for (c, p) in cur.iter_mut().zip(&self.buf) {
                *c |= *p;
            }
        }
        self.prev = Some(raw);
    }
}

impl Default for MaskSmoother {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(x: f32, y: f32, s: f32, score: f32, mask_val: u8) -> PersonInstance {
        PersonInstance {
            score,
            xyxy: [x, y, x + s, y + s],
            mask: vec![mask_val; 8 * 8],
        }
    }

    const HI: f32 = 0.35; // 测试用高分线

    #[test]
    fn tracks_match_by_iou_and_hold_on_miss() {
        let mut tr = IouTracker::new(TrackerOptions { iou_thr: 0.3, low_iou_thr: 0.5, max_lost: 2, ema: true, ..Default::default() });
        // 帧 1：一个实例
        let id0 = {
            let active = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].lost, 0);
            active[0].id
        };
        assert_eq!(tr.len(), 1);
        // 帧 2：位移 10px（IoU 高）→ 同一 track
        {
            let active = tr.update(vec![inst(10.0, 0.0, 100.0, 0.88, 1)], HI);
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].id, id0);
        }
        // 帧 3：漏检 → track 保持（lost=1）
        {
            let active = tr.update(vec![], HI);
            assert_eq!(active.len(), 1, "漏检帧应保持遮罩");
            assert_eq!(active[0].lost, 1);
        }
        // 帧 4-5：持续漏检 → lost=2 仍保持，随后删除
        assert_eq!(tr.update(vec![], HI).len(), 1);
        assert_eq!(tr.update(vec![], HI).len(), 0);
        assert!(tr.is_empty());
    }

    #[test]
    fn velocity_converges_and_hold_dilate_bounded() {
        let mut tr = IouTracker::new(TrackerOptions::default());
        // 匀速右移 30px/帧：KF 速度需 ~3 帧收敛（首帧后 0，二帧 ~半速，三帧到位）
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let id = tr.update(vec![inst(30.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
        tr.update(vec![inst(60.0, 0.0, 100.0, 0.9, 1)], HI);
        {
            let active = tr.update(vec![], HI); // 漏检 lost=1
            let t = active.iter().find(|t| t.id == id).unwrap();
            let (vx, _) = t.velocity();
            assert!((vx - 30.0).abs() < 3.0, "vx 应收敛到 ~30, 得 {vx}");
            // 外推语义：0.5×|v|×(lost+1)≈30 → 上限 24
            assert_eq!(t.hold_dilate_px(), 24, "0.5×|v|×2≈30 被上限夹到 24");
        }
        // 持续漏检 → 限幅 24
        tr.update(vec![], HI);
        tr.update(vec![], HI);
        {
            let active = tr.update(vec![], HI);
            let t = active.iter().find(|t| t.id == id).unwrap();
            assert_eq!(t.hold_dilate_px(), 24, "上限夹紧");
        }
        // 静止人物：|v|≈0 → 下限 6
        let mut tr2 = IouTracker::new(TrackerOptions::default());
        tr2.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        tr2.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let t = tr2.update(vec![], HI);
        assert_eq!(t[0].hold_dilate_px(), 6, "下限夹紧");
    }

    #[test]
    fn new_person_creates_new_track() {
        let mut tr = IouTracker::new(TrackerOptions::default());
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let active = tr.update(
            vec![inst(0.0, 0.0, 100.0, 0.9, 1), inst(500.0, 500.0, 100.0, 0.7, 1)],
            HI,
        );
        assert_eq!(active.len(), 2);
        drop(active);
        assert_eq!(tr.len(), 2);
    }

    #[test]
    fn low_det_rescues_occluded_track() {
        // BYTE 二段救援：track 高分建立 → 下一帧只有低分检测（遮挡场景）
        // → 二段关联成功：不丢轨、不建新轨、mask 用低分观测更新
        let mut tr = IouTracker::new(TrackerOptions::default());
        let id0 = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 5)], HI)[0].id;
        let active = tr.update(vec![inst(5.0, 0.0, 100.0, 0.18, 7)], HI);
        assert_eq!(active.len(), 1, "低分检测应救回 track");
        assert_eq!(active[0].id, id0, "同一 ID（未建新轨）");
        assert_eq!(active[0].lost, 0);
        // mask 为 per-ID EMA 的二值化输出（恒 0/1），不再透传原始观测值
        assert_eq!(active[0].mask[0], 1, "低分观测并入 EMA 后仍应遮盖");
        assert_eq!(tr.len(), 1);
    }

    #[test]
    fn low_det_alone_never_spawns_track() {
        // 孤立低分检测不建轨（防误检起轨）
        let mut tr = IouTracker::new(TrackerOptions::default());
        let active = tr.update(vec![inst(0.0, 0.0, 100.0, 0.15, 1)], HI);
        assert_eq!(active.len(), 0);
        assert!(tr.is_empty());
    }

    #[test]
    fn low_det_far_away_not_absorbed() {
        // 二段阈值（0.5）比一段更严：远处的低分检测不得被吸收
        let mut tr = IouTracker::new(TrackerOptions::default());
        let id0 = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
        // IoU ≈ 0.24：过一段 0.3 都不到，更过不了二段 0.5
        let active = tr.update(vec![inst(70.0, 0.0, 100.0, 0.2, 1)], HI);
        let t = active.iter().find(|t| t.id == id0).unwrap();
        assert_eq!(t.lost, 1, "远处低分检测不应被关联");
        assert_eq!(t.mask[0], 1, "mask 保持旧值");
    }

    #[test]
    fn lost_track_fades_over_second_half_of_hold() {
        // max_lost=8：lost 1..4 全强度（fade=0），5..8 线性 0.25→1.0，随后删除
        let mut tr = IouTracker::new(TrackerOptions { max_lost: 8, ..Default::default() });
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        for expected in [0.0, 0.0, 0.0, 0.0] {
            let a = tr.update(vec![], HI);
            assert!((a[0].fade - expected).abs() < 1e-6, "前半段应全强度");
        }
        for expected in [0.25, 0.5, 0.75, 1.0] {
            let a = tr.update(vec![], HI);
            assert!(
                (a[0].fade - expected).abs() < 1e-6,
                "后半段线性渐隐，期望 {expected} 得 {}",
                a[0].fade
            );
        }
        assert!(tr.update(vec![], HI).is_empty(), "超期删除");
    }

    #[test]
    fn rematched_track_resets_fade() {
        let mut tr = IouTracker::new(TrackerOptions { max_lost: 8, ..Default::default() });
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        for _ in 0..6 {
            tr.update(vec![], HI); // fade 到 0.5
        }
        let a = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        assert_eq!(a[0].lost, 0);
        assert_eq!(a[0].fade, 0.0, "重新匹配应复位渐隐");
    }

    #[test]
    fn short_hold_disables_fade() {
        // max_lost < 4：无渐隐（保持期太短，漏检补偿优先）
        let mut tr = IouTracker::new(TrackerOptions { max_lost: 2, ..Default::default() });
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let a = tr.update(vec![], HI);
        assert_eq!(a[0].fade, 0.0);
    }

    #[test]
    fn gmc_motion_saves_track_on_camera_pan() {
        // 相机平移 80px：100px 宽的静止人物观测整体平移 80px。
        // 无 GMC：预测框（KF 速度未及收敛 ≈0）与观测 IoU≈0.10 < 0.3 → 丢轨建新轨。
        // 有 GMC：预测框平移 80px 后与观测完全重合 → 同轨保持。
        let mut tr = IouTracker::new(TrackerOptions::default());
        let id0 = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI); // 二帧让 KF 稳定
        let a = tr.update_with_motion(vec![inst(80.0, 0.0, 100.0, 0.9, 1)], HI, [80.0, 0.0]);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, id0, "平移后的预测框应与观测关联，保持同轨");
        assert_eq!(a[0].shift, [0.0, 0.0], "匹配后相机位移清零");

        // 无 GMC 对照：同位移 IoU 不足 → 丢轨建新轨（旧轨仍在保持期，active=2）
        let mut tr2 = IouTracker::new(TrackerOptions::default());
        let id1 = tr2.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
        tr2.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let a2 = tr2.update(vec![inst(80.0, 0.0, 100.0, 0.9, 1)], HI);
        assert_eq!(a2.len(), 2, "无 GMC 时大位移应丢旧轨建新轨（旧轨保持期，对照组）");
        assert!(a2.iter().any(|t| t.id != id1), "新轨 ID 不同于旧轨");
    }

    #[test]
    fn near_frame_edge_detection() {
        let (w, h) = (500usize, 860usize);
        assert!(near_frame_edge([0.0, 372.0, 127.0, 859.0], w, h, 12.0), "左缘贴边");
        assert!(near_frame_edge([400.0, 0.0, 480.0, 500.0], w, h, 12.0), "上缘贴边");
        assert!(near_frame_edge([490.0, 100.0, 500.0, 400.0], w, h, 12.0), "右缘贴边");
        assert!(near_frame_edge([100.0, 850.0, 400.0, 860.0], w, h, 12.0), "下缘贴边");
        assert!(!near_frame_edge([100.0, 100.0, 400.0, 700.0], w, h, 12.0), "画面内部不贴边");
        assert!(!near_frame_edge([15.0, 15.0, 485.0, 845.0], w, h, 12.0), "margin 之外不算");
    }

    #[test]
    fn hold_extrapolates_mask_by_velocity() {
        // 匀速右移 20px/帧 3 帧建立 KF 速度 → 漏检保持帧：每帧外推 ~20px
        // （修复"人物移走后马赛克原地残影"：码跟随目标而非冻结，2026-08-20）
        let mut tr = IouTracker::new(TrackerOptions::default());
        for x in [0.0, 20.0, 40.0] {
            tr.update(vec![inst(x, 0.0, 100.0, 0.9, 1)], HI);
        }
        let s1 = {
            let a = tr.update(vec![], HI);
            a[0].shift
        };
        let s2 = {
            let a = tr.update(vec![], HI);
            a[0].shift
        };
        assert!((s1[0] - 20.0).abs() < 4.0, "首个保持帧外推 ~1 帧速度，得 {:?}", s1);
        assert!((s2[0] - 40.0).abs() < 8.0, "两个保持帧累计 ~2 帧速度，得 {:?}", s2);
        assert!(s1[1].abs() < 2.0 && s2[1].abs() < 2.0, "垂直无运动应 ≈0");
    }

    #[test]
    fn gmc_accumulates_shift_for_lost_tracks() {
        // 漏检保持帧：累积相机位移（保持 mask 平移跟随镜头的依据）
        let mut tr = IouTracker::new(TrackerOptions::default());
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        tr.update_with_motion(vec![], HI, [10.0, 4.0]);
        tr.update_with_motion(vec![], HI, [12.0, -2.0]);
        let a = tr.update_with_motion(vec![], HI, [8.0, 2.0]);
        let s = a[0].shift;
        assert!((s[0] - 30.0).abs() < 1e-4 && (s[1] - 4.0).abs() < 1e-4, "得 {s:?}");
    }

    #[test]
    fn kalman_prediction_matches_fast_motion() {
        // 先以 30px/帧匀速 3 帧建立 KF 速度，随后一帧跳变 60px：
        // 旧框 IoU = 40/160 = 0.25（< 0.3，无预测必丢），预测框偏移 30 后
        // 与观测位移仅 30 → IoU ≈ 0.54 ≥ 0.3 → 凭预测保持同轨
        let mut tr = IouTracker::new(TrackerOptions::default());
        let id0 = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
        tr.update(vec![inst(30.0, 0.0, 100.0, 0.9, 1)], HI);
        tr.update(vec![inst(60.0, 0.0, 100.0, 0.9, 1)], HI);
        let active = tr.update(vec![inst(120.0, 0.0, 100.0, 0.9, 1)], HI);
        assert_eq!(active.len(), 1, "不应产生多余 track");
        assert_eq!(active[0].id, id0, "跳变帧应凭 KF 预测保持同轨");
        assert_eq!(tr.len(), 1);
    }

    #[test]
    fn kalman_dim_math() {
        // 静止目标：predict 不动；多次 update 后速度仍 ≈0
        let mut k = KalmanDim::new(10.0, 100.0);
        k.predict();
        k.update(10.0);
        k.predict();
        k.update(10.0);
        assert!(k.p.abs() - 10.0 < 1e-3);
        assert!(k.v.abs() < 0.01, "静止目标速度应保持 0，得 {}", k.v);
        // 匀速目标：每帧 +5，速度收敛到 ~5
        let mut k2 = KalmanDim::new(0.0, 100.0);
        for i in 1..=6 {
            k2.predict();
            k2.update(i as f32 * 5.0);
        }
        assert!((k2.v - 5.0).abs() < 0.5, "速度应收敛到 ~5，得 {}", k2.v);
    }

    #[test]
    fn ocr_rescues_stopped_person_and_oru_purges_stale_velocity() {
        // 人先匀速 +30（3 帧建立 KF 速度）→ 停住并漏检 3 帧（KF 按旧速度把
        // 预测框跑到 +150，两段 IoU 关联必败）→ 人回到原地被检出。
        // OCR：用最后观测框（60）关联成功 → 同轨复活；
        // ORU：观测差分速度 = (60-60)/4 = 0，陈旧速度 30 被清除
        //      ——否则恢复后的保持帧仍按旧速度外推（拖影根源）。
        let run = |ocru: bool| {
            let mut tr = IouTracker::new(TrackerOptions { ocru, ..Default::default() });
            let id0 = tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI)[0].id;
            tr.update(vec![inst(30.0, 0.0, 100.0, 0.9, 1)], HI);
            tr.update(vec![inst(60.0, 0.0, 100.0, 0.9, 1)], HI);
            for _ in 0..3 {
                tr.update(vec![], HI);
            }
            let a = tr.update(vec![inst(60.0, 0.0, 100.0, 0.9, 1)], HI);
            (id0, a.len(), a[0].id, a[0].lost, a[0].velocity().0)
        };
        let (id0, n, id, lost, v) = run(true);
        assert_eq!((n, lost), (1, 0), "OCR 应救回停住的旧轨（不建新轨）");
        assert_eq!(id, id0, "保持同一 ID");
        assert!(v.abs() < 2.0, "ORU 应清除陈旧速度（观测差分=0），得 {v}");
        // 对照（ocru=false）：OCR 关闭 → 旧轨救不回，检出建新轨
        let (_, n2, _, _, _) = run(false);
        assert_eq!(n2, 2, "无 OCR 时旧轨保持 + 新轨并存（速度污染不可见但 ID 断裂）");
    }

    #[test]
    fn ocru_reupdate_recovers_average_velocity_after_gap() {
        // 静止起轨（KF 速度 0）→ 漏检 2 帧期间目标移动到 +40（一段关联凭静态
        // 预测框直接命中）→ ORU 差分速度 = 40/3 ≈ 13.3，恢复后的保持外推按
        // 真实平均速度走
        let mut tr = IouTracker::new(TrackerOptions::default());
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        for _ in 0..2 {
            tr.update(vec![], HI);
        }
        let a = tr.update(vec![inst(40.0, 0.0, 100.0, 0.9, 1)], HI);
        assert_eq!(a.len(), 1);
        let (vx, _) = a[0].velocity();
        assert!(
            (vx - 40.0 / 3.0).abs() < 3.0,
            "ORU 间隙差分速度应 ≈13.3（40/3），得 {vx}"
        );
        let a2 = tr.update(vec![], HI);
        assert!(
            (a2[0].shift[0] - 40.0 / 3.0).abs() < 5.0,
            "恢复后的速度应驱动保持帧外推，得 {:?}",
            a2[0].shift
        );
    }

    #[test]
    fn ocru_not_applied_on_consecutive_match() {
        // 连续匹配（lost=0）走标准更新路径——ORU 仅在丢失后重关联时触发
        let mut tr = IouTracker::new(TrackerOptions::default());
        tr.update(vec![inst(0.0, 0.0, 100.0, 0.9, 1)], HI);
        let a = tr.update(vec![inst(30.0, 0.0, 100.0, 0.9, 1)], HI);
        let (vx, _) = a[0].velocity();
        assert!((vx - 30.0).abs() < 15.0, "连续帧的常规收敛，得 {vx}");
    }

    #[test]
    fn smoother_does_not_accumulate_history() {
        // 回归：三帧各在相距很远的位置打点，第 3 帧 mask 只含
        // C ∪ dilate(B)，不得残留 A（旧实现会 A∪B∪C 无限累积）
        let (w, h) = (64, 64);
        let mut sm = MaskSmoother::new();
        let at = |x, y| {
            let mut m = vec![0u8; w * h];
            m[y * w + x] = 1;
            m
        };
        let mut m1 = at(5, 5);
        sm.apply(&mut m1, w, h);
        let mut m2 = at(30, 30);
        sm.apply(&mut m2, w, h);
        let mut m3 = at(55, 55);
        sm.apply(&mut m3, w, h);
        assert_eq!(m3[55 * w + 55], 1, "当前帧位置保留");
        assert_eq!(m3[30 * w + 30], 1, "上一帧位置以 3px 光环保留");
        assert_eq!(m3[5 * w + 5], 0, "两帧前的位置不得残留（累积 bug 回归）");
    }

    #[test]
    fn smoother_unions_dilated_prev() {
        let mut sm = MaskSmoother::new();
        let (w, h) = (16, 16);
        // 帧 1：单点 mask
        let mut m1 = vec![0u8; w * h];
        m1[8 * w + 8] = 1;
        sm.apply(&mut m1, w, h);
        // 帧 2：空 mask → 但上一帧膨胀后应补上 3×3 邻域
        let mut m2 = vec![0u8; w * h];
        sm.apply(&mut m2, w, h);
        assert_eq!(m2[8 * w + 8], 1, "中心应保留");
        assert_eq!(m2[7 * w + 7], 1, "对角邻域应膨胀出来");
        assert_eq!(m2[6 * w + 6], 0, "3px 之外不应有");
    }
}
