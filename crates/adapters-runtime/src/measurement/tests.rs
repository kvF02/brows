//! Synthetic fixtures verify the evaluator and local protocol, not Chrome.
use super::*;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpStream,
};
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn build() -> BrowserBuild {
    BrowserBuild {
        id: BrowserBuildId::new("measurement-fixture").unwrap(),
        provider: "fixture".into(),
        version: "156.0.8078.80".into(),
        channel: brows_domain::BrowserChannel::Stable,
        executable_path: PathBuf::from("/fixture/chrome.exe"),
        sha256: "a".repeat(64),
        publisher: "fixture".into(),
        trust: RuntimeTrust::Verified,
        capabilities: brows_domain::BrowserCapabilities {
            fingerprint_support: FingerprintSupport::Supported,
            ..Default::default()
        },
        installed_at_ms: 0,
    }
}
fn signal(cfg: &FingerprintSampleConfigDto, hash: &str) -> FingerprintSignalsDto {
    FingerprintSignalsDto {
        user_agent: Some(cfg.user_agent.clone()),
        platform: Some(cfg.platform.clone()),
        language: cfg.languages.first().cloned(),
        languages: cfg.languages.clone(),
        concurrency: Some(cfg.concurrency),
        memory: Some(cfg.memory),
        memory_type: "number".into(),
        touch_points: Some(0),
        screen_width: Some(cfg.screen_width),
        screen_height: Some(cfg.screen_height),
        color_depth: Some(cfg.color_depth),
        inner_width: Some(cfg.window_width),
        inner_height: Some(cfg.window_height),
        outer_width: Some(cfg.window_width),
        outer_height: Some(cfg.window_height),
        pixel_ratio: Some(1.0),
        webgl_vendor: Some(cfg.webgl_vendor.clone()),
        webgl_renderer: Some(cfg.webgl_renderer.clone()),
        timezone: Some("UTC".into()),
        client_hints: Some(FingerprintClientHintsDto {
            brands: vec!["fixture/156".into()],
            platform: Some("Windows".into()),
            mobile: Some(false),
            architecture: None,
            bitness: None,
            platform_version: None,
            full_version: None,
        }),
        canvas: Some(FingerprintCanvasDto {
            read_hash: hash.into(),
            repeat_read_hash: hash.into(),
            after_export_read_hash: hash.into(),
            export_hash: Some(format!("f{}", &hash[1..])),
            repeat_export_hash: Some(format!("f{}", &hash[1..])),
        }),
    }
}
fn body(run: &str) -> Value {
    let cfg = configured(&sample(&build(), TOKEN, &run[..1]).unwrap(), &build()).unwrap();
    let s = signal(
        &cfg,
        if run.starts_with('a') {
            "12345678"
        } else {
            "87654321"
        },
    );
    let marker = format!("{TOKEN}-{}", &run[..1]);
    json!({"token":TOKEN,"runId":run,"page":s,"iframe":s,"worker":s,
        "audio":{"firstHash":s.canvas.as_ref().unwrap().read_hash,"repeatHash":s.canvas.as_ref().unwrap().read_hash,"firstEventHash":s.canvas.as_ref().unwrap().read_hash,"repeatEventHash":s.canvas.as_ref().unwrap().read_hash},
        "storage":{"localAvailable":true,"cookieAvailable":true,"localBefore":if run.ends_with('0'){None}else{Some(&marker)},"cookieBefore":if run.ends_with('0'){None}else{Some(&marker)},"localAfter":marker,"cookieAfter":marker}})
}
fn samples() -> Vec<FingerprintSampleDto> {
    ["a", "b"]
        .into_iter()
        .map(|label| {
            let cfg = configured(&sample(&build(), TOKEN, label).unwrap(), &build()).unwrap();
            let observations = (0..2)
                .map(|round| {
                    let run = format!("{label}{round}");
                    let raw = RawObservation {
                        browser: server::decode(
                            &serde_json::to_vec(&body(&run)).unwrap(),
                            TOKEN,
                            &run,
                        )
                        .unwrap(),
                        network: FingerprintHeadersDto {
                            user_agent: Some(cfg.user_agent.clone()),
                            accept_language: cfg.languages.first().cloned(),
                            ..Default::default()
                        },
                    };
                    evaluate::observation(raw, &format!("{TOKEN}-{label}"), round)
                })
                .collect();
            FingerprintSampleDto {
                label: label.to_uppercase(),
                configured: cfg,
                observations,
            }
        })
        .collect()
}
fn status(samples: Vec<FingerprintSampleDto>, key: &str) -> FingerprintMeasurementStatusDto {
    let report = evaluate::report(&build(), samples);
    assert!(
        !report.covered_checks_passed,
        "bad observation must fail required coverage"
    );
    report
        .checks
        .iter()
        .find(|row| row.key == key)
        .unwrap()
        .status
}
#[test]
fn complete_synthetic_fixture_passes_only_implemented_coverage() {
    let report = evaluate::report(&build(), samples());
    assert!(report.covered_checks_passed);
    assert_eq!(report.runtime_sha256, "a".repeat(64));
    assert_eq!(report.mode, "headless");
    assert!(report.external_endpoints.is_empty());
    for key in ["a.worker", "a.client_hints", "a.timezone"] {
        assert_eq!(
            report.checks.iter().find(|r| r.key == key).unwrap().status,
            FingerprintMeasurementStatusDto::NotCovered
        );
    }
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(
        !serialized.contains(TOKEN),
        "never return raw storage identifiers"
    );
    assert!(!serialized.contains("localAvailable"));
}
#[test]
fn observed_main_and_frame_values_are_never_replaced_by_configuration() {
    for field in [
        "ua",
        "memory",
        "memory_type",
        "language",
        "screen",
        "webgl",
        "frame",
    ] {
        let mut rows = samples();
        let row = &mut rows[0].observations[1];
        let key = if field == "frame" {
            row.iframe = None;
            "a.iframe"
        } else {
            match field {
                "ua" => row.page.user_agent = None,
                "memory" => row.page.memory = Some(8.0),
                "memory_type" => row.page.memory_type = "string".into(),
                "language" => row.page.languages = vec!["en-US".into()],
                "screen" => row.page.screen_width = Some(1920),
                _ => row.page.webgl_renderer = Some("host GPU".into()),
            };
            "a.page"
        };
        assert_eq!(
            status(rows, key),
            FingerprintMeasurementStatusDto::Fail,
            "{field}"
        );
    }
}
#[test]
fn canvas_export_mutation_repeat_and_restart_drift_fail() {
    for change in 0..4 {
        let mut rows = samples();
        let v = rows[0].observations[if change == 3 { 1 } else { 0 }]
            .page
            .canvas
            .as_mut()
            .unwrap();
        match change {
            0 => v.after_export_read_hash = "00000000".into(),
            1 => v.repeat_read_hash = "00000000".into(),
            2 => v.repeat_export_hash = None,
            _ => {
                v.read_hash = "00000000".into();
                v.repeat_read_hash = v.read_hash.clone();
                v.after_export_read_hash = v.read_hash.clone();
            }
        };
        assert_eq!(
            status(rows, "a.canvas"),
            FingerprintMeasurementStatusDto::Fail
        );
    }
}
#[test]
fn audio_promise_event_repeat_and_restart_drift_fail() {
    for change in 0..4 {
        let mut rows = samples();
        let v = rows[0].observations[if change == 3 { 1 } else { 0 }]
            .audio
            .as_mut()
            .unwrap();
        match change {
            0 => v.first_event_hash = "00000000".into(),
            1 => v.repeat_hash = "00000000".into(),
            2 => v.repeat_event_hash = "00000000".into(),
            _ => {
                v.first_hash = "00000000".into();
                v.repeat_hash = v.first_hash.clone();
                v.first_event_hash = v.first_hash.clone();
                v.repeat_event_hash = v.first_hash.clone();
            }
        };
        assert_eq!(
            status(rows, "a.audio"),
            FingerprintMeasurementStatusDto::Fail
        );
    }
}
#[test]
fn shared_outputs_cannot_pass_the_two_seed_difference_checks() {
    for canvas in [true, false] {
        let mut rows = samples();
        for round in 0..2 {
            if canvas {
                rows[1].observations[round].page.canvas =
                    rows[0].observations[round].page.canvas.clone();
            } else {
                rows[1].observations[round].audio = rows[0].observations[round].audio.clone();
            }
        }
        assert_eq!(
            status(
                rows,
                if canvas {
                    "canvas_difference"
                } else {
                    "audio_difference"
                }
            ),
            FingerprintMeasurementStatusDto::Fail
        );
    }
}
#[test]
fn cross_profile_storage_or_missing_restart_persistence_fails() {
    for (round, value) in [(0, "other"), (1, "empty"), (1, "other"), (1, "unavailable")] {
        let mut rows = samples();
        rows[0].observations[round].storage.local_before = value.into();
        assert_eq!(
            status(rows, "a.storage"),
            FingerprintMeasurementStatusDto::Fail
        );
        let mut rows = samples();
        rows[0].observations[round].storage.cookie_before = value.into();
        assert_eq!(
            status(rows, "a.storage"),
            FingerprintMeasurementStatusDto::Fail
        );
    }
}
#[test]
fn incomplete_rounds_or_http_header_mismatch_fail() {
    let mut rows = samples();
    rows[0].observations.pop();
    assert_eq!(
        status(rows, "a.page"),
        FingerprintMeasurementStatusDto::Fail
    );
    let mut rows = samples();
    rows[0].observations[1].round = 0;
    assert_eq!(
        status(rows, "a.page"),
        FingerprintMeasurementStatusDto::Fail
    );
    for ua in [true, false] {
        let mut rows = samples();
        if ua {
            rows[0].observations[0].network.user_agent = Some("host UA".into());
        } else {
            rows[0].observations[0].network.accept_language = None;
        }
        assert_eq!(
            status(rows, "a.headers"),
            FingerprintMeasurementStatusDto::Fail
        );
    }
}
#[test]
fn raw_observations_require_identity_bounded_fields_and_strict_shape() {
    let mut variants = Vec::new();
    let mut v = body("a0");
    v["token"] = json!("foreign");
    variants.push(v);
    let mut v = body("a0");
    v["runId"] = json!("b0");
    variants.push(v);
    let mut v = body("a0");
    v["configured"] = json!({"memory":0.5});
    variants.push(v);
    let mut v = body("a0");
    v["page"]["memory"] = json!("0.5");
    variants.push(v);
    let mut v = body("a0");
    v["page"]["canvas"]["readHash"] = json!("invalid");
    variants.push(v);
    let mut v = body("a0");
    v["page"]["memoryType"] = json!("a".repeat(100));
    variants.push(v);
    let mut v = body("a0");
    v["storage"]["localAfter"] = json!("a".repeat(129));
    variants.push(v);
    for v in variants {
        assert!(server::decode(&serde_json::to_vec(&v).unwrap(), TOKEN, "a0").is_err());
    }
    assert!(server::decode(&vec![b' '; 65537], TOKEN, "a0").is_err());
}
fn request(
    collector: &Collector,
    run: &str,
    route: &str,
    method: &str,
    headers: &str,
    body: &[u8],
) -> String {
    let base = collector.url(run);
    let address = base
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let head=format!("{method} /{TOKEN}/{run}/{route} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n{headers}\r\n",body.len());
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    output
}
fn post_headers(c: &Collector) -> String {
    let origin = c.url("a0").split('/').take(3).collect::<Vec<_>>().join("/");
    format!("Origin: {origin}\r\nContent-Type: application/json\r\n")
}
#[test]
fn loopback_collector_authenticates_and_keeps_only_selected_request_headers() {
    let c = Collector::start(TOKEN).unwrap();
    let payload = serde_json::to_vec(&body("a0")).unwrap();
    assert!(
        request(&c, "a0", "collect", "POST", &post_headers(&c), &payload)
            .starts_with("HTTP/1.1 409")
    );
    let page = std::thread::scope(|scope| {
        let peer = scope.spawn(|| request(&c, "b0", "page", "GET", "", b""));
        let page=request(&c,"a0","page","GET","User-Agent: actual-browser\r\nAccept-Language: pl-PL\r\nCookie: private-cookie\r\nAuthorization: private-auth\r\n",b"");
        assert!(peer.join().unwrap().starts_with("HTTP/1.1 200"));
        page
    });
    assert!(page.contains("Content-Security-Policy: default-src 'none'"));
    assert!(page.contains("finish.svg"));
    assert!(!page.contains("1777"));
    assert!(!page.contains("brows-pair-sample"));
    assert!(request(
        &c,
        "a0",
        "collect",
        "POST",
        "Origin: http://foreign.invalid\r\nContent-Type: application/json\r\n",
        &payload
    )
    .starts_with("HTTP/1.1 403"));
    let mut wrong = body("a0");
    wrong["token"] = json!("foreign");
    assert!(request(
        &c,
        "a0",
        "collect",
        "POST",
        &post_headers(&c),
        &serde_json::to_vec(&wrong).unwrap()
    )
    .starts_with("HTTP/1.1 400"));
    assert!(
        request(&c, "a0", "collect", "POST", &post_headers(&c), &payload)
            .starts_with("HTTP/1.1 204")
    );
    assert!(
        request(&c, "a0", "collect", "POST", &post_headers(&c), &payload)
            .starts_with("HTTP/1.1 409")
    );
    let raw = c.take("a0").unwrap().unwrap();
    assert_eq!(raw.network.user_agent.as_deref(), Some("actual-browser"));
    let serialized =
        serde_json::to_string(&evaluate::observation(raw, &format!("{TOKEN}-a"), 0)).unwrap();
    assert!(!serialized.contains("private-cookie"));
    assert!(!serialized.contains("private-auth"));
    assert!(!serialized.contains(TOKEN));
    assert!(c.take("a0").unwrap().is_none());
}
#[test]
fn two_pending_load_barriers_allow_collection_and_shutdown_is_bounded() {
    let c = Collector::start(TOKEN).unwrap();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| request(&c, "a0", "finish.svg", "GET", "", b""));
        let b = scope.spawn(|| request(&c, "b0", "finish.svg", "GET", "", b""));
        let page_a = scope.spawn(|| request(&c, "a0", "page", "GET", "", b""));
        let page_b = request(&c, "b0", "page", "GET", "", b"");
        assert!(page_a.join().unwrap().starts_with("HTTP/1.1 200"));
        assert!(page_b.starts_with("HTTP/1.1 200"));
        for run in ["a0", "b0"] {
            assert!(request(
                &c,
                run,
                "collect",
                "POST",
                &post_headers(&c),
                &serde_json::to_vec(&body(run)).unwrap()
            )
            .starts_with("HTTP/1.1 204"));
        }
        assert!(a.join().unwrap().starts_with("HTTP/1.1 200"));
        assert!(b.join().unwrap().starts_with("HTTP/1.1 200"));
    });
    let started = Instant::now();
    drop(c);
    assert!(started.elapsed() < Duration::from_secs(2));
}
#[test]
fn pair_measurement_reserves_both_probe_slots_and_observes_shutdown() {
    use fingerprint_probe::{ProbeGate, ProbeSlot};
    let gate = ProbeGate::default();
    let a = BrowserBuildId::new("a").unwrap();
    let b = BrowserBuildId::new("b").unwrap();
    let first = ProbeSlot::acquire(&gate, &a).unwrap();
    assert!(ProbeSlot::acquire_pair(&gate, &b).is_err());
    drop(first);
    let pair = ProbeSlot::acquire_pair(&gate, &a).unwrap();
    assert!(ProbeSlot::acquire(&gate, &b).is_err());
    gate.stop();
    assert_eq!(
        gate.checkpoint().unwrap_err().kind,
        PortErrorKind::Cancelled
    );
    drop(pair);
    gate.wait();
    assert!(ProbeSlot::acquire_pair(&gate, &a).is_err());
}

fn runtime_fixture() -> (Arc<ManagedChromiumRuntime>, BrowserBuild, PathBuf) {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!(
        "brows-measure-test-{:032x}",
        u128::from_le_bytes(random)
    ));
    let runtime =
        Arc::new(ManagedChromiumRuntime::new(root.join("runtime"), root.join("profiles")).unwrap());
    let mut build = build();
    build.executable_path = root.join("runtime/chrome.exe");
    fs::write(&build.executable_path, b"fixture, must never execute").unwrap();
    build.capabilities.supports_language = true;
    runtime
        .register_build_with_current_digest(build.clone(), true)
        .unwrap();
    let build = runtime
        .resolve_build(&BrowserBuildSelector::Pinned(build.id))
        .unwrap();
    (runtime, build, root)
}
#[test]
fn measurements_require_real_readiness_and_integrity_failure_clears_it() {
    let (runtime, build, root) = runtime_fixture();
    let observer = LocalFingerprintMeasurement::new(runtime.clone());
    assert_eq!(
        observer.measure_pair(&build.id).unwrap_err().kind,
        PortErrorKind::CapabilityUnsupported
    );
    runtime
        .set_fingerprint_support(&build.id, FingerprintSupport::Supported)
        .unwrap();
    fs::write(&build.executable_path, b"changed binary").unwrap();
    assert_eq!(
        observer.measure_pair(&build.id).unwrap_err().kind,
        PortErrorKind::RuntimeUntrusted
    );
    assert_eq!(
        runtime
            .resolve_build(&BrowserBuildSelector::Pinned(build.id.clone()))
            .unwrap()
            .capabilities
            .fingerprint_support,
        FingerprintSupport::Unsupported
    );
    assert!(fs::read_dir(root.join("profiles"))
        .unwrap()
        .next()
        .is_none());
    assert!(fingerprint_probe::ProbeSlot::acquire_pair(&runtime.probing, &build.id).is_ok());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn samples_use_production_compiler_preferences_and_disjoint_owned_directories() {
    let (runtime, mut build, root) = runtime_fixture();
    runtime
        .set_fingerprint_support(&build.id, FingerprintSupport::Supported)
        .unwrap();
    build.capabilities.fingerprint_support = FingerprintSupport::Supported;
    let directory = fingerprint_probe::ProbeDirectory::create(&runtime.profile_root).unwrap();
    let catalog = Arc::new(brows_application::memory::InMemoryPresetCatalog::default());
    catalog
        .insert(brows_application::memory::system_native_preset())
        .unwrap();
    let compiler = LaunchPlanCompiler::new(
        catalog,
        runtime.clone(),
        Arc::new(NoProxy),
        Arc::new(
            brows_application::memory::FingerprintRuntimeRouter::new(
                directory.path.join("extensions"),
            )
            .unwrap(),
        ),
        directory.path.join("samples"),
    )
    .unwrap();
    let mut paths = Vec::new();
    for label in ["a", "b"] {
        let mut profile = sample(&build, TOKEN, label).unwrap();
        profile.startup_urls = vec![format!("http://127.0.0.1:12345/{TOKEN}/{label}0/page")];
        let plan = compiler
            .compile(&profile, &SessionId::new(format!("test-{label}")).unwrap())
            .unwrap()
            .plan;
        assert!(plan.user_data_dir.starts_with(&directory.path));
        assert!(matches!(
            plan.proxy,
            brows_domain::ProxyLaunchBinding::Direct
        ));
        runtime.verified_build_for_plan(&plan).unwrap();
        let data = runtime.prepare_profile(&plan).unwrap();
        let args = ManagedChromiumRuntime::render_arguments(&plan, &data).unwrap();
        assert!(args
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--load-extension=")));
        let prefs: Value =
            serde_json::from_slice(&fs::read(data.join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(
            prefs["intl"]["accept_languages"],
            profile.locale.accept_languages.join(",")
        );
        paths.push(data);
    }
    assert_ne!(paths[0], paths[1]);
    directory.cleanup().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn launch_rejects_changed_extensions_and_identity_mismatches_before_profile_writes_or_spawn() {
    for change in [
        "content",
        "background",
        "missing",
        "extra",
        "profile",
        "ua",
        "duplicate_extension",
        "duplicate_ua",
        "missing_ua",
    ] {
        let (runtime, build, root) = runtime_fixture();
        runtime
            .set_fingerprint_support(&build.id, FingerprintSupport::Supported)
            .unwrap();
        let catalog = Arc::new(brows_application::memory::InMemoryPresetCatalog::default());
        catalog
            .insert(brows_application::memory::system_native_preset())
            .unwrap();
        let compiler = LaunchPlanCompiler::new(
            catalog,
            runtime.clone(),
            Arc::new(NoProxy),
            Arc::new(
                brows_application::memory::FingerprintRuntimeRouter::new(root.join("extensions"))
                    .unwrap(),
            ),
            runtime.profile_root.clone(),
        )
        .unwrap();
        let profile = sample(&build, TOKEN, "a").unwrap();
        let mut plan = compiler
            .compile(&profile, &SessionId::new("artifact-admission").unwrap())
            .unwrap()
            .plan;
        ManagedChromiumRuntime::verify_plan_extension(&plan).unwrap();
        let extension = plan
            .arguments
            .iter()
            .find_map(|arg| match arg {
                LaunchArgument::LoadExtension(path) => Some(path.clone()),
                _ => None,
            })
            .unwrap();
        let before = if change == "content" {
            let data = runtime.prepare_profile(&plan).unwrap();
            Some(fs::read(data.join("Default/Preferences")).unwrap())
        } else {
            None
        };
        plan.preferences.accept_languages = vec!["en-US".into()];
        match change {
            "content" => {
                fs::write(extension.join("content.js"), b"changed after compilation").unwrap();
            }
            "background" => {
                fs::write(extension.join("background.js"), b"unexpected worker").unwrap();
            }
            "missing" => {
                fs::remove_file(extension.join("manifest.json")).unwrap();
            }
            "extra" => {
                fs::write(extension.join("extra.js"), b"unexpected file").unwrap();
            }
            "profile" => plan.profile_id = ProfileId::new("wrong-profile").unwrap(),
            "ua" => {
                for arg in &mut plan.arguments {
                    if let LaunchArgument::UserAgentOverride(ua) = arg {
                        *ua = "different-UA".into();
                    }
                }
            }
            "duplicate_extension" => plan
                .arguments
                .push(LaunchArgument::LoadExtension(extension)),
            "duplicate_ua" => plan
                .arguments
                .push(LaunchArgument::UserAgentOverride("second-UA".into())),
            "missing_ua" => plan
                .arguments
                .retain(|arg| !matches!(arg, LaunchArgument::UserAgentOverride(_))),
            _ => unreachable!(),
        }
        assert_eq!(
            runtime.launch(&plan).unwrap_err().kind,
            PortErrorKind::InvalidData,
            "{change}"
        );
        if let Some(before) = before {
            assert_eq!(
                fs::read(plan.user_data_dir.join("Default/Preferences")).unwrap(),
                before
            );
        } else {
            assert!(!plan.user_data_dir.exists());
        }
        assert!(runtime.processes.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn first_document_waits_for_the_second_browser_before_sampling() {
    let c = Collector::start(TOKEN).unwrap();
    std::thread::scope(|scope| {
        let (done, result) = std::sync::mpsc::channel();
        let collector = &c;
        scope.spawn(move || {
            done.send(request(collector, "a0", "page", "GET", "", b""))
                .unwrap();
        });
        assert!(
            result.recv_timeout(Duration::from_millis(100)).is_err(),
            "first browser must not sample alone"
        );
        assert!(request(&c, "b0", "page", "GET", "", b"").starts_with("HTTP/1.1 200"));
        assert!(result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .starts_with("HTTP/1.1 200"));
    });
}
