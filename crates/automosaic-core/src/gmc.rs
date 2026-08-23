//! 相位相关全局运动补偿（GMC，DESIGN §5.3 OC-SORT 的"运动镜头开此项"落地）。
//!
//! 运动镜头下静止物体的观测位移 = 自身运动 + 相机运动；逐帧相位相关估计
//! 相机全局平移（下采样 Y 平面 → 2D FFT 互功率谱 → 逆变换峰值），用于：
//! - 关联前平移 KF 预测框（大平移下 IoU 匹配不丢轨）；
//! - 漏检保持帧把冻结 mask 按累积相机位移平移（隔帧检测的遮罩跟随镜头）。
//!
//! 峰值显著性（峰值/中位数）低于门限或频谱能量近零（静机位/无纹理）时
//! 输出 (0,0) 不干预——静态镜头零副作用，故无需用户判断即可常开。

use rustfft::FftPlanner;
use std::sync::OnceLock;

/// 下采样边长（可测位移 ±GMC_SIZE/2 个下采样像素；1080p 下 ±320 原始像素/帧）。
pub const GMC_SIZE: usize = 128;
/// 峰值显著性门限（峰值/中位数）：低于此视为无确定性全局运动。
const PEAK_RATIO: f32 = 6.0;
/// 频谱能量下限（防止平坦帧 0/0）。
const ENERGY_EPS: f32 = 1e-3;

/// Hann 窗（减少边缘跳变的频谱泄漏），构建一次常驻。
fn hann_1d() -> &'static [f32] {
    static W: OnceLock<Vec<f32>> = OnceLock::new();
    W.get_or_init(|| {
        (0..GMC_SIZE)
            .map(|i| {
                let s = (std::f32::consts::PI * i as f32 / GMC_SIZE as f32).sin();
                s * s
            })
            .collect()
    })
}

/// 逐帧全局位移估计器（有状态：持有上一帧的下采样 Y）。
pub struct GlobalMotionEstimator {
    prev: Option<Vec<f32>>,
    /// 行/列 FFT 计划（rustfft 计划可复用，非线程安全 → 估计器单线程使用）。
    planner: FftPlanner<f32>,
}

impl Default for GlobalMotionEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalMotionEstimator {
    pub fn new() -> Self {
        Self { prev: None, planner: FftPlanner::new() }
    }

    /// 当前帧相对上一帧的全局位移（原始分辨率像素，向右/向下为正）；
    /// 语义 = "上一帧的预测框平移该量后与当前帧观测对齐"。首帧/不显著 → (0,0)。
    pub fn shift(&mut self, nv12: &[u8], w: usize, h: usize) -> (f32, f32) {
        let cur = downsample_y(nv12, w, h);
        let mut a = real_to_complex(&cur);
        let prev = match self.prev.replace(cur) {
            Some(p) => p,
            None => return (0.0, 0.0),
        };
        let mut b = real_to_complex(&prev);
        fft2d(&mut self.planner, &mut a, true);
        fft2d(&mut self.planner, &mut b, true);
        let mut energy = 0.0f32;
        for (ca, cb) in a.iter_mut().zip(&b) {
            let (cr, ci) = (ca.re * cb.re + ca.im * cb.im, ca.im * cb.re - ca.re * cb.im);
            let mag = (cr * cr + ci * ci).sqrt();
            energy += mag;
            if mag > 1e-12 {
                *ca = rustfft::num_complex::Complex { re: cr / mag, im: ci / mag };
            } else {
                *ca = rustfft::num_complex::Complex { re: 0.0, im: 0.0 };
            }
        }
        if energy < ENERGY_EPS * (GMC_SIZE * GMC_SIZE) as f32 {
            return (0.0, 0.0); // 无纹理：不给噪声位移
        }
        fft2d(&mut self.planner, &mut a, false);
        let corr: Vec<f32> = a.iter().map(|c| (c.re * c.re + c.im * c.im).sqrt()).collect();
        let (dx, dy) = peak_shift(&corr);
        // 下采样空间 → 原始分辨率（宽高各向比例）
        (
            dx * w as f32 / GMC_SIZE as f32,
            dy * h as f32 / GMC_SIZE as f32,
        )
    }
}

/// Y 平面最近邻下采样到 GMC_SIZE²。**先减帧均值再去窗**（常数场加窗本身
/// 产生十字形频谱泄漏，必须精确去直流才能让平坦帧落入能量门限）。
fn downsample_y(nv12: &[u8], w: usize, h: usize) -> Vec<f32> {
    let win = hann_1d();
    let mut raw = vec![0.0f32; GMC_SIZE * GMC_SIZE];
    let mut mean = 0.0f32;
    for (i, o) in raw.iter_mut().enumerate() {
        let (x, y) = (i % GMC_SIZE, i / GMC_SIZE);
        let v = nv12[(y * h / GMC_SIZE) * w + x * w / GMC_SIZE] as f32;
        *o = v;
        mean += v;
    }
    mean /= (GMC_SIZE * GMC_SIZE) as f32;
    raw.into_iter()
        .enumerate()
        .map(|(i, v)| {
            let (x, y) = (i % GMC_SIZE, i / GMC_SIZE);
            (v - mean) * win[x] * win[y]
        })
        .collect()
}

fn real_to_complex(v: &[f32]) -> Vec<rustfft::num_complex::Complex<f32>> {
    v.iter().map(|&x| rustfft::num_complex::Complex { re: x, im: 0.0 }).collect()
}

/// 就地 2D FFT（行→列）；inverse 时除以 N² 归一化。
fn fft2d(
    planner: &mut FftPlanner<f32>,
    buf: &mut [rustfft::num_complex::Complex<f32>],
    forward: bool,
) {
    use rustfft::num_complex::Complex;
    let n = GMC_SIZE;
    let fft = if forward {
        planner.plan_fft_forward(n)
    } else {
        planner.plan_fft_inverse(n)
    };
    let mut tmp: Vec<Complex<f32>> = vec![Complex { re: 0.0, im: 0.0 }; n];
    // 行
    for r in 0..n {
        let row = &mut buf[r * n..(r + 1) * n];
        fft.process(row);
    }
    // 列（经临时缓冲）
    for c in 0..n {
        for (r, t) in tmp.iter_mut().enumerate() {
            *t = buf[r * n + c];
        }
        fft.process(&mut tmp);
        for (r, t) in tmp.iter().enumerate() {
            buf[r * n + c] = *t;
        }
    }
    if !forward {
        let scale = 1.0 / (n * n) as f32;
        for v in buf.iter_mut() {
            *v *= scale;
        }
    }
}

/// 相关面峰值 → 亚像素位移（原始尺度 = 下采样尺度；调用方再乘分辨率比）。
/// 峰值显著性不足 → (0,0)。
fn peak_shift(corr: &[f32]) -> (f32, f32) {
    let n = GMC_SIZE;
    let mut sorted = corr.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2].max(1e-9);
    let (peak_idx, &peak) = corr
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    if peak / median < PEAK_RATIO {
        return (0.0, 0.0);
    }
    let wrap = |v: usize| if v > n / 2 { v as isize - n as isize } else { v as isize };
    let (px, py) = (peak_idx % n, peak_idx / n);
    // 抛物线亚像素：比较环形邻域
    let at = |x: usize, y: usize| corr[y * n + x];
    let sub = |i: usize, dim: usize, get: &dyn Fn(usize) -> f32| -> f32 {
        let l = get((i + dim - 1) % dim);
        let c = get(i);
        let r = get((i + 1) % dim);
        let denom = l - 2.0 * c + r;
        if denom.abs() > 1e-9 {
            (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    };
    let row = |x: usize| at(x, py);
    let col = |y: usize| at(px, y);
    let dx = wrap(px) as f32 + sub(px, n, &row);
    let dy = wrap(py) as f32 + sub(py, n, &col);
    (dx, dy)
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪随机纹理（相位相关需要频谱丰富）。
    fn pattern(x: usize, y: usize) -> u8 {
        ((x * 73 + y * 151 + (x * y) % 17) % 256) as u8
    }

    fn frame_of(f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let (w, h) = (GMC_SIZE, GMC_SIZE);
        let mut nv12 = vec![128u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                nv12[y * w + x] = f(x, y);
            }
        }
        nv12
    }

    #[test]
    fn estimates_pure_translation() {
        let (w, h) = (GMC_SIZE, GMC_SIZE);
        let mut g = GlobalMotionEstimator::new();
        let prev = frame_of(pattern);
        // 向右 6、向下 3：cur(x,y) = prev(x-6, y-3)
        let cur = frame_of(|x, y| {
            let (sx, sy) = (x as isize - 6, y as isize - 3);
            if sx < 0 || sy < 0 || sx >= w as isize || sy >= h as isize {
                pattern(0, 0) // 边缘填充（相位相关对边缘不敏感）
            } else {
                pattern(sx as usize, sy as usize)
            }
        });
        g.shift(&prev, w, h);
        let (dx, dy) = g.shift(&cur, w, h);
        assert!((dx - 6.0).abs() < 0.6, "dx 应 ≈6，得 {dx}");
        assert!((dy - 3.0).abs() < 0.6, "dy 应 ≈3，得 {dy}");
    }

    #[test]
    fn static_and_flat_frames_give_zero() {
        let (w, h) = (GMC_SIZE, GMC_SIZE);
        let mut g = GlobalMotionEstimator::new();
        let f1 = frame_of(pattern);
        g.shift(&f1, w, h);
        let (dx, dy) = g.shift(&f1, w, h);
        assert!(dx.abs() < 0.05 && dy.abs() < 0.05, "完全相同帧 → 零位移，得 ({dx},{dy})");
        let flat1 = frame_of(|_, _| 100u8);
        let flat2 = frame_of(|_, _| 120u8);
        let mut g2 = GlobalMotionEstimator::new();
        g2.shift(&flat1, w, h);
        assert_eq!(g2.shift(&flat2, w, h), (0.0, 0.0), "无纹理帧 → 门限拦下");
    }

    #[test]
    fn scales_to_original_resolution() {
        // 256 宽帧移 16 原始像素 = 8 下采样像素；估计值应回到 16 原始像素
        let mut g = GlobalMotionEstimator::new();
        let (w, h) = (GMC_SIZE * 2, GMC_SIZE);
        let prev: Vec<u8> = (0..w * h).map(|i| pattern(i % w, i / w)).collect();
        let mut cur = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                cur[y * w + x] = if x >= 16 { pattern(x - 16, y) } else { pattern(0, y) };
            }
        }
        g.shift(&prev, w, h);
        let (dx, dy) = g.shift(&cur, w, h);
        assert!((dx - 16.0).abs() < 2.5, "16 原始像素平移应按 2× 比例还原，得 {dx}");
        assert!(dy.abs() < 1.0);
    }
}
