use crate::{
    ApplicationError, ApplicationResult, Clock, LegacyProfileDataMigrator,
    LegacyProfileDataSummary, MigrationRecord, MigrationRecordRepository, MigrationRecordStatus,
    ProfileRepository, SessionApplicationService,
};
use brows_contracts::ErrorCode;
use brows_domain::ProfileId;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

#[derive(Debug)]
pub struct MigrationApplicationService {
    profiles: Arc<dyn ProfileRepository>,
    sessions: Arc<SessionApplicationService>,
    migrator: Arc<dyn LegacyProfileDataMigrator>,
    records: Arc<dyn MigrationRecordRepository>,
    clock: Arc<dyn Clock>,
    operations: Mutex<()>,
    blocked: Mutex<BTreeMap<ProfileId, crate::ProfileReservation>>,
    recovery_warning: AtomicBool,
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
            blocked: Mutex::new(BTreeMap::new()),
            recovery_warning: AtomicBool::new(false),
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
        let _reservation = self.sessions.reserve_stopped_profile(&profile_id)?;
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
        let reservation = self.sessions.reserve_stopped_profile(&profile_id)?;
        let inspection = self.migrator.inspect(&profile_id, &source_profile_id)?;
        if inspection.source_may_be_running {
            return Err(ApplicationError::new(
                ErrorCode::ProfileLocked,
                "MoreLogin source environment appears to be running",
            ));
        }
        let started_at_ms = self.clock.now_ms();
        let running =
            match self
                .records
                .begin(&profile_id, "morelogin", &source_profile_id, started_at_ms)
            {
                Ok(record) => record,
                Err(error) => {
                    self.hold(profile_id, reservation)?;
                    return Err(error.into());
                }
            };
        let operation_key = format!("migration-{}", running.id);
        let attempt = self
            .migrator
            .migrate(&profile_id, &source_profile_id, &operation_key)
            .map_err(ApplicationError::from)
            .and_then(|artifact| {
                Ok(self
                    .records
                    .complete(running.id, self.clock.now_ms(), &artifact)?)
            });
        match attempt {
            Ok(record) => Ok(record),
            Err(error) => {
                // A repository may have committed before a later read failed.
                // Never undo data belonging to an acknowledged completed record.
                match self.records.get(running.id) {
                    Ok(record) if record.status == MigrationRecordStatus::Completed => Ok(record),
                    Ok(record) => {
                        if self.recover_record(&record).is_err() {
                            self.hold(profile_id, reservation)?;
                        }
                        Err(error)
                    }
                    Err(_) => {
                        self.hold(profile_id, reservation)?;
                        Err(error)
                    }
                }
            }
        }
    }

    pub fn rollback(&self, id: u64) -> ApplicationResult<MigrationRecord> {
        let _operation = self.lock_operations()?;
        let record = self.records.get(id)?;
        if record.status != MigrationRecordStatus::Completed || record.recovery_pending {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "only completed migrations without pending recovery can be rolled back",
            ));
        }
        if self
            .records
            .has_newer_commit(&record.profile_id, record.id)?
        {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "a newer committed migration must be inspected before rolling back this record",
            ));
        }
        let reservation = self.sessions.reserve_stopped_profile(&record.profile_id)?;
        let backup_key = record.backup_key.as_deref().ok_or_else(|| {
            ApplicationError::new(ErrorCode::InvalidProfileState, "migration has no backup")
        })?;
        // Persist intent before either filesystem rename.
        if let Err(error) = self.records.begin_rollback(id) {
            self.hold(record.profile_id, reservation)?;
            return Err(error.into());
        }
        let attempt = self
            .migrator
            .rollback(&record.profile_id, backup_key)
            .map_err(ApplicationError::from)
            .and_then(|()| Ok(self.records.mark_rolled_back(id, self.clock.now_ms())?));
        match attempt {
            Ok(record) => Ok(record),
            Err(error) => {
                if let Ok(finished) = self.records.get(id) {
                    if finished.status == MigrationRecordStatus::RolledBack {
                        return Ok(finished);
                    }
                }
                self.hold(record.profile_id, reservation)?;
                Err(error)
            }
        }
    }

    pub fn recovery_pending(&self) -> bool {
        self.recovery_warning.load(Ordering::Acquire)
    }

    /// Startup runs under the data-root lock, before sessions can launch.
    /// A manual retry reuses reservations retained by previous failed recovery.
    pub fn recover_incomplete(&self) -> ApplicationResult<bool> {
        let _operation = self.lock_operations()?;
        let pending = self.records.pending()?;
        let mut groups = BTreeMap::<ProfileId, Vec<MigrationRecord>>::new();
        for record in pending {
            groups
                .entry(record.profile_id.clone())
                .or_default()
                .push(record);
        }
        // A failed acknowledgement may have committed. Release only after a
        // fresh durable query confirms that profile has no pending operation.
        self.blocked
            .lock()
            .map_err(|_| ApplicationError::poisoned("migration recovery"))?
            .retain(|id, _| groups.contains_key(id));
        let mut blocked = false;
        for (id, mut records) in groups {
            records.sort_by_key(|record| std::cmp::Reverse(record.id));
            let held = self
                .blocked
                .lock()
                .map_err(|_| ApplicationError::poisoned("migration recovery"))?
                .remove(&id);
            let reservation = match held
                .map(Ok)
                .unwrap_or_else(|| self.sessions.reserve_stopped_profile(&id))
            {
                Ok(reservation) => reservation,
                Err(_) => {
                    blocked = true;
                    continue;
                }
            };
            let mut failed = false;
            for record in records {
                if self.recover_record(&record).is_err() {
                    failed = true;
                }
            }
            if failed {
                self.hold(id, reservation)?;
                blocked = true;
            }
        }
        self.recovery_warning.store(blocked, Ordering::Release);
        Ok(blocked)
    }

    fn recover_record(&self, record: &MigrationRecord) -> ApplicationResult<()> {
        // Old versions could admit another migration after a failed journal.
        // Never rewrite userdata owned by a later committed operation.
        if self
            .records
            .has_newer_commit(&record.profile_id, record.id)?
        {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "historical migration overlaps a newer committed version; inspection required",
            ));
        }
        if record.rollback_pending {
            if record.status != MigrationRecordStatus::Completed {
                return Err(ApplicationError::new(
                    ErrorCode::InvalidProfileState,
                    "inconsistent rollback journal",
                ));
            }
            let key = record.backup_key.as_deref().ok_or_else(|| {
                ApplicationError::new(
                    ErrorCode::InvalidProfileState,
                    "pending rollback has no backup",
                )
            })?;
            self.migrator.rollback(&record.profile_id, key)?;
            self.records
                .mark_rolled_back(record.id, self.clock.now_ms())?;
        } else {
            if !matches!(
                record.status,
                MigrationRecordStatus::Running | MigrationRecordStatus::Failed
            ) {
                return Err(ApplicationError::new(
                    ErrorCode::InvalidProfileState,
                    "inconsistent migration journal",
                ));
            }
            self.migrator
                .recover(&record.profile_id, &format!("migration-{}", record.id))?;
            if record.status == MigrationRecordStatus::Running {
                self.records.fail(
                    record.id,
                    self.clock.now_ms(),
                    "migration did not complete; previous data restored",
                )?;
            }
            self.records.recovery_finished(record.id)?;
        }
        Ok(())
    }

    fn hold(&self, id: ProfileId, reservation: crate::ProfileReservation) -> ApplicationResult<()> {
        self.blocked
            .lock()
            .map_err(|_| ApplicationError::poisoned("migration recovery"))?
            .insert(id, reservation);
        self.recovery_warning.store(true, Ordering::Release);
        Ok(())
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
