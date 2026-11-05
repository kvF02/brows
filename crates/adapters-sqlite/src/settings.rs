use crate::migration::{migrate, sqlite_error};
use brows_application::{PortError, PortErrorKind, PortResult};
use brows_contracts::SettingsDto;
use rusqlite::{params, Connection};
use std::{path::Path, sync::Mutex, time::Duration};

/// Only non-secret, implemented preferences are persisted here.
#[derive(Debug)]
pub struct SqliteSettingsStore {
    connection: Mutex<Connection>,
}

impl SqliteSettingsStore {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection = Connection::open(path)
            .map_err(|error| sqlite_error("open settings database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure settings timeout", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("configure settings WAL", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted settings schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn get(&self) -> PortResult<SettingsDto> {
        let connection = self.connection.lock().map_err(|_| unavailable())?;
        connection.query_row(
            "SELECT max_concurrent_sessions, retain_archived_days FROM app_settings WHERE singleton=1",
            [],
            |row| Ok(SettingsDto {
                max_concurrent_sessions: row.get(0)?,
                retain_archived_days: row.get(1)?,
                ..SettingsDto::default()
            }),
        ).map_err(|error| sqlite_error("read settings", error))
    }

    pub fn save(&self, settings: &SettingsDto) -> PortResult<()> {
        settings
            .validate()
            .map_err(|_| PortError::new(PortErrorKind::InvalidData, "invalid settings"))?;
        let connection = self.connection.lock().map_err(|_| unavailable())?;
        connection.execute(
            "UPDATE app_settings SET max_concurrent_sessions=?1, retain_archived_days=?2 WHERE singleton=1",
            params![settings.max_concurrent_sessions, settings.retain_archived_days],
        ).map_err(|error| sqlite_error("save settings", error))?;
        Ok(())
    }
}

fn unavailable() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "settings database is unavailable",
    )
}
