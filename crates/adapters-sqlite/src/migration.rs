use brows_application::{PortError, PortErrorKind, PortResult};
use rusqlite::{Connection, TransactionBehavior};

pub const CURRENT_SCHEMA_VERSION: u32 = 5;

const MIGRATION_1: &str = r#"
CREATE TABLE profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    preset_id TEXT NOT NULL,
    preset_version INTEGER NOT NULL CHECK (preset_version > 0),
    proxy_id TEXT,
    browser_selector_kind TEXT NOT NULL
        CHECK (browser_selector_kind IN ('pinned', 'channel')),
    browser_build_id TEXT,
    browser_channel TEXT
        CHECK (browser_channel IS NULL OR browser_channel IN ('stable', 'beta', 'dev', 'canary')),
    persona_kind TEXT NOT NULL CHECK (persona_kind = 'system_native'),
    preserve_system_timezone INTEGER NOT NULL CHECK (preserve_system_timezone IN (0, 1)),
    preserve_system_fonts INTEGER NOT NULL CHECK (preserve_system_fonts IN (0, 1)),
    preserve_system_gpu INTEGER NOT NULL CHECK (preserve_system_gpu IN (0, 1)),
    preserve_system_audio INTEGER NOT NULL CHECK (preserve_system_audio IN (0, 1)),
    timezone TEXT,
    window_width INTEGER NOT NULL CHECK (window_width >= 320),
    window_height INTEGER NOT NULL CHECK (window_height >= 240),
    start_maximized INTEGER NOT NULL CHECK (start_maximized IN (0, 1)),
    revision INTEGER NOT NULL CHECK (revision > 0),
    archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    CHECK (
        (browser_selector_kind = 'pinned' AND browser_build_id IS NOT NULL AND browser_channel IS NULL)
        OR
        (browser_selector_kind = 'channel' AND browser_build_id IS NULL AND browser_channel IS NOT NULL)
    )
) STRICT;

CREATE TABLE profile_tags (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    tag TEXT NOT NULL,
    tag_normalized TEXT NOT NULL,
    PRIMARY KEY (profile_id, position),
    UNIQUE (profile_id, tag_normalized)
) STRICT;

CREATE TABLE profile_languages (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    language TEXT NOT NULL,
    PRIMARY KEY (profile_id, position)
) STRICT;

CREATE TABLE profile_startup_urls (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    url TEXT NOT NULL,
    PRIMARY KEY (profile_id, position)
) STRICT;

CREATE INDEX profiles_archive_name_idx
    ON profiles (archived, name COLLATE NOCASE, id);
CREATE INDEX profile_tags_search_idx
    ON profile_tags (tag_normalized, profile_id);
"#;

const MIGRATION_2: &str = r#"
CREATE TABLE migration_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE RESTRICT,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('morelogin')),
    source_profile_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'rolled_back')),
    started_at_ms INTEGER NOT NULL CHECK (started_at_ms >= 0),
    completed_at_ms INTEGER CHECK (completed_at_ms IS NULL OR completed_at_ms >= started_at_ms),
    copied_files INTEGER CHECK (copied_files IS NULL OR copied_files >= 0),
    copied_bytes INTEGER CHECK (copied_bytes IS NULL OR copied_bytes >= 0),
    backup_key TEXT,
    error_summary TEXT,
    CHECK (
        (status = 'running' AND completed_at_ms IS NULL)
        OR (status != 'running' AND completed_at_ms IS NOT NULL)
    )
) STRICT;

CREATE INDEX migration_records_profile_time_idx
    ON migration_records (profile_id, started_at_ms DESC, id DESC);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE proxies (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    scheme TEXT NOT NULL CHECK (scheme IN ('http', 'https', 'socks5')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    username TEXT,
    secret_ref TEXT,
    dns_mode TEXT NOT NULL CHECK (dns_mode IN ('proxy', 'local')),
    connect_timeout_ms INTEGER NOT NULL CHECK (connect_timeout_ms > 0),
    CHECK ((username IS NULL AND secret_ref IS NULL) OR (username IS NOT NULL AND secret_ref IS NOT NULL))
) STRICT;

CREATE TABLE proxy_bypass_hosts (
    proxy_id TEXT NOT NULL REFERENCES proxies(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    host TEXT NOT NULL,
    PRIMARY KEY (proxy_id, position)
) STRICT;

CREATE INDEX proxies_name_idx ON proxies (name COLLATE NOCASE, id);
"#;

// SeedableNoise fields are kept nullable so databases created by v0.1 can be
// upgraded without rewriting the profile table.  `persona_kind` remains
// constrained for backwards compatibility; the presence of `persona_seed`
// is the authoritative discriminator for the new persona variant.
const MIGRATION_4: &str = r#"
ALTER TABLE profiles ADD COLUMN persona_seed TEXT;
ALTER TABLE profiles ADD COLUMN persona_platform TEXT;
ALTER TABLE profiles ADD COLUMN persona_hardware_concurrency INTEGER;
ALTER TABLE profiles ADD COLUMN persona_device_memory TEXT;
ALTER TABLE profiles ADD COLUMN persona_touch_points INTEGER;
ALTER TABLE profiles ADD COLUMN persona_screen_width INTEGER;
ALTER TABLE profiles ADD COLUMN persona_screen_height INTEGER;
ALTER TABLE profiles ADD COLUMN persona_color_depth INTEGER;
ALTER TABLE profiles ADD COLUMN persona_scale_factor_hundredths INTEGER;
ALTER TABLE profiles ADD COLUMN persona_webgl_vendor TEXT;
ALTER TABLE profiles ADD COLUMN persona_webgl_renderer TEXT;
ALTER TABLE profiles ADD COLUMN persona_spoof_canvas INTEGER;
ALTER TABLE profiles ADD COLUMN persona_spoof_audio INTEGER;
"#;

const MIGRATION_5: &str = r#"
CREATE TABLE audit_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL CHECK (at_ms >= 0),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 64),
    entity_id TEXT NOT NULL CHECK (length(entity_id) BETWEEN 1 AND 128),
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'failed'))
) STRICT;

CREATE INDEX audit_events_time_idx
    ON audit_events (at_ms DESC, id DESC);
"#;

pub(crate) fn migrate(connection: &mut Connection) -> PortResult<()> {
    let found: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_error("read schema version", error))?;
    if found > CURRENT_SCHEMA_VERSION {
        return Err(PortError::new(
            PortErrorKind::Incompatible,
            format!(
                "database schema version {found} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            ),
        ));
    }

    let mut version = found;
    while version < CURRENT_SCHEMA_VERSION {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error("start schema migration", error))?;
        match version + 1 {
            1 => transaction
                .execute_batch(MIGRATION_1)
                .map_err(|error| sqlite_error("apply schema migration 1", error))?,
            2 => transaction
                .execute_batch(MIGRATION_2)
                .map_err(|error| sqlite_error("apply schema migration 2", error))?,
            3 => transaction
                .execute_batch(MIGRATION_3)
                .map_err(|error| sqlite_error("apply schema migration 3", error))?,
            4 => transaction
                .execute_batch(MIGRATION_4)
                .map_err(|error| sqlite_error("apply schema migration 4", error))?,
            5 => transaction
                .execute_batch(MIGRATION_5)
                .map_err(|error| sqlite_error("apply schema migration 5", error))?,
            target => {
                return Err(PortError::new(
                    PortErrorKind::Incompatible,
                    format!("no migration is registered for schema version {target}"),
                ));
            }
        }
        version += 1;
        transaction
            .pragma_update(None, "user_version", version)
            .map_err(|error| sqlite_error("record schema version", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_error("commit schema migration", error))?;
    }
    Ok(())
}

pub(crate) fn sqlite_error(context: &'static str, error: rusqlite::Error) -> PortError {
    PortError::new(PortErrorKind::Internal, format!("{context}: {error}"))
}
