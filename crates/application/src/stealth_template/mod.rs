//! Static MV3 stealth extension templates.
//!
//! Templates contain `{{profile_id}}` and `{{PERSONA_JSON}}` placeholders that
//! `SeedableNoiseFingerprintRuntime::materialize_extension` substitutes at
//! materialization time.  The JSON value is escaped so it is safe to embed
//! inside a single-quoted JavaScript string literal.

pub const MANIFEST_TEMPLATE: &str = include_str!("manifest.template.json");
pub const CONTENT_TEMPLATE: &str = include_str!("content.template.js");
pub const BACKGROUND_TEMPLATE: &str = include_str!("background.template.js");

/// Renders `manifest.template.json` for the given profile id.
pub fn render_manifest(profile_id: &str) -> String {
    let escaped = serde_json::to_string(profile_id).expect("strings serialize as JSON");
    MANIFEST_TEMPLATE.replace("{{profile_id}}", &escaped[1..escaped.len() - 1])
}

/// Renders `content.template.js` for the given persona, escaping the JSON value
/// for embedding inside a single-quoted JavaScript string literal.
pub fn render_content(persona_json: &str) -> String {
    // Escape characters that would break out of the single-quoted string.
    let escaped = persona_json
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    CONTENT_TEMPLATE.replace("{{PERSONA_JSON}}", &escaped)
}

/// Renders `background.template.js` (no placeholders; identity).
pub fn render_background() -> String {
    BACKGROUND_TEMPLATE.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_substitutes_profile_id() {
        let rendered = render_manifest("abc123");
        assert!(rendered.contains("\"name\": \"Brows Stealth for abc123\""));
        assert!(!rendered.contains("{{profile_id}}"));
    }

    #[test]
    fn content_substitutes_persona_json() {
        let persona_json = "{\"seed\":\"x\"}";
        let rendered = render_content(persona_json);
        assert!(rendered.contains("\"seed\":\"x\""));
        assert!(!rendered.contains("{{PERSONA_JSON}}"));
    }

    #[test]
    fn content_escapes_dangerous_json_characters() {
        // \n here is the raw newline byte (0x0A), not a literal backslash-n pair.
        let persona_json = "{\"x\":\"a'b\nc\"}";
        let rendered = render_content(persona_json);
        // After escaping:
        //   '       → \' (backslash + quote)
        //   newline → \n  (backslash + n)
        assert!(rendered.contains("a\\'b\\nc"));
    }

    #[test]
    fn background_renders_to_static_template() {
        let a = render_background();
        let b = render_background();
        assert_eq!(a, b);
        assert!(a.contains("self.addEventListener"));
    }
}
