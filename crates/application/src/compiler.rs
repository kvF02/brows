use crate::{
    sha256::digest_hex, ApplicationError, ApplicationResult, BrowserRuntime, FingerprintRuntime,
    PortError, PortErrorKind, PresetCatalog, ProxyLease, ProxyRuntime,
};
use brows_contracts::ErrorCode;
use brows_domain::{
    BrowserPreferences, LaunchArgument, LaunchWarning, ProfileSpec, ProxyLaunchBinding,
    ProxyScheme, ResolvedLaunchPlan, RuntimeTrust, SessionId,
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

        let fingerprint = self.fingerprints.compile(&profile.persona, &build)?;
        // v0.1 has no fingerprint-specific launch argument variants. Keeping
        // this boundary strict prevents an adapter from overriding storage,
        // proxy, locale, or window settings through duplicate arguments.
        if !fingerprint.arguments.is_empty() {
            return Err(ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                "the fingerprint runtime emitted unsupported launch arguments",
            ));
        }

        // A digest-derived directory component remains safe even if a future
        // ID format permits path separators.
        let directory_key = digest_hex(profile.id.as_str().as_bytes());
        let user_data_dir = self.profile_data_root.join(directory_key).join("userdata");
        let mut arguments = vec![LaunchArgument::UserDataDirectory(user_data_dir.clone())];
        let mut warnings = fingerprint.warnings;

        if build.capabilities.supports_language {
            arguments.push(LaunchArgument::AcceptLanguages(
                profile.locale.accept_languages.clone(),
            ));
        } else {
            warnings.push(LaunchWarning::UnsupportedCapability("language"));
        }
        arguments.push(LaunchArgument::WindowSize {
            width: profile.window.width,
            height: profile.window.height,
        });

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
            arguments.push(LaunchArgument::ProxyBypassList(vec![
                "<-loopback>".to_owned()
            ]));
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
