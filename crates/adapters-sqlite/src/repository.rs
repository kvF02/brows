use crate::migration::{migrate, sqlite_error};
use brows_application::{PortError, PortErrorKind, PortResult, ProfileQuery, ProfileRepository};
use brows_domain::{
    BrowserBuildId, BrowserBuildSelector, BrowserChannel, EnvironmentPresetRef, FingerprintPersona,
    LocaleConfig, PresetId, ProfileId, ProfileSpec, ProxyId, Revision, SystemNativePersona,
    WindowConfig,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, Transaction};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

const PROFILE_COLUMNS: &str = r#"
    id, name, preset_id, preset_version, proxy_id,
    browser_selector_kind, browser_build_id, browser_channel,
    persona_kind, preserve_system_timezone, preserve_system_fonts,
    preserve_system_gpu, preserve_system_audio, timezone,
    window_width, window_height, start_maximized, revision, archived
"#;

#[derive(Debug)]
pub struct SqliteProfileRepository {
    connection: Mutex<Connection>,
}

impl SqliteProfileRepository {
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "database path must be non-empty",
            ));
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite_error("open profile database", error))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| sqlite_error("configure database busy timeout", error))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| sqlite_error("enable foreign keys", error))?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| sqlite_error("enable WAL journal mode", error))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| sqlite_error("configure synchronous mode", error))?;
        connection
            .pragma_update(None, "trusted_schema", "OFF")
            .map_err(|error| sqlite_error("disable trusted schema", error))?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn schema_version(&self) -> PortResult<u32> {
        self.lock()?
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| sqlite_error("read schema version", error))
    }

    pub fn journal_mode(&self) -> PortResult<String> {
        self.lock()?
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(|error| sqlite_error("read journal mode", error))
    }

    fn lock(&self) -> PortResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Unavailable,
                "profile database connection is unavailable",
            )
        })
    }
}

impl ProfileRepository for SqliteProfileRepository {
    fn list(&self, query: &ProfileQuery) -> PortResult<Vec<ProfileSpec>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.lock()?;
        let search = query.search.as_ref().and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| literal_like_pattern(&trimmed.to_ascii_lowercase()))
        });
        let sql = format!(
            r#"
            SELECT {PROFILE_COLUMNS}
            FROM profiles AS p
            WHERE (?1 OR p.archived = 0)
              AND (
                    ?2 IS NULL
                    OR lower(p.name) LIKE ?2 ESCAPE '\'
                    OR EXISTS (
                        SELECT 1 FROM profile_tags AS t
                        WHERE t.profile_id = p.id
                          AND t.tag_normalized LIKE ?2 ESCAPE '\'
                    )
              )
            ORDER BY p.name COLLATE NOCASE ASC, p.id ASC
            LIMIT ?3 OFFSET ?4
            "#
        );
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(query.offset).unwrap_or(i64::MAX);
        let records = {
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| sqlite_error("prepare profile list query", error))?;
            let rows = statement
                .query_map(
                    params![query.include_archived, search.as_deref(), limit, offset],
                    StoredProfile::from_row,
                )
                .map_err(|error| sqlite_error("query profiles", error))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| sqlite_error("decode profile rows", error))?
        };
        records
            .into_iter()
            .map(|record| record.into_domain(&connection))
            .collect()
    }

    fn get(&self, id: &ProfileId) -> PortResult<ProfileSpec> {
        let connection = self.lock()?;
        let sql = format!("SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?1");
        let record = connection
            .query_row(&sql, [id.as_str()], StoredProfile::from_row)
            .optional()
            .map_err(|error| sqlite_error("query profile", error))?
            .ok_or_else(|| PortError::not_found("profile"))?;
        record.into_domain(&connection)
    }

    fn create(&self, profile: ProfileSpec) -> PortResult<()> {
        validate_profile(&profile)?;
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error("start profile create transaction", error))?;
        if profile_exists(&transaction, &profile.id)? {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "profile already exists",
            ));
        }
        insert_profile(&transaction, &profile)?;
        insert_collections(&transaction, &profile)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error("commit profile create", error))
    }

    fn update(&self, profile: ProfileSpec, expected_revision: Revision) -> PortResult<()> {
        validate_profile(&profile)?;
        if profile.revision != expected_revision.next() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "replacement revision must increment exactly once",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error("start profile update transaction", error))?;
        let changed = update_profile(&transaction, &profile, expected_revision)?;
        if changed == 0 {
            return if profile_exists(&transaction, &profile.id)? {
                Err(PortError::new(
                    PortErrorKind::RevisionConflict,
                    "profile revision changed",
                ))
            } else {
                Err(PortError::not_found("profile"))
            };
        }
        transaction
            .execute(
                "DELETE FROM profile_tags WHERE profile_id = ?1",
                [profile.id.as_str()],
            )
            .map_err(|error| sqlite_error("replace profile tags", error))?;
        transaction
            .execute(
                "DELETE FROM profile_languages WHERE profile_id = ?1",
                [profile.id.as_str()],
            )
            .map_err(|error| sqlite_error("replace profile languages", error))?;
        transaction
            .execute(
                "DELETE FROM profile_startup_urls WHERE profile_id = ?1",
                [profile.id.as_str()],
            )
            .map_err(|error| sqlite_error("replace profile startup URLs", error))?;
        insert_collections(&transaction, &profile)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error("commit profile update", error))
    }
}

#[derive(Debug)]
struct StoredProfile {
    id: String,
    name: String,
    preset_id: String,
    preset_version: i64,
    proxy_id: Option<String>,
    browser_selector_kind: String,
    browser_build_id: Option<String>,
    browser_channel: Option<String>,
    persona_kind: String,
    preserve_system_timezone: i64,
    preserve_system_fonts: i64,
    preserve_system_gpu: i64,
    preserve_system_audio: i64,
    timezone: Option<String>,
    window_width: i64,
    window_height: i64,
    start_maximized: i64,
    revision: i64,
    archived: i64,
}

impl StoredProfile {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            preset_id: row.get(2)?,
            preset_version: row.get(3)?,
            proxy_id: row.get(4)?,
            browser_selector_kind: row.get(5)?,
            browser_build_id: row.get(6)?,
            browser_channel: row.get(7)?,
            persona_kind: row.get(8)?,
            preserve_system_timezone: row.get(9)?,
            preserve_system_fonts: row.get(10)?,
            preserve_system_gpu: row.get(11)?,
            preserve_system_audio: row.get(12)?,
            timezone: row.get(13)?,
            window_width: row.get(14)?,
            window_height: row.get(15)?,
            start_maximized: row.get(16)?,
            revision: row.get(17)?,
            archived: row.get(18)?,
        })
    }

    fn into_domain(self, connection: &Connection) -> PortResult<ProfileSpec> {
        let id = ProfileId::new(self.id).map_err(invalid_stored)?;
        let browser =
            match self.browser_selector_kind.as_str() {
                "pinned" => BrowserBuildSelector::Pinned(
                    BrowserBuildId::new(self.browser_build_id.ok_or_else(|| {
                        invalid_data("pinned profile is missing its browser build")
                    })?)
                    .map_err(invalid_stored)?,
                ),
                "channel" => BrowserBuildSelector::Channel(parse_channel(
                    self.browser_channel
                        .as_deref()
                        .ok_or_else(|| invalid_data("channel profile is missing its channel"))?,
                )?),
                _ => return Err(invalid_data("unknown browser selector kind")),
            };
        if self.persona_kind != "system_native" {
            return Err(invalid_data("unknown fingerprint persona kind"));
        }
        let profile = ProfileSpec {
            tags: load_strings(
                connection,
                "SELECT tag FROM profile_tags WHERE profile_id = ?1 ORDER BY position",
                &id,
            )?,
            locale: LocaleConfig {
                accept_languages: load_strings(
                    connection,
                    "SELECT language FROM profile_languages WHERE profile_id = ?1 ORDER BY position",
                    &id,
                )?,
                timezone: self.timezone,
            },
            startup_urls: load_strings(
                connection,
                "SELECT url FROM profile_startup_urls WHERE profile_id = ?1 ORDER BY position",
                &id,
            )?,
            id,
            name: self.name,
            environment_preset: EnvironmentPresetRef::new(
                PresetId::new(self.preset_id).map_err(invalid_stored)?,
                to_u32(self.preset_version, "preset version")?,
            )
            .map_err(invalid_stored)?,
            proxy_id: self
                .proxy_id
                .map(ProxyId::new)
                .transpose()
                .map_err(invalid_stored)?,
            browser,
            persona: FingerprintPersona::SystemNative(SystemNativePersona {
                preserve_system_timezone: to_bool(
                    self.preserve_system_timezone,
                    "preserve system timezone",
                )?,
                preserve_system_fonts: to_bool(
                    self.preserve_system_fonts,
                    "preserve system fonts",
                )?,
                preserve_system_gpu: to_bool(self.preserve_system_gpu, "preserve system GPU")?,
                preserve_system_audio: to_bool(
                    self.preserve_system_audio,
                    "preserve system audio",
                )?,
            }),
            window: WindowConfig {
                width: to_u32(self.window_width, "window width")?,
                height: to_u32(self.window_height, "window height")?,
                start_maximized: to_bool(self.start_maximized, "start maximized")?,
            },
            revision: Revision::new(to_u64(self.revision, "revision")?)
                .map_err(invalid_stored)?,
            archived: to_bool(self.archived, "archived")?,
        };
        validate_profile(&profile)?;
        Ok(profile)
    }
}

fn insert_profile(transaction: &Transaction<'_>, profile: &ProfileSpec) -> PortResult<()> {
    let (selector_kind, build_id, channel) = browser_columns(&profile.browser);
    let FingerprintPersona::SystemNative(persona) = profile.persona;
    transaction
        .execute(
            r#"
            INSERT INTO profiles (
                id, name, preset_id, preset_version, proxy_id,
                browser_selector_kind, browser_build_id, browser_channel,
                persona_kind, preserve_system_timezone, preserve_system_fonts,
                preserve_system_gpu, preserve_system_audio, timezone,
                window_width, window_height, start_maximized, revision, archived
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'system_native',
                ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
            params![
                profile.id.as_str(),
                profile.name,
                profile.environment_preset.id.as_str(),
                profile.environment_preset.version,
                profile.proxy_id.as_ref().map(ProfileIdLike::as_id_str),
                selector_kind,
                build_id,
                channel,
                persona.preserve_system_timezone,
                persona.preserve_system_fonts,
                persona.preserve_system_gpu,
                persona.preserve_system_audio,
                profile.locale.timezone,
                profile.window.width,
                profile.window.height,
                profile.window.start_maximized,
                profile.revision.get(),
                profile.archived,
            ],
        )
        .map_err(|error| sqlite_error("insert profile", error))?;
    Ok(())
}

fn update_profile(
    transaction: &Transaction<'_>,
    profile: &ProfileSpec,
    expected_revision: Revision,
) -> PortResult<usize> {
    let (selector_kind, build_id, channel) = browser_columns(&profile.browser);
    let FingerprintPersona::SystemNative(persona) = profile.persona;
    transaction
        .execute(
            r#"
            UPDATE profiles SET
                name = ?2,
                preset_id = ?3,
                preset_version = ?4,
                proxy_id = ?5,
                browser_selector_kind = ?6,
                browser_build_id = ?7,
                browser_channel = ?8,
                persona_kind = 'system_native',
                preserve_system_timezone = ?9,
                preserve_system_fonts = ?10,
                preserve_system_gpu = ?11,
                preserve_system_audio = ?12,
                timezone = ?13,
                window_width = ?14,
                window_height = ?15,
                start_maximized = ?16,
                revision = ?17,
                archived = ?18
            WHERE id = ?1 AND revision = ?19
            "#,
            params![
                profile.id.as_str(),
                profile.name,
                profile.environment_preset.id.as_str(),
                profile.environment_preset.version,
                profile.proxy_id.as_ref().map(ProfileIdLike::as_id_str),
                selector_kind,
                build_id,
                channel,
                persona.preserve_system_timezone,
                persona.preserve_system_fonts,
                persona.preserve_system_gpu,
                persona.preserve_system_audio,
                profile.locale.timezone,
                profile.window.width,
                profile.window.height,
                profile.window.start_maximized,
                profile.revision.get(),
                profile.archived,
                expected_revision.get(),
            ],
        )
        .map_err(|error| sqlite_error("update profile", error))
}

fn insert_collections(transaction: &Transaction<'_>, profile: &ProfileSpec) -> PortResult<()> {
    {
        let mut statement = transaction
            .prepare(
                "INSERT INTO profile_tags (profile_id, position, tag, tag_normalized) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|error| sqlite_error("prepare profile tag insert", error))?;
        for (position, tag) in profile.tags.iter().enumerate() {
            statement
                .execute(params![
                    profile.id.as_str(),
                    position as u64,
                    tag,
                    tag.to_ascii_lowercase()
                ])
                .map_err(|error| sqlite_error("insert profile tag", error))?;
        }
    }
    insert_ordered_strings(
        transaction,
        "INSERT INTO profile_languages (profile_id, position, language) VALUES (?1, ?2, ?3)",
        &profile.id,
        &profile.locale.accept_languages,
        "profile language",
    )?;
    insert_ordered_strings(
        transaction,
        "INSERT INTO profile_startup_urls (profile_id, position, url) VALUES (?1, ?2, ?3)",
        &profile.id,
        &profile.startup_urls,
        "profile startup URL",
    )
}

fn insert_ordered_strings(
    transaction: &Transaction<'_>,
    sql: &str,
    profile_id: &ProfileId,
    values: &[String],
    context: &'static str,
) -> PortResult<()> {
    let mut statement = transaction
        .prepare(sql)
        .map_err(|error| sqlite_error("prepare ordered profile value insert", error))?;
    for (position, value) in values.iter().enumerate() {
        statement
            .execute(params![profile_id.as_str(), position as u64, value])
            .map_err(|error| sqlite_error(context, error))?;
    }
    Ok(())
}

fn load_strings(connection: &Connection, sql: &str, id: &ProfileId) -> PortResult<Vec<String>> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| sqlite_error("prepare profile collection query", error))?;
    let rows = statement
        .query_map([id.as_str()], |row| row.get(0))
        .map_err(|error| sqlite_error("query profile collection", error))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite_error("decode profile collection", error))
}

fn profile_exists(connection: &Connection, id: &ProfileId) -> PortResult<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE id = ?1)",
            [id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_error("check profile existence", error))
}

fn browser_columns(selector: &BrowserBuildSelector) -> (&'static str, Option<&str>, Option<&str>) {
    match selector {
        BrowserBuildSelector::Pinned(id) => ("pinned", Some(id.as_str()), None),
        BrowserBuildSelector::Channel(channel) => ("channel", None, Some(channel_name(*channel))),
    }
}

fn channel_name(channel: BrowserChannel) -> &'static str {
    match channel {
        BrowserChannel::Stable => "stable",
        BrowserChannel::Beta => "beta",
        BrowserChannel::Dev => "dev",
        BrowserChannel::Canary => "canary",
    }
}

fn parse_channel(value: &str) -> PortResult<BrowserChannel> {
    match value {
        "stable" => Ok(BrowserChannel::Stable),
        "beta" => Ok(BrowserChannel::Beta),
        "dev" => Ok(BrowserChannel::Dev),
        "canary" => Ok(BrowserChannel::Canary),
        _ => Err(invalid_data("unknown browser channel")),
    }
}

fn literal_like_pattern(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn validate_profile(profile: &ProfileSpec) -> PortResult<()> {
    profile
        .validate()
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))
}

fn invalid_stored(error: brows_domain::DomainError) -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        format!("stored profile is invalid: {error}"),
    )
}

fn invalid_data(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn to_bool(value: i64, field: &'static str) -> PortResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_data(format!("stored {field} is not a boolean"))),
    }
}

fn to_u32(value: i64, field: &'static str) -> PortResult<u32> {
    u32::try_from(value).map_err(|_| invalid_data(format!("stored {field} is out of range")))
}

fn to_u64(value: i64, field: &'static str) -> PortResult<u64> {
    u64::try_from(value).map_err(|_| invalid_data(format!("stored {field} is out of range")))
}

trait ProfileIdLike {
    fn as_id_str(&self) -> &str;
}

impl ProfileIdLike for ProxyId {
    fn as_id_str(&self) -> &str {
        self.as_str()
    }
}
