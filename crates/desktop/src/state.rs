use brows_adapters_backup::EncryptedBackupService;
use brows_adapters_diagnostic::LocalNetworkDiagnosticRunner;
use brows_adapters_morelogin::MoreLoginProfileDataMigrator;
use brows_adapters_proxy::{StoredProxyRuntime, TcpProxyConnectivityTester};
use brows_adapters_runtime::{verify_authenticode_publisher, ManagedChromiumRuntime};
use brows_adapters_secret_windows::WindowsCredentialStore;
use brows_adapters_sqlite::{
    SqliteAuditSink, SqliteMigrationRecordRepository, SqliteProfileRepository,
    SqliteProxyRepository, SqliteRestoreTaskRepository, SqliteSettingsRepository,
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
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Tauri state contains application services only. Concrete adapters are
/// assembled once and remain hidden behind the application's ports.
#[derive(Debug)]
pub(crate) struct AppState {
    pub(crate) backup_jobs: Arc<crate::backup_jobs::BackupJobs>,
    pub(crate) runtime_issue: Option<String>,
    pub(crate) runtime_installer: Option<Arc<brows_adapters_runtime::OfficialRuntimeInstaller>>,
    pub(crate) fingerprint_measurement: Option<Arc<dyn brows_application::FingerprintMeasurement>>,
    pub(crate) backup_operations: Option<Arc<brows_application::BackupApplicationService>>,
    pub(crate) browsers: Arc<dyn BrowserRuntime>,
    pub(crate) profiles: Arc<ProfileApplicationService>,
    pub(crate) sessions: Arc<SessionApplicationService>,
    pub(crate) settings: Arc<brows_application::SettingsApplicationService>,
    pub(crate) migrations: Option<Arc<MigrationApplicationService>>,
    pub(crate) proxy_management: Option<Arc<ProxyApplicationService>>,
    pub(crate) diagnostics: Option<Arc<LocalNetworkDiagnosticRunner>>,
    pub(crate) backups: Option<Arc<EncryptedBackupService>>,
    // Declared last: release exclusive data-root ownership after all adapters.
    _instance_lock: Option<std::fs::File>,
}

impl AppState {
    /// Production composition root for the phase-one desktop application.
    pub(crate) fn production(data_root: PathBuf) -> ApplicationResult<Self> {
        Self::production_with_runtime(data_root, discover_chrome_root())
    }

    fn production_with_runtime(
        data_root: PathBuf,
        chrome: ApplicationResult<PathBuf>,
    ) -> ApplicationResult<Self> {
        std::fs::create_dir_all(&data_root).map_err(|error| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::Internal,
                format!("failed to create application data directory: {error}"),
            )
        })?;

        let instance_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_root.join("manager.lock"))
            .map_err(|_| {
                brows_application::ApplicationError::new(
                    brows_contracts::ErrorCode::Internal,
                    "无法打开管理器数据锁",
                )
            })?;
        instance_lock.try_lock().map_err(|_| {
            brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::ProfileLocked,
                "此数据目录已由另一管理器实例使用，或无法取得独占锁",
            )
        })?;
        let clock = Arc::new(SystemClock);
        let database_path = data_root.join("profiles.sqlite3");
        let settings_repository = Arc::new(SqliteSettingsRepository::open(&database_path)?);
        let saved_settings =
            brows_application::SettingsRepository::load(settings_repository.as_ref())?;
        let profiles = Arc::new(SqliteProfileRepository::open(&database_path)?);
        let migration_records = Arc::new(SqliteMigrationRecordRepository::open(&database_path)?);
        let proxy_records = Arc::new(SqliteProxyRepository::open(&database_path)?);
        let restore_tasks = Arc::new(SqliteRestoreTaskRepository::open(&database_path)?);
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

        let (browser_root, mut runtime_issue) = match chrome {
            Ok(root) => (root, None),
            Err(_) => (data_root.join("runtimes"), Some("未找到可用浏览器。可在设置中安装官方 Chrome for Testing，或安装 Google Chrome 后重启；已有环境仍可查看和备份。".to_owned())),
        };
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
        let managed_runtimes = data_root.join("runtimes/chrome-for-testing");
        let browsers = Arc::new(ManagedChromiumRuntime::with_roots(
            vec![browser_root.clone(), managed_runtimes.clone()],
            &profile_root,
        )?);
        // The system Chrome installation is discovered at runtime. Verify its
        // Authenticode chain and publisher before recording the current digest,
        // then re-check that digest before every launch. This tolerates an
        // update between app runs while failing closed on changes during a run.
        if runtime_issue.is_none() {
            let admission = (|| -> ApplicationResult<()> {
                let mut chrome_build = discovered_chrome_build(&browser_root)?;
                chrome_build.publisher =
                    verify_authenticode_publisher(&chrome_build.executable_path, &["Google LLC"])?;
                browsers.register_build_with_current_digest(chrome_build, true)?;
                Ok(())
            })();
            if admission.is_err() {
                runtime_issue = Some("系统 Chrome 未通过运行时验证。可修复官方安装后重启，或在设置中安装官方 Chrome for Testing。".to_owned());
            }
        }

        let runtime_installer = Arc::new(brows_adapters_runtime::OfficialRuntimeInstaller::new(
            managed_runtimes,
            browsers.clone(),
        )?);
        let _ = runtime_installer.load_installed();

        let compiler = Arc::new(LaunchPlanCompiler::new(
            presets.clone(),
            browsers.clone(),
            proxies.clone(),
            fingerprints,
            profile_root.clone(),
        )?);
        let profile_operations = Arc::new(brows_application::ProfileOperations::default());
        let profile_service = Arc::new(ProfileApplicationService::new(
            profiles.clone(),
            profile_operations.clone(),
            presets,
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let session_service = Arc::new(SessionApplicationService::new(
            profiles.clone(),
            profile_operations.clone(),
            compiler,
            browsers.clone(),
            proxies,
            audit,
            clock.clone(),
            ids.clone(),
            saved_settings.max_concurrent_sessions,
        )?);
        let settings_service = Arc::new(brows_application::SettingsApplicationService::new(
            settings_repository,
            session_service.clone(),
        ));
        let migration_service = Arc::new(MigrationApplicationService::new(
            profiles.clone(),
            session_service.clone(),
            Arc::new(MoreLoginProfileDataMigrator::new(
                if cfg!(windows) {
                    default_morelogin_cache_root()
                } else {
                    data_root.join("morelogin-cache")
                },
                profile_root,
            )?),
            migration_records,
            clock.clone(),
        ));
        // Recovery must finish or retain per-profile launch reservations before
        // the supervisor/UI can admit any sessions.
        migration_service.recover_incomplete()?;
        let proxy_management = Arc::new(ProxyApplicationService::new(
            proxy_records,
            credential_store,
            Arc::new(TcpProxyConnectivityTester),
            ids,
        ));

        let backup_operations = Some(Arc::new(brows_application::BackupApplicationService::new(
            profile_service.clone(),
            session_service.clone(),
            backup_service.clone(),
            restore_tasks,
            clock,
        )));
        // The data-root file lock is held and no UI/supervisor can start work yet.
        if let Some(backups) = &backup_operations {
            let _ = backups.recover_incomplete();
        }
        Ok(Self {
            backup_jobs: Arc::new(crate::backup_jobs::BackupJobs::default()),
            runtime_issue,
            runtime_installer: Some(runtime_installer),
            fingerprint_measurement: Some(Arc::new(
                brows_adapters_runtime::LocalFingerprintMeasurement::new(browsers.clone()),
            )),
            backup_operations,
            browsers: browsers.clone(),
            profiles: profile_service,
            settings: settings_service,
            sessions: session_service,
            migrations: Some(migration_service),
            proxy_management: Some(proxy_management),
            diagnostics: Some(diagnostics),
            backups: Some(backup_service),
            _instance_lock: Some(instance_lock),
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
        let profile_operations = Arc::new(brows_application::ProfileOperations::default());
        let profile_service = Arc::new(ProfileApplicationService::new(
            profiles.clone(),
            profile_operations.clone(),
            presets,
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let session_service = Arc::new(SessionApplicationService::new(
            profiles,
            profile_operations,
            compiler,
            browsers.clone(),
            proxies,
            audit,
            clock,
            ids,
            max_concurrent_sessions,
        )?);

        let settings_repository = Arc::new(brows_application::memory::InMemorySettingsRepository(
            std::sync::Mutex::new(brows_domain::ManagerSettings {
                max_concurrent_sessions,
            }),
        ));
        let settings_service = Arc::new(brows_application::SettingsApplicationService::new(
            settings_repository,
            session_service.clone(),
        ));
        Ok(Self {
            backup_jobs: Arc::new(crate::backup_jobs::BackupJobs::default()),
            runtime_issue: None,
            runtime_installer: None,
            fingerprint_measurement: None,
            backup_operations: None,
            browsers: browsers.clone(),
            profiles: profile_service,
            settings: settings_service,
            sessions: session_service,
            migrations: None,
            proxy_management: None,
            diagnostics: None,
            backups: None,
            _instance_lock: None,
        })
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.backup_jobs.stop_accepting();
        self.browsers.stop_accepting_probes();
        if let Some(installer) = &self.runtime_installer {
            installer.stop_accepting();
        }
        let _ = self.sessions.shutdown();
        self.backup_jobs.shutdown();
        if let Some(installer) = &self.runtime_installer {
            installer.shutdown();
        }
        self.browsers.wait_for_probes();
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

    #[test]
    fn missing_or_rejected_chrome_keeps_the_management_services_available() {
        for missing in [true, false] {
            let root =
                std::env::temp_dir().join(format!("brows-recovery-{}", uuid::Uuid::new_v4()));
            let chrome = if missing {
                Err(brows_application::ApplicationError::new(
                    brows_contracts::ErrorCode::CapabilityUnsupported,
                    "missing",
                ))
            } else {
                let chrome_root = root.join("invalid-chrome");
                std::fs::create_dir_all(&chrome_root).unwrap();
                std::fs::write(chrome_root.join("chrome.exe"), b"not an executable").unwrap();
                Ok(chrome_root)
            };
            let state = AppState::production_with_runtime(root.clone(), chrome).unwrap();
            assert!(state.runtime_issue.is_some());
            assert!(state.browsers.list_builds().unwrap().is_empty());
            assert!(state
                .profiles
                .list(brows_contracts::ProfileListRequest {
                    page: 1,
                    page_size: 20,
                    search: None,
                    include_archived: true,
                })
                .unwrap()
                .is_empty());
            assert!(state.backup_operations.is_some());
            assert!(
                AppState::production_with_runtime(
                    root.clone(),
                    Err(brows_application::ApplicationError::new(
                        brows_contracts::ErrorCode::CapabilityUnsupported,
                        "missing"
                    ))
                )
                .is_err(),
                "a second manager must not share profile ownership"
            );
            state.sessions.shutdown().unwrap();
            drop(state);
            let reopened = AppState::production_with_runtime(
                root.clone(),
                Err(brows_application::ApplicationError::new(
                    brows_contracts::ErrorCode::CapabilityUnsupported,
                    "missing",
                )),
            )
            .unwrap();
            drop(reopened);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn settings_are_restored_on_restart_without_a_browser_installation() {
        let root = std::env::temp_dir().join(format!("brows-settings-{}", uuid::Uuid::new_v4()));
        let missing = || {
            Err(brows_application::ApplicationError::new(
                brows_contracts::ErrorCode::CapabilityUnsupported,
                "missing",
            ))
        };
        let state = AppState::production_with_runtime(root.clone(), missing()).unwrap();
        assert_eq!(state.settings.get().unwrap().max_concurrent_sessions, 10);
        let saved = state
            .settings
            .update(brows_contracts::SettingsUpdateRequest {
                changes: brows_contracts::SettingsPatchDto {
                    max_concurrent_sessions: Some(2),
                    ..Default::default()
                },
            })
            .unwrap();
        assert_eq!(saved.max_concurrent_sessions, 2);
        assert!(saved.stop_sessions_on_exit);
        drop(state);
        let state = AppState::production_with_runtime(root.clone(), missing()).unwrap();
        assert_eq!(state.settings.get().unwrap().max_concurrent_sessions, 2);
        state
            .settings
            .update(brows_contracts::SettingsUpdateRequest {
                changes: brows_contracts::SettingsPatchDto {
                    max_concurrent_sessions: Some(3),
                    ..Default::default()
                },
            })
            .unwrap();
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn startup_cleans_owned_runtime_downloads_and_preserves_rejected_packages() {
        let root =
            std::env::temp_dir().join(format!("brows-runtime-startup-{}", uuid::Uuid::new_v4()));
        let missing = || {
            Err(brows_application::ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                "missing",
            ))
        };
        drop(AppState::production_with_runtime(root.clone(), missing()).unwrap());
        let managed = root.join("runtimes/chrome-for-testing");
        let staging = managed.join(format!(".install-{}", "a".repeat(32)));
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("download.zip"), b"partial download").unwrap();
        let rejected = managed.join("cft-unknown");
        std::fs::create_dir(&rejected).unwrap();
        std::fs::write(rejected.join("receipt.json"), b"invalid receipt").unwrap();
        let state = AppState::production_with_runtime(root.clone(), missing()).unwrap();
        let installer = state.runtime_installer.as_ref().unwrap();
        assert!(!installer.status().detail.is_empty());
        assert!(!staging.exists());
        assert_eq!(
            std::fs::read(rejected.join("receipt.json")).unwrap(),
            b"invalid receipt"
        );
        assert!(state.browsers.list_builds().unwrap().is_empty());
        assert!(state.profiles.list(ProfileListRequest::default()).is_ok());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_recovers_migration_and_rollback_boundaries_and_locks_ambiguous_profiles() {
        use brows_application::{
            LegacyProfileDataMigrator, MigrationRecordRepository, MigrationRecordStatus,
            ProfileRepository,
        };
        for phase in 0..9 {
            let root = std::env::temp_dir()
                .join(format!("brows-migration-restart-{}", uuid::Uuid::new_v4()));
            let missing = || {
                Err(brows_application::ApplicationError::new(
                    ErrorCode::CapabilityUnsupported,
                    "missing",
                ))
            };
            drop(AppState::production_with_runtime(root.clone(), missing()).unwrap());
            let database = root.join("profiles.sqlite3");
            let profiles = SqliteProfileRepository::open(&database).unwrap();
            let original = draft("Migration target")
                .into_domain(
                    ProfileId::new("migration-target").unwrap(),
                    brows_domain::Revision::FIRST,
                    false,
                )
                .unwrap();
            profiles.create(original.clone()).unwrap();
            let records = SqliteMigrationRecordRepository::open(&database).unwrap();
            let task = records
                .begin(&original.id, "morelogin", "12345678", 100)
                .unwrap();
            let source = root.join("source/chrome_12345678");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("state"), b"imported").unwrap();
            let migrator =
                MoreLoginProfileDataMigrator::new(root.join("source"), root.join("profiles"))
                    .unwrap();
            let key = format!("migration-{}", task.id);
            let artifact = migrator.migrate(&original.id, "12345678", &key).unwrap();
            let directory = std::fs::read_dir(root.join("profiles"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let target = directory.join("userdata");
            let backup = directory.join(&artifact.backup_key);
            let staging = directory.join(format!("userdata.staging-{key}"));
            std::fs::write(backup.join("state"), b"original").unwrap();
            match phase {
                0 => {
                    std::fs::rename(&target, &staging).unwrap();
                    std::fs::rename(&backup, &target).unwrap();
                }
                1 => {
                    std::fs::rename(&target, &staging).unwrap();
                }
                2 => {}
                3 => {
                    std::fs::rename(
                        &target,
                        directory.join(format!("userdata.interrupted-{key}")),
                    )
                    .unwrap();
                }
                4 => {
                    records.complete(task.id, 101, &artifact).unwrap();
                }
                5 => {
                    records.complete(task.id, 101, &artifact).unwrap();
                    records.begin_rollback(task.id).unwrap();
                    std::fs::rename(
                        &target,
                        directory.join(format!("userdata.after-rollback-{key}")),
                    )
                    .unwrap();
                }
                6 => {
                    records.complete(task.id, 101, &artifact).unwrap();
                    records.begin_rollback(task.id).unwrap();
                    migrator
                        .rollback(&original.id, &artifact.backup_key)
                        .unwrap();
                }
                7 => {
                    std::fs::create_dir(&staging).unwrap();
                    records
                        .begin(&original.id, "morelogin", "12345678", 102)
                        .unwrap();
                }
                8 => {
                    // A legacy failure followed by a newer committed migration
                    // cannot be reconstructed by reverting the older backup.
                    records.fail(task.id, 101, "legacy failure").unwrap();
                    let newer = records
                        .begin(&original.id, "morelogin", "12345678", 102)
                        .unwrap();
                    let newer_backup = format!("userdata.backup-migration-{}", newer.id);
                    std::fs::create_dir(directory.join(&newer_backup)).unwrap();
                    std::fs::write(directory.join(&newer_backup).join("state"), b"imported")
                        .unwrap();
                    std::fs::write(target.join("state"), b"latest").unwrap();
                    records
                        .complete(
                            newer.id,
                            103,
                            &brows_application::LegacyMigrationArtifact {
                                backup_key: newer_backup,
                                copied_files: 1,
                                copied_bytes: 6,
                            },
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            drop(records);
            drop(profiles);
            drop(migrator);
            // Production's legacy source does not exist. Recovery must use only
            // the durable journal and target layout, not the original cache.
            std::fs::remove_dir_all(root.join("source")).unwrap();
            for attempt in 0..2 {
                let state = AppState::production_with_runtime(root.clone(), missing()).unwrap();
                let migrations = state.migrations.as_ref().unwrap();
                if phase == 8 {
                    assert!(migrations.recovery_pending());
                    assert!(state
                        .sessions
                        .reserve_stopped_profile(&original.id)
                        .is_err());
                    assert!(migrations.recover_incomplete().unwrap());
                    assert_eq!(std::fs::read(target.join("state")).unwrap(), b"latest");
                    assert_eq!(std::fs::read(backup.join("state")).unwrap(), b"original");
                    drop(state);
                    continue;
                }
                if phase == 7 && attempt == 0 {
                    assert!(migrations.recovery_pending());
                    assert!(state
                        .sessions
                        .reserve_stopped_profile(&original.id)
                        .is_err());
                    assert!(state
                        .profiles
                        .archive(ProfileRevisionRequest {
                            profile_id: (&original.id).into(),
                            expected_revision: original.revision.get()
                        })
                        .is_err());
                    assert_eq!(std::fs::read(target.join("state")).unwrap(), b"imported");
                    std::fs::remove_dir(&staging).unwrap();
                    assert!(!migrations.recover_incomplete().unwrap());
                }
                assert!(!migrations.recovery_pending());
                let record = migrations
                    .list(20)
                    .unwrap()
                    .into_iter()
                    .find(|r| r.id == task.id)
                    .unwrap();
                assert!(!record.recovery_pending);
                assert_eq!(
                    record.status,
                    if phase == 4 {
                        MigrationRecordStatus::Completed
                    } else if (5..=6).contains(&phase) {
                        MigrationRecordStatus::RolledBack
                    } else {
                        MigrationRecordStatus::Failed
                    }
                );
                assert_eq!(
                    std::fs::read(target.join("state")).unwrap(),
                    if phase == 4 {
                        b"imported".as_slice()
                    } else {
                        b"original".as_slice()
                    }
                );
                assert!(state.sessions.reserve_stopped_profile(&original.id).is_ok());
                drop(state);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn startup_recovers_each_restore_boundary_without_activating_interrupted_profiles() {
        use brows_application::{ProfileRepository, RestoreTaskRepository, RestoreTaskStatus};
        for phase in 0..5 {
            let root = std::env::temp_dir()
                .join(format!("brows-restore-restart-{}", uuid::Uuid::new_v4()));
            let missing = || {
                Err(brows_application::ApplicationError::new(
                    brows_contracts::ErrorCode::CapabilityUnsupported,
                    "missing",
                ))
            };
            drop(AppState::production_with_runtime(root.clone(), missing()).unwrap());
            // Production imports allocate staging below a canonical root. On
            // Windows that includes the verbatim path prefix; keep the manual
            // crash fixture consistent with the path returned by the importer.
            let root = root.canonicalize().unwrap();
            let database = root.join("profiles.sqlite3");
            let profiles = SqliteProfileRepository::open(&database).unwrap();
            let original = draft("Existing")
                .into_domain(
                    ProfileId::new("existing").unwrap(),
                    brows_domain::Revision::FIRST,
                    false,
                )
                .unwrap();
            profiles.create(original.clone()).unwrap();
            let restored = draft("Restored")
                .into_domain(
                    ProfileId::new("fresh-restored").unwrap(),
                    brows_domain::Revision::FIRST,
                    true,
                )
                .unwrap();
            let tasks = SqliteRestoreTaskRepository::open(&database).unwrap();
            let task = tasks.begin(1_000).unwrap();
            if phase >= 1 {
                tasks.bind_profile(task.id, &restored.id).unwrap();
            }
            if phase >= 2 {
                profiles.create(restored.clone()).unwrap();
            }
            let stage = root
                .join("backups/restore-staging")
                .join(format!("task-{:016x}", task.id));
            std::fs::create_dir_all(stage.join("userdata/Default")).unwrap();
            std::fs::write(stage.join("userdata/Default/state"), b"restored state").unwrap();
            let sentinel = root.join("profiles/existing-data");
            std::fs::create_dir_all(&sentinel).unwrap();
            std::fs::write(sentinel.join("state"), b"existing state").unwrap();
            if phase >= 3 {
                EncryptedBackupService::new(root.join("backups"), root.join("profiles"))
                    .unwrap()
                    .install_restored_data(restored.id.as_str(), &stage.join("userdata"))
                    .unwrap();
            }
            if phase == 4 {
                tasks.complete(task.id, &restored, 1_001).unwrap();
            }
            drop(tasks);
            drop(profiles);

            for _ in 0..2 {
                let state = AppState::production_with_runtime(root.clone(), missing()).unwrap();
                let operations = state.backup_operations.as_ref().unwrap();
                let records = operations.restore_tasks().unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(
                    records[0].status,
                    if phase == 4 {
                        RestoreTaskStatus::Completed
                    } else {
                        RestoreTaskStatus::Interrupted
                    }
                );
                assert!(!records[0].cleanup_pending);
                assert!(!operations.cleanup_pending());
                assert!(!stage.exists());
                let profile_repository = SqliteProfileRepository::open(&database).unwrap();
                assert_eq!(profile_repository.get(&original.id).unwrap(), original);
                if phase >= 2 {
                    assert_eq!(
                        profile_repository.get(&restored.id).unwrap().archived,
                        phase != 4
                    );
                }
                if phase >= 3 {
                    let installed = std::fs::read_dir(root.join("profiles"))
                        .unwrap()
                        .map(Result::unwrap)
                        .find(|entry| entry.file_name().to_string_lossy().len() == 64)
                        .unwrap();
                    assert_eq!(
                        std::fs::read(installed.path().join("userdata/Default/state")).unwrap(),
                        b"restored state"
                    );
                }
                assert_eq!(
                    std::fs::read(sentinel.join("state")).unwrap(),
                    b"existing state"
                );
                drop(profile_repository);
                drop(state);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
