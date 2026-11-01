use crate::migration::{migrate, sqlite_error};
use brows_application::{PortError, PortErrorKind, PortResult, SettingsRepository};
use brows_domain::ManagerSettings;
use rusqlite::{params, Connection};
use std::{path::Path, sync::Mutex, time::Duration};

#[derive(Debug)]
pub struct SqliteSettingsRepository {
    connection: Mutex<Connection>,
}

impl SqliteSettingsRepository {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let mut connection = Connection::open(path)
            .map_err(|error| sqlite_error("open settings database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure settings timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| sqlite_error("enable settings foreign keys", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("enable settings WAL", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted settings schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

impl SettingsRepository for SqliteSettingsRepository {
    fn load(&self) -> PortResult<ManagerSettings> {
        let connection = self.connection.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "settings database is unavailable",
            )
        })?;
        let limit: u32 = connection
            .query_row(
                "SELECT max_concurrent_sessions FROM manager_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_error("read settings", error))?;
        let settings = ManagerSettings {
            max_concurrent_sessions: limit as usize,
        };
        settings
            .validate()
            .map_err(|_| PortError::new(PortErrorKind::InvalidData, "invalid saved settings"))?;
        Ok(settings)
    }

    fn save(&self, settings: &ManagerSettings) -> PortResult<()> {
        settings
            .validate()
            .map_err(|_| PortError::new(PortErrorKind::InvalidData, "invalid settings"))?;
        let connection = self.connection.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "settings database is unavailable",
            )
        })?;
        let changed = connection
            .execute(
                "UPDATE manager_settings SET max_concurrent_sessions = ?1 WHERE id = 1",
                params![settings.max_concurrent_sessions as u32],
            )
            .map_err(|error| sqlite_error("save settings", error))?;
        if changed != 1 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "saved settings record is missing",
            ));
        }
        Ok(())
    }
}
