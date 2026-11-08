//! Real signed Edge, a trusted HTTPS fixture, and the production backup commands.
use super::{draft, started, AppState, NativeAcceptanceRoot};
use brows_contracts::{
    BackupExportRequest, BackupImportRequest, BrowserSelectorDto, PersonaDto, ProfileCreateRequest,
    ProfileDraftDto, ProfileDto, ProfileIdRequest, ProfileUpdateRequest, SecretInput, SessionDto,
    SessionLaunchRequest, SessionStateDto, SessionStopRequest,
};
use serde_json::{json, Value};
use std::{
    env, fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Site {
    child: Child,
    directory: PathBuf,
    origin: String,
}
impl Site {
    fn start(root: &std::path::Path) -> Self {
        let directory = root.join("site");
        fs::create_dir_all(&directory).unwrap();
        let mut child = Command::new("node.exe")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tools/test_support/login_fixture.mjs"
            ))
            .arg(env::var_os("BROWS_LOGIN_TLS_PFX").expect("missing disposable TLS fixture"))
            .arg(&directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let ready = directory.join("ready.json");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.is_file() {
            assert!(child.try_wait().unwrap().is_none(), "login site exited");
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("login site did not start");
            }
            thread::sleep(Duration::from_millis(100));
        }
        let ready: Value = serde_json::from_slice(&fs::read(ready).unwrap()).unwrap();
        let origin = ready["origin"].as_str().unwrap().to_owned();
        assert!(origin.starts_with("https://localhost:"));
        Self {
            child,
            directory,
            origin,
        }
    }
    fn proofs(&self) -> Value {
        fs::read(self.directory.join("proofs.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(json!({}))
    }
    fn wait(&self, key: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let value = self.proofs()[key].clone();
            if !value.is_null() {
                return value;
            }
            assert!(Instant::now() < deadline, "no browser proof for {key}");
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn verify(&self, round: &str, account: &str, expected: Option<&str>) -> Value {
        let proof = self.wait(&format!("{round}/{account}"));
        println!("login fixture {round}/{account}: {proof}");
        assert_eq!(proof["error"], Value::Null);
        assert_eq!(proof["secureContext"], true);
        assert_eq!(proof["httpOnly"], true);
        for field in ["account", "local", "indexed", "cache"] {
            assert_eq!(proof[field], json!(expected), "{round}/{account}: {field}");
        }
        proof
    }
}
impl Drop for Site {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct OwnedState(AppState);
impl Drop for OwnedState {
    fn drop(&mut self) {
        crate::cleanup_sessions(&self.0);
    }
}

fn navigate(
    state: &AppState,
    site: &Site,
    profile: &ProfileDto,
    action: &str,
    round: &str,
    account: &str,
) -> (ProfileDto, SessionDto) {
    let profile = state
        .profiles
        .update(ProfileUpdateRequest {
            profile_id: profile.id.clone(),
            expected_revision: profile.revision,
            profile: ProfileDraftDto {
                name: profile.name.clone(),
                tags: profile.tags.clone(),
                preset_id: profile.preset_id.clone(),
                preset_version: profile.preset_version,
                proxy_id: profile.proxy_id.clone(),
                browser: profile.browser.clone(),
                persona: profile.persona.clone(),
                locale: profile.locale.clone(),
                window: profile.window,
                startup_urls: vec![format!("{}/{action}/{round}/{account}", site.origin)],
            },
        })
        .unwrap();
    let session = started(
        state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: profile.id.clone(),
            })
            .unwrap(),
    );
    (profile, session)
}
fn stop(state: &AppState, session: SessionDto) {
    state
        .sessions
        .stop(SessionStopRequest {
            session_id: session.id,
        })
        .unwrap();
}
fn storage_metadata(data: &std::path::Path, profile: &ProfileDto) -> Value {
    let journal =
        brows_adapters_profile_data::ProfileDataJournal::new(data.join("profiles"), None).unwrap();
    let id = profile.id.clone().try_into().unwrap();
    let output = Command::new("python.exe")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test_support/login_storage_metadata.py"
        ))
        .arg(journal.profile_dir(&id).join("userdata"))
        .output()
        .unwrap();
    assert!(output.status.success(), "cookie metadata fixture failed");
    serde_json::from_slice(&output.stdout).unwrap()
}
fn force_browser_crash(root: &std::path::Path) -> Value {
    let output = Command::new("pwsh.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/windows-login-crash.ps1"
        ))
        .arg("-OwnerId")
        .arg(std::process::id().to_string())
        .arg("-ProfileRoot")
        .arg(root.join("profiles").canonicalize().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "owned browser crash fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires hosted Windows, trusted localhost HTTPS, and signed Edge"]
fn login_persistence_acceptance() {
    assert_eq!(env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    let owned = NativeAcceptanceRoot::new("login");
    fs::create_dir_all(&owned.0).unwrap();
    fs::write(
        owned.0.join("login-fixture-owner.json"),
        json!({"ownerId":std::process::id()}).to_string(),
    )
    .unwrap();
    let site = Site::start(&owned.0);
    let data = owned.0.join("data");
    let state = OwnedState(AppState::production(data.clone()).unwrap());
    let edge = state
        .0
        .browsers
        .list_builds()
        .unwrap()
        .into_iter()
        .find(|b| b.provider == "Microsoft Edge")
        .unwrap();
    let managed = state
        .0
        .runtime_management
        .as_ref()
        .unwrap()
        .capture(edge.id.clone())
        .unwrap();
    let probed = state
        .0
        .browsers
        .probe_fingerprint_support(&managed.id)
        .unwrap();
    assert_eq!(
        probed.build.capabilities.fingerprint_support,
        brows_domain::FingerprintSupport::Supported
    );
    let mut evidence = Vec::new();
    let mut normal_closes = Vec::new();
    let report = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/login-persistence-acceptance.json");
    for seeded in [false, true] {
        let persona = if seeded { "seeded" } else { "native" };
        let mut profiles = Vec::new();
        for account in ["a", "b"] {
            let mut profile = draft(&format!("login-{persona}-{account}"));
            let build = if seeded { &managed } else { &edge };
            profile.browser = BrowserSelectorDto::Pinned {
                build_id: (&build.id).into(),
            };
            if seeded {
                profile.persona = PersonaDto::SeedableNoise {
                    seed: format!("login-{account}"),
                    platform: brows_contracts::PlatformDto::Windows,
                    hardware_concurrency: 4,
                    device_memory: brows_contracts::DeviceMemoryDto::Four,
                    touch_points: 0,
                    screen: brows_contracts::ScreenConfigDto {
                        width: 1920,
                        height: 1080,
                        color_depth: 24,
                        device_scale_factor_hundredths: 100,
                    },
                    webgl_vendor: String::new(),
                    webgl_renderer: String::new(),
                    spoof_canvas: true,
                    spoof_audio: false,
                };
            }
            let mut profile = state
                .0
                .profiles
                .create(ProfileCreateRequest { profile })
                .unwrap();
            for (action, phase, expected) in [
                ("check", "empty", None),
                ("enroll", "enroll", Some(account)),
                ("check", "reopen", Some(account)),
            ] {
                let round = format!("{persona}-{phase}");
                let (updated, session) =
                    navigate(&state.0, &site, &profile, action, &round, account);
                profile = updated;
                let proof = site.verify(&round, account, expected);
                stop(&state.0, session);
                let metadata = storage_metadata(&data, &profile);
                println!("login storage after {round}/{account}: {metadata}");
                // Pagehide proves an orderly document shutdown, independently of storage reads.
                normal_closes.push((
                    format!("closed/{round}/{account}"),
                    site.proofs()[format!("closed/{round}/{account}")].clone(),
                ));
                evidence
                    .push(json!({"persona":persona,"phase":phase,"profile":account,"proof":proof,"storageMetadata":metadata}));
                fs::write(
                    &report,
                    json!({"assertionsPassed":false,"samples":evidence}).to_string(),
                )
                .unwrap();
            }
            profiles.push(profile);
        }
        // Fresh profile b observed the same origin while a's authenticated state existed.
        let (a, crashed) = navigate(
            &state.0,
            &site,
            &profiles[0],
            "check",
            &format!("{persona}-before-crash"),
            "a",
        );
        site.verify(&format!("{persona}-before-crash"), "a", Some("a"));
        let crash = force_browser_crash(&data);
        state.0.sessions.reconcile_exits().unwrap();
        assert!(state
            .0
            .sessions
            .list()
            .unwrap()
            .iter()
            .any(|s| s.id == crashed.id && s.state == SessionStateDto::Failed));
        let (a, session) = navigate(
            &state.0,
            &site,
            &a,
            "check",
            &format!("{persona}-after-crash"),
            "a",
        );
        let proof = site.verify(&format!("{persona}-after-crash"), "a", Some("a"));
        stop(&state.0, session);
        evidence.push(
            json!({"persona":persona,"phase":"browser-crash-reopen","proof":proof,"crash":crash}),
        );
        let backups = state.0.backups.as_ref().unwrap();
        let name = format!("{persona}-login.browsbak");
        let password = || SecretInput::new("disposable login fixture password").unwrap();
        crate::commands::export_backup(
            &state.0.profiles,
            &state.0.sessions,
            backups,
            BackupExportRequest {
                profile_id: a.id.clone(),
                destination_name: name.clone(),
                password: password(),
                include_proxy_credential: false,
            },
        )
        .unwrap();
        assert!(crate::commands::import_backup(
            &state.0.profiles,
            backups,
            BackupImportRequest {
                source_name: name.clone(),
                password: SecretInput::new("wrong disposable password").unwrap(),
            }
        )
        .is_err());
        let restored = crate::commands::import_backup(
            &state.0.profiles,
            backups,
            BackupImportRequest {
                source_name: name,
                password: password(),
            },
        )
        .unwrap()
        .profile;
        assert_ne!(restored.id, a.id);
        let (restored, session) = navigate(
            &state.0,
            &site,
            &restored,
            "check",
            &format!("{persona}-restore"),
            "a",
        );
        let proof = site.verify(&format!("{persona}-restore"), "a", Some("a"));
        stop(&state.0, session);
        evidence.push(
            json!({"persona":persona,"phase":"encrypted-backup-restore-new-id","proof":proof}),
        );
        profiles[0] = a;
        profiles.push(restored);
        for (index, p) in profiles.iter().enumerate() {
            let account = if index == 1 { "b" } else { "a" };
            let round = format!("{persona}-isolation-final-{index}");
            let (_, session) = navigate(&state.0, &site, p, "check", &round, account);
            site.verify(&round, account, Some(account));
            stop(&state.0, session);
        }
    }
    let saved = state
        .0
        .profiles
        .list(brows_contracts::ProfileListRequest::default())
        .unwrap();
    drop(state);
    let reopened = OwnedState(AppState::production(data).unwrap());
    // Capability observations expire with the host. Exercise the same fresh
    // probe required by Settings before reopening SeedableNoise environments.
    let admitted = reopened
        .0
        .browsers
        .probe_fingerprint_support(&managed.id)
        .unwrap();
    assert_eq!(
        admitted.build.capabilities.fingerprint_support,
        brows_domain::FingerprintSupport::Supported
    );
    for p in saved {
        let p = reopened
            .0
            .profiles
            .get(ProfileIdRequest { profile_id: p.id })
            .unwrap();
        let account = if p.name.ends_with("-b") { "b" } else { "a" };
        let round = format!("app-reopen-{}", p.id.as_str());
        let (_, session) = navigate(&reopened.0, &site, &p, "check", &round, account);
        site.verify(&round, account, Some(account));
        stop(&reopened.0, session);
    }
    let orderly = normal_closes
        .iter()
        .all(|(key, _)| site.proofs()[key] == true);
    fs::write(&report,json!({"schemaVersion":1,"assertionsPassed":orderly,"commit":env::var("GITHUB_SHA").ok(),"runtimeVersion":edge.version,
        "samples":evidence,"normalDocumentShutdown":orderly,"notChecked":["external websites and accounts","cross-device or cross-Windows-user restore","physical power loss","Windows 10/11 clients"]}).to_string()).unwrap();
    assert!(
        orderly,
        "normal Stop must close documents before terminating the browser job"
    );
}
