//! Production composition exercised by the Windows soak supervisor.
use super::{draft, start_capacity_profiles, started, AppState, ControlFixture};
use brows_contracts::{
    ProfileIdDto, ProfileIdRequest, ProfileListRequest, ProfileUpdateRequest, SessionDto,
    SessionLaunchRequest, SessionStateDto,
};
use brows_domain::SessionId;
use std::{
    env, fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod seeded;

fn await_condition(seconds: u64, description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {description}");
        thread::sleep(Duration::from_millis(250));
    }
}

fn page_hits(fixture: &ControlFixture, index: usize) -> u64 {
    let key = format!("/soak/{index}/heartbeat");
    let changed = format!("/soak/{index}/canvas-changed");
    let proofs = fixture.proofs.lock().unwrap();
    assert!(
        !proofs.iter().any(|(path, _, _)| path == &changed),
        "profile {index} returned different Canvas pixels during the run"
    );
    proofs
        .iter()
        .find(|(path, _, _)| path == &key)
        .and_then(|(_, _, count)| count.as_u64())
        .unwrap_or(0)
}

fn verify_steady_load(
    state: &AppState,
    fixture: &ControlFixture,
    ids: &[SessionId],
    duration: Duration,
    queued_profile: &ProfileIdDto,
) -> u64 {
    let started_at = Instant::now();
    let initial_hits: Vec<_> = (0..ids.len())
        .map(|index| page_hits(fixture, index))
        .collect();
    let mut activity: Vec<_> = initial_hits
        .iter()
        .map(|&count| (count, started_at))
        .collect();
    loop {
        state.sessions.reconcile_exits().unwrap();
        for (index, (count, last_seen)) in activity.iter_mut().enumerate() {
            assert!(
                state.browsers.is_running(&ids[index]).unwrap(),
                "browser {index} exited during the steady phase"
            );
            let hits = page_hits(fixture, index);
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
            ids.len()
        );
        assert!(current.iter().any(|session| {
            &session.profile_id == queued_profile && session.state == SessionStateDto::Queued
        }));
        if started_at.elapsed() >= duration {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    assert!(initial_hits
        .iter()
        .enumerate()
        .all(|(index, initial)| page_hits(fixture, index) > *initial));
    started_at.elapsed().as_secs()
}

fn write_marker(root: &Path, name: &str, contents: &[u8]) {
    let temporary = root.join("marker.tmp");
    fs::write(&temporary, contents).unwrap();
    fs::rename(temporary, root.join(name)).unwrap();
}

fn await_promotion(
    state: &AppState,
    fixture: &ControlFixture,
    victim: &SessionDto,
    queued: &ProfileIdDto,
) {
    await_condition(90, "browser crash and queue promotion", || {
        state.sessions.reconcile_exits().unwrap();
        let current = state.sessions.list().unwrap();
        let failed = current.iter().any(|session| {
            session.id == victim.id
                && session.state == SessionStateDto::Failed
                && session.failure_code.as_deref() == Some("BROWSER_EXITED")
        });
        let promoted = current.iter().any(|session| {
            &session.profile_id == queued && session.state == SessionStateDto::Running
        });
        failed && promoted && page_hits(fixture, 10) > 0
    });
}

fn expect_host_crash(root: &Path, state: &AppState, survivors: &[SessionId]) {
    assert!(survivors
        .iter()
        .all(|id| state.browsers.is_running(id).unwrap()));
    write_marker(root, "host-crash-ready", b"");
    // The supervisor terminates this process while it still owns ten jobs.
    thread::sleep(Duration::from_secs(90));
    panic!("the soak supervisor did not terminate the host");
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
    let ids: Vec<SessionId> = sessions
        .iter()
        .map(|session| session.id.clone().try_into().unwrap())
        .collect();
    await_condition(60, "first heartbeat from every profile", || {
        (0..10).all(|index| page_hits(&fixture, index) > 0)
    });
    let ready = serde_json::json!({
        "runtimeVersion": edge.version,
        "edgeExecutable": edge.executable_path,
        "profileRoot": data.join("profiles").canonicalize().unwrap(),
        "victimPage": format!("{}/soak/0", fixture.url),
        "profileId": profiles[1],
    });
    write_marker(root, "steady-ready.json", ready.to_string().as_bytes());
    let elapsed = verify_steady_load(&state, &fixture, &ids, duration, &profiles[10]);
    write_marker(root, "browser-crash-ready", elapsed.to_string().as_bytes());
    await_promotion(&state, &fixture, &sessions[0], &profiles[10]);
    expect_host_crash(root, &state, &ids[1..]);
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
    write_marker(root, "reopen-verified", b"");
}

#[test]
#[ignore = "requires the Windows soak supervisor and signed Edge"]
fn desktop_soak_host() {
    let root = PathBuf::from(env::var_os("BROWS_SOAK_ROOT").expect("missing soak fixture root"));
    assert!(root.join("soak-fixture").is_file());
    let (exercise, reopen): (fn(&Path, Duration), fn(&Path)) =
        match env::var("BROWS_SOAK_PERSONA").as_deref() {
            Ok("SystemNative") => (exercise, reopen),
            Ok("SeedableNoise") => (seeded::exercise, seeded::reopen),
            _ => panic!("unknown soak persona"),
        };
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
