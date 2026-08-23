//! Weighted Boxes Fusion（DESIGN §5.6 管线 A 步骤 2）：多检测器框级融合。
//!
//! 与 NMS"只留一个"不同，WBF 把 IoU 重叠的假设**加权平均**成一个框、分数取
//! 加权置信——两路检测器都看见的目标分数被确认（≈最高分），单路看见的目标
//! 分数被稀释（召回优先：低分保留，由 SAM2 精修裁决 mask 质量）。
//! 算法对齐 ZFTurbo/weighted-boxes-fusion 的单类简化版（~百行自实现，MIT 思路）。

/// 单个检测假设（原始分辨率 xyxy）。
#[derive(Debug, Clone, Copy)]
pub struct WbfBox {
    pub xyxy: [f32; 4],
    pub score: f32,
    /// 来源标记（调用方自定义，如模型索引；透传到融合结果）。
    pub src: u32,
}

/// WBF 融合结果：融合框 + 融合分数 + 参与融合的模型数（确认度）。
#[derive(Debug, Clone, Copy)]
pub struct FusedBox {
    pub xyxy: [f32; 4],
    pub score: f32,
    /// 有几路检测器的框落进了该簇（= n_lists 时为全确认）。
    pub votes: u32,
    /// 簇内最高原始分数（供召回优先的低分保留判断）。
    pub best_src_score: f32,
    /// 簇内分数最高的来源假设（mask 候选优先取它）。
    pub best_src: u32,
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let aa = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let ab = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    if aa + ab <= 0.0 { 0.0 } else { inter / (aa + ab - inter) }
}

/// 加权框融合。
///
/// - `lists`：各检测器的假设列表；`weights`：各检测器权重（空 = 全 1）。
/// - `iou_thr`：簇合并阈值（WBF 论文/库常用 0.55）。
/// - `score_norm = Σ全部权重`：只被一路发现的目标分数被按比例稀释，
///   `votes` 供上层区分"全确认"与"单路假设"。
/// - 输出按融合分数降序。
pub fn fuse(
    lists: &[Vec<WbfBox>],
    weights: &[f32],
    iou_thr: f32,
) -> Vec<FusedBox> {
    let w: Vec<f32> = if weights.is_empty() {
        vec![1.0; lists.len()]
    } else {
        weights.to_vec()
    };
    let w_total: f32 = w.iter().sum();

    // 展开成 (list_idx, box) 并按分数降序（高分先入簇，簇代表更稳）
    let mut flat: Vec<(usize, WbfBox)> = lists
        .iter()
        .enumerate()
        .flat_map(|(i, l)| l.iter().map(move |b| (i, *b)))
        .collect();
    flat.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));

    // 贪心聚类：每簇记录各 list 的成员（同 list 内重合假设按 NMS 语义丢弃）
    struct Cluster {
        members: Vec<(usize, WbfBox)>,
        rep: [f32; 4],
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    'next: for (li, b) in flat {
        for c in &mut clusters {
            if iou(&c.rep, &b.xyxy) < iou_thr {
                continue;
            }
            // 与簇重合：同 list 已有成员 → 冗余假设丢弃（NMS 语义）；
            // 否则并入并实时刷新簇代表（加权均值）
            if c.members.iter().all(|(l, _)| *l != li) {
                c.members.push((li, b));
                let tw: f32 = c.members.iter().map(|(l, _)| w[*l]).sum();
                for k in 0..4 {
                    c.rep[k] = c.members.iter().map(|(l, m)| w[*l] * m.xyxy[k]).sum::<f32>() / tw;
                }
            }
            continue 'next;
        }
        clusters.push(Cluster { members: vec![(li, b)], rep: b.xyxy });
    }

    let mut out: Vec<FusedBox> = clusters
        .into_iter()
        .map(|c| {
            let tw: f32 = c.members.iter().map(|(l, _)| w[*l]).sum();
            let score = c.members.iter().map(|(l, m)| w[*l] * m.score).sum::<f32>() / w_total;
            let (best, best_src, votes) = c
                .members
                .iter()
                .fold((0.0f32, 0u32, 0u32), |(bs, bsrc, v), (_, m)| {
                    (bs.max(m.score), if m.score >= bs && m.score > 0.0 { m.src } else { bsrc }, v + 1)
                });
            // 融合框 = 加权均值
            let mut xyxy = [0f32; 4];
            for k in 0..4 {
                xyxy[k] = c.members.iter().map(|(l, m)| w[*l] * m.xyxy[k]).sum::<f32>() / tw;
            }
            FusedBox { xyxy, score, votes, best_src_score: best, best_src }
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f32, y: f32, s: f32, src: u32) -> WbfBox {
        WbfBox { xyxy: [x, y, x + 100.0, y + 100.0], score: s, src }
    }

    #[test]
    fn two_models_confirming_each_other_gets_full_score() {
        let a = vec![b(0.0, 0.0, 0.9, 0)];
        let c = vec![b(5.0, 0.0, 0.8, 1)]; // IoU≈0.68 > 0.55
        let out = fuse(&[a, c], &[], 0.55);
        assert_eq!(out.len(), 1);
        assert!((out[0].score - 0.85).abs() < 1e-4, "两路均值 {}", out[0].score);
        assert_eq!(out[0].votes, 2);
        // 融合框 = 两框均值
        assert!((out[0].xyxy[0] - 2.5).abs() < 1e-3);
        assert!((out[0].best_src_score - 0.9).abs() < 1e-4);
    }

    #[test]
    fn single_model_detection_score_damped() {
        let a = vec![b(0.0, 0.0, 0.9, 0)];
        let empty: Vec<WbfBox> = vec![];
        let out = fuse(&[a, empty], &[], 0.55);
        assert_eq!(out.len(), 1);
        assert!((out[0].score - 0.45).abs() < 1e-4, "单路 0.9 → 0.9×(1/2)");
        assert_eq!(out[0].votes, 1);
        assert!((out[0].best_src_score - 0.9).abs() < 1e-4, "原始分数保留在 best_src_score");
    }

    #[test]
    fn disjoint_boxes_stay_separate() {
        let a = vec![b(0.0, 0.0, 0.9, 0), b(300.0, 0.0, 0.7, 0)];
        let out = fuse(&[a], &[], 0.55);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].votes, 1);
        assert!(out[0].score > out[1].score, "按融合分数降序");
    }

    #[test]
    fn weights_bias_toward_trusted_model() {
        let a = vec![b(0.0, 0.0, 0.6, 0)];
        let c = vec![b(4.0, 0.0, 0.8, 1)];
        // YOLO 权重 2、GD 权重 1：分数 = (2×0.6 + 1×0.8)/3
        let out = fuse(&[a, c], &[2.0, 1.0], 0.55);
        assert_eq!(out.len(), 1);
        assert!((out[0].score - (2.0 * 0.6 + 0.8) / 3.0).abs() < 1e-4);
        // 融合框偏 YOLO 框：x = (2×0 + 1×4)/3
        assert!((out[0].xyxy[0] - 4.0 / 3.0).abs() < 1e-3);
    }

    #[test]
    fn same_model_duplicates_deduped_like_nms() {
        // 同一 list 内两个重合假设：只有高分进簇
        let a = vec![b(0.0, 0.0, 0.9, 0), b(10.0, 0.0, 0.5, 0)];
        let out = fuse(&[a], &[], 0.55);
        assert_eq!(out.len(), 1, "同源冗余去重（IoU≈0.47<0.55? 0.45 重合）");
        // IoU(0..100, 10..110) = 90×100/(2×10000-9000) = 0.818 → 合并簇但第二成员被拒
        assert!((out[0].score - 0.9).abs() < 1e-4);
        assert_eq!(out[0].votes, 1);
    }

    #[test]
    fn empty_inputs_yield_empty() {
        assert!(fuse(&[], &[], 0.55).is_empty());
        assert!(fuse(&[vec![], vec![]], &[], 0.55).is_empty());
    }
}
