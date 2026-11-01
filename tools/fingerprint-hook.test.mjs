// Behavioral contracts of MAIN-world hooks, not engine coverage or pass rates.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const template = readFileSync(new URL('../crates/application/src/stealth_template/content.template.js', import.meta.url), 'utf8');
const defaults = {
  seed: 'regression-seed', platform: 'windows', languages: ['zh-CN', 'zh'],
  hardware_concurrency: 8, device_memory: 0.5, touch_points: 0,
  screen_width: 1920, screen_height: 1080, color_depth: 24, scale_factor: 1.25,
  webgl_vendor: 'Configured vendor', webgl_renderer: 'Configured renderer',
  spoof_canvas: true, spoof_audio: true,
};

function environment(overrides = {}, missing = []) {
  const persona = { ...defaults, ...overrides };
  const brands = Object.fromEntries(['navigator', 'screen', 'canvas', 'context', 'gl', 'audio', 'offline', 'event'].map(key => [key, new WeakSet()]));
  const calls = { reads: [], exports: [], audio: [], realtime: [] };
  function branded(key, value) { if (!brands[key].has(value)) throw new TypeError('Illegal invocation'); }
  class Navigator { constructor() { brands.navigator.add(this); } }
  class Screen { constructor() { brands.screen.add(this); } }
  for (const [ctor, key, values] of [
    [Navigator, 'navigator', { language: 'en-US', languages: ['en-US'], hardwareConcurrency: 2, deviceMemory: 8, maxTouchPoints: 1, platform: 'native' }],
    [Screen, 'screen', { width: 800, height: 600, availWidth: 790, availHeight: 580, colorDepth: 24, pixelDepth: 24 }],
  ]) for (const [name, value] of Object.entries(values)) Object.defineProperty(ctor.prototype, name, {
    configurable: true, enumerable: true, get() { branded(key, this); return value; },
  });
  class WebGLRenderingContext {
    constructor() { brands.gl.add(this); this.extensionAvailable = true; }
    getParameter(...args) {
      branded('gl', this);
      if ((args[0] === 37445 || args[0] === 37446) && !this.extensionAvailable) return null;
      return args[0] === 37445 ? 'Native vendor' : args[0] === 37446 ? 'Native renderer' : args[0] === 123 ? 64 : null;
    }
  }
  class WebGL2RenderingContext extends WebGLRenderingContext {}
  class CanvasRenderingContext2D {
    constructor(canvas) { brands.context.add(this); this.canvas = canvas; }
    getImageData(...args) {
      branded('context', this); calls.reads.push(args);
      if (this.canvas.tainted) throw new DOMException('Tainted canvas', 'SecurityError');
      let [x, y, width, height] = args.slice(0, 4).map(value => Number(value) | 0);
      if (!width || !height) throw new DOMException('Zero rectangle', 'IndexSizeError');
      x += Math.min(0, width); y += Math.min(0, height);
      width = Math.abs(width); height = Math.abs(height);
      const data = new Uint8ClampedArray(width * height * 4);
      for (let row = 0; row < height; row++) for (let col = 0; col < width; col++) {
        const sx = x + col, sy = y + row;
        if (sx >= 0 && sx < this.canvas.width && sy >= 0 && sy < this.canvas.height) {
          const offset = (sy * this.canvas.width + sx) * 4;
          data.set(this.canvas.pixels.subarray(offset, offset + 4), (row * width + col) * 4);
        }
      }
      return { width, height, data };
    }
    drawImage(source) { branded('context', this); branded('canvas', source); this.canvas.pixels.set(source.pixels); this.canvas.tainted = source.tainted; }
    putImageData(image, x, y) {
      branded('context', this);
      for (let row = 0; row < image.height; row++) for (let col = 0; col < image.width; col++) {
        this.canvas.pixels.set(image.data.subarray((row * image.width + col) * 4, (row * image.width + col + 1) * 4), ((y + row) * this.canvas.width + x + col) * 4);
      }
    }
  }
  // The browser exposes independent interfaces, not inherited Canvas methods.
  class OffscreenCanvasRenderingContext2D {
    constructor(canvas) { brands.context.add(this); this.canvas = canvas; }
  }
  for (const method of ['getImageData', 'drawImage', 'putImageData']) {
    Object.defineProperty(OffscreenCanvasRenderingContext2D.prototype, method,
      Object.getOwnPropertyDescriptor(CanvasRenderingContext2D.prototype, method));
  }
  function nativeExport(canvas, method, args) {
    branded('canvas', canvas); calls.exports.push({ method, args });
    if (canvas.tainted) throw new DOMException('Tainted canvas', 'SecurityError');
    return { type: typeof args[0] === 'string' ? args[0] : 'image/png', data: [...canvas.pixels] };
  }
  class HTMLCanvasElement {
    constructor(width = 6, height = 4) { brands.canvas.add(this); this._width = width; this._height = height; this.pixels = new Uint8ClampedArray(width * height * 4); this.tainted = false; }
    get width() { branded('canvas', this); return this._width; }
    set width(value) { branded('canvas', this); this._width = value; this.pixels = new Uint8ClampedArray(this._width * this._height * 4); }
    get height() { branded('canvas', this); return this._height; }
    set height(value) { branded('canvas', this); this._height = value; this.pixels = new Uint8ClampedArray(this._width * this._height * 4); }
    getContext(type) { branded('canvas', this); return type === '2d' ? (this.context ??= new CanvasRenderingContext2D(this)) : null; }
    toDataURL(...args) { return JSON.stringify(nativeExport(this, 'toDataURL', args)); }
    toBlob(callback, ...args) { const result = nativeExport(this, 'toBlob', args); queueMicrotask(() => callback(result)); }
  }
  class OffscreenCanvas extends HTMLCanvasElement {
    getContext(type) { branded('canvas', this); return type === '2d' ? (this.context ??= new OffscreenCanvasRenderingContext2D(this)) : null; }
    convertToBlob(...args) {
      try { return Promise.resolve(nativeExport(this, 'convertToBlob', args)); }
      catch (error) { return Promise.reject(error); }
    }
  }
  class AudioBuffer {
    constructor(channels = 2, length = 128) {
      brands.audio.add(this); this.numberOfChannels = channels;
      this.samples = Array.from({ length: channels }, (_, c) => Float32Array.from({ length }, (_, i) => 0.2 + c * 0.1 + i * 0.001));
    }
    getChannelData(channel) { branded('audio', this); return this.samples[channel]; }
  }
  class OfflineAudioCompletionEvent {
    constructor(buffer) { brands.event.add(this); this.buffer = buffer; }
    get renderedBuffer() { branded('event', this); return this.buffer; }
  }
  class OfflineAudioContext {
    constructor(buffer = new AudioBuffer()) { brands.offline.add(this); this.buffer = buffer; }
    startRendering(...args) {
      branded('offline', this); calls.audio.push(args);
      if (this.failure) return Promise.reject(this.failure);
      return Promise.resolve().then(() => {
        this.event = new OfflineAudioCompletionEvent(this.buffer);
        if (this.oncomplete) this.oncomplete(this.event);
        return this.buffer;
      });
    }
  }
  class AudioBufferSourceNode { start(...args) { calls.realtime.push(args); return 'native-start-result'; } }
  const globals = {
    Navigator, navigator: new Navigator(), Screen, screen: new Screen(),
    WebGLRenderingContext, WebGL2RenderingContext, CanvasRenderingContext2D,
    OffscreenCanvasRenderingContext2D, HTMLCanvasElement, OffscreenCanvas,
    AudioBuffer, OfflineAudioContext, OfflineAudioCompletionEvent, AudioBufferSourceNode,
    Uint8ClampedArray, Float32Array, DOMException, chrome: { app: { nativeSentinel: true } },
    devicePixelRatio: 2, document: { createElement(name) { assert.equal(name, 'canvas'); return new HTMLCanvasElement(); } },
  };
  for (const key of missing) delete globals[key];
  globals.window = globals;
  const originalRealtime = AudioBufferSourceNode.prototype.start;
  const originalDpr = Object.getOwnPropertyDescriptor(globals, 'devicePixelRatio');
  const context = vm.createContext(globals);
  const serialized = JSON.stringify(persona).replaceAll('\\', '\\\\').replaceAll("'", "\\'");
  vm.runInContext(template.replace('{{PERSONA_JSON}}', serialized), context, { filename: 'fingerprint-content.js' });
  return { ...globals, persona, calls, context, originalRealtime, originalDpr };
}

function populatedCanvas(env, offscreen = false) {
  const canvas = offscreen ? new env.OffscreenCanvas(6, 4) : new env.HTMLCanvasElement(6, 4);
  for (let i = 0; i < canvas.pixels.length; i += 4) canvas.pixels.set([40 + (i % 150), 100, 180, 255], i);
  canvas.pixels.set([0, 0, 0, 0], 0); canvas.pixels[7] = 123;
  return canvas;
}
function crop(image, x, y, width, height) {
  const result = [];
  for (let row = 0; row < height; row++) for (let col = 0; col < width; col++) {
    result.push(...image.data.subarray(((y + row) * image.width + x + col) * 4, ((y + row) * image.width + x + col + 1) * 4));
  }
  return result;
}
const samplesOf = buffer => buffer.samples.map(samples => [...samples]);

test('navigator native types, primary language, frozen languages and getter brand checks', () => {
  const env = environment();
  assert.equal(typeof env.navigator.deviceMemory, 'number'); assert.equal(env.navigator.deviceMemory, 0.5);
  assert.deepEqual([...env.navigator.languages], ['zh-CN', 'zh']);
  assert.equal(env.navigator.language, env.navigator.languages[0]); assert.ok(Object.isFrozen(env.navigator.languages));
  for (const name of ['language', 'languages', 'hardwareConcurrency', 'deviceMemory', 'maxTouchPoints', 'platform']) {
    const descriptor = Object.getOwnPropertyDescriptor(env.Navigator.prototype, name);
    assert.equal(descriptor.enumerable, true); assert.equal(descriptor.configurable, true);
    assert.throws(() => descriptor.get.call({}), TypeError);
  }
  assert.equal(env.navigator.platform, 'Win32');
  assert.equal(environment({ platform: 'mac' }).navigator.platform, 'MacIntel');
  assert.equal(environment({ platform: 'linux' }).navigator.platform, 'Linux x86_64');
});

test('screen receiver checks and native DPR, chrome and realtime audio remain intact', () => {
  const env = environment();
  assert.equal(env.screen.width, 1920); assert.equal(env.screen.height, 1080);
  assert.throws(() => Object.getOwnPropertyDescriptor(env.Screen.prototype, 'width').get.call({}), TypeError);
  assert.deepEqual(Object.getOwnPropertyDescriptor(env, 'devicePixelRatio'), env.originalDpr);
  assert.equal(env.devicePixelRatio, 2); assert.deepEqual(env.chrome, { app: { nativeSentinel: true } });
  assert.equal(env.AudioBufferSourceNode.prototype.start, env.originalRealtime);
  const marker = {};
  assert.equal(new env.AudioBufferSourceNode().start(marker, 1), 'native-start-result');
  assert.deepEqual(env.calls.realtime, [[marker, 1]]);
});

test('WebGL1 without WebGL2 preserves null, ordinary parameters and receiver validation', () => {
  const env = environment({}, ['WebGL2RenderingContext']), gl = new env.WebGLRenderingContext();
  assert.equal(gl.getParameter(37445), 'Configured vendor'); assert.equal(gl.getParameter(37446), 'Configured renderer');
  assert.equal(gl.getParameter(123), 64); assert.equal(gl.getParameter(999), null);
  gl.extensionAvailable = false; assert.equal(gl.getParameter(37445), null);
  assert.throws(() => env.WebGLRenderingContext.prototype.getParameter.call({}, 37445), TypeError);
});

test('Canvas reads are stable and coordinate-consistent, preserve alpha and never mutate source', () => {
  const env = environment(), canvas = populatedCanvas(env), context = canvas.getContext('2d');
  const original = [...canvas.pixels], full = context.getImageData(0, 0, 6, 4);
  assert.deepEqual([...context.getImageData(0, 0, 6, 4).data], [...full.data]);
  assert.deepEqual([...context.getImageData(2, 1, 3, 2).data], crop(full, 2, 1, 3, 2));
  assert.deepEqual([...context.getImageData(5, 3, -3, -2).data], crop(full, 2, 1, 3, 2));
  assert.deepEqual([...full.data.slice(0, 4)], [0, 0, 0, 0]);
  let changed = 0;
  for (let i = 0; i < original.length; i++) {
    if (i % 4 === 3) assert.equal(full.data[i], original[i]);
    else { assert.ok(Math.abs(full.data[i] - original[i]) <= 1); changed += Number(full.data[i] !== original[i]); }
  }
  assert.ok(changed > 0); assert.deepEqual([...canvas.pixels], original);
  const other = environment({ seed: 'different-seed' });
  assert.notDeepEqual([...populatedCanvas(other).getContext('2d').getImageData(0, 0, 6, 4).data], [...full.data]);
  assert.deepEqual([...context.getImageData(-1, -1, 2, 2).data], Array(16).fill(0));
});

test('Canvas and Offscreen exports match reads, preserve args and do not accumulate noise', async () => {
  const env = environment(), canvas = populatedCanvas(env), original = [...canvas.pixels];
  const expected = [...canvas.getContext('2d').getImageData(0, 0, 6, 4).data];
  assert.deepEqual(JSON.parse(canvas.toDataURL('image/jpeg', 0.8)).data, expected);
  assert.deepEqual(JSON.parse(canvas.toDataURL()).data, expected);
  const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/webp', 0.7));
  assert.deepEqual(blob.data, expected);
  assert.deepEqual(env.calls.exports[0].args, ['image/jpeg', 0.8]);
  assert.deepEqual(env.calls.exports[2].args, ['image/webp', 0.7]);
  assert.deepEqual([...canvas.pixels], original);
  const offscreen = populatedCanvas(env, true), options = { type: 'image/png' };
  assert.deepEqual([...offscreen.getContext('2d').getImageData(0, 0, 6, 4).data], expected);
  assert.deepEqual((await offscreen.convertToBlob(options)).data, expected);
  assert.equal(env.calls.exports.at(-1).args[0], options); assert.deepEqual([...offscreen.pixels], original);
});

test('Canvas preserves taint, native exceptions, coercion count and disabled behavior', async () => {
  const env = environment(), canvas = populatedCanvas(env), context = canvas.getContext('2d');
  assert.throws(() => env.CanvasRenderingContext2D.prototype.getImageData.call({}, 0, 0, 1, 1), TypeError);
  assert.throws(() => context.getImageData(0, 0, 0, 1), { name: 'IndexSizeError' });
  assert.throws(() => env.HTMLCanvasElement.prototype.toDataURL.call({}), TypeError);
  let coerced = 0; const x = { valueOf() { coerced++; return 0; } };
  const read = context.getImageData(x, 0, 6, 4, { colorSpace: 'srgb' });
  assert.equal(coerced, 1); assert.deepEqual([...read.data], [...canvas.pixels]);
  canvas.tainted = true;
  assert.throws(() => context.getImageData(0, 0, 1, 1), { name: 'SecurityError' });
  assert.throws(() => canvas.toDataURL(), { name: 'SecurityError' });
  assert.throws(() => canvas.toBlob(() => {}), { name: 'SecurityError' });
  const offscreen = populatedCanvas(env, true); offscreen.tainted = true;
  await assert.rejects(offscreen.convertToBlob(), { name: 'SecurityError' });
  const disabled = environment({ spoof_canvas: false }), nativeCanvas = populatedCanvas(disabled);
  assert.deepEqual([...nativeCanvas.getContext('2d').getImageData(0, 0, 6, 4).data], [...nativeCanvas.pixels]);
  assert.deepEqual(JSON.parse(nativeCanvas.toDataURL()).data, [...nativeCanvas.pixels]);
});

test('Offline audio Promise and event share a single perturbation; arguments and errors survive', async () => {
  const env = environment(), buffer = new env.AudioBuffer(), original = samplesOf(buffer);
  const context = new env.OfflineAudioContext(buffer), marker = {};
  let eventBuffer, eventSamples;
  context.oncomplete = event => { eventBuffer = event.renderedBuffer; eventSamples = samplesOf(eventBuffer); };
  const result = await context.startRendering(marker, 123);
  assert.equal(result, buffer); assert.equal(eventBuffer, result);
  assert.deepEqual(samplesOf(result), eventSamples); assert.notDeepEqual(samplesOf(result), original);
  assert.deepEqual(env.calls.audio, [[marker, 123]]);
  const after = samplesOf(result);
  assert.equal(context.event.renderedBuffer, result); assert.equal(await context.startRendering(), result);
  assert.deepEqual(samplesOf(result), after);
  for (let c = 0; c < buffer.numberOfChannels; c++) for (let i = 0; i < buffer.samples[c].length; i++) {
    const delta = Math.abs(after[c][i] - original[c][i]); assert.ok(delta <= 2e-7);
    if (i % 32 !== 0) assert.equal(delta, 0);
  }
  const failure = new Error('native rendering failure'); context.failure = failure;
  await assert.rejects(context.startRendering('rejected-arg'), error => error === failure);
  assert.throws(() => env.OfflineAudioContext.prototype.startRendering.call({}), TypeError);
  assert.throws(() => Object.getOwnPropertyDescriptor(env.OfflineAudioCompletionEvent.prototype, 'renderedBuffer').get.call({}), TypeError);
});

test('Offline audio is seed-stable, leaves silence/nonfinite values intact and honors disabled flag', async () => {
  async function rendered(seed) { const env = environment({ seed }); return samplesOf(await new env.OfflineAudioContext().startRendering()); }
  assert.deepEqual(await rendered('audio-seed-1'), await rendered('audio-seed-1'));
  assert.notDeepEqual(await rendered('audio-seed-1'), await rendered('audio-seed-2'));
  const env = environment(), buffer = new env.AudioBuffer();
  buffer.samples.forEach(channel => channel.fill(0)); buffer.samples[0][32] = Infinity; buffer.samples[0][64] = NaN;
  const original = samplesOf(buffer); await new env.OfflineAudioContext(buffer).startRendering();
  assert.deepEqual(samplesOf(buffer), original);
  const disabled = environment({ spoof_audio: false }), nativeBuffer = new disabled.AudioBuffer(), nativeOriginal = samplesOf(nativeBuffer);
  await new disabled.OfflineAudioContext(nativeBuffer).startRendering(); assert.deepEqual(samplesOf(nativeBuffer), nativeOriginal);
});

test('missing optional APIs do not prevent other available hooks from applying', () => {
  const env = environment({}, ['WebGL2RenderingContext', 'OffscreenCanvas', 'OffscreenCanvasRenderingContext2D', 'OfflineAudioContext', 'AudioBuffer', 'OfflineAudioCompletionEvent']);
  assert.equal(env.navigator.deviceMemory, 0.5);
  assert.equal(new env.WebGLRenderingContext().getParameter(37445), 'Configured vendor');
  const canvas = populatedCanvas(env);
  assert.notDeepEqual([...canvas.getContext('2d').getImageData(0, 0, 6, 4).data], [...canvas.pixels]);
});
