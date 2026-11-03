//! One fixed rendering hook for page and Worker realms; no caller-supplied code.
use crate::{persona_fingerprint::PersonaFingerprint, stealth_template, PortError, PortErrorKind, PortResult};
use brows_domain::{LaunchArgument, LocaleConfig, SeedableNoisePersona, SessionFingerprintConfig};

pub fn render_session_fingerprint(policy: &SessionFingerprintConfig, version: &str) -> PortResult<String> {
    LaunchArgument::SessionFingerprint(Box::new(policy.clone())).validate()
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
    let fingerprint = PersonaFingerprint::derive(&policy.persona, version)
        .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
    if fingerprint.user_agent != policy.user_agent {
        return Err(PortError::new(PortErrorKind::InvalidData, "session identity does not match the selected runtime and persona"));
    }
    Ok(render_persona_content(&policy.persona, &fingerprint, &LocaleConfig {
        accept_languages: policy.languages.clone(), timezone: policy.timezone.clone(),
    }))
}

pub(crate) fn render_persona_content(persona: &SeedableNoisePersona, fp: &PersonaFingerprint, locale: &LocaleConfig) -> String {
    let data = serde_json::json!({
        "seed": persona.seed,
        "platform": persona.platform_string_for_js(),
        "hardware_concurrency": fp.hardware_concurrency,
        "device_memory": fp.device_memory_number(),
        "languages": locale.accept_languages,
        "touch_points": fp.touch_points,
        "screen_width": persona.screen.width,
        "screen_height": persona.screen.height,
        "color_depth": persona.screen.color_depth,
        "device_scale_factor": persona.screen.device_scale_factor_hundredths as f64 / 100.0,
        "webgl_vendor": fp.webgl_vendor,
        "webgl_renderer": fp.webgl_renderer,
        "spoof_canvas": persona.spoof_canvas,
        "spoof_audio": persona.spoof_audio,
        "webrtc_policy": "none",
    }).to_string();
    stealth_template::render_content(&data)
}
