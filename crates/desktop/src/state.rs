use brows_adapters_backup::EncryptedBackupService;
use brows_adapters_diagnostic::LocalNetworkDiagnosticRunner;
use brows_adapters_morelogin::MoreLoginProfileDataMigrator;
use brows_adapters_proxy::{StoredProxyRuntime, TcpProxyConnectivityTester};
use brows_adapters_runtime::{verify_authenticode_publisher, ManagedChromiumRuntime};
use brows_adapters_secret_windows::WindowsCredentialStore;
use brows_adapters_sqlite::{
    SqliteAuditSink, SqliteMigrationRecordRepository, SqliteProfileRepository,
    SqliteProxyRepository, SqliteSettingsStore,
};
#[cfg(test)]
use brows_application::memory::InMemoryAuditSink;
#[cfg(test)]
use brows_application::memory::InMemoryProxyRuntime;
#[cfg(test)]
use brows_application::memory::SystemNativeFingerprintRuntime;
#[cfg(test)]
use brows_application::memory::{
    InMemoryBrowserRuntime, InMemoryProfileRepository, ManualClock, SequentialIdGenerator,
};
use brows_application::{
    memory::{system_native_preset, FingerprintRuntimeRouter, InMemoryPresetCatalog},
    ApplicationResult, BrowserRuntime, Clock, IdGenerator, LaunchPlanCompiler,
    MigrationApplicationService, PortError, PortErrorKind, PortResult, ProfileApplicationService,
    ProxyApplicationService, SessionApplicationService,
};
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserCapabilities, BrowserChannel, ProfileId, RuntimeTrust,
    SecretRef, SessionId,
};
use std::{
    io::Read,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Tauri state contains application services only. Concrete adapters are
/// assembled once and remain hidden behind the application's ports.
#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) runtime_management: Option<Arc<crate::runtime_management::RuntimeManagementService>>,
    pub(crate) runtime_issues: Vec<String>,
    pub(crate) settings: Arc<crate::settings::SettingsService>,
    pub(crate) browsers: Arc<dyn BrowserRuntime>,
    pub(crate) profiles: Arc<ProfileApplicationService>,
    pub(crate) sessions: Arc<SessionApplicationService>,
    pub(crate) migrations: Option<Arc<MigrationApplicationService>>,
    pub(crate) proxy_management: Option<Arc<ProxyApplicationService>>,
    pub(crate) diagnostics: Option<Arc<LocalNetworkDiagnosticRunner>>,
    pub(crate) backups: Option<Arc<EncryptedBackupService>>,
}

impl AppState {
    /// Production composition root for the phase-one desktop application.
    pub(crate) fn production(data_root: PathBuf) -> ApplicationResult<Self> {
        std::fs::create_dir_all(&data_root).map_err(|error| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::Internal,
                format!("failed to create application data directory: {error}"),
            )
        })?;

        let clock = Arc::new(SystemClock);
        let database_path = data_root.join("profiles.sqlite3");
        let settings_store = Arc::new(SqliteSettingsStore::open(&database_path)?);
        let settings = settings_store.get()?;
        let profiles = Arc::new(SqliteProfileRepository::open(&database_path)?);
        let migration_records = Arc::new(SqliteMigrationRecordRepository::open(&database_path)?);
        let proxy_records = Arc::new(SqliteProxyRepository::open(&database_path)?);
        let presets = Arc::new(InMemoryPresetCatalog::default());
        let credential_store = Arc::new(WindowsCredentialStore::new());
        let proxies = Arc::new(StoredProxyRuntime::new(
            proxy_records.clone(),
            credential_store.clone(),
        ));
        let fingerprints = Arc::new(FingerprintRuntimeRouter::new(data_root.join("extensions"))?);
        let audit = Arc::new(SqliteAuditSink::open(&database_path)?);
        let diagnostics = Arc::new(LocalNetworkDiagnosticRunner::new(
            proxies.clone(),
            clock.clone(),
        ));
        let ids = Arc::new(UuidIdGenerator);

        presets.insert(system_native_preset())?;

        let system_root = discover_chrome_root().ok();
        let edge_root = discover_edge_root();
        let profile_root = data_root.join("profiles");
        std::fs::create_dir_all(&profile_root).map_err(|error| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::Internal,
                format!("failed to create profile data directory: {error}"),
            )
        })?;
        let profile_root = profile_root.canonicalize().map_err(|error| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::Internal,
                format!("failed to canonicalize profile data directory: {error}"),
            )
        })?;
        let backup_service = Arc::new(EncryptedBackupService::new(
            data_root.join("backups"),
            profile_root.clone(),
        )?);
        let managed_root = data_root.join("runtimes");
        std::fs::create_dir_all(&managed_root).map_err(|_| PortError::new(PortErrorKind::Unavailable, "cannot create managed runtime directory"))?;
        let mut roots: Vec<_> = system_root
            .iter()
            .chain(edge_root.iter())
            .cloned()
            .collect();
        roots.push(managed_root.clone());
        let browsers = Arc::new(ManagedChromiumRuntime::with_runtime_roots(
            roots,
            &profile_root,
        )?);
        let probe_settings = data_root.join("network-probe.json");
        if probe_settings.is_file() {
            let mut bytes = Vec::new();
            std::fs::File::open(&probe_settings)
                .and_then(|file| file.take(4097).read_to_end(&mut bytes))
                .map_err(|_| {
                    PortError::new(
                        PortErrorKind::InvalidData,
                        "cannot read network probe settings",
                    )
                })?;
            if bytes.len() > 4096 {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "network probe settings too large",
                )
                .into());
            }
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
                PortError::new(PortErrorKind::InvalidData, "invalid network probe settings")
            })?;
            let field = |key: &str| {
                value[key].as_str().map(str::to_owned).ok_or_else(|| {
                    PortError::new(PortErrorKind::InvalidData, "missing network probe setting")
                })
            };
            let port = |key: &str| {
                value[key]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .ok_or_else(|| {
                        PortError::new(PortErrorKind::InvalidData, "invalid STUN port setting")
                    })
            };
            browsers.set_network_probe_config(brows_application::NetworkProbeConfig {
                origin: field("origin")?,
                zone: field("zone")?,
                stun_min: port("stunMin")?,
                stun_max: port("stunMax")?,
            })?;
        }
        if let Some(root) = system_root {
            let mut build = discovered_chrome_build(&root)?;
            build.publisher =
                verify_authenticode_publisher(&build.executable_path, &["Google LLC"])?;
            build.version = brows_adapters_runtime::read_file_version(&build.executable_path)?;
            browsers.register_build_with_current_digest(build, true)?;
        }
        if let Some(root) = edge_root {
            let mut build = discovered_edge_build(&root)?;
            build.publisher =
                verify_authenticode_publisher(&build.executable_path, &["Microsoft Corporation"])?;
            build.version = brows_adapters_runtime::read_file_version(&build.executable_path)?;
            // Chrome remains the default for existing channel-based profiles.
            let activate = browsers.installed_builds()?.is_empty();
            browsers.register_build_with_current_digest(build, activate)?;
        }

        let mut runtime_issues = browsers.load_snapshots(managed_root)?;
        if let Some(id) = &settings.active_runtime_id {
            let id = BrowserBuildId::new(id.clone())?;
            if browsers.activate(&id).is_err() {
                browsers.clear_active_channel(BrowserChannel::Stable)?;
                runtime_issues.push("Saved default runtime is unavailable; select an admitted build before launching channel-based profiles".to_owned());
            }
        }

        let compiler = Arc::new(LaunchPlanCompiler::new(
            presets.clone(),
            browsers.clone(),
            proxies.clone(),
            fingerprints,
            profile_root.clone(),
        )?);
        let profile_service = Arc::new(ProfileApplicationService::new(
            profiles.clone(),
            presets,
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let session_service = Arc::new(SessionApplicationService::new(
            profiles.clone(),
            compiler,
            browsers.clone(),
            proxies,
            audit,
            clock.clone(),
            ids.clone(),
            settings.max_concurrent_sessions as usize,
        )?);
        let migration_service = Arc::new(MigrationApplicationService::new(
            profiles.clone(),
            session_service.clone(),
            Arc::new(MoreLoginProfileDataMigrator::new(
                default_morelogin_cache_root(),
                profile_root,
            )?),
            migration_records,
            clock,
        ));
        let proxy_management = Arc::new(ProxyApplicationService::new(
            proxy_records,
            credential_store,
            Arc::new(TcpProxyConnectivityTester),
            ids,
        ));

        Ok(Self {
            runtime_management: Some(Arc::new(crate::runtime_management::RuntimeManagementService {
                browsers: browsers.clone(), store: settings_store.clone(), sessions: session_service.clone(),
            })),
            runtime_issues,
            settings: Arc::new(crate::settings::SettingsService::new(
                settings_store,
                session_service.clone(),
            )),
            browsers: browsers.clone(),
            profiles: profile_service,
            sessions: session_service,
            migrations: Some(migration_service),
            proxy_management: Some(proxy_management),
            diagnostics: Some(diagnostics),
            backups: Some(backup_service),
        })
    }

    #[cfg(test)]
    pub(crate) fn prototype(data_root: PathBuf) -> ApplicationResult<Self> {
        Self::prototype_with_capacity(data_root, brows_domain::DEFAULT_MAX_CONCURRENT_SESSIONS)
    }

    #[cfg(test)]
    fn prototype_with_capacity(
        data_root: PathBuf,
        max_concurrent_sessions: usize,
    ) -> ApplicationResult<Self> {
        let clock = Arc::new(ManualClock::new(unix_time_ms()));
        let profiles = Arc::new(InMemoryProfileRepository::default());
        let presets = Arc::new(InMemoryPresetCatalog::default());
        let browsers = Arc::new(InMemoryBrowserRuntime::new(clock.clone()));
        let proxies = Arc::new(InMemoryProxyRuntime::default());
        let fingerprints = Arc::new(SystemNativeFingerprintRuntime);
        let audit = Arc::new(InMemoryAuditSink::default());
        let ids = Arc::new(SequentialIdGenerator::default());

        presets.insert(system_native_preset())?;
        browsers.insert_build(prototype_browser_build(&data_root)?)?;

        let compiler = Arc::new(LaunchPlanCompiler::new(
            presets.clone(),
            browsers.clone(),
            proxies.clone(),
            fingerprints,
            data_root.join("profiles"),
        )?);
        let profile_service = Arc::new(ProfileApplicationService::new(
            profiles.clone(),
            presets,
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let session_service = Arc::new(SessionApplicationService::new(
            profiles,
            compiler,
            browsers.clone(),
            proxies,
            audit,
            clock,
            ids,
            max_concurrent_sessions,
        )?);

        Ok(Self {
            runtime_management: None,
            runtime_issues: Vec::new(),
            settings: Arc::new(crate::settings::SettingsService::new(
                Arc::new(SqliteSettingsStore::open(":memory:")?),
                session_service.clone(),
            )),
            browsers: browsers.clone(),
            profiles: profile_service,
            sessions: session_service,
            migrations: None,
            proxy_management: None,
            diagnostics: None,
            backups: None,
        })
    }
}

fn default_morelogin_cache_root() -> PathBuf {
    [
        PathBuf::from(r"D:\.MoreLogin\cache"),
        PathBuf::from(r"C:\.MoreLogin\cache"),
    ]
    .into_iter()
    .find(|path| path.is_dir())
    .unwrap_or_else(|| PathBuf::from(r"D:\.MoreLogin\cache"))
}

fn discover_chrome_root() -> ApplicationResult<PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(root) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(root).join("Google/Chrome/Application"));
        }
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Google\Chrome\Application"));
    candidates.push(PathBuf::from(
        r"C:\Program Files (x86)\Google\Chrome\Application",
    ));
    candidates
        .into_iter()
        .find(|root| root.join("chrome.exe").is_file())
        .ok_or_else(|| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::CapabilityUnsupported,
                "Google Chrome installation was not found",
            )
        })
}

fn discover_edge_root() -> Option<PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles", "LocalAppData"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Microsoft/Edge/Application"))
        .find(|root| root.join("msedge.exe").is_file())
}

fn discovered_edge_build(root: &std::path::Path) -> ApplicationResult<BrowserBuild> {
    let mut build = discovered_chrome_build(root)?;
    build.id = BrowserBuildId::new("microsoft-edge-stable")?;
    build.provider = "Microsoft Edge".to_owned();
    build.executable_path = root.join("msedge.exe");
    build.publisher = "Microsoft Corporation".to_owned();
    Ok(build)
}

#[derive(Debug, Default)]
struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        unix_time_ms()
    }
}

#[derive(Debug, Default)]
struct UuidIdGenerator;

impl IdGenerator for UuidIdGenerator {
    fn next_profile_id(&self) -> PortResult<ProfileId> {
        new_id("profile", ProfileId::new)
    }

    fn next_proxy_id(&self) -> PortResult<brows_domain::ProxyId> {
        new_id("proxy", brows_domain::ProxyId::new)
    }

    fn next_session_id(&self) -> PortResult<SessionId> {
        new_id("session", SessionId::new)
    }

    fn next_secret_ref(&self) -> PortResult<SecretRef> {
        new_id("secret", SecretRef::new)
    }
}

fn new_id<T>(
    prefix: &str,
    parse: impl FnOnce(String) -> Result<T, brows_domain::DomainError>,
) -> PortResult<T> {
    parse(format!("{prefix}-{}", uuid::Uuid::new_v4())).map_err(|error| {
        PortError::new(
            PortErrorKind::Internal,
            format!("failed to generate identifier: {error}"),
        )
    })
}

fn discovered_chrome_build(browser_root: &std::path::Path) -> ApplicationResult<BrowserBuild> {
    let version = installed_chrome_version(browser_root);
    Ok(BrowserBuild {
        id: BrowserBuildId::new("google-chrome-stable-150")?,
        provider: "Google Chrome".to_owned(),
        version,
        channel: BrowserChannel::Stable,
        executable_path: browser_root.join("chrome.exe"),
        sha256: "0".repeat(64),
        publisher: "Google LLC".to_owned(),
        trust: RuntimeTrust::Verified,
        capabilities: BrowserCapabilities {
            supports_language: true,
            supports_proxy: true,
            supports_webrtc_policy: true,
            supports_devtools_pipe: true,
            fingerprint_support: brows_domain::FingerprintSupport::Unverified,
        },
        installed_at_ms: unix_time_ms(),
    })
}

fn installed_chrome_version(root: &std::path::Path) -> String {
    std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| chrome_version_key(&name).map(|key| (key, name)))
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, name)| name)
        .unwrap_or_else(|| "installed".to_owned())
}

fn chrome_version_key(version: &str) -> Option<Vec<u64>> {
    let parts = version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!parts.is_empty()).then_some(parts)
}

#[cfg(test)]
fn prototype_browser_build(data_root: &std::path::Path) -> ApplicationResult<BrowserBuild> {
    Ok(BrowserBuild {
        id: BrowserBuildId::new("chrome-testing-stable")?,
        provider: "Chrome for Testing".to_owned(),
        version: "prototype".to_owned(),
        channel: BrowserChannel::Stable,
        // The in-memory browser port never executes this path. A verified
        // runtime adapter replaces it without changing AppState or commands.
        executable_path: data_root.join("runtimes").join("chrome.exe"),
        sha256: "0".repeat(64),
        publisher: "Google LLC".to_owned(),
        trust: RuntimeTrust::Verified,
        capabilities: BrowserCapabilities {
            supports_language: true,
            supports_proxy: true,
            supports_webrtc_policy: true,
            supports_devtools_pipe: true,
            fingerprint_support: brows_domain::FingerprintSupport::Unverified,
        },
        installed_at_ms: unix_time_ms(),
    })
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_application::SessionLaunchOutcome;
    use brows_contracts::{
        BrowserChannelDto, BrowserSelectorDto, ErrorCode, LocaleDto, PersonaDto,
        ProfileCreateRequest, ProfileDraftDto, ProfileIdRequest, ProfileListRequest,
        ProfileRevisionRequest, ProfileUpdateRequest, SessionCancelQueuedRequest,
        SessionLaunchRequest, SessionStateDto, SessionStopRequest, WindowDto,
    };

    mod browser_fixture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test_support/controlled_browser.rs"
        ));
    }
    use browser_fixture::ControlFixture;

    fn draft(name: &str) -> ProfileDraftDto {
        ProfileDraftDto {
            name: name.to_owned(),
            tags: vec!["prototype".to_owned()],
            preset_id: brows_contracts::PresetIdDto::new("system-native").unwrap(),
            preset_version: 1,
            proxy_id: None,
            browser: BrowserSelectorDto::Channel {
                channel: BrowserChannelDto::Stable,
            },
            persona: PersonaDto::default(),
            locale: LocaleDto {
                accept_languages: vec!["zh-CN".to_owned(), "zh".to_owned()],
                timezone: None,
            },
            window: WindowDto {
                width: 1280,
                height: 800,
                start_maximized: false,
            },
            startup_urls: Vec::new(),
        }
    }

    #[test]
    fn chrome_versions_are_compared_numerically() {
        assert!(chrome_version_key("150.0.1").unwrap() > chrome_version_key("99.9.9").unwrap());
        assert!(chrome_version_key("150.preview").is_none());
    }

    #[test]
    fn saved_concurrency_changes_the_live_queue_and_busy_changes_fail() {
        let root = std::env::temp_dir().join(format!("brows-settings-{}", uuid::Uuid::new_v4()));
        let state = AppState::prototype(root).unwrap();
        let patch =
            serde_json::from_value(serde_json::json!({"maxConcurrentSessions": 1})).unwrap();
        assert_eq!(
            state
                .settings
                .update(patch)
                .unwrap()
                .max_concurrent_sessions,
            1
        );
        for name in ["first", "second"] {
            let profile = state
                .profiles
                .create(ProfileCreateRequest {
                    profile: draft(name),
                })
                .unwrap();
            let result = state
                .sessions
                .launch(SessionLaunchRequest {
                    profile_id: profile.id,
                })
                .unwrap();
            match result {
                SessionLaunchOutcome::Started { .. } => assert_eq!(name, "first"),
                SessionLaunchOutcome::Queued { position, .. } => {
                    assert_eq!(name, "second");
                    assert_eq!(position, 1);
                }
            }
        }
        let patch =
            serde_json::from_value(serde_json::json!({"maxConcurrentSessions": 2})).unwrap();
        assert!(state.settings.update(patch).is_err());
        assert_eq!(state.settings.get().unwrap().max_concurrent_sessions, 1);
    }

    #[test]
    #[ignore = "launches an isolated headless signed Chrome capability probe"]
    fn fingerprint_runtime_real_probe_smoke_test() {
        let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .canonicalize()
            .unwrap();
        let data_root = target.join(format!(
            "fingerprint-runtime-smoke-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&data_root).unwrap();
        let state = AppState::production(data_root.clone()).unwrap();
        let builds = state.browsers.list_builds().unwrap();
        let build = builds.first().unwrap();
        assert_eq!(
            build.capabilities.fingerprint_support,
            brows_domain::FingerprintSupport::Unverified
        );
        let result = state.browsers.probe_fingerprint_support(&build.id).unwrap();
        assert_ne!(
            result.build.capabilities.fingerprint_support,
            brows_domain::FingerprintSupport::Unverified
        );
        println!(
            "{} {}: {:?}; {}",
            result.build.provider,
            result.build.version,
            result.build.capabilities.fingerprint_support,
            result.detail
        );
        assert_eq!(
            state.browsers.list_builds().unwrap()[0]
                .capabilities
                .fingerprint_support,
            result.build.capabilities.fingerprint_support
        );
        assert!(state
            .profiles
            .list(ProfileListRequest::default())
            .unwrap()
            .is_empty());
        assert!(state.sessions.list().unwrap().is_empty());
        assert!(std::fs::read_dir(data_root.join("profiles"))
            .unwrap()
            .next()
            .is_none());
        drop(state);
        // Only this test's freshly created, exact workspace-owned directory.
        let resolved = data_root.canonicalize().unwrap();
        assert_eq!(resolved.parent(), Some(target.as_path()));
        let metadata = std::fs::symlink_metadata(&resolved).unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            assert_eq!(metadata.file_attributes() & 0x400, 0);
        }
        std::fs::remove_dir_all(resolved).unwrap();
    }

    #[test]
    fn profile_vertical_slice_obeys_revisions_and_archive_state() {
        let state = AppState::prototype(PathBuf::from("test-data")).unwrap();
        let created = state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft("Primary"),
            })
            .unwrap();
        let fetched = state
            .profiles
            .get(ProfileIdRequest {
                profile_id: created.id.clone(),
            })
            .unwrap();
        assert_eq!(fetched, created);

        let updated = state
            .profiles
            .update(ProfileUpdateRequest {
                profile_id: created.id.clone(),
                profile: draft("Primary Updated"),
                expected_revision: created.revision,
            })
            .unwrap();
        let stale = state
            .profiles
            .archive(ProfileRevisionRequest {
                profile_id: updated.id.clone(),
                expected_revision: created.revision,
            })
            .unwrap_err();
        assert_eq!(stale.code(), &ErrorCode::RevisionConflict);

        let cloned = state
            .profiles
            .clone_profile(ProfileIdRequest {
                profile_id: updated.id.clone(),
            })
            .unwrap();
        let archived = state
            .profiles
            .archive(ProfileRevisionRequest {
                profile_id: updated.id.clone(),
                expected_revision: updated.revision,
            })
            .unwrap();
        assert!(archived.archived);
        let restored = state
            .profiles
            .restore(ProfileRevisionRequest {
                profile_id: archived.id.clone(),
                expected_revision: archived.revision,
            })
            .unwrap();
        assert!(!restored.archived);

        let visible = state.profiles.list(ProfileListRequest::default()).unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|profile| profile.id == cloned.id));
    }

    #[test]
    fn session_vertical_slice_launches_queues_cancels_and_stops() {
        let state = AppState::prototype_with_capacity(PathBuf::from("test-data"), 1).unwrap();
        let first = state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft("First"),
            })
            .unwrap();
        let second = state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft("Second"),
            })
            .unwrap();

        let running = state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: first.id,
            })
            .unwrap();
        let running = match running {
            SessionLaunchOutcome::Started { session, .. } => session,
            SessionLaunchOutcome::Queued { .. } => panic!("first session must start"),
        };
        assert_eq!(running.state, SessionStateDto::Running);

        let queued = state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: second.id,
            })
            .unwrap();
        let queued = match queued {
            SessionLaunchOutcome::Queued { session, .. } => session,
            SessionLaunchOutcome::Started { .. } => panic!("second session must queue"),
        };
        let cancelled = state
            .sessions
            .cancel_queued(SessionCancelQueuedRequest {
                session_id: queued.id,
            })
            .unwrap();
        assert_eq!(cancelled.state, SessionStateDto::Stopped);

        let stopped = state
            .sessions
            .stop(SessionStopRequest {
                session_id: running.id,
            })
            .unwrap();
        assert_eq!(stopped.state, SessionStateDto::Stopped);
    }

    #[test]
    #[ignore = "requires Windows and launches an isolated signed Edge probe"]
    fn signed_runtime_acceptance() {
        let root = PathBuf::from(
            std::env::var_os("BROWS_ACCEPTANCE_DATA_ROOT").expect("explicit acceptance root"),
        );
        assert_eq!(root.file_name().unwrap(), "brows-acceptance");
        let state = AppState::production(root).unwrap();
        let build = state
            .browsers
            .list_builds()
            .unwrap()
            .into_iter()
            .find(|b| b.provider == "Microsoft Edge")
            .expect("signed Edge registered");
        assert_eq!(build.trust, RuntimeTrust::Verified);
        let result = state.browsers.probe_fingerprint_support(&build.id).unwrap();
        assert_eq!(
            result.build.capabilities.fingerprint_support,
            brows_domain::FingerprintSupport::Supported,
            "{}",
            result.detail
        );
        let fixture = ControlFixture::start(true);
        let header = |headers: &str, key: &str| -> Option<String> {
            headers.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                    .map(|(_, value)| value.trim().to_owned())
            })
        };
        // Header presence is compared against an unmodified instance of the
        // same signed build; missing native Worker hints are not invented.
        let mut native = draft("Edge native header baseline");
        native.browser = BrowserSelectorDto::Pinned {
            build_id: (&build.id).into(),
        };
        native.startup_urls = vec![format!("{}/native/page", fixture.url)];
        let native = state
            .profiles
            .create(ProfileCreateRequest { profile: native })
            .unwrap();
        let native_session = match state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: native.id,
            })
            .unwrap()
        {
            SessionLaunchOutcome::Started { session, .. } => session,
            _ => panic!("native baseline unexpectedly queued"),
        };
        let baseline = fixture.wait_proofs("/native/");
        println!(
            "native contexts: {:?}",
            baseline
                .iter()
                .map(|(_, _, value)| &value["kind"])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            baseline
                .iter()
                .map(|(_, _, value)| value["kind"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            9,
            "all native context samples are required"
        );
        let native_hints: std::collections::BTreeMap<_, _> = baseline
            .iter()
            .map(|(_, headers, first)| {
                (
                    first["kind"].as_str().unwrap().to_owned(),
                    header(headers, "sec-ch-ua-platform"),
                )
            })
            .collect();
        println!("native Client Hints header availability: {native_hints:?}");
        state
            .sessions
            .stop(SessionStopRequest {
                session_id: native_session.id,
            })
            .unwrap();
        let mut started = Vec::new();
        let mut evidence = Vec::new();
        let mut invalid_policy = None;
        for (index, name) in ["Edge acceptance A", "Edge acceptance B"]
            .into_iter()
            .enumerate()
        {
            let mut profile = draft(name);
            profile.browser = BrowserSelectorDto::Pinned {
                build_id: (&build.id).into(),
            };
            profile.persona = PersonaDto::SeedableNoise {
                seed: name.to_owned(),
                platform: brows_contracts::PlatformDto::Windows,
                hardware_concurrency: if index == 0 { 3 } else { 6 },
                device_memory: if index == 0 {
                    brows_contracts::DeviceMemoryDto::Half
                } else {
                    brows_contracts::DeviceMemoryDto::Two
                },
                touch_points: 0,
                screen: brows_contracts::ScreenConfigDto {
                    width: 1920,
                    height: 1080,
                    color_depth: 24,
                    device_scale_factor_hundredths: 100,
                },
                webgl_vendor: String::new(),
                webgl_renderer: String::new(),
                spoof_canvas: true,
                spoof_audio: false,
            };
            profile.locale.accept_languages = if index == 0 {
                vec!["ja-JP".to_owned(), "ja".to_owned()]
            } else {
                vec!["en-US".to_owned(), "en".to_owned()]
            };
            profile.locale.timezone = Some(
                if index == 0 {
                    "Asia/Tokyo"
                } else {
                    "US/Eastern"
                }
                .to_owned(),
            );
            profile.startup_urls = vec![format!("{}/{index}/page", fixture.url)];
            if index == 0 {
                invalid_policy = Some(profile.clone());
            }
            let profile = state
                .profiles
                .create(ProfileCreateRequest { profile })
                .unwrap();
            let outcome = state
                .sessions
                .launch(SessionLaunchRequest {
                    profile_id: profile.id,
                })
                .unwrap();
            match outcome {
                SessionLaunchOutcome::Started { session, .. } => started.push(session),
                SessionLaunchOutcome::Queued { .. } => {
                    panic!("acceptance launch unexpectedly queued")
                }
            }
        }
        for (index, session) in started.into_iter().enumerate() {
            let matching = fixture.wait_proofs(&format!("/{index}/"));
            let id: brows_domain::SessionId = session.id.clone().try_into().unwrap();
            let observed = state.sessions.observe(&id).unwrap();
            println!(
                "session {index} sampled/configured kinds: {:?} / {:?}",
                matching
                    .iter()
                    .map(|(_, _, value)| &value["kind"])
                    .collect::<Vec<_>>(),
                observed
                    .targets
                    .iter()
                    .map(|target| &target.kind)
                    .collect::<Vec<_>>()
            );
            assert!(
                state.browsers.is_running(&id).unwrap(),
                "signed Edge must remain running"
            );
            assert_eq!(
                matching
                    .iter()
                    .map(|(_, _, value)| value["kind"].as_str().unwrap())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                9,
                "all controlled context samples are required"
            );
            assert!(
                observed.targets.iter().any(|t| t.kind == "worker"),
                "worker must be observed"
            );
            for kind in ["shared_worker", "service_worker", "iframe"] {
                assert!(
                    observed.targets.iter().any(|target| target.kind == kind),
                    "{kind} must be observed"
                );
            }
            let cpu = if index == 0 { 3 } else { 6 };
            let timezone = if index == 0 {
                "Asia/Tokyo"
            } else {
                "America/New_York"
            };
            let page_signals = matching
                .iter()
                .find(|(_, _, first)| first["kind"] == "page")
                .unwrap()
                .2
                .clone();
            assert!(!page_signals["hints"].is_null());
            assert_ne!(
                page_signals["canvas"],
                baseline
                    .iter()
                    .find(|(_, _, first)| first["kind"] == "page")
                    .unwrap()
                    .2["canvas"],
                "Canvas noise must be observed, not inferred from configuration"
            );
            for target in observed.targets {
                assert_eq!(target.hardware_concurrency, cpu, "{} CPU", target.kind);
                assert_eq!(target.timezone, timezone, "{} timezone", target.kind);
                assert_eq!(
                    target.client_hints_platform, "Windows",
                    "{} UA-CH",
                    target.kind
                );
            }
            for (_, headers, first) in matching {
                assert_eq!(
                    first["canvas"], page_signals["canvas"],
                    "{} Canvas",
                    first["kind"]
                );
                assert_eq!(
                    first["repeat"], first["canvas"],
                    "{} stable repeat",
                    first["kind"]
                );
                if baseline
                    .iter()
                    .find(|(_, _, native)| native["kind"] == first["kind"])
                    .is_some_and(|(_, _, native)| !native["webgl"].is_null())
                {
                    assert!(
                        !first["webgl"].is_null(),
                        "{} must preserve available WebGL",
                        first["kind"]
                    );
                    assert_eq!(
                        first["webgl"], page_signals["webgl"],
                        "{} WebGL declarations",
                        first["kind"]
                    );
                }
                assert_eq!(first["userAgent"], page_signals["userAgent"]);
                assert_eq!(first["hints"], page_signals["hints"]);
                assert_eq!(first["platform"], "Win32");
                assert_eq!(
                    first["languages"],
                    if index == 0 {
                        serde_json::json!(["ja-JP", "ja"])
                    } else {
                        serde_json::json!(["en-US", "en"])
                    }
                );
                assert_eq!(first["hardwareConcurrency"], cpu);
                assert_eq!(first["deviceMemory"], if index == 0 { 0.5 } else { 2.0 });
                assert_eq!(first["timezone"], timezone);
                assert_eq!(first["offsetWinter"], if index == 0 { -540 } else { 300 });
                assert_eq!(first["offsetSummer"], if index == 0 { -540 } else { 240 });
                let allowed_headers: Vec<_> = headers
                    .lines()
                    .filter(|line| {
                        let lower = line.to_ascii_lowercase();
                        ["user-agent:", "accept-language:", "sec-ch-ua"]
                            .iter()
                            .any(|key| lower.starts_with(key))
                    })
                    .collect();
                println!(
                    "session {index} {} allowed headers: {allowed_headers:?}",
                    first["kind"]
                );
                let hints = header(&headers, "sec-ch-ua-platform");
                let native_has_hints = native_hints
                    .get(first["kind"].as_str().unwrap())
                    .unwrap()
                    .is_some();
                if hints.is_some() || native_has_hints {
                    assert_eq!(
                        hints.as_deref(),
                        Some("\"Windows\""),
                        "{} Client Hints",
                        first["kind"]
                    );
                }
                assert_eq!(
                    header(&headers, "user-agent").as_deref(),
                    first["userAgent"].as_str()
                );
                assert_eq!(
                    header(&headers, "accept-language")
                        .unwrap()
                        .split(',')
                        .next()
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    first["languages"][0].as_str().unwrap()
                );
                evidence.push(serde_json::json!({"profile": index, "first": first, "allowedHeaders": allowed_headers,
                    "clientHintsHeaderStatus": if hints.is_some() { "pass" } else { "not_checked" }}));
            }
            if index == 0 {
                fixture.bump_service_version();
                let updates = fixture.wait_kind_count("/0/", "service_worker", 2);
                assert!(
                    updates.len() >= 2,
                    "updated Service Worker first-script evidence is required"
                );
                let updated = &updates.last().unwrap().2;
                assert_eq!(updated["fixtureVersion"], 1);
                assert_eq!(updated["hardwareConcurrency"], cpu);
                assert_eq!(updated["canvas"], page_signals["canvas"]);
                assert_eq!(updated["repeat"], updated["canvas"]);
                assert_eq!(updated["timezone"], timezone);
                evidence.push(serde_json::json!({"profile": index, "phase":"service-worker-update", "first":updated}));
            }
            let stopped = state
                .sessions
                .stop(SessionStopRequest {
                    session_id: session.id,
                })
                .unwrap();
            assert_eq!(stopped.state, SessionStateDto::Stopped);
        }
        let mut invalid = invalid_policy.unwrap();
        invalid.name = "Edge invalid timezone rejection".to_owned();
        invalid.locale.timezone = Some("Asia/Not_A_Timezone".to_owned());
        invalid.startup_urls.clear();
        let invalid = state
            .profiles
            .create(ProfileCreateRequest { profile: invalid })
            .unwrap();
        assert!(
            state
                .sessions
                .launch(SessionLaunchRequest {
                    profile_id: invalid.id.clone()
                })
                .is_err(),
            "invalid native timezone must fail closed"
        );
        let failed = state
            .sessions
            .list()
            .unwrap()
            .into_iter()
            .find(|session| session.profile_id == invalid.id)
            .unwrap();
        assert_eq!(failed.state, SessionStateDto::Failed);
        let failed_id: brows_domain::SessionId = failed.id.try_into().unwrap();
        assert!(!state.browsers.is_running(&failed_id).unwrap());
        let report = serde_json::json!({"scope":"signed Edge production sessions; native header baseline", "assertionsPassed":true, "complete":false,
            "observations":evidence,"uncovered":["public DNS/ICE", "fonts/GPU/network stack", "uncovered rendering APIs and live Audio"]});
        let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/fingerprint-session-acceptance.json");
        std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }

    #[test]
    #[ignore = "launches two visible signed Chrome processes"]
    fn phase_one_real_chrome_smoke_test() {
        let data_root =
            std::env::temp_dir().join(format!("brows-phase-one-smoke-{}", uuid::Uuid::new_v4()));
        let state = AppState::production(data_root.clone()).unwrap();
        let first = state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft("Phase One A"),
            })
            .unwrap();
        let second = state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft("Phase One B"),
            })
            .unwrap();

        let first_session = match state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: first.id.clone(),
            })
            .unwrap()
        {
            SessionLaunchOutcome::Started { session, .. } => {
                println!("profile A session: {}", session.id.as_str());
                session
            }
            SessionLaunchOutcome::Queued { .. } => panic!("profile A unexpectedly queued"),
        };
        let second_session = match state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: second.id.clone(),
            })
            .unwrap()
        {
            SessionLaunchOutcome::Started { session, .. } => {
                println!("profile B session: {}", session.id.as_str());
                session
            }
            SessionLaunchOutcome::Queued { .. } => panic!("profile B unexpectedly queued"),
        };

        let duplicate = state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: first.id,
            })
            .unwrap_err();
        assert_eq!(duplicate.code(), &ErrorCode::AlreadyExists);

        let profile_directories = std::fs::read_dir(data_root.join("profiles"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        assert_eq!(profile_directories.len(), 2);
        assert!(profile_directories
            .iter()
            .all(|entry| entry.path().join("userdata/Default/Preferences").is_file()));

        std::thread::sleep(std::time::Duration::from_secs(8));
        state
            .sessions
            .stop(SessionStopRequest {
                session_id: first_session.id,
            })
            .unwrap();
        state
            .sessions
            .stop(SessionStopRequest {
                session_id: second_session.id,
            })
            .unwrap();
        drop(state);

        let reopened = AppState::production(data_root.clone()).unwrap();
        let persisted = reopened
            .profiles
            .list(ProfileListRequest::default())
            .unwrap();
        assert_eq!(persisted.len(), 2);
        drop(reopened);
        let _ = std::fs::remove_dir_all(data_root);
    }
}
