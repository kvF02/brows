use crate::{memory::*, *};
use brows_contracts::{
    BrowserChannelDto, BrowserSelectorDto, ErrorCode, LocaleDto, PersonaDto, ProfileCreateRequest,
    ProfileDraftDto, ProfileIdRequest, ProfileListRequest, ProfileRevisionRequest,
    ProfileUpdateRequest, SessionLaunchRequest, SessionStateDto, SessionStopRequest, WindowDto,
};
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserCapabilities, BrowserChannel, DnsMode, ProfileId, ProxyId,
    ProxyScheme, ProxySpec, RuntimeTrust, SecretRef, SessionId,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Debug)]
struct Harness {
    profiles: Arc<InMemoryProfileRepository>,
    browsers: Arc<InMemoryBrowserRuntime>,
    proxies: Arc<InMemoryProxyRuntime>,
    audit: Arc<InMemoryAuditSink>,
    clock: Arc<ManualClock>,
    profile_service: Arc<ProfileApplicationService>,
    session_service: Arc<SessionApplicationService>,
    compiler: Arc<LaunchPlanCompiler>,
}

fn settings_patch(limit: u32) -> brows_contracts::SettingsUpdateRequest {
    brows_contracts::SettingsUpdateRequest {
        changes: brows_contracts::SettingsPatchDto {
            max_concurrent_sessions: Some(limit),
            ..Default::default()
        },
    }
}

#[test]
fn settings_resize_live_sessions_fifo_and_drain_after_lowering_the_limit() {
    let h = Harness::new(1);
    let repository = Arc::new(InMemorySettingsRepository(std::sync::Mutex::new(
        brows_domain::ManagerSettings {
            max_concurrent_sessions: 1,
        },
    )));
    let settings = SettingsApplicationService::new(repository.clone(), h.session_service.clone());
    let profiles: Vec<_> = (0..5)
        .map(|n| h.create_profile(&format!("Resize {n}")))
        .collect();
    for profile in &profiles {
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: profile.id.clone(),
            })
            .unwrap();
    }
    assert_eq!(
        settings
            .update(settings_patch(3))
            .unwrap()
            .max_concurrent_sessions,
        3
    );
    let sessions = h.session_service.list().unwrap();
    for (index, profile) in profiles.iter().enumerate() {
        assert_eq!(
            sessions
                .iter()
                .find(|s| s.profile_id == profile.id)
                .unwrap()
                .state,
            if index < 3 {
                SessionStateDto::Running
            } else {
                SessionStateDto::Queued
            }
        );
    }
    assert_eq!(repository.load().unwrap().max_concurrent_sessions, 3);
    settings.update(settings_patch(1)).unwrap();
    for profile in &profiles[..2] {
        let session = h
            .session_service
            .list()
            .unwrap()
            .into_iter()
            .find(|s| s.profile_id == profile.id)
            .unwrap();
        h.session_service
            .stop(SessionStopRequest {
                session_id: session.id,
            })
            .unwrap();
        assert_eq!(
            h.session_service
                .list()
                .unwrap()
                .iter()
                .filter(|s| s.state == SessionStateDto::Queued)
                .count(),
            2
        );
    }
    let session = h
        .session_service
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.profile_id == profiles[2].id)
        .unwrap();
    h.session_service
        .stop(SessionStopRequest {
            session_id: session.id,
        })
        .unwrap();
    assert_eq!(
        h.browsers.running_plans().unwrap()[0].profile_id.as_str(),
        profiles[3].id.as_str()
    );
    assert_eq!(repository.load().unwrap().max_concurrent_sessions, 1);
}

#[derive(Debug)]
struct UnwritableSettings;
impl SettingsRepository for UnwritableSettings {
    fn load(&self) -> PortResult<brows_domain::ManagerSettings> {
        Ok(brows_domain::ManagerSettings {
            max_concurrent_sessions: 1,
        })
    }
    fn save(&self, _: &brows_domain::ManagerSettings) -> PortResult<()> {
        Err(PortError::new(
            PortErrorKind::Unavailable,
            "simulated write failure",
        ))
    }
}

#[test]
fn failed_settings_write_leaves_live_capacity_and_queue_unchanged() {
    let h = Harness::new(1);
    let settings =
        SettingsApplicationService::new(Arc::new(UnwritableSettings), h.session_service.clone());
    for n in 0..3 {
        let p = h.create_profile(&format!("Write failure {n}"));
        h.session_service
            .launch(SessionLaunchRequest { profile_id: p.id })
            .unwrap();
    }
    let before = h.session_service.list().unwrap();
    assert!(settings.update(settings_patch(3)).is_err());
    assert_eq!(h.session_service.list().unwrap(), before);
    assert_eq!(h.browsers.running_plans().unwrap().len(), 1);
    assert_eq!(settings.get().unwrap().max_concurrent_sessions, 1);
    let p = h.create_profile("Must still queue");
    assert!(matches!(
        h.session_service
            .launch(SessionLaunchRequest { profile_id: p.id })
            .unwrap(),
        SessionLaunchOutcome::Queued { .. }
    ));
}

#[test]
fn invalid_or_locked_settings_do_not_partially_save_other_changes() {
    let h = Harness::new(1);
    let repository = Arc::new(InMemorySettingsRepository(std::sync::Mutex::new(
        brows_domain::ManagerSettings {
            max_concurrent_sessions: 1,
        },
    )));
    let settings = SettingsApplicationService::new(repository.clone(), h.session_service.clone());
    for limit in [0, 11, u32::MAX] {
        assert_eq!(
            settings.update(settings_patch(limit)).unwrap_err().code(),
            &ErrorCode::ValidationFailed
        );
    }
    for changes in [
        brows_contracts::SettingsPatchDto {
            stop_sessions_on_exit: Some(false),
            ..Default::default()
        },
        brows_contracts::SettingsPatchDto {
            telemetry_enabled: Some(true),
            ..Default::default()
        },
        brows_contracts::SettingsPatchDto {
            check_updates: Some(true),
            ..Default::default()
        },
        brows_contracts::SettingsPatchDto {
            retain_archived_days: Some(0),
            ..Default::default()
        },
    ] {
        let request = brows_contracts::SettingsUpdateRequest {
            changes: brows_contracts::SettingsPatchDto {
                max_concurrent_sessions: Some(3),
                ..changes
            },
        };
        assert_eq!(
            settings.update(request).unwrap_err().code(),
            &ErrorCode::CapabilityUnsupported
        );
        assert_eq!(repository.load().unwrap().max_concurrent_sessions, 1);
    }
    h.session_service.shutdown().unwrap();
    assert!(settings.update(settings_patch(3)).is_err());
    assert_eq!(repository.load().unwrap().max_concurrent_sessions, 1);
}

#[test]
fn resize_keeps_fifo_when_a_promoted_browser_fails_to_start() {
    let h = Harness::new(1);
    let repository = Arc::new(InMemorySettingsRepository::default());
    let settings = SettingsApplicationService::new(repository, h.session_service.clone());
    let profiles: Vec<_> = (0..4)
        .map(|n| h.create_profile(&format!("Failing promotion {n}")))
        .collect();
    for p in &profiles {
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: p.id.clone(),
            })
            .unwrap();
    }
    h.browsers.fail_next_launch();
    assert_eq!(
        settings
            .update(settings_patch(3))
            .unwrap()
            .max_concurrent_sessions,
        3
    );
    let launched: Vec<_> = h
        .audit
        .events()
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "session.launch" && e.outcome == AuditOutcome::Succeeded)
        .collect();
    let sessions = h.session_service.list().unwrap();
    let launched_profiles: Vec<_> = launched
        .iter()
        .map(|event| {
            sessions
                .iter()
                .find(|s| s.id.as_str() == event.entity_id)
                .unwrap()
                .profile_id
                .clone()
        })
        .collect();
    assert_eq!(
        launched_profiles,
        vec![
            profiles[0].id.clone(),
            profiles[2].id.clone(),
            profiles[3].id.clone()
        ]
    );
    assert_eq!(h.browsers.running_plans().unwrap().len(), 3);
}

impl Harness {
    fn new(limit: usize) -> Self {
        let profiles = Arc::new(InMemoryProfileRepository::default());
        let presets = Arc::new(InMemoryPresetCatalog::default());
        presets.insert(system_native_preset()).unwrap();
        let clock = Arc::new(ManualClock::new(1_000));
        let browsers = Arc::new(InMemoryBrowserRuntime::new(clock.clone()));
        browsers.insert_build(verified_build()).unwrap();
        let proxies = Arc::new(InMemoryProxyRuntime::default());
        let audit = Arc::new(InMemoryAuditSink::default());
        let ids = Arc::new(SequentialIdGenerator::default());
        let fingerprints = Arc::new(SystemNativeFingerprintRuntime);
        let compiler = Arc::new(
            LaunchPlanCompiler::new(
                presets.clone(),
                browsers.clone(),
                proxies.clone(),
                fingerprints,
                PathBuf::from("C:\\BrowsData\\profiles"),
            )
            .unwrap(),
        );
        let profile_operations = Arc::new(crate::ProfileOperations::default());
        let profile_service = ProfileApplicationService::new(
            profiles.clone(),
            profile_operations.clone(),
            presets.clone(),
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        );
        let session_service = SessionApplicationService::new(
            profiles.clone(),
            profile_operations.clone(),
            compiler.clone(),
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
            limit,
        )
        .unwrap();
        Self {
            profiles,
            browsers,
            proxies,
            audit,
            clock,
            profile_service: Arc::new(profile_service),
            session_service: Arc::new(session_service),
            compiler,
        }
    }

    fn create_profile(&self, name: &str) -> brows_contracts::ProfileDto {
        self.profile_service
            .create(ProfileCreateRequest {
                profile: draft(name, None),
            })
            .unwrap()
    }
}

fn verified_build() -> BrowserBuild {
    BrowserBuild {
        id: BrowserBuildId::new("chrome-stable-1").unwrap(),
        provider: "test-provider".to_owned(),
        version: "1.0.0".to_owned(),
        channel: BrowserChannel::Stable,
        executable_path: PathBuf::from("C:\\Program Files\\Brows\\chrome.exe"),
        sha256: "a".repeat(64),
        publisher: "Verified Test Publisher".to_owned(),
        trust: RuntimeTrust::Verified,
        capabilities: BrowserCapabilities {
            supports_language: true,
            supports_proxy: true,
            supports_webrtc_policy: true,
            supports_devtools_pipe: true,
            fingerprint_support: brows_domain::FingerprintSupport::Supported,
        },
        installed_at_ms: 1,
    }
}

fn draft(name: &str, proxy_id: Option<&ProxyId>) -> ProfileDraftDto {
    ProfileDraftDto {
        name: name.to_owned(),
        tags: vec!["qa".to_owned()],
        preset_id: brows_contracts::PresetIdDto::new("system-native").unwrap(),
        preset_version: 1,
        proxy_id: proxy_id.map(Into::into),
        browser: BrowserSelectorDto::Channel {
            channel: BrowserChannelDto::Stable,
        },
        persona: PersonaDto::default(),
        locale: LocaleDto {
            accept_languages: vec!["en-US".to_owned(), "en".to_owned()],
            timezone: None,
        },
        window: WindowDto {
            width: 1280,
            height: 800,
            start_maximized: false,
        },
        startup_urls: vec!["https://example.test".to_owned()],
    }
}

#[test]
fn profile_service_enforces_revision_and_manages_clone_archive() {
    let harness = Harness::new(1);
    let created = harness.create_profile("Research");
    assert_eq!(created.revision, 1);

    let updated = harness
        .profile_service
        .update(ProfileUpdateRequest {
            profile_id: created.id.clone(),
            profile: draft("Research Updated", None),
            expected_revision: 1,
        })
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.name, "Research Updated");

    let conflict = harness
        .profile_service
        .update(ProfileUpdateRequest {
            profile_id: created.id.clone(),
            profile: draft("Stale", None),
            expected_revision: 1,
        })
        .unwrap_err();
    assert_eq!(conflict.code(), &ErrorCode::RevisionConflict);

    let cloned = harness
        .profile_service
        .clone_profile(ProfileIdRequest {
            profile_id: created.id.clone(),
        })
        .unwrap();
    assert_ne!(cloned.id, created.id);
    assert_eq!(cloned.revision, 1);
    assert!(cloned.name.ends_with(" Copy"));

    let archived = harness
        .profile_service
        .archive(ProfileRevisionRequest {
            profile_id: created.id.clone(),
            expected_revision: updated.revision,
        })
        .unwrap();
    assert!(archived.archived);
    let visible = harness
        .profile_service
        .list(ProfileListRequest::default())
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(harness.audit.events().unwrap().len(), 4);
}

#[test]
fn compiler_produces_typed_deterministic_plan_and_safe_directory() {
    let harness = Harness::new(1);
    let created = harness.create_profile("Plan");
    let id: ProfileId = created.id.try_into().unwrap();
    let profile = harness.profiles.get(&id).unwrap();
    let session_id = SessionId::new("session-static").unwrap();
    let first = harness.compiler.compile(&profile, &session_id).unwrap();
    let second = harness.compiler.compile(&profile, &session_id).unwrap();
    assert_eq!(first.plan.plan_hash, second.plan.plan_hash);
    assert_eq!(first.plan.user_data_dir, second.plan.user_data_dir);
    assert!(first
        .plan
        .user_data_dir
        .starts_with("C:\\BrowsData\\profiles"));
    assert!(!first
        .plan
        .user_data_dir
        .to_string_lossy()
        .contains(profile.id.as_str()));
    assert_eq!(first.plan.plan_hash.len(), 64);
    assert!(first.plan.warnings.iter().any(|warning| matches!(
        warning,
        brows_domain::LaunchWarning::HostFingerprintShared("canvas")
    )));
    first.plan.validate().unwrap();
}

#[test]
fn session_capacity_queues_and_stop_promotes_fifo() {
    let harness = Harness::new(1);
    let first_profile = harness.create_profile("First");
    let second_profile = harness.create_profile("Second");

    let first = harness
        .session_service
        .launch(SessionLaunchRequest {
            profile_id: first_profile.id.clone(),
        })
        .unwrap();
    let first_session = match first {
        SessionLaunchOutcome::Started { session, .. } => session,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(first_session.state, SessionStateDto::Running);

    let second = harness
        .session_service
        .launch(SessionLaunchRequest {
            profile_id: second_profile.id.clone(),
        })
        .unwrap();
    let second_session = match second {
        SessionLaunchOutcome::Queued {
            session, position, ..
        } => {
            assert_eq!(position, 1);
            session
        }
        other => panic!("unexpected outcome: {other:?}"),
    };

    let duplicate = harness
        .session_service
        .launch(SessionLaunchRequest {
            profile_id: second_profile.id,
        })
        .unwrap_err();
    assert_eq!(duplicate.code(), &ErrorCode::AlreadyExists);

    harness.clock.advance(1);
    harness
        .session_service
        .stop(SessionStopRequest {
            session_id: first_session.id,
        })
        .unwrap();
    let sessions = harness.session_service.list().unwrap();
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == second_session.id)
            .unwrap()
            .state,
        SessionStateDto::Running
    );
    let plans = harness.browsers.running_plans().unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].profile_id.as_str(),
        second_profile_id(&sessions, &second_session)
    );
}

#[test]
fn compiler_passes_the_whole_profile_to_fingerprint_runtime() {
    use brows_domain::{
        DeviceMemoryGb, FingerprintPersona, LaunchArgument, PlatformKind, ScreenConfig,
        SeedableNoisePersona, WebGLProfile,
    };
    let harness = Harness::new(1);
    let created = harness.create_profile("Fingerprint");
    let id: ProfileId = created.id.try_into().unwrap();
    let mut profile = harness.profiles.get(&id).unwrap();
    profile.locale.accept_languages = vec!["ja-JP".to_owned(), "ja".to_owned()];
    profile.window.width = 1200;
    profile.window.height = 700;
    profile.persona = FingerprintPersona::SeedableNoise(SeedableNoisePersona {
        seed: "compiler-flow".to_owned(),
        platform: PlatformKind::Windows,
        hardware_concurrency: 4,
        device_memory: DeviceMemoryGb::Four,
        touch_points: 0,
        screen: ScreenConfig {
            width: 1920,
            height: 1080,
            color_depth: 24,
            device_scale_factor_hundredths: 125,
        },
        webgl: WebGLProfile::auto(),
        spoof_canvas: false,
        spoof_audio: false,
    });
    let presets = Arc::new(InMemoryPresetCatalog::default());
    presets.insert(system_native_preset()).unwrap();
    let compiler = LaunchPlanCompiler::new(
        presets,
        harness.browsers.clone(),
        harness.proxies.clone(),
        Arc::new(
            FingerprintRuntimeRouter::new(
                std::env::temp_dir().join("brows-fingerprint-compiler-test"),
            )
            .unwrap(),
        ),
        PathBuf::from("C:\\BrowsData\\profiles"),
    )
    .unwrap();
    let plan = compiler
        .compile(&profile, &SessionId::new("fingerprint-flow").unwrap())
        .unwrap()
        .plan;
    assert!(plan.arguments.contains(&LaunchArgument::WindowSize {
        width: 1200,
        height: 700
    }));
    assert_eq!(
        plan.preferences.accept_languages,
        profile.locale.accept_languages
    );
    let dir = plan
        .arguments
        .iter()
        .find_map(|arg| match arg {
            LaunchArgument::LoadExtension(dir) => Some(dir),
            _ => None,
        })
        .unwrap();
    assert!(std::fs::read_to_string(dir.join("content.js"))
        .unwrap()
        .contains(r#""languages":["ja-JP","ja"]"#));
    for support in [
        brows_domain::FingerprintSupport::Unverified,
        brows_domain::FingerprintSupport::Unsupported,
    ] {
        let mut build = verified_build();
        build.capabilities.fingerprint_support = support;
        harness.browsers.insert_build(build).unwrap();
        assert_eq!(
            compiler
                .compile(&profile, &SessionId::new("unsupported-flow").unwrap())
                .unwrap_err()
                .code(),
            &ErrorCode::CapabilityUnsupported
        );
        assert!(harness.browsers.running_plans().unwrap().is_empty());
    }
    harness.browsers.insert_build(verified_build()).unwrap();
    profile.locale.timezone = Some("Asia/Tokyo".to_owned());
    assert_eq!(
        compiler
            .compile(&profile, &SessionId::new("timezone-flow").unwrap())
            .unwrap_err()
            .code(),
        &ErrorCode::CapabilityUnsupported
    );
}

fn second_profile_id<'a>(
    sessions: &'a [brows_contracts::SessionDto],
    second: &brows_contracts::SessionDto,
) -> &'a str {
    sessions
        .iter()
        .find(|session| session.id == second.id)
        .unwrap()
        .profile_id
        .as_str()
}

#[test]
fn failed_promoted_launch_releases_slot_and_starts_next_ticket() {
    let harness = Harness::new(1);
    let p1 = harness.create_profile("One");
    let p2 = harness.create_profile("Two");
    let p3 = harness.create_profile("Three");
    let running = match harness
        .session_service
        .launch(SessionLaunchRequest { profile_id: p1.id })
        .unwrap()
    {
        SessionLaunchOutcome::Started { session, .. } => session,
        other => panic!("unexpected outcome: {other:?}"),
    };
    let queued_two = match harness
        .session_service
        .launch(SessionLaunchRequest { profile_id: p2.id })
        .unwrap()
    {
        SessionLaunchOutcome::Queued { session, .. } => session,
        other => panic!("unexpected outcome: {other:?}"),
    };
    let queued_three = match harness
        .session_service
        .launch(SessionLaunchRequest { profile_id: p3.id })
        .unwrap()
    {
        SessionLaunchOutcome::Queued { session, .. } => session,
        other => panic!("unexpected outcome: {other:?}"),
    };

    harness.browsers.fail_next_launch();
    harness
        .session_service
        .stop(SessionStopRequest {
            session_id: running.id,
        })
        .unwrap();
    let sessions = harness.session_service.list().unwrap();
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == queued_two.id)
            .unwrap()
            .state,
        SessionStateDto::Failed
    );
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == queued_three.id)
            .unwrap()
            .state,
        SessionStateDto::Running
    );
}

#[test]
fn proxy_lease_is_bound_to_session_and_released_on_stop() {
    let harness = Harness::new(1);
    let proxy_id = ProxyId::new("proxy-1").unwrap();
    harness
        .proxies
        .insert_proxy(ProxySpec {
            id: proxy_id.clone(),
            name: "Proxy".to_owned(),
            scheme: ProxyScheme::Socks5,
            host: "proxy.example.test".to_owned(),
            port: 1080,
            username: Some("user".to_owned()),
            secret_ref: Some(SecretRef::new("proxy-secret").unwrap()),
            dns_mode: DnsMode::Proxy,
            bypass_hosts: Vec::new(),
            connect_timeout_ms: 5_000,
        })
        .unwrap();
    let profile = harness
        .profile_service
        .create(ProfileCreateRequest {
            profile: draft("Proxy profile", Some(&proxy_id)),
        })
        .unwrap();
    let session = match harness
        .session_service
        .launch(SessionLaunchRequest {
            profile_id: profile.id,
        })
        .unwrap()
    {
        SessionLaunchOutcome::Started { session, .. } => session,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(harness.proxies.active_leases().unwrap().len(), 1);
    let plan = harness.browsers.running_plans().unwrap().pop().unwrap();
    assert!(matches!(
        plan.proxy,
        brows_domain::ProxyLaunchBinding::Loopback { .. }
    ));

    harness
        .session_service
        .stop(SessionStopRequest {
            session_id: session.id,
        })
        .unwrap();
    assert!(harness.proxies.active_leases().unwrap().is_empty());
}

#[test]
fn secrets_are_redacted_and_backup_password_is_checked() {
    let secret = SecretValue::new(b"do-not-log".to_vec()).unwrap();
    assert!(!format!("{secret:?}").contains("do-not-log"));
    let store = InMemorySecretStore::default();
    let reference = SecretRef::new("s-1").unwrap();
    store.put(&reference, &secret).unwrap();
    assert_eq!(store.get(&reference).unwrap().expose(), b"do-not-log");

    let harness = Harness::new(1);
    let profile = harness.create_profile("Backup");
    let profile_id: ProfileId = profile.id.try_into().unwrap();
    let profile = harness.profiles.get(&profile_id).unwrap();
    let backups = InMemoryBackupService::default();
    backups
        .export(&BackupExportSpec {
            destination_name: "backup-1".to_owned(),
            profile,
            password: SecretValue::new(b"correct".to_vec()).unwrap(),
            include_proxy_credential: false,
        })
        .unwrap();
    let error = backups
        .import(&BackupImportSpec {
            source_name: "backup-1".to_owned(),
            password: SecretValue::new(b"wrong".to_vec()).unwrap(),
        })
        .unwrap_err();
    assert_eq!(error.kind, PortErrorKind::WrongPassword);
}

#[test]
fn all_ports_are_object_safe() {
    fn assert_profile(_: &dyn ProfileRepository) {}
    fn assert_preset(_: &dyn PresetCatalog) {}
    fn assert_secret(_: &dyn SecretStore) {}
    fn assert_browser(_: &dyn BrowserRuntime) {}
    fn assert_proxy(_: &dyn ProxyRuntime) {}
    fn assert_fingerprint(_: &dyn FingerprintRuntime) {}
    fn assert_diagnostic(_: &dyn DiagnosticRunner) {}
    fn assert_backup(_: &dyn BackupService) {}
    fn assert_audit(_: &dyn AuditSink) {}
    fn assert_clock(_: &dyn Clock) {}
    fn assert_ids(_: &dyn IdGenerator) {}

    let clock = Arc::new(ManualClock::default());
    assert_profile(&InMemoryProfileRepository::default());
    assert_preset(&InMemoryPresetCatalog::default());
    assert_secret(&InMemorySecretStore::default());
    assert_browser(&InMemoryBrowserRuntime::new(clock.clone()));
    assert_proxy(&InMemoryProxyRuntime::default());
    assert_fingerprint(&SystemNativeFingerprintRuntime);
    assert_diagnostic(&InMemoryDiagnosticRunner::new(clock.clone()));
    assert_backup(&InMemoryBackupService::default());
    assert_audit(&InMemoryAuditSink::default());
    assert_clock(clock.as_ref());
    assert_ids(&SequentialIdGenerator::default());
}

#[test]
fn supervisor_reaps_clean_and_crashed_sessions_and_promotes_fifo() {
    for success in [true, false] {
        let h = Harness::new(1);
        let first = h.create_profile("First");
        let second = h.create_profile("Second");
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: first.id.clone(),
            })
            .unwrap();
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: second.id.clone(),
            })
            .unwrap();
        let first_session = h
            .session_service
            .list()
            .unwrap()
            .into_iter()
            .find(|s| s.profile_id == first.id)
            .unwrap();
        h.browsers
            .simulate_exit(&first_session.id.clone().try_into().unwrap(), success)
            .unwrap();
        assert!(h.session_service.reconcile_exits().unwrap());
        let sessions = h.session_service.list().unwrap();
        assert_eq!(
            sessions
                .iter()
                .find(|s| s.profile_id == first.id)
                .unwrap()
                .state,
            if success {
                SessionStateDto::Stopped
            } else {
                SessionStateDto::Failed
            }
        );
        assert_eq!(
            sessions
                .iter()
                .find(|s| s.profile_id == second.id)
                .unwrap()
                .state,
            SessionStateDto::Running
        );
        assert_eq!(h.browsers.running_plans().unwrap().len(), 1);
        assert!(!h.session_service.reconcile_exits().unwrap());
        // The exited profile's reservation has been released.
        h.profile_service
            .archive(ProfileRevisionRequest {
                profile_id: first.id,
                expected_revision: first.revision,
            })
            .unwrap();
    }
}

#[test]
fn shutdown_cancels_queue_without_launching_it_and_rejects_new_launches() {
    let h = Harness::new(1);
    for n in 0..4 {
        let p = h.create_profile(&format!("Profile {n}"));
        h.session_service
            .launch(SessionLaunchRequest { profile_id: p.id })
            .unwrap();
    }
    h.session_service.shutdown().unwrap();
    assert!(h
        .session_service
        .list()
        .unwrap()
        .iter()
        .all(|s| s.state == SessionStateDto::Stopped));
    assert!(h.browsers.running_plans().unwrap().is_empty());
    assert_eq!(
        h.audit
            .events()
            .unwrap()
            .iter()
            .filter(|e| e.action == "session.launch")
            .count(),
        1
    );
    let p = h.create_profile("After shutdown");
    assert!(h
        .session_service
        .launch(SessionLaunchRequest { profile_id: p.id })
        .is_err());
    h.session_service.shutdown().unwrap();
}

#[test]
fn running_queued_and_data_operations_exclude_profile_mutations() {
    let h = Harness::new(1);
    let p = h.create_profile("Reserved");
    let id = p.id.clone().try_into().unwrap();
    let reservation = h.session_service.reserve_stopped_profile(&id).unwrap();
    assert_eq!(
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: p.id.clone()
            })
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
    assert_eq!(
        h.profile_service
            .archive(ProfileRevisionRequest {
                profile_id: p.id.clone(),
                expected_revision: p.revision
            })
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
    drop(reservation);
    h.session_service
        .launch(SessionLaunchRequest { profile_id: p.id })
        .unwrap();
    let queued = h.create_profile("Queued");
    h.session_service
        .launch(SessionLaunchRequest {
            profile_id: queued.id.clone(),
        })
        .unwrap();
    assert_eq!(
        h.profile_service
            .archive(ProfileRevisionRequest {
                profile_id: queued.id.clone(),
                expected_revision: queued.revision
            })
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
    assert_eq!(
        h.profile_service
            .update(ProfileUpdateRequest {
                profile_id: queued.id,
                expected_revision: queued.revision,
                profile: draft("Changed", None)
            })
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
}

#[derive(Debug)]
struct TestArchives {
    profile: brows_domain::ProfileSpec,
    exported: std::sync::atomic::AtomicUsize,
    discarded: std::sync::atomic::AtomicUsize,
    fail_install: bool,
    cleanup_blocked: std::sync::atomic::AtomicBool,
    export_gate: Option<(
        std::sync::mpsc::Sender<()>,
        std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    )>,
}

impl BackupService for TestArchives {
    fn inspect(&self, _: &str) -> PortResult<BackupInspection> {
        unreachable!()
    }
    fn export(&self, _: &BackupExportSpec) -> PortResult<()> {
        self.exported
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some((started, resume)) = &self.export_gate {
            started.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        Ok(())
    }
    fn import(&self, _: &BackupImportSpec) -> PortResult<BackupImportPayload> {
        if let Some((started, resume)) = &self.export_gate {
            started.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        Ok(BackupImportPayload {
            profile: self.profile.clone(),
            restored_data_dir: PathBuf::from("test-staging/userdata"),
        })
    }
}

impl RestorableBackupService for TestArchives {
    fn import_for_task(&self, spec: &BackupImportSpec, _: u64) -> PortResult<BackupImportPayload> {
        self.import(spec)
    }
    fn cleanup_task(&self, _: u64) -> PortResult<()> {
        if self
            .cleanup_blocked
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated cleanup failure",
            ));
        }
        self.discard_restored_data(std::path::Path::new("test-staging/userdata"));
        Ok(())
    }
    fn cleanup_stale_staging(&self) -> PortResult<bool> {
        Ok(self
            .cleanup_blocked
            .load(std::sync::atomic::Ordering::SeqCst))
    }

    fn install_restored_data(&self, _: &ProfileId, _: &std::path::Path) -> PortResult<()> {
        if self.fail_install {
            Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated disk failure",
            ))
        } else {
            Ok(())
        }
    }
    fn discard_restored_data(&self, _: &std::path::Path) {
        self.discarded
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn backup_holds_profile_reservation_for_the_entire_export() {
    let h = Harness::new(1);
    let profile = h.create_profile("Backup");
    let (started, wait) = std::sync::mpsc::channel();
    let (resume, receiver) = std::sync::mpsc::channel();
    let archives = Arc::new(TestArchives {
        profile: profile.clone().try_into().unwrap(),
        exported: 0.into(),
        discarded: 0.into(),
        fail_install: false,
        cleanup_blocked: false.into(),
        export_gate: Some((started, std::sync::Mutex::new(receiver))),
    });
    let service = BackupApplicationService::new(
        h.profile_service.clone(),
        h.session_service.clone(),
        archives.clone(),
        Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone())),
        h.clock.clone(),
    );
    let id = profile.id.clone().try_into().unwrap();
    let worker = std::thread::spawn(move || {
        service.export(
            id,
            "backup.browsbak".into(),
            SecretValue::new(b"long-test-password".to_vec()).unwrap(),
            false,
        )
    });
    wait.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        h.session_service
            .launch(SessionLaunchRequest {
                profile_id: profile.id.clone()
            })
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
    resume.send(()).unwrap();
    worker.join().unwrap().unwrap();
    h.session_service
        .launch(SessionLaunchRequest {
            profile_id: profile.id.clone(),
        })
        .unwrap();
    let service = BackupApplicationService::new(
        h.profile_service.clone(),
        h.session_service.clone(),
        archives.clone(),
        Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone())),
        h.clock.clone(),
    );
    assert_eq!(
        service
            .export(
                profile.id.try_into().unwrap(),
                "second.browsbak".into(),
                SecretValue::new(b"long-test-password".to_vec()).unwrap(),
                false
            )
            .unwrap_err()
            .code(),
        &ErrorCode::ProfileLocked
    );
    assert_eq!(
        archives.exported.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn failed_restore_install_is_archived_and_staging_is_always_discarded() {
    let h = Harness::new(1);
    let profile = h.create_profile("Restore source");
    let archives = Arc::new(TestArchives {
        profile: profile.try_into().unwrap(),
        exported: 0.into(),
        discarded: 0.into(),
        fail_install: true,
        cleanup_blocked: false.into(),
        export_gate: None,
    });
    let service = BackupApplicationService::new(
        h.profile_service.clone(),
        h.session_service.clone(),
        archives.clone(),
        Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone())),
        h.clock.clone(),
    );
    assert!(service
        .import(BackupImportSpec {
            source_name: "test.browsbak".into(),
            password: SecretValue::new(b"long-test-password".to_vec()).unwrap()
        })
        .is_err());
    assert_eq!(
        archives.discarded.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let profiles = h.profiles.profiles().unwrap();
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles.iter().filter(|p| p.archived).count(), 1);
}

#[test]
fn restore_task_outcomes_survive_cleanup_failures_and_retries() {
    let h = Harness::new(1);
    let profile = h.create_profile("Restore source");
    let archives = Arc::new(TestArchives {
        profile: profile.try_into().unwrap(),
        exported: 0.into(),
        discarded: 0.into(),
        fail_install: false,
        cleanup_blocked: true.into(),
        export_gate: None,
    });
    let tasks = Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone()));
    let service = BackupApplicationService::new(
        h.profile_service.clone(),
        h.session_service.clone(),
        archives.clone(),
        tasks.clone(),
        h.clock.clone(),
    );
    let result = service
        .import(BackupImportSpec {
            source_name: "test.browsbak".into(),
            password: SecretValue::new(b"long-test-password".to_vec()).unwrap(),
        })
        .unwrap();
    assert!(!result.archived);
    let record = tasks.list(10).unwrap().remove(0);
    assert_eq!(record.status, RestoreTaskStatus::Completed);
    assert_eq!(
        record.profile_id.as_ref().unwrap().as_str(),
        result.id.as_str()
    );
    assert!(record.cleanup_pending);
    assert!(service.cleanup_pending());
    assert!(service.recover_incomplete().unwrap());
    archives
        .cleanup_blocked
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(!service.recover_incomplete().unwrap());
    assert!(!service.cleanup_pending());
    assert!(tasks.pending().unwrap().is_empty());
    assert_eq!(
        tasks.list(10).unwrap()[0].status,
        RestoreTaskStatus::Completed
    );
    assert_eq!(
        h.profile_service
            .get(ProfileIdRequest {
                profile_id: result.id
            })
            .unwrap()
            .revision,
        result.revision
    );
}

#[test]
fn recovery_interrupts_unfinished_tasks_and_never_activates_their_profiles() {
    let h = Harness::new(1);
    let profile = h.create_profile("Source");
    let archives = Arc::new(TestArchives {
        profile: profile.clone().try_into().unwrap(),
        exported: 0.into(),
        discarded: 0.into(),
        fail_install: false,
        cleanup_blocked: false.into(),
        export_gate: None,
    });
    let tasks = Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone()));
    let record = tasks.begin(100).unwrap();
    tasks
        .bind_profile(record.id, &profile.id.clone().try_into().unwrap())
        .unwrap();
    h.profile_service
        .archive(ProfileRevisionRequest {
            profile_id: profile.id.clone(),
            expected_revision: profile.revision,
        })
        .unwrap();
    let service = BackupApplicationService::new(
        h.profile_service.clone(),
        h.session_service.clone(),
        archives,
        tasks.clone(),
        h.clock.clone(),
    );
    assert!(!service.recover_incomplete().unwrap());
    assert_eq!(
        tasks.list(10).unwrap()[0].status,
        RestoreTaskStatus::Interrupted
    );
    assert!(
        h.profile_service
            .get(ProfileIdRequest {
                profile_id: profile.id
            })
            .unwrap()
            .archived
    );
    assert!(!service.recover_incomplete().unwrap());
    assert_eq!(
        tasks.list(10).unwrap()[0].failure,
        Some(RestoreFailure::Interrupted)
    );
}

#[test]
fn cancelled_restore_is_journaled_without_creating_a_profile_and_cleanup_can_retry() {
    for blocked in [false, true] {
        let h = Harness::new(1);
        let source = h.create_profile("Cancellation source");
        let (started, wait) = std::sync::mpsc::channel();
        let (resume, receiver) = std::sync::mpsc::channel();
        let archives = Arc::new(TestArchives {
            profile: source.clone().try_into().unwrap(),
            exported: 0.into(),
            discarded: 0.into(),
            fail_install: false,
            cleanup_blocked: blocked.into(),
            export_gate: Some((started, std::sync::Mutex::new(receiver))),
        });
        let tasks = Arc::new(InMemoryRestoreTaskRepository::new(h.profiles.clone()));
        let service = BackupApplicationService::new(
            h.profile_service.clone(),
            h.session_service.clone(),
            archives.clone(),
            tasks.clone(),
            h.clock.clone(),
        );
        let control = BackupTaskControl::new();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                service.import_controlled(
                    BackupImportSpec {
                        source_name: "test.browsbak".into(),
                        password: SecretValue::new(b"long-test-password".to_vec()).unwrap(),
                    },
                    &control,
                )
            });
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(
                tasks.list(10).unwrap()[0].status,
                RestoreTaskStatus::Running
            );
            assert!(control.request_cancel());
            resume.send(()).unwrap();
            assert_eq!(
                worker.join().unwrap().unwrap_err().code(),
                &ErrorCode::OperationCancelled
            );
        });
        let task = tasks.list(10).unwrap().remove(0);
        assert_eq!(task.status, RestoreTaskStatus::Cancelled);
        assert_eq!(task.failure, Some(RestoreFailure::Cancelled));
        assert!(task.profile_id.is_none());
        assert_eq!(task.cleanup_pending, blocked);
        assert_eq!(h.profiles.profiles().unwrap().len(), 1);
        assert_eq!(service.cleanup_pending(), blocked);
        archives
            .cleanup_blocked
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!service.recover_incomplete().unwrap());
        assert_eq!(
            tasks.list(10).unwrap()[0].status,
            RestoreTaskStatus::Cancelled
        );
        assert!(tasks.pending().unwrap().is_empty());

        let control = BackupTaskControl::new();
        assert!(control.request_cancel());
        assert_eq!(
            service
                .import_controlled(
                    BackupImportSpec {
                        source_name: "test.browsbak".into(),
                        password: SecretValue::new(b"long-test-password".to_vec()).unwrap(),
                    },
                    &control
                )
                .unwrap_err()
                .code(),
            &ErrorCode::OperationCancelled
        );
        assert_eq!(tasks.list(10).unwrap().len(), 1);
    }
}

#[test]
fn cloning_fingerprint_profiles_creates_independent_stable_seeds_without_changing_the_source() {
    use brows_domain::{
        DeviceMemoryGb, FingerprintPersona, PlatformKind, ScreenConfig, SeedableNoisePersona,
        WebGLProfile,
    };
    let h = Harness::new(2);
    let source = h.create_profile("Identity source");
    let source_id: ProfileId = source.id.clone().try_into().unwrap();
    let mut spec = h.profiles.get(&source_id).unwrap();
    spec.persona = FingerprintPersona::SeedableNoise(SeedableNoisePersona {
        seed: "original-stable-identity".into(),
        platform: PlatformKind::Windows,
        hardware_concurrency: 4,
        device_memory: DeviceMemoryGb::Four,
        touch_points: 0,
        screen: ScreenConfig {
            width: 1920,
            height: 1080,
            color_depth: 24,
            device_scale_factor_hundredths: 100,
        },
        webgl: WebGLProfile::auto(),
        spoof_canvas: true,
        spoof_audio: true,
    });
    spec.bump_revision();
    h.profiles
        .update(spec.clone(), brows_domain::Revision::FIRST)
        .unwrap();
    let clone = h
        .profile_service
        .clone_profile(ProfileIdRequest {
            profile_id: source.id.clone(),
        })
        .unwrap();
    let sibling = h
        .profile_service
        .clone_profile(ProfileIdRequest {
            profile_id: clone.id.clone(),
        })
        .unwrap();
    let restored_clone = h
        .profiles
        .get(&clone.id.clone().try_into().unwrap())
        .unwrap();
    let sibling_spec = h.profiles.get(&sibling.id.try_into().unwrap()).unwrap();
    let FingerprintPersona::SeedableNoise(mut clone_persona) = restored_clone.persona.clone()
    else {
        panic!("persona changed")
    };
    let FingerprintPersona::SeedableNoise(source_persona) = &spec.persona else {
        unreachable!()
    };
    let FingerprintPersona::SeedableNoise(sibling_persona) = sibling_spec.persona else {
        unreachable!()
    };
    assert_ne!(clone_persona.seed, source_persona.seed);
    assert_ne!(clone_persona.seed, sibling_persona.seed);
    assert_eq!(h.profiles.get(&source_id).unwrap(), spec);
    let stable_seed = clone_persona.seed.clone();
    clone_persona.seed = source_persona.seed.clone();
    assert_eq!(&clone_persona, source_persona, "only the seed changes");
    assert!(
        matches!(h.profile_service.get(ProfileIdRequest { profile_id: clone.id }).unwrap().persona,
        PersonaDto::SeedableNoise { seed, .. } if seed == stable_seed)
    );
}
