use brows_application::{PortError, PortErrorKind, PortResult};
use rusqlite::{Connection, TransactionBehavior};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

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
