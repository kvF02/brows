#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

mod error;
mod ids;
mod model;
mod scheduler;

pub use error::DomainError;
pub use ids::{BrowserBuildId, PresetId, ProfileId, ProxyId, Revision, SecretRef, SessionId};
pub use model::{
    BrowserBuild, BrowserBuildSelector, BrowserCapabilities, BrowserChannel, BrowserPreferences,
    DiagnosticCheck, DiagnosticOverall, DiagnosticReport, DiagnosticScope, DiagnosticStatus,
    DnsMode, EnvironmentPreset, EnvironmentPresetKind, EnvironmentPresetRef, FingerprintPersona,
    LaunchArgument, LaunchWarning, LocaleConfig, PersonaKind, ProfileSpec, ProxyLaunchBinding,
    ProxyScheme, ProxySpec, ResolvedLaunchPlan, RuntimeTrust, Session, SessionFailure,
    SessionState, SystemNativePersona, WindowConfig, DEFAULT_MAX_CONCURRENT_SESSIONS,
    DEFAULT_PROXY_TIMEOUT_MS, MAX_CONCURRENT_SESSIONS, MAX_PROFILE_NAME_CHARS, MAX_TAG_CHARS,
};
pub use scheduler::{EnqueueOutcome, LaunchTicket, QueueSnapshot, SessionScheduler};

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
}
