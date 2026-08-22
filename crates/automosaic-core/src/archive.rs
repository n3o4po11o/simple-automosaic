//! 极限·档案级分析管线（DESIGN §5.6 管线 A，M5）：accuracy-first 两阶段的分析段。
//!
//! 每帧流程（0.1-0.5fps 预期，一切以精度为准）：
//! 1. 双检测器并行：YOLO26x-seg @1536（框+粗 mask）+ Grounding DINO "person."
//! 2. 框级 WBF 融合（召回优先：votes 或低分保留）
//! 3. SAM2.1 逐框 box-prompting 精修（encoder 每帧一次，decoder 批量）；
//!    SAM 置信不足时回退 YOLO proto mask / 框+margin
//! 4. RetinaFace 滑窗人脸 + 多尺度：宿主 person 框内的脸并入该 person masklet
//!    （landmark 外扩）；孤立小脸补框喂 SAM2 精修为独立实例
//! 5. masklet 关联（BoT-SORT 语义：IoU + OSNet 外观嵌入，greedy）——
//!    ID 供复核 UI 按 masklet 编辑；**不做漏检填补**（档案级不编造观测，
//!    SAM2 传播仅作一致性校验的设计决策）
//!
//! 产出 Vec<ArchiveInstance>（id/score/框/mask）由调用方落盘 maskstore
//!（.inst 实例层 + .mask 合并层）。

use crate::detect::{filter_implausible_faces, gate_faces, Detector, FaceBox};
use crate::gdino::GroundingDino;
use crate::retinaface::RetinaFace;
use crate::reid::{cosine, ReId};
use crate::sam2::Sam2;
use crate::wbf::{fuse, WbfBox};

/// SAM 置信低于此值的精修回退 YOLO 原生 mask（原型图解码）或框+margin。
const SAM_IOU_FALLBACK: f32 = 0.5;
/// WBF 融合 IoU 阈值（论文/库惯例 0.55）。
const WBF_IOU: f32 = 0.55;
/// 融合后保留阈值：全确认（votes=2）恒保留；单路假设 ≥ 此值保留（召回优先）。
const FUSED_KEEP_SOLO: f32 = 0.10;
/// 与 YOLO 检测配对取回 proto mask 的框 IoU 阈值。
const YOLO_MATCH_IOU: f32 = 0.55;
/// 人脸并入 person 的宿主判定（脸中心落在 person 框内）。
const FACE_HOST_MARGIN: f32 = 0.08;
/// masklet 丢失容忍帧数（超过即注销 ID；不做漏检 mask 编造）。
const MASKLET_MAX_MISS: u32 = 8;
/// 关联代价中外观相似度权重（IoU 权重 = 1 - 此值）。
const APPEARANCE_W: f32 = 0.4;
/// 关联接受的最低综合得分。
const LINK_THRESH: f32 = 0.30;

/// 实例类别（复核 UI 区分 person masklet 与孤立人脸）。
pub const KIND_PERSON: u8 = 0;
pub const KIND_FACE: u8 = 1;

/// 分析产出的单个实例（一帧内的一个 masklet 观测）。
#[derive(Clone)]
pub struct ArchiveInstance {
    /// masklet id（跨帧一致；复核 UI 的编辑单元）。
    pub id: u64,
    /// WBF 融合分数（person）/ RetinaFace 分数（face）。
    pub score: f32,
    pub xyxy: [f32; 4],
    /// W×H 二值 mask（1=遮罩）。
    pub mask: Vec<u8>,
    pub kind: u8,
}

/// 分析管线参数（预设展开 + CLI 覆写）。
#[derive(Debug, Clone)]
pub struct ArchiveOptions {
    pub conf: f32,
    pub gd_conf: f32,
    pub face_conf: f32,
    pub face_expand: u32,
    pub face_roi_sliding: bool,
    pub tta: bool,
    pub sam_iou_min: f32,
}

impl Default for ArchiveOptions {
    fn default() -> Self {
        Self {
            conf: 0.25,
            gd_conf: 0.35,
            face_conf: 0.6,
            face_expand: 12,
            face_roi_sliding: true,
            tta: true,
            sam_iou_min: SAM_IOU_FALLBACK,
        }
    }
}

/// 模型路径集合（缺一不可启动分析；preset 展开或 CLI 显式给出）。
#[derive(Debug, Clone, Default)]
pub struct ArchiveModelPaths {
    pub yolo: std::path::PathBuf,
    pub gd: std::path::PathBuf,
    pub sam_encoder: std::path::PathBuf,
    pub sam_decoder: std::path::PathBuf,
    pub retina: std::path::PathBuf,
    /// 可选：缺失时纯 IoU 关联。
    pub reid: Option<std::path::PathBuf>,
}

struct Masklet {
    id: u64,
    last_box: [f32; 4],
    last_emb: Option<[f32; 512]>,
    missed: u32,
}

/// 档案级分析器（有状态：masklet 关联记忆跨帧）。
pub struct ArchiveAnalyzer {
    det: Detector,
    gd: GroundingDino,
    sam: Sam2,
    face: RetinaFace,
    reid: Option<ReId>,
    opts: ArchiveOptions,
    masklets: Vec<Masklet>,
    next_id: u64,
    /// 复用缓冲（框级 rect mask）。
    w: usize,
    h: usize,
}

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

fn box_area_overlap(box_: &[f32; 4], mask: &[u8], w: usize) -> f32 {
    // mask 与框的交集占框面积比（host 判定用；下采样 4px 提速）
    let h = mask.len() / w;
    let x1 = box_[0].max(0.0) as usize;
    let y1 = box_[1].max(0.0) as usize;
    let x2 = (box_[2].ceil() as usize).min(w).max(x1 + 1);
    let y2 = (box_[3].ceil() as usize).min(h).max(y1 + 1);
    let mut hit = 0usize;
    let mut total = 0usize;
    for y in (y1..y2).step_by(4) {
        for x in (x1..x2).step_by(4) {
            total += 1;
            if mask[y * w + x] == 1 {
                hit += 1;
            }
        }
    }
    if total == 0 { 0.0 } else { hit as f32 / total as f32 }
}

impl ArchiveAnalyzer {
    /// 加载全部模型（Archive 档启动耗时主体：~1.2GB 权重 + CoreML 首次编译）。
    pub fn new(paths: &ArchiveModelPaths, opts: ArchiveOptions, device: &str, w: usize, h: usize) -> Result<Self, String> {
        Self::new_with_progress(paths, opts, device, w, h, |_| {})
    }

    /// 同 [`Self::new`]，但每件模型加载前回调一次（FFI 逐件发日志——
    /// 五件套 + CoreML 首编译可达数分钟，无反馈会被当作卡死）。
    pub fn new_with_progress(
        paths: &ArchiveModelPaths,
        opts: ArchiveOptions,
        device: &str,
        w: usize,
        h: usize,
        mut on_stage: impl FnMut(&str),
    ) -> Result<Self, String> {
        on_stage("YOLO26x-seg@1536（主检）");
        let mut det = Detector::load(&paths.yolo, device, opts.conf).map_err(|e| format!("YOLO@1536: {e}"))?;
        det.low_conf = Some(0.05); // 召回优先：低分假设交给 WBF/SAM 裁决
        det.tta = opts.tta;
        on_stage("Grounding DINO（开放词汇第二路）");
        let gd = GroundingDino::load(&paths.gd, device, opts.gd_conf).map_err(|e| format!("Grounding DINO: {e}"))?;
        on_stage("SAM2.1（mask 精修 encoder+decoder）");
        let sam = Sam2::load(&paths.sam_encoder, &paths.sam_decoder, device).map_err(|e| format!("SAM2.1: {e}"))?;
        on_stage("RetinaFace（滑窗人脸）");
        let mut face = RetinaFace::load(&paths.retina, device, opts.face_conf).map_err(|e| format!("RetinaFace: {e}"))?;
        face.sliding = opts.face_roi_sliding;
        let reid = match &paths.reid {
            Some(p) => {
                on_stage("OSNet ReID（外观关联）");
                Some(ReId::load(p, device).map_err(|e| format!("OSNet ReID: {e}"))?)
            }
            None => None,
        };
        Ok(Self { det, gd, sam, face, reid, opts, masklets: Vec::new(), next_id: 1, w, h })
    }

    /// 当前活跃 masklet 数（进度/UI 信息）。
    pub fn active_masklets(&self) -> usize {
        self.masklets.len()
    }

    /// 分析一帧：返回带 masklet id 的实例列表（person + 孤立人脸）。
    pub fn analyze_frame(&mut self, nv12: &[u8]) -> Result<Vec<ArchiveInstance>, String> {
        let (w, h) = (self.w, self.h);
        let opts = self.opts.clone();

        // ---- 1) 双检测器 ----
        let yolo = self.det.detect_person_instances(nv12, w, h).map_err(|e| e.to_string())?;
        let gd = self.gd.detect_persons(nv12, w, h).map_err(|e| e.to_string())?;

        // ---- 2) WBF 融合 ----
        let yolo_list: Vec<WbfBox> = yolo
            .iter()
            .map(|p| WbfBox { xyxy: p.xyxy, score: p.score, src: 0 })
            .collect();
        let gd_list: Vec<WbfBox> = gd
            .iter()
            .map(|g| WbfBox { xyxy: g.xyxy, score: g.score, src: 1 })
            .collect();
        let fused: Vec<_> = fuse(&[yolo_list, gd_list], &[1.0, 1.0], WBF_IOU)
            .into_iter()
            .filter(|f| f.votes >= 2 || f.score >= FUSED_KEEP_SOLO || f.best_src_score >= self.opts.conf * 0.6)
            .collect();

        // ---- 3) SAM 精修（encoder 一次 + decoder 批量）----
        self.sam.set_frame(nv12, w, h).map_err(|e| e.to_string())?;
        let boxes: Vec<[f32; 4]> = fused.iter().map(|f| f.xyxy).collect();
        let sam_results = self
            .sam
            .refine_boxes(&boxes, w, h)
            .map_err(|e| e.to_string())?;

        let mut persons: Vec<(f32, [f32; 4], Vec<u8>, bool)> = Vec::new(); // (score, box, mask, sam_ok)
        for (f, (sam_mask, sam_iou)) in fused.iter().zip(sam_results) {
            if sam_iou >= opts.sam_iou_min {
                persons.push((f.score, f.xyxy, sam_mask, true));
                continue;
            }
            // 回退链：YOLO proto mask（框配对）→ 框+margin
            let yolo_match = yolo
                .iter()
                .filter(|p| box_iou(&p.xyxy, &f.xyxy) > YOLO_MATCH_IOU)
                .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
            match yolo_match {
                Some(p) if box_area_overlap(&f.xyxy, &p.mask, w) > 0.3 => {
                    persons.push((f.score, f.xyxy, p.mask.clone(), false));
                }
                _ => {
                    let m = rect_mask(f.xyxy, w, h);
                    persons.push((f.score, f.xyxy, m, false));
                }
            }
        }

        // ---- 4) 滑窗人脸 ----
        let person_boxes: Vec<[f32; 4]> = persons.iter().map(|(_, b, _, _)| *b).collect();
        let faces = self.face.detect_faces(nv12, w, h).map_err(|e| e.to_string())?;
        let faces = filter_implausible_faces(faces, &person_boxes);
        let faces = gate_faces(faces, &person_boxes, 0.75);
        let mut orphan_faces: Vec<FaceBox> = Vec::new();
        for f in &faces {
            let (cx, cy) = ((f.xyxy[0] + f.xyxy[2]) * 0.5, (f.xyxy[1] + f.xyxy[3]) * 0.5);
            let host = persons.iter_mut().find(|(_, b, m, _)| {
                let (ex, ey) = ((b[2] - b[0]) * FACE_HOST_MARGIN, (b[3] - b[1]) * FACE_HOST_MARGIN);
                cx >= b[0] - ex && cx <= b[2] + ex && cy >= b[1] - ey && cy <= b[3] + ey && box_area_overlap(b, m, w) > 0.1
            });
            match host {
                // 人脸归并到所属 person masklet：外扩后并入 mask
                Some((_, _, m, _)) => {
                    let (ex, ey) = crate::detect::face_expand_xy(f, opts.face_expand as usize, true);
                    let (x1, y1) = ((f.xyxy[0] as usize).saturating_sub(ex), (f.xyxy[1] as usize).saturating_sub(ey));
                    let (x2, y2) = ((f.xyxy[2] as usize + 1 + ex).min(w), (f.xyxy[3] as usize + 1 + ey).min(h));
                    for y in y1..y2 {
                        for x in x1..x2 {
                            m[y * w + x] = 1;
                        }
                    }
                }
                // 孤立人脸：独立实例（SAM 精修，设计 §5.6 "孤立小人脸补框喂 SAM2"）
                None => orphan_faces.push(f.clone()),
            }
        }
        let mut instances: Vec<ArchiveInstance> = Vec::new();
        // ---- 5) masklet 关联（person）----
        let person_refs: Vec<(f32, [f32; 4], Vec<u8>)> = persons
            .into_iter()
            .map(|(s, b, m, sam_ok)| (if sam_ok { s } else { s * 0.9 }, b, m))
            .collect();
        let ids = self.link(nv12, &person_refs, KIND_PERSON);
        for ((score, xyxy, mask), id) in person_refs.into_iter().zip(ids) {
            instances.push(ArchiveInstance { id, score, xyxy, mask, kind: KIND_PERSON });
        }
        // 孤立人脸：SAM 精修后关联（人脸 masklet）
        if !orphan_faces.is_empty() {
            let fboxes: Vec<[f32; 4]> = orphan_faces.iter().map(|f| f.xyxy).collect();
            let refined = self
                .sam
                .refine_boxes(&fboxes, w, h)
                .map_err(|e| e.to_string())?;
            let face_refs: Vec<(f32, [f32; 4], Vec<u8>)> = orphan_faces
                .iter()
                .zip(refined)
                .map(|(f, (m, iou))| {
                    if iou >= opts.sam_iou_min {
                        (f.score, f.xyxy, m)
                    } else {
                        // 回退：外扩矩形
                        let (ex, ey) = crate::detect::face_expand_xy(f, opts.face_expand as usize, true);
                        let mut r = vec![0u8; w * h];
                        let (x1, y1) = ((f.xyxy[0] as usize).saturating_sub(ex), (f.xyxy[1] as usize).saturating_sub(ey));
                        let (x2, y2) = ((f.xyxy[2] as usize + 1 + ex).min(w), (f.xyxy[3] as usize + 1 + ey).min(h));
                        for y in y1..y2 {
                            for x in x1..x2 {
                                r[y * w + x] = 1;
                            }
                        }
                        (f.score, f.xyxy, r)
                    }
                })
                .collect();
            let face_ids = self.link(nv12, &face_refs, KIND_FACE);
            for ((score, xyxy, mask), id) in face_refs.into_iter().zip(face_ids) {
                instances.push(ArchiveInstance { id, score, xyxy, mask, kind: KIND_FACE });
            }
        }
        Ok(instances)
    }

    /// BoT-SORT 语义关联：IoU + 外观嵌入 greedy 匹配，未匹配起新 ID。
    /// 返回与 refs 等长的 id 列表；同时推进 masklets 状态（missed/注销）。
    fn link(&mut self, nv12: &[u8], refs: &[(f32, [f32; 4], Vec<u8>)], kind: u8) -> Vec<u64> {
        let (w, h) = (self.w, self.h);
        // 新观测外观嵌入
        let embs: Vec<Option<[f32; 512]>> = match &mut self.reid {
            Some(reid) if kind == KIND_PERSON => refs
                .iter()
                .map(|(_, b, _)| reid.embed(nv12, w, h, *b).ok())
                .collect(),
            _ => vec![None; refs.len()],
        };
        // 综合得分矩阵 greedy：score = (1-w)·IoU + w·max(0, cos)
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new(); // (score, ref_i, masklet_i)
        for (i, (_, b, m)) in refs.iter().enumerate() {
            let _ = m;
            for (j, mk) in self.masklets.iter().enumerate() {
                if mk.missed > MASKLET_MAX_MISS {
                    continue;
                }
                let iou = box_iou(b, &mk.last_box);
                let app = match (&embs[i], &mk.last_emb) {
                    (Some(a), Some(b_)) => cosine(a, b_),
                    _ => 0.0,
                };
                let score = (1.0 - APPEARANCE_W) * iou + APPEARANCE_W * app.max(0.0);
                if score > LINK_THRESH {
                    pairs.push((score, i, j));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut ref_id = vec![u64::MAX; refs.len()];
        let mut taken: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for &(score, i, j) in &pairs {
            if ref_id[i] != u64::MAX || !taken.insert(j) {
                continue;
            }
            let _ = score;
            ref_id[i] = self.masklets[j].id;
        }
        // 更新 masklets：命中的刷新观测；未命中的 missed+1；新观测起新 ID
        let mut refresh: Vec<(usize, Option<[f32; 512]>)> = Vec::new();
        for (i, (_, b, _)) in refs.iter().enumerate() {
            if ref_id[i] != u64::MAX {
                let j = self
                    .masklets
                    .iter()
                    .position(|mk| mk.id == ref_id[i])
                    .expect("关联 id 必存在");
                refresh.push((j, embs[i]));
                let mk = &mut self.masklets[j];
                mk.last_box = *b;
                mk.missed = 0;
            } else {
                let id = self.next_id;
                self.next_id += 1;
                ref_id[i] = id;
                self.masklets.push(Masklet { id, last_box: *b, last_emb: embs[i], missed: 0 });
            }
        }
        for (j, emb) in refresh {
            if let Some(e) = emb {
                self.masklets[j].last_emb = Some(e);
            }
        }
        // 未观测到的 masklet missed+1，超限注销
        let observed: std::collections::HashSet<u64> = ref_id.iter().copied().collect();
        for mk in &mut self.masklets {
            if !observed.contains(&mk.id) {
                mk.missed += 1;
            }
        }
        self.masklets.retain(|mk| mk.missed <= MASKLET_MAX_MISS);
        ref_id
    }
}

/// 框 + 5% margin 的实心矩形 mask（回退链末级）。
fn rect_mask(xyxy: [f32; 4], w: usize, h: usize) -> Vec<u8> {
    let margin = 0.05;
    let (bw, bh) = ((xyxy[2] - xyxy[0]) * margin, (xyxy[3] - xyxy[1]) * margin);
    let x1 = (xyxy[0] - bw).max(0.0) as usize;
    let y1 = (xyxy[1] - bh).max(0.0) as usize;
    let x2 = ((xyxy[2] + bw).ceil() as usize).min(w).max(x1 + 1);
    let y2 = ((xyxy[3] + bh).ceil() as usize).min(h).max(y1 + 1);
    let mut mask = vec![0u8; w * h];
    for y in y1..y2 {
        mask[y * w + x1..y * w + x2].fill(1);
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_mask_inflates() {
        let m = rect_mask([100.0, 100.0, 200.0, 200.0], 640, 480);
        assert_eq!(m[100 * 640 + 100], 1);
        assert_eq!(m[95 * 640 + 95], 1);
        assert_eq!(m[90 * 640 + 90], 0);
    }

    #[test]
    fn box_area_overlap_measures_containment() {
        let (w, h) = (64, 64);
        let mut m = vec![0u8; w * h];
        for y in 0..32 {
            for x in 0..32 {
                m[y * w + x] = 1;
            }
        }
        assert!(box_area_overlap(&[0.0, 0.0, 32.0, 32.0], &m, w) > 0.9);
        assert!(box_area_overlap(&[32.0, 32.0, 64.0, 64.0], &m, w) < 0.1);
        let _ = h;
    }

    /// 端到端回归锚点（真实模型，tiny SAM 提速）：clip5s 首帧 →
    /// ≥1 个 person masklet，mask 与 GD/YOLO person 框一致，ID 稳定跨帧。
    #[test]
    fn analyze_real_frame_produces_masklet() {
        let paths = ArchiveModelPaths {
            yolo: crate::models::resolve_model("yolo26x-seg-1536.onnx"),
            gd: crate::models::resolve_model("grounding-dino-tiny.onnx"),
            sam_encoder: crate::models::resolve_model("sam2.1-tiny-encoder.onnx"),
            sam_decoder: crate::models::resolve_model("sam2.1-tiny-decoder.onnx"),
            retina: crate::models::resolve_model("retinaface-r34.onnx"),
            reid: Some(crate::models::resolve_model("osnet-x025-msmt17.onnx")),
        };
        let video = std::path::Path::new("../../tests/clip5s.mp4");
        let all_present = paths.yolo.exists()
            && paths.gd.exists()
            && paths.sam_encoder.exists()
            && paths.sam_decoder.exists()
            && paths.retina.exists()
            && video.exists();
        if !all_present {
            eprintln!("skip: M5 模型不全（CI 环境）");
            return;
        }
        let meta = crate::media::probe(video).expect("probe");
        let (w, h) = (meta.width as usize, meta.height as usize);
        let mut az = ArchiveAnalyzer::new(&paths, ArchiveOptions::default(), "cpu", w, h).expect("加载");
        let nv12 = crate::media::decode_frame_at(video, 0.0, &meta).expect("抽帧");
        let inst = az.analyze_frame(&nv12).expect("分析");
        assert!(!inst.is_empty(), "clip5s 首帧应检出 person");
        let p = inst.iter().find(|i| i.kind == KIND_PERSON).expect("person 实例");
        // GD/YOLO 共识框 (455,237)-(848,1039)：实例框应覆盖其大部
        let gt = [455.0f32, 237.0, 848.0, 1039.0];
        assert!(box_iou(&p.xyxy, &gt) > 0.4, "实例框 {:?} vs GT {gt:?}", p.xyxy);
        assert!(p.id >= 1);
        let area = p.mask.iter().map(|&v| v as usize).sum::<usize>();
        assert!(area > 10_000, "SAM 精修 mask 面积 {area}");
        // 第二帧：ID 稳定（同 person 持续）
        let nv12_2 = crate::media::decode_frame_at(video, 1.0 / 15.0, &meta).expect("抽帧 2");
        let inst2 = az.analyze_frame(&nv12_2).expect("分析 2");
        assert!(
            inst2.iter().any(|i| i.kind == KIND_PERSON && i.id == p.id),
            "第二帧应保持 masklet id {}（得 {:?}）",
            p.id,
            inst2.iter().map(|i| (i.id, i.kind)).collect::<Vec<_>>()
        );
    }
}
