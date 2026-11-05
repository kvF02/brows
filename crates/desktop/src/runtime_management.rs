use brows_adapters_runtime::ManagedChromiumRuntime;
use brows_adapters_sqlite::SqliteSettingsStore;
use brows_application::{ApplicationResult, SessionApplicationService};
use brows_domain::{BrowserBuild, BrowserBuildId};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct RuntimeManagementService {
    pub(crate) browsers: Arc<ManagedChromiumRuntime>,
    pub(crate) store: Arc<SqliteSettingsStore>,
    pub(crate) sessions: Arc<SessionApplicationService>,
}

impl RuntimeManagementService {
    pub(crate) fn capture(&self, id: BrowserBuildId) -> ApplicationResult<BrowserBuild> {
        Ok(self.browsers.capture_snapshot(&id)?)
    }

    pub(crate) fn activate(&self, id: BrowserBuildId) -> ApplicationResult<()> {
        self.sessions.with_idle_runtime_change(|| {
            self.browsers
                .activate_persisted(&id, || self.store.set_active_runtime(&id))?;
            Ok(())
        })
    }
}
