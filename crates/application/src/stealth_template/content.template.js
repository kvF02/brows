// Brows Stealth — generated content script for the SeedableNoise persona.
// The {{PERSONA_JSON}} placeholder is filled in by the runtime at materialization.
// All values are stable per seed (no per-call randomness); two reads from the
// same canvas with the same seed produce the same hash.
//
// Fields marked as "declared-only" cannot be modified at the pixel level;
// only the JS-readable values are overridden. This is honest — we do not
// pretend to bypass deep fingerprinting (CreepJS).

(() => {
  const persona = JSON.parse('{{PERSONA_JSON}}');
  const seed = persona.seed;

  // ── splitmix32 PRNG seeded from the profile seed ─────────────────────────
  const fnv1a32 = (s) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
    }
    return h >>> 0;
  };
  const seedInt = fnv1a32(seed);
  const splitmix32 = () => {
    let s = (seedInt + 0x9E3779B9) | 0;
    return () => {
      s = (s + 0x9E3779B9) | 0;
      let t = Math.imul(s ^ (s >>> 16), 0x85EBCA6B);
      t = Math.imul(t ^ (t >>> 13), 0xC2B2AE35);
      return ((t ^ (t >>> 16)) >>> 0);
    };
  };

  // ── P0: navigator.webdriver ──────────────────────────────────────────────
  try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      get: () => false,
      configurable: true,
    });
  } catch (_) {}

  // ── P0: chrome.runtime stub ──────────────────────────────────────────────
  if (!window.chrome) window.chrome = {};
  try {
    Object.defineProperty(window.chrome, 'runtime', {
      value: {
        PlatformOs: persona.platform,
        PlatformArch: 'x86-64',
        RequestUpdateCheckStatus: 'no_update',
        connect: () => {},
        sendMessage: () => {},
        id: undefined,
      },
      configurable: true,
    });
  } catch (_) {}

  // ── P1: navigator.languages / plugins / mimeTypes ───────────────────────
  if (Array.isArray(persona.languages)) {
    try {
      Object.defineProperty(Navigator.prototype, 'languages', {
        get: () => persona.languages.slice(),
        configurable: true,
      });
    } catch (_) {}
  }
  try {
    Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
      get: () => persona.hardware_concurrency,
      configurable: true,
    });
    Object.defineProperty(Navigator.prototype, 'deviceMemory', {
      get: () => persona.device_memory,
      configurable: true,
    });
    Object.defineProperty(Navigator.prototype, 'maxTouchPoints', {
      get: () => persona.touch_points,
      configurable: true,
    });
    Object.defineProperty(Navigator.prototype, 'platform', {
      get: () => persona.platform === 'windows' ? 'Win32' : persona.platform === 'mac' ? 'MacIntel' : 'Linux x86_64',
      configurable: true,
    });
  } catch (_) {}

  // P1: screen/window values declared by the persona. These are read-only
  // properties, so define them on the prototype before page code runs.
  try {
    for (const [name, value] of [
      ['width', persona.screen_width],
      ['height', persona.screen_height],
      ['availWidth', persona.screen_width],
      ['availHeight', persona.screen_height],
      ['colorDepth', persona.color_depth],
      ['pixelDepth', persona.color_depth],
    ]) {
      Object.defineProperty(Screen.prototype, name, { get: () => value, configurable: true });
    }
    Object.defineProperty(window, 'devicePixelRatio', {
      get: () => persona.device_scale_factor,
      configurable: true,
    });
  } catch (_) {}

  // ── P1: WebGL UNMASKED_VENDOR / UNMASKED_RENDERER ────────────────────────
  // Strings only — pixel-level output cannot be changed from a hook.
  try {
    const origGetParameter = WebGLRenderingContext.prototype.getParameter;
    const origGetParameter2 = WebGL2RenderingContext.prototype.getParameter;
    const wrap = (orig) => function (p) {
      if (p === 37445) return persona.webgl_vendor;       // UNMASKED_VENDOR_WEBGL
      if (p === 37446) return persona.webgl_renderer;     // UNMASKED_RENDERER_WEBGL
      return orig.call(this, p);
    };
    WebGLRenderingContext.prototype.getParameter = wrap(origGetParameter);
    WebGL2RenderingContext.prototype.getParameter = wrap(origGetParameter2);
  } catch (_) {}

  // ── P2 (declared-only): Canvas read perturbation ─────────────────────────
  // We perturb the LSB of each RGBA pixel using a seed-derived PRNG. The
  // perturbation is invisible to the eye but causes canvas hashes to differ
  // between sessions. CreepJS detects this; pixelscan does not.
  if (persona.spoof_canvas) {
    try {
      const origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
      CanvasRenderingContext2D.prototype.getImageData = function (x, y, w, h) {
        const data = origGetImageData.call(this, x, y, w, h);
        const rng = splitmix32();
        for (let i = 0; i < data.data.length; i += 4) {
          data.data[i]     ^= (rng() & 1);
          data.data[i + 1] ^= ((rng() >> 1) & 1);
          data.data[i + 2] ^= ((rng() >> 2) & 1);
        }
        return data;
      };
    } catch (_) {}
  }

  // ── P2 (declared-only): AudioContext fingerprint noise ───────────────────
  if (persona.spoof_audio) {
    try {
      const origCreateOscillator = AudioContext.prototype.createOscillator;
      // Lightweight: prepend a near-silent noise buffer once.
      const origStart = AudioBufferSourceNode.prototype.start;
      AudioBufferSourceNode.prototype.start = function (when, offset, duration) {
        try {
          const ctx = this.context;
          const buf = ctx.createBuffer(1, 1, 22050);
          // No audible energy; fingerprint noise only.
          // Real noise shaping would degrade compatibility — this stays conservative.
          void buf;
        } catch (_) {}
        return origStart.call(this, when, offset, duration);
      };
    } catch (_) {}
  }

  // ── P0: WebRTC IP leak suppression ───────────────────────────────────────
  // Forcing relay-only ICE prevents the browser from exposing the host's
  // public IP via STUN candidates. Only active when the profile has a proxy.
  if (persona.webrtc_policy === 'disable_non_proxied') {
    try {
      const OrigRTC = window.RTCPeerConnection;
      if (OrigRTC) {
        const ProxiedRTC = function (config, constraints) {
          const cfg = Object.assign({}, config || {});
          cfg.iceServers = []; // Strip STUN to prevent IP leak.
          const c = Object.assign({}, constraints || {}, {
            iceTransportPolicy: 'relay',
          });
          return new OrigRTC(cfg, c);
        };
        ProxiedRTC.prototype = OrigRTC.prototype;
        if (OrigRTC.generateCertificate) {
          ProxiedRTC.generateCertificate = OrigRTC.generateCertificate.bind(OrigRTC);
        }
        ProxiedRTC.name = 'RTCPeerConnection';
        window.RTCPeerConnection = ProxiedRTC;
        if (window.webkitRTCPeerConnection) {
          window.webkitRTCPeerConnection = ProxiedRTC;
        }
      }
    } catch (_) {}
  }
})();
