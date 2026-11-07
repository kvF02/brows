use super::super::{draft, started, AppState, ControlFixture, NativeAcceptanceRoot};
use super::{await_condition, page_hits, verify_steady_load};
use brows_contracts::{
    BrowserSelectorDto, DeviceMemoryDto, PersonaDto, PlatformDto, ProfileCreateRequest, ProfileDto,
    ProfileIdRequest, ScreenConfigDto, SessionDto, SessionLaunchRequest, SessionStateDto,
    SessionStopRequest,
};
use brows_domain::{BrowserBuild, BrowserBuildId, FingerprintSupport, RuntimeTrust, SessionId};
use serde_json::{json, Value};
use std::{collections::BTreeSet, env, fs, path::PathBuf, time::Duration};

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

fn capture_matrix(fixture: &ControlFixture, revision: u64) -> Vec<Value> {
    let mut captures = None;
    await_condition(
        90,
        "first-script page and Worker proofs for all ten profiles",
        || {
            let proofs = fixture.proofs.lock().unwrap();
            captures = (0..PROFILES)
                .map(|index| {
                    let sample = |kind| {
                        let expected = format!("/seeded-soak/{index}/{kind}/proof");
                        proofs
                            .iter()
                            .find(|(path, _, value)| {
                                path == &expected
                                    && value["kind"] == kind
                                    && value["fixtureVersion"] == revision
                            })
                            .map(|(_, _, value)| value.clone())
                    };
                    Some([sample(CONTEXTS[0])?, sample(CONTEXTS[1])?])
                })
                .collect::<Option<Vec<_>>>();
            captures.is_some()
        },
    );

    let unmodified_canvas = "a42367ff".repeat(16 * 16);
    let matrix: Vec<_> = captures
        .unwrap()
        .into_iter()
        .enumerate()
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
        .collect();
    let distinct: BTreeSet<_> = matrix
        .iter()
        .map(|sample| sample["canvas"].as_str().unwrap())
        .collect();
    assert_eq!(
        distinct.len(),
        PROFILES,
        "the ten fixture seeds must be distinct"
    );
    matrix
}

#[test]
#[ignore = "launches and restarts ten isolated SeedableNoise Edge profiles"]
fn seedable_noise_soak() {
    let minutes: u64 = env::var("BROWS_SOAK_MINUTES")
        .expect("set BROWS_SOAK_MINUTES")
        .parse()
        .unwrap();
    assert!((1..=240).contains(&minutes));
    let owned = NativeAcceptanceRoot::new("seeded-soak");
    let fixture = ControlFixture::start(false);
    let state = AppState::production(owned.0.clone()).unwrap();
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
    let profiles: Vec<_> = (0..PROFILES)
        .map(|index| {
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
            state
                .profiles
                .create(ProfileCreateRequest { profile })
                .unwrap()
        })
        .collect();
    let sessions = launch_all(&state, &profiles);
    let ids: Vec<SessionId> = sessions
        .iter()
        .map(|session| session.id.clone().try_into().unwrap())
        .collect();
    let initial = capture_matrix(&fixture, 0);
    println!("Ten SeedableNoise profiles returned distinct page and Worker Canvas");
    await_condition(60, "first heartbeat from every seeded profile", || {
        (0..PROFILES).all(|index| page_hits(&fixture, index) > 0)
    });
    let elapsed = verify_steady_load(
        &state,
        &fixture,
        &ids,
        Duration::from_secs(minutes * 60),
        None,
    );
    println!("Ten SeedableNoise profiles stayed active for {elapsed} seconds");
    stop_all(&state, sessions);
    drop(state);

    let state = AppState::production(owned.0.clone()).unwrap();
    assert!(state.sessions.list().unwrap().is_empty());
    let reopened_edge = admitted_edge(&state, &edge.id);
    assert_eq!(edge.id, reopened_edge.id);
    assert_eq!(edge.sha256, reopened_edge.sha256);
    for profile in &profiles {
        let saved = state
            .profiles
            .get(ProfileIdRequest {
                profile_id: profile.id.clone(),
            })
            .unwrap();
        assert_eq!(&saved, profile);
    }
    // A new fixture revision proves the restart samples were freshly captured
    // at the same origin without rewriting the persisted profile configuration.
    fixture.bump_service_version();
    let restarted = launch_all(&state, &profiles);
    let after = capture_matrix(&fixture, 1);
    for session in &restarted {
        let id = session.id.clone().try_into().unwrap();
        assert!(state.browsers.is_running(&id).unwrap());
    }
    for (index, (before, after)) in initial.iter().zip(after.iter()).enumerate() {
        for field in FIELDS {
            assert_eq!(
                before[field], after[field],
                "profile {index} changed {field} after restart"
            );
        }
    }
    stop_all(&state, restarted);
    println!("All ten reopened profiles reproduced their original page and Worker signals");

    let report = json!({
        "assertionsPassed": true,
        "complete": false,
        "commit": env::var("GITHUB_SHA").ok(),
        "runtimeVersion": edge.version,
        "runtimeId": edge.id.as_str(),
        "persona": "SeedableNoise",
        "loopbackSleepException": env::var("BROWS_SOAK_LOOPBACK_POLICY").ok(),
        "concurrentProfiles": PROFILES,
        "restartedProfiles": PROFILES,
        "requestedSeconds": minutes * 60,
        "observedSeconds": elapsed,
        "contexts": CONTEXTS,
        "checks": ["distinct-seed-canvas", "page-and-worker-consistency", "continuous-page-canvas",
                   "sqlite-profile-reopen", "ten-fresh-matching-restart-samples"],
        "profileSignals": initial,
        "notChecked": ["packaged UI", "public sites and DNS/ICE", "GPU output, fonts and live audio",
                       "browser or host crash in SeedableNoise"]
    });
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/windows-seeded-soak-report.json");
    fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
