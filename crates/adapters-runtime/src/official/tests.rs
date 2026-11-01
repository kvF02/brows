use super::*;
use std::{io::Cursor, sync::mpsc, time::Duration};
use zip::write::SimpleFileOptions;

const VERSION: &str = "156.0.8078.80";
fn metadata(version: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"channels":{"Stable":{"channel":"Stable","version":version,"downloads":{"chrome":[{"platform":"win64","url":archive_url(version).unwrap()}]}}}})).unwrap()
}
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, content) in entries {
        writer
            .start_file(
                *path,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
fn package() -> Vec<u8> {
    archive(&[
        ("chrome-win64/chrome.exe", b"not executable"),
        ("chrome-win64/chrome.dll", b"test DLL"),
        ("chrome-win64/chrome_elf.dll", b"elf"),
        ("chrome-win64/resources.pak", b"resources"),
        ("chrome-win64/icudtl.dat", b"data"),
        ("chrome-win64/First Run", b""),
    ])
}
pub(super) fn fixture(label: &str) -> (OfficialRuntimeInstaller, PathBuf) {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!(
        "brows-cft-{label}-{:032x}",
        u128::from_le_bytes(random)
    ));
    let runtime = Arc::new(
        ManagedChromiumRuntime::with_roots(
            vec![root.join("system"), root.join("managed")],
            root.join("profiles"),
        )
        .unwrap(),
    );
    (
        OfficialRuntimeInstaller::new(root.join("managed"), runtime).unwrap(),
        root,
    )
}
struct FakeSource {
    zip: Vec<u8>,
    gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
}
impl Default for FakeSource {
    fn default() -> Self {
        Self {
            zip: package(),
            gate: None,
        }
    }
}
impl Source for FakeSource {
    fn fetch(
        &self,
        url: &str,
        output: &mut dyn Write,
        limit: u64,
        control: &OfficialRuntimeInstaller,
    ) -> PortResult<String> {
        assert!(allowed_url(url));
        let bytes = if url == CATALOG {
            metadata(VERSION)
        } else {
            self.zip.clone()
        };
        assert!(bytes.len() as u64 <= limit);
        control.reset_bytes(Some(bytes.len() as u64));
        if url != CATALOG {
            if let Some((started, resume)) = &self.gate {
                output.write_all(b"partial").unwrap();
                started.send(()).unwrap();
                resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }
        control.checkpoint()?;
        output.write_all(&bytes).unwrap();
        control.bytes(bytes.len());
        Ok(format!("{:x}", Sha256::digest(&bytes)))
    }
}
struct FakeVerifier<'a> {
    reject: bool,
    cancel: Option<&'a OfficialRuntimeInstaller>,
}
impl Verifier for FakeVerifier<'_> {
    fn verify(&self, path: &Path, version: &str) -> PortResult<String> {
        assert!(parse_version(version).is_ok());
        assert!(path.ends_with("chrome.exe"));
        if self.reject {
            return Err(bundle::untrusted("test signature rejection"));
        }
        if let Some(control) = self.cancel {
            control.cancel().unwrap();
        }
        Ok("Google LLC".into())
    }
}
fn verifier() -> FakeVerifier<'static> {
    FakeVerifier {
        reject: false,
        cancel: None,
    }
}

#[test]
fn official_catalog_requires_exact_channel_platform_version_and_download_origin() {
    let valid = metadata(VERSION);
    assert_eq!(
        catalog(&valid).unwrap(),
        (VERSION.into(), archive_url(VERSION).unwrap())
    );
    for version in [
        "156",
        "0156.0.8078.80",
        "156.0.8078.80/../x",
        "156.0.8078.80?x",
        "0.0.0.0",
        "156.0.8078.4294967296",
    ] {
        assert!(parse_version(version).is_err());
    }
    for url in ["http://storage.googleapis.com/chrome-for-testing-public/156.0.8078.80/win64/chrome-win64.zip", "https://storage.googleapis.com.evil.example/file.zip", "https://storage.googleapis.com/chrome-for-testing-public/156.0.8078.80/win64/chrome-win64.zip?redirect=1", "https://user:password@storage.googleapis.com/file.zip", "file:///tmp/chrome.exe", "https://127.0.0.1/chrome-win64.zip"] { assert!(!allowed_url(url)); }
    for mutation in 0..4 {
        let mut value: Value = serde_json::from_slice(&valid).unwrap();
        match mutation {
            0 => {
                value["channels"]["Stable"]["downloads"]["chrome"][0]["url"] =
                    Value::String("https://evil.example/chrome.zip".into())
            }
            1 => value["channels"]["Stable"]["channel"] = Value::String("Beta".into()),
            2 => {
                value["channels"]["Stable"]["downloads"]["chrome"][0]["platform"] =
                    Value::String("win32".into())
            }
            _ => {
                let row = value["channels"]["Stable"]["downloads"]["chrome"][0].clone();
                value["channels"]["Stable"]["downloads"]["chrome"]
                    .as_array_mut()
                    .unwrap()
                    .push(row);
            }
        }
        assert!(catalog(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn verified_install_survives_restart_and_checks_every_bundle_file_without_claiming_probe_success() {
    let (installer, root) = fixture("restart");
    let result = installer
        .install_with(&FakeSource::default(), &verifier())
        .unwrap();
    assert_eq!(
        result.build.capabilities.fingerprint_support,
        FingerprintSupport::Unsupported
    );
    assert_eq!(installer.status().phase, "unsupported");
    assert!(!installer.status().running);
    assert_eq!(installer.runtime.installed_builds().unwrap().len(), 1);
    assert!(installer.root.join(result.build.id.as_str()).is_dir());
    assert_eq!(fs::read_dir(&installer.root).unwrap().count(), 1);
    let runtime = Arc::new(
        ManagedChromiumRuntime::with_roots(
            vec![root.join("system"), root.join("managed")],
            root.join("profiles"),
        )
        .unwrap(),
    );
    let reopened = OfficialRuntimeInstaller::new(root.join("managed"), runtime).unwrap();
    assert!(reopened.load_with(&verifier()).unwrap().is_empty());
    let loaded = reopened
        .runtime
        .resolve_build(&BrowserBuildSelector::Pinned(result.build.id.clone()))
        .unwrap();
    assert_eq!(
        loaded.capabilities.fingerprint_support,
        FingerprintSupport::Unverified
    );
    reopened.runtime.verify_build_integrity(&loaded).unwrap();
    fs::write(
        loaded.executable_path.parent().unwrap().join("chrome.dll"),
        b"changed DLL",
    )
    .unwrap();
    assert_eq!(
        reopened
            .runtime
            .verify_build_integrity(&loaded)
            .unwrap_err()
            .kind,
        PortErrorKind::RuntimeUntrusted
    );
    let third_runtime =
        Arc::new(ManagedChromiumRuntime::new(root.join("managed"), root.join("profiles")).unwrap());
    let third = OfficialRuntimeInstaller::new(root.join("managed"), third_runtime).unwrap();
    assert!(!third.load_with(&verifier()).unwrap().is_empty());
    assert!(third.runtime.installed_builds().unwrap().is_empty());
    assert!(
        root.join("managed").join(result.build.id.as_str()).exists(),
        "bad packages are preserved for inspection"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn signature_rejection_and_cancel_before_publication_never_register_a_runtime() {
    for cancel in [false, true] {
        let (installer, root) = fixture("rejection");
        let check = FakeVerifier {
            reject: !cancel,
            cancel: cancel.then_some(&installer),
        };
        let failure = installer
            .install_with(&FakeSource::default(), &check)
            .unwrap_err();
        assert_eq!(
            failure.kind,
            if cancel {
                PortErrorKind::Cancelled
            } else {
                PortErrorKind::RuntimeUntrusted
            }
        );
        assert!(installer.runtime.installed_builds().unwrap().is_empty());
        assert_eq!(fs::read_dir(&installer.root).unwrap().count(), 0);
        assert!(!installer.status().can_cancel);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn cancelling_download_and_shutdown_wait_for_owned_files_and_reject_duplicate_installation() {
    let (installer, root) = fixture("cancel");
    let (started, ready) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let source = FakeSource {
        gate: Some((started, Mutex::new(wait))),
        ..Default::default()
    };
    std::thread::scope(|scope| {
        let work = scope.spawn(|| installer.install_with(&source, &verifier()));
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(installer.status().phase, "downloading");
        assert_eq!(
            installer
                .install_with(&FakeSource::default(), &verifier())
                .unwrap_err()
                .kind,
            PortErrorKind::AlreadyExists
        );
        installer.cancel().unwrap();
        assert!(installer.status().running);
        let (finished, done) = mpsc::channel();
        let control = &installer;
        let shutdown = scope.spawn(move || {
            control.shutdown();
            finished.send(()).unwrap();
        });
        assert!(done.recv_timeout(Duration::from_millis(20)).is_err());
        resume.send(()).unwrap();
        assert_eq!(
            work.join().unwrap().unwrap_err().kind,
            PortErrorKind::Cancelled
        );
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        shutdown.join().unwrap();
    });
    assert!(installer
        .install_with(&FakeSource::default(), &verifier())
        .is_err());
    assert_eq!(fs::read_dir(&installer.root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_zip_paths_duplicates_links_and_missing_files_are_rejected() {
    for path in [
        "chrome-win64/../outside",
        "chrome-win64/C:/evil",
        "chrome-win64/con.txt",
        "chrome-win64/a:stream",
        "chrome-win64/a\\evil",
        "/chrome-win64/absolute",
        "chrome-win64/dir./file",
        "chrome-win64/dir /file",
        "chrome-win64/trailing ",
        "chrome-win64/ leading",
        "chrome-win64/CON .txt",
        "other/chrome.exe",
    ] {
        let (installer, root) = fixture("zip-path");
        let source = FakeSource {
            zip: archive(&[(path, b"untrusted")]),
            gate: None,
        };
        assert!(installer.install_with(&source, &verifier()).is_err());
        assert!(installer.runtime.installed_builds().unwrap().is_empty());
        assert!(!root.join("outside").exists());
        fs::remove_dir_all(root).unwrap();
    }
    for entries in [
        vec![
            ("chrome-win64/a", b"one".as_slice()),
            ("chrome-win64/A", b"two".as_slice()),
        ],
        vec![("chrome-win64/chrome.exe", b"only executable".as_slice())],
        vec![
            ("chrome-win64/First Run", b"one".as_slice()),
            ("chrome-win64/first run", b"two".as_slice()),
        ],
    ] {
        let (installer, root) = fixture("zip-reject");
        assert!(installer
            .install_with(
                &FakeSource {
                    zip: archive(&entries),
                    gate: None
                },
                &verifier()
            )
            .is_err());
        assert!(installer.runtime.installed_builds().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    let (installer, root) = fixture("link");
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink(
            "chrome-win64/link",
            "../../outside",
            SimpleFileOptions::default(),
        )
        .unwrap();
    let zip = writer.finish().unwrap().into_inner();
    assert!(installer
        .install_with(&FakeSource { zip, gate: None }, &verifier())
        .is_err());
    assert!(!root.join("outside").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn staging_cleanup_refuses_links_and_preserves_unknown_user_files() {
    let (installer, root) = fixture("staging");
    let staging = installer.allocate_staging().unwrap();
    fs::write(staging.join("notes"), b"keep").unwrap();
    assert!(installer.remove_staging(&staging).is_err());
    assert_eq!(fs::read(staging.join("notes")).unwrap(), b"keep");
    fs::remove_file(staging.join("notes")).unwrap();
    fs::create_dir(staging.join("package")).unwrap();
    fs::write(staging.join("package/notes"), b"keep package notes").unwrap();
    assert!(installer.remove_staging(&staging).is_err());
    assert_eq!(
        fs::read(staging.join("package/notes")).unwrap(),
        b"keep package notes"
    );
    fs::remove_file(staging.join("package/notes")).unwrap();
    fs::remove_dir(staging.join("package")).unwrap();
    let external = root.join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("keep"), b"keep").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&external, staging.join("package")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&external, staging.join("package")).unwrap();
    assert!(installer.remove_staging(&staging).is_err());
    assert_eq!(fs::read(external.join("keep")).unwrap(), b"keep");
    #[cfg(unix)]
    fs::remove_file(staging.join("package")).unwrap();
    #[cfg(windows)]
    fs::remove_dir(staging.join("package")).unwrap();
    installer.remove_staging(&staging).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn installation_preserves_system_channel_and_refuses_catalog_downgrades() {
    for provider in ["Google Chrome", PROVIDER] {
        let (installer, root) = fixture("channels");
        let executable = root.join("system/chrome.exe");
        fs::write(&executable, b"system runtime").unwrap();
        let build = BrowserBuild {
            id: BrowserBuildId::new("system-runtime").unwrap(),
            provider: provider.into(),
            version: "157.0.1.2".into(),
            channel: BrowserChannel::Stable,
            executable_path: executable,
            sha256: "0".repeat(64),
            publisher: "Google LLC".into(),
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities {
                supports_language: true,
                supports_proxy: true,
                supports_webrtc_policy: true,
                supports_devtools_pipe: true,
                fingerprint_support: FingerprintSupport::Unverified,
            },
            installed_at_ms: 1,
        };
        installer
            .runtime
            .register_build_with_current_digest(build.clone(), true)
            .unwrap();
        let result = installer.install_with(&FakeSource::default(), &verifier());
        if provider == PROVIDER {
            assert_eq!(result.unwrap_err().kind, PortErrorKind::RuntimeUntrusted);
        } else {
            assert!(result.is_ok());
        }
        assert_eq!(
            installer
                .runtime
                .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
                .unwrap()
                .id,
            build.id
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore = "requires Windows; downloads official Chrome for Testing and measures two real temporary profiles"]
fn official_windows_install_and_probe_smoke() {
    let artifact =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fingerprint-measurement.json");
    let admission = artifact.with_file_name("runtime-acceptance.json");
    for path in [&artifact, &admission] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot remove earlier measurement report: {error}"),
        }
    }
    let (installer, root) = fixture("official-smoke");
    let result = installer.install_stable();
    // Retain a negative admission result too. No API observations are invented
    // when the browser was rejected before it could be launched.
    let outcome = match &result {
        Ok(result) => {
            serde_json::json!({"runtimeAccepted":true,"runtimeVersion":result.build.version,"runtimeSha256":result.build.sha256,"fingerprintSupport":format!("{:?}",result.build.capabilities.fingerprint_support)})
        }
        Err(error) => {
            serde_json::json!({"runtimeAccepted":false,"errorKind":format!("{:?}",error.kind),"error":error.message})
        }
    };
    fs::create_dir_all(admission.parent().unwrap()).unwrap();
    fs::write(&admission,serde_json::to_vec_pretty(&serde_json::json!({"schemaVersion":1,"stage":"installation_and_basic_probe","measuredAtMs":now_ms(),"commit":std::env::var("GITHUB_SHA").ok(),"measurementPerformed":false,"outcome":outcome})).unwrap()).unwrap();
    let result =
        result.expect("official download, signature, version, registration and probe must succeed");
    assert_eq!(
        result.build.capabilities.fingerprint_support,
        FingerprintSupport::Supported,
        "{}",
        result.detail
    );
    let repeat = installer
        .runtime
        .probe_fingerprint_support(&result.build.id)
        .unwrap();
    assert_eq!(
        repeat.build.capabilities.fingerprint_support,
        FingerprintSupport::Supported,
        "{}",
        repeat.detail
    );
    let report = brows_application::FingerprintMeasurement::measure_pair(
        &crate::LocalFingerprintMeasurement::new(installer.runtime.clone()),
        &result.build.id,
    )
    .expect("real two-profile observation must complete without configuration fallbacks");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert_eq!(report.samples.len(), 2);
    assert!(report
        .samples
        .iter()
        .all(|sample| sample.observations.len() == 2));
    assert!(
        report.covered_checks_passed,
        "covered observations failed; see {}",
        artifact.display()
    );
    eprintln!(
        "Official runtime {} / {}: {}",
        result.build.version, result.build.publisher, result.detail
    );
    assert!(!installer.status().running);
    installer.shutdown();
    drop(installer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancellation_and_install_publication_have_one_atomic_admission_boundary() {
    let (installer, root) = fixture("commit-race");
    for _ in 0..40 {
        let lease = installer.begin().unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let work = scope.spawn(|| {
                barrier.wait();
                installer.commit().is_ok()
            });
            barrier.wait();
            let cancelled = installer.cancel().is_ok();
            assert_ne!(cancelled, work.join().unwrap());
        });
        drop(lease);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn newer_installs_advance_the_cft_channel_and_preserve_pinned_older_versions_after_restart() {
    struct NextSource;
    impl Source for NextSource {
        fn fetch(
            &self,
            url: &str,
            output: &mut dyn Write,
            limit: u64,
            control: &OfficialRuntimeInstaller,
        ) -> PortResult<String> {
            assert!(allowed_url(url));
            control.checkpoint()?;
            let bytes = if url == CATALOG {
                metadata("157.0.1.2")
            } else {
                package()
            };
            assert!((bytes.len() as u64) < limit);
            output.write_all(&bytes).unwrap();
            Ok(format!("{:x}", Sha256::digest(bytes)))
        }
    }
    let (installer, root) = fixture("versions");
    let first = installer
        .install_with(&FakeSource::default(), &verifier())
        .unwrap();
    let second = installer.install_with(&NextSource, &verifier()).unwrap();
    assert_eq!(
        installer
            .runtime
            .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
            .unwrap()
            .id,
        second.build.id
    );
    assert_eq!(
        installer
            .runtime
            .resolve_build(&BrowserBuildSelector::Pinned(first.build.id.clone()))
            .unwrap()
            .version,
        VERSION
    );
    let runtime =
        Arc::new(ManagedChromiumRuntime::new(root.join("managed"), root.join("profiles")).unwrap());
    let reopened = OfficialRuntimeInstaller::new(root.join("managed"), runtime).unwrap();
    assert!(reopened.load_with(&verifier()).unwrap().is_empty());
    assert_eq!(
        reopened
            .runtime
            .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
            .unwrap()
            .id,
        second.build.id
    );
    assert!(reopened
        .runtime
        .resolve_build(&BrowserBuildSelector::Pinned(first.build.id))
        .is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn refusing_a_busy_reprobe_never_reports_old_readiness_as_a_new_probe_success() {
    let (installer, root) = fixture("busy-probe");
    let installed = installer
        .install_with(&FakeSource::default(), &verifier())
        .unwrap();
    installer
        .runtime
        .set_fingerprint_support(&installed.build.id, FingerprintSupport::Supported)
        .unwrap();
    let first = fingerprint_probe::ProbeSlot::acquire(
        &installer.runtime.probing,
        &BrowserBuildId::new("other-one").unwrap(),
    )
    .unwrap();
    let second = fingerprint_probe::ProbeSlot::acquire(
        &installer.runtime.probing,
        &BrowserBuildId::new("other-two").unwrap(),
    )
    .unwrap();
    assert_eq!(
        installer
            .install_with(&FakeSource::default(), &verifier())
            .unwrap_err()
            .kind,
        PortErrorKind::AlreadyExists
    );
    assert_eq!(installer.status().phase, "failed");
    drop(first);
    drop(second);
    fs::remove_dir_all(root).unwrap();
}
