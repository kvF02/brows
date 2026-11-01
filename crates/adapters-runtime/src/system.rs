//! Fixed system-browser providers. No executable paths or publisher policies
//! come from IPC. Source trust and fingerprint capability remain independent.
use super::*;
use brows_domain::BrowserCapabilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemBrowserProvider {
    GoogleChrome,
    MicrosoftEdge,
}
impl SystemBrowserProvider {
    fn name(self) -> &'static str {
        match self {
            Self::GoogleChrome => "Google Chrome",
            Self::MicrosoftEdge => "Microsoft Edge",
        }
    }
    fn publisher(self) -> &'static str {
        match self {
            Self::GoogleChrome => "Google LLC",
            Self::MicrosoftEdge => "Microsoft Corporation",
        }
    }
    fn executable(self) -> &'static str {
        match self {
            Self::GoogleChrome => "chrome.exe",
            Self::MicrosoftEdge => "msedge.exe",
        }
    }
    fn relative_root(self) -> &'static str {
        match self {
            Self::GoogleChrome => "Google/Chrome/Application",
            Self::MicrosoftEdge => "Microsoft/Edge/Application",
        }
    }
    fn id(self) -> &'static str {
        match self {
            // Preserve persisted selectors from earlier Alpha builds. The
            // actual PE version is recorded separately, never inferred here.
            Self::GoogleChrome => "google-chrome-stable-150",
            Self::MicrosoftEdge => "microsoft-edge-stable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemBrowserInstallation {
    provider: SystemBrowserProvider,
    root: PathBuf,
}
impl SystemBrowserInstallation {
    /// Backend composition only. Admission still requires the provider's fixed
    /// executable, publisher, real PE version, and a controlled runtime root.
    pub fn at_root(provider: SystemBrowserProvider, root: PathBuf) -> Self {
        Self { provider, root }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn provider(&self) -> SystemBrowserProvider {
        self.provider
    }

    pub fn admit(
        &self,
        runtime: &ManagedChromiumRuntime,
        activate: bool,
    ) -> PortResult<BrowserBuild> {
        self.admit_with(runtime, activate, &NativeSystemVerifier)
    }
    fn admit_with(
        &self,
        runtime: &ManagedChromiumRuntime,
        activate: bool,
        verifier: &impl SystemVerifier,
    ) -> PortResult<BrowserBuild> {
        let executable = canonical_regular_file(
            &self.root.join(self.provider.executable()),
            "system browser",
        )?;
        runtime.ensure_runtime_path(&executable)?;
        let digest = digest_file(&executable)?;
        let (publisher, version) = verifier.inspect(&executable, self.provider.publisher())?;
        if !publisher.eq_ignore_ascii_case(self.provider.publisher()) || version[0] == 0 {
            return Err(bundle::untrusted(
                "系统浏览器发布者或版本与 Provider 不匹配",
            ));
        }
        // A concurrent update cannot turn the signature/version observation
        // into admission of different bytes. register_build checks again.
        verify_digest(&executable, &digest)?;
        let build = BrowserBuild {
            id: BrowserBuildId::new(self.provider.id()).map_err(|e| invalid_data(e.to_string()))?,
            provider: self.provider.name().into(),
            version: version.map(|v| v.to_string()).join("."),
            channel: BrowserChannel::Stable,
            executable_path: executable,
            sha256: digest,
            publisher,
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities {
                supports_language: true,
                supports_proxy: true,
                supports_webrtc_policy: true,
                supports_devtools_pipe: true,
                fingerprint_support: FingerprintSupport::Unverified,
            },
            installed_at_ms: now_ms(),
        };
        runtime.register_build(build.clone(), activate)?;
        Ok(build)
    }
}

trait SystemVerifier {
    fn inspect(&self, executable: &Path, publisher: &str) -> PortResult<(String, [u32; 4])>;
}
struct NativeSystemVerifier;
impl SystemVerifier for NativeSystemVerifier {
    fn inspect(&self, executable: &Path, publisher: &str) -> PortResult<(String, [u32; 4])> {
        Ok((
            verify_authenticode_publisher(executable, &[publisher])?,
            version::file_version(executable)?,
        ))
    }
}

pub fn discover_system_browsers() -> Vec<SystemBrowserInstallation> {
    let roots = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    discover_under(&roots)
}
fn discover_under(roots: &[PathBuf]) -> Vec<SystemBrowserInstallation> {
    [
        SystemBrowserProvider::GoogleChrome,
        SystemBrowserProvider::MicrosoftEdge,
    ]
    .into_iter()
    .filter_map(|provider| {
        roots
            .iter()
            .map(|root| root.join(provider.relative_root()))
            .find(|root| root.join(provider.executable()).is_file())
            .map(|root| SystemBrowserInstallation::at_root(provider, root))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Verifier {
        publisher: &'static str,
        modify: bool,
    }
    impl SystemVerifier for Verifier {
        fn inspect(&self, executable: &Path, _publisher: &str) -> PortResult<(String, [u32; 4])> {
            if self.modify {
                fs::write(executable, b"changed while verifying").unwrap();
            }
            Ok((self.publisher.into(), [156, 1, 2, 3]))
        }
    }
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "brows-system-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for provider in [
            SystemBrowserProvider::GoogleChrome,
            SystemBrowserProvider::MicrosoftEdge,
        ] {
            let path = root.join(provider.relative_root());
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join(provider.executable()),
                b"test fixture, not a real trusted browser",
            )
            .unwrap();
        }
        root
    }
    #[test]
    fn fixed_providers_keep_publishers_separate_and_do_not_replace_the_default_channel() {
        let root = fixture();
        let candidates = discover_under(std::slice::from_ref(&root));
        assert_eq!(candidates.len(), 2);
        let runtime = ManagedChromiumRuntime::with_roots(
            candidates.iter().map(|v| v.root.clone()).collect(),
            root.join("profiles"),
        )
        .unwrap();
        let chrome = candidates[0]
            .admit_with(
                &runtime,
                true,
                &Verifier {
                    publisher: "Google LLC",
                    modify: false,
                },
            )
            .unwrap();
        let edge = candidates[1]
            .admit_with(
                &runtime,
                false,
                &Verifier {
                    publisher: "Microsoft Corporation",
                    modify: false,
                },
            )
            .unwrap();
        assert_eq!(edge.version, "156.1.2.3");
        assert_eq!(
            edge.capabilities.fingerprint_support,
            FingerprintSupport::Unverified
        );
        assert_eq!(
            runtime
                .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
                .unwrap()
                .id,
            chrome.id
        );
        assert_eq!(
            runtime
                .resolve_build(&BrowserBuildSelector::Pinned(edge.id.clone()))
                .unwrap()
                .provider,
            "Microsoft Edge"
        );
        drop(runtime);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn provider_mismatch_update_during_admission_and_uncontrolled_paths_are_rejected() {
        for mode in 0..3 {
            let root = fixture();
            let candidates = discover_under(std::slice::from_ref(&root));
            let runtime = ManagedChromiumRuntime::new(
                if mode == 2 {
                    root.join("other")
                } else {
                    candidates[1].root.clone()
                },
                root.join("profiles"),
            )
            .unwrap();
            let error = candidates[1]
                .admit_with(
                    &runtime,
                    true,
                    &Verifier {
                        publisher: if mode == 0 {
                            "Google LLC"
                        } else {
                            "Microsoft Corporation"
                        },
                        modify: mode == 1,
                    },
                )
                .unwrap_err();
            assert!(matches!(
                error.kind,
                PortErrorKind::RuntimeUntrusted | PortErrorKind::InvalidData
            ));
            assert!(runtime.list_builds().unwrap().is_empty());
            drop(runtime);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
