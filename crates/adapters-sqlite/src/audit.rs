use crate::migration::{migrate, sqlite_error};
use brows_application::{
    AuditEvent, AuditOutcome, AuditSink, PortError, PortErrorKind, PortResult,
};
use rusqlite::{params, Connection, OpenFlags, Row};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

const MAX_ACTION_BYTES: usize = 64;
const MAX_ENTITY_ID_BYTES: usize = 128;

/// Append-only local audit sink backed by the application's SQLite database.
///
/// Events contain only a fixed application action, an opaque entity id, a
/// timestamp and an outcome. Secrets and free-form error messages are outside
/// this port and therefore cannot be persisted by this adapter.
#[derive(Debug)]
pub struct SqliteAuditSink {
    connection: Mutex<Connection>,
}

impl SqliteAuditSink {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error("open audit database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure audit database timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| sqlite_error("enable audit foreign keys", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("enable audit WAL", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted audit schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Returns the newest events first. The limit is capped to keep future IPC
    /// callers from accidentally loading an unbounded audit history.
    pub fn list(&self, limit: usize) -> PortResult<Vec<AuditEvent>> {
        let limit = limit.min(1_000);
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT at_ms, action, entity_id, outcome FROM audit_events \
                 ORDER BY at_ms DESC, id DESC LIMIT ?1",
            )
            .map_err(|error| sqlite_error("prepare audit history", error))?;
        let rows = statement
            .query_map([i64::try_from(limit).unwrap_or(1_000)], from_row)
            .map_err(|error| sqlite_error("query audit history", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error("read audit history", error))
    }

    fn lock(&self) -> PortResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "audit history database is unavailable",
            )
        })
    }
}

impl AuditSink for SqliteAuditSink {
    fn record(&self, event: AuditEvent) -> PortResult<()> {
        validate_event(&event)?;
        let at_ms = i64::try_from(event.at_ms).map_err(|_| {
            PortError::new(PortErrorKind::InvalidData, "audit timestamp is too large")
        })?;
        self.lock()?
            .execute(
                "INSERT INTO audit_events (at_ms, action, entity_id, outcome) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    at_ms,
                    event.action,
                    event.entity_id,
                    outcome_text(event.outcome)
                ],
            )
            .map_err(|error| sqlite_error("record audit event", error))?;
        Ok(())
    }
}

fn validate_event(event: &AuditEvent) -> PortResult<()> {
    if event.action.is_empty() || event.action.len() > MAX_ACTION_BYTES {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "audit action must be 1 to 64 bytes",
        ));
    }
    if !event.action.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'_'
    }) {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "audit action contains unsupported characters",
        ));
    }
    if event.entity_id.is_empty() || event.entity_id.len() > MAX_ENTITY_ID_BYTES {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "audit entity id must be 1 to 128 bytes",
        ));
    }
    if event
        .entity_id
        .chars()
        .any(|character| character.is_control())
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "audit entity id contains control characters",
        ));
    }
    Ok(())
}

fn outcome_text(outcome: AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Succeeded => "succeeded",
        AuditOutcome::Failed => "failed",
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<AuditEvent> {
    let at_ms = row.get::<_, i64>(0)?;
    let outcome = row.get::<_, String>(3)?;
    Ok(AuditEvent {
        at_ms: u64::try_from(at_ms)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, at_ms))?,
        action: row.get(1)?,
        entity_id: row.get(2)?,
        outcome: match outcome.as_str() {
            "succeeded" => AuditOutcome::Succeeded,
            "failed" => AuditOutcome::Failed,
            _ => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid stored audit outcome",
                    )),
                ))
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn audit_events_survive_reopen_and_keep_newest_first() {
        let path = std::env::temp_dir().join(format!(
            "brows-audit-{}-{}.sqlite3",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let sink = SqliteAuditSink::open(&path).unwrap();
            sink.record(AuditEvent {
                at_ms: 10,
                action: "profile.create".to_owned(),
                entity_id: "profile-a".to_owned(),
                outcome: AuditOutcome::Succeeded,
            })
            .unwrap();
            sink.record(AuditEvent {
                at_ms: 20,
                action: "session.launch".to_owned(),
                entity_id: "session-b".to_owned(),
                outcome: AuditOutcome::Failed,
            })
            .unwrap();
        }

        let reopened = SqliteAuditSink::open(&path).unwrap();
        let events = reopened.list(10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, "session.launch");
        assert_eq!(events[0].outcome, AuditOutcome::Failed);
        assert_eq!(events[1].action, "profile.create");
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn audit_event_rejects_free_form_or_control_data() {
        let invalid = AuditEvent {
            at_ms: 1,
            action: "profile.create: secret".to_owned(),
            entity_id: "profile\nsecret".to_owned(),
            outcome: AuditOutcome::Failed,
        };
        assert_eq!(
            validate_event(&invalid).unwrap_err().kind,
            PortErrorKind::InvalidData
        );
    }
}
