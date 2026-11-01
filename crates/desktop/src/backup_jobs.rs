use brows_application::{BackupPhase, BackupProgress, BackupTaskControl};
use brows_contracts::{ErrorCode, ErrorDto};
use std::{
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, ErrorDto>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackupJobKind {
    Export,
    Import,
    Cleanup,
}

#[derive(Debug, Clone)]
pub(crate) struct BackupJobSnapshot {
    pub id: String,
    pub kind: BackupJobKind,
    pub created_at_ms: u64,
    pub progress: BackupProgress,
    pub error: Option<ErrorCode>,
}

#[derive(Debug)]
struct Job {
    id: String,
    kind: BackupJobKind,
    created_at_ms: u64,
    control: BackupTaskControl,
    outcome: Mutex<Option<ErrorCode>>,
}

impl Job {
    fn snapshot(&self) -> BackupJobSnapshot {
        let outcome = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        BackupJobSnapshot {
            id: self.id.clone(),
            kind: self.kind,
            created_at_ms: self.created_at_ms,
            progress: self.control.snapshot(),
            error: outcome.clone(),
        }
    }
    fn finish<T>(&self, result: &Result<T>) {
        let mut outcome = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        *outcome = result.as_ref().err().map(|error| error.code.clone());
        self.control.finish(match result {
            Ok(_) => BackupPhase::Completed,
            Err(error) if error.code == ErrorCode::OperationCancelled => BackupPhase::Cancelled,
            Err(_) => BackupPhase::Failed,
        });
    }
}

#[derive(Debug, Default)]
struct State {
    stopping: bool,
    active: usize,
    history: VecDeque<Arc<Job>>,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
    finished: Condvar,
}

/// Bounds heavy archive work to one operation. Only non-secret snapshots live
/// in the registry; request data belongs to the worker's FnOnce closure.
#[derive(Debug, Default)]
pub(crate) struct BackupJobs {
    inner: Arc<Inner>,
}

impl BackupJobs {
    fn begin(&self, kind: BackupJobKind) -> Result<JobLease> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err(error(ErrorCode::InvalidProfileState, "管理器正在退出"));
        }
        if state.active != 0 {
            return Err(error(
                ErrorCode::AlreadyExists,
                "已有备份、恢复或清理任务正在执行",
            ));
        }
        let job = Arc::new(Job {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            created_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            control: BackupTaskControl::new(),
            outcome: Mutex::new(None),
        });
        while state.history.len() >= 32 {
            state.history.pop_front();
        }
        state.history.push_back(job.clone());
        state.active = 1;
        Ok(JobLease {
            inner: self.inner.clone(),
            job,
        })
    }

    pub(crate) fn snapshots(&self) -> Vec<BackupJobSnapshot> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .iter()
            .rev()
            .map(|job| job.snapshot())
            .collect()
    }

    pub(crate) fn cancel(&self, id: &str) -> Result<()> {
        let job = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .history
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .ok_or_else(|| error(ErrorCode::NotFound, "找不到该后台任务"))?;
        if job.control.request_cancel() || job.control.snapshot().cancel_requested {
            return Ok(());
        }
        Err(error(
            ErrorCode::InvalidProfileState,
            "任务已结束或进入提交、清理阶段，不能取消",
        ))
    }

    pub(crate) fn stop_accepting(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopping = true;
        for job in &state.history {
            job.control.request_cancel();
        }
    }

    pub(crate) fn shutdown(&self) {
        self.stop_accepting();
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.inner
                .finished
                .wait_while(state, |state| state.active != 0)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
}

struct JobLease {
    inner: Arc<Inner>,
    job: Arc<Job>,
}

impl JobLease {
    fn run<T>(self, work: impl FnOnce(&BackupTaskControl) -> Result<T>) -> Result<T> {
        let result =
            catch_unwind(AssertUnwindSafe(|| work(&self.job.control))).unwrap_or_else(|_| {
                Err(error(
                    ErrorCode::Internal,
                    "后台任务执行失败，请检查任务记录",
                ))
            });
        // The FnOnce and its captured request/service resources have dropped.
        self.job.finish(&result);
        result
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        if !self.job.control.snapshot().phase.is_terminal() {
            self.job
                .finish::<()>(&Err(error(ErrorCode::Internal, "后台任务未完成")));
        }
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
        self.inner.finished.notify_all();
    }
}

pub(crate) async fn execute<T, F>(jobs: Arc<BackupJobs>, kind: BackupJobKind, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&BackupTaskControl) -> Result<T> + Send + 'static,
{
    let lease = jobs.begin(kind)?;
    tauri::async_runtime::spawn_blocking(move || lease.run(work))
        .await
        .map_err(|_| error(ErrorCode::Internal, "后台任务执行失败"))?
}

fn error(code: ErrorCode, message: &str) -> ErrorDto {
    ErrorDto {
        code,
        message: message.into(),
        violations: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn cancelled_job_keeps_ownership_until_the_worker_finishes_cleanup() {
        let jobs = BackupJobs::default();
        let lease = jobs.begin(BackupJobKind::Import).unwrap();
        let id = jobs.snapshots()[0].id.clone();
        assert!(jobs.begin(BackupJobKind::Export).is_err());
        jobs.cancel(&id).unwrap();
        assert_eq!(jobs.snapshots()[0].progress.phase, BackupPhase::Preparing);
        let result: Result<()> = lease.run(|control| {
            assert!(control.snapshot().cancel_requested);
            control.begin_cleanup();
            assert!(jobs.begin(BackupJobKind::Cleanup).is_err());
            assert_eq!(jobs.snapshots()[0].progress.phase, BackupPhase::Cleaning);
            Err(error(ErrorCode::OperationCancelled, "cancelled"))
        });
        assert_eq!(result.unwrap_err().code, ErrorCode::OperationCancelled);
        assert_eq!(jobs.snapshots()[0].progress.phase, BackupPhase::Cancelled);
        jobs.begin(BackupJobKind::Export)
            .unwrap()
            .run(|_| Ok(()))
            .unwrap();
    }

    #[test]
    fn committing_job_rejects_cancel_and_shutdown_waits_for_its_resources() {
        let jobs = Arc::new(BackupJobs::default());
        let lease = jobs.begin(BackupJobKind::Export).unwrap();
        lease.job.control.begin_commit().unwrap();
        assert!(jobs.cancel(&lease.job.id).is_err());
        let (started, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let worker = {
            let jobs = jobs.clone();
            std::thread::spawn(move || {
                jobs.stop_accepting();
                started.send(()).unwrap();
                jobs.shutdown();
                finished.send(()).unwrap();
            })
        };
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(jobs.begin(BackupJobKind::Import).is_err());
        assert!(done.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(!jobs.snapshots()[0].progress.cancel_requested);
        lease.run(|_| Ok(())).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn shutdown_cancels_processing_and_panics_release_the_job_slot() {
        let jobs = BackupJobs::default();
        let lease = jobs.begin(BackupJobKind::Import).unwrap();
        jobs.stop_accepting();
        assert!(lease.job.control.snapshot().cancel_requested);
        drop(lease);
        jobs.shutdown();

        let jobs = BackupJobs::default();
        let result: Result<()> = jobs
            .begin(BackupJobKind::Import)
            .unwrap()
            .run(|_| panic!("test request payload"));
        let failure = result.unwrap_err();
        assert_eq!(failure.code, ErrorCode::Internal);
        assert!(!failure.message.contains("payload"));
        assert_eq!(jobs.snapshots()[0].error, Some(ErrorCode::Internal));
        assert_eq!(jobs.snapshots()[0].progress.phase, BackupPhase::Failed);
        for _ in 0..40 {
            jobs.begin(BackupJobKind::Export)
                .unwrap()
                .run(|_| Ok(()))
                .unwrap();
        }
        assert_eq!(jobs.snapshots().len(), 32);
    }
}
