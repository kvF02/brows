use crate::{ApplicationError, ApplicationResult, SessionApplicationService, SettingsRepository};
use brows_contracts::{ErrorCode, SettingsDto, SettingsUpdateRequest};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct SettingsApplicationService {
    repository: Arc<dyn SettingsRepository>,
    sessions: Arc<SessionApplicationService>,
    operations: Mutex<()>,
}

impl SettingsApplicationService {
    pub fn new(
        repository: Arc<dyn SettingsRepository>,
        sessions: Arc<SessionApplicationService>,
    ) -> Self {
        Self {
            repository,
            sessions,
            operations: Mutex::new(()),
        }
    }

    pub fn get(&self) -> ApplicationResult<SettingsDto> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| ApplicationError::poisoned("settings"))?;
        let settings = self.repository.load()?;
        settings.validate()?;
        Ok(SettingsDto::from(&settings))
    }

    pub fn update(&self, request: SettingsUpdateRequest) -> ApplicationResult<SettingsDto> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| ApplicationError::poisoned("settings"))?;
        let changes = request.changes;
        if changes.telemetry_enabled == Some(true)
            || changes.stop_sessions_on_exit == Some(false)
            || changes.check_updates == Some(true)
            || changes.retain_archived_days.is_some_and(|days| days != 7)
        {
            return Err(ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                "仅支持修改并发上限；退出时停止会话固定开启，遥测关闭，自动更新和归档清理尚未开放",
            ));
        }
        let mut settings = self.repository.load()?;
        if let Some(limit) = changes.max_concurrent_sessions {
            settings.max_concurrent_sessions = limit as usize;
        }
        settings.validate().map_err(|_| {
            ApplicationError::invalid_field("maxConcurrentSessions", "must be between 1 and 10")
        })?;
        // Session operations stay serialized across persistence and admission.
        // A failed write leaves the live scheduler and its queue untouched.
        self.sessions
            .update_concurrency_limit(settings.max_concurrent_sessions, || {
                Ok(self.repository.save(&settings)?)
            })?;
        Ok(SettingsDto::from(&settings))
    }
}
