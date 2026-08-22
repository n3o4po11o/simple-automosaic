//! NV12 平面上的遮罩合成（DESIGN §6 性能清单的 CPU 包围盒版，M1 为标量实现）。
//!
//! 只处理 mask 包围盒区域（对应 Python 版 masking.py 的优化），
//! 三种样式：Mosaic（像素格马赛克）/ Blur（可分离 box 模糊近似高斯）/ Solid（纯黑）。
//! Y 平面全分辨率处理，UV 平面按 4:2:0 半分辨率处理。

/// 遮罩样式。
#[derive(Debug, Clone)]
pub enum MaskStyle {
    /// 像素格马赛克，cell 为格边长（像素）。
    Mosaic { cell: usize },
    /// 模糊，radius 为 box 模糊半径（水平+垂直两趟近似高斯）。
    Blur { radius: usize },
    /// 纯黑。
    Solid,
}

// --------------------------------------------------------------------------- //
// 合成后端抽象（DESIGN §4.3 ComposeBackend）：CPU 包围盒版为默认；GPU compute
// 版（可分离高斯/块均值 shader）预留此扩展位——帧数据当前在系统内存（NV12
// 管道），GPU 版需与进程内零拷贝路线（rusty_ffmpeg hwframes，§0.5-D v2）
// 联动才不付上传/回读往返。
// --------------------------------------------------------------------------- //

/// NV12 帧遮罩合成后端。
pub trait ComposeBackend: Send {
    /// 就地应用 mask（W×H，1=遮罩区域）。
    fn apply(&mut self, nv12: &mut [u8], w: usize, h: usize, mask: &[u8], style: &MaskStyle);
}

/// CPU 实现（默认）：包围盒裁剪 + LLVM 自动向量化的标量核（见 [`apply`]）。
#[derive(Debug, Default)]
pub struct ComposeCpu;

impl ComposeBackend for ComposeCpu {
    fn apply(&mut self, nv12: &mut [u8], w: usize, h: usize, mask: &[u8], style: &MaskStyle) {
        apply(nv12, w, h, mask, style)
    }
}

/// 在 NV12 帧上应用 mask（W×H，1=遮罩区域）。
pub fn apply(nv12: &mut [u8], w: usize, h: usize, mask: &[u8], style: &MaskStyle) {
    debug_assert_eq!(mask.len(), w * h);
    let pad = match style {
        MaskStyle::Mosaic { cell } => *cell,
        MaskStyle::Blur { radius } => radius * 3,
        MaskStyle::Solid => 0,
    };
    let Some((x1, y1, x2, y2)) = mask_bbox(mask, w, h, pad) else {
        return; // 空 mask，帧原样返回
    };
    match style {
        MaskStyle::Solid => solid(nv12, w, mask, x1, y1, x2, y2),
        MaskStyle::Mosaic { cell } => pixelate(nv12, w, h, mask, x1, y1, x2, y2, (*cell).max(2)),
        MaskStyle::Blur { radius } => {
            let r = (*radius).clamp(1, 64);
            blur(nv12, w, h, mask, x1, y1, x2, y2, r);
        }
    }
}

/// mask 非零像素的包围盒，四周扩 pad 并裁剪到帧内；空 mask 返回 None。
fn mask_bbox(mask: &[u8], w: usize, h: usize, pad: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x1, mut y1, mut x2, mut y2) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (i, &m) in mask.iter().enumerate() {
        if m != 0 {
            let (x, y) = (i % w, i / w);
            x1 = x1.min(x);
            y1 = y1.min(y);
            x2 = x2.max(x);
            y2 = y2.max(y);
        }
    }
    if x1 == usize::MAX {
        return None;
    }
    Some((
        x1.saturating_sub(pad),
        y1.saturating_sub(pad),
        (x2 + 1 + pad).min(w),
        (y2 + 1 + pad).min(h),
    ))
}

/// 纯黑：Y=16，U=V=128（limited range 黑，避免偏色）。
fn solid(nv12: &mut [u8], w: usize, mask: &[u8], x1: usize, y1: usize, x2: usize, y2: usize) {
    let ysize = mask.len(); // w*h
    for y in y1..y2 {
        for x in x1..x2 {
            if mask[y * w + x] != 0 {
                nv12[y * w + x] = 16;
                let uvi = ysize + (y / 2) * w + (x / 2) * 2;
                nv12[uvi] = 128;
                nv12[uvi + 1] = 128;
            }
        }
    }
}

/// 像素格马赛克：Y 平面 cell 块均值；UV 平面 cell/2 块（对应 4:2:0）。
/// 块求和保持标量写法：release 下被 LLVM 自动向量化（1080p 全帧基准
/// ~6ms，见 bench_pixelate_1080p）；2026-08-20 实测手写 NEON vpadalq 版
/// 全帧 +15%（5.96→5.08ms），但实际 mask 只覆盖人物区域（<20% 画面），
/// 真实收益 <0.2ms/帧——复杂度不值，故不引入。
fn pixelate(
    nv12: &mut [u8],
    w: usize,
    h: usize,
    mask: &[u8],
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    cell: usize,
) {
    // Y 平面
    for by in (y1..y2).step_by(cell) {
        let be_y = (by + cell).min(y2);
        for bx in (x1..x2).step_by(cell) {
            let be_x = (bx + cell).min(x2);
            let (mut sum, mut n) = (0u32, 0u32);
            for y in by..be_y {
                for x in bx..be_x {
                    sum += nv12[y * w + x] as u32;
                    n += 1;
                }
            }
            let avg = (sum / n) as u8;
            for y in by..be_y {
                for x in bx..be_x {
                    if mask[y * w + x] != 0 {
                        nv12[y * w + x] = avg;
                    }
                }
            }
        }
    }
    // UV 交错平面
    let cuv = (cell / 2).max(1);
    let uv = &mut nv12[w * h..];
    let (cx1, cy1) = (x1 / 2, y1 / 2);
    let (cx2, cy2) = ((x2 + 1) / 2, (y2 + 1) / 2);
    for by in (cy1..cy2).step_by(cuv) {
        let be_y = (by + cuv).min(cy2);
        for bx in (cx1..cx2).step_by(cuv) {
            let be_x = (bx + cuv).min(cx2);
            let (mut su, mut sv, mut n) = (0u32, 0u32, 0u32);
            for y in by..be_y {
                for x in bx..be_x {
                    let i = y * w + x * 2;
                    su += uv[i] as u32;
                    sv += uv[i + 1] as u32;
                    n += 1;
                }
            }
            let (au, av) = ((su / n) as u8, (sv / n) as u8);
            for y in by..be_y {
                for x in bx..be_x {
                    // 该色度像素对应的任一 luma 命中 mask 即写
                    let (ly, lx) = (y * 2, x * 2);
                    let hit = mask[ly * w + lx] != 0
                        || mask[ly * w + lx + 1] != 0
                        || mask[(ly + 1) * w + lx] != 0
                        || mask[(ly + 1) * w + lx + 1] != 0;
                    if hit {
                        let i = y * w + x * 2;
                        uv[i] = au;
                        uv[i + 1] = av;
                    }
                }
            }
        }
    }
}

/// box 模糊（水平+垂直两趟，clamp 边界采样），只写回 mask 命中像素。
/// Y 半径 r，UV 半径 (r+1)/2。
fn blur(nv12: &mut [u8], w: usize, h: usize, mask: &[u8], x1: usize, y1: usize, x2: usize, y2: usize, r: usize) {
    blur_plane(&mut nv12[..w * h], w, mask, x1, y1, x2, y2, r, false);
    let (cx1, cy1, cx2, cy2) = (x1 / 2, y1 / 2, (x2 + 1) / 2, (y2 + 1) / 2);
    blur_plane(&mut nv12[w * h..], w, mask, cx1, cy1, cx2, cy2, (r + 1) / 2, true);
}

/// 单平面（stride = w 字节）的模糊，滑动窗口实现（每像素 O(1) 累加）。
/// chroma=true 时平面为 UV 交错平面、坐标为色度坐标（半分辨率），
/// mask 判断用对应 luma (y*2, x*2)。窗口在 bbox 边缘 clamp 采样。
fn blur_plane(
    plane: &mut [u8],
    w: usize,
    mask: &[u8],
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    r: usize,
    chroma: bool,
) {
    let width = x2 - x1;
    let height = y2 - y1;
    if width == 0 || height == 0 {
        return;
    }
    let win = (2 * r + 1) as u32;
    let ri = r as isize;
    let mut tmp = vec![0u8; width * height];

    // 水平趟（滑动窗口，clamp 到 [0, x2-1]）
    let hi_x = x2 as isize - 1;
    let cx = |x: isize| x.clamp(0, hi_x) as usize;
    for (yi, y) in (y1..y2).enumerate() {
        let row = &plane[y * w..];
        // window(x1) = Σ_{d=-r..r} row[cx(x1+d)]
        let mut sum: u32 = (-ri..=ri).map(|d| row[cx(x1 as isize + d)] as u32).sum();
        for (xi, x) in (x1..x2).enumerate() {
            tmp[yi * width + xi] = (sum / win) as u8;
            // window(x) → window(x+1)：+row[cx(x+1+r)] -row[cx(x-r)]
            sum += row[cx(x as isize + 1 + ri)] as u32;
            sum -= row[cx(x as isize - ri)] as u32;
        }
    }

    // 垂直趟（滑动窗口，clamp 到 [0, height-1]）+ 按 mask 写回
    let cy = |y: isize| y.clamp(0, height as isize - 1) as usize;
    for xi in 0..width {
        let mut sum: u32 = (-ri..=ri).map(|d| tmp[cy(d) * width + xi] as u32).sum();
        for yi in 0..height {
            let (x, y) = (x1 + xi, y1 + yi);
            let hit = if chroma {
                mask[(y * 2) * w + x * 2] != 0
            } else {
                mask[y * w + x] != 0
            };
            if hit {
                plane[y * w + x] = (sum / win) as u8;
            }
            sum += tmp[cy(yi as isize + 1 + ri) * width + xi] as u32;
            sum -= tmp[cy(yi as isize - ri) * width + xi] as u32;
        }
    }
}

/// 限定区域内的 n 像素膨胀（区域 = xyxy 外扩 n+2；只影响区域内像素）。
/// 用于保持帧按 track 位移补边，成本 ∝ 区域面积×n，远低于全幅迭代。
pub fn dilate_region(mask: &mut [u8], w: usize, h: usize, xyxy: [f32; 4], n: usize) {
    let x1 = (xyxy[0].max(0.0) as usize).saturating_sub(n + 2);
    let y1 = (xyxy[1].max(0.0) as usize).saturating_sub(n + 2);
    let x2 = ((xyxy[2] as usize).min(w - 1) + n + 2).min(w - 1);
    let y2 = ((xyxy[3] as usize).min(h - 1) + n + 2).min(h - 1);
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let mut src;
    let mut cur = mask.to_vec();
    for _ in 0..n {
        src = cur;
        cur = mask.to_vec(); // 以原 mask 为底，仅区域内膨胀
        for y in y1..=y2 {
            let (r0, r2) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in x1..=x2 {
                if src[y * w + x] != 0 {
                    continue;
                }
                let (c0, c2) = (x.saturating_sub(1), (x + 1).min(w - 1));
                let hit = src[r0 * w + c0..=r0 * w + c2].iter().any(|&v| v != 0)
                    || src[y * w + c0..=y * w + c2].iter().any(|&v| v != 0)
                    || src[r2 * w + c0..=r2 * w + c2].iter().any(|&v| v != 0);
                if hit {
                    cur[y * w + x] = 1;
                    mask[y * w + x] = 1;
                }
            }
        }
    }
}

/// n 像素膨胀（n 次 3×3 迭代；隔帧保持帧的掩膜滞后补偿用）。
pub fn dilate(mask: &mut [u8], w: usize, h: usize, n: usize) {
    for _ in 0..n {
        dilate3(mask, w, h);
    }
}

/// mask 区域整数平移（GMC 保持帧跟随镜头）：区域内像素按 (dx,dy) 搬移，
/// 源缺失（越界）处置 0。区域 = xyxy 外扩 |d|+4。
pub fn shift_mask_region(mask: &mut [u8], w: usize, h: usize, xyxy: [f32; 4], dx: isize, dy: isize) {
    if dx == 0 && dy == 0 {
        return;
    }
    let m = (dx.unsigned_abs() + dy.unsigned_abs() + 4) as usize;
    let x1 = (xyxy[0].max(0.0) as usize).saturating_sub(m);
    let y1 = (xyxy[1].max(0.0) as usize).saturating_sub(m);
    let x2 = ((xyxy[2] as usize).min(w - 1) + m).min(w - 1);
    let y2 = ((xyxy[3] as usize).min(h - 1) + m).min(h - 1);
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let src = mask.to_vec();
    for y in y1..=y2 {
        let sy = y as isize - dy;
        if sy < 0 || sy > h as isize - 1 {
            for x in x1..=x2 {
                mask[y * w + x] = 0;
            }
            continue;
        }
        let sy = sy as usize;
        for x in x1..=x2 {
            let sx = x as isize - dx;
            mask[y * w + x] = if sx < 0 || sx > w as isize - 1 {
                0
            } else {
                src[sy * w + sx as usize]
            };
        }
    }
}

/// 限定区域内的 n 像素腐蚀（区域 = xyxy 外扩 3px；3×3 迭代 n 次）。
/// 丢失 track 遮罩渐隐用：边界逐帧回缩，人物离场不硬切。
/// 区域外视为 0（mask 基本落在观测框内），故框缘像素自然被腐蚀；
/// 迭代至稳定（全 0）即提前退出，n 大时不空转。
pub fn erode_region(mask: &mut [u8], w: usize, h: usize, xyxy: [f32; 4], n: usize) {
    if n == 0 {
        return;
    }
    let x1 = (xyxy[0].max(0.0) as usize).saturating_sub(3);
    let y1 = (xyxy[1].max(0.0) as usize).saturating_sub(3);
    let x2 = ((xyxy[2] as usize).min(w - 1) + 3).min(w - 1);
    let y2 = ((xyxy[3] as usize).min(h - 1) + 3).min(h - 1);
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let mut cur = mask.to_vec();
    for _ in 0..n {
        let src = cur.clone();
        let mut changed = false;
        for y in y1..=y2 {
            let (r0, r2) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in x1..=x2 {
                let i = y * w + x;
                if src[i] == 0 {
                    continue;
                }
                let (c0, c2) = (x.saturating_sub(1), (x + 1).min(w - 1));
                // 3×3 邻域任一为 0 → 腐蚀为 0
                let all_set = [r0, y, r2].iter().all(|&ry| {
                    src[ry * w + c0..=ry * w + c2].iter().all(|&v| v != 0)
                });
                if !all_set {
                    cur[i] = 0;
                    changed = true;
                }
            }
        }
        if !changed {
            break; // 已腐蚀殆尽/稳定
        }
    }
    for y in y1..=y2 {
        for x in x1..=x2 {
            mask[y * w + x] = cur[y * w + x];
        }
    }
}

/// 3×3 膨胀（就地，时序平滑用：上一帧 mask 膨胀后并集可补瞬时漏检）。
pub fn dilate3(mask: &mut [u8], w: usize, h: usize) {
    let src = mask.to_vec();
    for y in 0..h {
        let y0 = y.saturating_sub(1);
        let y2 = (y + 1).min(h - 1);
        for x in 0..w {
            if src[y * w + x] != 0 {
                continue;
            }
            let x0 = x.saturating_sub(1);
            let x2 = (x + 1).min(w - 1);
            let hit = src[y0 * w + x0..=y0 * w + x2].iter().any(|&v| v != 0)
                || src[y * w + x0..=y * w + x2].iter().any(|&v| v != 0)
                || src[y2 * w + x0..=y2 * w + x2].iter().any(|&v| v != 0);
            if hit {
                mask[y * w + x] = 1;
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, y: u8) -> Vec<u8> {
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        nv12[..w * h].fill(y);
        nv12
    }

    #[test]
    fn solid_blacks_masked_region() {
        let (w, h) = (32, 32);
        let mut nv12 = frame(w, h, 200);
        let mut mask = vec![0u8; w * h];
        for y in 8..16 {
            for x in 8..16 {
                mask[y * w + x] = 1;
            }
        }
        apply(&mut nv12, w, h, &mask, &MaskStyle::Solid);
        assert_eq!(nv12[12 * w + 12], 16); // mask 内 Y=16
        assert_eq!(nv12[0], 200); // mask 外不变
        let uv = h * w + 6 * w + 6 * 2; // (12,12) 的色度
        assert_eq!((nv12[uv], nv12[uv + 1]), (128, 128));
    }

    #[test]
    fn mosaic_flattens_gradient() {
        // 4 像素渐变区域被 cell=4 平均后应相等
        let (w, h) = (16, 16);
        let mut nv12 = frame(w, h, 0);
        for y in 0..4 {
            for x in 0..4 {
                nv12[y * w + x] = (x * 60) as u8; // 0,60,120,180
            }
        }
        let mut mask = vec![0u8; w * h];
        for y in 0..4 {
            for x in 0..4 {
                mask[y * w + x] = 1;
            }
        }
        apply(&mut nv12, w, h, &mask, &MaskStyle::Mosaic { cell: 4 });
        let vals: Vec<u8> = (0..4).map(|y| nv12[y * w + y]).collect();
        assert!(vals.iter().all(|&v| v == 90), "{vals:?}"); // (0+60+120+180)/4=90
    }

    #[test]
    fn empty_mask_is_noop() {
        let (w, h) = (16, 16);
        let mut nv12 = frame(w, h, 100);
        let before = nv12.clone();
        apply(&mut nv12, w, h, &vec![0; w * h], &MaskStyle::Solid);
        assert_eq!(nv12, before);
    }

    #[test]
    fn blur_reduces_contrast() {
        // 中心单点 255，模糊后该点值应下降
        let (w, h) = (32, 32);
        let mut nv12 = frame(w, h, 0);
        nv12[16 * w + 16] = 255;
        let mut mask = vec![0u8; w * h];
        for y in 12..20 {
            for x in 12..20 {
                mask[y * w + x] = 1;
            }
        }
        apply(&mut nv12, w, h, &mask, &MaskStyle::Blur { radius: 4 });
        assert!(nv12[16 * w + 16] < 255, "中心点应被模糊稀释");
    }

    #[test]
    fn erode_region_shrinks_rect_by_n() {
        // [8,24)² 实心矩形腐蚀 4px → [12,20)²；区域外不受影响
        let (w, h) = (48, 48);
        let mut mask = vec![0u8; w * h];
        for y in 8..24 {
            for x in 8..24 {
                mask[y * w + x] = 1;
            }
        }
        mask[40 * w + 40] = 1; // 区域外的孤立点必须原样保留
        erode_region(&mut mask, w, h, [8.0, 8.0, 24.0, 24.0], 4);
        assert_eq!(mask[12 * w + 12], 1, "收缩后的内域保留");
        assert_eq!(mask[16 * w + 16], 1, "中心保留");
        assert_eq!(mask[10 * w + 10], 0, "被腐蚀的边缘清零");
        assert_eq!(mask[11 * w + 11], 0, "腐蚀边界外清零");
        assert_eq!(mask[40 * w + 40], 1, "区域外不受影响");
    }

    #[test]
    fn erode_region_fully_consumes_large_n() {
        // n 远超矩形尺寸 → 全部腐蚀（且走提前退出路径）
        let (w, h) = (32, 32);
        let mut mask = vec![0u8; w * h];
        for y in 4..16 {
            for x in 4..16 {
                mask[y * w + x] = 1;
            }
        }
        erode_region(&mut mask, w, h, [4.0, 4.0, 16.0, 16.0], 200);
        assert!(mask.iter().all(|&v| v == 0), "大 n 应完全腐蚀");
    }

    #[test]
    fn erode_region_zero_n_is_noop() {
        let (w, h) = (16, 16);
        let mut mask = vec![1u8; w * h];
        let before = mask.clone();
        erode_region(&mut mask, w, h, [0.0, 0.0, 16.0, 16.0], 0);
        assert_eq!(mask, before);
    }

    #[test]
    #[ignore] // 基准（信息性）：1080p 全帧 mosaic cell=35，release 下 LLVM 自动
    // 向量化（2026-08-20：5.96ms；手写 NEON 版 5.08ms 但实际 mask 区域远小于全帧，不引入）
    fn bench_pixelate_1080p() {
        let (w, h) = (1920, 1080);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        for (i, v) in nv12.iter_mut().enumerate() {
            *v = ((i * 17) % 256) as u8;
        }
        let mask = vec![1u8; w * h];
        let t0 = std::time::Instant::now();
        for _ in 0..30 {
            apply(&mut nv12, w, h, &mask, &MaskStyle::Mosaic { cell: 35 });
        }
        println!("mosaic 1080p 全帧: {:.2} ms/帧", t0.elapsed().as_secs_f64() * 1000.0 / 30.0);
    }

    #[test]
    fn shift_mask_region_moves_rect() {        // [8,24)² 实心矩形平移 (+6,+3) → [14,30)×[11,27)；区域外清零、越界截断
        let (w, h) = (48, 48);
        let mut mask = vec![0u8; w * h];
        for y in 8..24 {
            for x in 8..24 {
                mask[y * w + x] = 1;
            }
        }
        mask[40 * w + 2] = 1; // 区域外的点必须原样保留
        shift_mask_region(&mut mask, w, h, [8.0, 8.0, 24.0, 24.0], 6, 3);
        assert_eq!(mask[15 * w + 20], 1, "平移后的内域");
        assert_eq!(mask[9 * w + 9], 0, "原位置清零");
        assert_eq!(mask[11 * w + 14], 1, "新左上角");
        assert_eq!(mask[10 * w + 14], 0, "新上边界之外");
        assert_eq!(mask[40 * w + 2], 1, "区域外不受影响");

        // 负向平移（-4,0）：dst(x) ← src(x+4)，[0,8)² 矩形 → x∈[0,4) 有值
        let mut mask2 = vec![0u8; w * h];
        for y in 0..8 {
            for x in 0..8 {
                mask2[y * w + x] = 1;
            }
        }
        shift_mask_region(&mut mask2, w, h, [0.0, 0.0, 8.0, 8.0], -4, 0);
        assert_eq!(mask2[4 * w + 0], 1, "src(4,4) 搬到 dst(0,4)");
        assert_eq!(mask2[4 * w + 3], 1, "src(7,4) 搬到 dst(3,4)");
        assert_eq!(mask2[4 * w + 4], 0, "src(8,4) 在矩形外 → 0");
        assert_eq!(mask2[4 * w + 6], 0, "旧位置已搬走");
    }
}
