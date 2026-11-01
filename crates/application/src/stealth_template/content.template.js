// Generated MAIN-world hooks. These change page reads, not the browser engine.
// No detection-site pass rate or native-function invisibility is promised.
(() => {
  const persona = JSON.parse('{{PERSONA_JSON}}');
  let seed = 0x811c9dc5;
  for (let i = 0; i < persona.seed.length; i++) {
    seed = Math.imul(seed ^ persona.seed.charCodeAt(i), 0x01000193) >>> 0;
  }
  const hash = (x, y, channel) => {
    let n = seed ^ Math.imul(x, 0x9e3779b1) ^ Math.imul(y, 0x85ebca77)
      ^ Math.imul(channel + 1, 0xc2b2ae3d);
    n = Math.imul(n ^ (n >>> 16), 0x85ebca6b);
    n = Math.imul(n ^ (n >>> 13), 0xc2b2ae35);
    return (n ^ (n >>> 16)) >>> 0;
  };

  // Keep native receiver checks, availability and descriptor attributes.
  const overrideGetter = (proto, name, value) => {
    try {
      const descriptor = Object.getOwnPropertyDescriptor(proto, name);
      if (!descriptor?.get || !descriptor.configurable) return;
      const original = descriptor.get;
      Object.defineProperty(proto, name, {
        ...descriptor,
        get() { original.call(this); return value; },
      });
    } catch (_) {}
  };
  if (typeof Navigator !== 'undefined') {
    const languages = Object.freeze([...persona.languages]);
    for (const [name, value] of [
      ['language', languages[0]], ['languages', languages],
      ['hardwareConcurrency', persona.hardware_concurrency],
      ['deviceMemory', persona.device_memory], ['maxTouchPoints', persona.touch_points],
      ['platform', persona.platform === 'windows' ? 'Win32'
        : persona.platform === 'mac' ? 'MacIntel' : 'Linux x86_64'],
    ]) overrideGetter(Navigator.prototype, name, value);
  }
  // devicePixelRatio stays native: the launch scale flag and browser zoom own it.
  // Do not fabricate chrome.runtime or replace WebRTC constructors.
  if (typeof Screen !== 'undefined') {
    for (const [name, value] of [
      ['width', persona.screen_width], ['height', persona.screen_height],
      ['availWidth', persona.screen_width], ['availHeight', persona.screen_height],
      ['colorDepth', persona.color_depth], ['pixelDepth', persona.color_depth],
    ]) overrideGetter(Screen.prototype, name, value);
  }

  // WebGL1 remains covered even when WebGL2 is absent. Only valid unmasked
  // reads are overridden; invalid enum/receiver behavior remains native.
  for (const name of ['WebGLRenderingContext', 'WebGL2RenderingContext']) {
    try {
      const proto = globalThis[name]?.prototype;
      if (!proto) continue;
      const original = proto.getParameter;
      proto.getParameter = function (...args) {
        const value = Reflect.apply(original, this, args);
        if (value === null || value === undefined) return value;
        if (args[0] === 37445) return persona.webgl_vendor;
        if (args[0] === 37446) return persona.webgl_renderer;
        return value;
      };
    } catch (_) {}
  }

  const perturb = (image, x, y) => {
    // Float16/HDR image data is intentionally untouched.
    if (!(image.data instanceof Uint8ClampedArray)) return image;
    for (let row = 0; row < image.height; row++) {
      for (let col = 0; col < image.width; col++) {
        const offset = (row * image.width + col) * 4;
        if (image.data[offset + 3] === 0) continue; // keep transparent black
        for (let channel = 0; channel < 3; channel++) {
          image.data[offset + channel] ^= hash(x + col, y + row, channel) & 1;
        }
      }
    }
    return image;
  };
  const canvasApis = [];
  if (persona.spoof_canvas) {
    for (const name of ['CanvasRenderingContext2D', 'OffscreenCanvasRenderingContext2D']) {
      try {
        const proto = globalThis[name]?.prototype;
        if (!proto) continue;
        const original = proto.getImageData;
        canvasApis.push({ proto, read: original, draw: proto.drawImage, write: proto.putImageData });
        proto.getImageData = function (...args) {
          const image = Reflect.apply(original, this, args);
          // Avoid coercing objects a second time (valueOf can have side effects).
          if (!args.slice(0, 4).every(v => typeof v === 'number' && Number.isFinite(v))) {
            return image;
          }
          const [x, y, w, h] = args.map(v => v | 0);
          return perturb(image, x + Math.min(0, w), y + Math.min(0, h));
        };
      } catch (_) {}
    }
    const exportCopy = (source, offscreen) => {
      if (!source.width || !source.height) return null;
      const copy = offscreen
        ? new OffscreenCanvas(source.width, source.height)
        : document.createElement('canvas');
      copy.width = source.width;
      copy.height = source.height;
      const context = copy.getContext('2d');
      const api = canvasApis.find(entry => entry.proto.isPrototypeOf(context));
      if (!api) return null;
      api.draw.call(context, source, 0, 0);
      const image = api.read.call(context, 0, 0, copy.width, copy.height);
      api.write.call(context, perturb(image, 0, 0), 0, 0);
      return copy;
    };
    // Export via a copy: reads never modify the original drawing or accumulate.
    // On tainted/detached/unsupported canvases, native export owns the error.
    for (const [name, methods, offscreen] of [
      ['HTMLCanvasElement', ['toDataURL', 'toBlob'], false],
      ['OffscreenCanvas', ['convertToBlob'], true],
    ]) {
      try {
        const ctor = globalThis[name];
        if (!ctor) continue;
        for (const method of methods) {
          const original = ctor.prototype[method];
          if (typeof original !== 'function') continue;
          ctor.prototype[method] = function (...args) {
            if (!(this instanceof ctor)) return Reflect.apply(original, this, args);
            let copy;
            try { copy = exportCopy(this, offscreen); } catch (_) {}
            return Reflect.apply(original, copy || this, args);
          };
        }
      } catch (_) {}
    }
  }

  // Perturb offline rendered output only. Real-time audio nodes, device sample
  // rate and live playback stay native. Promise and completion-event reads use
  // the same buffer and apply noise exactly once.
  if (persona.spoof_audio) {
    try {
      const proto = globalThis.OfflineAudioContext?.prototype;
      const read = globalThis.AudioBuffer?.prototype.getChannelData;
      if (proto && read) {
        const seen = new WeakSet();
        const perturbAudio = buffer => {
          if (seen.has(buffer)) return buffer;
          for (let channel = 0; channel < buffer.numberOfChannels; channel++) {
            const samples = read.call(buffer, channel);
            for (let i = 0; i < samples.length; i += 32) {
              if (samples[i] !== 0 && Number.isFinite(samples[i])) {
                samples[i] += (hash(i, 0, channel) & 1 ? 1 : -1) * 1e-7;
              }
            }
          }
          seen.add(buffer);
          return buffer;
        };
        const original = proto.startRendering;
        proto.startRendering = function (...args) {
          return Reflect.apply(original, this, args).then(perturbAudio);
        };
        const eventProto = globalThis.OfflineAudioCompletionEvent?.prototype;
        const descriptor = eventProto && Object.getOwnPropertyDescriptor(eventProto, 'renderedBuffer');
        if (descriptor?.get && descriptor.configurable) {
          Object.defineProperty(eventProto, 'renderedBuffer', {
            ...descriptor,
            get() { return perturbAudio(descriptor.get.call(this)); },
          });
        }
      }
    } catch (_) {}
  }
})();
