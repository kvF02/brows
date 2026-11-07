//! Production composition exercised by the Windows soak supervisor.
use super::{draft, start_capacity_profiles, started, AppState, ControlFixture};
use brows_contracts::{
    ProfileIdRequest, ProfileListRequest, ProfileUpdateRequest, SessionLaunchRequest,
    SessionStateDto,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn await_condition(seconds: u64, description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {description}");
        thread::sleep(Duration::from_millis(250));
    }
}

fn page_hits(fixture: &ControlFixture, index: usize) -> u64 {
    let key = format!("/soak/{index}/heartbeat");
    fixture
        .proofs
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _, _)| path == &key)
        .and_then(|(_, _, count)| count.as_u64())
        .unwrap_or(0)
}

fn exercise(root: &Path, duration: Duration) {
    let data = root.join("data");
    fs::create_dir(&data).expect("soak profile directory must be fresh");
    let fixture = ControlFixture::start(false);
    let state = AppState::production(data.clone()).unwrap();
    let (sessions, profiles) =
        start_capacity_profiles(&state, |index| format!("{}/soak/{index}", fixture.url));
    let edge = state
        .browsers
        .list_builds()
        .unwrap()
        .into_iter()
        .find(|build| build.provider == "Microsoft Edge")
        .unwrap();
    let ids: Vec<brows_domain::SessionId> = sessions
        .iter()
        .map(|session| session.id.clone().try_into().unwrap())
        .collect();
    await_condition(60, "first heartbeat from every profile", || {
        (0..10).all(|index| page_hits(&fixture, index) > 0)
    });
    let initial_hits: [u64; 10] = std::array::from_fn(|index| page_hits(&fixture, index));
    let started_at = Instant::now();
    let mut activity = [(0, started_at); 10];
    let ready = serde_json::json!({
        "runtimeVersion": edge.version,
        "edgeExecutable": edge.executable_path,
        "profileRoot": data.join("profiles").canonicalize().unwrap(),
        "victimPage": format!("{}/soak/0", fixture.url),
        "profileId": profiles[1],
    });
    fs::write(root.join("ready.tmp"), serde_json::to_vec(&ready).unwrap()).unwrap();
    fs::rename(root.join("ready.tmp"), root.join("steady-ready.json")).unwrap();
    loop {
        state.sessions.reconcile_exits().unwrap();
        for (index, (count, last_seen)) in activity.iter_mut().enumerate() {
            assert!(
                state.browsers.is_running(&ids[index]).unwrap(),
                "browser {index} exited during the steady phase"
            );
            let hits = page_hits(&fixture, index);
            if hits > *count {
                *count = hits;
                *last_seen = Instant::now();
            }
            // Background Chromium pages can batch timers to once per minute.
            assert!(
                last_seen.elapsed() < Duration::from_secs(150),
                "profile {index} stopped sending page heartbeats"
            );
        }
        let current = state.sessions.list().unwrap();
        assert_eq!(
            current
                .iter()
                .filter(|session| session.state == SessionStateDto::Running)
                .count(),
            10
        );
        assert!(current.iter().any(|session| {
            session.profile_id == profiles[10] && session.state == SessionStateDto::Queued
        }));
        if started_at.elapsed() >= duration {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    assert!(initial_hits
        .iter()
        .enumerate()
        .all(|(index, initial)| page_hits(&fixture, index) > *initial));
    fs::write(
        root.join("elapsed.tmp"),
        started_at.elapsed().as_secs().to_string(),
    )
    .unwrap();
    fs::rename(root.join("elapsed.tmp"), root.join("browser-crash-ready")).unwrap();
    await_condition(90, "browser crash and queue promotion", || {
        state.sessions.reconcile_exits().unwrap();
        let current = state.sessions.list().unwrap();
        let failed = current.iter().any(|session| {
            session.id == sessions[0].id
                && session.state == SessionStateDto::Failed
                && session.failure_code.as_deref() == Some("BROWSER_EXITED")
        });
        let promoted = current.iter().any(|session| {
            session.profile_id == profiles[10] && session.state == SessionStateDto::Running
        });
        failed && promoted && page_hits(&fixture, 10) > 0
    });
    assert!(ids[1..]
        .iter()
        .all(|id| state.browsers.is_running(id).unwrap()));
    fs::File::create(root.join("host-crash-ready")).unwrap();
    // The supervisor terminates this process while it still owns ten jobs.
    thread::sleep(Duration::from_secs(90));
    panic!("the soak supervisor did not terminate the host");
}

fn reopen(root: &Path) {
    let fixture = ControlFixture::start(false);
    let state = AppState::production(root.join("data")).unwrap();
    assert_eq!(
        state
            .profiles
            .list(ProfileListRequest::default())
            .unwrap()
            .len(),
        11
    );
    assert!(state.sessions.list().unwrap().is_empty());
    let ready: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("steady-ready.json")).unwrap()).unwrap();
    let profile_id = serde_json::from_value(ready["profileId"].clone()).unwrap();
    let saved = state.profiles.get(ProfileIdRequest { profile_id }).unwrap();
    assert_eq!(saved.name, "capacity-1");
    let mut profile = draft(&saved.name);
    profile.browser = saved.browser;
    profile.startup_urls = vec![format!("{}/soak/1", fixture.url)];
    let saved = state
        .profiles
        .update(ProfileUpdateRequest {
            profile_id: saved.id,
            profile,
            expected_revision: saved.revision,
        })
        .unwrap();
    let relaunched = started(
        state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: saved.id,
            })
            .unwrap(),
    );
    await_condition(60, "heartbeat from the reopened profile", || {
        page_hits(&fixture, 1) > 0
    });
    crate::cleanup_sessions(&state);
    assert!(!state
        .browsers
        .is_running(&relaunched.id.try_into().unwrap())
        .unwrap());
    fs::File::create(root.join("reopen-verified")).unwrap();
}

#[test]
#[ignore = "requires the Windows soak supervisor and a signed installed Edge"]
fn desktop_soak_host() {
    let root = PathBuf::from(env::var_os("BROWS_SOAK_ROOT").expect("missing soak fixture root"));
    assert!(root.join("soak-fixture").is_file());
    match env::var("BROWS_SOAK_PHASE").as_deref() {
        Ok("exercise") => {
            let seconds: u64 = env::var("BROWS_SOAK_SECONDS").unwrap().parse().unwrap();
            assert!((60..=14_400).contains(&seconds));
            exercise(&root, Duration::from_secs(seconds));
        }
        Ok("reopen") => reopen(&root),
        _ => panic!("unknown soak phase"),
    }
}
