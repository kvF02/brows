use crate::{
    BrowserBuildId, DomainError, PresetId, ProfileId, ProxyId, Revision, SecretRef, SessionId,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub const MAX_PROFILE_NAME_CHARS: usize = 128;
pub const MAX_TAG_CHARS: usize = 64;
pub const DEFAULT_PROXY_TIMEOUT_MS: u32 = 15_000;
pub const DEFAULT_MAX_CONCURRENT_SESSIONS: usize = 10;
pub const MAX_CONCURRENT_SESSIONS: usize = 10;
pub const MAX_QUEUED_SESSIONS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPresetRef {
    pub id: PresetId,
    pub version: u32,
}

impl EnvironmentPresetRef {
    pub fn new(id: PresetId, version: u32) -> Result<Self, DomainError> {
        if version == 0 {
            return Err(DomainError::InvalidRevision);
        }
        Ok(Self { id, version })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentPresetKind {
    SystemNative,
    RegionalNetwork,
    QaRestricted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPreset {
    pub reference: EnvironmentPresetRef,
    pub name: String,
    pub kind: EnvironmentPresetKind,
    pub description: String,
}

impl EnvironmentPreset {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_name(&self.name)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserChannel {
    Stable,
    Beta,
    Dev,
    Canary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserBuildSelector {
    Pinned(BrowserBuildId),
    Channel(BrowserChannel),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTrust {
    Verified,
    Untrusted,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FingerprintSupport {
    /// Publisher trust alone is not evidence that MAIN-world hooks load.
    #[default]
    Unverified,
    /// A bounded local probe observed document-start hooks.
    Supported,
    /// The local probe completed but did not observe the required hooks.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BrowserCapabilities {
    pub supports_language: bool,
    pub supports_proxy: bool,
    pub supports_webrtc_policy: bool,
    pub supports_devtools_pipe: bool,
    pub fingerprint_support: FingerprintSupport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserBuild {
    pub id: BrowserBuildId,
    pub provider: String,
    pub version: String,
    pub channel: BrowserChannel,
    pub executable_path: PathBuf,
    /// Lowercase hexadecimal SHA-256 digest.
    pub sha256: String,
    pub publisher: String,
    pub trust: RuntimeTrust,
    pub capabilities: BrowserCapabilities,
    pub installed_at_ms: u64,
}

impl BrowserBuild {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.provider.trim().is_empty() || self.version.trim().is_empty() {
            return Err(DomainError::InvalidId {
                kind: "browser build",
                reason: "provider and version are required".to_owned(),
            });
        }
        validate_path(&self.executable_path)?;
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DomainError::InvalidHash);
        }
        if self.publisher.trim().is_empty() {
            return Err(DomainError::InvalidId {
                kind: "publisher",
                reason: "publisher is required".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Https,
    Socks5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsMode {
    Proxy,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub id: ProxyId,
    pub name: String,
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub secret_ref: Option<SecretRef>,
    pub dns_mode: DnsMode,
    pub bypass_hosts: Vec<String>,
    pub connect_timeout_ms: u32,
}

impl ProxySpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_name(&self.name)?;
        if self.host.trim().is_empty()
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || c == '/' || c == '\\')
        {
            return Err(DomainError::InvalidHost);
        }
        if self.port == 0 {
            return Err(DomainError::InvalidPort);
        }
        if self.username.as_ref().is_some_and(|v| v.trim().is_empty()) {
            return Err(DomainError::MissingProxySecret);
        }
        if self.username.is_some() && self.secret_ref.is_none() {
            return Err(DomainError::MissingProxySecret);
        }
        if self.connect_timeout_ms == 0 {
            return Err(DomainError::InvalidTimeout);
        }
        for host in &self.bypass_hosts {
            if host.trim().is_empty() || host.chars().any(char::is_whitespace) {
                return Err(DomainError::InvalidHost);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaKind {
    SystemNative,
    SeedableNoise,
}

/// The v0.1 persona intentionally preserves host-level browser properties.
/// It is a named capability so a future native runtime can be introduced
/// without adding ad-hoc flags to ProfileSpec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemNativePersona {
    pub preserve_system_timezone: bool,
    pub preserve_system_fonts: bool,
    pub preserve_system_gpu: bool,
    pub preserve_system_audio: bool,
}

impl Default for SystemNativePersona {
    fn default() -> Self {
        Self {
            preserve_system_timezone: true,
            preserve_system_fonts: true,
            preserve_system_gpu: true,
            preserve_system_audio: true,
        }
    }
}

impl SystemNativePersona {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.preserve_system_timezone
            && self.preserve_system_fonts
            && self.preserve_system_gpu
            && self.preserve_system_audio
        {
            Ok(())
        } else {
            Err(DomainError::InconsistentConfiguration(
                "SystemNative persona must preserve all host-level properties".to_owned(),
            ))
        }
    }
}

/// Discrete device-memory values, avoiding `f32` in domain types that need `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryGb {
    Quarter,
    Half,
    One,
    Two,
    Four,
    Eight,
}

impl DeviceMemoryGb {
    pub fn reported_f64(self) -> f64 {
        match self {
            Self::Quarter => 0.25,
            Self::Half => 0.5,
            Self::One => 1.0,
            Self::Two => 2.0,
            Self::Four => 4.0,
            Self::Eight => 8.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Windows,
    MacOs,
    Linux,
}

/// Screen configuration for the SeedableNoise persona.
/// `device_scale_factor_hundredths`: 100 = 1.0×, 150 = 1.5×, 200 = 2.0×. Range 50–400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenConfig {
    pub width: u32,
    pub height: u32,
    pub color_depth: u8,
    pub device_scale_factor_hundredths: u16,
}

impl ScreenConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.width < 320 || self.height < 240 {
            return Err(DomainError::InconsistentConfiguration(
                "screen dimensions must be at least 320×240".to_owned(),
            ));
        }
        if !matches!(self.color_depth, 16 | 24 | 30 | 48) {
            return Err(DomainError::InconsistentConfiguration(
                "color_depth must be 16, 24, 30, or 48".to_owned(),
            ));
        }
        if !(50..=400).contains(&self.device_scale_factor_hundredths) {
            return Err(DomainError::InconsistentConfiguration(
                "device_scale_factor must be between 0.5× and 4.0×".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Manually-specified WebGL UNMASKED_VENDOR/RENDERER strings.
/// Both empty ⇒ auto-derive from platform via `PersonaFingerprint`.
/// If either is set both must be non-empty and vendor must be compatible with the platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebGLProfile {
    pub vendor: String,
    pub unmasked_renderer: String,
}

impl WebGLProfile {
    pub fn auto() -> Self {
        Self {
            vendor: String::new(),
            unmasked_renderer: String::new(),
        }
    }

    pub fn validate(&self, platform: PlatformKind) -> Result<(), DomainError> {
        if self.vendor.is_empty() != self.unmasked_renderer.is_empty() {
            return Err(DomainError::InconsistentConfiguration(
                "webgl vendor and renderer must both be set or both be empty".to_owned(),
            ));
        }
        if self.vendor.is_empty() {
            return Ok(());
        }
        let allowed: &[&str] = match platform {
            PlatformKind::Windows => &["Google Inc.", "NVIDIA", "Intel", "AMD"],
            PlatformKind::MacOs => &["Apple Inc.", "Intel Inc.", "AMD"],
            PlatformKind::Linux => &["Mesa/X.org", "NVIDIA Corporation", "VMware, Inc.", "Intel"],
        };
        if !allowed.iter().any(|prefix| self.vendor.contains(prefix)) {
            return Err(DomainError::InconsistentConfiguration(format!(
                "webgl vendor '{}' is not consistent with the {:?} platform",
                self.vendor, platform
            )));
        }
        Ok(())
    }
}

/// A deterministic, seed-driven fingerprint persona.
/// The seed is stable across sessions; same seed → same reported properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedableNoisePersona {
    pub seed: String,
    pub platform: PlatformKind,
    /// 1..=128
    pub hardware_concurrency: u8,
    pub device_memory: DeviceMemoryGb,
    /// 0..=10
    pub touch_points: u8,
    pub screen: ScreenConfig,
    /// Empty = auto-derive from platform using the PRNG seed.
    pub webgl: WebGLProfile,
    /// Perturb supported page-level canvas reads/exports deterministically.
    pub spoof_canvas: bool,
    /// Perturb offline rendered audio buffers; live audio remains native.
    pub spoof_audio: bool,
}

impl SeedableNoisePersona {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.seed.trim().is_empty() {
            return Err(DomainError::InconsistentConfiguration(
                "SeedableNoise persona requires a non-empty seed".to_owned(),
            ));
        }
        if self.hardware_concurrency == 0 || self.hardware_concurrency > 128 {
            return Err(DomainError::InconsistentConfiguration(
                "hardware_concurrency must be between 1 and 128".to_owned(),
            ));
        }
        if self.touch_points > 10 {
            return Err(DomainError::InconsistentConfiguration(
                "touch_points must be between 0 and 10".to_owned(),
            ));
        }
        self.screen.validate()?;
        self.webgl.validate(self.platform)?;
        Ok(())
    }
}

/// Returns the set of language prefixes considered consistent with the given
/// IANA timezone region prefix (the segment before the first '/').
fn expected_language_prefixes(region: &str) -> &'static [&'static str] {
    match region {
        "Asia" => &["zh", "ja", "ko", "en", "th", "vi", "id", "ms", "hi"],
        "Europe" => &["en", "de", "fr", "es", "it", "nl", "pl", "ru", "sv", "pt"],
        "America" => &["en", "es", "pt", "fr"],
        "Africa" => &["en", "fr", "ar", "sw"],
        "Oceania" => &["en"],
        "Atlantic" => &["en", "pt"],
        _ => &["en"],
    }
}

/// Checks that at least one of the supplied `accept_languages` has a language
/// prefix that is considered consistent with the IANA `timezone`.
pub(crate) fn check_locale_consistency(
    timezone: &str,
    accept_languages: &[String],
) -> Result<(), DomainError> {
    let region = timezone.split('/').next().unwrap_or("");
    let allowed = expected_language_prefixes(region);
    let matched = accept_languages.iter().any(|lang| {
        let prefix = lang
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        allowed.contains(&prefix.as_str())
    });
    if !matched {
        return Err(DomainError::InconsistentConfiguration(format!(
            "timezone {timezone} requires one of the language prefixes {allowed:?}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintPersona {
    SystemNative(SystemNativePersona),
    SeedableNoise(SeedableNoisePersona),
}

impl FingerprintPersona {
    pub fn kind(&self) -> PersonaKind {
        match self {
            Self::SystemNative(_) => PersonaKind::SystemNative,
            Self::SeedableNoise(_) => PersonaKind::SeedableNoise,
        }
    }

    pub fn host_shared_fields(&self) -> Vec<&'static str> {
        let mut fields = match self {
            Self::SystemNative(_) => vec![
                "timezone",
                "canvas",
                "webgl",
                "audio",
                "fonts",
                "gpu",
                "cpu",
                "tls_http2",
            ],
            // JS hooks do not replace the rendering engine or host devices.
            Self::SeedableNoise(_) => vec![
                "timezone",
                "fonts",
                "gpu",
                "tls_http2",
                "client_hints",
                "workers",
                "webgl_pixels",
                "audio_device",
                "canvas_uncovered_apis",
            ],
        };
        if let Self::SeedableNoise(persona) = self {
            if !persona.spoof_canvas {
                fields.push("canvas");
            }
            if !persona.spoof_audio {
                fields.push("audio");
            }
        }
        fields
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleConfig {
    pub accept_languages: Vec<String>,
    pub timezone: Option<String>,
}

impl Default for LocaleConfig {
    fn default() -> Self {
        Self {
            accept_languages: vec!["en-US".to_owned(), "en".to_owned()],
            timezone: None,
        }
    }
}

impl LocaleConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.accept_languages.is_empty()
            || self
                .accept_languages
                .iter()
                .any(|v| v.trim().is_empty() || v.len() > 32)
        {
            return Err(DomainError::InconsistentConfiguration(
                "at least one valid language is required".to_owned(),
            ));
        }
        if self.timezone.as_ref().is_some_and(|v| v.trim().is_empty()) {
            return Err(DomainError::InconsistentConfiguration(
                "timezone cannot be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
    pub start_maximized: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            start_maximized: false,
        }
    }
}

impl WindowConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.width < 320 || self.height < 240 {
            return Err(DomainError::InconsistentConfiguration(
                "window dimensions are too small".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSpec {
    pub id: ProfileId,
    pub name: String,
    pub tags: Vec<String>,
    pub environment_preset: EnvironmentPresetRef,
    pub proxy_id: Option<ProxyId>,
    pub browser: BrowserBuildSelector,
    pub persona: FingerprintPersona,
    pub locale: LocaleConfig,
    pub window: WindowConfig,
    pub startup_urls: Vec<String>,
    pub revision: Revision,
    pub archived: bool,
}

impl ProfileSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_name(&self.name)?;
        let mut seen = BTreeSet::new();
        for tag in &self.tags {
            if tag.trim().is_empty()
                || tag.len() > MAX_TAG_CHARS
                || !seen.insert(tag.to_ascii_lowercase())
            {
                return Err(DomainError::InvalidTag(tag.clone()));
            }
        }
        self.locale.validate()?;
        self.window.validate()?;
        match &self.persona {
            FingerprintPersona::SystemNative(persona) => {
                persona.validate()?;
                if self.locale.timezone.is_some() {
                    return Err(DomainError::InconsistentConfiguration(
                        "SystemNative persona cannot override the host timezone".to_owned(),
                    ));
                }
            }
            FingerprintPersona::SeedableNoise(persona) => {
                persona.validate()?;
                // For SeedableNoise, the profile locale timezone is allowed and
                // must be consistent with the declared accept_languages.
                if let Some(ref tz) = self.locale.timezone {
                    check_locale_consistency(tz, &self.locale.accept_languages)?;
                }
            }
        }
        if self.startup_urls.len() > 32 {
            return Err(DomainError::InconsistentConfiguration(
                "no more than 32 startup URLs are allowed".to_owned(),
            ));
        }
        for url in &self.startup_urls {
            validate_startup_url(url)?;
        }
        if self.revision.get() == 0 {
            return Err(DomainError::InvalidRevision);
        }
        Ok(())
    }

    pub fn bump_revision(&mut self) {
        self.revision = self.revision.next();
    }
}

fn validate_name(name: &str) -> Result<(), DomainError> {
    if name.trim().is_empty() {
        return Err(DomainError::EmptyName);
    }
    if name.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(DomainError::NameTooLong {
            max: MAX_PROFILE_NAME_CHARS,
        });
    }
    Ok(())
}

fn validate_startup_url(url: &str) -> Result<(), DomainError> {
    let lower = url.to_ascii_lowercase();
    if url.trim().is_empty() || !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(DomainError::InvalidUrl(url.to_owned()));
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), DomainError> {
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(DomainError::InvalidPath);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchArgument {
    UserDataDirectory(PathBuf),
    ProxyServer {
        scheme: ProxyScheme,
        host: String,
        port: u16,
    },
    ProxyBypassList(Vec<String>),
    DisableNonProxiedWebRtc,
    AcceptLanguages(Vec<String>),
    WindowSize {
        width: u32,
        height: u32,
    },
    // ── v0.2 SeedableNoise additions ──────────────────────────────────────
    /// Overrides the browser's User-Agent request header.
    UserAgentOverride(String),
    /// Forces the device-pixel-ratio reported to pages (hundredths: 100=1.0×).
    ForceDeviceScaleFactor(u16),
    /// Loads an unpacked MV3 extension from this directory at startup.
    LoadExtension(PathBuf),
    /// Comma-separated Blink feature flags to disable (e.g. "AutomationControlled").
    DisableBlinkFeatures(Vec<String>),
    /// Comma-separated Blink feature flags to enable.
    EnableBlinkFeatures(Vec<String>),
}

impl LaunchArgument {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::UserDataDirectory(path) => validate_path(path),
            Self::ProxyServer { host, port, .. } => {
                if host.trim().is_empty()
                    || host
                        .chars()
                        .any(|c| c.is_whitespace() || c == '/' || c == '\\')
                    || *port == 0
                {
                    Err(DomainError::InvalidLaunchArgument(
                        "proxy server".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
            Self::AcceptLanguages(values) => {
                if values.is_empty()
                    || values.iter().any(|value| {
                        value.trim().is_empty()
                            || value.len() > 32
                            || value.chars().any(char::is_whitespace)
                    })
                {
                    Err(DomainError::InvalidLaunchArgument(
                        "accept languages".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
            Self::ProxyBypassList(values) => {
                if values
                    .iter()
                    .any(|value| value.trim().is_empty() || value.contains('\n'))
                {
                    Err(DomainError::InvalidLaunchArgument(
                        "proxy bypass list".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
            Self::DisableNonProxiedWebRtc => Ok(()),
            Self::WindowSize { width, height } if *width < 320 || *height < 240 => {
                Err(DomainError::InvalidLaunchArgument("window size".to_owned()))
            }
            Self::WindowSize { .. } => Ok(()),
            Self::UserAgentOverride(ua) => {
                if ua.trim().is_empty() || ua.len() > 512 || ua.contains('\n') {
                    Err(DomainError::InvalidLaunchArgument("user agent".to_owned()))
                } else {
                    Ok(())
                }
            }
            Self::ForceDeviceScaleFactor(f) if !(50..=400).contains(f) => {
                Err(DomainError::InvalidLaunchArgument(
                    "device scale factor must be 0.5×–4.0× (50–400 hundredths)".to_owned(),
                ))
            }
            Self::ForceDeviceScaleFactor(_) => Ok(()),
            Self::LoadExtension(path) => validate_path(path),
            Self::DisableBlinkFeatures(features) | Self::EnableBlinkFeatures(features) => {
                if features
                    .iter()
                    .any(|f| f.trim().is_empty() || f.contains(',') || f.contains('\n'))
                {
                    Err(DomainError::InvalidLaunchArgument(
                        "blink features".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyLaunchBinding {
    Direct,
    Loopback { host: String, port: u16 },
}

impl ProxyLaunchBinding {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Direct => Ok(()),
            Self::Loopback { host, port } => {
                if host != "127.0.0.1" && host != "::1" {
                    return Err(DomainError::InvalidHost);
                }
                if *port == 0 {
                    return Err(DomainError::InvalidPort);
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchWarning {
    HostFingerprintShared(&'static str),
    ProxyNotConfigured,
    UnsupportedCapability(&'static str),
    LocaleMayDifferFromSystem,
    /// A fingerprint field is declared in the persona but cannot be enforced at
    /// the pixel or kernel level — only the JS-readable value is overridden.
    DeclaredFingerprintOnly {
        field: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserPreferences {
    pub accept_languages: Vec<String>,
    pub startup_urls: Vec<String>,
    pub permissions_prompt: bool,
    // ── v0.2 SeedableNoise additions (all optional; None = browser default) ──
    pub webgl_enabled: Option<bool>,
    pub hardware_acceleration: Option<bool>,
    /// Content-settings notification value: 1 = ask, 2 = block.
    pub notifications_setting: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunchPlan {
    pub session_id: SessionId,
    pub profile_id: ProfileId,
    pub profile_revision: Revision,
    pub executable_path: PathBuf,
    pub user_data_dir: PathBuf,
    pub arguments: Vec<LaunchArgument>,
    pub preferences: BrowserPreferences,
    pub proxy: ProxyLaunchBinding,
    pub capabilities: BrowserCapabilities,
    pub warnings: Vec<LaunchWarning>,
    pub plan_hash: String,
}

impl ResolvedLaunchPlan {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path(&self.executable_path)?;
        validate_path(&self.user_data_dir)?;
        if self.plan_hash.len() != 64 || !self.plan_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DomainError::InvalidHash);
        }
        self.proxy.validate()?;
        for argument in &self.arguments {
            argument.validate()?;
        }
        let user_data_arguments: Vec<&PathBuf> = self
            .arguments
            .iter()
            .filter_map(|argument| {
                if let LaunchArgument::UserDataDirectory(path) = argument {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        if user_data_arguments.len() != 1 || user_data_arguments[0] != &self.user_data_dir {
            return Err(DomainError::InconsistentConfiguration(
                "launch plan must contain exactly one matching user data directory".to_owned(),
            ));
        }
        let proxy_arguments: Vec<(&String, u16)> = self
            .arguments
            .iter()
            .filter_map(|argument| {
                if let LaunchArgument::ProxyServer { host, port, .. } = argument {
                    Some((host, *port))
                } else {
                    None
                }
            })
            .collect();
        match &self.proxy {
            ProxyLaunchBinding::Direct if !proxy_arguments.is_empty() => {
                return Err(DomainError::InconsistentConfiguration(
                    "direct launch plan cannot contain a proxy argument".to_owned(),
                ));
            }
            ProxyLaunchBinding::Loopback { host, port }
                if proxy_arguments.len() != 1
                    || proxy_arguments[0].0 != host
                    || proxy_arguments[0].1 != *port =>
            {
                return Err(DomainError::InconsistentConfiguration(
                    "proxy argument must match the loopback broker binding".to_owned(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Stopped,
    Queued,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl SessionState {
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Stopped, Self::Queued)
                | (Self::Stopped, Self::Starting)
                | (Self::Queued, Self::Starting)
                | (Self::Queued, Self::Stopped)
                | (Self::Starting, Self::Running)
                | (Self::Starting, Self::Stopping)
                | (Self::Starting, Self::Failed)
                | (Self::Running, Self::Stopping)
                | (Self::Running, Self::Failed)
                | (Self::Stopping, Self::Stopped)
                | (Self::Stopping, Self::Failed)
                | (Self::Failed, Self::Stopped)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub profile_id: ProfileId,
    pub state: SessionState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub failure: Option<SessionFailure>,
}

impl Session {
    pub fn new(id: SessionId, profile_id: ProfileId, now_ms: u64) -> Self {
        Self {
            id,
            profile_id,
            state: SessionState::Stopped,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            failure: None,
        }
    }

    pub fn transition(&mut self, next: SessionState, now_ms: u64) -> Result<(), DomainError> {
        if !self.state.can_transition_to(&next) {
            return Err(DomainError::InvalidStateTransition {
                from: self.state.as_str().to_owned(),
                to: next.as_str().to_owned(),
            });
        }
        if !matches!(next, SessionState::Failed) {
            self.failure = None;
        }
        self.state = next;
        self.updated_at_ms = now_ms;
        Ok(())
    }

    pub fn fail(
        &mut self,
        code: impl Into<String>,
        message: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), DomainError> {
        self.transition(SessionState::Failed, now_ms)?;
        self.failure = Some(SessionFailure {
            code: code.into(),
            message: message.into(),
        });
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticScope {
    Isolated,
    Configurable,
    HostShared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
    NotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub key: String,
    pub scope: DiagnosticScope,
    pub status: DiagnosticStatus,
    pub observed: Option<String>,
    pub expected: Option<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticOverall {
    Healthy,
    Attention,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub profile_id: ProfileId,
    pub session_id: Option<SessionId>,
    pub generated_at_ms: u64,
    pub checks: Vec<DiagnosticCheck>,
    pub overall: DiagnosticOverall,
    pub external_endpoints: Vec<String>,
}
