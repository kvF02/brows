use crate::{
    sha256::digest_hex, ApplicationError, ApplicationResult, BrowserRuntime, FingerprintRuntime,
    PortError, PortErrorKind, PresetCatalog, ProxyLease, ProxyRuntime,
};
use brows_contracts::ErrorCode;
use brows_domain::{
    BrowserPreferences, FingerprintPersona, FingerprintSupport, LaunchArgument, LaunchWarning,
    ProfileSpec, ProxyLaunchBinding, ProxyScheme, ResolvedLaunchPlan, RuntimeTrust, SessionId,
};
use std::{path::PathBuf, sync::Arc};

/// A validated launch plan plus the optional proxy broker lease that must live
/// for exactly as long as its browser session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLaunch {
    pub plan: ResolvedLaunchPlan,
    pub proxy_lease: Option<ProxyLease>,
}

/// Resolves a declarative profile into the only representation a concrete
/// browser adapter may execute. It deliberately emits typed arguments rather
/// than accepting arbitrary caller-provided command lines.
#[derive(Debug)]
pub struct LaunchPlanCompiler {
    presets: Arc<dyn PresetCatalog>,
    browsers: Arc<dyn BrowserRuntime>,
    proxies: Arc<dyn ProxyRuntime>,
    fingerprints: Arc<dyn FingerprintRuntime>,
    profile_data_root: PathBuf,
}

impl LaunchPlanCompiler {
    pub fn new(
        presets: Arc<dyn PresetCatalog>,
        browsers: Arc<dyn BrowserRuntime>,
        proxies: Arc<dyn ProxyRuntime>,
        fingerprints: Arc<dyn FingerprintRuntime>,
        profile_data_root: PathBuf,
    ) -> ApplicationResult<Self> {
        if profile_data_root.as_os_str().is_empty()
            || profile_data_root.to_string_lossy().contains('\0')
        {
            return Err(ApplicationError::invalid_field(
                "profile_data_root",
                "must be a non-empty local path",
            ));
        }
        Ok(Self {
            presets,
            browsers,
            proxies,
            fingerprints,
            profile_data_root,
        })
    }

    pub fn compile(
        &self,
        profile: &ProfileSpec,
        session_id: &SessionId,
    ) -> ApplicationResult<PreparedLaunch> {
        profile.validate()?;
        if profile.locale.timezone.is_some() {
            return Err(ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                "timezone override is not implemented; use the host timezone",
            ));
        }
        if profile.archived {
            return Err(ApplicationError::new(
                ErrorCode::InvalidProfileState,
                "archived profiles cannot be launched",
            ));
        }

        let preset = self.presets.resolve(&profile.environment_preset)?;
        preset.validate()?;
        if preset.reference != profile.environment_preset {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "preset catalog returned a different version",
            )
            .into());
        }

        let build = self.browsers.resolve_build(&profile.browser)?;
        build.validate()?;
        if build.trust != RuntimeTrust::Verified {
            return Err(ApplicationError::new(
                ErrorCode::RuntimeUntrusted,
                "the selected browser runtime is not verified",
            ));
        }

        if matches!(profile.persona, FingerprintPersona::SeedableNoise(_))
            && build.capabilities.fingerprint_support != FingerprintSupport::Supported
        {
            return Err(ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                match build.capabilities.fingerprint_support {
                    FingerprintSupport::Unsupported => "所选浏览器未能加载指纹扩展，SeedableNoise 已阻止启动。可切换系统原生，或选择通过指纹能力检测的可信运行时。",
                    _ => "所选浏览器的指纹能力尚未验证。请先在设置中检测指纹能力；系统原生环境不受此限制。",
                },
            ));
        }
        let fingerprint = self.fingerprints.compile_profile(profile, &build)?;
        // v0.1: fingerprint arguments must be empty.
        // v0.2+: accept typed fingerprint arguments from SeedableNoiseFingerprintRuntime.
        // All LaunchArgument variants are allowlisted in the domain, so this is safe.
        for argument in &fingerprint.arguments {
            argument.validate().map_err(ApplicationError::from)?;
        }

        // A digest-derived directory component remains safe even if a future
        // ID format permits path separators.
        let directory_key = digest_hex(profile.id.as_str().as_bytes());
        let user_data_dir = self.profile_data_root.join(directory_key).join("userdata");
        let mut arguments = vec![LaunchArgument::UserDataDirectory(user_data_dir.clone())];
        // v0.2: append fingerprint runtime arguments (UA, scale factor, extension load, etc.)
        // They were already validated above.
        arguments.extend(fingerprint.arguments);
        let mut warnings = fingerprint.warnings;

        if build.capabilities.supports_language {
            arguments.push(LaunchArgument::AcceptLanguages(
                profile.locale.accept_languages.clone(),
            ));
        } else {
            warnings.push(LaunchWarning::UnsupportedCapability("language"));
        }
        if !arguments
            .iter()
            .any(|argument| matches!(argument, LaunchArgument::WindowSize { .. }))
        {
            arguments.push(LaunchArgument::WindowSize {
                width: profile.window.width,
                height: profile.window.height,
            });
        }

        for field in profile.persona.host_shared_fields() {
            let warning = LaunchWarning::HostFingerprintShared(field);
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }

        let mut proxy_lease = None;
        let proxy = if let Some(proxy_id) = &profile.proxy_id {
            if !build.capabilities.supports_proxy {
                return Err(ApplicationError::new(
                    ErrorCode::CapabilityUnsupported,
                    "the selected browser runtime does not support a fail-closed proxy",
                ));
            }
            let proxy_spec = self.proxies.resolve_proxy(proxy_id)?;
            proxy_spec.validate()?;
            let lease = self.proxies.prepare_session(session_id, &proxy_spec)?;
            let binding = lease.binding.clone();
            let (host, port) = match &binding {
                ProxyLaunchBinding::Loopback { host, port } => (host.clone(), *port),
                ProxyLaunchBinding::Direct => {
                    let _ = self.proxies.release_session(&lease);
                    return Err(ApplicationError::new(
                        ErrorCode::ProxyWouldFallback,
                        "configured proxy resolved to a direct connection",
                    ));
                }
            };
            arguments.push(LaunchArgument::ProxyServer {
                // The local credential-hiding broker exposes an HTTP proxy
                // regardless of the upstream proxy protocol.
                scheme: ProxyScheme::Http,
                host,
                port,
            });
            let mut bypass_hosts = vec!["<-loopback>".to_owned()];
            bypass_hosts.extend(proxy_spec.bypass_hosts.iter().cloned());
            arguments.push(LaunchArgument::ProxyBypassList(bypass_hosts));
            if build.capabilities.supports_webrtc_policy {
                arguments.push(LaunchArgument::DisableNonProxiedWebRtc);
            }
            proxy_lease = Some(lease);
            binding
        } else {
            warnings.push(LaunchWarning::ProxyNotConfigured);
            ProxyLaunchBinding::Direct
        };

        let preferences = BrowserPreferences {
            accept_languages: profile.locale.accept_languages.clone(),
            startup_urls: profile.startup_urls.clone(),
            permissions_prompt: true,
            webgl_enabled: fingerprint
                .preferences
                .as_ref()
                .and_then(|p| p.webgl_enabled),
            hardware_acceleration: fingerprint
                .preferences
                .as_ref()
                .and_then(|p| p.hardware_acceleration),
            notifications_setting: fingerprint
                .preferences
                .as_ref()
                .and_then(|p| p.notifications_setting),
        };
        let hash_material = format!(
            "brows-launch-v1\0{}\0{}\0{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
            session_id.as_str(),
            profile.id.as_str(),
            profile.revision.get(),
            build.sha256,
            build.executable_path,
            user_data_dir,
            arguments,
            preferences,
        );
        let plan = ResolvedLaunchPlan {
            session_id: session_id.clone(),
            profile_id: profile.id.clone(),
            profile_revision: profile.revision,
            executable_path: build.executable_path,
            user_data_dir,
            arguments,
            preferences,
            proxy,
            capabilities: build.capabilities,
            warnings,
            plan_hash: digest_hex(hash_material.as_bytes()),
        };

        if let Err(error) = plan.validate() {
            if let Some(lease) = &proxy_lease {
                let _ = self.proxies.release_session(lease);
            }
            return Err(error.into());
        }
        Ok(PreparedLaunch { plan, proxy_lease })
    }
}
