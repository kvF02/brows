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
    profile_service: ProfileApplicationService,
    session_service: SessionApplicationService,
    compiler: Arc<LaunchPlanCompiler>,
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
        let profile_service = ProfileApplicationService::new(
            profiles.clone(),
            presets.clone(),
            browsers.clone(),
            proxies.clone(),
            audit.clone(),
            clock.clone(),
            ids.clone(),
        );
        let session_service = SessionApplicationService::new(
            profiles.clone(),
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
            profile_service,
            session_service,
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
    let plan = compiler.compile(&profile, &SessionId::new("timezone-flow").unwrap()).unwrap().plan;
    assert!(plan.arguments.iter().any(|arg| matches!(arg,
        LaunchArgument::SessionFingerprint(policy) if policy.timezone.as_deref() == Some("Asia/Tokyo"))));
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
fn process_reconciliation_releases_failed_session_and_promotes_queue() {
    let h = Harness::new(1);
    let first = h.create_profile("first"); let second = h.create_profile("second");
    let running = h.session_service.launch(SessionLaunchRequest { profile_id: first.id }).unwrap();
    h.session_service.launch(SessionLaunchRequest { profile_id: second.id.clone() }).unwrap();
    let SessionLaunchOutcome::Started { session, .. } = running else { panic!("expected running") };
    let id: SessionId = session.id.clone().try_into().unwrap();
    h.browsers.stop(&id).unwrap();
    h.session_service.reconcile_exits().unwrap();
    let list = h.session_service.list().unwrap();
    assert_eq!(list.iter().find(|s| s.id == session.id).unwrap().state, SessionStateDto::Failed);
    assert_eq!(list.iter().find(|s| s.profile_id == second.id).unwrap().state, SessionStateDto::Running);
    h.session_service.reconcile_exits().unwrap();
}
