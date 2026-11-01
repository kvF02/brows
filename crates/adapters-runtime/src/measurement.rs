//! Real observations from two fixed temporary profiles, run together twice.
//! The page never receives hardware expectations or an existing profile path.
use super::*;
use brows_application::{FingerprintMeasurement, LaunchPlanCompiler};
use brows_contracts::*;
use brows_domain::{
    DeviceMemoryGb, EnvironmentPresetRef, FingerprintPersona, LocaleConfig, PlatformKind, PresetId,
    ProfileId, ProfileSpec, Revision, ScreenConfig, SeedableNoisePersona, WebGLProfile,
    WindowConfig,
};
use std::time::{Duration, Instant};
mod evaluate;
mod server;
#[cfg(test)]
mod tests;
use server::{Collector, RawObservation};

#[derive(Debug)]
pub struct LocalFingerprintMeasurement {
    runtime: Arc<ManagedChromiumRuntime>,
}
impl LocalFingerprintMeasurement {
    pub fn new(runtime: Arc<ManagedChromiumRuntime>) -> Self {
        Self { runtime }
    }
}
impl FingerprintMeasurement for LocalFingerprintMeasurement {
    fn measure_pair(&self, id: &BrowserBuildId) -> PortResult<FingerprintMeasurementReportDto> {
        let _slot = fingerprint_probe::ProbeSlot::acquire_pair(&self.runtime.probing, id)?;
        let build = self
            .runtime
            .resolve_build(&BrowserBuildSelector::Pinned(id.clone()))?;
        if build.trust != RuntimeTrust::Verified
            || build.capabilities.fingerprint_support != FingerprintSupport::Supported
        {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "请先通过此运行时的指纹加载检测，再进行双临时环境实测",
            ));
        }
        let result = (|| {
            self.runtime.verify_build_integrity(&build)?;
            let directory = fingerprint_probe::ProbeDirectory::create(&self.runtime.profile_root)?;
            let result = self.run(&build, &directory.path);
            // Recheck even if execution failed; never retain readiness from a
            // changed executable. Cleanup always runs before releasing the gate.
            let integrity = self.runtime.verify_build_integrity(&build);
            let cleanup = directory.cleanup();
            cleanup?;
            integrity?;
            result
        })();
        if matches!(&result, Ok(report) if !report.covered_checks_passed)
            || matches!(&result, Err(error) if error.kind != PortErrorKind::Cancelled)
        {
            self.runtime
                .set_fingerprint_support(id, FingerprintSupport::Unsupported)?;
        }
        result
    }
}
impl LocalFingerprintMeasurement {
    fn run(
        &self,
        build: &BrowserBuild,
        root: &Path,
    ) -> PortResult<FingerprintMeasurementReportDto> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| internal("无法创建临时观测身份"))?;
        let token = format!("{:032x}", u128::from_le_bytes(random));
        let collector = Collector::start(&token)?;
        let catalog = Arc::new(brows_application::memory::InMemoryPresetCatalog::default());
        catalog.insert(brows_application::memory::system_native_preset())?;
        let compiler = LaunchPlanCompiler::new(
            catalog,
            self.runtime.clone(),
            Arc::new(NoProxy),
            Arc::new(brows_application::memory::FingerprintRuntimeRouter::new(
                root.join("extensions"),
            )?),
            root.join("samples"),
        )
        .map_err(app_error)?;
        let mut profiles = [sample(build, &token, "a")?, sample(build, &token, "b")?];
        let configs = profiles
            .iter()
            .map(|profile| configured(profile, build))
            .collect::<PortResult<Vec<_>>>()?;
        let mut observations = [Vec::new(), Vec::new()];
        let deadline = Instant::now() + Duration::from_secs(80);
        for round in 0..2 {
            self.runtime.probing.checkpoint()?;
            let mut children = Vec::new();
            for (index, profile) in profiles.iter_mut().enumerate() {
                let run_id = format!("{}{}", if index == 0 { "a" } else { "b" }, round);
                profile.startup_urls = vec![collector.url(&run_id)];
                let plan = compiler
                    .compile(
                        profile,
                        &SessionId::new(format!("measure-{token}-{run_id}"))
                            .map_err(|e| invalid_data(e.to_string()))?,
                    )
                    .map_err(app_error)?
                    .plan;
                self.runtime.probing.checkpoint()?;
                ensure_within(&plan.user_data_dir, root, "temporary measurement profile")?;
                let verified = self.runtime.verified_build_for_plan(&plan)?;
                if verified.id != build.id {
                    return Err(bundle::untrusted("观测计划解析到了不同的运行时"));
                }
                let data = self.runtime.prepare_profile(&plan)?;
                let args = ManagedChromiumRuntime::render_arguments(&plan, &data)?;
                ManagedChromiumRuntime::verify_plan_extension(&plan)?;
                let child = BrowserChild::spawn(
                    Command::new(&verified.executable_path)
                        .args(args)
                        .args([
                            "--headless=new",
                            "--dump-dom",
                            "--disable-background-networking",
                            "--disable-component-update",
                            "--disable-sync",
                            "--metrics-recording-only",
                            "--disable-domain-reliability",
                            "--no-proxy-server",
                        ])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null()),
                )
                .map_err(|_| {
                    PortError::new(PortErrorKind::Unavailable, "无法启动双环境实测浏览器")
                })?;
                children.push(child);
            }
            let round_deadline = (Instant::now() + Duration::from_secs(30)).min(deadline);
            let mut exited = [false, false];
            loop {
                self.runtime.probing.checkpoint()?;
                for (index, child) in children.iter_mut().enumerate() {
                    if !exited[index] {
                        if let Some(status) = child
                            .try_wait()
                            .map_err(|_| internal("无法查询临时浏览器状态"))?
                        {
                            if !status.success() {
                                return Err(PortError::new(
                                    PortErrorKind::Unavailable,
                                    "临时浏览器异常退出，未生成可信观测",
                                ));
                            }
                            exited[index] = true;
                        }
                    }
                }
                if exited.iter().all(|value| *value) {
                    break;
                }
                if Instant::now() >= round_deadline {
                    return Err(PortError::new(
                        PortErrorKind::Unavailable,
                        "双环境观测超时，临时进程将回收",
                    ));
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            for child in &mut children {
                child
                    .stop()
                    .map_err(|_| internal("无法回收临时浏览器进程树"))?;
            }
            for (index, values) in observations.iter_mut().enumerate() {
                let run_id = format!("{}{}", if index == 0 { "a" } else { "b" }, round);
                let result = collector
                    .take(&run_id)?
                    .ok_or_else(|| invalid_data("浏览器未返回完整观测；不会使用配置值代替"))?;
                values.push(evaluate::observation(
                    result,
                    &format!("{token}-{}", if index == 0 { "a" } else { "b" }),
                    round,
                ));
            }
        }
        let samples = configs
            .into_iter()
            .zip(observations)
            .enumerate()
            .map(|(index, (configured, observations))| FingerprintSampleDto {
                label: if index == 0 { "A" } else { "B" }.into(),
                configured,
                observations,
            })
            .collect();
        Ok(evaluate::report(build, samples))
    }
}
fn app_error(error: brows_application::ApplicationError) -> PortError {
    PortError::new(PortErrorKind::InvalidData, error.to_string())
}
#[derive(Debug)]
struct NoProxy;
impl brows_application::ProxyRuntime for NoProxy {
    fn resolve_proxy(&self, _: &brows_domain::ProxyId) -> PortResult<brows_domain::ProxySpec> {
        Err(invalid_data("measurements cannot resolve saved proxies"))
    }
    fn prepare_session(
        &self,
        _: &SessionId,
        _: &brows_domain::ProxySpec,
    ) -> PortResult<brows_application::ProxyLease> {
        Err(invalid_data("measurements cannot use saved proxies"))
    }
    fn release_session(&self, _: &brows_application::ProxyLease) -> PortResult<()> {
        Err(invalid_data("measurements never create proxy leases"))
    }
}
fn sample(build: &BrowserBuild, token: &str, label: &str) -> PortResult<ProfileSpec> {
    let a = label == "a";
    Ok(ProfileSpec {
        id: ProfileId::new(format!("measure-{token}-{label}"))
            .map_err(|e| invalid_data(e.to_string()))?,
        name: format!("临时样本 {label}"),
        tags: Vec::new(),
        environment_preset: EnvironmentPresetRef::new(PresetId::new("system-native").unwrap(), 1)
            .unwrap(),
        proxy_id: None,
        browser: BrowserBuildSelector::Pinned(build.id.clone()),
        persona: FingerprintPersona::SeedableNoise(SeedableNoisePersona {
            seed: format!("brows-pair-sample-{label}-v1"),
            platform: PlatformKind::Windows,
            hardware_concurrency: if a { 3 } else { 7 },
            device_memory: if a {
                DeviceMemoryGb::Half
            } else {
                DeviceMemoryGb::Two
            },
            touch_points: 0,
            screen: ScreenConfig {
                width: if a { 1777 } else { 1555 },
                height: if a { 1111 } else { 999 },
                color_depth: 24,
                device_scale_factor_hundredths: 100,
            },
            webgl: if a {
                WebGLProfile {
                    vendor: "Google Inc. (Intel)".into(),
                    unmasked_renderer:
                        "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)"
                            .into(),
                }
            } else {
                WebGLProfile {
                    vendor: "Google Inc. (NVIDIA)".into(),
                    unmasked_renderer:
                        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)"
                            .into(),
                }
            },
            spoof_canvas: true,
            spoof_audio: true,
        }),
        locale: LocaleConfig {
            accept_languages: if a {
                vec!["pl-PL".into(), "pl".into()]
            } else {
                vec!["ja-JP".into(), "ja".into()]
            },
            timezone: None,
        },
        window: WindowConfig {
            width: if a { 1200 } else { 1100 },
            height: if a { 800 } else { 700 },
            start_maximized: false,
        },
        startup_urls: Vec::new(),
        revision: Revision::FIRST,
        archived: false,
    })
}
fn configured(
    profile: &ProfileSpec,
    build: &BrowserBuild,
) -> PortResult<FingerprintSampleConfigDto> {
    let FingerprintPersona::SeedableNoise(persona) = &profile.persona else {
        return Err(invalid_data("unexpected sample persona"));
    };
    let fp =
        brows_application::persona_fingerprint::PersonaFingerprint::derive(persona, &build.version)
            .map_err(|e| invalid_data(e.to_string()))?;
    let memory = fp.device_memory_number();
    Ok(FingerprintSampleConfigDto {
        seed: persona.seed.clone(),
        user_agent: fp.user_agent,
        platform: fp.platform_string.to_owned(),
        languages: profile.locale.accept_languages.clone(),
        concurrency: u32::from(persona.hardware_concurrency),
        memory,
        screen_width: persona.screen.width,
        screen_height: persona.screen.height,
        color_depth: u32::from(persona.screen.color_depth),
        window_width: profile.window.width,
        window_height: profile.window.height,
        webgl_vendor: fp.webgl_vendor,
        webgl_renderer: fp.webgl_renderer,
    })
}
