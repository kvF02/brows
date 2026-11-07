//! Explicit opt-in public-network acceptance using the production composition.
use super::{draft, started, AppState, ControlFixture, NativeAcceptanceRoot};
use brows_contracts::{
    BrowserSelectorDto, DeviceMemoryDto, DnsModeDto, PersonaDto, PlatformDto, ProfileCreateRequest,
    ProfileDraftDto, ProfileDto, ProfileIdRequest, ProfileUpdateRequest, ProxyDraftDto, ProxyIdDto,
    ProxySaveRequest, ProxySchemeDto, ScreenConfigDto, SecretInput, SessionLaunchRequest,
    SessionStopRequest,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    env, fs,
    net::SocketAddrV4,
    thread,
    time::{Duration, Instant},
};

struct Harness {
    state: Option<AppState>,
    proxies: Vec<ProxyIdDto>,
    profiles: Vec<ProfileDto>,
}

fn editable(profile: &ProfileDto) -> ProfileDraftDto {
    // Both DTOs share the editable fields; managed identity/revision are ignored.
    serde_json::from_value(serde_json::to_value(profile).unwrap()).unwrap()
}

impl Harness {
    fn state(&self) -> &AppState {
        self.state.as_ref().unwrap()
    }
    fn cleanup(&mut self) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        crate::cleanup_sessions(state);
        let mut ok = true;
        for profile in &self.profiles {
            let result = state
                .profiles
                .get(ProfileIdRequest {
                    profile_id: profile.id.clone(),
                })
                .and_then(|current| {
                    let mut edit = editable(&current);
                    edit.proxy_id = None;
                    state.profiles.update(ProfileUpdateRequest {
                        profile_id: current.id,
                        profile: edit,
                        expected_revision: current.revision,
                    })
                });
            ok &= result.is_ok();
        }
        for proxy in &self.proxies {
            ok &= state
                .proxy_management
                .as_ref()
                .unwrap()
                .remove(proxy.clone().try_into().unwrap())
                .is_ok();
        }
        self.proxies.clear();
        self.profiles.clear();
        ok
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn write_report(report: &Value) {
    fs::create_dir_all("../../target").unwrap();
    fs::write(
        "../../target/live-browser-network-acceptance.json",
        serde_json::to_vec_pretty(report).unwrap(),
    )
    .unwrap();
}

fn observe(state: &AppState, fixture: &ControlFixture, profile: &ProfileDto) -> Value {
    let prefix = profile.startup_urls[0]
        .strip_prefix(&fixture.url)
        .unwrap()
        .trim_end_matches("page");
    let count = || {
        fixture
            .proofs
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _, value)| path.starts_with(prefix) && value["kind"] == "network")
            .count()
    };
    let before = count();
    let session = started(
        state
            .sessions
            .launch(SessionLaunchRequest {
                profile_id: profile.id.clone(),
            })
            .expect("production browser launch"),
    );
    let deadline = Instant::now() + Duration::from_secs(45);
    while count() <= before && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    state
        .sessions
        .stop(SessionStopRequest {
            session_id: session.id.clone(),
        })
        .expect("stop owned browser");
    let id = session.id.try_into().unwrap();
    assert!(
        !state.browsers.is_running(&id).unwrap(),
        "owned browser must stop"
    );
    assert!(count() > before, "fresh browser network evidence missing");
    fixture
        .proofs
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(path, _, value)| path.starts_with(prefix) && value["kind"] == "network")
        .unwrap()
        .2
        .clone()
}

fn exit(sample: &Value) -> Option<std::net::IpAddr> {
    if sample["https"]["status"] != "observed" {
        return None;
    }
    sample["https"]["ip"].as_str()?.parse().ok()
}
fn resolvers(sample: &Value) -> BTreeSet<std::net::IpAddr> {
    sample["dns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| item["resolvers"].as_array().into_iter().flatten())
        .filter_map(|ip| ip.as_str()?.parse().ok())
        .collect()
}
fn positive_stun(sample: &Value) -> bool {
    sample["webrtc"]["srflx"]
        .as_array()
        .is_some_and(|ips| !ips.is_empty())
        && matches!(
            sample["webrtc"]["status"].as_str(),
            Some("complete" | "timeout")
        )
}
fn udp_candidate(sample: &Value) -> bool {
    sample["webrtc"]["protocols"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == "udp"))
}

#[test]
#[ignore = "uses repository Secrets and authorized public proxies on a disposable Windows CI runner"]
fn live_browser_network_acceptance() {
    assert_eq!(
        env::var("GITHUB_ACTIONS").as_deref(),
        Ok("true"),
        "disposable CI runner only"
    );
    let endpoints: Vec<SocketAddrV4> = env::var("BROWS_PROXY_ENDPOINTS")
        .expect("proxy endpoints")
        .split(',')
        .map(|s| s.trim().parse().expect("IPv4:port only"))
        .collect();
    assert!(
        !endpoints.is_empty() && endpoints.len() <= 2,
        "one or two authorized endpoints"
    );
    let username = env::var("BROWS_PROXY_ACCEPTANCE_USERNAME").expect("username Secret");
    let password = env::var("BROWS_PROXY_ACCEPTANCE_PASSWORD").expect("password Secret");
    assert!(
        !username.is_empty() && !password.is_empty(),
        "Secrets cannot be empty"
    );
    // Remove before any browser process exists; the Edge children need only the broker.
    env::remove_var("BROWS_PROXY_ACCEPTANCE_USERNAME");
    env::remove_var("BROWS_PROXY_ACCEPTANCE_PASSWORD");
    let root = NativeAcceptanceRoot::new("live-network");
    let data = root.0.join("data");
    let fixture = ControlFixture::start(false);
    let mut harness = Harness {
        state: Some(AppState::production(data.clone()).unwrap()),
        proxies: vec![],
        profiles: vec![],
    };
    let edge = harness
        .state()
        .browsers
        .list_builds()
        .unwrap()
        .into_iter()
        .find(|b| b.provider == "Microsoft Edge")
        .expect("signed Edge");
    let edge = harness
        .state()
        .browsers
        .probe_fingerprint_support(&edge.id)
        .unwrap()
        .build;
    assert_eq!(edge.trust, brows_domain::RuntimeTrust::Verified);
    assert_eq!(
        edge.capabilities.fingerprint_support,
        brows_domain::FingerprintSupport::Supported
    );
    let mut native = draft("Live network direct control");
    native.browser = BrowserSelectorDto::Pinned {
        build_id: (&edge.id).into(),
    };
    native.startup_urls = vec![format!("{}/live-network/direct/page", fixture.url)];
    let direct = harness
        .state()
        .profiles
        .create(ProfileCreateRequest { profile: native })
        .unwrap();
    harness.profiles.push(direct.clone());
    let mut cases = Vec::new();
    for (index, endpoint) in endpoints.iter().enumerate() {
        for (protocol, scheme) in [
            ("http", ProxySchemeDto::Http),
            ("socks5", ProxySchemeDto::Socks5),
        ] {
            let proxy = harness
                .state()
                .proxy_management
                .as_ref()
                .unwrap()
                .save(ProxySaveRequest {
                    proxy: ProxyDraftDto {
                        id: None,
                        name: format!("Live network {} {protocol}", index + 1),
                        scheme,
                        host: endpoint.ip().to_string(),
                        port: endpoint.port(),
                        username: Some(username.clone()),
                        dns_mode: DnsModeDto::Proxy,
                        // The owned fixture is the only explicit bypass; public requests use the broker.
                        bypass_hosts: vec!["127.0.0.1".to_owned()],
                        connect_timeout_ms: 10000,
                    },
                    credential: Some(SecretInput::new(password.clone()).unwrap()),
                })
                .expect("save proxy with Windows credential store");
            harness.proxies.push(proxy.id.clone());
            for seeded in [false, true] {
                let label = format!(
                    "{}-{protocol}-{}",
                    index + 1,
                    if seeded { "seeded" } else { "native" }
                );
                let mut profile = draft(&format!("Live network {label}"));
                profile.browser = BrowserSelectorDto::Pinned {
                    build_id: (&edge.id).into(),
                };
                profile.proxy_id = Some(proxy.id.clone());
                profile.startup_urls = vec![format!("{}/live-network/{label}/page", fixture.url)];
                if seeded {
                    profile.persona = PersonaDto::SeedableNoise {
                        seed: format!("brows-live-network-{label}"),
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
                    profile.locale.timezone = Some("America/New_York".to_owned());
                }
                let profile = harness
                    .state()
                    .profiles
                    .create(ProfileCreateRequest { profile })
                    .unwrap();
                harness.profiles.push(profile.clone());
                cases.push((label, profile, *endpoint.ip(), seeded));
            }
        }
    }
    drop(username);
    drop(password);
    let mut report = json!({
        "schemaVersion": 1, "complete": false, "productCommit": env::var("BROWS_PRODUCT_COMMIT").ok(),
        "browserVersion": edge.version, "runtimeTrust": "verified", "direct": [], "cases": [],
        "dnsAcceptance": {"status": "not_checked", "reason": "expected_proxy_resolver_ranges_not_configured", "method": "fresh_operator_owned_dns4_and_dns6_names"},
        "webrtcScope": "Google public STUN; no media permissions; direct positive control required",
        "notChecked": ["authoritative DNS resolver allowlist", "all WebRTC/STUN/TURN transports", "Windows 10/11 client matrix", "arbitrary site compatibility"]
    });
    write_report(&report);
    for round in 0..2 {
        if round == 1 {
            crate::cleanup_sessions(harness.state());
            drop(harness.state.take());
            harness.state =
                Some(AppState::production(data.clone()).expect("reopen persisted application"));
            let support = harness
                .state()
                .browsers
                .probe_fingerprint_support(&edge.id)
                .unwrap();
            assert_eq!(
                support.build.capabilities.fingerprint_support,
                brows_domain::FingerprintSupport::Supported
            );
        }
        let baseline = observe(harness.state(), &fixture, &direct);
        report["direct"]
            .as_array_mut()
            .unwrap()
            .push(baseline.clone());
        write_report(&report);
        for (label, profile, expected_ip, seeded) in &cases {
            // Reload metadata rather than relying on the original in-memory DTO.
            let saved = harness
                .state()
                .profiles
                .get(ProfileIdRequest {
                    profile_id: profile.id.clone(),
                })
                .unwrap();
            assert_eq!(
                saved, *profile,
                "profile metadata changed across application restart"
            );
            let sample = observe(harness.state(), &fixture, &saved);
            let exit_ok = exit(&sample) == Some(std::net::IpAddr::V4(*expected_ip))
                && exit(&sample) != exit(&baseline);
            let signal = &sample["fingerprint"];
            let canvas_ok = signal["canvas"].as_str().is_some_and(|s| s.len() == 2048)
                && signal["canvas"] == signal["repeat"];
            let persona_ok = !seeded
                || (signal["platform"] == "Win32"
                    && signal["hardwareConcurrency"] == 3
                    && signal["timezone"] == "America/New_York");
            let baseline_dns = resolvers(&baseline);
            let dns_overlap: Vec<_> = resolvers(&sample)
                .intersection(&baseline_dns)
                .map(ToString::to_string)
                .collect();
            report["cases"].as_array_mut().unwrap().push(json!({
                "label": label, "round": round, "reopened": round == 1, "exitPassed": exit_ok,
                "canvasRepeatPassed": canvas_ok, "personaPassed": persona_ok,
                "baselineResolverOverlap": dns_overlap, "sample": sample,
            }));
            write_report(&report);
            println!("live browser {label} round {round}: exit={exit_ok}, canvas={canvas_ok}, persona={persona_ok}");
        }
    }
    let observations = report["cases"].as_array().unwrap();
    let browser_passed = observations.iter().all(|c| {
        c["exitPassed"] == true && c["canvasRepeatPassed"] == true && c["personaPassed"] == true
    });
    let reopened_stable = cases.iter().all(|(label, _, _, _)| {
        let matching: Vec<_> = observations
            .iter()
            .filter(|c| c["label"] == *label)
            .collect();
        matching.len() == 2
            && matching[0]["sample"]["fingerprint"] == matching[1]["sample"]["fingerprint"]
    });
    let distinct_seeded: BTreeSet<_> = observations
        .iter()
        .filter(|c| c["round"] == 0 && c["label"].as_str().unwrap().ends_with("seeded"))
        .map(|c| {
            c["sample"]["fingerprint"]["canvas"]
                .as_str()
                .unwrap_or_default()
        })
        .collect();
    let distinct_passed = distinct_seeded.len() == endpoints.len() * 2;
    let control = report["direct"]
        .as_array()
        .unwrap()
        .iter()
        .all(positive_stun);
    let leaked_udp = observations.iter().any(|c| udp_candidate(&c["sample"]));
    let rtc_ran = observations.iter().all(|c| {
        matches!(
            c["sample"]["webrtc"]["status"].as_str(),
            Some("complete" | "timeout")
        )
    });
    let rtc_status = if leaked_udp {
        "fail"
    } else if control && rtc_ran {
        "pass"
    } else {
        "not_checked"
    };
    report["webrtcUdpPolicy"] = json!({"status": rtc_status, "directStunPositiveControl": control, "udpCandidateObservedThroughProxy": leaked_udp});
    report["allBrowserChecksPassed"] = json!(browser_passed && reopened_stable && distinct_passed);
    report["reopenFingerprintStable"] = json!(reopened_stable);
    report["distinctSeededCanvases"] = json!(distinct_passed);
    report["ownedCredentialsRemoved"] = json!(harness.cleanup());
    report["complete"] = json!(true);
    write_report(&report);
    assert!(
        browser_passed,
        "actual HTTPS browser exits or persona signals failed; see sanitized report"
    );
    assert!(
        reopened_stable && distinct_passed,
        "fingerprint persistence/isolation failed"
    );
    assert!(
        !leaked_udp,
        "browser exposed a UDP ICE candidate despite proxy policy"
    );
    assert_eq!(report["ownedCredentialsRemoved"], true);
}
