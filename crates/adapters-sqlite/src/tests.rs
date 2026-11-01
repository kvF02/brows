use super::*;
use brows_application::SettingsRepository;
use brows_application::{PortErrorKind, ProfileQuery, ProfileRepository, ProxyRepository};
use brows_domain::{
    BrowserBuildId, BrowserBuildSelector, BrowserChannel, DeviceMemoryGb, DnsMode,
    EnvironmentPresetRef, FingerprintPersona, LocaleConfig, PlatformKind, PresetId, ProfileId,
    ProfileSpec, ProxyId, ProxyScheme, ProxySpec, Revision, ScreenConfig, SecretRef,
    SeedableNoisePersona, SystemNativePersona, WebGLProfile, WindowConfig,
    DEFAULT_PROXY_TIMEOUT_MS,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn manager_settings_persist_and_invalid_writes_do_not_replace_them() {
    let database = TemporaryDatabase::new("settings-persist");
    let repository = SqliteSettingsRepository::open(database.path()).unwrap();
    assert_eq!(repository.load().unwrap().max_concurrent_sessions, 10);
    repository
        .save(&brows_domain::ManagerSettings {
            max_concurrent_sessions: 3,
        })
        .unwrap();
    for limit in [0, 11, usize::MAX] {
        assert!(repository
            .save(&brows_domain::ManagerSettings {
                max_concurrent_sessions: limit
            })
            .is_err());
    }
    drop(repository);
    let reopened = SqliteSettingsRepository::open(database.path()).unwrap();
    assert_eq!(reopened.load().unwrap().max_concurrent_sessions, 3);
}

#[test]
fn settings_migration_upgrades_v5_without_modifying_existing_profiles() {
    let database = TemporaryDatabase::new("settings-upgrade");
    let profiles = SqliteProfileRepository::open(database.path()).unwrap();
    let original = profile("retained", "Retained profile");
    profiles.create(original.clone()).unwrap();
    drop(profiles);
    {
        let connection = Connection::open(database.path()).unwrap();
        connection
            .execute_batch("DROP TABLE manager_settings; PRAGMA user_version = 5;")
            .unwrap();
    }
    let settings = SqliteSettingsRepository::open(database.path()).unwrap();
    assert_eq!(settings.load().unwrap().max_concurrent_sessions, 10);
    let profiles = SqliteProfileRepository::open(database.path()).unwrap();
    assert_eq!(profiles.get(&original.id).unwrap(), original);
    assert_eq!(profiles.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
}

#[test]
fn missing_or_invalid_settings_are_reported_instead_of_resetting_user_preferences() {
    for missing in [true, false] {
        let database = TemporaryDatabase::new("settings-corrupt");
        let settings = SqliteSettingsRepository::open(database.path()).unwrap();
        let connection = Connection::open(database.path()).unwrap();
        if missing {
            connection
                .execute("DELETE FROM manager_settings", [])
                .unwrap();
        } else {
            connection.execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE manager_settings SET max_concurrent_sessions = 42;").unwrap();
        }
        assert!(settings.load().is_err());
        if missing {
            assert!(settings
                .save(&brows_domain::ManagerSettings::default())
                .is_err());
        }
    }
}

#[derive(Debug)]
struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new(test_name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the Unix epoch")
            .as_nanos();
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "brows-sqlite-{test_name}-{}-{nonce}-{sequence}.sqlite3",
            std::process::id()
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

fn profile(id: &str, name: &str) -> ProfileSpec {
    ProfileSpec {
        id: ProfileId::new(id).unwrap(),
        name: name.to_owned(),
        tags: vec!["qa".to_owned(), "regional".to_owned()],
        environment_preset: EnvironmentPresetRef::new(PresetId::new("system-native").unwrap(), 1)
            .unwrap(),
        proxy_id: Some(ProxyId::new("proxy-1").unwrap()),
        browser: BrowserBuildSelector::Channel(BrowserChannel::Stable),
        persona: FingerprintPersona::SystemNative(SystemNativePersona::default()),
        locale: LocaleConfig {
            accept_languages: vec!["en-US".to_owned(), "en".to_owned()],
            timezone: None,
        },
        window: WindowConfig {
            width: 1366,
            height: 768,
            start_maximized: false,
        },
        startup_urls: vec![
            "https://example.test".to_owned(),
            "https://status.example.test".to_owned(),
        ],
        revision: Revision::FIRST,
        archived: false,
    }
}

fn all_profiles() -> ProfileQuery {
    ProfileQuery {
        offset: 0,
        limit: 200,
        search: None,
        include_archived: true,
    }
}

#[test]
fn migration_enables_wal_and_round_trips_every_profile_field() {
    let database = TemporaryDatabase::new("migration-roundtrip");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    assert_eq!(repository.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    assert_eq!(
        repository.journal_mode().unwrap().to_ascii_lowercase(),
        "wal"
    );

    let mut expected = profile("profile-1", "Regional QA");
    expected.browser =
        BrowserBuildSelector::Pinned(BrowserBuildId::new("chrome-stable-128").unwrap());
    expected.tags = vec!["Release".to_owned(), "East Asia".to_owned()];
    repository.create(expected.clone()).unwrap();
    assert_eq!(repository.get(&expected.id).unwrap(), expected);

    drop(repository);
    let reopened = SqliteProfileRepository::open(database.path()).unwrap();
    assert_eq!(reopened.get(&expected.id).unwrap(), expected);
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
}

#[test]
fn seedable_noise_persona_round_trips_through_sqlite() {
    let database = TemporaryDatabase::new("seedable-roundtrip");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    let mut expected = profile("profile-seed", "Seedable");
    expected.persona = FingerprintPersona::SeedableNoise(SeedableNoisePersona {
        seed: "stable-seed".to_owned(),
        platform: PlatformKind::Windows,
        hardware_concurrency: 8,
        device_memory: DeviceMemoryGb::Eight,
        touch_points: 0,
        screen: ScreenConfig {
            width: 1920,
            height: 1080,
            color_depth: 24,
            device_scale_factor_hundredths: 100,
        },
        webgl: WebGLProfile::auto(),
        spoof_canvas: true,
        spoof_audio: false,
    });
    repository.create(expected.clone()).unwrap();
    assert_eq!(repository.get(&expected.id).unwrap(), expected);
    drop(repository);
    let reopened = SqliteProfileRepository::open(database.path()).unwrap();
    assert_eq!(reopened.get(&expected.id).unwrap(), expected);
}

#[test]
fn list_orders_pages_searches_tags_and_filters_archived_profiles() {
    let database = TemporaryDatabase::new("list");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    let zulu = profile("profile-z", "Zulu");
    let mut alpha = profile("profile-a", "Alpha");
    alpha.tags = vec!["sales%qa".to_owned()];
    let beta = profile("profile-b", "Beta");
    repository.create(zulu).unwrap();
    repository.create(alpha.clone()).unwrap();
    repository.create(beta.clone()).unwrap();

    let first_page = repository
        .list(&ProfileQuery {
            offset: 0,
            limit: 2,
            search: None,
            include_archived: false,
        })
        .unwrap();
    assert_eq!(
        first_page
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Alpha", "Beta"]
    );
    let second_page = repository
        .list(&ProfileQuery {
            offset: 2,
            limit: 2,
            search: None,
            include_archived: false,
        })
        .unwrap();
    assert_eq!(second_page[0].name, "Zulu");

    let literal_wildcard_search = repository
        .list(&ProfileQuery {
            offset: 0,
            limit: 10,
            search: Some("%".to_owned()),
            include_archived: false,
        })
        .unwrap();
    assert_eq!(literal_wildcard_search.len(), 1);
    assert_eq!(literal_wildcard_search[0].id, alpha.id);

    let mut archived = beta;
    let expected_revision = archived.revision;
    archived.archived = true;
    archived.bump_revision();
    repository
        .update(archived.clone(), expected_revision)
        .unwrap();
    assert_eq!(
        repository
            .list(&ProfileQuery {
                include_archived: false,
                ..all_profiles()
            })
            .unwrap()
            .len(),
        2
    );
    assert_eq!(repository.list(&all_profiles()).unwrap().len(), 3);
    assert!(repository.get(&archived.id).unwrap().archived);
}

#[test]
fn update_atomically_checks_revision_and_replaces_child_collections() {
    let database = TemporaryDatabase::new("revision");
    let first = SqliteProfileRepository::open(database.path()).unwrap();
    let second = SqliteProfileRepository::open(database.path()).unwrap();
    let original = profile("profile-1", "Original");
    first.create(original.clone()).unwrap();

    let mut winner = first.get(&original.id).unwrap();
    winner.name = "Winner".to_owned();
    winner.tags = vec!["updated".to_owned()];
    winner.locale.accept_languages = vec!["zh-CN".to_owned(), "zh".to_owned()];
    winner.startup_urls = vec!["https://updated.example.test".to_owned()];
    let expected_revision = winner.revision;
    winner.bump_revision();
    first.update(winner.clone(), expected_revision).unwrap();

    let mut stale = original;
    stale.name = "Stale".to_owned();
    stale.bump_revision();
    let error = second.update(stale, Revision::FIRST).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::RevisionConflict);
    assert_eq!(second.get(&winner.id).unwrap(), winner);
}

#[test]
fn create_supports_clone_semantics_and_rejects_duplicate_identity() {
    let database = TemporaryDatabase::new("clone");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    let source = profile("profile-source", "Source");
    repository.create(source.clone()).unwrap();

    let mut cloned = source.clone();
    cloned.id = ProfileId::new("profile-clone").unwrap();
    cloned.name = "Source Copy".to_owned();
    cloned.revision = Revision::FIRST;
    repository.create(cloned.clone()).unwrap();
    assert_eq!(repository.list(&all_profiles()).unwrap().len(), 2);
    assert_eq!(repository.get(&cloned.id).unwrap(), cloned);

    let error = repository.create(source).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::AlreadyExists);
}

#[test]
fn update_distinguishes_missing_profile_from_revision_conflict() {
    let database = TemporaryDatabase::new("missing-update");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    let mut missing = profile("missing", "Missing");
    missing.bump_revision();
    let error = repository.update(missing, Revision::FIRST).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::NotFound);
}

#[test]
fn newer_database_schema_is_rejected() {
    let database = TemporaryDatabase::new("future-schema");
    {
        let connection = Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
    }
    let error = SqliteProfileRepository::open(database.path()).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::Incompatible);
}

#[test]
fn repository_satisfies_send_sync_object_safe_port() {
    fn assert_repository(_: &dyn ProfileRepository) {}
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<SqliteProfileRepository>();
    let database = TemporaryDatabase::new("object-safe");
    let repository = SqliteProfileRepository::open(database.path()).unwrap();
    assert_repository(&repository);
}

#[test]
fn proxy_repository_round_trips_metadata_and_refuses_in_use_delete() {
    let database = TemporaryDatabase::new("proxy-metadata");
    let profiles = SqliteProfileRepository::open(database.path()).unwrap();
    let proxies = SqliteProxyRepository::open(database.path()).unwrap();
    let proxy = ProxySpec {
        id: ProxyId::new("proxy-one").unwrap(),
        name: "Tokyo gateway".to_owned(),
        scheme: ProxyScheme::Socks5,
        host: "proxy.example.test".to_owned(),
        port: 1080,
        username: Some("tester".to_owned()),
        secret_ref: Some(SecretRef::new("secret-one").unwrap()),
        dns_mode: DnsMode::Proxy,
        bypass_hosts: vec!["localhost".to_owned()],
        connect_timeout_ms: DEFAULT_PROXY_TIMEOUT_MS,
    };
    proxies.save(&proxy).unwrap();
    assert_eq!(proxies.get(&proxy.id).unwrap(), proxy);
    assert_eq!(proxies.list().unwrap(), vec![proxy.clone()]);

    let mut assigned = profile("profile-with-proxy", "Assigned");
    assigned.proxy_id = Some(proxy.id.clone());
    profiles.create(assigned).unwrap();
    let error = proxies.delete(&proxy.id).unwrap_err();
    assert_eq!(error.kind, PortErrorKind::Locked);
}
