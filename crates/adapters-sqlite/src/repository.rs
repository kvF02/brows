use crate::migration::{migrate, sqlite_error};
use brows_application::{PortError, PortErrorKind, PortResult, ProfileQuery, ProfileRepository};
use brows_domain::{
    BrowserBuildId, BrowserBuildSelector, BrowserChannel, DeviceMemoryGb, EnvironmentPresetRef,
    FingerprintPersona, LocaleConfig, PlatformKind, PresetId, ProfileId, ProfileSpec, ProxyId,
    Revision, ScreenConfig, SeedableNoisePersona, SystemNativePersona, WebGLProfile, WindowConfig,
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
    window_width, window_height, start_maximized, revision, archived,
    persona_seed, persona_platform, persona_hardware_concurrency,
    persona_device_memory, persona_touch_points, persona_screen_width,
    persona_screen_height, persona_color_depth, persona_scale_factor_hundredths,
    persona_webgl_vendor, persona_webgl_renderer, persona_spoof_canvas,
    persona_spoof_audio
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
            .pragma_update(None, "synchronous", "FULL")
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
    persona_seed: Option<String>,
    persona_platform: Option<String>,
    persona_hardware_concurrency: Option<i64>,
    persona_device_memory: Option<String>,
    persona_touch_points: Option<i64>,
    persona_screen_width: Option<i64>,
    persona_screen_height: Option<i64>,
    persona_color_depth: Option<i64>,
    persona_scale_factor_hundredths: Option<i64>,
    persona_webgl_vendor: Option<String>,
    persona_webgl_renderer: Option<String>,
    persona_spoof_canvas: Option<i64>,
    persona_spoof_audio: Option<i64>,
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
            persona_seed: row.get(19)?,
            persona_platform: row.get(20)?,
            persona_hardware_concurrency: row.get(21)?,
            persona_device_memory: row.get(22)?,
            persona_touch_points: row.get(23)?,
            persona_screen_width: row.get(24)?,
            persona_screen_height: row.get(25)?,
            persona_color_depth: row.get(26)?,
            persona_scale_factor_hundredths: row.get(27)?,
            persona_webgl_vendor: row.get(28)?,
            persona_webgl_renderer: row.get(29)?,
            persona_spoof_canvas: row.get(30)?,
            persona_spoof_audio: row.get(31)?,
        })
    }

    fn into_domain(self, connection: &Connection) -> PortResult<ProfileSpec> {
        let id = ProfileId::new(self.id.clone()).map_err(invalid_stored)?;
        let persona = if self.persona_seed.is_some() {
            FingerprintPersona::SeedableNoise(self.seedable_persona()?)
        } else if self.persona_kind == "system_native" {
            FingerprintPersona::SystemNative(SystemNativePersona {
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
            })
        } else {
            return Err(invalid_data("unknown fingerprint persona kind"));
        };
        let browser =
            match self.browser_selector_kind.as_str() {
                "pinned" => BrowserBuildSelector::Pinned(
                    BrowserBuildId::new(self.browser_build_id.clone().ok_or_else(|| {
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
            persona,
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

    fn seedable_persona(&self) -> PortResult<SeedableNoisePersona> {
        let required_text = |value: Option<String>, field: &'static str| {
            value.ok_or_else(|| invalid_data(format!("seedable persona is missing {field}")))
        };
        let required_i64 = |value: Option<i64>, field: &'static str| {
            value.ok_or_else(|| invalid_data(format!("seedable persona is missing {field}")))
        };
        let platform = match required_text(self.persona_platform.clone(), "platform")?.as_str() {
            "windows" => PlatformKind::Windows,
            "mac_os" => PlatformKind::MacOs,
            "linux" => PlatformKind::Linux,
            _ => return Err(invalid_data("unknown seedable persona platform")),
        };
        let device_memory =
            match required_text(self.persona_device_memory.clone(), "device memory")?.as_str() {
                "quarter" => DeviceMemoryGb::Quarter,
                "half" => DeviceMemoryGb::Half,
                "one" => DeviceMemoryGb::One,
                "two" => DeviceMemoryGb::Two,
                "four" => DeviceMemoryGb::Four,
                "eight" => DeviceMemoryGb::Eight,
                _ => return Err(invalid_data("unknown seedable persona device memory")),
            };
        let bool_field =
            |value: Option<i64>, field: &'static str| to_bool(required_i64(value, field)?, field);
        let persona = SeedableNoisePersona {
            seed: required_text(self.persona_seed.clone(), "seed")?,
            platform,
            hardware_concurrency: u8::try_from(required_i64(
                self.persona_hardware_concurrency,
                "hardware concurrency",
            )?)
            .map_err(|_| invalid_data("invalid seedable hardware concurrency"))?,
            device_memory,
            touch_points: u8::try_from(required_i64(self.persona_touch_points, "touch points")?)
                .map_err(|_| invalid_data("invalid seedable touch points"))?,
            screen: ScreenConfig {
                width: u32::try_from(required_i64(self.persona_screen_width, "screen width")?)
                    .map_err(|_| invalid_data("invalid seedable screen width"))?,
                height: u32::try_from(required_i64(self.persona_screen_height, "screen height")?)
                    .map_err(|_| invalid_data("invalid seedable screen height"))?,
                color_depth: u8::try_from(required_i64(self.persona_color_depth, "color depth")?)
                    .map_err(|_| invalid_data("invalid seedable color depth"))?,
                device_scale_factor_hundredths: u16::try_from(required_i64(
                    self.persona_scale_factor_hundredths,
                    "scale factor",
                )?)
                .map_err(|_| invalid_data("invalid seedable scale factor"))?,
            },
            webgl: WebGLProfile {
                vendor: required_text(self.persona_webgl_vendor.clone(), "WebGL vendor")?,
                unmasked_renderer: required_text(
                    self.persona_webgl_renderer.clone(),
                    "WebGL renderer",
                )?,
            },
            spoof_canvas: bool_field(self.persona_spoof_canvas, "spoof canvas")?,
            spoof_audio: bool_field(self.persona_spoof_audio, "spoof audio")?,
        };
        persona.validate().map_err(invalid_stored)?;
        Ok(persona)
    }
}

fn insert_profile(transaction: &Transaction<'_>, profile: &ProfileSpec) -> PortResult<()> {
    let (selector_kind, build_id, channel) = browser_columns(&profile.browser);
    // v0.2: SeedableNoise profiles are stored with persona_kind='seedable_noise'.
    // SystemNative columns fall back to default (all true) for non-native personas.
    let (persona_kind, ptz, pfonts, pgpu, paudio, seedable) = match &profile.persona {
        FingerprintPersona::SystemNative(p) => (
            "system_native",
            p.preserve_system_timezone,
            p.preserve_system_fonts,
            p.preserve_system_gpu,
            p.preserve_system_audio,
            None,
        ),
        FingerprintPersona::SeedableNoise(p) => {
            ("system_native", false, false, false, false, Some(p))
        }
    };
    transaction
        .execute(
            r#"
            INSERT INTO profiles (
                id, name, preset_id, preset_version, proxy_id,
                browser_selector_kind, browser_build_id, browser_channel,
                persona_kind, preserve_system_timezone, preserve_system_fonts,
                preserve_system_gpu, preserve_system_audio, timezone,
                window_width, window_height, start_maximized, revision, archived,
                persona_seed, persona_platform, persona_hardware_concurrency,
                persona_device_memory, persona_touch_points, persona_screen_width,
                persona_screen_height, persona_color_depth, persona_scale_factor_hundredths,
                persona_webgl_vendor, persona_webgl_renderer, persona_spoof_canvas,
                persona_spoof_audio
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32
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
                persona_kind,
                ptz,
                pfonts,
                pgpu,
                paudio,
                profile.locale.timezone,
                profile.window.width,
                profile.window.height,
                profile.window.start_maximized,
                profile.revision.get(),
                profile.archived,
                seedable.map(|p| p.seed.as_str()),
                seedable.map(|p| platform_storage(p.platform)),
                seedable.map(|p| i64::from(p.hardware_concurrency)),
                seedable.map(|p| device_memory_storage(p.device_memory)),
                seedable.map(|p| i64::from(p.touch_points)),
                seedable.map(|p| i64::from(p.screen.width)),
                seedable.map(|p| i64::from(p.screen.height)),
                seedable.map(|p| i64::from(p.screen.color_depth)),
                seedable.map(|p| i64::from(p.screen.device_scale_factor_hundredths)),
                seedable.map(|p| p.webgl.vendor.as_str()),
                seedable.map(|p| p.webgl.unmasked_renderer.as_str()),
                seedable.map(|p| if p.spoof_canvas { 1i64 } else { 0i64 }),
                seedable.map(|p| if p.spoof_audio { 1i64 } else { 0i64 }),
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
    let (persona_kind, ptz, pfonts, pgpu, paudio, seedable) = match &profile.persona {
        FingerprintPersona::SystemNative(p) => (
            "system_native",
            p.preserve_system_timezone,
            p.preserve_system_fonts,
            p.preserve_system_gpu,
            p.preserve_system_audio,
            None,
        ),
        FingerprintPersona::SeedableNoise(p) => {
            ("system_native", false, false, false, false, Some(p))
        }
    };
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
                persona_kind = ?9,
                preserve_system_timezone = ?10,
                preserve_system_fonts = ?11,
                preserve_system_gpu = ?12,
                preserve_system_audio = ?13,
                timezone = ?14,
                window_width = ?15,
                window_height = ?16,
                start_maximized = ?17,
                revision = ?18,
                archived = ?19,
                persona_seed = ?21,
                persona_platform = ?22,
                persona_hardware_concurrency = ?23,
                persona_device_memory = ?24,
                persona_touch_points = ?25,
                persona_screen_width = ?26,
                persona_screen_height = ?27,
                persona_color_depth = ?28,
                persona_scale_factor_hundredths = ?29,
                persona_webgl_vendor = ?30,
                persona_webgl_renderer = ?31,
                persona_spoof_canvas = ?32,
                persona_spoof_audio = ?33
            WHERE id = ?1 AND revision = ?20
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
                persona_kind,
                ptz,
                pfonts,
                pgpu,
                paudio,
                profile.locale.timezone,
                profile.window.width,
                profile.window.height,
                profile.window.start_maximized,
                profile.revision.get(),
                profile.archived,
                expected_revision.get(),
                seedable.map(|p| p.seed.as_str()),
                seedable.map(|p| platform_storage(p.platform)),
                seedable.map(|p| i64::from(p.hardware_concurrency)),
                seedable.map(|p| device_memory_storage(p.device_memory)),
                seedable.map(|p| i64::from(p.touch_points)),
                seedable.map(|p| i64::from(p.screen.width)),
                seedable.map(|p| i64::from(p.screen.height)),
                seedable.map(|p| i64::from(p.screen.color_depth)),
                seedable.map(|p| i64::from(p.screen.device_scale_factor_hundredths)),
                seedable.map(|p| p.webgl.vendor.as_str()),
                seedable.map(|p| p.webgl.unmasked_renderer.as_str()),
                seedable.map(|p| if p.spoof_canvas { 1i64 } else { 0i64 }),
                seedable.map(|p| if p.spoof_audio { 1i64 } else { 0i64 }),
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

fn platform_storage(platform: PlatformKind) -> &'static str {
    match platform {
        PlatformKind::Windows => "windows",
        PlatformKind::MacOs => "mac_os",
        PlatformKind::Linux => "linux",
    }
}

fn device_memory_storage(memory: DeviceMemoryGb) -> &'static str {
    match memory {
        DeviceMemoryGb::Quarter => "quarter",
        DeviceMemoryGb::Half => "half",
        DeviceMemoryGb::One => "one",
        DeviceMemoryGb::Two => "two",
        DeviceMemoryGb::Four => "four",
        DeviceMemoryGb::Eight => "eight",
    }
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

#[cfg(test)]
#[test]
fn profile_publication_uses_full_synchronous_wal_commits() {
    let repo = SqliteProfileRepository::open(":memory:").unwrap();
    let mode: i64 = repo
        .lock()
        .unwrap()
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, 2);
}
