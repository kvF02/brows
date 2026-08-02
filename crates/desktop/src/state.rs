use brows_adapters_morelogin::MoreLoginProfileDataMigrator;
use brows_adapters_proxy::{StoredProxyRuntime, TcpProxyConnectivityTester};
use brows_adapters_runtime::ManagedChromiumRuntime;
use brows_adapters_secret_windows::WindowsCredentialStore;
use brows_adapters_sqlite::{
    SqliteMigrationRecordRepository, SqliteProfileRepository, SqliteProxyRepository,
};
#[cfg(test)]
use brows_application::memory::InMemoryProxyRuntime;
#[cfg(test)]
use brows_application::memory::{
    InMemoryBrowserRuntime, InMemoryProfileRepository, ManualClock, SequentialIdGenerator,
};
use brows_application::{
    memory::{
        system_native_preset, InMemoryAuditSink, InMemoryPresetCatalog,
        SystemNativeFingerprintRuntime,
    },
    ApplicationResult, Clock, IdGenerator, LaunchPlanCompiler, MigrationApplicationService,
    PortError, PortErrorKind, PortResult, ProfileApplicationService, ProxyApplicationService,
    SessionApplicationService,
};
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserCapabilities, BrowserChannel, ProfileId, RuntimeTrust,
    SecretRef, SessionId, DEFAULT_MAX_CONCURRENT_SESSIONS,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Tauri state contains application services only. Concrete adapters are
/// assembled once and remain hidden behind the application's ports.
#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) profiles: Arc<ProfileApplicationService>,
    pub(crate) sessions: Arc<SessionApplicationService>,
    pub(crate) migrations: Option<Arc<MigrationApplicationService>>,
    pub(crate) proxy_management: Option<Arc<ProxyApplicationService>>,
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
        let profiles = Arc::new(SqliteProfileRepository::open(&database_path)?);
        let migration_records = Arc::new(SqliteMigrationRecordRepository::open(&database_path)?);
        let proxy_records = Arc::new(SqliteProxyRepository::open(&database_path)?);
        let presets = Arc::new(InMemoryPresetCatalog::default());
        let proxies = Arc::new(StoredProxyRuntime::new(proxy_records.clone()));
        let fingerprints = Arc::new(SystemNativeFingerprintRuntime);
        let audit = Arc::new(InMemoryAuditSink::default());
        let ids = Arc::new(UuidIdGenerator);

        presets.insert(system_native_preset())?;

        let browser_root = PathBuf::from(r"C:\Program Files\Google\Chrome\Application");
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
        let browsers = Arc::new(ManagedChromiumRuntime::new(&browser_root, &profile_root)?);
        browsers.register_build(verified_chrome_build(&browser_root)?, true)?;

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
            browsers,
            proxies,
            audit,
            clock.clone(),
            ids.clone(),
            DEFAULT_MAX_CONCURRENT_SESSIONS,
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
            Arc::new(WindowsCredentialStore::new()),
            Arc::new(TcpProxyConnectivityTester),
            ids,
        ));

        Ok(Self {
            profiles: profile_service,
            sessions: session_service,
            migrations: Some(migration_service),
            proxy_management: Some(proxy_management),
        })
    }

    #[cfg(test)]
    pub(crate) fn prototype(data_root: PathBuf) -> ApplicationResult<Self> {
        Self::prototype_with_capacity(data_root, DEFAULT_MAX_CONCURRENT_SESSIONS)
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
            browsers,
            proxies,
            audit,
            clock,
            ids,
            max_concurrent_sessions,
        )?);

        Ok(Self {
            profiles: profile_service,
            sessions: session_service,
            migrations: None,
            proxy_management: None,
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

fn verified_chrome_build(browser_root: &std::path::Path) -> ApplicationResult<BrowserBuild> {
    Ok(BrowserBuild {
        id: BrowserBuildId::new("google-chrome-stable-150")?,
        provider: "Google Chrome".to_owned(),
        version: "150.0.7871.188".to_owned(),
        channel: BrowserChannel::Stable,
        executable_path: browser_root.join("chrome.exe"),
        sha256: "36bd4eb0917605dcdda6458215d5657b320b0cef1b29bc03a1aa10976159162a".to_owned(),
        publisher: "Google LLC".to_owned(),
        trust: RuntimeTrust::Verified,
        capabilities: BrowserCapabilities {
            supports_language: true,
            supports_proxy: true,
            supports_webrtc_policy: true,
            supports_devtools_pipe: true,
        },
        installed_at_ms: unix_time_ms(),
    })
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
