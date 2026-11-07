use super::super::{draft, started, AppState, ControlFixture, SessionLaunchOutcome};
use super::{
    await_condition, await_promotion, expect_host_crash, page_hits, verify_steady_load,
    write_marker,
};
use brows_contracts::{
    BrowserBuildIdDto, BrowserSelectorDto, DeviceMemoryDto, PersonaDto, PlatformDto,
    ProfileCreateRequest, ProfileDraftDto, ProfileDto, ProfileIdRequest, ProfileListRequest,
    ProfileUpdateRequest, ScreenConfigDto, SessionDto, SessionLaunchRequest, SessionStateDto,
    SessionStopRequest,
};
use brows_domain::{BrowserBuild, BrowserBuildId, FingerprintSupport, RuntimeTrust, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, ops::Range, path::Path, time::Duration};

const PROFILES: usize = 10;
const CONTEXTS: [&str; 2] = ["page", "worker"];
const FIELDS: [&str; 10] = [
    "userAgent",
    "platform",
    "languages",
    "hardwareConcurrency",
    "deviceMemory",
    "timezone",
    "offsetWinter",
    "offsetSummer",
    "hints",
    "canvas",
];

#[derive(Debug, Serialize, Deserialize)]
struct RestartEvidence {
    runtime_id: BrowserBuildIdDto,
    runtime_digest: String,
    profiles: Vec<ProfileDto>,
    signals: Vec<Value>,
}

fn admitted_edge(state: &AppState, id: &BrowserBuildId) -> BrowserBuild {
    let result = state.browsers.probe_fingerprint_support(id).unwrap();
    assert_eq!(result.build.trust, RuntimeTrust::Verified);
    assert_eq!(
        result.build.capabilities.fingerprint_support,
        FingerprintSupport::Supported,
        "{}",
        result.detail
    );
    result.build
}

fn launch_all(state: &AppState, profiles: &[ProfileDto]) -> Vec<SessionDto> {
    profiles
        .iter()
        .map(|profile| {
            started(
                state
                    .sessions
                    .launch(SessionLaunchRequest {
                        profile_id: profile.id.clone(),
                    })
                    .unwrap_or_else(|error| panic!("{} failed to start: {error:?}", profile.name)),
            )
        })
        .collect()
}

fn stop_all(state: &AppState, sessions: Vec<SessionDto>) {
    for session in sessions {
        let id: SessionId = session.id.clone().try_into().unwrap();
        let stopped = state
            .sessions
            .stop(SessionStopRequest {
                session_id: session.id,
            })
            .unwrap();
        assert_eq!(stopped.state, SessionStateDto::Stopped);
        assert!(!state.browsers.is_running(&id).unwrap());
    }
}

fn capture_matrix(fixture: &ControlFixture, indices: Range<usize>) -> Vec<Value> {
    let mut captures = None;
    await_condition(90, "first-script page and Worker proofs", || {
        let proofs = fixture.proofs.lock().unwrap();
        captures = indices
            .clone()
            .map(|index| {
                let sample = |kind| {
                    let expected = format!("/seeded-soak/{index}/{kind}/proof");
                    proofs
                        .iter()
                        .find(|(path, _, value)| path == &expected && value["kind"] == kind)
                        .map(|(_, _, value)| value.clone())
                };
                Some([sample(CONTEXTS[0])?, sample(CONTEXTS[1])?])
            })
            .collect::<Option<Vec<_>>>();
        captures.is_some()
    });

    let unmodified_canvas = "a42367ff".repeat(16 * 16);
    indices
        .zip(captures.unwrap())
        .map(|(index, [page, worker])| {
            for field in FIELDS {
                assert_eq!(
                    page[field], worker[field],
                    "profile {index} disagreed across page and Worker on {field}"
                );
            }
            assert!(page["userAgent"].as_str().is_some_and(|ua| !ua.is_empty()));
            assert_eq!(page["platform"], "Win32");
            assert_eq!(page["hints"]["platform"], "Windows");
            assert_eq!(page["languages"], json!(["en-US", "en"]));
            assert_eq!(page["hardwareConcurrency"], 3);
            assert_eq!(page["deviceMemory"], 0.5);
            assert_eq!(page["timezone"], "Asia/Tokyo");
            assert_eq!(page["offsetWinter"], -540);
            assert_eq!(page["offsetSummer"], -540);
            let canvas = page["canvas"].as_str().unwrap();
            assert_eq!(canvas.len(), unmodified_canvas.len());
            assert_ne!(
                canvas, unmodified_canvas,
                "profile {index} returned unmodified Canvas"
            );
            assert_eq!(
                page["repeat"], page["canvas"],
                "profile {index} page repeat"
            );
            assert_eq!(
                worker["repeat"], page["canvas"],
                "profile {index} Worker repeat"
            );
            Value::Object(
                FIELDS
                    .iter()
                    .map(|&field| (field.to_owned(), page[field].clone()))
                    .collect(),
            )
        })
        .collect()
}

fn assert_distinct(matrix: &[Value]) {
    let distinct: BTreeSet<_> = matrix
        .iter()
        .map(|sample| sample["canvas"].as_str().unwrap())
        .collect();
    assert_eq!(
        distinct.len(),
        matrix.len(),
        "fixture seeds must produce distinct Canvas pixels"
    );
}

fn profile_draft(index: usize, fixture: &ControlFixture, edge: &BrowserBuild) -> ProfileDraftDto {
    let mut profile = draft(&format!("seeded-soak-{index}"));
    profile.browser = BrowserSelectorDto::Pinned {
        build_id: (&edge.id).into(),
    };
    profile.persona = PersonaDto::SeedableNoise {
        seed: format!("brows-seeded-soak-{index}"),
        platform: PlatformDto::Windows,
        hardware_concurrency: 3,
        device_memory: DeviceMemoryDto::Half,
        touch_points: 0,
        screen: ScreenConfigDto {
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
    profile.locale.accept_languages = vec!["en-US".to_owned(), "en".to_owned()];
    profile.locale.timezone = Some("Asia/Tokyo".to_owned());
    profile.startup_urls = vec![format!("{}/seeded-soak/{index}/page", fixture.url)];
    profile
}

pub(super) fn exercise(root: &Path, duration: Duration) {
    let data = root.join("data");
    fs::create_dir(&data).expect("soak profile directory must be fresh");
    let fixture = ControlFixture::start(false);
    let state = AppState::production(data.clone()).unwrap();
    let installed = state
        .browsers
        .list_builds()
        .unwrap()
        .into_iter()
        .find(|build| build.provider == "Microsoft Edge")
        .expect("signed Edge registered");
    let snapshot = state
        .runtime_management
        .as_ref()
        .unwrap()
        .capture(installed.id)
        .unwrap();
    let edge = admitted_edge(&state, &snapshot.id);
    println!("Managed Edge {} admitted", edge.version);
    let profiles: Vec<_> = (0..=PROFILES)
        .map(|index| {
            state
                .profiles
                .create(ProfileCreateRequest {
                    profile: profile_draft(index, &fixture, &edge),
                })
                .unwrap()
        })
        .collect();
    let mut sessions = launch_all(&state, &profiles[..1]);
    let profile_root = data.join("profiles").canonicalize().unwrap();
    let user_dirs: Vec<_> = fs::read_dir(&profile_root)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("userdata"))
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(
        user_dirs.len(),
        1,
        "the victim must own the only active user data directory"
    );
    sessions.extend(launch_all(&state, &profiles[1..PROFILES]));
    let queued = &profiles[PROFILES].id;
    let outcome = state
        .sessions
        .launch(SessionLaunchRequest {
            profile_id: queued.clone(),
        })
        .unwrap();
    assert!(matches!(
        outcome,
        SessionLaunchOutcome::Queued { position: 1, .. }
    ));
    let ids: Vec<SessionId> = sessions
        .iter()
        .map(|session| session.id.clone().try_into().unwrap())
        .collect();
    let mut signals = capture_matrix(&fixture, 0..PROFILES);
    assert_distinct(&signals);
    await_condition(60, "first heartbeat from every seeded profile", || {
        (0..PROFILES).all(|index| page_hits(&fixture, index) > 0)
    });
    let ready = json!({
        "runtimeVersion": edge.version,
        "runtimeId": edge.id.as_str(),
        "edgeExecutable": edge.executable_path,
        "profileRoot": profile_root,
        "victimUserData": user_dirs[0],
    });
    write_marker(root, "steady-ready.json", ready.to_string().as_bytes());
    let elapsed = verify_steady_load(&state, &fixture, &ids, duration, queued);
    write_marker(root, "browser-crash-ready", elapsed.to_string().as_bytes());
    await_promotion(&state, &fixture, &sessions[0], queued);
    signals.extend(capture_matrix(&fixture, PROFILES..PROFILES + 1));
    assert_distinct(&signals);
    let evidence = RestartEvidence {
        runtime_id: (&edge.id).into(),
        runtime_digest: edge.sha256,
        profiles,
        signals,
    };
    write_marker(
        root,
        "seeded-baseline.json",
        &serde_json::to_vec(&evidence).unwrap(),
    );
    expect_host_crash(root, &state, &ids[1..]);
}

pub(super) fn reopen(root: &Path) {
    let before: RestartEvidence =
        serde_json::from_slice(&fs::read(root.join("seeded-baseline.json")).unwrap()).unwrap();
    let fixture = ControlFixture::start(false);
    let state = AppState::production(root.join("data")).unwrap();
    assert!(state.sessions.list().unwrap().is_empty());
    assert_eq!(
        state
            .profiles
            .list(ProfileListRequest::default())
            .unwrap()
            .len(),
        PROFILES + 1
    );
    let edge = admitted_edge(&state, &before.runtime_id.try_into().unwrap());
    assert_eq!(edge.sha256, before.runtime_digest);
    let profiles: Vec<_> = before
        .profiles
        .iter()
        .enumerate()
        .map(|(index, original)| {
            let saved = state
                .profiles
                .get(ProfileIdRequest {
                    profile_id: original.id.clone(),
                })
                .unwrap();
            assert_eq!(&saved, original);
            // The old host owned the listener. Keep every persisted field
            // except the disposable loopback startup address.
            let profile = ProfileDraftDto {
                name: saved.name,
                tags: saved.tags,
                preset_id: saved.preset_id,
                preset_version: saved.preset_version,
                proxy_id: saved.proxy_id,
                browser: saved.browser,
                persona: saved.persona,
                locale: saved.locale,
                window: saved.window,
                startup_urls: vec![format!("{}/seeded-soak/{index}/page", fixture.url)],
            };
            state
                .profiles
                .update(ProfileUpdateRequest {
                    profile_id: saved.id,
                    expected_revision: saved.revision,
                    profile,
                })
                .unwrap()
        })
        .collect();
    let restarted = launch_all(&state, &profiles[..PROFILES]);
    let mut after = capture_matrix(&fixture, 0..PROFILES);
    for session in &restarted {
        let id = session.id.clone().try_into().unwrap();
        assert!(state.browsers.is_running(&id).unwrap());
    }
    stop_all(&state, restarted);
    let promoted = launch_all(&state, &profiles[PROFILES..]);
    after.extend(capture_matrix(&fixture, PROFILES..PROFILES + 1));
    stop_all(&state, promoted);
    assert_eq!(before.signals.len(), after.len());
    for (index, (before, after)) in before.signals.iter().zip(after.iter()).enumerate() {
        for field in FIELDS {
            assert_eq!(
                before[field], after[field],
                "profile {index} changed {field} after restart"
            );
        }
    }
    let report = json!({
        "restartedProfiles": after.len(),
        "contexts": CONTEXTS,
        "profileSignals": before.signals,
    });
    write_marker(root, "reopen-verified", report.to_string().as_bytes());
}
