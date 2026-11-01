#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod error;
mod ids;
mod model;
mod scheduler;
mod settings;

pub use error::DomainError;
pub use ids::{BrowserBuildId, PresetId, ProfileId, ProxyId, Revision, SecretRef, SessionId};
pub use model::{
    BrowserBuild, BrowserBuildSelector, BrowserCapabilities, BrowserChannel, BrowserPreferences,
    DeviceMemoryGb, DiagnosticCheck, DiagnosticOverall, DiagnosticReport, DiagnosticScope,
    DiagnosticStatus, DnsMode, EnvironmentPreset, EnvironmentPresetKind, EnvironmentPresetRef,
    FingerprintPersona, FingerprintSupport, LaunchArgument, LaunchWarning, LocaleConfig,
    PersonaKind, PlatformKind, ProfileSpec, ProxyLaunchBinding, ProxyScheme, ProxySpec,
    ResolvedLaunchPlan, RuntimeTrust, ScreenConfig, SeedableNoisePersona, Session, SessionFailure,
    SessionState, SystemNativePersona, WebGLProfile, WindowConfig, DEFAULT_MAX_CONCURRENT_SESSIONS,
    DEFAULT_PROXY_TIMEOUT_MS, MAX_CONCURRENT_SESSIONS, MAX_PROFILE_NAME_CHARS, MAX_QUEUED_SESSIONS,
    MAX_TAG_CHARS,
};
pub use scheduler::{EnqueueOutcome, LaunchTicket, QueueSnapshot, SessionScheduler};
pub use settings::ManagerSettings;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn profile() -> ProfileSpec {
        ProfileSpec {
            id: ProfileId::new("profile-1").unwrap(),
            name: "Research".to_owned(),
            tags: vec!["qa".to_owned()],
            environment_preset: EnvironmentPresetRef::new(
                PresetId::new("system-native").unwrap(),
                1,
            )
            .unwrap(),
            proxy_id: None,
            browser: BrowserBuildSelector::Channel(BrowserChannel::Stable),
            persona: FingerprintPersona::SystemNative(SystemNativePersona::default()),
            locale: LocaleConfig::default(),
            window: WindowConfig::default(),
            startup_urls: vec!["https://example.test".to_owned()],
            revision: Revision::FIRST,
            archived: false,
        }
    }

    #[test]
    fn profile_validation_and_revision_bump() {
        let mut value = profile();
        assert!(value.validate().is_ok());
        assert_eq!(value.revision.get(), 1);
        value.bump_revision();
        assert_eq!(value.revision.get(), 2);
    }

    #[test]
    fn launch_plan_rejects_invalid_hash_and_untrusted_argument() {
        let plan = ResolvedLaunchPlan {
            session_id: SessionId::new("session-1").unwrap(),
            profile_id: ProfileId::new("profile-1").unwrap(),
            profile_revision: Revision::FIRST,
            executable_path: PathBuf::from("C:\\Program Files\\Brows\\chrome.exe"),
            user_data_dir: PathBuf::from("C:\\Users\\tester\\Brows\\profiles\\profile-1"),
            arguments: vec![LaunchArgument::WindowSize {
                width: 1280,
                height: 800,
            }],
            preferences: BrowserPreferences {
                accept_languages: vec!["en-US".to_owned()],
                startup_urls: vec![],
                permissions_prompt: true,
                webgl_enabled: None,
                hardware_acceleration: None,
                notifications_setting: None,
            },
            proxy: ProxyLaunchBinding::Direct,
            capabilities: BrowserCapabilities::default(),
            warnings: vec![],
            plan_hash: "not-a-hash".to_owned(),
        };
        assert_eq!(plan.validate(), Err(DomainError::InvalidHash));
    }

    #[test]
    fn launch_plan_requires_typed_matching_user_data_directory() {
        let user_data_dir = PathBuf::from("C:\\Users\\tester\\Brows\\profiles\\profile-1");
        let plan = ResolvedLaunchPlan {
            session_id: SessionId::new("session-1").unwrap(),
            profile_id: ProfileId::new("profile-1").unwrap(),
            profile_revision: Revision::FIRST,
            executable_path: PathBuf::from("C:\\Program Files\\Brows\\chrome.exe"),
            user_data_dir: user_data_dir.clone(),
            arguments: vec![LaunchArgument::UserDataDirectory(user_data_dir)],
            preferences: BrowserPreferences {
                accept_languages: vec!["en-US".to_owned()],
                startup_urls: vec![],
                permissions_prompt: true,
                webgl_enabled: None,
                hardware_acceleration: None,
                notifications_setting: None,
            },
            proxy: ProxyLaunchBinding::Direct,
            capabilities: BrowserCapabilities::default(),
            warnings: vec![],
            plan_hash: "a".repeat(64),
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn session_state_machine_rejects_running_to_queued() {
        let mut session = Session::new(
            SessionId::new("s").unwrap(),
            ProfileId::new("p").unwrap(),
            10,
        );
        session.transition(SessionState::Starting, 11).unwrap();
        session.transition(SessionState::Running, 12).unwrap();
        assert!(matches!(
            session.transition(SessionState::Queued, 13),
            Err(DomainError::InvalidStateTransition { .. })
        ));
        session.fail("CRASHED", "browser exited", 14).unwrap();
        assert_eq!(session.state, SessionState::Failed);
        assert_eq!(session.failure.as_ref().unwrap().code, "CRASHED");
    }

    #[test]
    fn authenticated_proxy_requires_secret_reference() {
        let proxy = ProxySpec {
            id: ProxyId::new("proxy-1").unwrap(),
            name: "Lab proxy".to_owned(),
            scheme: ProxyScheme::Socks5,
            host: "proxy.example.test".to_owned(),
            port: 1080,
            username: Some("researcher".to_owned()),
            secret_ref: None,
            dns_mode: DnsMode::Proxy,
            bypass_hosts: vec![],
            connect_timeout_ms: DEFAULT_PROXY_TIMEOUT_MS,
        };
        assert_eq!(proxy.validate(), Err(DomainError::MissingProxySecret));
    }

    #[test]
    fn system_native_persona_rejects_explicit_timezone_override() {
        let mut value = profile();
        value.locale.timezone = Some("Asia/Shanghai".to_owned());
        assert!(
            matches!(value.validate(), Err(DomainError::InconsistentConfiguration(reason)) if reason.contains("timezone"))
        );
    }

    #[test]
    fn system_native_persona_cannot_claim_hidden_spoofing() {
        let mut value = profile();
        value.persona = FingerprintPersona::SystemNative(SystemNativePersona {
            preserve_system_gpu: false,
            ..SystemNativePersona::default()
        });
        assert!(
            matches!(value.validate(), Err(DomainError::InconsistentConfiguration(reason)) if reason.contains("host-level"))
        );
    }

    // ── v0.2 SeedableNoise domain tests ──────────────────────────────────────

    fn seedable_profile() -> ProfileSpec {
        let mut p = profile();
        p.persona = FingerprintPersona::SeedableNoise(SeedableNoisePersona {
            seed: "test-seed-001".to_owned(),
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
        p
    }

    #[test]
    fn seedable_noise_persona_requires_non_empty_seed() {
        let mut p = seedable_profile();
        if let FingerprintPersona::SeedableNoise(ref mut persona) = p.persona {
            persona.seed = "   ".to_owned();
        }
        assert!(
            matches!(p.validate(), Err(DomainError::InconsistentConfiguration(msg)) if msg.contains("seed"))
        );
    }

    #[test]
    fn seedable_noise_persona_validates_webgl_platform_consistency() {
        let mut p = seedable_profile();
        if let FingerprintPersona::SeedableNoise(ref mut persona) = p.persona {
            // Apple Inc. vendor is only valid for MacOs, not Windows
            persona.webgl = WebGLProfile {
                vendor: "Apple Inc.".to_owned(),
                unmasked_renderer: "Apple M2".to_owned(),
            };
        }
        assert!(
            matches!(p.validate(), Err(DomainError::InconsistentConfiguration(msg)) if msg.contains("vendor"))
        );
    }

    #[test]
    fn seedable_noise_persona_rejects_oversized_hardware_concurrency() {
        let mut p = seedable_profile();
        if let FingerprintPersona::SeedableNoise(ref mut persona) = p.persona {
            persona.hardware_concurrency = 200;
        }
        assert!(
            matches!(p.validate(), Err(DomainError::InconsistentConfiguration(msg)) if msg.contains("hardware_concurrency"))
        );
    }

    #[test]
    fn seedable_noise_validates_locale_consistency() {
        let mut p = seedable_profile();
        // Asia/Tokyo with German only is inconsistent (de not in Asia allowed list).
        p.locale.timezone = Some("Asia/Tokyo".to_owned());
        p.locale.accept_languages = vec!["de-DE".to_owned(), "de".to_owned()];
        assert!(
            matches!(p.validate(), Err(DomainError::InconsistentConfiguration(msg)) if msg.contains("timezone"))
        );
        // Adding ja makes it consistent.
        p.locale.accept_languages.push("ja".to_owned());
        assert!(p.validate().is_ok());
        // Europe/Berlin + de-DE is consistent.
        let mut q = seedable_profile();
        q.locale.timezone = Some("Europe/Berlin".to_owned());
        q.locale.accept_languages = vec!["de-DE".to_owned(), "de".to_owned()];
        assert!(q.validate().is_ok());
        // Europe/Berlin + zh-CN only is inconsistent.
        q.locale.accept_languages = vec!["zh-CN".to_owned()];
        assert!(
            matches!(q.validate(), Err(DomainError::InconsistentConfiguration(msg)) if msg.contains("timezone"))
        );
    }

    #[test]
    fn seedable_persona_reports_unmodified_host_surfaces() {
        let p = seedable_profile();
        let shared = p.persona.host_shared_fields();
        for field in [
            "timezone",
            "fonts",
            "gpu",
            "tls_http2",
            "client_hints",
            "workers",
            "webgl_pixels",
            "audio_device",
            "canvas_uncovered_apis",
        ] {
            assert!(
                shared.contains(&field),
                "missing host-shared field: {field}"
            );
        }
    }
}
