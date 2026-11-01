//! Opt-in acceptance through the production services and visible, signed Chrome.
//! UI clicks, ten-browser load and fingerprint spoofing are separate checks.
use crate::state::AppState;
use brows_application::SessionLaunchOutcome;
use brows_contracts::*;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
struct ObservationServer(Child);
impl Drop for ObservationServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn draft(name: &str, url: String) -> ProfileDraftDto {
    ProfileDraftDto {
        name: format!("Acceptance {name}"),
        tags: vec!["acceptance".into()],
        preset_id: PresetIdDto::new("system-native").unwrap(),
        preset_version: 1,
        proxy_id: None,
        browser: BrowserSelectorDto::Channel {
            channel: BrowserChannelDto::Stable,
        },
        persona: PersonaDto::default(),
        locale: LocaleDto {
            accept_languages: vec!["en-US".into(), "en".into()],
            timezone: None,
        },
        window: WindowDto {
            width: 1000,
            height: 700,
            start_maximized: false,
        },
        startup_urls: vec![url],
    }
}

fn launch(state: &AppState, profile: &ProfileDto) -> SessionDto {
    match state
        .sessions
        .launch(SessionLaunchRequest {
            profile_id: profile.id.clone(),
        })
        .unwrap()
    {
        SessionLaunchOutcome::Started { session, .. } => session,
        SessionLaunchOutcome::Queued { .. } => panic!("unexpected queued session"),
    }
}

fn running(state: &AppState, session: &SessionDto) -> bool {
    state
        .browsers
        .is_running(&session.id.clone().try_into().unwrap())
        .unwrap()
}

fn stop(state: &AppState, session: &SessionDto) {
    assert_eq!(
        state
            .sessions
            .stop(SessionStopRequest {
                session_id: session.id.clone(),
            })
            .unwrap()
            .state,
        SessionStateDto::Stopped
    );
    assert!(!running(state, session));
}

fn observe(root: &Path, run: &str, report: &mut Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let contents = fs::read_to_string(root.join("observations.jsonl")).unwrap_or_default();
        for line in contents.split_inclusive('\n').filter(|s| s.ends_with('\n')) {
            let value: Value = serde_json::from_str(line).expect("invalid browser observation");
            if value["runId"] == run {
                report["observations"]
                    .as_array_mut()
                    .unwrap()
                    .push(value.clone());
                assert!(value.get("error").is_none(), "browser error: {value}");
                assert!(value["userAgent"].as_str().unwrap().contains("Chrome/"));
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no browser observation for {run}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn storage(value: &Value, marker: &str, reopened: bool) {
    for kind in ["local", "cookie", "indexed"] {
        assert_eq!(value[format!("{kind}After")], marker, "{value}");
        let expected = if reopened { json!(marker) } else { Value::Null };
        assert_eq!(value[format!("{kind}Before")], expected, "{value}");
    }
    assert!(value["httpCookie"]
        .as_str()
        .unwrap()
        .split(';')
        .any(|v| v.trim() == format!("brows_native_acceptance={marker}")));
}

fn check(report: &mut Value, name: &str) {
    report["checks"].as_array_mut().unwrap().push(json!(name));
    eprintln!("[native-browser] passed: {name}");
}

fn exercise(root: &Path, report: &mut Value) {
    if !cfg!(windows) {
        panic!("this acceptance requires real Windows Chrome");
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut server = ObservationServer(
        Command::new("node")
            .arg(repo.join("tools/native-storage-server.mjs"))
            .arg(root)
            .stdin(Stdio::null())
            .spawn()
            .expect("cannot start local storage observation server"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("server.json").is_file() {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "observation server exited"
        );
        assert!(Instant::now() < deadline, "observation server not ready");
        thread::sleep(Duration::from_millis(25));
    }
    let server: Value =
        serde_json::from_slice(&fs::read(root.join("server.json")).unwrap()).unwrap();
    let url = |run: &str| {
        format!(
            "{}/{}/{run}",
            server["origin"].as_str().unwrap(),
            server["token"].as_str().unwrap()
        )
    };
    let data = root.join("manager-data");
    let state = AppState::production(data.clone()).expect("production startup failed");
    assert!(
        state.runtime_issue.is_none(),
        "runtime rejected: {:?}",
        state.runtime_issue
    );
    let builds = state.browsers.list_builds().unwrap();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0].publisher, "Google LLC");
    report["runtime"] = json!({"version":builds[0].version,"sha256":builds[0].sha256,"publisher":builds[0].publisher,"mode":"SystemNative"});
    check(report, "signed_system_chrome_admitted");
    state
        .settings
        .update(SettingsUpdateRequest {
            changes: SettingsPatchDto {
                max_concurrent_sessions: Some(2),
                ..Default::default()
            },
        })
        .unwrap();
    let profiles = ["A", "B", "C"].map(|label| {
        state
            .profiles
            .create(ProfileCreateRequest {
                profile: draft(label, url(&format!("{}0", label.to_lowercase()))),
            })
            .unwrap()
    });
    let a = launch(&state, &profiles[0]);
    let b = launch(&state, &profiles[1]);
    assert_eq!(
        state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: profiles[0].id.clone()
            })
            .unwrap_err()
            .code(),
        &ErrorCode::AlreadyExists
    );
    check(report, "duplicate_launch_rejected");
    let c = match state
        .sessions
        .launch(SessionLaunchRequest {
            profile_id: profiles[2].id.clone(),
        })
        .unwrap()
    {
        SessionLaunchOutcome::Queued {
            session, position, ..
        } => {
            assert_eq!(position, 1);
            session
        }
        SessionLaunchOutcome::Started { .. } => panic!("third browser exceeded capacity"),
    };
    assert!(!running(&state, &c));
    storage(&observe(root, "a0", report), "A", false);
    storage(&observe(root, "b0", report), "B", false);
    assert!(running(&state, &a) && running(&state, &b));
    check(report, "concurrent_cookie_localstorage_indexeddb_isolation");
    stop(&state, &a);
    storage(&observe(root, "c0", report), "C", false);
    assert!(running(&state, &b) && running(&state, &c));
    assert_eq!(
        state
            .sessions
            .list()
            .unwrap()
            .iter()
            .find(|s| s.id == c.id)
            .unwrap()
            .state,
        SessionStateDto::Running
    );
    check(report, "capacity_and_queue_promotion");
    let browsers = state.browsers.clone();
    drop(state); // Same AppState shutdown used when the manager closes.
    for session in [&a, &b, &c] {
        assert!(!browsers
            .is_running(&session.id.clone().try_into().unwrap())
            .unwrap());
    }
    drop(browsers);
    check(report, "manager_shutdown_stops_active_browsers");

    let state = AppState::production(data).unwrap();
    assert_eq!(state.settings.get().unwrap().max_concurrent_sessions, 2);
    assert_eq!(
        state
            .profiles
            .list(ProfileListRequest::default())
            .unwrap()
            .len(),
        3
    );
    assert!(state.sessions.list().unwrap().is_empty());
    let profiles = profiles
        .into_iter()
        .enumerate()
        .map(|(index, profile)| {
            state
                .profiles
                .update(ProfileUpdateRequest {
                    profile_id: profile.id,
                    expected_revision: profile.revision,
                    profile: draft(["A", "B", "C"][index], url(["a1", "b1", "c1"][index])),
                })
                .unwrap()
        })
        .collect::<Vec<_>>();
    check(report, "manager_restart_retains_profiles_and_capacity");
    let a = launch(&state, &profiles[0]);
    let b = launch(&state, &profiles[1]);
    storage(&observe(root, "a1", report), "A", true);
    storage(&observe(root, "b1", report), "B", true);
    assert!(running(&state, &a) && running(&state, &b));
    stop(&state, &a);
    let c = launch(&state, &profiles[2]);
    storage(&observe(root, "c1", report), "C", true);
    assert!(running(&state, &b) && running(&state, &c));
    check(report, "browser_restart_retains_all_three_storage_types");
    drop(state);
}

#[test]
#[ignore = "requires Windows, Node 24 and a signed system Chrome; opens real isolated browser windows"]
fn system_native_windows_acceptance() {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    fs::create_dir_all(&target).unwrap();
    let target = target.canonicalize().unwrap();
    let root = target.join(format!("native-browser-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let mut report = json!({"schemaVersion":1,"scope":"production_services_visible_system_chrome","commit":std::env::var("GITHUB_SHA").ok(),"passed":false,"checks":[],"observations":[],"notCovered":["frontend_clicks","ten_browser_load","fingerprint_independence"]});
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exercise(&root, &mut report);
        // Remove only this test's unique root. On Windows this also detects
        // browser database handles remaining open after shutdown.
        fs::remove_dir_all(&root).expect("browser test data remains locked after shutdown");
        check(&mut report, "temporary_browser_data_removed_after_shutdown");
    }));
    report["passed"] = json!(result.is_ok());
    if let Err(error) = &result {
        report["error"] = json!(error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or("acceptance panicked"));
    }
    fs::write(
        target.join("native-browser-acceptance.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
