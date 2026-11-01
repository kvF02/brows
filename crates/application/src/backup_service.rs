use crate::{
    ApplicationError, ApplicationResult, BackupExportSpec, BackupImportSpec, BackupTaskControl,
    Clock, ProfileApplicationService, RestorableBackupService, RestoreFailure, RestoreTask,
    RestoreTaskRepository, RestoreTaskStatus, SecretValue, SessionApplicationService,
};
use brows_contracts::{ProfileDto, ProfileIdRequest};
use brows_domain::ProfileId;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// Coordinates profile ownership with archive I/O. Transport handlers only map DTOs.
#[derive(Debug)]
pub struct BackupApplicationService {
    profiles: Arc<ProfileApplicationService>,
    sessions: Arc<SessionApplicationService>,
    archives: Arc<dyn RestorableBackupService>,
    tasks: Arc<dyn RestoreTaskRepository>,
    clock: Arc<dyn Clock>,
    restore_operation: Mutex<()>,
    cleanup_warning: AtomicBool,
}

impl BackupApplicationService {
    pub fn new(
        profiles: Arc<ProfileApplicationService>,
        sessions: Arc<SessionApplicationService>,
        archives: Arc<dyn RestorableBackupService>,
        tasks: Arc<dyn RestoreTaskRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            profiles,
            sessions,
            archives,
            tasks,
            clock,
            restore_operation: Mutex::new(()),
            cleanup_warning: AtomicBool::new(false),
        }
    }

    pub fn export(
        &self,
        id: ProfileId,
        destination_name: String,
        password: SecretValue,
        include_proxy_credential: bool,
    ) -> ApplicationResult<ProfileDto> {
        self.export_controlled(
            id,
            destination_name,
            password,
            include_proxy_credential,
            &BackupTaskControl::new(),
        )
    }

    pub fn export_controlled(
        &self,
        id: ProfileId,
        destination_name: String,
        password: SecretValue,
        include_proxy_credential: bool,
        control: &BackupTaskControl,
    ) -> ApplicationResult<ProfileDto> {
        control.checkpoint()?;
        let _reservation = self.sessions.reserve_stopped_profile(&id)?;
        let profile = self.profiles.get(ProfileIdRequest {
            profile_id: (&id).into(),
        })?;
        self.archives.export_controlled(
            &BackupExportSpec {
                profile: profile.clone().try_into()?,
                destination_name,
                password,
                include_proxy_credential,
            },
            control,
        )?;
        Ok(profile)
    }

    pub fn import(&self, spec: BackupImportSpec) -> ApplicationResult<ProfileDto> {
        self.import_controlled(spec, &BackupTaskControl::new())
    }

    pub fn import_controlled(
        &self,
        spec: BackupImportSpec,
        control: &BackupTaskControl,
    ) -> ApplicationResult<ProfileDto> {
        control.checkpoint()?;
        let _operation = self
            .restore_operation
            .lock()
            .map_err(|_| ApplicationError::poisoned("restore"))?;
        control.checkpoint()?;
        let task = self.tasks.begin(self.clock.now_ms())?;
        control.attach_restore(task.id);
        let result = (|| -> ApplicationResult<ProfileDto> {
            let payload = self.archives.import_controlled(&spec, task.id, control)?;
            control.begin_commit()?;
            self.profiles.create_imported_journaled(
                payload.profile,
                |id| Ok(self.tasks.bind_profile(task.id, id)?),
                |profile| {
                    self.archives
                        .install_restored_data(&profile.id, &payload.restored_data_dir)?;
                    Ok(self.tasks.complete(task.id, profile, self.clock.now_ms())?)
                },
            )
        })();
        control.begin_cleanup();
        if let Err(error) = &result {
            if error.code() == &brows_contracts::ErrorCode::OperationCancelled {
                let _ = self.tasks.cancel(task.id, self.clock.now_ms());
            } else {
                let failure = match error.code() {
                    brows_contracts::ErrorCode::BackupWrongPassword => {
                        RestoreFailure::WrongPassword
                    }
                    brows_contracts::ErrorCode::BackupTampered => RestoreFailure::Tampered,
                    brows_contracts::ErrorCode::BackupIncompatible
                    | brows_contracts::ErrorCode::ValidationFailed
                    | brows_contracts::ErrorCode::RuntimeUntrusted
                    | brows_contracts::ErrorCode::NotFound => RestoreFailure::Incompatible,
                    _ => RestoreFailure::Unavailable,
                };
                // A journal failure leaves a pending record for restart recovery.
                let _ = self
                    .tasks
                    .fail(task.id, false, failure, self.clock.now_ms());
            }
        }
        if self
            .archives
            .cleanup_task(task.id)
            .and_then(|()| self.tasks.cleanup_finished(task.id))
            .is_err()
        {
            self.cleanup_warning.store(true, Ordering::Release);
        }
        result
    }

    pub fn cleanup_pending(&self) -> bool {
        self.cleanup_warning.load(Ordering::Acquire)
    }

    pub fn restore_tasks(&self) -> ApplicationResult<Vec<RestoreTask>> {
        Ok(self.tasks.list(100)?)
    }

    /// Called at startup while the manager's data-root lock is held, or for a
    /// retry under this same restore mutex. Never resumes without a password.
    pub fn recover_incomplete(&self) -> ApplicationResult<bool> {
        let _operation = self
            .restore_operation
            .lock()
            .map_err(|_| ApplicationError::poisoned("restore"))?;
        let result = (|| -> ApplicationResult<bool> {
            let mut pending_cleanup = false;
            for task in self.tasks.pending()? {
                if task.status == RestoreTaskStatus::Running {
                    self.tasks.fail(
                        task.id,
                        true,
                        RestoreFailure::Interrupted,
                        self.clock.now_ms(),
                    )?;
                }
                if self.archives.cleanup_task(task.id).is_ok() {
                    self.tasks.cleanup_finished(task.id)?;
                } else {
                    pending_cleanup = true;
                }
            }
            pending_cleanup |= self.archives.cleanup_stale_staging()?;
            Ok(pending_cleanup)
        })();
        self.cleanup_warning
            .store(!matches!(result, Ok(false)), Ordering::Release);
        result
    }
}
