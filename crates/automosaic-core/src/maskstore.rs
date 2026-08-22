//! 两阶段 mask 缓存（DESIGN §5.6/§2.1「分析→渲染」的断点续跑地基）。
//!
//! 每帧一个 RLE 二进制文件 `frame_{idx:08}.mask`（写 tmp 后 rename，单帧原子
//! ——中断最多丢当前帧，已落盘帧全部有效）；`meta.json` 记录分辨率与帧数，
//! render 前校验一致。复核/重渲染只消费缓存，不重推（§5.6 渲染段语义）。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MaskMeta {
    pub width: u32,
    pub height: u32,
    /// 已分析的最大帧号 + 1（= 下一个待分析帧）。
    pub frames: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MaskStoreError {
    #[error("mask 目录不存在: {0}")]
    NoDir(PathBuf),
    #[error("meta.json 缺失或损坏（先运行 analyze）")]
    NoMeta,
    #[error("mask 缓存与视频不符：缓存 {cached}×{cached_h}，视频 {got}×{got_h}")]
    SizeMismatch { cached: u32, cached_h: u32, got: u32, got_h: u32 },
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("帧 {0} 的 mask 文件损坏（RLE 长度不符）")]
    Corrupt(u64),
}

pub struct MaskStore {
    dir: PathBuf,
}

impl MaskStore {
    pub fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.to_path_buf() })
    }

    fn frame_path(&self, idx: u64) -> PathBuf {
        self.dir.join(format!("frame_{idx:08}.mask"))
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    pub fn save_meta(&self, meta: &MaskMeta) -> io::Result<()> {
        let tmp = self.dir.join("meta.json.tmp");
        fs::write(&tmp, serde_json::to_vec(meta).unwrap())?;
        fs::rename(&tmp, self.meta_path())
    }

    pub fn load_meta(&self) -> Result<MaskMeta, MaskStoreError> {
        if !self.meta_path().is_file() {
            return Err(MaskStoreError::NoMeta);
        }
        let bytes = fs::read(self.meta_path())?;
        Ok(serde_json::from_slice(&bytes).map_err(|_| MaskStoreError::NoMeta)?)
    }

    /// render 前校验：缓存尺寸须与视频一致。
    pub fn verify(&self, width: u32, height: u32) -> Result<MaskMeta, MaskStoreError> {
        if !self.dir.is_dir() {
            return Err(MaskStoreError::NoDir(self.dir.clone()));
        }
        let meta = self.load_meta()?;
        if meta.width != width || meta.height != height {
            return Err(MaskStoreError::SizeMismatch {
                cached: meta.width,
                cached_h: meta.height,
                got: width,
                got_h: height,
            });
        }
        Ok(meta)
    }

    /// 落盘一帧 mask（0/1 二值，RLE = [u32 行程数][(u32 长度, u8 值)…] 小端）。
    /// 同步更新 meta 的 frames（已完成帧号 +1），写失败返回 Err 由管线中止。
    pub fn save_mask(&self, idx: u64, mask: &[u8]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(16 + mask.len() / 8);
        rle_encode(mask, &mut buf);
        let path = self.frame_path(idx);
        let tmp = self.dir.join(format!("frame_{idx:08}.tmp"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_data()?; // rename 前确保数据落盘（断电安全）
        }
        fs::rename(&tmp, &path)
    }

    /// 读取一帧 mask 并解码（按 w*h 校验 RLE 总长）。缺帧返回 Ok(None)。
    pub fn load_mask(&self, idx: u64, w: usize, h: usize) -> Result<Option<Vec<u8>>, MaskStoreError> {
        let path = self.frame_path(idx);
        if !path.is_file() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)?.read_to_end(&mut bytes)?;
        if bytes.len() < 4 {
            return Err(MaskStoreError::Corrupt(idx));
        }
        let n = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if bytes.len() != 4 + n * 5 {
            return Err(MaskStoreError::Corrupt(idx));
        }
        let mut mask = Vec::with_capacity(w * h);
        for r in 0..n {
            let off = 4 + r * 5;
            let c = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
            let v = bytes[off + 4];
            if v > 1 {
                return Err(MaskStoreError::Corrupt(idx));
            }
            mask.resize((mask.len() + c).min(w * h + 1), v);
        }
        if mask.len() != w * h {
            return Err(MaskStoreError::Corrupt(idx));
        }
        Ok(Some(mask))
    }

    /// 已分析帧数（目录内最大帧号 + 1；空目录 = 0）——断点续跑的起点。
    pub fn analyzed_frames(&self) -> u64 {
        let mut max: Option<u64> = None;
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if let Some(idx) = name
                    .strip_prefix("frame_")
                    .and_then(|s| s.strip_suffix(".mask"))
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    max = Some(max.map_or(idx, |m: u64| m.max(idx)));
                }
            }
        }
        max.map_or(0, |m| m + 1)
    }

    // --------------------------------------------------------------------------- //
    // 实例层（M5 复核的编辑单元）：frame_{idx:08}.inst
    // 格式：[u32 n] × n 条 [u64 id][u8 kind][f32 score][4×f32 xyxy][RLE mask]
    // --------------------------------------------------------------------------- //

    fn inst_path(&self, idx: u64) -> PathBuf {
        self.dir.join(format!("frame_{idx:08}.inst"))
    }

    /// 落盘一帧的实例列表（原子写，同 .mask 语义）。缺实例帧 = 无 .inst 文件。
    pub fn save_instances(&self, idx: u64, instances: &[InstanceRecord]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(64 + instances.len() * 128);
        buf.extend_from_slice(&(instances.len() as u32).to_le_bytes());
        for inst in instances {
            buf.extend_from_slice(&inst.id.to_le_bytes());
            buf.push(inst.kind);
            buf.extend_from_slice(&inst.score.to_le_bytes());
            for v in inst.xyxy {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            rle_encode(&inst.mask, &mut buf);
        }
        let path = self.inst_path(idx);
        let tmp = self.dir.join(format!("frame_{idx:08}.inst.tmp"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_data()?;
        }
        fs::rename(&tmp, &path)
    }

    /// 读取一帧实例（按 w*h 校验）。无文件 = Ok(None)。
    pub fn load_instances(&self, idx: u64, w: usize, h: usize) -> Result<Option<Vec<InstanceRecord>>, MaskStoreError> {
        let path = self.inst_path(idx);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        if bytes.len() < 4 {
            return Err(MaskStoreError::Corrupt(idx));
        }
        let n = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let mut out = Vec::with_capacity(n);
        let mut off = 4usize;
        for _ in 0..n {
            if off + 8 + 1 + 4 + 16 > bytes.len() {
                return Err(MaskStoreError::Corrupt(idx));
            }
            let id = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
            let kind = bytes[off + 8];
            let score = f32::from_le_bytes(bytes[off + 9..off + 13].try_into().unwrap());
            let mut xyxy = [0f32; 4];
            for (k, v) in xyxy.iter_mut().enumerate() {
                let s = off + 13 + k * 4;
                *v = f32::from_le_bytes(bytes[s..s + 4].try_into().unwrap());
            }
            off += 13 + 16;
            let mask = rle_decode(&bytes, &mut off, w * h).ok_or(MaskStoreError::Corrupt(idx))?;
            out.push(InstanceRecord { id, kind, score, xyxy, mask });
        }
        Ok(Some(out))
    }
}

/// 实例层的单条记录（复核 UI / 渲染重组用）。
#[derive(Clone)]
pub struct InstanceRecord {
    pub id: u64,
    /// 0=person masklet，1=孤立人脸（archive::KIND_*）。
    pub kind: u8,
    pub score: f32,
    pub xyxy: [f32; 4],
    pub mask: Vec<u8>,
}

/// RLE 编码（与 .mask 文件一致：[u32 runs][(u32 len, u8 val)…] 小端）。
fn rle_encode(mask: &[u8], out: &mut Vec<u8>) {
    let mut runs: Vec<(u32, u8)> = Vec::new();
    let mut it = mask.iter().copied();
    let mut cur = it.next().unwrap_or(0);
    let mut count: u32 = if mask.is_empty() { 0 } else { 1 };
    for v in it {
        if v == cur && count < u32::MAX {
            count += 1;
        } else {
            runs.push((count, cur));
            cur = v;
            count = 1;
        }
    }
    if !mask.is_empty() {
        runs.push((count, cur));
    }
    out.extend_from_slice(&(runs.len() as u32).to_le_bytes());
    for (c, v) in runs {
        out.extend_from_slice(&c.to_le_bytes());
        out.push(v);
    }
}

/// 从 `off` 起解码一条 RLE mask（期望 len 像素）；成功时推进 off。
fn rle_decode(bytes: &[u8], off: &mut usize, expect: usize) -> Option<Vec<u8>> {
    if *off + 4 > bytes.len() {
        return None;
    }
    let n = u32::from_le_bytes(bytes[*off..*off + 4].try_into().ok()?) as usize;
    *off += 4;
    let mut mask = Vec::with_capacity(expect);
    for _ in 0..n {
        if *off + 5 > bytes.len() {
            return None;
        }
        let c = u32::from_le_bytes(bytes[*off..*off + 4].try_into().ok()?) as usize;
        let v = bytes[*off + 4];
        if v > 1 {
            return None;
        }
        *off += 5;
        if mask.len() + c > expect {
            return None;
        }
        mask.resize(mask.len() + c, v);
    }
    if mask.len() != expect {
        return None;
    }
    Some(mask)
}

// --------------------------------------------------------------------------- //
// 复核补丁（M5 复核 UI 的产物，渲染段消费）：patches.bin
// 格式：[u32 版本=1][u32 n] × n 条 [u64 帧][u8 op(0=add,1=erase)][RLE]
// --------------------------------------------------------------------------- //

/// 补丁操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOp {
    Add,
    Erase,
}

/// 单条补丁（一帧上的笔刷区域或 SAM 重提示差异）。
#[derive(Clone)]
pub struct Patch {
    pub frame: u64,
    pub op: PatchOp,
    pub mask: Vec<u8>,
}

/// 补丁集：内存态 + 原子落盘；render 按文件顺序应用。
#[derive(Default, Clone)]
pub struct PatchStore {
    pub patches: Vec<Patch>,
}

impl PatchStore {
    /// 从 mask 目录读取（无文件 = 空集）。
    pub fn load(dir: &Path) -> Self {
        let bytes = match fs::read(dir.join("patches.bin")) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        if bytes.len() < 8 {
            return Self::default();
        }
        let version = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let n = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if version != 1 {
            return Self::default();
        }
        let mut patches = Vec::with_capacity(n);
        let mut off = 8usize;
        for _ in 0..n {
            if off + 8 + 1 > bytes.len() {
                return Self::default(); // 损坏：整体忽略（宁可丢补丁不出错片）
            }
            let frame = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
            let op = match bytes[off + 8] {
                0 => PatchOp::Add,
                _ => PatchOp::Erase,
            };
            off += 9;
            // 尺寸未知（渲染时按 w*h 校验）：先解码任意长度
            let mask = rle_decode_any(&bytes, &mut off);
            patches.push(Patch { frame, op, mask });
        }
        Self { patches }
    }

    /// 原子落盘。
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let mut buf = Vec::with_capacity(16 + self.patches.len() * 128);
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&(self.patches.len() as u32).to_le_bytes());
        for p in &self.patches {
            buf.extend_from_slice(&p.frame.to_le_bytes());
            buf.push(match p.op {
                PatchOp::Add => 0,
                PatchOp::Erase => 1,
            });
            rle_encode(&p.mask, &mut buf);
        }
        let tmp = dir.join("patches.bin.tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, dir.join("patches.bin"))
    }

    /// 追加一条（内存 + 落盘）。
    pub fn push(&mut self, dir: &Path, patch: Patch) -> io::Result<()> {
        self.patches.push(patch);
        self.save(dir)
    }

    /// 清空指定帧的全部补丁（撤销该帧编辑）。
    pub fn clear_frame(&mut self, dir: &Path, frame: u64) -> io::Result<()> {
        self.patches.retain(|p| p.frame != frame);
        self.save(dir)
    }

    /// 应用某帧的全部补丁到 mask（按顺序：add |= ，erase &= !）。
    pub fn apply(&self, frame: u64, mask: &mut [u8]) {
        for p in self.patches.iter().filter(|p| p.frame == frame) {
            if p.mask.len() != mask.len() {
                continue; // 分辨率不符（缓存换过）：跳过而非崩溃
            }
            match p.op {
                PatchOp::Add => {
                    for (o, &v) in mask.iter_mut().zip(&p.mask) {
                        *o |= v;
                    }
                }
                PatchOp::Erase => {
                    for (o, &v) in mask.iter_mut().zip(&p.mask) {
                        *o &= !v;
                    }
                }
            }
        }
    }
}

/// 任意长度 RLE 解码（load 时未知 w*h）。
fn rle_decode_any(bytes: &[u8], off: &mut usize) -> Vec<u8> {
    if *off + 4 > bytes.len() {
        return vec![];
    }
    let n = u32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap()) as usize;
    *off += 4;
    let mut mask = Vec::new();
    for _ in 0..n {
        if *off + 5 > bytes.len() {
            return mask;
        }
        let c = u32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap()) as usize;
        let v = bytes[*off + 4].min(1);
        *off += 5;
        mask.resize(mask.len() + c, v);
    }
    mask
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_resume() {
        let dir = std::env::temp_dir().join(format!("maskstore_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = MaskStore::new(&dir).unwrap();
        assert_eq!(store.analyzed_frames(), 0, "空目录 = 0");

        let (w, h) = (64usize, 32usize);
        let mut mask = vec![0u8; w * h];
        for y in 4..10 {
            for x in 8..30 {
                mask[y * w + x] = 1;
            }
        }
        store.save_mask(0, &vec![0; w * h]).unwrap();
        store.save_mask(1, &mask).unwrap();
        store.save_mask(2, &vec![1; w * h]).unwrap();
        store.save_meta(&MaskMeta { width: w as u32, height: h as u32, frames: 3 }).unwrap();

        assert_eq!(store.analyzed_frames(), 3, "断点续跑起点 = 最大帧号 + 1");
        let m = store.load_mask(1, w, h).unwrap().unwrap();
        assert_eq!(m, mask, "RLE 往返无损");
        assert_eq!(store.load_mask(1, w, h).unwrap().unwrap().len(), w * h);
        assert!(store.load_mask(9, w, h).unwrap().is_none(), "缺帧 = None");

        // meta 校验：尺寸不符报错
        assert!(matches!(
            store.verify(320, 240),
            Err(MaskStoreError::SizeMismatch { .. })
        ));
        let meta = store.verify(w as u32, h as u32).unwrap();
        assert_eq!(meta.frames, 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_detected() {
        let dir = std::env::temp_dir().join(format!("maskstore_bad_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = MaskStore::new(&dir).unwrap();
        store.save_mask(0, &vec![1, 0, 1]).unwrap();
        // 截断破坏
        let path = dir.join("frame_00000000.mask");
        let full = fs::read(&path).unwrap();
        fs::write(&path, &full[..full.len() - 2]).unwrap();
        assert!(matches!(store.load_mask(0, 2, 2), Err(MaskStoreError::Corrupt(0))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn instances_roundtrip() {
        let dir = std::env::temp_dir().join(format!("maskstore_inst_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = MaskStore::new(&dir).unwrap();
        let (w, h) = (32usize, 16usize);
        let mut m1 = vec![0u8; w * h];
        m1[100..140].fill(1);
        let recs = vec![
            InstanceRecord { id: 7, kind: 0, score: 0.9, xyxy: [1.0, 2.0, 3.0, 4.0], mask: m1.clone() },
            InstanceRecord { id: 9, kind: 1, score: 0.5, xyxy: [5.0, 6.0, 7.0, 8.0], mask: vec![1; w * h] },
        ];
        store.save_instances(3, &recs).unwrap();
        let back = store.load_instances(3, w, h).unwrap().unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].id, 7);
        assert_eq!(back[0].score, 0.9);
        assert_eq!(back[0].xyxy, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(back[0].mask, m1);
        assert_eq!(back[1].mask, vec![1; w * h]);
        assert!(store.load_instances(4, w, h).unwrap().is_none(), "缺帧 = None");
        // analyzed_frames 只看 .mask 文件，不受 .inst 影响
        assert_eq!(store.analyzed_frames(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patches_roundtrip_and_apply() {
        let dir = std::env::temp_dir().join(format!("maskstore_pat_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (w, h) = (16usize, 8usize);
        let mut add = vec![0u8; w * h];
        add[..8].fill(1);
        let mut erase = vec![0u8; w * h];
        erase[0..2].fill(1);

        let mut ps = PatchStore::default();
        ps.push(&dir, Patch { frame: 5, op: PatchOp::Add, mask: add.clone() }).unwrap();
        ps.push(&dir, Patch { frame: 5, op: PatchOp::Erase, mask: erase.clone() }).unwrap();
        ps.push(&dir, Patch { frame: 9, op: PatchOp::Add, mask: vec![1; w * h] }).unwrap();

        // 重新加载：帧序与操作保序
        let ps2 = PatchStore::load(&dir);
        assert_eq!(ps2.patches.len(), 3);
        assert_eq!(ps2.patches[0].frame, 5);
        assert_eq!(ps2.patches[1].op, PatchOp::Erase);

        // 应用：帧 5 前 8 置 1 再擦前 2
        let mut mask = vec![0u8; w * h];
        ps2.apply(5, &mut mask);
        assert_eq!(&mask[..8], &[0, 0, 1, 1, 1, 1, 1, 1]);
        // 帧无关补丁不越帧
        let mut other = vec![0u8; w * h];
        ps2.apply(6, &mut other);
        assert!(other.iter().all(|&v| v == 0));

        // 清帧
        let mut ps3 = ps2;
        ps3.clear_frame(&dir, 5).unwrap();
        assert_eq!(PatchStore::load(&dir).patches.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
