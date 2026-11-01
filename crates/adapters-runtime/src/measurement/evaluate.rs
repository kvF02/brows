use super::*;
use FingerprintMeasurementStatusDto as Status;

pub(super) fn observation(
    raw: RawObservation,
    marker: &str,
    round: u32,
) -> FingerprintObservationDto {
    let value = raw.browser;
    let storage = value.storage;
    let classify = |available: bool, text: Option<String>| {
        match (available, text) {
            (false, _) => "unavailable",
            (true, None) => "empty",
            (true, Some(value)) if value == marker => "own",
            _ => "other",
        }
        .to_owned()
    };
    FingerprintObservationDto {
        round,
        page: value.page,
        iframe: value.iframe,
        worker: value.worker,
        audio: value.audio,
        network: raw.network,
        storage: FingerprintStorageDto {
            local_before: classify(storage.local_available, storage.local_before),
            local_after: classify(storage.local_available, storage.local_after),
            cookie_before: classify(storage.cookie_available, storage.cookie_before),
            cookie_after: classify(storage.cookie_available, storage.cookie_after),
        },
    }
}
fn matches_config(actual: &FingerprintSignalsDto, cfg: &FingerprintSampleConfigDto) -> bool {
    actual.user_agent.as_ref() == Some(&cfg.user_agent)
        && actual.platform.as_ref() == Some(&cfg.platform)
        && actual.language.as_ref() == cfg.languages.first()
        && actual.languages == cfg.languages
        && actual.concurrency == Some(cfg.concurrency)
        && actual.memory == Some(cfg.memory)
        && actual.memory_type == "number"
        && actual.touch_points == Some(0)
        && actual.screen_width == Some(cfg.screen_width)
        && actual.screen_height == Some(cfg.screen_height)
        && actual.color_depth == Some(cfg.color_depth)
        && actual.webgl_vendor.as_ref() == Some(&cfg.webgl_vendor)
        && actual.webgl_renderer.as_ref() == Some(&cfg.webgl_renderer)
}
fn canvas_stable(value: &FingerprintCanvasDto) -> bool {
    value.read_hash == value.repeat_read_hash
        && value.read_hash == value.after_export_read_hash
        && value.export_hash.is_some()
        && value.export_hash == value.repeat_export_hash
}
fn audio_stable(value: &FingerprintAudioDto) -> bool {
    value.first_hash == value.repeat_hash
        && value.first_hash == value.first_event_hash
        && value.first_hash == value.repeat_event_hash
}
fn summary(value: &FingerprintSignalsDto) -> String {
    format!(
        "语言 {:?}；CPU {:?}；Memory {:?} ({})；屏幕 {:?}×{:?}；WebGL {:?}",
        value.languages,
        value.concurrency,
        value.memory,
        value.memory_type,
        value.screen_width,
        value.screen_height,
        value.webgl_vendor
    )
}
#[allow(clippy::too_many_arguments)]
fn check(
    key: String,
    label: String,
    scope: DiagnosticScopeDto,
    required: bool,
    status: Status,
    expected: Option<String>,
    observed: Option<String>,
    explanation: &str,
) -> FingerprintMeasurementCheckDto {
    FingerprintMeasurementCheckDto {
        key,
        label,
        scope,
        required,
        status,
        expected,
        observed,
        explanation: explanation.into(),
    }
}
fn result(value: bool) -> Status {
    if value {
        Status::Pass
    } else {
        Status::Fail
    }
}
fn first(sample: &FingerprintSampleDto) -> Option<&FingerprintObservationDto> {
    sample.observations.first()
}
pub(super) fn report(
    build: &BrowserBuild,
    samples: Vec<FingerprintSampleDto>,
) -> FingerprintMeasurementReportDto {
    let mut checks = Vec::new();
    for sample in &samples {
        let complete = sample.observations.len() == 2
            && sample
                .observations
                .iter()
                .enumerate()
                .all(|(i, row)| row.round == i as u32);
        let prefix = sample.label.to_ascii_lowercase();
        let observed = first(sample).map(|row| summary(&row.page));
        checks.push(check(format!("{prefix}.page"), format!("{} 主页面声明读数", sample.label), DiagnosticScopeDto::Configurable, true,
            result(complete && sample.observations.iter().all(|row| matches_config(&row.page, &sample.configured))),
            Some(format!("语言 {:?}；CPU {}；Memory {}；屏幕 {}×{}", sample.configured.languages, sample.configured.concurrency, sample.configured.memory, sample.configured.screen_width, sample.configured.screen_height)), observed,
            "首段脚本读取真实 API；比较 UA、语言、CPU、Memory 类型、Touch、Screen 和 WebGL 声明，不代表实际硬件。"));
        checks.push(check(
            format!("{prefix}.iframe"),
            format!("{} 同源 iframe", sample.label),
            DiagnosticScopeDto::Configurable,
            true,
            result(
                complete
                    && sample.observations.iter().all(|row| {
                        row.iframe
                            .as_ref()
                            .is_some_and(|frame| matches_config(frame, &sample.configured))
                    }),
            ),
            None,
            first(sample)
                .and_then(|row| row.iframe.as_ref())
                .map(summary),
            "iframe 在自身首段脚本采样；未使用父页面读数填充。",
        ));
        let network_ok = complete
            && sample.observations.iter().all(|row| {
                row.network.user_agent.as_ref() == Some(&sample.configured.user_agent)
                    && row
                        .network
                        .accept_language
                        .as_deref()
                        .and_then(|v| v.split(',').next())
                        .and_then(|v| v.split(';').next())
                        .is_some_and(|v| {
                            sample
                                .configured
                                .languages
                                .first()
                                .is_some_and(|expected| v.trim().eq_ignore_ascii_case(expected))
                        })
            });
        checks.push(check(format!("{prefix}.headers"), format!("{} HTTP UA / 语言", sample.label), DiagnosticScopeDto::Configurable, true, result(network_ok),
            Some(format!("UA 与配置一致；首选语言 {}", sample.configured.languages.first().map(String::as_str).unwrap_or("—"))), first(sample).map(|row| format!("UA {:?}；Accept-Language {:?}", row.network.user_agent, row.network.accept_language)),
            "由本地服务器独立记录主文档请求头，仅保留 UA / Accept-Language / 两项低熵 Client Hints。"));
        let canvas_ok = complete
            && sample
                .observations
                .iter()
                .all(|row| row.page.canvas.as_ref().is_some_and(canvas_stable))
            && first(sample)
                .and_then(|r| r.page.canvas.as_ref())
                .zip(
                    sample
                        .observations
                        .get(1)
                        .and_then(|r| r.page.canvas.as_ref()),
                )
                .is_some_and(|(a, b)| a.read_hash == b.read_hash && a.export_hash == b.export_hash);
        checks.push(check(format!("{prefix}.canvas"), format!("{} Canvas 重复 / 重启稳定性", sample.label), DiagnosticScopeDto::Configurable, true, result(canvas_ok), None,
            first(sample).and_then(|row| row.page.canvas.as_ref()).map(|v| format!("read {}；export {:?}", v.read_hash, v.export_hash)),
            "分别比较重复像素读取、重复 PNG 导出、导出后再次读取及正常退出后重启；不把 PNG 文本摘要当成像素摘要。"));
        let audio_ok = complete
            && sample
                .observations
                .iter()
                .all(|row| row.audio.as_ref().is_some_and(audio_stable))
            && first(sample)
                .and_then(|r| r.audio.as_ref())
                .zip(sample.observations.get(1).and_then(|r| r.audio.as_ref()))
                .is_some_and(|(a, b)| a.first_hash == b.first_hash);
        checks.push(check(format!("{prefix}.audio"), format!("{} 离线 Audio 重复 / 重启稳定性", sample.label), DiagnosticScopeDto::Configurable, true, result(audio_ok), None,
            first(sample).and_then(|row| row.audio.as_ref()).map(|v| format!("Promise {}；event {}", v.first_hash, v.first_event_hash)),
            "比较两个离线渲染、Promise 与完成事件，以及重启后的相同样本；不测真实音频设备或采样率伪装。"));
        let storage_ok = complete
            && sample.observations.iter().enumerate().all(|(i, row)| {
                row.storage.local_after == "own"
                    && row.storage.cookie_after == "own"
                    && row.storage.local_before == if i == 0 { "empty" } else { "own" }
                    && row.storage.cookie_before == if i == 0 { "empty" } else { "own" }
            });
        checks.push(check(
            format!("{prefix}.storage"),
            format!("{} Cookie / LocalStorage 隔离与重启", sample.label),
            DiagnosticScopeDto::Isolated,
            true,
            result(storage_ok),
            Some("首次为空；写入本样本标记；重启后只读取本样本标记".into()),
            first(sample).map(|row| {
                format!(
                    "首次 LocalStorage {}；Cookie {}",
                    row.storage.local_before, row.storage.cookie_before
                )
            }),
            "两个浏览器访问同一 localhost origin；只读写专用测试键，报告不返回 Cookie 或存储原文。",
        ));
        checks.push(check(
            format!("{prefix}.worker"),
            format!("{} Worker 读数", sample.label),
            DiagnosticScopeDto::HostShared,
            false,
            if first(sample).is_some_and(|r| r.worker.is_some()) {
                Status::NotCovered
            } else {
                Status::NotMeasured
            },
            None,
            first(sample)
                .and_then(|row| row.worker.as_ref())
                .map(summary),
            "当前生产扩展不覆盖 Worker；即使字段碰巧相等，也不算独立指纹已验证。",
        ));
        checks.push(check(
            format!("{prefix}.client_hints"),
            format!("{} UA-CH 读数", sample.label),
            DiagnosticScopeDto::HostShared,
            false,
            if first(sample).is_some_and(|r| r.page.client_hints.is_some()) {
                Status::NotCovered
            } else {
                Status::NotMeasured
            },
            None,
            first(sample).map(|row| {
                format!(
                    "页面 {:?}；请求头 {:?} / {:?}",
                    row.page.client_hints, row.network.sec_ch_ua, row.network.sec_ch_ua_platform
                )
            }),
            "记录页面与请求头的实际 Client Hints；尚未实现全链路覆盖。",
        ));
        checks.push(check(
            format!("{prefix}.window"),
            format!("{} 窗口与视口", sample.label),
            DiagnosticScopeDto::Configurable,
            false,
            Status::Observed,
            Some(format!(
                "请求窗口 {}×{}",
                sample.configured.window_width, sample.configured.window_height
            )),
            first(sample).map(|row| {
                format!(
                    "outer {:?}×{:?}；inner {:?}×{:?}；DPR {:?}",
                    row.page.outer_width,
                    row.page.outer_height,
                    row.page.inner_width,
                    row.page.inner_height,
                    row.page.pixel_ratio
                )
            }),
            "headless 实测尺寸；浏览器边框、视口与声明 Screen 是不同含义，不宣称可见窗口已验收。",
        ));
        checks.push(check(
            format!("{prefix}.timezone"),
            format!("{} 时区读数", sample.label),
            DiagnosticScopeDto::HostShared,
            false,
            Status::NotCovered,
            None,
            first(sample).and_then(|row| row.page.timezone.clone()),
            "时区继续来自主机；语言样本不代表地理位置一致性。",
        ));
    }
    let pair = samples.first().zip(samples.get(1));
    for (suffix, label, kind) in [
        ("canvas_difference", "两个 seed 的 Canvas 输出差异", 0),
        ("audio_difference", "两个 seed 的离线 Audio 输出差异", 1),
    ] {
        let different = pair
            .and_then(|(a, b)| first(a).zip(first(b)))
            .is_some_and(|(a, b)| {
                if kind == 0 {
                    a.page
                        .canvas
                        .as_ref()
                        .zip(b.page.canvas.as_ref())
                        .is_some_and(|(a, b)| {
                            a.read_hash != b.read_hash
                                && a.export_hash.is_some()
                                && b.export_hash.is_some()
                                && a.export_hash != b.export_hash
                        })
                } else {
                    a.audio
                        .as_ref()
                        .zip(b.audio.as_ref())
                        .is_some_and(|(a, b)| a.first_hash != b.first_hash)
                }
            });
        checks.push(check(
            suffix.into(),
            label.into(),
            DiagnosticScopeDto::Configurable,
            true,
            result(different),
            Some("本次两个固定样本的输出不同".into()),
            None,
            "只说明当前固定样本的输出差异，不保证任意 seed 或实际设备唯一。",
        ));
    }
    checks.push(check(
        "unmeasured".into(),
        "字体 / GPU 像素 / TLS / DNS / WebRTC".into(),
        DiagnosticScopeDto::HostShared,
        false,
        Status::NotMeasured,
        None,
        None,
        "本轮没有测这些维度；本机 HTTP 头部一致不等于网络出口隔离。",
    ));
    let covered_checks_passed = samples.len() == 2
        && checks
            .iter()
            .filter(|check| check.required)
            .all(|check| check.status == Status::Pass);
    FingerprintMeasurementReportDto {
        schema_version: 1,
        runtime_id: build.id.as_str().into(),
        runtime_version: build.version.clone(),
        runtime_sha256: build.sha256.clone(),
        measured_at_ms: now_ms(),
        mode: "headless".into(),
        covered_checks_passed,
        samples,
        checks,
        limitations: vec![
            "两个临时样本，不是已有环境或登录态的体检；结果仅对应本次运行时摘要。".into(),
            "覆盖项通过不等于完整独立设备、检测站通过率或可见窗口已验收。".into(),
            "Worker、UA-CH、时区和网络出口等边界仍需单独实现和验收。".into(),
        ],
        external_endpoints: Vec::new(),
    }
}
