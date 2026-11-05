use crate::{
    ApplicationError, ApplicationResult, Clock, LegacyProfileDataMigrator,
    LegacyProfileDataSummary, MigrationRecord, MigrationRecordRepository, MigrationRecordStatus,
    ProfileRepository, SessionApplicationService,
};
use brows_contracts::ErrorCode;
use brows_domain::ProfileId;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug)]
pub struct MigrationApplicationService {
    profiles: Arc<dyn ProfileRepository>,
    sessions: Arc<SessionApplicationService>,
    migrator: Arc<dyn LegacyProfileDataMigrator>,
    records: Arc<dyn MigrationRecordRepository>,
    clock: Arc<dyn Clock>,
    operations: Mutex<()>,
}

impl MigrationApplicationService {
    pub fn new(
        profiles: Arc<dyn ProfileRepository>,
        sessions: Arc<SessionApplicationService>,
        migrator: Arc<dyn LegacyProfileDataMigrator>,
        records: Arc<dyn MigrationRecordRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            profiles,
            sessions,
            migrator,
            records,
            clock,
            operations: Mutex::new(()),
        }
    }

    pub fn inspect(
        &self,
        profile_id: ProfileId,
        source_profile_id: String,
    ) -> ApplicationResult<LegacyProfileDataSummary> {
        let _operation = self.lock_operations()?;
        validate_source_id(&source_profile_id)?;
        self.profiles.get(&profile_id)?;
        let _profile_lease = self.sessions.reserve_stopped_profile(profile_id.clone())?;
        Ok(self.migrator.inspect(&profile_id, &source_profile_id)?)
    }

    pub fn migrate(
        &self,
        profile_id: ProfileId,
        source_profile_id: String,
    ) -> ApplicationResult<MigrationRecord> {
        let _operation = self.lock_operations()?;
        validate_source_id(&source_profile_id)?;
        self.profiles.get(&profile_id)?;
        let _profile_lease = self.sessions.reserve_stopped_profile(profile_id.clone())?;
        let inspection = self.migrator.inspect(&profile_id, &source_profile_id)?;
        if inspection.source_may_be_running {
            return Err(ApplicationError::new(
                ErrorCode::ProfileLocked,
                "MoreLogin source environment appears to be running",
            ));
        }
        let started_at_ms = self.clock.now_ms();
        let running =
            self.records
                .begin(&profile_id, "morelogin", &source_profile_id, started_at_ms)?;
        let operation_key = format!("migration-{}", running.id);
        match self
            .migrator
            .migrate(&profile_id, &source_profile_id, &operation_key)
        {
            Ok(artifact) => {
                Ok(self
                    .records
                    .complete(running.id, self.clock.now_ms(), &artifact)?)
            }
            Err(error) => {
                let summary = public_failure_summary(error.kind);
                let _ = self.records.fail(running.id, self.clock.now_ms(), summary);
                Err(error.into())
            }
        }
    }

    pub fn rollback(&self, id: u64) -> ApplicationResult<MigrationRecord> {
        let _operation = self.lock_operations()?;
        let record = self.records.get(id)?;
        if record.status != MigrationRecordStatus::Completed {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "only completed migrations can be rolled back",
            ));
        }
        let _profile_lease = self
            .sessions
            .reserve_stopped_profile(record.profile_id.clone())?;
        let backup_key = record.backup_key.as_deref().ok_or_else(|| {
            ApplicationError::new(ErrorCode::InvalidProfileState, "migration has no backup")
        })?;
        self.migrator.rollback(&record.profile_id, backup_key)?;
        Ok(self.records.mark_rolled_back(id, self.clock.now_ms())?)
    }

    pub fn list(&self, limit: usize) -> ApplicationResult<Vec<MigrationRecord>> {
        if limit == 0 || limit > 200 {
            return Err(ApplicationError::invalid_field(
                "limit",
                "must be between 1 and 200",
            ));
        }
        Ok(self.records.list(limit)?)
    }

    fn lock_operations(&self) -> ApplicationResult<MutexGuard<'_, ()>> {
        self.operations
            .lock()
            .map_err(|_| ApplicationError::poisoned("migration operation"))
    }
}

fn validate_source_id(value: &str) -> ApplicationResult<()> {
    if value.len() < 8 || value.len() > 32 || !value.bytes().all(|value| value.is_ascii_digit()) {
        return Err(ApplicationError::invalid_field(
            "sourceProfileId",
            "must be a numeric MoreLogin profile identifier",
        ));
    }
    Ok(())
}

fn public_failure_summary(kind: crate::PortErrorKind) -> &'static str {
    use crate::PortErrorKind;
    match kind {
        PortErrorKind::Locked | PortErrorKind::RuntimeInUse => "source or target was in use",
        PortErrorKind::InvalidData | PortErrorKind::Incompatible => "source data was incompatible",
        PortErrorKind::Unavailable => "copy operation was unavailable",
        _ => "migration failed",
    }
}
