//! Deterministic in-memory adapters for unit tests, prototypes, and contract
//! tests. They are not intended to persist production secrets or profiles.

use crate::ports::*;
use crate::sha256::digest_hex;
use crate::{persona_fingerprint::PersonaFingerprint, stealth_template};
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserBuildSelector, BrowserChannel, BrowserPreferences,
    DiagnosticCheck, DiagnosticOverall, DiagnosticReport, DiagnosticScope, DiagnosticStatus,
    EnvironmentPreset, EnvironmentPresetRef, FingerprintPersona, FingerprintSupport,
    LaunchArgument, LaunchWarning, LocaleConfig, PresetId, ProfileId, ProfileSpec, ProxyId,
    ProxyLaunchBinding, ProxySpec, ResolvedLaunchPlan, Revision, SecretRef, SeedableNoisePersona,
    SessionId, WindowConfig,
};
#[cfg(test)]
use std::fs;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

fn unavailable(component: &'static str) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("{component} state is unavailable"),
    )
}

#[derive(Debug, Default)]
pub struct InMemoryProfileRepository {
    values: RwLock<BTreeMap<ProfileId, ProfileSpec>>,
}

impl InMemoryProfileRepository {
    pub fn profiles(&self) -> PortResult<Vec<ProfileSpec>> {
        Ok(self
            .values
            .read()
            .map_err(|_| unavailable("profile repository"))?
            .values()
            .cloned()
            .collect())
    }
}

impl ProfileRepository for InMemoryProfileRepository {
    fn list(&self, query: &ProfileQuery) -> PortResult<Vec<ProfileSpec>> {
        let search = query
            .search
            .as_ref()
            .map(|value| value.to_ascii_lowercase());
        let values = self
            .values
            .read()
            .map_err(|_| unavailable("profile repository"))?;
        Ok(values
            .values()
            .filter(|profile| query.include_archived || !profile.archived)
            .filter(|profile| {
                search.as_ref().is_none_or(|needle| {
                    profile.name.to_ascii_lowercase().contains(needle)
                        || profile
                            .tags
                            .iter()
                            .any(|tag| tag.to_ascii_lowercase().contains(needle))
                })
            })
            .skip(query.offset)
            .take(query.limit)
            .cloned()
            .collect())
    }

    fn get(&self, id: &ProfileId) -> PortResult<ProfileSpec> {
        self.values
            .read()
            .map_err(|_| unavailable("profile repository"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PortError::not_found("profile"))
    }

    fn create(&self, profile: ProfileSpec) -> PortResult<()> {
        profile
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let mut values = self
            .values
            .write()
            .map_err(|_| unavailable("profile repository"))?;
        if values.contains_key(&profile.id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "profile already exists",
            ));
        }
        values.insert(profile.id.clone(), profile);
        Ok(())
    }

    fn update(&self, profile: ProfileSpec, expected_revision: Revision) -> PortResult<()> {
        profile
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let mut values = self
            .values
            .write()
            .map_err(|_| unavailable("profile repository"))?;
        let current = values
            .get(&profile.id)
            .ok_or_else(|| PortError::not_found("profile"))?;
        if current.revision != expected_revision {
            return Err(PortError::new(
                PortErrorKind::RevisionConflict,
                "profile revision changed",
            ));
        }
        if profile.revision != expected_revision.next() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "replacement revision must increment exactly once",
            ));
        }
        values.insert(profile.id.clone(), profile);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct InMemoryPresetCatalog {
    values: RwLock<BTreeMap<(PresetId, u32), EnvironmentPreset>>,
}

impl InMemoryPresetCatalog {
    pub fn insert(&self, preset: EnvironmentPreset) -> PortResult<()> {
        preset
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.values
            .write()
            .map_err(|_| unavailable("preset catalog"))?
            .insert(
                (preset.reference.id.clone(), preset.reference.version),
                preset,
            );
        Ok(())
    }
}

impl PresetCatalog for InMemoryPresetCatalog {
    fn resolve(&self, reference: &EnvironmentPresetRef) -> PortResult<EnvironmentPreset> {
        self.values
            .read()
            .map_err(|_| unavailable("preset catalog"))?
            .get(&(reference.id.clone(), reference.version))
            .cloned()
            .ok_or_else(|| PortError::not_found("environment preset"))
    }
}

#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    values: RwLock<BTreeMap<SecretRef, SecretValue>>,
}

impl SecretStore for InMemorySecretStore {
    fn put(&self, reference: &SecretRef, value: &SecretValue) -> PortResult<()> {
        self.values
            .write()
            .map_err(|_| unavailable("secret store"))?
            .insert(reference.clone(), value.clone());
        Ok(())
    }

    fn get(&self, reference: &SecretRef) -> PortResult<SecretValue> {
        self.values
            .read()
            .map_err(|_| unavailable("secret store"))?
            .get(reference)
            .cloned()
            .ok_or_else(|| PortError::not_found("secret"))
    }

    fn delete(&self, reference: &SecretRef) -> PortResult<()> {
        self.values
            .write()
            .map_err(|_| unavailable("secret store"))?
            .remove(reference)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("secret"))
    }
}

#[derive(Debug)]
pub struct InMemoryBrowserRuntime {
    builds: RwLock<BTreeMap<BrowserBuildId, BrowserBuild>>,
    running: Mutex<BTreeMap<SessionId, (BrowserProcess, ResolvedLaunchPlan)>>,
    next_process_id: AtomicU32,
    fail_next_launch: AtomicBool,
    exits: Mutex<BTreeMap<SessionId, crate::BrowserExit>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryBrowserRuntime {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            builds: RwLock::new(BTreeMap::new()),
            running: Mutex::new(BTreeMap::new()),
            next_process_id: AtomicU32::new(10_000),
            fail_next_launch: AtomicBool::new(false),
            exits: Mutex::new(BTreeMap::new()),
            clock,
        }
    }

    pub fn insert_build(&self, build: BrowserBuild) -> PortResult<()> {
        build
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.builds
            .write()
            .map_err(|_| unavailable("browser runtime"))?
            .insert(build.id.clone(), build);
        Ok(())
    }

    pub fn fail_next_launch(&self) {
        self.fail_next_launch.store(true, Ordering::SeqCst);
    }

    /// Simulate OS-observed termination while retaining a handle for cleanup.
    pub fn simulate_exit(&self, id: &SessionId, success: bool) -> PortResult<()> {
        self.exits
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .insert(id.clone(), crate::BrowserExit { success });
        Ok(())
    }

    pub fn running_plans(&self) -> PortResult<Vec<ResolvedLaunchPlan>> {
        Ok(self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .values()
            .map(|(_, plan)| plan.clone())
            .collect())
    }
}

impl BrowserRuntime for InMemoryBrowserRuntime {
    fn list_builds(&self) -> PortResult<Vec<BrowserBuild>> {
        let builds = self
            .builds
            .read()
            .map_err(|_| unavailable("browser runtime"))?;
        Ok(builds.values().cloned().collect())
    }

    fn resolve_build(&self, selector: &BrowserBuildSelector) -> PortResult<BrowserBuild> {
        let builds = self
            .builds
            .read()
            .map_err(|_| unavailable("browser runtime"))?;
        match selector {
            BrowserBuildSelector::Pinned(id) => builds
                .get(id)
                .cloned()
                .ok_or_else(|| PortError::not_found("browser build")),
            BrowserBuildSelector::Channel(channel) => builds
                .values()
                .filter(|build| &build.channel == channel)
                .max_by_key(|build| build.installed_at_ms)
                .cloned()
                .ok_or_else(|| PortError::not_found("browser build")),
        }
    }

    fn launch(&self, plan: &ResolvedLaunchPlan) -> PortResult<BrowserProcess> {
        plan.validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        if self.fail_next_launch.swap(false, Ordering::SeqCst) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated browser launch failure",
            ));
        }
        let mut running = self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?;
        if running.contains_key(&plan.session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "browser session is already running",
            ));
        }
        let process = BrowserProcess {
            session_id: plan.session_id.clone(),
            process_id: self.next_process_id.fetch_add(1, Ordering::SeqCst),
            started_at_ms: self.clock.now_ms(),
        };
        running.insert(plan.session_id.clone(), (process.clone(), plan.clone()));
        Ok(process)
    }

    fn stop(&self, session_id: &SessionId) -> PortResult<()> {
        self.running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .remove(session_id)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("running browser session"))
    }

    fn poll_exit(&self, session_id: &SessionId) -> PortResult<Option<crate::BrowserExit>> {
        Ok(self
            .exits
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .get(session_id)
            .copied())
    }

    fn is_running(&self, session_id: &SessionId) -> PortResult<bool> {
        Ok(self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .contains_key(session_id))
    }
}

#[derive(Debug, Default)]
pub struct InMemoryProxyRuntime {
    proxies: RwLock<BTreeMap<ProxyId, ProxySpec>>,
    leases: Mutex<BTreeMap<SessionId, ProxyLease>>,
    next_port: AtomicU32,
}

impl InMemoryProxyRuntime {
    pub fn insert_proxy(&self, proxy: ProxySpec) -> PortResult<()> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.proxies
            .write()
            .map_err(|_| unavailable("proxy runtime"))?
            .insert(proxy.id.clone(), proxy);
        Ok(())
    }

    pub fn active_leases(&self) -> PortResult<Vec<ProxyLease>> {
        Ok(self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?
            .values()
            .cloned()
            .collect())
    }
}

impl ProxyRuntime for InMemoryProxyRuntime {
    fn resolve_proxy(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.proxies
            .read()
            .map_err(|_| unavailable("proxy runtime"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PortError::not_found("proxy"))
    }

    fn prepare_session(&self, session_id: &SessionId, proxy: &ProxySpec) -> PortResult<ProxyLease> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.resolve_proxy(&proxy.id)?;
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?;
        if leases.contains_key(session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "proxy lease already exists for the session",
            ));
        }
        let sequence = self.next_port.fetch_add(1, Ordering::SeqCst);
        let port = 20_000 + (sequence % 30_000) as u16;
        let lease = ProxyLease {
            session_id: session_id.clone(),
            proxy_id: proxy.id.clone(),
            binding: ProxyLaunchBinding::Loopback {
                host: "127.0.0.1".to_owned(),
                port,
            },
            lease_token: format!("lease-{sequence}"),
        };
        leases.insert(session_id.clone(), lease.clone());
        Ok(lease)
    }

    fn release_session(&self, lease: &ProxyLease) -> PortResult<()> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?;
        let current = leases
            .get(&lease.session_id)
            .ok_or_else(|| PortError::not_found("proxy lease"))?;
        if current.lease_token != lease.lease_token {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "proxy lease token does not match",
            ));
        }
        leases.remove(&lease.session_id);
        Ok(())
    }
}

impl ProxyRepository for InMemoryProxyRuntime {
    fn list(&self) -> PortResult<Vec<ProxySpec>> {
        Ok(self
            .proxies
            .read()
            .map_err(|_| unavailable("proxy repository"))?
            .values()
            .cloned()
            .collect())
    }

    fn get(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.resolve_proxy(id)
    }

    fn save(&self, proxy: &ProxySpec) -> PortResult<()> {
        self.insert_proxy(proxy.clone())
    }

    fn delete(&self, id: &ProxyId) -> PortResult<()> {
        self.proxies
            .write()
            .map_err(|_| unavailable("proxy repository"))?
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("proxy"))
    }
}

#[derive(Debug, Default)]
pub struct InMemoryProxyConnectivityTester;

impl ProxyConnectivityTester for InMemoryProxyConnectivityTester {
    fn test(
        &self,
        _proxy: &ProxySpec,
        _credential: Option<&SecretValue>,
    ) -> PortResult<ProxyConnectivityCheck> {
        Ok(ProxyConnectivityCheck {
            reachable: true,
            latency_ms: 1,
            detail: "in-memory endpoint reachable".to_owned(),
            exit_ip: Some("203.0.113.1".to_owned()),
            exit_ip_checked: true,
        })
    }
}

#[derive(Debug, Default)]
pub struct SystemNativeFingerprintRuntime;

impl FingerprintRuntime for SystemNativeFingerprintRuntime {
    fn compile(
        &self,
        persona: &FingerprintPersona,
        _build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        let warnings = persona
            .host_shared_fields()
            .iter()
            .map(|field| LaunchWarning::HostFingerprintShared(field))
            .collect();
        Ok(FingerprintContribution {
            arguments: Vec::new(),
            warnings,
            preferences: None,
        })
    }
}

// ── SeedableNoiseFingerprintRuntime ──────────────────────────────────────────
//
// Materializes a per-profile MV3 stealth extension under `extension_root` and
// produces the typed launch arguments that load it.

#[derive(Debug)]
pub struct SeedableNoiseFingerprintRuntime {
    extension_root: PathBuf,
    profile_id: ProfileId,
}

/// Creates the production MAIN-world script for a bounded local capability
/// probe. Sentinels intentionally differ from normal physical devices; this
/// never changes a persisted profile and never claims a real device identity.
pub fn materialize_fingerprint_probe(
    extension_root: &std::path::Path,
    profile_id: &ProfileId,
    runtime_version: &str,
) -> PortResult<PathBuf> {
    let persona = SeedableNoisePersona {
        seed: "brows-local-fingerprint-probe-v1".to_owned(),
        platform: brows_domain::PlatformKind::Windows,
        hardware_concurrency: 3,
        device_memory: brows_domain::DeviceMemoryGb::Half,
        touch_points: 0,
        screen: brows_domain::ScreenConfig {
            width: 1777,
            height: 1111,
            color_depth: 24,
            device_scale_factor_hundredths: 100,
        },
        webgl: brows_domain::WebGLProfile {
            vendor: "Google Inc. (Intel)".to_owned(),
            unmasked_renderer:
                "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)"
                    .to_owned(),
        },
        spoof_canvas: true,
        spoof_audio: true,
    };
    let fp = PersonaFingerprint::derive(&persona, runtime_version)
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
    let locale = LocaleConfig {
        accept_languages: vec!["pl-PL".to_owned(), "pl".to_owned()],
        timezone: None,
    };
    SeedableNoiseFingerprintRuntime::new(extension_root, profile_id.clone())?
        .materialize_extension(profile_id, &persona, &fp, &locale)
}

impl SeedableNoiseFingerprintRuntime {
    pub fn new(extension_root: impl Into<PathBuf>, profile_id: ProfileId) -> PortResult<Self> {
        let extension_root = extension_root.into();
        if extension_root.as_os_str().is_empty() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "extension root cannot be empty",
            ));
        }
        Ok(Self {
            extension_root,
            profile_id,
        })
    }

    /// Shared production constructor. Profile identity is supplied through
    /// `compile_for_profile`, so the placeholder is used only by legacy direct
    /// calls to `compile`.
    pub fn new_shared(extension_root: impl Into<PathBuf>) -> PortResult<Self> {
        let profile_id = ProfileId::new("runtime-shared")
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        Self::new(extension_root, profile_id)
    }

    fn materialize_extension(
        &self,
        profile_id: &ProfileId,
        persona: &SeedableNoisePersona,
        fp: &PersonaFingerprint,
        locale: &LocaleConfig,
    ) -> PortResult<PathBuf> {
        let persona_json = serde_json::json!({
            "seed": persona.seed,
            "platform": persona.platform_string_for_js(),
            "hardware_concurrency": fp.hardware_concurrency,
            "device_memory": fp.device_memory_number(),
            "languages": locale.accept_languages,
            "touch_points": fp.touch_points,
            "screen_width": persona.screen.width,
            "screen_height": persona.screen.height,
            "color_depth": persona.screen.color_depth,
            "device_scale_factor": persona.screen.device_scale_factor_hundredths as f64 / 100.0,
            "webgl_vendor": fp.webgl_vendor,
            "webgl_renderer": fp.webgl_renderer,
            "spoof_canvas": persona.spoof_canvas,
            "spoof_audio": persona.spoof_audio,
            // WebRTC is enforced by the typed Chromium argument when a proxy
            // is configured.  The extension must not force relay-only mode for
            // direct profiles, otherwise local network use is unexpectedly
            // disabled.
            "webrtc_policy": "none",
        })
        .to_string();
        let content = stealth_template::render_content(&persona_json);
        let manifest = stealth_template::render_manifest(profile_id.as_str());
        let background = stealth_template::render_background();
        crate::fingerprint_artifact::publish_extension(
            &self.extension_root,
            profile_id,
            &manifest,
            &content,
            &background,
            &fp.user_agent,
        )
    }
}

/// Production adapter that supports both the original host-native persona and
/// the opt-in SeedableNoise implementation behind one application port.
#[derive(Debug)]
pub struct FingerprintRuntimeRouter {
    seedable: SeedableNoiseFingerprintRuntime,
}

impl FingerprintRuntimeRouter {
    pub fn new(extension_root: impl Into<PathBuf>) -> PortResult<Self> {
        Ok(Self {
            seedable: SeedableNoiseFingerprintRuntime::new_shared(extension_root)?,
        })
    }
}

impl FingerprintRuntime for FingerprintRuntimeRouter {
    fn compile_profile(
        &self,
        profile: &ProfileSpec,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        match &profile.persona {
            FingerprintPersona::SystemNative(_) => {
                SystemNativeFingerprintRuntime.compile_profile(profile, build)
            }
            FingerprintPersona::SeedableNoise(_) => self.seedable.compile_profile(profile, build),
        }
    }

    fn compile(
        &self,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        match persona {
            FingerprintPersona::SystemNative(_) => {
                SystemNativeFingerprintRuntime.compile(persona, build)
            }
            FingerprintPersona::SeedableNoise(_) => self.seedable.compile(persona, build),
        }
    }

    fn compile_for_profile(
        &self,
        profile_id: &ProfileId,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        match persona {
            FingerprintPersona::SystemNative(_) => {
                SystemNativeFingerprintRuntime.compile_for_profile(profile_id, persona, build)
            }
            FingerprintPersona::SeedableNoise(_) => self
                .seedable
                .compile_for_profile(profile_id, persona, build),
        }
    }
}

impl FingerprintRuntime for SeedableNoiseFingerprintRuntime {
    fn compile_profile(
        &self,
        profile: &ProfileSpec,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        self.compile_inner(
            &profile.id,
            &profile.persona,
            build,
            &profile.locale,
            &profile.window,
        )
    }

    fn compile(
        &self,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        self.compile_inner(
            &self.profile_id,
            persona,
            build,
            &LocaleConfig::default(),
            &WindowConfig::default(),
        )
    }

    fn compile_for_profile(
        &self,
        profile_id: &ProfileId,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        self.compile_inner(
            profile_id,
            persona,
            build,
            &LocaleConfig::default(),
            &WindowConfig::default(),
        )
    }
}

impl SeedableNoiseFingerprintRuntime {
    fn compile_inner(
        &self,
        profile_id: &ProfileId,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
        locale: &LocaleConfig,
        window: &WindowConfig,
    ) -> PortResult<FingerprintContribution> {
        let FingerprintPersona::SeedableNoise(p) = persona else {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "SeedableNoiseFingerprintRuntime requires a SeedableNoise persona",
            ));
        };
        if build.capabilities.fingerprint_support != FingerprintSupport::Supported {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "所选运行时尚未通过指纹扩展能力检测，不能启动 SeedableNoise",
            ));
        }
        locale
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        window
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        if locale.timezone.is_some() {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "timezone override is not implemented",
            ));
        }
        let fp = PersonaFingerprint::derive(p, &build.version)
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        // Keep the UA major aligned with the concrete Chromium binary. A
        // random major from a global pool would be trivially inconsistent with
        // the runtime selected by the profile.
        let ext_dir = self.materialize_extension(profile_id, p, &fp, locale)?;

        let mut warnings = Vec::new();
        let arguments = vec![
            LaunchArgument::UserAgentOverride(fp.user_agent),
            LaunchArgument::ForceDeviceScaleFactor(p.screen.device_scale_factor_hundredths),
            LaunchArgument::WindowSize {
                width: window.width,
                height: window.height,
            },
            LaunchArgument::DisableBlinkFeatures(vec!["AutomationControlled".to_owned()]),
            LaunchArgument::LoadExtension(ext_dir),
        ];

        // Declared-only fields: be honest about what we did not actually change.
        if p.spoof_canvas {
            warnings.push(LaunchWarning::DeclaredFingerprintOnly {
                field: "canvas".to_owned(),
                reason: "JS read/export perturbation only; not native rendering or worker coverage"
                    .to_owned(),
            });
        }
        if p.spoof_audio {
            warnings.push(LaunchWarning::DeclaredFingerprintOnly {
                field: "audio".to_owned(),
                reason: "offline rendered-buffer perturbation only; audio devices and sample rate remain native"
                    .to_owned(),
            });
        }

        let preferences = BrowserPreferences {
            accept_languages: Vec::new(),
            startup_urls: Vec::new(),
            permissions_prompt: true,
            webgl_enabled: Some(true),
            hardware_acceleration: Some(true),
            notifications_setting: Some(1),
        };

        Ok(FingerprintContribution {
            arguments,
            warnings,
            preferences: Some(preferences),
        })
    }
}

#[cfg(test)]
mod seedable_noise_tests {
    use super::*;
    use brows_domain::{
        BrowserBuild, BrowserBuildId, BrowserChannel, DeviceMemoryGb, FingerprintPersona,
        PlatformKind, ProfileId, RuntimeTrust, ScreenConfig, SeedableNoisePersona, WebGLProfile,
    };

    fn verified_build() -> BrowserBuild {
        BrowserBuild {
            id: BrowserBuildId::new("stable-150").unwrap(),
            provider: "google".to_owned(),
            version: "150.0.0.0".to_owned(),
            channel: BrowserChannel::Stable,
            executable_path: PathBuf::from("C:/chrome.exe"),
            sha256: "0".repeat(64),
            publisher: "Google LLC".to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: brows_domain::BrowserCapabilities {
                fingerprint_support: FingerprintSupport::Supported,
                ..Default::default()
            },
            installed_at_ms: 1,
        }
    }

    fn sample_persona(seed: &str) -> SeedableNoisePersona {
        SeedableNoisePersona {
            seed: seed.to_owned(),
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
            spoof_audio: true,
        }
    }

    #[test]
    fn seedable_noise_compile_is_deterministic_for_same_seed() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("p").unwrap(),
        )
        .unwrap();
        let p = FingerprintPersona::SeedableNoise(sample_persona("stable-seed"));
        let a = runtime
            .compile(&p, &verified_build())
            .expect("compile should succeed");
        let b = runtime
            .compile(&p, &verified_build())
            .expect("compile should succeed");
        // Arguments must be identical for the same seed.
        assert_eq!(a.arguments, b.arguments);
        // UA override must be present and stable.
        let ua_a = a
            .arguments
            .iter()
            .find_map(|arg| match arg {
                LaunchArgument::UserAgentOverride(s) => Some(s.clone()),
                _ => None,
            })
            .expect("UA override argument present");
        let ua_b = b
            .arguments
            .iter()
            .find_map(|arg| match arg {
                LaunchArgument::UserAgentOverride(s) => Some(s.clone()),
                _ => None,
            })
            .expect("UA override argument present");
        assert_eq!(ua_a, ua_b);
    }

    #[test]
    fn seedable_noise_compile_produces_different_ua_for_different_seeds() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("p").unwrap(),
        )
        .unwrap();
        let p_a = FingerprintPersona::SeedableNoise(sample_persona("seed-alpha"));
        let p_b = FingerprintPersona::SeedableNoise(sample_persona("seed-beta"));
        let a = runtime.compile(&p_a, &verified_build()).unwrap();
        let b = runtime.compile(&p_b, &verified_build()).unwrap();
        let ua_a = a.arguments.iter().find_map(|arg| match arg {
            LaunchArgument::UserAgentOverride(s) => Some(s.clone()),
            _ => None,
        });
        let ua_b = b.arguments.iter().find_map(|arg| match arg {
            LaunchArgument::UserAgentOverride(s) => Some(s.clone()),
            _ => None,
        });
        // The concrete browser major is intentionally shared so the override
        // cannot contradict the selected runtime. Other derived artifacts (the
        // extension path and WebGL values) still vary with the seed.
        assert!(ua_a.is_some() && ua_b.is_some());
        assert_ne!(a.arguments, b.arguments);
    }

    #[test]
    fn seedable_noise_compile_rejects_wrong_persona_kind() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("p").unwrap(),
        )
        .unwrap();
        let wrong = FingerprintPersona::SystemNative(brows_domain::SystemNativePersona::default());
        let err = runtime.compile(&wrong, &verified_build()).unwrap_err();
        assert_eq!(err.kind, PortErrorKind::InvalidData);
    }

    #[test]
    fn seedable_noise_compile_emits_load_extension_argument() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("p").unwrap(),
        )
        .unwrap();
        let p = FingerprintPersona::SeedableNoise(sample_persona("ext-test"));
        let out = runtime.compile(&p, &verified_build()).unwrap();
        let has_load_extension = out
            .arguments
            .iter()
            .any(|a| matches!(a, LaunchArgument::LoadExtension(_)));
        assert!(has_load_extension, "must emit LoadExtension argument");
        let has_disable_blink = out
            .arguments
            .iter()
            .any(|a| matches!(a, LaunchArgument::DisableBlinkFeatures(features) if features.contains(&"AutomationControlled".to_owned())));
        assert!(
            has_disable_blink,
            "must emit DisableBlinkFeatures for AutomationControlled"
        );
    }

    #[test]
    fn seedable_noise_compile_emits_declared_only_warnings_when_canvas_audio_spoofed() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("p").unwrap(),
        )
        .unwrap();
        let p = FingerprintPersona::SeedableNoise(sample_persona("warn-test"));
        let out = runtime.compile(&p, &verified_build()).unwrap();
        let canvas_warn = out.warnings.iter().any(|w| {
            matches!(w,
                LaunchWarning::DeclaredFingerprintOnly { field, .. } if field == "canvas"
            )
        });
        let audio_warn = out.warnings.iter().any(|w| {
            matches!(w,
                LaunchWarning::DeclaredFingerprintOnly { field, .. } if field == "audio"
            )
        });
        assert!(canvas_warn, "must emit declared-only warning for canvas");
        assert!(audio_warn, "must emit declared-only warning for audio");
    }

    #[test]
    fn seedable_noise_extension_directory_is_isolated_per_profile() {
        let root = std::env::temp_dir();
        let runtime =
            SeedableNoiseFingerprintRuntime::new(&root, ProfileId::new("p").unwrap()).unwrap();
        let p_a = FingerprintPersona::SeedableNoise(sample_persona("isolation-A"));
        let p_b = FingerprintPersona::SeedableNoise(sample_persona("isolation-B"));
        let ext_a = runtime
            .compile(&p_a, &verified_build())
            .unwrap()
            .arguments
            .into_iter()
            .find_map(|a| match a {
                LaunchArgument::LoadExtension(p) => Some(p),
                _ => None,
            })
            .expect("LoadExtension");
        let ext_b = runtime
            .compile(&p_b, &verified_build())
            .unwrap()
            .arguments
            .into_iter()
            .find_map(|a| match a {
                LaunchArgument::LoadExtension(p) => Some(p),
                _ => None,
            })
            .expect("LoadExtension");
        assert_ne!(
            ext_a, ext_b,
            "extension directories must be different per seed"
        );
    }

    #[test]
    fn seedable_noise_template_render_includes_persona_json() {
        use crate::stealth_template;
        let rendered = stealth_template::render_content(r#"{"seed":"hello","platform":"windows"}"#);
        assert!(rendered.contains("\"seed\":\"hello\""));
        assert!(rendered.contains("platform"));
        assert!(!rendered.contains("{{PERSONA_JSON}}"));
    }

    #[test]
    fn profile_configuration_reaches_extension_and_window_arguments() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir().join("brows-fingerprint-test"),
            ProfileId::new("configuration").unwrap(),
        )
        .unwrap();
        let persona = FingerprintPersona::SeedableNoise(sample_persona("shared-config"));
        let locale = LocaleConfig {
            accept_languages: vec!["zh-CN".to_owned(), "zh".to_owned()],
            timezone: None,
        };
        let window = WindowConfig {
            width: 1280,
            height: 800,
            start_maximized: false,
        };
        let out = runtime
            .compile_inner(
                &runtime.profile_id,
                &persona,
                &verified_build(),
                &locale,
                &window,
            )
            .unwrap();
        assert!(out.arguments.contains(&LaunchArgument::WindowSize {
            width: 1280,
            height: 800
        }));
        assert!(!out.arguments.contains(&LaunchArgument::WindowSize {
            width: 1920,
            height: 1080
        }));
        let dir = out
            .arguments
            .iter()
            .find_map(|arg| match arg {
                LaunchArgument::LoadExtension(dir) => Some(dir),
                _ => None,
            })
            .unwrap();
        let content = fs::read_to_string(dir.join("content.js")).unwrap();
        assert!(content.contains(r#""device_memory":8.0"#));
        assert!(!content.contains(r#""device_memory":"8""#));
        assert!(content.contains(r#""languages":["zh-CN","zh"]"#));
    }

    #[test]
    fn changing_configuration_or_runtime_never_overwrites_previous_extension() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir().join("brows-fingerprint-test"),
            ProfileId::new("immutable-config").unwrap(),
        )
        .unwrap();
        let mut persona = FingerprintPersona::SeedableNoise(sample_persona("unchanged-seed"));
        let extension = |out: FingerprintContribution| {
            out.arguments
                .into_iter()
                .find_map(|arg| match arg {
                    LaunchArgument::LoadExtension(dir) => Some(dir),
                    _ => None,
                })
                .unwrap()
        };
        let first = extension(runtime.compile(&persona, &verified_build()).unwrap());
        let first_content = fs::read_to_string(first.join("content.js")).unwrap();
        if let FingerprintPersona::SeedableNoise(ref mut p) = persona {
            p.hardware_concurrency = 4;
        }
        let changed = extension(runtime.compile(&persona, &verified_build()).unwrap());
        assert_ne!(first, changed);
        assert_eq!(
            first_content,
            fs::read_to_string(first.join("content.js")).unwrap()
        );
        let mut build = verified_build();
        build.version = "151.1.2.3".to_owned();
        let newer = extension(runtime.compile(&persona, &build).unwrap());
        assert_ne!(changed, newer);
        assert_eq!(newer, extension(runtime.compile(&persona, &build).unwrap()));
        let locale = LocaleConfig {
            accept_languages: vec!["ja-JP".to_owned()],
            timezone: None,
        };
        let localized = extension(
            runtime
                .compile_inner(
                    &runtime.profile_id,
                    &persona,
                    &build,
                    &locale,
                    &WindowConfig::default(),
                )
                .unwrap(),
        );
        assert_ne!(localized, newer);
    }

    #[test]
    fn unsupported_timezone_and_invalid_version_fail_explicitly() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("validation").unwrap(),
        )
        .unwrap();
        let persona = FingerprintPersona::SeedableNoise(sample_persona("valid"));
        let locale = LocaleConfig {
            accept_languages: vec!["en".to_owned()],
            timezone: Some("Europe/London".to_owned()),
        };
        assert_eq!(
            runtime
                .compile_inner(
                    &runtime.profile_id,
                    &persona,
                    &verified_build(),
                    &locale,
                    &WindowConfig::default()
                )
                .unwrap_err()
                .kind,
            PortErrorKind::CapabilityUnsupported
        );
        let mut build = verified_build();
        build.version = "installed".to_owned();
        assert_eq!(
            runtime.compile(&persona, &build).unwrap_err().kind,
            PortErrorKind::InvalidData
        );
    }

    #[test]
    fn seedable_requires_observed_runtime_support_not_publisher_trust() {
        let runtime = SeedableNoiseFingerprintRuntime::new(
            std::env::temp_dir(),
            ProfileId::new("capability-gate").unwrap(),
        )
        .unwrap();
        let persona = FingerprintPersona::SeedableNoise(sample_persona("gate"));
        for support in [
            FingerprintSupport::Unverified,
            FingerprintSupport::Unsupported,
        ] {
            let mut build = verified_build();
            build.capabilities.fingerprint_support = support;
            assert_eq!(build.trust, RuntimeTrust::Verified);
            assert_eq!(
                runtime.compile(&persona, &build).unwrap_err().kind,
                PortErrorKind::CapabilityUnsupported
            );
        }
    }

    #[test]
    fn materialization_rejects_corrupted_artifacts_without_overwriting_them() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "brows-artifact-integrity-{}-{unique}",
            std::process::id()
        ));
        let runtime =
            SeedableNoiseFingerprintRuntime::new(&root, ProfileId::new("integrity").unwrap())
                .unwrap();
        let persona = FingerprintPersona::SeedableNoise(sample_persona("integrity"));
        let out = runtime.compile(&persona, &verified_build()).unwrap();
        let dir = out
            .arguments
            .iter()
            .find_map(|arg| match arg {
                LaunchArgument::LoadExtension(dir) => Some(dir),
                _ => None,
            })
            .unwrap();
        let damaged = dir.join("content.js");
        fs::write(&damaged, b"damaged").unwrap();
        assert_eq!(
            runtime
                .compile(&persona, &verified_build())
                .unwrap_err()
                .kind,
            PortErrorKind::InvalidData
        );
        assert_eq!(fs::read(&damaged).unwrap(), b"damaged");
        fs::remove_dir_all(root).unwrap();
    }
}

// Small helpers on the domain type — used only by SeedableNoiseFingerprintRuntime.
trait SeedableNoiseExt {
    fn platform_string_for_js(&self) -> &'static str;
}

impl SeedableNoiseExt for SeedableNoisePersona {
    fn platform_string_for_js(&self) -> &'static str {
        match self.platform {
            brows_domain::PlatformKind::Windows => "windows",
            brows_domain::PlatformKind::MacOs => "mac",
            brows_domain::PlatformKind::Linux => "linux",
        }
    }
}

#[derive(Debug)]
pub struct InMemoryDiagnosticRunner {
    reports: Mutex<Vec<DiagnosticReport>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryDiagnosticRunner {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            reports: Mutex::new(Vec::new()),
            clock,
        }
    }

    pub fn reports(&self) -> PortResult<Vec<DiagnosticReport>> {
        Ok(self
            .reports
            .lock()
            .map_err(|_| unavailable("diagnostic runner"))?
            .clone())
    }
}

impl DiagnosticRunner for InMemoryDiagnosticRunner {
    fn run(&self, request: &DiagnosticRequest) -> PortResult<DiagnosticReport> {
        let mut checks = vec![DiagnosticCheck {
            key: "profile_storage".to_owned(),
            scope: DiagnosticScope::Isolated,
            status: DiagnosticStatus::Pass,
            observed: Some("dedicated user data directory".to_owned()),
            expected: Some("isolated".to_owned()),
            explanation: "site storage is scoped to this profile".to_owned(),
        }];
        checks.extend(
            request
                .profile
                .persona
                .host_shared_fields()
                .iter()
                .map(|field| DiagnosticCheck {
                    key: (*field).to_owned(),
                    scope: DiagnosticScope::HostShared,
                    status: DiagnosticStatus::Warn,
                    observed: Some("system native".to_owned()),
                    expected: Some("system native".to_owned()),
                    explanation: "v0.1 intentionally shares this host property".to_owned(),
                }),
        );
        let report = DiagnosticReport {
            profile_id: request.profile.id.clone(),
            session_id: request.session_id.clone(),
            generated_at_ms: self.clock.now_ms(),
            checks,
            overall: DiagnosticOverall::Attention,
            external_endpoints: if request.include_external_checks {
                vec!["https://diagnostic.invalid/explicit-test".to_owned()]
            } else {
                Vec::new()
            },
        };
        self.reports
            .lock()
            .map_err(|_| unavailable("diagnostic runner"))?
            .push(report.clone());
        Ok(report)
    }
}

#[derive(Debug, Clone)]
struct StoredBackup {
    profile: ProfileSpec,
    password: SecretValue,
    includes_proxy_credential: bool,
}

#[derive(Debug, Default)]
pub struct InMemoryBackupService {
    backups: RwLock<BTreeMap<String, StoredBackup>>,
}

impl BackupService for InMemoryBackupService {
    fn inspect(&self, source_name: &str) -> PortResult<BackupInspection> {
        let backups = self
            .backups
            .read()
            .map_err(|_| unavailable("backup service"))?;
        let backup = backups
            .get(source_name)
            .ok_or_else(|| PortError::not_found("backup"))?;
        Ok(BackupInspection {
            format_version: 1,
            profile_name: backup.profile.name.clone(),
            includes_proxy_credential: backup.includes_proxy_credential,
        })
    }

    fn export(&self, spec: &BackupExportSpec) -> PortResult<()> {
        if spec.destination_name.trim().is_empty() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "backup destination name cannot be empty",
            ));
        }
        let mut backups = self
            .backups
            .write()
            .map_err(|_| unavailable("backup service"))?;
        if backups.contains_key(&spec.destination_name) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "backup destination already exists",
            ));
        }
        backups.insert(
            spec.destination_name.clone(),
            StoredBackup {
                profile: spec.profile.clone(),
                password: spec.password.clone(),
                includes_proxy_credential: spec.include_proxy_credential,
            },
        );
        Ok(())
    }

    fn import(&self, spec: &BackupImportSpec) -> PortResult<BackupImportPayload> {
        let backups = self
            .backups
            .read()
            .map_err(|_| unavailable("backup service"))?;
        let backup = backups
            .get(&spec.source_name)
            .ok_or_else(|| PortError::not_found("backup"))?;
        if backup.password != spec.password {
            return Err(PortError::new(
                PortErrorKind::WrongPassword,
                "backup password is incorrect",
            ));
        }
        Ok(BackupImportPayload {
            profile: backup.profile.clone(),
            restored_data_dir: PathBuf::from("memory-backups")
                .join(digest_hex(spec.source_name.as_bytes())),
        })
    }
}

#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl InMemoryAuditSink {
    pub fn events(&self) -> PortResult<Vec<AuditEvent>> {
        Ok(self
            .events
            .lock()
            .map_err(|_| unavailable("audit sink"))?
            .clone())
    }
}

impl AuditSink for InMemoryAuditSink {
    fn record(&self, event: AuditEvent) -> PortResult<()> {
        self.events
            .lock()
            .map_err(|_| unavailable("audit sink"))?
            .push(event);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ManualClock {
    now_ms: AtomicU64,
}

impl ManualClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    pub fn set(&self, now_ms: u64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: u64) -> u64 {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst) + delta_ms
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Default)]
pub struct SequentialIdGenerator {
    profile: AtomicU64,
    proxy: AtomicU64,
    session: AtomicU64,
    secret: AtomicU64,
}

impl IdGenerator for SequentialIdGenerator {
    fn next_profile_id(&self) -> PortResult<ProfileId> {
        ProfileId::new(format!(
            "profile-{}",
            self.profile.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_proxy_id(&self) -> PortResult<ProxyId> {
        let value = self.proxy.fetch_add(1, Ordering::SeqCst) + 1;
        ProxyId::new(format!("proxy-{value}"))
            .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_session_id(&self) -> PortResult<SessionId> {
        SessionId::new(format!(
            "session-{}",
            self.session.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_secret_ref(&self) -> PortResult<SecretRef> {
        SecretRef::new(format!(
            "secret-{}",
            self.secret.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }
}

/// Convenience bootstrap for the single v0.1 preset.
pub fn system_native_preset() -> EnvironmentPreset {
    use brows_domain::EnvironmentPresetKind;
    EnvironmentPreset {
        reference: EnvironmentPresetRef::new(
            PresetId::new("system-native").expect("static preset id is valid"),
            1,
        )
        .expect("static preset version is valid"),
        name: "System Native".to_owned(),
        kind: EnvironmentPresetKind::SystemNative,
        description: "Use truthful host-level browser properties".to_owned(),
    }
}

/// Convenience helper for resolving the newest stable build in adapter tests.
pub fn stable_selector() -> BrowserBuildSelector {
    BrowserBuildSelector::Channel(BrowserChannel::Stable)
}

#[derive(Debug, Default)]
pub struct InMemorySettingsRepository(pub Mutex<brows_domain::ManagerSettings>);

impl crate::SettingsRepository for InMemorySettingsRepository {
    fn load(&self) -> PortResult<brows_domain::ManagerSettings> {
        Ok(self.0.lock().map_err(|_| unavailable("settings"))?.clone())
    }
    fn save(&self, settings: &brows_domain::ManagerSettings) -> PortResult<()> {
        settings
            .validate()
            .map_err(|_| PortError::new(PortErrorKind::InvalidData, "invalid settings"))?;
        *self.0.lock().map_err(|_| unavailable("settings"))? = settings.clone();
        Ok(())
    }
}

mod restore_tasks;
pub use restore_tasks::InMemoryRestoreTaskRepository;
