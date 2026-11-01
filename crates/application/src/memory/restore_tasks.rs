use super::{unavailable, InMemoryProfileRepository};
use crate::{
    PortError, PortErrorKind, PortResult, RestoreFailure, RestoreTask, RestoreTaskRepository,
    RestoreTaskStatus,
};
use brows_domain::{ProfileId, ProfileSpec};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub struct InMemoryRestoreTaskRepository {
    tasks: Mutex<BTreeMap<u64, RestoreTask>>,
    profiles: Arc<InMemoryProfileRepository>,
}

impl InMemoryRestoreTaskRepository {
    pub fn new(profiles: Arc<InMemoryProfileRepository>) -> Self {
        Self {
            tasks: Mutex::new(BTreeMap::new()),
            profiles,
        }
    }
}

impl RestoreTaskRepository for InMemoryRestoreTaskRepository {
    fn begin(&self, at_ms: u64) -> PortResult<RestoreTask> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?;
        let id = tasks.last_key_value().map_or(1, |(id, _)| id + 1);
        let task = RestoreTask {
            id,
            status: RestoreTaskStatus::Running,
            profile_id: None,
            started_at_ms: at_ms,
            completed_at_ms: None,
            cleanup_pending: true,
            failure: None,
        };
        tasks.insert(id, task.clone());
        Ok(task)
    }
    fn bind_profile(&self, id: u64, profile: &ProfileId) -> PortResult<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?;
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| PortError::not_found("restore task"))?;
        if task.status != RestoreTaskStatus::Running || task.profile_id.is_some() {
            return Err(invalid());
        }
        task.profile_id = Some(profile.clone());
        Ok(())
    }
    fn complete(&self, id: u64, profile: &ProfileSpec, at_ms: u64) -> PortResult<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?;
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| PortError::not_found("restore task"))?;
        if task.status != RestoreTaskStatus::Running
            || task.profile_id.as_ref() != Some(&profile.id)
            || !profile.archived
        {
            return Err(invalid());
        }
        let mut profiles = self
            .profiles
            .values
            .write()
            .map_err(|_| unavailable("profiles"))?;
        let saved = profiles
            .get_mut(&profile.id)
            .ok_or_else(|| PortError::not_found("restored profile"))?;
        if !saved.archived || saved.revision != profile.revision {
            return Err(PortError::new(
                PortErrorKind::RevisionConflict,
                "restored profile changed",
            ));
        }
        saved.archived = false;
        saved.bump_revision();
        task.status = RestoreTaskStatus::Completed;
        task.completed_at_ms = Some(at_ms.max(task.started_at_ms));
        Ok(())
    }
    fn fail(
        &self,
        id: u64,
        interrupted: bool,
        failure: RestoreFailure,
        at_ms: u64,
    ) -> PortResult<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?;
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| PortError::not_found("restore task"))?;
        if task.status != RestoreTaskStatus::Running
            || interrupted != (failure == RestoreFailure::Interrupted)
        {
            return Err(invalid());
        }
        task.status = if interrupted {
            RestoreTaskStatus::Interrupted
        } else {
            RestoreTaskStatus::Failed
        };
        task.failure = Some(failure);
        task.completed_at_ms = Some(at_ms.max(task.started_at_ms));
        Ok(())
    }
    fn cleanup_finished(&self, id: u64) -> PortResult<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?;
        let task = tasks
            .get_mut(&id)
            .ok_or_else(|| PortError::not_found("restore task"))?;
        if task.status == RestoreTaskStatus::Running {
            return Err(invalid());
        }
        task.cleanup_pending = false;
        Ok(())
    }
    fn list(&self, limit: usize) -> PortResult<Vec<RestoreTask>> {
        Ok(self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?
            .values()
            .rev()
            .take(limit.min(200))
            .cloned()
            .collect())
    }
    fn pending(&self) -> PortResult<Vec<RestoreTask>> {
        Ok(self
            .tasks
            .lock()
            .map_err(|_| unavailable("restore tasks"))?
            .values()
            .filter(|t| t.status == RestoreTaskStatus::Running || t.cleanup_pending)
            .cloned()
            .collect())
    }
}

fn invalid() -> PortError {
    PortError::new(PortErrorKind::InvalidData, "restore task state changed")
}
