use brows_adapters_sqlite::SqliteSettingsStore;
use brows_application::{ApplicationError, ApplicationResult, SessionApplicationService};
use brows_contracts::{ErrorCode, SettingsDto, SettingsPatchDto};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct SettingsService {
    store: Arc<SqliteSettingsStore>,
    sessions: Arc<SessionApplicationService>,
    operations: Mutex<()>,
}

impl SettingsService {
    pub(crate) fn new(store: Arc<SqliteSettingsStore>, sessions: Arc<SessionApplicationService>) -> Self {
        Self { store, sessions, operations: Mutex::new(()) }
    }

    pub(crate) fn get(&self) -> ApplicationResult<SettingsDto> {
        Ok(self.store.get()?)
    }

    pub(crate) fn update(&self, patch: SettingsPatchDto) -> ApplicationResult<SettingsDto> {
        let _operation = self.operations.lock().map_err(|_| ApplicationError::new(ErrorCode::Internal, "settings state unavailable"))?;
        let current = self.store.get()?;
        let next = patch.apply(&current)?;
        if current.max_concurrent_sessions != next.max_concurrent_sessions {
            self.sessions.configure_idle_limit(next.max_concurrent_sessions as usize, || Ok(self.store.save(&next)?))?;
        } else {
            self.store.save(&next)?;
        }
        Ok(next)
    }
}
