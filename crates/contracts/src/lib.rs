#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Versioned, serialisable messages crossing the Tauri boundary.
//!
//! These types intentionally contain no file paths supplied by the caller,
//! arbitrary command lines, or generic JSON configuration maps.  The desktop
//! adapter maps them to domain commands after validating the request.

use brows_domain as domain;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const CONTRACT_VERSION: u16 = 1;

macro_rules! id_dto {
    ($name:ident, $domain:ty, $kind:literal) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                <$domain>::new(value.clone()).map_err(ContractError::from_domain)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple($kind).field(&self.0).finish()
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }

        impl From<$domain> for $name {
            fn from(value: $domain) -> Self {
                Self(value.into_string())
            }
        }

        impl From<&$domain> for $name {
            fn from(value: &$domain) -> Self {
                Self(value.as_str().to_owned())
            }
        }

        impl TryFrom<$name> for $domain {
            type Error = ContractError;

            fn try_from(value: $name) -> Result<Self, Self::Error> {
                <$domain>::new(value.0).map_err(ContractError::from_domain)
            }
        }
    };
}

id_dto!(ProfileIdDto, domain::ProfileId, "ProfileId");
id_dto!(ProxyIdDto, domain::ProxyId, "ProxyId");
id_dto!(PresetIdDto, domain::PresetId, "PresetId");
id_dto!(BrowserBuildIdDto, domain::BrowserBuildId, "BrowserBuildId");
id_dto!(SessionIdDto, domain::SessionId, "SessionId");

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SecretInput(String);

impl SecretInput {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ContractError::invalid_field("secret", "cannot be empty"));
        }
        Ok(Self(value))
    }

    /// Deliberately explicit so call sites cannot accidentally log a secret.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretInput(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for SecretInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    OperationCancelled,
    ValidationFailed,
    RevisionConflict,
    InvalidProfileState,
    ProfileLocked,
    CapabilityUnsupported,
    ProxyConnectFailed,
    ProxyAuthFailed,
    ProxyWouldFallback,
    RuntimeUntrusted,
    RuntimeInUse,
    BackupWrongPassword,
    BackupTampered,
    BackupIncompatible,
    NotFound,
    AlreadyExists,
    QueueFull,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldViolationDto {
    pub field: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDto {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<FieldViolationDto>,
}

impl ErrorDto {
    pub fn invalid_field(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::ValidationFailed,
            message: "one or more fields are invalid".to_owned(),
            violations: vec![FieldViolationDto {
                field: field.into(),
                reason: reason.into(),
            }],
        }
    }

    pub fn from_domain(error: domain::DomainError) -> Self {
        use domain::DomainError;
        let (code, message) = match &error {
            DomainError::InvalidStateTransition { .. } => {
                (ErrorCode::InvalidProfileState, error.to_string())
            }
            DomainError::ProfileAlreadyScheduled | DomainError::DuplicateSession => {
                (ErrorCode::AlreadyExists, error.to_string())
            }
            DomainError::QueueFull => (ErrorCode::QueueFull, error.to_string()),
            DomainError::SessionNotFound => (ErrorCode::NotFound, error.to_string()),
            DomainError::InvalidHash
            | DomainError::InvalidPath
            | DomainError::InvalidLaunchArgument(_) => {
                (ErrorCode::ValidationFailed, error.to_string())
            }
            _ => (ErrorCode::ValidationFailed, error.to_string()),
        };
        Self {
            code,
            message,
            violations: Vec::new(),
        }
    }
}

/// Profile-facing representation of the v0.1 fingerprint model.  The UI can
/// show that host-level values are shared instead of implying full spoofing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PersonaDto {
    SystemNative {
        preserve_system_timezone: bool,
        preserve_system_fonts: bool,
        preserve_system_gpu: bool,
        preserve_system_audio: bool,
    },
    SeedableNoise {
        seed: String,
        platform: PlatformDto,
        hardware_concurrency: u8,
        device_memory: DeviceMemoryDto,
        touch_points: u8,
        screen: ScreenConfigDto,
        webgl_vendor: String,
        webgl_renderer: String,
        spoof_canvas: bool,
        spoof_audio: bool,
    },
}

/// Device memory discrete values mirroring `domain::DeviceMemoryGb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceMemoryDto {
    Quarter,
    Half,
    One,
    Two,
    Four,
    Eight,
}

impl From<DeviceMemoryDto> for domain::DeviceMemoryGb {
    fn from(v: DeviceMemoryDto) -> Self {
        match v {
            DeviceMemoryDto::Quarter => domain::DeviceMemoryGb::Quarter,
            DeviceMemoryDto::Half => domain::DeviceMemoryGb::Half,
            DeviceMemoryDto::One => domain::DeviceMemoryGb::One,
            DeviceMemoryDto::Two => domain::DeviceMemoryGb::Two,
            DeviceMemoryDto::Four => domain::DeviceMemoryGb::Four,
            DeviceMemoryDto::Eight => domain::DeviceMemoryGb::Eight,
        }
    }
}

impl From<domain::DeviceMemoryGb> for DeviceMemoryDto {
    fn from(v: domain::DeviceMemoryGb) -> Self {
        match v {
            domain::DeviceMemoryGb::Quarter => DeviceMemoryDto::Quarter,
            domain::DeviceMemoryGb::Half => DeviceMemoryDto::Half,
            domain::DeviceMemoryGb::One => DeviceMemoryDto::One,
            domain::DeviceMemoryGb::Two => DeviceMemoryDto::Two,
            domain::DeviceMemoryGb::Four => DeviceMemoryDto::Four,
            domain::DeviceMemoryGb::Eight => DeviceMemoryDto::Eight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformDto {
    Windows,
    MacOs,
    Linux,
}

impl From<PlatformDto> for domain::PlatformKind {
    fn from(v: PlatformDto) -> Self {
        match v {
            PlatformDto::Windows => domain::PlatformKind::Windows,
            PlatformDto::MacOs => domain::PlatformKind::MacOs,
            PlatformDto::Linux => domain::PlatformKind::Linux,
        }
    }
}

impl From<domain::PlatformKind> for PlatformDto {
    fn from(v: domain::PlatformKind) -> Self {
        match v {
            domain::PlatformKind::Windows => PlatformDto::Windows,
            domain::PlatformKind::MacOs => PlatformDto::MacOs,
            domain::PlatformKind::Linux => PlatformDto::Linux,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenConfigDto {
    pub width: u32,
    pub height: u32,
    pub color_depth: u8,
    /// Scale factor in hundredths: 100 = 1.0×, 200 = 2.0×.
    pub device_scale_factor_hundredths: u16,
}

impl From<ScreenConfigDto> for domain::ScreenConfig {
    fn from(v: ScreenConfigDto) -> Self {
        domain::ScreenConfig {
            width: v.width,
            height: v.height,
            color_depth: v.color_depth,
            device_scale_factor_hundredths: v.device_scale_factor_hundredths,
        }
    }
}

impl From<domain::ScreenConfig> for ScreenConfigDto {
    fn from(v: domain::ScreenConfig) -> Self {
        ScreenConfigDto {
            width: v.width,
            height: v.height,
            color_depth: v.color_depth,
            device_scale_factor_hundredths: v.device_scale_factor_hundredths,
        }
    }
}

/// Configuration preview for a SeedableNoise profile and its selected runtime.
/// These are intended values, not observations: this command never launches a browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintPreviewDto {
    pub runtime_id: String,
    pub runtime_version: String,
    pub effective_user_agent: String,
    pub effective_platform: String,
    pub effective_languages: Vec<String>,
    pub effective_window_width: u32,
    pub effective_window_height: u32,
    pub effective_screen_width: u32,
    pub effective_screen_height: u32,
    pub device_scale_factor_hundredths: u16,
    pub effective_webgl_vendor: String,
    pub effective_webgl_renderer: String,
    pub hardware_concurrency: u8,
    /// Device memory as a display string ("0.25", "0.5", "1", "2", "4", "8") in GB.
    pub device_memory_gb: String,
    /// Fields that are configured at the JS level only (not kernel/GPU level).
    pub declared_only_fields: Vec<String>,
    /// Fields still reported at host-system values.
    pub host_shared_fields: Vec<String>,
    /// Runtime limitations and the distinction between configuration and observation.
    pub runtime_warnings: Vec<String>,
}

impl Default for PersonaDto {
    fn default() -> Self {
        let persona = domain::SystemNativePersona::default();
        Self::SystemNative {
            preserve_system_timezone: persona.preserve_system_timezone,
            preserve_system_fonts: persona.preserve_system_fonts,
            preserve_system_gpu: persona.preserve_system_gpu,
            preserve_system_audio: persona.preserve_system_audio,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserChannelDto {
    Stable,
    Beta,
    Dev,
    Canary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "mode",
    content = "value"
)]
pub enum BrowserSelectorDto {
    Pinned { build_id: BrowserBuildIdDto },
    Channel { channel: BrowserChannelDto },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleDto {
    pub accept_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDto {
    pub width: u32,
    pub height: u32,
    pub start_maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDto {
    pub id: ProfileIdDto,
    pub name: String,
    pub tags: Vec<String>,
    pub preset_id: PresetIdDto,
    pub preset_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<ProxyIdDto>,
    pub browser: BrowserSelectorDto,
    pub persona: PersonaDto,
    pub locale: LocaleDto,
    pub window: WindowDto,
    pub startup_urls: Vec<String>,
    pub revision: u64,
    pub archived: bool,
}

/// Editable profile fields. Identity, revision and archive state are managed
/// by the application layer rather than trusted from the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDraftDto {
    pub name: String,
    pub tags: Vec<String>,
    pub preset_id: PresetIdDto,
    pub preset_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<ProxyIdDto>,
    pub browser: BrowserSelectorDto,
    pub persona: PersonaDto,
    pub locale: LocaleDto,
    pub window: WindowDto,
    pub startup_urls: Vec<String>,
}

impl From<&domain::ProfileSpec> for ProfileDraftDto {
    fn from(value: &domain::ProfileSpec) -> Self {
        let profile = ProfileDto::from(value);
        Self {
            name: profile.name,
            tags: profile.tags,
            preset_id: profile.preset_id,
            preset_version: profile.preset_version,
            proxy_id: profile.proxy_id,
            browser: profile.browser,
            persona: profile.persona,
            locale: profile.locale,
            window: profile.window,
            startup_urls: profile.startup_urls,
        }
    }
}

impl ProfileDraftDto {
    pub fn into_domain(
        self,
        id: domain::ProfileId,
        revision: domain::Revision,
        archived: bool,
    ) -> Result<domain::ProfileSpec, ContractError> {
        ProfileDto {
            id: id.into(),
            name: self.name,
            tags: self.tags,
            preset_id: self.preset_id,
            preset_version: self.preset_version,
            proxy_id: self.proxy_id,
            browser: self.browser,
            persona: self.persona,
            locale: self.locale,
            window: self.window,
            startup_urls: self.startup_urls,
            revision: revision.get(),
            archived,
        }
        .try_into()
    }
}

impl From<&domain::ProfileSpec> for ProfileDto {
    fn from(value: &domain::ProfileSpec) -> Self {
        let browser = match &value.browser {
            domain::BrowserBuildSelector::Pinned(id) => BrowserSelectorDto::Pinned {
                build_id: id.into(),
            },
            domain::BrowserBuildSelector::Channel(channel) => BrowserSelectorDto::Channel {
                channel: (*channel).into(),
            },
        };
        let persona = match &value.persona {
            domain::FingerprintPersona::SystemNative(persona) => PersonaDto::SystemNative {
                preserve_system_timezone: persona.preserve_system_timezone,
                preserve_system_fonts: persona.preserve_system_fonts,
                preserve_system_gpu: persona.preserve_system_gpu,
                preserve_system_audio: persona.preserve_system_audio,
            },
            domain::FingerprintPersona::SeedableNoise(p) => PersonaDto::SeedableNoise {
                seed: p.seed.clone(),
                platform: p.platform.into(),
                hardware_concurrency: p.hardware_concurrency,
                device_memory: p.device_memory.into(),
                touch_points: p.touch_points,
                screen: p.screen.into(),
                webgl_vendor: p.webgl.vendor.clone(),
                webgl_renderer: p.webgl.unmasked_renderer.clone(),
                spoof_canvas: p.spoof_canvas,
                spoof_audio: p.spoof_audio,
            },
        };
        Self {
            id: (&value.id).into(),
            name: value.name.clone(),
            tags: value.tags.clone(),
            preset_id: (&value.environment_preset.id).into(),
            preset_version: value.environment_preset.version,
            proxy_id: value.proxy_id.as_ref().map(Into::into),
            browser,
            persona,
            locale: LocaleDto {
                accept_languages: value.locale.accept_languages.clone(),
                timezone: value.locale.timezone.clone(),
            },
            window: WindowDto {
                width: value.window.width,
                height: value.window.height,
                start_maximized: value.window.start_maximized,
            },
            startup_urls: value.startup_urls.clone(),
            revision: value.revision.get(),
            archived: value.archived,
        }
    }
}

impl TryFrom<ProfileDto> for domain::ProfileSpec {
    type Error = ContractError;

    fn try_from(value: ProfileDto) -> Result<Self, Self::Error> {
        let browser = match value.browser {
            BrowserSelectorDto::Pinned { build_id } => {
                domain::BrowserBuildSelector::Pinned(build_id.try_into()?)
            }
            BrowserSelectorDto::Channel { channel } => {
                domain::BrowserBuildSelector::Channel(channel.into())
            }
        };
        let persona = match value.persona {
            PersonaDto::SystemNative {
                preserve_system_timezone,
                preserve_system_fonts,
                preserve_system_gpu,
                preserve_system_audio,
            } => domain::FingerprintPersona::SystemNative(domain::SystemNativePersona {
                preserve_system_timezone,
                preserve_system_fonts,
                preserve_system_gpu,
                preserve_system_audio,
            }),
            PersonaDto::SeedableNoise {
                seed,
                platform,
                hardware_concurrency,
                device_memory,
                touch_points,
                screen,
                webgl_vendor,
                webgl_renderer,
                spoof_canvas,
                spoof_audio,
            } => domain::FingerprintPersona::SeedableNoise(domain::SeedableNoisePersona {
                seed,
                platform: platform.into(),
                hardware_concurrency,
                device_memory: device_memory.into(),
                touch_points,
                screen: screen.into(),
                webgl: domain::WebGLProfile {
                    vendor: webgl_vendor,
                    unmasked_renderer: webgl_renderer,
                },
                spoof_canvas,
                spoof_audio,
            }),
        };
        let profile = domain::ProfileSpec {
            id: value.id.try_into()?,
            name: value.name,
            tags: value.tags,
            environment_preset: domain::EnvironmentPresetRef::new(
                value.preset_id.try_into()?,
                value.preset_version,
            )
            .map_err(ContractError::from_domain)?,
            proxy_id: value.proxy_id.map(TryInto::try_into).transpose()?,
            browser,
            persona,
            locale: domain::LocaleConfig {
                accept_languages: value.locale.accept_languages,
                timezone: value.locale.timezone,
            },
            window: domain::WindowConfig {
                width: value.window.width,
                height: value.window.height,
                start_maximized: value.window.start_maximized,
            },
            startup_urls: value.startup_urls,
            revision: domain::Revision::new(value.revision).map_err(ContractError::from_domain)?,
            archived: value.archived,
        };
        profile.validate().map_err(ContractError::from_domain)?;
        Ok(profile)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintSummaryDto {
    pub persona: String,
    pub isolated_fields: Vec<String>,
    pub configurable_fields: Vec<String>,
    pub host_shared_fields: Vec<String>,
}

impl Default for FingerprintSummaryDto {
    fn default() -> Self {
        Self {
            persona: "system_native".to_owned(),
            isolated_fields: vec![
                "cookies".to_owned(),
                "storage".to_owned(),
                "cache".to_owned(),
                "permissions".to_owned(),
            ],
            configurable_fields: vec![
                "proxy".to_owned(),
                "language".to_owned(),
                "startup_urls".to_owned(),
                "window".to_owned(),
            ],
            host_shared_fields: domain::FingerprintPersona::SystemNative(
                domain::SystemNativePersona::default(),
            )
            .host_shared_fields()
            .iter()
            .map(|v| (*v).to_owned())
            .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateDto {
    Stopped,
    Queued,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl From<&domain::SessionState> for SessionStateDto {
    fn from(value: &domain::SessionState) -> Self {
        match value {
            domain::SessionState::Stopped => Self::Stopped,
            domain::SessionState::Queued => Self::Queued,
            domain::SessionState::Starting => Self::Starting,
            domain::SessionState::Running => Self::Running,
            domain::SessionState::Stopping => Self::Stopping,
            domain::SessionState::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub id: SessionIdDto,
    pub profile_id: ProfileIdDto,
    pub state: SessionStateDto,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

impl From<&domain::Session> for SessionDto {
    fn from(value: &domain::Session) -> Self {
        Self {
            id: (&value.id).into(),
            profile_id: (&value.profile_id).into(),
            state: (&value.state).into(),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
            failure_code: value.failure.as_ref().map(|failure| failure.code.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxySchemeDto {
    Http,
    Https,
    Socks5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsModeDto {
    Proxy,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyDto {
    pub id: ProxyIdDto,
    pub name: String,
    pub scheme: ProxySchemeDto,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub has_secret: bool,
    pub dns_mode: DnsModeDto,
    pub bypass_hosts: Vec<String>,
    pub connect_timeout_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyDraftDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ProxyIdDto>,
    pub name: String,
    pub scheme: ProxySchemeDto,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub dns_mode: DnsModeDto,
    #[serde(default)]
    pub bypass_hosts: Vec<String>,
    #[serde(default = "default_proxy_timeout_ms")]
    pub connect_timeout_ms: u32,
}

const fn default_proxy_timeout_ms() -> u32 {
    domain::DEFAULT_PROXY_TIMEOUT_MS
}

impl From<&domain::ProxySpec> for ProxyDto {
    fn from(value: &domain::ProxySpec) -> Self {
        Self {
            id: (&value.id).into(),
            name: value.name.clone(),
            scheme: value.scheme.into(),
            host: value.host.clone(),
            port: value.port,
            username: value.username.clone(),
            has_secret: value.secret_ref.is_some(),
            dns_mode: value.dns_mode.into(),
            bypass_hosts: value.bypass_hosts.clone(),
            connect_timeout_ms: value.connect_timeout_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticScopeDto {
    Isolated,
    Configurable,
    HostShared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatusDto {
    Pass,
    Warn,
    Fail,
    NotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOverallDto {
    Healthy,
    Attention,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCheckDto {
    pub key: String,
    pub scope: DiagnosticScopeDto,
    pub status: DiagnosticStatusDto,
    pub observed: Option<String>,
    pub expected: Option<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReportDto {
    pub profile_id: ProfileIdDto,
    pub session_id: Option<SessionIdDto>,
    pub generated_at_ms: u64,
    pub checks: Vec<DiagnosticCheckDto>,
    pub overall: DiagnosticOverallDto,
    pub external_endpoints: Vec<String>,
}

impl From<&domain::DiagnosticCheck> for DiagnosticCheckDto {
    fn from(value: &domain::DiagnosticCheck) -> Self {
        let scope = match value.scope {
            domain::DiagnosticScope::Isolated => DiagnosticScopeDto::Isolated,
            domain::DiagnosticScope::Configurable => DiagnosticScopeDto::Configurable,
            domain::DiagnosticScope::HostShared => DiagnosticScopeDto::HostShared,
        };
        let status = match value.status {
            domain::DiagnosticStatus::Pass => DiagnosticStatusDto::Pass,
            domain::DiagnosticStatus::Warn => DiagnosticStatusDto::Warn,
            domain::DiagnosticStatus::Fail => DiagnosticStatusDto::Fail,
            domain::DiagnosticStatus::NotChecked => DiagnosticStatusDto::NotChecked,
        };
        Self {
            key: value.key.clone(),
            scope,
            status,
            observed: value.observed.clone(),
            expected: value.expected.clone(),
            explanation: value.explanation.clone(),
        }
    }
}

impl From<&domain::DiagnosticReport> for DiagnosticReportDto {
    fn from(value: &domain::DiagnosticReport) -> Self {
        let overall = match value.overall {
            domain::DiagnosticOverall::Healthy => DiagnosticOverallDto::Healthy,
            domain::DiagnosticOverall::Attention => DiagnosticOverallDto::Attention,
            domain::DiagnosticOverall::Failed => DiagnosticOverallDto::Failed,
        };
        Self {
            profile_id: (&value.profile_id).into(),
            session_id: value.session_id.as_ref().map(Into::into),
            generated_at_ms: value.generated_at_ms,
            checks: value.checks.iter().map(Into::into).collect(),
            overall,
            external_endpoints: value.external_endpoints.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTrustDto {
    Verified,
    Untrusted,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDto {
    pub id: BrowserBuildIdDto,
    pub provider: String,
    pub version: String,
    pub channel: BrowserChannelDto,
    pub publisher: String,
    pub sha256: String,
    pub trust: RuntimeTrustDto,
    pub in_use_by: Vec<ProfileIdDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressDto {
    pub phase: ProgressPhase,
    pub completed: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

// ----- Commands ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListRequest {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
}

const fn default_page() -> u32 {
    1
}

const fn default_page_size() -> u32 {
    50
}

impl Default for ProfileListRequest {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: 50,
            search: None,
            include_archived: false,
        }
    }
}

impl ProfileListRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.page == 0 || self.page_size == 0 || self.page_size > 200 {
            return Err(ContractError::invalid_field(
                "page_size",
                "must be between 1 and 200; page starts at 1",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCreateRequest {
    pub profile: ProfileDraftDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateRequest {
    pub profile_id: ProfileIdDto,
    pub profile: ProfileDraftDto,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileIdRequest {
    pub profile_id: ProfileIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRevisionRequest {
    pub profile_id: ProfileIdDto,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLaunchRequest {
    pub profile_id: ProfileIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStopRequest {
    pub session_id: SessionIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCancelQueuedRequest {
    pub session_id: SessionIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySaveRequest {
    pub proxy: ProxyDraftDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<SecretInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestRequest {
    pub proxy_id: ProxyIdDto,
    #[serde(default)]
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRemoveRequest {
    pub proxy_id: ProxyIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConnectivityCheckDto {
    pub proxy_id: ProxyIdDto,
    pub reachable: bool,
    pub latency_ms: u64,
    pub detail: String,
    pub exit_ip: Option<String>,
    pub exit_ip_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRunRequest {
    pub profile_id: ProfileIdDto,
    #[serde(default)]
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticGetRequest {
    pub report_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<ProfileIdDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallRequest {
    pub provider: String,
    pub version: String,
    pub channel: BrowserChannelDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeIdRequest {
    pub runtime_id: BrowserBuildIdDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupExportRequest {
    pub profile_id: ProfileIdDto,
    pub destination_name: String,
    pub password: SecretInput,
    pub include_proxy_credential: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupImportRequest {
    pub source_name: String,
    pub password: SecretInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMigrationInspectRequest {
    pub profile_id: ProfileIdDto,
    pub source_profile_id: String,
}

pub type LegacyMigrationExecuteRequest = LegacyMigrationInspectRequest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMigrationRollbackRequest {
    pub migration_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMigrationListRequest {
    #[serde(default = "default_migration_list_limit")]
    pub limit: usize,
}

fn default_migration_list_limit() -> usize {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMigrationInspectionDto {
    pub source_profile_id: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub has_history: bool,
    pub has_cookies: bool,
    pub has_login_data: bool,
    pub has_local_storage: bool,
    pub has_indexed_db: bool,
    pub source_may_be_running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyMigrationStatusDto {
    Running,
    Completed,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMigrationRecordDto {
    pub id: u64,
    pub profile_id: ProfileIdDto,
    pub source_kind: String,
    pub source_profile_id: String,
    pub status: LegacyMigrationStatusDto,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_bytes: Option<u64>,
    pub can_rollback: bool,
    #[serde(default)]
    pub recovery_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    #[serde(default = "default_stop_sessions_on_exit")]
    pub stop_sessions_on_exit: bool,
    pub max_concurrent_sessions: u32,
    pub retain_archived_days: u32,
    pub telemetry_enabled: bool,
    pub check_updates: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatchDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sessions_on_exit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_sessions: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_archived_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
}

fn default_stop_sessions_on_exit() -> bool {
    true
}

impl From<&domain::ManagerSettings> for SettingsDto {
    fn from(value: &domain::ManagerSettings) -> Self {
        Self {
            max_concurrent_sessions: value.max_concurrent_sessions as u32,
            retain_archived_days: 7,
            telemetry_enabled: false,
            check_updates: false,
            stop_sessions_on_exit: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateRequest {
    pub changes: SettingsPatchDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRuntimeDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub channel: BrowserChannelDto,
    pub trusted: bool,
    pub active: bool,
    pub provider: String,
    pub publisher: String,
    pub executable_path: String,
    pub fingerprint_support: FingerprintSupportDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintSupportDto {
    Unverified,
    Supported,
    Unsupported,
}

impl From<domain::FingerprintSupport> for FingerprintSupportDto {
    fn from(value: domain::FingerprintSupport) -> Self {
        match value {
            domain::FingerprintSupport::Unverified => Self::Unverified,
            domain::FingerprintSupport::Supported => Self::Supported,
            domain::FingerprintSupport::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintProbeResultDto {
    pub runtime: BrowserRuntimeDto,
    pub detail: String,
}

/// Local-only samples, never observations from a saved or logged-in profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintMeasurementReportDto {
    pub schema_version: u32,
    pub runtime_id: String,
    pub runtime_version: String,
    pub runtime_sha256: String,
    pub measured_at_ms: u64,
    pub mode: String,
    pub covered_checks_passed: bool,
    pub samples: Vec<FingerprintSampleDto>,
    pub checks: Vec<FingerprintMeasurementCheckDto>,
    pub limitations: Vec<String>,
    pub external_endpoints: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintSampleDto {
    pub label: String,
    pub configured: FingerprintSampleConfigDto,
    pub observations: Vec<FingerprintObservationDto>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintSampleConfigDto {
    pub seed: String,
    pub user_agent: String,
    pub platform: String,
    pub languages: Vec<String>,
    pub concurrency: u32,
    pub memory: f64,
    pub screen_width: u32,
    pub screen_height: u32,
    pub color_depth: u32,
    pub window_width: u32,
    pub window_height: u32,
    pub webgl_vendor: String,
    pub webgl_renderer: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintObservationDto {
    pub round: u32,
    pub page: FingerprintSignalsDto,
    pub iframe: Option<FingerprintSignalsDto>,
    pub worker: Option<FingerprintSignalsDto>,
    pub audio: Option<FingerprintAudioDto>,
    pub network: FingerprintHeadersDto,
    pub storage: FingerprintStorageDto,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FingerprintSignalsDto {
    pub user_agent: Option<String>,
    pub platform: Option<String>,
    pub language: Option<String>,
    pub languages: Vec<String>,
    pub concurrency: Option<u32>,
    pub memory: Option<f64>,
    pub memory_type: String,
    pub touch_points: Option<u32>,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
    pub color_depth: Option<u32>,
    pub inner_width: Option<u32>,
    pub inner_height: Option<u32>,
    pub outer_width: Option<u32>,
    pub outer_height: Option<u32>,
    pub pixel_ratio: Option<f64>,
    pub webgl_vendor: Option<String>,
    pub webgl_renderer: Option<String>,
    pub timezone: Option<String>,
    pub client_hints: Option<FingerprintClientHintsDto>,
    pub canvas: Option<FingerprintCanvasDto>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FingerprintClientHintsDto {
    pub brands: Vec<String>,
    pub platform: Option<String>,
    pub mobile: Option<bool>,
    pub architecture: Option<String>,
    pub bitness: Option<String>,
    pub platform_version: Option<String>,
    pub full_version: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FingerprintCanvasDto {
    pub read_hash: String,
    pub repeat_read_hash: String,
    pub after_export_read_hash: String,
    pub export_hash: Option<String>,
    pub repeat_export_hash: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FingerprintAudioDto {
    pub first_hash: String,
    pub repeat_hash: String,
    pub first_event_hash: String,
    pub repeat_event_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FingerprintHeadersDto {
    pub user_agent: Option<String>,
    pub accept_language: Option<String>,
    pub sec_ch_ua: Option<String>,
    pub sec_ch_ua_platform: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintStorageDto {
    pub local_before: String,
    pub local_after: String,
    pub cookie_before: String,
    pub cookie_after: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintMeasurementStatusDto {
    Pass,
    Fail,
    Observed,
    NotCovered,
    NotMeasured,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintMeasurementCheckDto {
    pub key: String,
    pub label: String,
    pub scope: DiagnosticScopeDto,
    pub required: bool,
    pub status: FingerprintMeasurementStatusDto,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub explanation: String,
}

impl From<&domain::BrowserBuild> for BrowserRuntimeDto {
    fn from(value: &domain::BrowserBuild) -> Self {
        Self {
            id: value.id.as_str().to_owned(),
            name: format!("{} {}", value.provider, value.version),
            version: value.version.clone(),
            channel: value.channel.into(),
            trusted: matches!(value.trust, domain::RuntimeTrust::Verified),
            active: true,
            provider: value.provider.clone(),
            publisher: value.publisher.clone(),
            executable_path: value.executable_path.to_string_lossy().into_owned(),
            fingerprint_support: value.capabilities.fingerprint_support.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all_fields = "camelCase")]
pub enum CommandV1 {
    #[serde(rename = "profile.list")]
    ProfileList(ProfileListRequest),
    #[serde(rename = "profile.create")]
    ProfileCreate(ProfileCreateRequest),
    #[serde(rename = "profile.get")]
    ProfileGet(ProfileIdRequest),
    #[serde(rename = "profile.update")]
    ProfileUpdate(ProfileUpdateRequest),
    #[serde(rename = "profile.clone")]
    ProfileClone(ProfileIdRequest),
    #[serde(rename = "profile.archive")]
    ProfileArchive(ProfileRevisionRequest),
    #[serde(rename = "profile.restore")]
    ProfileRestore(ProfileRevisionRequest),
    #[serde(rename = "session.launch")]
    SessionLaunch(SessionLaunchRequest),
    #[serde(rename = "session.stop")]
    SessionStop(SessionStopRequest),
    #[serde(rename = "session.list")]
    SessionList,
    #[serde(rename = "session.cancel_queued")]
    SessionCancelQueued(SessionCancelQueuedRequest),
    #[serde(rename = "proxy.list")]
    ProxyList,
    #[serde(rename = "proxy.save")]
    ProxySave(ProxySaveRequest),
    #[serde(rename = "proxy.test")]
    ProxyTest(ProxyTestRequest),
    #[serde(rename = "proxy.remove")]
    ProxyRemove(ProxyIdDto),
    #[serde(rename = "diagnostic.run")]
    DiagnosticRun(DiagnosticRunRequest),
    #[serde(rename = "diagnostic.get")]
    DiagnosticGet(DiagnosticGetRequest),
    #[serde(rename = "diagnostic.list")]
    DiagnosticList(DiagnosticListRequest),
    #[serde(rename = "runtime.list")]
    RuntimeList,
    #[serde(rename = "runtime.install")]
    RuntimeInstall(RuntimeInstallRequest),
    #[serde(rename = "runtime.activate")]
    RuntimeActivate(RuntimeIdRequest),
    #[serde(rename = "runtime.remove")]
    RuntimeRemove(RuntimeIdRequest),
    #[serde(rename = "backup.inspect")]
    BackupInspect { source_name: String },
    #[serde(rename = "backup.list")]
    BackupList,
    #[serde(rename = "backup.export")]
    BackupExport(BackupExportRequest),
    #[serde(rename = "backup.import")]
    BackupImport(BackupImportRequest),
    #[serde(rename = "settings.get")]
    SettingsGet,
    #[serde(rename = "settings.update")]
    SettingsUpdate(SettingsUpdateRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEnvelope {
    pub version: u16,
    pub request_id: String,
    pub command: CommandV1,
}

impl CommandEnvelope {
    pub fn new(request_id: impl Into<String>, command: CommandV1) -> Result<Self, ContractError> {
        let request_id = request_id.into();
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(ContractError::invalid_field(
                "requestId",
                "must be non-empty and at most 128 bytes",
            ));
        }
        let envelope = Self {
            version: CONTRACT_VERSION,
            request_id,
            command,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.version != CONTRACT_VERSION {
            return Err(ContractError::invalid_field(
                "version",
                format!("unsupported contract version {}", self.version),
            ));
        }
        if self.request_id.trim().is_empty() || self.request_id.len() > 128 {
            return Err(ContractError::invalid_field(
                "requestId",
                "must be non-empty and at most 128 bytes",
            ));
        }
        match &self.command {
            CommandV1::ProfileList(request) => request.validate(),
            CommandV1::ProfileUpdate(request) if request.expected_revision == 0 => Err(
                ContractError::invalid_field("expectedRevision", "must be greater than zero"),
            ),
            CommandV1::ProfileArchive(request) | CommandV1::ProfileRestore(request)
                if request.expected_revision == 0 =>
            {
                Err(ContractError::invalid_field(
                    "expectedRevision",
                    "must be greater than zero",
                ))
            }
            CommandV1::SettingsUpdate(request)
                if request
                    .changes
                    .max_concurrent_sessions
                    .is_some_and(|value| {
                        value == 0 || value > domain::MAX_CONCURRENT_SESSIONS as u32
                    }) =>
            {
                Err(ContractError::invalid_field(
                    "maxConcurrentSessions",
                    "must be between 1 and 10",
                ))
            }
            CommandV1::BackupInspect { source_name } if !is_safe_opaque_name(source_name) => {
                Err(ContractError::invalid_field(
                    "sourceName",
                    "must be an opaque file-picker token, not a path",
                ))
            }
            CommandV1::BackupExport(request) if !is_safe_opaque_name(&request.destination_name) => {
                Err(ContractError::invalid_field(
                    "destinationName",
                    "must be an opaque file-picker token, not a path",
                ))
            }
            CommandV1::BackupImport(request) if !is_safe_opaque_name(&request.source_name) => {
                Err(ContractError::invalid_field(
                    "sourceName",
                    "must be an opaque file-picker token, not a path",
                ))
            }
            _ => Ok(()),
        }
    }
}

fn is_safe_opaque_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

// ----- Events -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateChangedEvent {
    pub session: SessionDto,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchQueuedEvent {
    pub session_id: SessionIdDto,
    pub profile_id: ProfileIdDto,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticProgressEvent {
    pub profile_id: ProfileIdDto,
    pub progress: ProgressDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallProgressEvent {
    pub runtime_id: Option<BrowserBuildIdDto>,
    pub progress: ProgressDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdateAvailableEvent {
    pub runtime: RuntimeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupProgressEvent {
    pub operation_id: String,
    pub progress: ProgressDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventV1 {
    #[serde(rename = "SessionStateChanged")]
    SessionStateChanged(SessionStateChangedEvent),
    #[serde(rename = "LaunchQueued")]
    LaunchQueued(LaunchQueuedEvent),
    #[serde(rename = "DiagnosticProgress")]
    DiagnosticProgress(DiagnosticProgressEvent),
    #[serde(rename = "RuntimeInstallProgress")]
    RuntimeInstallProgress(RuntimeInstallProgressEvent),
    #[serde(rename = "RuntimeUpdateAvailable")]
    RuntimeUpdateAvailable(RuntimeUpdateAvailableEvent),
    #[serde(rename = "BackupProgress")]
    BackupProgress(BackupProgressEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub version: u16,
    pub event_id: String,
    pub event: EventV1,
}

impl EventEnvelope {
    pub fn new(event_id: impl Into<String>, event: EventV1) -> Result<Self, ContractError> {
        let event_id = event_id.into();
        if event_id.trim().is_empty() || event_id.len() > 128 {
            return Err(ContractError::invalid_field(
                "eventId",
                "must be non-empty and at most 128 bytes",
            ));
        }
        Ok(Self {
            version: CONTRACT_VERSION,
            event_id,
            event,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub field: Option<String>,
    pub reason: String,
}

impl ContractError {
    pub fn invalid_field(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: Some(field.into()),
            reason: reason.into(),
        }
    }

    fn from_domain(error: domain::DomainError) -> Self {
        Self {
            field: None,
            reason: error.to_string(),
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = &self.field {
            write!(f, "{field}: {}", self.reason)
        } else {
            f.write_str(&self.reason)
        }
    }
}

impl std::error::Error for ContractError {}

impl From<domain::BrowserChannel> for BrowserChannelDto {
    fn from(value: domain::BrowserChannel) -> Self {
        match value {
            domain::BrowserChannel::Stable => Self::Stable,
            domain::BrowserChannel::Beta => Self::Beta,
            domain::BrowserChannel::Dev => Self::Dev,
            domain::BrowserChannel::Canary => Self::Canary,
        }
    }
}

impl From<BrowserChannelDto> for domain::BrowserChannel {
    fn from(value: BrowserChannelDto) -> Self {
        match value {
            BrowserChannelDto::Stable => Self::Stable,
            BrowserChannelDto::Beta => Self::Beta,
            BrowserChannelDto::Dev => Self::Dev,
            BrowserChannelDto::Canary => Self::Canary,
        }
    }
}

impl From<domain::ProxyScheme> for ProxySchemeDto {
    fn from(value: domain::ProxyScheme) -> Self {
        match value {
            domain::ProxyScheme::Http => Self::Http,
            domain::ProxyScheme::Https => Self::Https,
            domain::ProxyScheme::Socks5 => Self::Socks5,
        }
    }
}

impl From<ProxySchemeDto> for domain::ProxyScheme {
    fn from(value: ProxySchemeDto) -> Self {
        match value {
            ProxySchemeDto::Http => Self::Http,
            ProxySchemeDto::Https => Self::Https,
            ProxySchemeDto::Socks5 => Self::Socks5,
        }
    }
}

impl From<domain::DnsMode> for DnsModeDto {
    fn from(value: domain::DnsMode) -> Self {
        match value {
            domain::DnsMode::Proxy => Self::Proxy,
            domain::DnsMode::Local => Self::Local,
        }
    }
}

impl From<DnsModeDto> for domain::DnsMode {
    fn from(value: DnsModeDto) -> Self {
        match value {
            DnsModeDto::Proxy => Self::Proxy,
            DnsModeDto::Local => Self::Local,
        }
    }
}

impl From<domain::SessionState> for SessionStateDto {
    fn from(value: domain::SessionState) -> Self {
        (&value).into()
    }
}

impl From<domain::ProxySpec> for ProxyDto {
    fn from(value: domain::ProxySpec) -> Self {
        (&value).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_dto() -> ProfileDto {
        ProfileDto {
            id: ProfileIdDto::new("p-1").unwrap(),
            name: "QA".to_owned(),
            tags: vec!["test".to_owned()],
            preset_id: PresetIdDto::new("system-native").unwrap(),
            preset_version: 1,
            proxy_id: None,
            browser: BrowserSelectorDto::Channel {
                channel: BrowserChannelDto::Stable,
            },
            persona: PersonaDto::default(),
            locale: LocaleDto {
                accept_languages: vec!["en-US".to_owned()],
                timezone: None,
            },
            window: WindowDto {
                width: 1280,
                height: 800,
                start_maximized: false,
            },
            startup_urls: vec!["https://example.test".to_owned()],
            revision: 1,
            archived: false,
        }
    }

    #[test]
    fn profile_dto_round_trips_through_domain() {
        let dto = profile_dto();
        let domain: domain::ProfileSpec = dto.clone().try_into().unwrap();
        assert_eq!(ProfileDto::from(&domain), dto);
    }

    #[test]
    fn fingerprint_support_is_serialized_independently_of_publisher_trust() {
        for (support, expected) in [
            (domain::FingerprintSupport::Unverified, "unverified"),
            (domain::FingerprintSupport::Supported, "supported"),
            (domain::FingerprintSupport::Unsupported, "unsupported"),
        ] {
            assert_eq!(
                serde_json::to_value(FingerprintSupportDto::from(support)).unwrap(),
                expected
            );
        }
        assert_eq!(
            domain::BrowserCapabilities::default().fingerprint_support,
            domain::FingerprintSupport::Unverified
        );
    }

    #[test]
    fn command_envelope_is_versioned_and_does_not_expose_secret_in_debug() {
        let secret = SecretInput::new("super-secret").unwrap();
        assert!(!format!("{secret:?}").contains("super-secret"));
        let envelope = CommandEnvelope::new("request-1", CommandV1::SessionList).unwrap();
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["version"], CONTRACT_VERSION);
        assert_eq!(value["command"]["type"], "session.list");
    }

    #[test]
    fn wire_fields_use_frontend_camel_case() {
        let value = serde_json::to_value(ProfileListRequest {
            page: 1,
            page_size: 50,
            search: None,
            include_archived: true,
        })
        .unwrap();
        assert_eq!(value["pageSize"], 50);
        assert_eq!(value["includeArchived"], true);
        assert!(value.get("page_size").is_none());

        let profile = serde_json::to_value(profile_dto()).unwrap();
        assert_eq!(profile["presetId"], "system-native");
        assert_eq!(profile["browser"]["mode"], "channel");
        assert_eq!(profile["browser"]["value"]["channel"], "stable");
        assert_eq!(
            profile["persona"]["system_native"]["preserveSystemTimezone"],
            true
        );
    }

    #[test]
    fn invalid_page_size_is_rejected_before_ipc() {
        let request = ProfileListRequest {
            page: 1,
            page_size: 201,
            search: None,
            include_archived: false,
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn fingerprint_preview_wire_format_keeps_runtime_and_scale_explicit() {
        let preview = FingerprintPreviewDto {
            runtime_id: "selected-chrome".to_owned(),
            runtime_version: "150.0.1.2".to_owned(),
            effective_user_agent: "Chrome/150.0.0.0".to_owned(),
            effective_platform: "Win32".to_owned(),
            effective_languages: vec!["zh-CN".to_owned()],
            effective_window_width: 1280,
            effective_window_height: 800,
            effective_screen_width: 1920,
            effective_screen_height: 1080,
            device_scale_factor_hundredths: 125,
            effective_webgl_vendor: "Google Inc. (Intel)".to_owned(),
            effective_webgl_renderer: "ANGLE (Intel)".to_owned(),
            hardware_concurrency: 8,
            device_memory_gb: "8".to_owned(),
            declared_only_fields: vec!["navigator".to_owned()],
            host_shared_fields: vec!["workers".to_owned()],
            runtime_warnings: vec!["configuration only".to_owned()],
        };
        let value = serde_json::to_value(&preview).unwrap();
        assert_eq!(value["runtimeId"], "selected-chrome");
        assert_eq!(value["runtimeVersion"], "150.0.1.2");
        assert_eq!(value["deviceScaleFactorHundredths"], 125);
        assert_eq!(value["effectiveScreenWidth"], 1920);
        assert_eq!(value["runtimeWarnings"][0], "configuration only");
        assert_eq!(
            serde_json::from_value::<FingerprintPreviewDto>(value).unwrap(),
            preview
        );
    }

    #[test]
    fn empty_secret_and_whitespace_ids_fail_deserialization() {
        assert!(serde_json::from_str::<SecretInput>("\"\"").is_err());
        assert!(serde_json::from_str::<ProfileIdDto>("\"   \"").is_err());
    }

    #[test]
    fn backup_contract_rejects_frontend_supplied_paths() {
        let command = CommandV1::BackupInspect {
            source_name: "C:\\Users\\tester\\backup.browsbak".to_owned(),
        };
        assert!(CommandEnvelope::new("request-2", command).is_err());
    }
    #[test]
    fn settings_contract_rejects_unknown_fields_and_non_integer_limits() {
        for changes in [
            serde_json::json!({"maxConcurrentSessions": 1.5}),
            serde_json::json!({"maxConcurrentSessions": -1}),
            serde_json::json!({"activeRuntimeId": "unregistered"}),
            serde_json::json!({"stopSessionsOnExit": "false"}),
        ] {
            assert!(serde_json::from_value::<SettingsUpdateRequest>(
                serde_json::json!({"changes": changes})
            )
            .is_err());
        }
        let dto = SettingsDto::from(&domain::ManagerSettings::default());
        let value = serde_json::to_value(dto).unwrap();
        assert_eq!(value["stopSessionsOnExit"], true);
        assert_eq!(value["telemetryEnabled"], false);
        assert_eq!(value["maxConcurrentSessions"], 10);
    }
}
