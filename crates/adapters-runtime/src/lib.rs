#![deny(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Managed Chromium runtime adapter.
//!
//! The adapter accepts only a validated [`ResolvedLaunchPlan`]. It never
//! invokes a shell, never accepts arbitrary flags, and verifies the selected
//! executable against the registered SHA-256 digest before every launch.

use brows_application::{
    BrowserProcess, BrowserRuntime, FingerprintProbeResult, PortError, PortErrorKind, PortResult,
};
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserBuildSelector, BrowserChannel, FingerprintSupport,
    LaunchArgument, ResolvedLaunchPlan, RuntimeTrust, SessionId,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

mod authenticode;
mod bundle;
mod official;
mod system;
mod version;
pub use official::{OfficialRuntimeInstaller, RuntimeInstallStatus};
pub use system::{discover_system_browsers, SystemBrowserInstallation, SystemBrowserProvider};
mod fingerprint_probe;
mod measurement;
pub use measurement::LocalFingerprintMeasurement;
mod process;
use process::BrowserChild;

pub use authenticode::verify_authenticode_publisher;

const MAX_PREFERENCES_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Default)]
struct BuildRegistry {
    by_id: BTreeMap<BrowserBuildId, BrowserBuild>,
    bundles: BTreeMap<BrowserBuildId, Arc<bundle::BundleIntegrity>>,
    active_channels: BTreeMap<&'static str, BrowserBuildId>,
}

/// Owns verified browser builds and all child processes launched from them.
#[derive(Debug)]
pub struct ManagedChromiumRuntime {
    runtime_roots: Vec<PathBuf>,
    profile_root: PathBuf,
    builds: RwLock<BuildRegistry>,
    processes: Mutex<HashMap<SessionId, BrowserChild>>,
    probing: fingerprint_probe::ProbeGate,
}

impl ManagedChromiumRuntime {
    pub fn new(
        runtime_root: impl Into<PathBuf>,
        profile_root: impl Into<PathBuf>,
    ) -> PortResult<Self> {
        Self::with_roots(vec![runtime_root.into()], profile_root)
    }

    pub fn with_roots(roots: Vec<PathBuf>, profile_root: impl Into<PathBuf>) -> PortResult<Self> {
        if roots.is_empty() || roots.len() > 8 {
            return Err(invalid_data(
                "expected between one and eight controlled runtime roots",
            ));
        }
        let runtime_roots = roots
            .into_iter()
            .map(|root| create_and_canonicalize(root, "runtime root"))
            .collect::<PortResult<Vec<_>>>()?;
        let profile_root = create_and_canonicalize(profile_root.into(), "profile root")?;
        Ok(Self {
            runtime_roots,
            profile_root,
            builds: RwLock::new(BuildRegistry::default()),
            processes: Mutex::new(HashMap::new()),
            probing: fingerprint_probe::ProbeGate::default(),
        })
    }

    /// Registers a build only after checking its trust, location and digest.
    pub fn register_build(&self, build: BrowserBuild, activate: bool) -> PortResult<()> {
        self.register_build_checked(build, activate, None)
    }

    fn register_build_checked(
        &self,
        mut build: BrowserBuild,
        activate: bool,
        bundle: Option<bundle::BundleIntegrity>,
    ) -> PortResult<()> {
        build
            .validate()
            .map_err(|error| invalid_data(error.to_string()))?;
        if build.trust != RuntimeTrust::Verified {
            return Err(PortError::new(
                PortErrorKind::RuntimeUntrusted,
                "only verified browser builds can be registered",
            ));
        }

        let executable = canonical_regular_file(&build.executable_path, "browser executable")?;
        self.ensure_runtime_path(&executable)?;
        verify_digest(&executable, &build.sha256)?;
        build.executable_path = executable;
        // Publisher admission is not proof of extension support. Only the
        // adapter's own bounded observation may set Supported.
        build.capabilities.fingerprint_support = FingerprintSupport::Unverified;

        if let Some(integrity) = &bundle {
            integrity.verify()?;
        }
        self.insert_registered_build(build, activate, bundle)
    }

    /// Records the current digest of an executable already trusted by the
    /// composition root, then uses the normal fail-closed launch path.
    ///
    /// This is useful for a system Chrome installation whose digest changes
    /// after an automatic update. The caller must establish publisher trust
    /// before invoking this helper. Any later file change is rejected until
    /// the application is restarted and the build is admitted again.
    pub fn register_build_with_current_digest(
        &self,
        mut build: BrowserBuild,
        activate: bool,
    ) -> PortResult<()> {
        if build.trust != RuntimeTrust::Verified {
            return Err(PortError::new(
                PortErrorKind::RuntimeUntrusted,
                "only verified browser builds can be registered",
            ));
        }

        let executable = canonical_regular_file(&build.executable_path, "browser executable")?;
        self.ensure_runtime_path(&executable)?;
        build.sha256 = digest_file(&executable)?;
        build.executable_path = executable;
        self.register_build(build, activate)
    }

    fn ensure_runtime_path(&self, executable: &Path) -> PortResult<()> {
        if self
            .runtime_roots
            .iter()
            .any(|root| executable.starts_with(root) && executable != root)
        {
            Ok(())
        } else {
            Err(invalid_data(
                "browser executable is outside controlled runtime roots",
            ))
        }
    }

    fn verify_build_integrity(&self, build: &BrowserBuild) -> PortResult<()> {
        self.ensure_runtime_path(&canonical_regular_file(
            &build.executable_path,
            "browser executable",
        )?)?;
        let bundle = self
            .builds
            .read()
            .map_err(|_| internal("browser build registry lock is poisoned"))?
            .bundles
            .get(&build.id)
            .cloned();
        if let Some(bundle) = bundle {
            bundle.verify()?;
        }
        verify_digest(&build.executable_path, &build.sha256)
    }

    fn insert_registered_build(
        &self,
        build: BrowserBuild,
        activate: bool,
        bundle: Option<bundle::BundleIntegrity>,
    ) -> PortResult<()> {
        let mut registry = self
            .builds
            .write()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        if registry.by_id.contains_key(&build.id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "browser build is already registered",
            ));
        }
        if activate {
            registry
                .active_channels
                .insert(channel_key(build.channel), build.id.clone());
        }
        if let Some(bundle) = bundle {
            registry.bundles.insert(build.id.clone(), Arc::new(bundle));
        }
        registry.by_id.insert(build.id.clone(), build);
        Ok(())
    }

    pub fn activate(&self, build_id: &BrowserBuildId) -> PortResult<()> {
        let mut registry = self
            .builds
            .write()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        let channel = registry
            .by_id
            .get(build_id)
            .map(|build| build.channel)
            .ok_or_else(|| PortError::not_found("browser build"))?;
        registry
            .active_channels
            .insert(channel_key(channel), build_id.clone());
        Ok(())
    }

    pub fn installed_builds(&self) -> PortResult<Vec<BrowserBuild>> {
        let registry = self
            .builds
            .read()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        Ok(registry.by_id.values().cloned().collect())
    }

    fn set_fingerprint_support(
        &self,
        build_id: &BrowserBuildId,
        support: FingerprintSupport,
    ) -> PortResult<BrowserBuild> {
        let mut registry = self
            .builds
            .write()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        let build = registry
            .by_id
            .get_mut(build_id)
            .ok_or_else(|| PortError::not_found("browser build"))?;
        build.capabilities.fingerprint_support = support;
        Ok(build.clone())
    }

    fn verified_build_for_plan(&self, plan: &ResolvedLaunchPlan) -> PortResult<BrowserBuild> {
        let executable = canonical_regular_file(&plan.executable_path, "browser executable")?;

        let registry = self
            .builds
            .read()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        let build = registry
            .by_id
            .values()
            .find(|build| build.executable_path == executable)
            .cloned()
            .ok_or_else(|| {
                PortError::new(
                    PortErrorKind::RuntimeUntrusted,
                    "launch plan references an unregistered browser executable",
                )
            })?;
        if build.trust != RuntimeTrust::Verified {
            return Err(PortError::new(
                PortErrorKind::RuntimeUntrusted,
                "browser runtime trust was revoked",
            ));
        }
        drop(registry);
        self.verify_build_integrity(&build)?;
        Ok(build)
    }

    fn prepare_profile(&self, plan: &ResolvedLaunchPlan) -> PortResult<PathBuf> {
        Self::verify_plan_extension(plan)?;
        let user_data_dir = create_confined_directory(&self.profile_root, &plan.user_data_dir)?;
        write_preferences(&user_data_dir, plan)?;
        Ok(user_data_dir)
    }

    fn verify_plan_extension(plan: &ResolvedLaunchPlan) -> PortResult<()> {
        let extensions = plan
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                LaunchArgument::LoadExtension(path) => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        if extensions.is_empty() {
            return Ok(());
        }
        let agents = plan
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                LaunchArgument::UserAgentOverride(ua) => Some(ua),
                _ => None,
            })
            .collect::<Vec<_>>();
        if extensions.len() != 1 || agents.len() != 1 {
            return Err(invalid_data(
                "fingerprint plan requires exactly one extension and user agent",
            ));
        }
        brows_application::verify_fingerprint_extension(&plan.profile_id, extensions[0], agents[0])
    }

    fn render_arguments(
        plan: &ResolvedLaunchPlan,
        user_data_dir: &Path,
    ) -> PortResult<Vec<OsString>> {
        let mut rendered = vec![
            // Keep canonical paths for confinement/integrity, but use an
            // equivalent ordinary Windows path at the Chromium CLI boundary.
            // IndexedDB's backing store does not accept verbatim drive paths.
            OsString::from(format!(
                "--user-data-dir={}",
                dunce::simplified(user_data_dir).display()
            )),
            OsString::from("--no-first-run"),
            OsString::from("--no-default-browser-check"),
        ];
        let mut saw_user_data_dir = false;

        for argument in &plan.arguments {
            match argument {
                LaunchArgument::UserDataDirectory(path) => {
                    if path != &plan.user_data_dir || saw_user_data_dir {
                        return Err(invalid_data("invalid user data directory argument"));
                    }
                    saw_user_data_dir = true;
                }
                LaunchArgument::ProxyServer { scheme, host, port } => {
                    rendered.push(OsString::from(format!(
                        "--proxy-server={}://{}:{}",
                        proxy_scheme(*scheme),
                        host,
                        port
                    )));
                }
                LaunchArgument::ProxyBypassList(values) => {
                    rendered.push(OsString::from(format!(
                        "--proxy-bypass-list={}",
                        values.join(";")
                    )));
                }
                LaunchArgument::DisableNonProxiedWebRtc => {
                    rendered.push(OsString::from(
                        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                    ));
                }
                LaunchArgument::AcceptLanguages(languages) => {
                    let primary = languages
                        .first()
                        .ok_or_else(|| invalid_data("accept language list is empty"))?;
                    rendered.push(OsString::from(format!("--lang={primary}")));
                }
                LaunchArgument::WindowSize { width, height } => {
                    rendered.push(OsString::from(format!("--window-size={width},{height}")));
                }
                // ── v0.2 SeedableNoise arguments ──────────────────────────────
                LaunchArgument::UserAgentOverride(ua) => {
                    rendered.push(OsString::from(format!("--user-agent={ua}")));
                }
                LaunchArgument::ForceDeviceScaleFactor(hundredths) => {
                    // Convert hundredths back to a decimal string (e.g. 150 → "1.5")
                    let whole = hundredths / 100;
                    let frac = hundredths % 100;
                    let value = if frac == 0 {
                        format!("{whole}")
                    } else if frac % 10 == 0 {
                        format!("{whole}.{}", frac / 10)
                    } else {
                        format!("{whole}.{frac:02}")
                    };
                    rendered.push(OsString::from(format!(
                        "--force-device-scale-factor={value}"
                    )));
                }
                LaunchArgument::LoadExtension(path) => {
                    argument
                        .validate()
                        .map_err(|error| invalid_data(error.to_string()))?;
                    rendered.push(OsString::from(format!(
                        "--load-extension={}",
                        dunce::simplified(path).display()
                    )));
                }
                LaunchArgument::DisableBlinkFeatures(features) => {
                    rendered.push(OsString::from(format!(
                        "--disable-blink-features={}",
                        features.join(",")
                    )));
                }
                LaunchArgument::EnableBlinkFeatures(features) => {
                    rendered.push(OsString::from(format!(
                        "--enable-blink-features={}",
                        features.join(",")
                    )));
                }
            }
        }
        if !saw_user_data_dir {
            return Err(invalid_data("launch plan has no user data directory"));
        }
        rendered.extend(plan.preferences.startup_urls.iter().map(OsString::from));

        if rendered.iter().any(|value| {
            let value = value.to_string_lossy().to_ascii_lowercase();
            value == "--no-sandbox" || value.starts_with("--remote-debugging-")
        }) {
            return Err(invalid_data("unsafe Chromium flag was generated"));
        }
        Ok(rendered)
    }
}

impl BrowserRuntime for ManagedChromiumRuntime {
    fn stop_accepting_probes(&self) {
        self.probing.stop();
    }
    fn wait_for_probes(&self) {
        self.probing.wait();
    }

    fn probe_fingerprint_support(
        &self,
        build_id: &BrowserBuildId,
    ) -> PortResult<FingerprintProbeResult> {
        let _slot = fingerprint_probe::ProbeSlot::acquire(&self.probing, build_id)?;
        // Clear earlier success before any operation which can fail, including
        // digest checks or process creation. A failed recheck never keeps it.
        let build = self.set_fingerprint_support(build_id, FingerprintSupport::Unverified)?;
        let observation = (|| {
            if build.trust != RuntimeTrust::Verified {
                return Err(PortError::new(
                    PortErrorKind::RuntimeUntrusted,
                    "browser runtime trust was revoked",
                ));
            }
            self.verify_build_integrity(&build)?;
            let result = fingerprint_probe::run(&build, &self.profile_root);
            // Recheck even when browser execution failed; a changing binary
            // must never gain a capability result from an older executable.
            self.verify_build_integrity(&build)?;
            result
        })();
        match observation {
            Ok(observation) => {
                let build = self.set_fingerprint_support(build_id, observation.support)?;
                Ok(FingerprintProbeResult {
                    build,
                    detail: observation.detail,
                })
            }
            Err(error) => {
                self.set_fingerprint_support(build_id, FingerprintSupport::Unsupported)?;
                Err(error)
            }
        }
    }

    fn list_builds(&self) -> PortResult<Vec<BrowserBuild>> {
        self.installed_builds()
    }

    fn resolve_build(&self, selector: &BrowserBuildSelector) -> PortResult<BrowserBuild> {
        let registry = self
            .builds
            .read()
            .map_err(|_| internal("browser build registry lock is poisoned"))?;
        let id = match selector {
            BrowserBuildSelector::Pinned(id) => id,
            BrowserBuildSelector::Channel(channel) => registry
                .active_channels
                .get(channel_key(*channel))
                .ok_or_else(|| PortError::not_found("active browser channel"))?,
        };
        registry
            .by_id
            .get(id)
            .cloned()
            .ok_or_else(|| PortError::not_found("browser build"))
    }

    fn launch(&self, plan: &ResolvedLaunchPlan) -> PortResult<BrowserProcess> {
        plan.validate()
            .map_err(|error| invalid_data(error.to_string()))?;
        let loads_extension = plan
            .arguments
            .iter()
            .any(|argument| matches!(argument, LaunchArgument::LoadExtension(_)));
        // Serialize the short admission/spawn step against probe admission,
        // so a Supported snapshot cannot outlive a concurrent re-probe.
        let fingerprint_gate = if loads_extension {
            Some(self.probing.lock_for_launch()?)
        } else {
            None
        };
        let build = self.verified_build_for_plan(plan)?;
        // A stale or caller-forged launch-plan capability cannot bypass the
        // registry. Fail before touching the persisted profile's Preferences.
        if loads_extension
            && (build.capabilities.fingerprint_support != FingerprintSupport::Supported
                || fingerprint_gate
                    .as_ref()
                    .is_some_and(|active| active.contains(&build.id)))
        {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "所选运行时未通过指纹扩展检测；可使用系统原生模式，不能启动 SeedableNoise",
            ));
        }
        let user_data_dir = self.prepare_profile(plan)?;
        let arguments = Self::render_arguments(plan, &user_data_dir)?;

        let mut processes = self
            .processes
            .lock()
            .map_err(|_| internal("browser process registry lock is poisoned"))?;
        if processes.contains_key(&plan.session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "session already owns a browser process",
            ));
        }

        Self::verify_plan_extension(plan)?;
        let child = BrowserChild::spawn(
            Command::new(&build.executable_path)
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .map_err(|error| {
            PortError::new(
                PortErrorKind::Unavailable,
                format!("failed to launch browser process: {error}"),
            )
        })?;
        let process_id = child.id();
        processes.insert(plan.session_id.clone(), child);
        Ok(BrowserProcess {
            session_id: plan.session_id.clone(),
            process_id,
            started_at_ms: now_ms(),
        })
    }

    fn stop(&self, session_id: &SessionId) -> PortResult<()> {
        let mut processes = self
            .processes
            .lock()
            .map_err(|_| internal("browser process registry lock is poisoned"))?;
        let child = processes
            .get_mut(session_id)
            .ok_or_else(|| PortError::not_found("browser process"))?;
        child
            .stop()
            .map_err(|error| internal(format!("failed to stop browser process tree: {error}")))?;
        processes.remove(session_id);
        Ok(())
    }

    fn poll_exit(
        &self,
        session_id: &SessionId,
    ) -> PortResult<Option<brows_application::BrowserExit>> {
        let mut processes = self
            .processes
            .lock()
            .map_err(|_| internal("browser process registry lock is poisoned"))?;
        let child = processes
            .get_mut(session_id)
            .ok_or_else(|| PortError::not_found("browser process"))?;
        child
            .try_wait()
            .map(|status| {
                status.map(|status| brows_application::BrowserExit {
                    success: status.success(),
                })
            })
            .map_err(|error| internal(format!("failed to query browser process: {error}")))
    }

    fn is_running(&self, session_id: &SessionId) -> PortResult<bool> {
        let mut processes = self
            .processes
            .lock()
            .map_err(|_| internal("browser process registry lock is poisoned"))?;
        let Some(child) = processes.get_mut(session_id) else {
            return Ok(false);
        };
        match child
            .try_wait()
            .map_err(|error| internal(format!("failed to query browser process: {error}")))?
        {
            None => Ok(true),
            Some(_) => Ok(false),
        }
    }
}

impl Drop for ManagedChromiumRuntime {
    fn drop(&mut self) {
        if let Ok(processes) = self.processes.get_mut() {
            for child in processes.values_mut() {
                let _ = child.stop();
            }
            processes.clear();
        }
    }
}

fn write_preferences(user_data_dir: &Path, plan: &ResolvedLaunchPlan) -> PortResult<()> {
    let default_dir = user_data_dir.join("Default");
    fs::create_dir_all(&default_dir).map_err(|error| {
        internal(format!(
            "failed to create profile preferences directory: {error}"
        ))
    })?;
    let path = default_dir.join("Preferences");
    let mut root = if path.exists() {
        let metadata = fs::metadata(&path)
            .map_err(|error| invalid_data(format!("cannot inspect Preferences: {error}")))?;
        if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES {
            return Err(invalid_data("Preferences is not a bounded regular file"));
        }
        let bytes = fs::read(&path)
            .map_err(|error| invalid_data(format!("cannot read Preferences: {error}")))?;
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| invalid_data(format!("Preferences is invalid JSON: {error}")))?
    } else {
        Value::Object(Map::new())
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| invalid_data("Preferences root must be a JSON object"))?;
    let intl = object
        .entry("intl")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| invalid_data("Preferences intl field must be an object"))?;
    intl.insert(
        "accept_languages".to_owned(),
        Value::String(plan.preferences.accept_languages.join(",")),
    );

    // ── v0.2 optional preference overrides ──────────────────────────────
    if let Some(webgl_enabled) = plan.preferences.webgl_enabled {
        let webkit = object
            .entry("webkit")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| invalid_data("Preferences webkit field must be an object"))?;
        let webgl = webkit
            .entry("webgl")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| invalid_data("Preferences webkit.webgl field must be an object"))?;
        webgl.insert("enabled".to_owned(), Value::Bool(webgl_enabled));
    }
    if let Some(hw_accel) = plan.preferences.hardware_acceleration {
        let profile_obj = object
            .entry("profile")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| invalid_data("Preferences profile field must be an object"))?;
        let mode = profile_obj
            .entry("hardware_acceleration_mode")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                invalid_data("Preferences hardware_acceleration_mode field must be an object")
            })?;
        mode.insert("enabled".to_owned(), Value::Bool(hw_accel));
    }
    if let Some(notif) = plan.preferences.notifications_setting {
        let profile_obj = object
            .entry("profile")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| invalid_data("Preferences profile field must be an object"))?;
        let content_settings = profile_obj
            .entry("default_content_setting_values")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                invalid_data("Preferences default_content_setting_values must be an object")
            })?;
        content_settings.insert("notifications".to_owned(), Value::Number(notif.into()));
    }
    let serialized = serde_json::to_vec(&root)
        .map_err(|error| internal(format!("failed to serialize Preferences: {error}")))?;
    let temporary = default_dir.join("Preferences.brows.tmp");
    fs::write(&temporary, serialized)
        .map_err(|error| internal(format!("failed to stage Preferences: {error}")))?;
    fs::rename(&temporary, &path)
        .map_err(|error| internal(format!("failed to activate Preferences: {error}")))?;
    Ok(())
}

fn create_and_canonicalize(path: PathBuf, label: &str) -> PortResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid_data(format!("{label} cannot be empty")));
    }
    fs::create_dir_all(&path)
        .map_err(|error| internal(format!("failed to create {label}: {error}")))?;
    path.canonicalize()
        .map_err(|error| internal(format!("failed to canonicalize {label}: {error}")))
}

fn create_confined_directory(root: &Path, requested: &Path) -> PortResult<PathBuf> {
    let relative = requested
        .strip_prefix(root)
        .map_err(|_| invalid_data("profile directory is outside the managed root"))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data("profile directory contains unsafe components"));
    }

    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        if current.exists() {
            let metadata = fs::symlink_metadata(&current).map_err(|error| {
                invalid_data(format!("cannot inspect profile directory: {error}"))
            })?;
            if metadata.file_type().is_symlink()
                || is_reparse_point(&metadata)
                || !metadata.is_dir()
            {
                return Err(invalid_data(
                    "profile path contains a link or non-directory component",
                ));
            }
        } else {
            fs::create_dir(&current).map_err(|error| {
                internal(format!("failed to create profile directory: {error}"))
            })?;
        }
    }
    let canonical = current
        .canonicalize()
        .map_err(|error| internal(format!("failed to canonicalize profile directory: {error}")))?;
    ensure_within(&canonical, root, "profile directory")?;
    Ok(canonical)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn canonical_regular_file(path: &Path, label: &str) -> PortResult<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|error| invalid_data(format!("cannot canonicalize {label}: {error}")))?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| invalid_data(format!("cannot inspect {label}: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(invalid_data(format!("{label} must be a regular file")));
    }
    Ok(canonical)
}

fn ensure_within(path: &Path, root: &Path, label: &str) -> PortResult<()> {
    if !path.starts_with(root) {
        return Err(invalid_data(format!("{label} is outside its managed root")));
    }
    Ok(())
}

fn verify_digest(path: &Path, expected: &str) -> PortResult<()> {
    let actual = digest_file(path)?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(PortError::new(
            PortErrorKind::RuntimeUntrusted,
            "browser executable digest does not match the signed catalog",
        ));
    }
    Ok(())
}

fn digest_file(path: &Path) -> PortResult<String> {
    let file = File::open(path)
        .map_err(|error| invalid_data(format!("cannot open browser executable: {error}")))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| invalid_data(format!("cannot hash browser executable: {error}")))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn channel_key(channel: BrowserChannel) -> &'static str {
    match channel {
        BrowserChannel::Stable => "stable",
        BrowserChannel::Beta => "beta",
        BrowserChannel::Dev => "dev",
        BrowserChannel::Canary => "canary",
    }
}

fn proxy_scheme(scheme: brows_domain::ProxyScheme) -> &'static str {
    match scheme {
        brows_domain::ProxyScheme::Http => "http",
        brows_domain::ProxyScheme::Https => "https",
        brows_domain::ProxyScheme::Socks5 => "socks5",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn invalid_data(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn internal(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_domain::{
        BrowserCapabilities, BrowserPreferences, ProfileId, ProxyLaunchBinding, Revision,
        RuntimeTrust,
    };

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "brows-runtime-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sample_plan(executable: PathBuf, profile_root: &Path) -> ResolvedLaunchPlan {
        let user_data_dir = profile_root.join("profile-key").join("userdata");
        ResolvedLaunchPlan {
            session_id: SessionId::new("session-1").unwrap(),
            profile_id: ProfileId::new("profile-1").unwrap(),
            profile_revision: Revision::FIRST,
            executable_path: executable,
            user_data_dir: user_data_dir.clone(),
            arguments: vec![
                LaunchArgument::UserDataDirectory(user_data_dir),
                LaunchArgument::AcceptLanguages(vec!["zh-CN".to_owned(), "en-US".to_owned()]),
                LaunchArgument::WindowSize {
                    width: 1280,
                    height: 800,
                },
            ],
            preferences: BrowserPreferences {
                accept_languages: vec!["zh-CN".to_owned(), "en-US".to_owned()],
                startup_urls: vec!["https://example.com".to_owned()],
                permissions_prompt: true,
                webgl_enabled: None,
                hardware_acceleration: None,
                notifications_setting: None,
            },
            proxy: ProxyLaunchBinding::Direct,
            capabilities: BrowserCapabilities {
                supports_language: true,
                supports_proxy: true,
                supports_webrtc_policy: false,
                supports_devtools_pipe: false,
                fingerprint_support: FingerprintSupport::Unverified,
            },
            warnings: Vec::new(),
            plan_hash: "a".repeat(64),
        }
    }

    fn sample_build(
        id: &str,
        executable_path: PathBuf,
        contents: &[u8],
        channel: BrowserChannel,
    ) -> BrowserBuild {
        let sha256 = Sha256::digest(contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        BrowserBuild {
            id: BrowserBuildId::new(id).unwrap(),
            provider: "test".to_owned(),
            version: id.to_owned(),
            channel,
            executable_path,
            sha256,
            publisher: "Test Publisher".to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities::default(),
            installed_at_ms: 1,
        }
    }

    #[test]
    fn rendered_arguments_are_typed_and_never_disable_security() {
        let profile_root = temp_root("arguments");
        let plan = sample_plan(PathBuf::from("chrome.exe"), &profile_root);
        let rendered =
            ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).unwrap();
        let rendered = rendered
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|value| value == "--lang=zh-CN"));
        assert!(rendered
            .iter()
            .any(|value| value == "--window-size=1280,800"));
        assert!(rendered.iter().all(|value| value != "--no-sandbox"));
        assert!(rendered
            .iter()
            .all(|value| !value.starts_with("--remote-debugging-")));
    }

    #[test]
    fn profile_directory_must_stay_under_managed_root() {
        let root = temp_root("confinement").canonicalize().unwrap();
        let outside = root.parent().unwrap().join("outside-userdata");
        let error = create_confined_directory(&root, &outside).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::InvalidData);
    }

    #[test]
    fn build_registration_rejects_digest_mismatch() {
        let root = temp_root("digest");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir_all(&runtime_dir).unwrap();
        let executable = runtime_dir.join("chrome.exe");
        fs::write(&executable, b"not really chromium").unwrap();
        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        let build = BrowserBuild {
            id: BrowserBuildId::new("stable-1").unwrap(),
            provider: "test".to_owned(),
            version: "1".to_owned(),
            channel: BrowserChannel::Stable,
            executable_path: executable,
            sha256: "0".repeat(64),
            publisher: "Test Publisher".to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities::default(),
            installed_at_ms: 1,
        };
        let error = runtime.register_build(build, true).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::RuntimeUntrusted);
    }

    #[test]
    fn current_digest_registration_rechecks_file_before_launch() {
        let root = temp_root("current-digest");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir_all(&runtime_dir).unwrap();
        let executable = runtime_dir.join("chrome.exe");
        fs::write(&executable, b"initial runtime").unwrap();

        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        let build = BrowserBuild {
            id: BrowserBuildId::new("stable-current").unwrap(),
            provider: "test".to_owned(),
            version: "1".to_owned(),
            channel: BrowserChannel::Stable,
            executable_path: executable.clone(),
            sha256: "0".repeat(64),
            publisher: "Test Publisher".to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities::default(),
            installed_at_ms: 1,
        };
        runtime
            .register_build_with_current_digest(build, true)
            .unwrap();
        let registered = runtime
            .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
            .unwrap();
        assert_ne!(registered.sha256, "0".repeat(64));

        fs::write(&executable, b"changed runtime").unwrap();
        let plan = sample_plan(registered.executable_path, &profile_dir);
        let error = runtime.verified_build_for_plan(&plan).unwrap_err();
        assert_eq!(error.kind, PortErrorKind::RuntimeUntrusted);
    }

    #[test]
    fn activation_replaces_the_active_build_for_a_channel() {
        let root = temp_root("activation");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir_all(&runtime_dir).unwrap();

        let first_path = runtime_dir.join("chrome-1.exe");
        let second_path = runtime_dir.join("chrome-2.exe");
        let first_contents = b"first test runtime";
        let second_contents = b"second test runtime";
        fs::write(&first_path, first_contents).unwrap();
        fs::write(&second_path, second_contents).unwrap();

        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        let first = sample_build(
            "stable-1",
            first_path,
            first_contents,
            BrowserChannel::Stable,
        );
        let second = sample_build(
            "stable-2",
            second_path,
            second_contents,
            BrowserChannel::Stable,
        );
        let first_id = first.id.clone();
        let second_id = second.id.clone();

        runtime.register_build(first, false).unwrap();
        runtime.register_build(second, false).unwrap();
        runtime.activate(&first_id).unwrap();
        assert_eq!(
            runtime
                .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
                .unwrap()
                .id,
            first_id
        );

        runtime.activate(&second_id).unwrap();
        assert_eq!(
            runtime
                .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
                .unwrap()
                .id,
            second_id
        );
    }

    #[test]
    fn registration_cannot_claim_fingerprint_support_without_an_observation() {
        let root = temp_root("fake-support");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir(&runtime_dir).unwrap();
        let executable = runtime_dir.join("chrome.exe");
        let contents = b"test runtime, never executed";
        fs::write(&executable, contents).unwrap();
        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        let mut build = sample_build("build-1", executable, contents, BrowserChannel::Stable);
        build.capabilities.fingerprint_support = FingerprintSupport::Supported;
        runtime.register_build(build, true).unwrap();
        let registered = runtime
            .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
            .unwrap();
        assert_eq!(
            registered.capabilities.fingerprint_support,
            FingerprintSupport::Unverified
        );
    }

    #[test]
    fn unsupported_extension_launch_fails_before_profile_writes_even_with_forged_plan_support() {
        let root = temp_root("extension-gate");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir(&runtime_dir).unwrap();
        let executable = runtime_dir.join("chrome.exe");
        let contents = b"test runtime, never executed";
        fs::write(&executable, contents).unwrap();
        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        runtime
            .register_build(
                sample_build(
                    "build-1",
                    executable.clone(),
                    contents,
                    BrowserChannel::Stable,
                ),
                true,
            )
            .unwrap();
        let mut plan = sample_plan(executable, &profile_dir.canonicalize().unwrap());
        plan.capabilities.fingerprint_support = FingerprintSupport::Supported;
        plan.arguments
            .push(LaunchArgument::LoadExtension(root.join("extensions")));
        for state in [
            FingerprintSupport::Unverified,
            FingerprintSupport::Unsupported,
        ] {
            runtime
                .set_fingerprint_support(&BrowserBuildId::new("build-1").unwrap(), state)
                .unwrap();
            assert_eq!(
                runtime.launch(&plan).unwrap_err().kind,
                PortErrorKind::CapabilityUnsupported
            );
            assert!(!plan.user_data_dir.exists());
            assert!(runtime.processes.lock().unwrap().is_empty());
        }
        let id = BrowserBuildId::new("build-1").unwrap();
        runtime
            .set_fingerprint_support(&id, FingerprintSupport::Supported)
            .unwrap();
        let slot = fingerprint_probe::ProbeSlot::acquire(&runtime.probing, &id).unwrap();
        assert_eq!(
            runtime.launch(&plan).unwrap_err().kind,
            PortErrorKind::CapabilityUnsupported
        );
        assert!(!plan.user_data_dir.exists());
        drop(slot);
    }

    #[test]
    fn failed_probe_clears_previous_success_and_does_not_keep_a_busy_slot() {
        let root = temp_root("probe-digest");
        let runtime_dir = root.join("runtime");
        let profile_dir = root.join("profiles");
        fs::create_dir(&runtime_dir).unwrap();
        let executable = runtime_dir.join("chrome.exe");
        let contents = b"test runtime, never executed";
        fs::write(&executable, contents).unwrap();
        let runtime = ManagedChromiumRuntime::new(&runtime_dir, &profile_dir).unwrap();
        let build = sample_build(
            "build-1",
            executable.clone(),
            contents,
            BrowserChannel::Stable,
        );
        let id = build.id.clone();
        runtime.register_build(build, true).unwrap();
        runtime
            .set_fingerprint_support(&id, FingerprintSupport::Supported)
            .unwrap();
        fs::write(&executable, b"changed after registration").unwrap();
        assert_eq!(
            runtime.probe_fingerprint_support(&id).unwrap_err().kind,
            PortErrorKind::RuntimeUntrusted
        );
        let build = runtime
            .resolve_build(&BrowserBuildSelector::Pinned(id.clone()))
            .unwrap();
        assert_eq!(
            build.capabilities.fingerprint_support,
            FingerprintSupport::Unsupported
        );
        let _available = fingerprint_probe::ProbeSlot::acquire(&runtime.probing, &id).unwrap();
        assert!(fs::read_dir(&profile_dir).unwrap().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "starts an isolated signed headless Chrome against localhost"]
    fn real_signed_chrome_fingerprint_probe() {
        let executable = PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        let runtime_root = executable.parent().unwrap();
        let publisher = verify_authenticode_publisher(&executable, &["Google LLC"]).unwrap();
        let version = fs::read_dir(runtime_root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| {
                let key = name
                    .split('.')
                    .map(str::parse::<u32>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                (key.len() == 4).then_some((key, name))
            })
            .max_by(|left, right| left.0.cmp(&right.0))
            .unwrap()
            .1;
        let scratch = temp_root("real-fingerprint-probe");
        let profiles = scratch.join("profiles");
        let runtime = ManagedChromiumRuntime::new(runtime_root, &profiles).unwrap();
        let id = BrowserBuildId::new("real-chrome-probe").unwrap();
        runtime
            .register_build_with_current_digest(
                BrowserBuild {
                    id: id.clone(),
                    provider: "Google Chrome".to_owned(),
                    version,
                    channel: BrowserChannel::Stable,
                    executable_path: executable,
                    sha256: "0".repeat(64),
                    publisher,
                    trust: RuntimeTrust::Verified,
                    capabilities: BrowserCapabilities::default(),
                    installed_at_ms: now_ms(),
                },
                true,
            )
            .unwrap();
        let result = runtime.probe_fingerprint_support(&id).unwrap();
        println!(
            "fingerprint support: {:?}; {}",
            result.build.capabilities.fingerprint_support, result.detail
        );
        assert_ne!(
            result.build.capabilities.fingerprint_support,
            FingerprintSupport::Unverified
        );
        assert_eq!(
            runtime
                .resolve_build(&BrowserBuildSelector::Pinned(id))
                .unwrap(),
            result.build
        );
        assert!(fs::read_dir(profiles).unwrap().next().is_none());
    }

    // ── v0.2 SeedableNoise render tests ────────────────────────────────────

    fn seedable_plan(profile_root: &Path, extra_args: Vec<LaunchArgument>) -> ResolvedLaunchPlan {
        let user_data_dir = profile_root.join("profile-key").join("userdata");
        let mut arguments = vec![
            LaunchArgument::UserDataDirectory(user_data_dir.clone()),
            LaunchArgument::AcceptLanguages(vec!["en-US".to_owned()]),
            LaunchArgument::WindowSize {
                width: 1920,
                height: 1080,
            },
        ];
        arguments.extend(extra_args);
        ResolvedLaunchPlan {
            session_id: SessionId::new("session-1").unwrap(),
            profile_id: ProfileId::new("profile-1").unwrap(),
            profile_revision: Revision::FIRST,
            executable_path: PathBuf::from("chrome.exe"),
            user_data_dir,
            arguments,
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
            warnings: Vec::new(),
            plan_hash: "a".repeat(64),
        }
    }

    #[test]
    fn runtime_renders_user_agent_override_arg() {
        let root = temp_root("ua");
        let plan = seedable_plan(
            &root,
            vec![LaunchArgument::UserAgentOverride(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/150.0.0.0".to_owned(),
            )],
        );
        let rendered =
            ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).expect("render");
        let text = rendered
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("--user-agent=Mozilla/5.0"),
            "missing UA arg: {text}"
        );
    }

    #[test]
    fn runtime_renders_load_extension_arg() {
        let root = temp_root("load-ext");
        let ext_path = root.join("extensions").join("seed-abc");
        fs::create_dir_all(&ext_path).unwrap();
        let plan = seedable_plan(&root, vec![LaunchArgument::LoadExtension(ext_path.clone())]);
        let rendered =
            ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).expect("render");
        let text = rendered
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains(&format!("--load-extension={}", ext_path.display())),
            "missing load-extension arg: {text}"
        );
    }

    #[test]
    fn extension_paths_cannot_expand_to_a_comma_separated_list() {
        let root = temp_root("extension-comma");
        let path = root.join("first,second");
        let argument = LaunchArgument::LoadExtension(path);
        assert!(argument.validate().is_err());
        let plan = seedable_plan(&root, vec![argument]);
        assert!(ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).is_err());
        assert!(brows_application::memory::materialize_fingerprint_probe(
            &root.join("extensions,other"),
            &plan.profile_id,
            "156.0.8078.80"
        )
        .is_err());
        assert!(!root.join("extensions,other").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_writes_webgl_preference_when_present() {
        let root = temp_root("prefs-webgl");
        let user_data = root.join("profile-key").join("userdata");
        fs::create_dir_all(&user_data).unwrap();
        let mut plan = seedable_plan(&root, vec![]);
        plan.preferences.webgl_enabled = Some(false);
        ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).unwrap();
        write_preferences(&user_data, &plan).expect("write_preferences");
        let prefs_path = user_data.join("Default").join("Preferences");
        let bytes = fs::read(&prefs_path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("webkit")
                .and_then(|v| v.get("webgl"))
                .and_then(|v| v.get("enabled"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn runtime_writes_notifications_preference_when_present() {
        let root = temp_root("prefs-notif");
        let user_data = root.join("profile-key").join("userdata");
        fs::create_dir_all(&user_data).unwrap();
        let mut plan = seedable_plan(&root, vec![]);
        plan.preferences.notifications_setting = Some(2);
        ManagedChromiumRuntime::render_arguments(&plan, &plan.user_data_dir).unwrap();
        write_preferences(&user_data, &plan).expect("write_preferences");
        let prefs_path = user_data.join("Default").join("Preferences");
        let bytes = fs::read(&prefs_path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("profile")
                .and_then(|v| v.get("default_content_setting_values"))
                .and_then(|v| v.get("notifications"))
                .and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[test]
    fn runtime_rejects_unsupported_launch_argument_combinations() {
        let _root = temp_root("validate");
        // Empty path: validation should fail at the domain layer.
        let err = LaunchArgument::LoadExtension(PathBuf::new()).validate();
        assert!(err.is_err(), "empty extension path must be rejected");
        // Empty UA: rejected.
        let err = LaunchArgument::UserAgentOverride(String::new()).validate();
        assert!(err.is_err(), "empty UA must be rejected");
        // Empty feature list entry: rejected.
        let err = LaunchArgument::DisableBlinkFeatures(vec![String::new()]).validate();
        assert!(err.is_err(), "empty blink feature must be rejected");
    }
}
