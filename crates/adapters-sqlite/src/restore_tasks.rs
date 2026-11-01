use crate::migration::{migrate, sqlite_error};
use brows_application::{
    PortError, PortErrorKind, PortResult, RestoreFailure, RestoreTask, RestoreTaskRepository,
    RestoreTaskStatus,
};
use brows_domain::{ProfileId, ProfileSpec};
use rusqlite::{params, Connection, Row, TransactionBehavior};
use std::{path::Path, sync::Mutex, time::Duration};

const COLUMNS: &str =
    "id, status, profile_id, started_at_ms, completed_at_ms, cleanup_pending, failure_code";

#[derive(Debug)]
pub struct SqliteRestoreTaskRepository {
    connection: Mutex<Connection>,
}

impl SqliteRestoreTaskRepository {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection =
            Connection::open(path).map_err(|e| sqlite_error("open restore journal", e))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|e| sqlite_error("configure restore journal timeout", e))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| sqlite_error("configure restore foreign keys", e))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| sqlite_error("configure restore WAL", e))?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|e| sqlite_error("configure restore durability", e))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|e| sqlite_error("disable trusted restore schema", e))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> PortResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PortError::new(PortErrorKind::Unavailable, "restore journal is unavailable")
        })
    }
}

impl RestoreTaskRepository for SqliteRestoreTaskRepository {
    fn begin(&self, at_ms: u64) -> PortResult<RestoreTask> {
        let connection = self.lock()?;
        connection.execute("INSERT INTO restore_tasks (status, started_at_ms, cleanup_pending) VALUES ('running', ?1, 1)", [integer(at_ms)?])
            .map_err(|e| sqlite_error("begin restore task", e))?;
        connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM restore_tasks WHERE id = ?1"),
                [connection.last_insert_rowid()],
                from_row,
            )
            .map_err(|e| sqlite_error("read new restore task", e))
    }

    fn bind_profile(&self, id: u64, profile: &ProfileId) -> PortResult<()> {
        let changed = self.lock()?.execute("UPDATE restore_tasks SET profile_id = ?2 WHERE id = ?1 AND status = 'running' AND profile_id IS NULL",
            params![integer(id)?, profile.as_str()]).map_err(|e| sqlite_error("bind restore identity", e))?;
        changed_one(changed)
    }

    fn complete(&self, id: u64, profile: &ProfileSpec, at_ms: u64) -> PortResult<()> {
        if !profile.archived {
            return Err(invalid("restore activation requires an archived profile"));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| sqlite_error("begin restore commit", e))?;
        let changed = transaction.execute("UPDATE restore_tasks SET status = 'completed', completed_at_ms = MAX(started_at_ms, ?3) WHERE id = ?1 AND profile_id = ?2 AND status = 'running'",
            params![integer(id)?, profile.id.as_str(), integer(at_ms)?]).map_err(|e| sqlite_error("complete restore journal", e))?;
        changed_one(changed)?;
        let changed = transaction.execute("UPDATE profiles SET archived = 0, revision = ?3 WHERE id = ?1 AND revision = ?2 AND archived = 1",
            params![profile.id.as_str(), integer(profile.revision.get())?, integer(profile.revision.next().get())?])
            .map_err(|e| sqlite_error("activate restored profile", e))?;
        if changed != 1 {
            return Err(PortError::new(
                PortErrorKind::RevisionConflict,
                "restored profile revision changed",
            ));
        }
        transaction
            .commit()
            .map_err(|e| sqlite_error("commit restored profile and task", e))
    }

    fn fail(
        &self,
        id: u64,
        interrupted: bool,
        failure: RestoreFailure,
        at_ms: u64,
    ) -> PortResult<()> {
        if failure == RestoreFailure::Cancelled
            || interrupted != (failure == RestoreFailure::Interrupted)
        {
            return Err(invalid("inconsistent restore outcome"));
        }
        let changed = self.lock()?.execute("UPDATE restore_tasks SET status = ?2, failure_code = ?3, completed_at_ms = MAX(started_at_ms, ?4) WHERE id = ?1 AND status = 'running'",
            params![integer(id)?, if interrupted { "interrupted" } else { "failed" }, failure.as_str(), integer(at_ms)?])
            .map_err(|e| sqlite_error("record restore failure", e))?;
        changed_one(changed)
    }

    fn cancel(&self, id: u64, at_ms: u64) -> PortResult<()> {
        let changed = self.lock()?.execute("UPDATE restore_tasks SET status = 'cancelled', failure_code = 'cancelled', completed_at_ms = MAX(started_at_ms, ?2) WHERE id = ?1 AND status = 'running'", params![integer(id)?, integer(at_ms)?])
            .map_err(|e| sqlite_error("cancel restore task", e))?;
        changed_one(changed)
    }

    fn cleanup_finished(&self, id: u64) -> PortResult<()> {
        let changed = self.lock()?.execute("UPDATE restore_tasks SET cleanup_pending = 0 WHERE id = ?1 AND status != 'running'", [integer(id)?])
            .map_err(|e| sqlite_error("acknowledge restore cleanup", e))?;
        changed_one(changed)
    }

    fn list(&self, limit: usize) -> PortResult<Vec<RestoreTask>> {
        let connection = self.lock()?;
        let mut query = connection
            .prepare(&format!(
                "SELECT {COLUMNS} FROM restore_tasks ORDER BY id DESC LIMIT ?1"
            ))
            .map_err(|e| sqlite_error("prepare restore history", e))?;
        let result = query
            .query_map([limit.min(200) as u32], from_row)
            .map_err(|e| sqlite_error("list restore history", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite_error("read restore history", e));
        result
    }

    fn pending(&self) -> PortResult<Vec<RestoreTask>> {
        let connection = self.lock()?;
        let mut query = connection.prepare(&format!("SELECT {COLUMNS} FROM restore_tasks WHERE status = 'running' OR cleanup_pending = 1 ORDER BY id"))
            .map_err(|e| sqlite_error("prepare pending restores", e))?;
        let result = query
            .query_map([], from_row)
            .map_err(|e| sqlite_error("query pending restores", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite_error("read pending restores", e));
        result
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<RestoreTask> {
    let status: String = row.get(1)?;
    let status = match status.as_str() {
        "cancelled" => RestoreTaskStatus::Cancelled,
        "running" => RestoreTaskStatus::Running,
        "completed" => RestoreTaskStatus::Completed,
        "failed" => RestoreTaskStatus::Failed,
        "interrupted" => RestoreTaskStatus::Interrupted,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let profile: Option<String> = row.get(2)?;
    let failure: Option<String> = row.get(6)?;
    let failure = match failure.as_deref() {
        None => None,
        Some("cancelled") => Some(RestoreFailure::Cancelled),
        Some("wrong_password") => Some(RestoreFailure::WrongPassword),
        Some("tampered") => Some(RestoreFailure::Tampered),
        Some("incompatible") => Some(RestoreFailure::Incompatible),
        Some("unavailable") => Some(RestoreFailure::Unavailable),
        Some("interrupted") => Some(RestoreFailure::Interrupted),
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(RestoreTask {
        id: row.get(0)?,
        status,
        profile_id: profile
            .map(ProfileId::new)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        started_at_ms: row.get(3)?,
        completed_at_ms: row.get(4)?,
        cleanup_pending: row.get(5)?,
        failure,
    })
}

fn integer(value: u64) -> PortResult<i64> {
    i64::try_from(value).map_err(|_| invalid("restore journal integer out of range"))
}
fn invalid(message: &'static str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
fn changed_one(changed: usize) -> PortResult<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(invalid("restore task state changed"))
    }
}
