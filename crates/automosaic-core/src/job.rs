//! 作业队列管理（DESIGN §2.2 JobManager：队列、状态机、取消）。
//!
//! UI（FFI）当前在 Dart 侧 QueueController 自管队列——本模块把同一状态机
//! 下沉到 core：CLI 批处理（`queue` 子命令）直接消费，未来 FFI 队列迁移时
//! 复用。状态机：`Queued → Running → Done/Failed/Cancelled`，单向不可逆；
//! Queued 作业可直接取消（从未启动），Running 作业经取消标志由执行方回写
//! Cancelled 终态。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 单作业取消标志（与 pipe::CancelFlag 同型，可直连管线）。
pub type JobCancel = Arc<AtomicBool>;

/// 作业状态（终态：Done/Failed/Cancelled）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done { frames: u64 },
    Failed { error: String },
    Cancelled { frames: u64 },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running)
    }
}

/// 队列快照条目。
#[derive(Debug, Clone)]
pub struct JobStatus {
    pub id: u64,
    pub state: JobState,
}

#[derive(Default)]
pub struct JobManager {
    next_id: u64,
    jobs: Vec<(u64, JobState)>,
    current: Option<u64>,
    cancels: HashMap<u64, JobCancel>,
}

impl JobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队，返回作业 id（从 0 递增）。
    pub fn enqueue(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push((id, JobState::Queued));
        id
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn state(&self, id: u64) -> Option<&JobState> {
        self.jobs.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
    }

    /// 队列快照（入列顺序）。
    pub fn list(&self) -> Vec<JobStatus> {
        self.jobs
            .iter()
            .map(|(id, s)| JobStatus { id: *id, state: s.clone() })
            .collect()
    }

    /// 是否还有排队或运行中的作业。
    pub fn has_pending(&self) -> bool {
        self.jobs.iter().any(|(_, s)| !s.is_terminal())
    }

    /// 启动下一个排队作业：置 Running 并创建其取消句柄。
    /// 无排队作业返回 None（运行中的不算——本管理器语义为串行执行）。
    pub fn start_next(&mut self) -> Option<(u64, JobCancel)> {
        if self.current.is_some() {
            return None;
        }
        let slot = self
            .jobs
            .iter_mut()
            .find(|(_, s)| matches!(s, JobState::Queued))?;
        slot.1 = JobState::Running;
        let id = slot.0;
        self.current = Some(id);
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels.insert(id, Arc::clone(&flag));
        Some((id, flag))
    }

    /// 回写终态。仅 Running 作业可转入终态（幂等防线：重复回写/对排队作业
    /// 回写均拒绝）；Done/Cancelled 带 frames（半成品帧数，进度口径）。
    pub fn finish(&mut self, id: u64, state: JobState) -> bool {
        if !state.is_terminal() {
            return false;
        }
        let Some(slot) = self.jobs.iter_mut().find(|(i, _)| *i == id) else {
            return false;
        };
        if !matches!(slot.1, JobState::Running) {
            return false;
        }
        slot.1 = state;
        if self.current == Some(id) {
            self.current = None;
        }
        self.cancels.remove(&id);
        true
    }

    /// 请求取消：Running → 置取消标志（执行方在帧边界察觉后回写 Cancelled）；
    /// Queued → 直接转 Cancelled（从未启动）。已终态返回 false。
    pub fn cancel(&mut self, id: u64) -> bool {
        let Some(slot) = self.jobs.iter_mut().find(|(i, _)| *i == id) else {
            return false;
        };
        match &slot.1 {
            JobState::Running => {
                if let Some(f) = self.cancels.get(&id) {
                    f.store(true, Ordering::Relaxed);
                }
                true
            }
            JobState::Queued => {
                slot.1 = JobState::Cancelled { frames: 0 };
                true
            }
            _ => false,
        }
    }

    /// 取消句柄（Running 作业；供执行方轮询）。
    pub fn cancel_flag(&self, id: u64) -> Option<JobCancel> {
        self.cancels.get(&id).cloned()
    }

    /// 移除已终态的作业。
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|(i, s)| *i != id || !s.is_terminal());
        self.jobs.len() < before
    }

    /// 清除全部已终态作业。
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|(_, s)| !s.is_terminal());
    }
}

// --------------------------------------------------------------------------- //
// 测试
// --------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_queued_running_done() {
        let mut jm = JobManager::new();
        let a = jm.enqueue();
        let b = jm.enqueue();
        assert_eq!(jm.len(), 2);
        assert!(jm.has_pending());
        assert_eq!(jm.state(a), Some(&JobState::Queued));

        let (id, _flag) = jm.start_next().unwrap();
        assert_eq!(id, a, "FIFO");
        assert_eq!(jm.state(a), Some(&JobState::Running));
        assert!(jm.start_next().is_none(), "串行：运行中不取下一作业");

        assert!(jm.finish(a, JobState::Done { frames: 100 }));
        assert!(!jm.has_pending() || jm.state(b) == Some(&JobState::Queued));
        let (id2, _) = jm.start_next().unwrap();
        assert_eq!(id2, b);
        jm.finish(b, JobState::Failed { error: "编码失败".into() });
        assert!(!jm.has_pending());
    }

    #[test]
    fn finish_rejects_non_running() {
        let mut jm = JobManager::new();
        let a = jm.enqueue();
        assert!(!jm.finish(a, JobState::Done { frames: 1 }), "Queued 不可直接终态");
        let (_, _) = jm.start_next().unwrap();
        assert!(jm.finish(a, JobState::Done { frames: 5 }));
        assert!(!jm.finish(a, JobState::Done { frames: 6 }), "终态不可重复回写");
    }

    #[test]
    fn cancel_queued_marks_cancelled_running_sets_flag() {
        let mut jm = JobManager::new();
        let a = jm.enqueue();
        let b = jm.enqueue();
        assert!(jm.cancel(a), "排队作业直接取消");
        assert_eq!(jm.state(a), Some(&JobState::Cancelled { frames: 0 }));
        let (id, flag) = jm.start_next().unwrap();
        assert_eq!(id, b, "已取消的排队作业不被启动");
        assert!(!flag.load(Ordering::Relaxed));
        assert!(jm.cancel(b), "运行中作业置取消标志");
        assert!(flag.load(Ordering::Relaxed));
        assert!(jm.finish(b, JobState::Cancelled { frames: 12 }));
        assert!(!jm.cancel(b), "终态不可再取消");
    }

    #[test]
    fn remove_and_clear_finished() {
        let mut jm = JobManager::new();
        let a = jm.enqueue();
        let b = jm.enqueue();
        assert!(!jm.remove(a), "排队中不可移除");
        jm.cancel(a);
        assert!(jm.remove(a));
        assert_eq!(jm.len(), 1);
        let _ = jm.start_next();
        jm.finish(b, JobState::Done { frames: 1 });
        jm.clear_finished();
        assert!(jm.is_empty());
    }
}
