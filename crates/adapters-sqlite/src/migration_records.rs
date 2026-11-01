use crate::migration::{migrate, sqlite_error};
use brows_application::{
    LegacyMigrationArtifact, MigrationRecord, MigrationRecordRepository, MigrationRecordStatus,
    PortError, PortErrorKind, PortResult,
};
use brows_domain::ProfileId;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug)]
pub struct SqliteMigrationRecordRepository {
    connection: Mutex<Connection>,
}

impl SqliteMigrationRecordRepository {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error("open migration database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure migration database timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| sqlite_error("enable migration foreign keys", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("enable migration WAL", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted migration schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> PortResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "migration history database is unavailable",
            )
        })
    }
}

impl MigrationRecordRepository for SqliteMigrationRecordRepository {
    fn begin(
        &self,
        profile_id: &ProfileId,
        source_kind: &str,
        source_profile_id: &str,
        started_at_ms: u64,
    ) -> PortResult<MigrationRecord> {
        let started = to_i64(started_at_ms, "started_at_ms")?;
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO migration_records (profile_id, source_kind, source_profile_id, status, started_at_ms, recovery_pending) VALUES (?1, ?2, ?3, 'running', ?4, 1)",
            params![profile_id.as_str(), source_kind, source_profile_id, started],
        ).map_err(|error| sqlite_error("create migration record", error))?;
        let id = u64::try_from(connection.last_insert_rowid())
            .map_err(|_| PortError::new(PortErrorKind::Internal, "migration id overflow"))?;
        get_with_connection(&connection, id)
    }

    fn complete(
        &self,
        id: u64,
        completed_at_ms: u64,
        artifact: &LegacyMigrationArtifact,
    ) -> PortResult<MigrationRecord> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE migration_records SET status='completed', completed_at_ms=MAX(started_at_ms, ?2), copied_files=?3, copied_bytes=?4, backup_key=?5, error_summary=NULL, recovery_pending=0 WHERE id=?1 AND status='running' AND recovery_pending=1 AND rollback_pending=0",
            params![to_i64(id, "id")?, to_i64(completed_at_ms, "completed_at_ms")?, to_i64(artifact.copied_files, "copied_files")?, to_i64(artifact.copied_bytes, "copied_bytes")?, artifact.backup_key],
        ).map_err(|error| sqlite_error("complete migration record", error))?;
        ensure_changed(changed)?;
        get_with_connection(&connection, id)
    }

    fn fail(&self, id: u64, completed_at_ms: u64, summary: &str) -> PortResult<MigrationRecord> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE migration_records SET status='failed', completed_at_ms=MAX(started_at_ms, ?2), error_summary=?3 WHERE id=?1 AND status='running'",
            params![to_i64(id, "id")?, to_i64(completed_at_ms, "completed_at_ms")?, summary],
        ).map_err(|error| sqlite_error("fail migration record", error))?;
        ensure_changed(changed)?;
        get_with_connection(&connection, id)
    }

    fn has_newer_commit(&self, profile_id: &ProfileId, id: u64) -> PortResult<bool> {
        self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM migration_records WHERE profile_id=?1 AND id>?2 AND status IN ('completed', 'rolled_back'))", params![profile_id.as_str(), to_i64(id, "id")?], |row| row.get(0))
            .map_err(|error| sqlite_error("check newer migration commits", error))
    }

    fn begin_rollback(&self, id: u64) -> PortResult<MigrationRecord> {
        let connection = self.lock()?;
        let changed = connection.execute("UPDATE migration_records SET recovery_pending=1, rollback_pending=1 WHERE id=?1 AND status='completed' AND recovery_pending=0 AND rollback_pending=0", [to_i64(id, "id")?])
            .map_err(|error| sqlite_error("journal migration rollback", error))?;
        ensure_changed(changed)?;
        get_with_connection(&connection, id)
    }

    fn pending(&self) -> PortResult<Vec<MigrationRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT id, profile_id, source_kind, source_profile_id, status, started_at_ms, completed_at_ms, copied_files, copied_bytes, backup_key, error_summary, recovery_pending, rollback_pending FROM migration_records WHERE recovery_pending=1 ORDER BY id")
            .map_err(|error| sqlite_error("prepare migration recovery", error))?;
        let rows = statement
            .query_map([], from_row)
            .map_err(|error| sqlite_error("query migration recovery", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("read migration recovery", error))
    }

    fn recovery_finished(&self, id: u64) -> PortResult<()> {
        let changed = self.lock()?.execute("UPDATE migration_records SET recovery_pending=0 WHERE id=?1 AND status='failed' AND rollback_pending=0", [to_i64(id, "id")?])
            .map_err(|error| sqlite_error("finish migration recovery", error))?;
        ensure_changed(changed)
    }

    fn mark_rolled_back(&self, id: u64, completed_at_ms: u64) -> PortResult<MigrationRecord> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE migration_records SET status='rolled_back', completed_at_ms=MAX(started_at_ms, ?2), recovery_pending=0, rollback_pending=0 WHERE id=?1 AND status='completed' AND rollback_pending=1",
            params![to_i64(id, "id")?, to_i64(completed_at_ms, "completed_at_ms")?],
        ).map_err(|error| sqlite_error("mark migration rolled back", error))?;
        ensure_changed(changed)?;
        get_with_connection(&connection, id)
    }

    fn get(&self, id: u64) -> PortResult<MigrationRecord> {
        let connection = self.lock()?;
        get_with_connection(&connection, id)
    }

    fn list(&self, limit: usize) -> PortResult<Vec<MigrationRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, profile_id, source_kind, source_profile_id, status, started_at_ms, completed_at_ms, copied_files, copied_bytes, backup_key, error_summary, recovery_pending, rollback_pending FROM migration_records ORDER BY started_at_ms DESC, id DESC LIMIT ?1"
        ).map_err(|error| sqlite_error("prepare migration history", error))?;
        let rows = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], from_row)
            .map_err(|error| sqlite_error("query migration history", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("read migration history", error))
    }
}

fn get_with_connection(connection: &Connection, id: u64) -> PortResult<MigrationRecord> {
    connection.query_row(
        "SELECT id, profile_id, source_kind, source_profile_id, status, started_at_ms, completed_at_ms, copied_files, copied_bytes, backup_key, error_summary, recovery_pending, rollback_pending FROM migration_records WHERE id=?1",
        [to_i64(id, "id")?], from_row,
    ).optional().map_err(|error| sqlite_error("get migration record", error))?
        .ok_or_else(|| PortError::not_found("migration record"))
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<MigrationRecord> {
    let status: String = row.get(4)?;
    Ok(MigrationRecord {
        id: u64::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
        profile_id: ProfileId::new(row.get::<_, String>(1)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        recovery_pending: row.get(11)?,
        rollback_pending: row.get(12)?,
        source_kind: row.get(2)?,
        source_profile_id: row.get(3)?,
        status: parse_status(&status).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        started_at_ms: from_i64(row.get(5)?)?,
        completed_at_ms: row.get::<_, Option<i64>>(6)?.map(from_i64).transpose()?,
        copied_files: row.get::<_, Option<i64>>(7)?.map(from_i64).transpose()?,
        copied_bytes: row.get::<_, Option<i64>>(8)?.map(from_i64).transpose()?,
        backup_key: row.get(9)?,
        error_summary: row.get(10)?,
    })
}

fn parse_status(value: &str) -> Result<MigrationRecordStatus, PortError> {
    match value {
        "running" => Ok(MigrationRecordStatus::Running),
        "completed" => Ok(MigrationRecordStatus::Completed),
        "failed" => Ok(MigrationRecordStatus::Failed),
        "rolled_back" => Ok(MigrationRecordStatus::RolledBack),
        _ => Err(PortError::new(
            PortErrorKind::InvalidData,
            "invalid stored migration status",
        )),
    }
}

fn to_i64(value: u64, field: &'static str) -> PortResult<i64> {
    i64::try_from(value)
        .map_err(|_| PortError::new(PortErrorKind::InvalidData, format!("{field} is too large")))
}

fn from_i64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

fn ensure_changed(changed: usize) -> PortResult<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(PortError::new(
            PortErrorKind::RevisionConflict,
            "migration record state changed",
        ))
    }
}
