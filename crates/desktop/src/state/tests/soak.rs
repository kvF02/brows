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
        if Instant::now() >= deadline {
            panic!("timed out: {description}");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupTiming {
    launch_returned_ms: u128,
    first_document_observed_ms: Option<u128>,
    first_heartbeat_observed_ms: Option<u128>,
}

fn observe_startup(fixture: &ControlFixture, timings: &mut [StartupTiming], elapsed_ms: u128) {
    let proofs = fixture.proofs.lock().unwrap();
    for (index, timing) in timings.iter_mut().enumerate() {
        for (kind, first) in [
            ("document", &mut timing.first_document_observed_ms),
            ("heartbeat", &mut timing.first_heartbeat_observed_ms),
        ] {
            if first.is_none() {
                let key = format!("/soak/{index}/{kind}");
                if proofs.iter().any(|(path, _, _)| path == &key) {
                    *first = Some(elapsed_ms);
                }
            }
        }
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
    let startup_started = Instant::now();
    // Leave the 180-second supervisor time to capture a live failure snapshot.
    let startup_deadline = startup_started + Duration::from_secs(150);
    let data = root.join("data");
    fs::create_dir(&data).expect("soak profile directory must be fresh");
    let fixture = ControlFixture::start(false);
    let state = AppState::production(data.clone()).unwrap();
    let mut timings = Vec::new();
    let (sessions, profiles) = start_capacity_profiles(
        &state,
        |index| format!("{}/soak/{index}", fixture.url),
        |sessions| {
            timings.push(StartupTiming {
                launch_returned_ms: startup_started.elapsed().as_millis(),
                ..StartupTiming::default()
            });
            let next_batch = if matches!(sessions.len(), 2 | 4 | 6 | 8) {
                (Instant::now() + Duration::from_secs(2)).min(startup_deadline)
            } else {
                Instant::now()
            };
            loop {
                observe_startup(
                    &fixture,
                    &mut timings,
                    startup_started.elapsed().as_millis(),
                );
                if Instant::now() >= next_batch {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        },
    );
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
    let heartbeat_deadline = (Instant::now() + Duration::from_secs(60)).min(startup_deadline);
    let passed = loop {
        observe_startup(
            &fixture,
            &mut timings,
            startup_started.elapsed().as_millis(),
        );
        if timings
            .iter()
            .all(|timing| timing.first_document_observed_ms.is_some())
            && ids.iter().enumerate().all(|(index, id)| {
                page_hits(&fixture, index) > 0 && state.browsers.is_running(id) == Ok(true)
            })
        {
            break true;
        }
        if Instant::now() >= heartbeat_deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let counts = fixture.proofs.lock().unwrap().clone();
    let ready_profiles: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let count = |kind| {
                let key = format!("/soak/{index}/{kind}");
                counts
                    .iter()
                    .find(|(path, _, _)| path == &key)
                    .and_then(|(_, _, value)| value.as_u64())
                    .unwrap_or(0)
            };
            serde_json::json!({"index":index,"profileId":profiles[index],
            "documentRequests":count("document"),"heartbeats":count("heartbeat"),
            "processRunning":state.browsers.is_running(id).ok(),"timing":timings[index]})
        })
        .collect();
    let report =
        serde_json::json!({"assertionsPassed":passed,"complete":false,"profiles":ready_profiles});
    write_marker(
        root,
        "startup-heartbeats.json",
        report.to_string().as_bytes(),
    );
    if !passed {
        write_marker(root, "startup-failed", b"");
        let capture_deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("startup-captured").is_file() && Instant::now() < capture_deadline {
            thread::sleep(Duration::from_millis(100));
        }
        panic!("not all ten browsers sent a first heartbeat while running");
    }
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
    let duration = || {
        let seconds: u64 = env::var("BROWS_SOAK_SECONDS").unwrap().parse().unwrap();
        assert!((60..=14_400).contains(&seconds));
        Duration::from_secs(seconds)
    };
    match (
        env::var("BROWS_SOAK_PERSONA").as_deref(),
        env::var("BROWS_SOAK_PHASE").as_deref(),
    ) {
        (Ok("SystemNative"), Ok("exercise")) => exercise(&root, duration()),
        (Ok("SystemNative"), Ok("reopen")) => reopen(&root),
        (Ok("SeedableNoise"), Ok("exercise")) => seeded::exercise(&root, duration()),
        (Ok("SeedableNoise"), Ok("reopen")) => seeded::reopen(&root),
        _ => panic!("unknown soak persona or phase"),
    }
}
