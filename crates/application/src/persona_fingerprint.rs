//! Deterministic fingerprint derivation for the SeedableNoise persona.
//!
//! All output is a pure function of the seed — same seed always produces the
//! same reported properties.  No randomness is introduced at runtime.

use brows_domain::{DeviceMemoryGb, DomainError, PlatformKind, SeedableNoisePersona, WebGLProfile};

/// The full set of navigator/GPU properties that will be reported to pages.
/// Domain-side f64 is reported as a string to keep `Eq` sound for these values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaFingerprint {
    pub user_agent: String,
    /// `navigator.platform` string (e.g. "Win32", "MacIntel").
    pub platform_string: &'static str,
    pub webgl_vendor: String,
    pub webgl_renderer: String,
    pub hardware_concurrency: u8,
    /// Display string for device memory: "0.25", "0.5", "1", "2", "4", "8".
    pub device_memory_gb: String,
    pub touch_points: u8,
    pub chrome_version: &'static str,
}

impl PersonaFingerprint {
    /// Derives a `PersonaFingerprint` from the given `SeedableNoisePersona`.
    /// The derivation is deterministic: equal seeds produce equal output.
    pub fn derive(p: &SeedableNoisePersona) -> Result<Self, DomainError> {
        let mut rng = Splitmix64::from_seed(&p.seed);

        // ── Chrome version pool ──────────────────────────────────────────────
        // Only include versions still within Chrome's active support window.
        // Update this list when new Chrome majors ship.
        const CHROME_VERSIONS: &[&str] = &["136", "137", "138", "139", "150"];
        let chrome_version = CHROME_VERSIONS[rng.next_usize(CHROME_VERSIONS.len())];

        // ── Platform token for UA string ─────────────────────────────────────
        let (platform_token, platform_string) = match p.platform {
            PlatformKind::Windows => ("Windows NT 10.0; Win64; x64", "Win32"),
            PlatformKind::MacOs => ("Macintosh; Intel Mac OS X 10_15_7", "MacIntel"),
            PlatformKind::Linux => ("X11; Linux x86_64", "Linux x86_64"),
        };
        let user_agent = format!(
            "Mozilla/5.0 ({platform_token}) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/{chrome_version}.0.0.0 Safari/537.36"
        );

        // ── WebGL vendor / renderer ──────────────────────────────────────────
        let (webgl_vendor, webgl_renderer) = pick_webgl(&p.webgl, p.platform, &mut rng)?;

        Ok(Self {
            user_agent,
            platform_string,
            webgl_vendor,
            webgl_renderer,
            hardware_concurrency: p.hardware_concurrency,
            device_memory_gb: device_memory_to_string(p.device_memory),
            touch_points: p.touch_points,
            chrome_version,
        })
    }

    /// Returns a UA whose Chrome major matches the selected runtime. If the
    /// runtime does not expose a numeric major, the deterministic fallback UA
    /// is preserved.
    pub fn user_agent_for_chromium(&self, runtime_version: &str) -> String {
        let runtime_major = runtime_version.split('.').next().unwrap_or_default();
        if runtime_major.is_empty() || !runtime_major.chars().all(|ch| ch.is_ascii_digit()) {
            return self.user_agent.clone();
        }
        self.user_agent.replacen(
            &format!("Chrome/{}.0.0.0", self.chrome_version),
            &format!("Chrome/{runtime_major}.0.0.0"),
            1,
        )
    }
}

fn device_memory_to_string(m: DeviceMemoryGb) -> String {
    match m {
        DeviceMemoryGb::Quarter => "0.25".to_owned(),
        DeviceMemoryGb::Half => "0.5".to_owned(),
        DeviceMemoryGb::One => "1".to_owned(),
        DeviceMemoryGb::Two => "2".to_owned(),
        DeviceMemoryGb::Four => "4".to_owned(),
        DeviceMemoryGb::Eight => "8".to_owned(),
    }
}

// ── WebGL candidate pools ─────────────────────────────────────────────────────
// Each entry is (vendor, unmasked_renderer).
// Only real hardware strings are listed; no fabricated values.

const WEBGL_WINDOWS: &[(&str, &str)] = &[
    (
        "Google Inc. (NVIDIA)",
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "Google Inc. (NVIDIA)",
        "ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 Super Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "Google Inc. (Intel)",
        "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "Google Inc. (Intel)",
        "ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
    (
        "Google Inc. (AMD)",
        "ANGLE (AMD, AMD Radeon RX 6600 XT Direct3D11 vs_5_0 ps_5_0, D3D11)",
    ),
];

const WEBGL_MACOS: &[(&str, &str)] = &[
    ("Apple Inc.", "Apple M1"),
    ("Apple Inc.", "Apple M2"),
    ("Apple Inc.", "Apple M3"),
    ("Intel Inc.", "Intel Iris Plus Graphics OpenGL Engine"),
];

const WEBGL_LINUX: &[(&str, &str)] = &[
    ("Mesa/X.org", "Mesa Intel(R) UHD Graphics 620 (KBL GT2)"),
    ("NVIDIA Corporation", "NVIDIA GeForce RTX 3060/PCIe/SSE2"),
    ("VMware, Inc.", "SVGA3D; build: RELEASE; LLVM;"),
];

fn pick_webgl(
    override_: &WebGLProfile,
    platform: PlatformKind,
    rng: &mut Splitmix64,
) -> Result<(String, String), DomainError> {
    if !override_.vendor.is_empty() {
        // User manually set vendor/renderer; validation already ran in domain.
        return Ok((
            override_.vendor.clone(),
            override_.unmasked_renderer.clone(),
        ));
    }
    let pool: &[(&str, &str)] = match platform {
        PlatformKind::Windows => WEBGL_WINDOWS,
        PlatformKind::MacOs => WEBGL_MACOS,
        PlatformKind::Linux => WEBGL_LINUX,
    };
    let (v, r) = pool[rng.next_usize(pool.len())];
    Ok((v.to_owned(), r.to_owned()))
}

// ── splitmix64 PRNG ───────────────────────────────────────────────────────────

/// A deterministic 64-bit PRNG seeded from an arbitrary string.
/// Used solely for deriving fingerprint values; not for cryptographic purposes.
pub(crate) struct Splitmix64(u64);

impl Splitmix64 {
    pub fn from_seed(seed: &str) -> Self {
        Self(fnv1a64(seed.as_bytes()))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    pub fn next_usize(&mut self, len: usize) -> usize {
        debug_assert!(len > 0, "pool cannot be empty");
        (self.next_u64() as usize) % len
    }
}

/// FNV-1a 64-bit hash (no crate dependency needed for domain types).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_domain::{DeviceMemoryGb, ScreenConfig};

    fn sample_persona(seed: &str) -> SeedableNoisePersona {
        SeedableNoisePersona {
            seed: seed.to_owned(),
            platform: PlatformKind::Windows,
            hardware_concurrency: 8,
            device_memory: DeviceMemoryGb::Eight,
            touch_points: 0,
            screen: ScreenConfig {
                width: 1920,
                height: 1080,
                color_depth: 24,
                device_scale_factor_hundredths: 100,
            },
            webgl: WebGLProfile::auto(),
            spoof_canvas: false,
            spoof_audio: false,
        }
    }

    #[test]
    fn derive_is_deterministic_for_same_seed() {
        let p = sample_persona("stable-seed");
        let a = PersonaFingerprint::derive(&p).unwrap();
        let b = PersonaFingerprint::derive(&p).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn derive_produces_different_output_for_different_seeds() {
        let a = PersonaFingerprint::derive(&sample_persona("seed-alpha")).unwrap();
        let b = PersonaFingerprint::derive(&sample_persona("seed-beta")).unwrap();
        // It's astronomically unlikely they collide on all fields simultaneously.
        assert!(a.user_agent != b.user_agent || a.webgl_vendor != b.webgl_vendor);
    }

    #[test]
    fn windows_persona_picks_windows_webgl() {
        for seed in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            let fp = PersonaFingerprint::derive(&sample_persona(seed)).unwrap();
            assert!(
                fp.webgl_vendor.contains("NVIDIA")
                    || fp.webgl_vendor.contains("Intel")
                    || fp.webgl_vendor.contains("AMD"),
                "unexpected webgl vendor for Windows: {}",
                fp.webgl_vendor
            );
        }
    }

    #[test]
    fn user_agent_uses_the_selected_runtime_major() {
        let fp = PersonaFingerprint::derive(&sample_persona("runtime-major")).unwrap();
        assert!(fp
            .user_agent_for_chromium("151.0.8123.9")
            .contains("Chrome/151.0.0.0"));
        assert_eq!(fp.user_agent_for_chromium("installed"), fp.user_agent);
    }
}
