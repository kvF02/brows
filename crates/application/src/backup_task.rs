use crate::{PortError, PortErrorKind, PortResult};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupPhase {
    Preparing,
    DerivingKey,
    Processing,
    Verifying,
    Committing,
    Cleaning,
    Completed,
    Cancelled,
    Failed,
}

impl BackupPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::DerivingKey => "deriving_key",
            Self::Processing => "processing",
            Self::Verifying => "verifying",
            Self::Committing => "committing",
            Self::Cleaning => "cleaning",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProgress {
    pub phase: BackupPhase,
    pub processed_bytes: u64,
    pub total_bytes: Option<u64>,
    pub can_cancel: bool,
    pub cancel_requested: bool,
    pub restore_task_id: Option<u64>,
}

/// Cancellation and the irreversible commit boundary share one mutex. No
/// passwords, paths, archive contents or callback code are stored here.
#[derive(Debug)]
pub struct BackupTaskControl {
    state: Mutex<BackupProgress>,
}

impl Default for BackupTaskControl {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupTaskControl {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(BackupProgress {
                phase: BackupPhase::Preparing,
                processed_bytes: 0,
                total_bytes: None,
                can_cancel: true,
                cancel_requested: false,
                restore_task_id: None,
            }),
        }
    }
    pub fn snapshot(&self) -> BackupProgress {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn checkpoint(&self) -> PortResult<()> {
        if self.snapshot().cancel_requested {
            Err(PortError::new(PortErrorKind::Cancelled, "任务已取消"))
        } else {
            Ok(())
        }
    }
    pub fn request_cancel(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.can_cancel || state.phase.is_terminal() {
            return false;
        }
        state.cancel_requested = true;
        true
    }
    pub fn begin_commit(&self) -> PortResult<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancel_requested {
            return Err(PortError::new(PortErrorKind::Cancelled, "任务已取消"));
        }
        state.can_cancel = false;
        state.phase = BackupPhase::Committing;
        Ok(())
    }
    pub fn begin_cleanup(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = BackupPhase::Cleaning;
        state.can_cancel = false;
    }
    pub fn phase(&self, phase: BackupPhase) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
    }
    pub fn processed(&self, bytes: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.processed_bytes = state.processed_bytes.saturating_add(bytes as u64);
    }
    pub fn total(&self, bytes: u64) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .total_bytes = Some(bytes);
    }
    pub fn attach_restore(&self, id: u64) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore_task_id = Some(id);
    }
    pub fn finish(&self, phase: BackupPhase) {
        debug_assert!(phase.is_terminal());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = phase;
        state.can_cancel = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn cancellation_and_commit_admission_are_mutually_exclusive() {
        for _ in 0..40 {
            let control = Arc::new(BackupTaskControl::new());
            let barrier = Arc::new(Barrier::new(2));
            let commit = {
                let control = control.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    control.begin_commit().is_ok()
                })
            };
            barrier.wait();
            let cancelled = control.request_cancel();
            assert_ne!(cancelled, commit.join().unwrap());
            control.begin_cleanup();
            assert!(!control.request_cancel());
            assert_eq!(control.snapshot().cancel_requested, cancelled);
        }
    }
}
