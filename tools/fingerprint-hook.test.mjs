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

function environment(overrides = {}, missing = [], worker = false) {
  const persona = { ...defaults, ...overrides };
  const brands = Object.fromEntries(['navigator', 'screen', 'canvas', 'context', 'gl', 'audio', 'offline', 'event'].map(key => [key, new WeakSet()]));
  const calls = { reads: [], exports: [], audio: [], realtime: [], glReads: [], glErrors: 0, renderbufferBinds: [] };
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
    constructor(canvas, second = false) {
      brands.gl.add(this); this.extensionAvailable = true; this.canvas = canvas;
      this.second = second; this.errors = new Set(); this.lost = false; this.colorSpace = 'srgb';
      this.parameters = new Map([[36006,null],[36010,null],[36007,null],[36063,8],[35053,null],[3074,1029],
        [3330,0],[3331,0],[3332,0],[3333,4],[35738,5121],[35739,6408]]);
    }
    get drawingBufferWidth() { branded('gl', this); return this.canvas?.width ?? 6; }
    get drawingBufferHeight() { branded('gl', this); return this.canvas?.height ?? 4; }
    get drawingBufferColorSpace() { branded('gl', this); return this.colorSpace; }
    isContextLost() { branded('gl', this); return this.lost; }
    get error() { return this.errors.values().next().value ?? 0; }
    set error(value) { if (value) this.errors.add(value); }
    getError() { branded('gl', this); calls.glErrors++; const error = this.error; this.errors.delete(error); return error; }
    checkFramebufferStatus() { branded('gl', this); return this.incomplete ? 0x8cd6 : 0x8cd5; }
    getFramebufferAttachmentParameter(target, attachment, pname) {
      branded('gl', this);
      const entry = this.parameters.get(target === 0x8ca8 ? 36010 : 36006)?.attachments?.get(attachment);
      return pname === 0x8cd0 ? (entry?.type ?? 0) : entry?.buffer ?? null;
    }
    isRenderbuffer(buffer) { branded('gl', this); return buffer?.kind === 'renderbuffer' && !buffer.deleted; }
    bindRenderbuffer(target, buffer) {
      branded('gl', this);
      if (target !== 0x8d41 || (buffer !== null && !this.isRenderbuffer(buffer))) { this.error=0x0502; return; }
      calls.renderbufferBinds.push(buffer); this.parameters.set(36007, buffer);
    }
    getRenderbufferParameter(target, pname) {
      branded('gl', this);
      const buffer = this.parameters.get(36007);
      if (target !== 0x8d41 || !buffer) { this.error=0x0502; return null; }
      if (buffer.queryThrows) throw new Error('native renderbuffer query failed');
      return ({[0x8d42]:buffer.width,[0x8d43]:buffer.height,[0x8d44]:buffer.format,[0x8cab]:buffer.samples})[pname];
    }
    getParameter(...args) {
      branded('gl', this);
      if (this.incomplete && [35738,35739].includes(args[0])) { this.error=0x0502; return null; }
      if (this.parameters.has(args[0])) return this.parameters.get(args[0]);
      if ((args[0] === 37445 || args[0] === 37446) && !this.extensionAvailable) return null;
      return args[0] === 37445 ? 'Native vendor' : args[0] === 37446 ? 'Native renderer' : args[0] === 123 ? 64 : null;
    }
    readPixels(...args) {
      branded('gl', this); calls.glReads.push(args);
      if (this.canvas?.tainted) throw new DOMException('Tainted', 'SecurityError');
      const [x,y,width,height] = args.slice(0,4).map(v => Number(v) | 0);
      const output = args[6], offset = this.second ? Number(args[7] ?? 0) : 0;
      if (this.lost) return;
      if (this.incomplete) { this.error=0x0506; return; }
      const framebuffer = this.parameters.get(this.second ? 36010 : 36006);
      const entry = framebuffer?.attachments?.get(this.second ? this.parameters.get(3074) : 0x8ce0);
      const source = entry?.buffer ?? this.canvas;
      const rowLength = this.second ? this.parameters.get(3330) : 0;
      const skipRows = this.second ? this.parameters.get(3331) : 0;
      const skipPixels = this.second ? this.parameters.get(3332) : 0;
      const storeWidth = rowLength || width;
      const stride = Math.ceil(storeWidth * 4 / this.parameters.get(3333)) * this.parameters.get(3333);
      const first = offset + skipRows * stride + skipPixels * 4;
      if (args[4] !== 6408 || args[5] !== 5121 || Object.prototype.toString.call(output) !== '[object Uint8Array]' ||
          this.parameters.get(35053) || this.parameters.get(3074) === 0 || this.parameters.get(35738) !== 5121 ||
          source?.samples > 0 || skipPixels + width > storeWidth ||
          width < 0 || height < 0 || offset < 0 || first + (height - 1) * stride + width * 4 > output.length) {
        this.error = 0x0502; return;
      }
      for (let row = 0; row < height; row++) for (let col = 0; col < width; col++) {
        const sx = x + col, sy = source.height - y - row - 1;
        const dest = first + row * stride + col * 4;
        if (sx < 0 || sy < 0 || sx >= source.width || sy >= source.height) continue;
        else output.set(source.pixels.subarray((sy*source.width+sx)*4,(sy*source.width+sx)*4+4),dest);
      }
    }
  }
  class WebGL2RenderingContext extends WebGLRenderingContext { constructor(canvas) { super(canvas, true); } }
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
    getContext(type) {
      branded('canvas', this);
      if (this.context) return type === this.contextType ? this.context : null;
      const Ctor = type === '2d' ? CanvasRenderingContext2D : type === 'webgl' ? WebGLRenderingContext : type === 'webgl2' ? WebGL2RenderingContext : null;
      if (!Ctor) return null;
      this.contextType = type; return this.context = new Ctor(this);
    }
    toDataURL(...args) { return JSON.stringify(nativeExport(this, 'toDataURL', args)); }
    toBlob(callback, ...args) { const result = nativeExport(this, 'toBlob', args); queueMicrotask(() => callback(result)); }
  }
  class OffscreenCanvas extends HTMLCanvasElement {
    getContext(type) {
      branded('canvas', this);
      if (this.context) return type === this.contextType ? this.context : null;
      const Ctor = type === '2d' ? OffscreenCanvasRenderingContext2D : type === 'webgl' ? WebGLRenderingContext : type === 'webgl2' ? WebGL2RenderingContext : null;
      if (!Ctor) return null;
      this.contextType = type; return this.context = new Ctor(this);
    }
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
  if (worker) { globals.WorkerNavigator = Navigator; delete globals.Navigator; delete globals.Screen; delete globals.screen; delete globals.document; delete globals.HTMLCanvasElement; }
  for (const key of missing) delete globals[key];
  if (!worker) globals.window = globals;
  const originalRealtime = AudioBufferSourceNode.prototype.start;
  const originalDpr = Object.getOwnPropertyDescriptor(globals, 'devicePixelRatio');
  const originalNavigatorGetters = Object.fromEntries(['language','languages','platform','hardwareConcurrency','maxTouchPoints']
    .map(name => [name,Object.getOwnPropertyDescriptor(Navigator.prototype,name).get]));
  const context = vm.createContext(globals);
  const serialized = JSON.stringify(persona).replaceAll('\\', '\\\\').replaceAll("'", "\\'");
  vm.runInContext(template.replace('{{PERSONA_JSON}}', serialized), context, { filename: 'fingerprint-content.js' });
  const reinject = (next = persona) => {
    const json = JSON.stringify(next).replaceAll('\\', '\\\\').replaceAll("'", "\\'");
    return vm.runInContext(template.replace('{{PERSONA_JSON}}', json), context);
  };
  return { ...globals, persona, calls, context, originalRealtime, originalDpr, originalNavigatorGetters, reinject };
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

test('private native Navigator mode leaves the original page and Worker getters intact', () => {
  for (const worker of [false,true]) {
    const env = environment({native_navigator:true},[],worker);
    const proto = (env.Navigator ?? env.WorkerNavigator).prototype;
    for (const [name,getter] of Object.entries(env.originalNavigatorGetters)) {
      if (worker && name === 'platform') assert.notEqual(Object.getOwnPropertyDescriptor(proto,name).get,getter);
      else assert.equal(Object.getOwnPropertyDescriptor(proto,name).get,getter,name);
      assert.throws(()=>getter.call({}),TypeError);
    }
    assert.equal(env.navigator.hardwareConcurrency,2);
    assert.equal(env.navigator.platform,worker ? 'Win32' : 'native');
    assert.deepEqual([...env.navigator.languages],['en-US']);
    assert.equal(env.navigator.maxTouchPoints,1);
    assert.equal(env.navigator.deviceMemory,0.5);
    assert.equal(env.reinject(),true);
  }
});

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

test('WorkerNavigator and OffscreenCanvas share the page policy without fabricating window APIs', async () => {
  const page = environment();
  const worker = environment({}, [], true);
  for (const key of ['hardwareConcurrency', 'deviceMemory', 'language', 'platform']) assert.equal(worker.navigator[key], page.navigator[key]);
  assert.deepEqual([...worker.navigator.languages], [...page.navigator.languages]);
  assert.equal(worker.window, undefined); assert.equal(worker.document, undefined); assert.equal(worker.screen, undefined);
  const canvas = populatedCanvas(worker, true);
  const ctx = canvas.getContext('2d');
  const reading = [...ctx.getImageData(0, 0, 6, 4).data];
  assert.deepEqual(reading, [...populatedCanvas(page).getContext('2d').getImageData(0, 0, 6, 4).data]);
  assert.deepEqual((await canvas.convertToBlob()).data, reading);
  assert.equal(new worker.WebGLRenderingContext().getParameter(37446), defaults.webgl_renderer);
  assert.throws(() => Object.getOwnPropertyDescriptor(worker.WorkerNavigator.prototype, 'hardwareConcurrency').get.call({}), /Illegal invocation/);
});

test('reapplying the same policy preserves Canvas and audio noise; changed policy fails explicitly', async () => {
  const env = environment({}, [], true);
  const canvas = populatedCanvas(env, true);
  const read = () => [...canvas.getContext('2d').getImageData(0, 0, 6, 4).data];
  const first = read();
  const audio = new env.OfflineAudioContext();
  const buffer = await audio.startRendering();
  const samples = [...buffer.getChannelData(0)];
  assert.equal(env.reinject(), true);
  assert.deepEqual(read(), first);
  assert.deepEqual((await canvas.convertToBlob()).data, first);
  assert.deepEqual([...((await audio.startRendering()).getChannelData(0))], samples);
  assert.throws(() => env.reinject({...env.persona, seed:'changed'}), /policy changed/);
  const marker = env.context[Symbol.for('brows.fingerprint.policy.v1')];
  assert.equal(typeof marker, 'function');
  assert.ok(!String(marker).includes(env.persona.seed));
});

function topRows(bytes, width, height, stride = width * 4, offset = 0) {
  const output = [];
  for (let row = height - 1; row >= 0; row--) output.push(...bytes.subarray(offset + row * stride, offset + row * stride + width * 4));
  return output;
}
function readGl(gl, x, y, width, height) {
  const output = new Uint8Array(width * height * 4);
  assert.equal(gl.readPixels(x, y, width, height, 6408, 5121, output), undefined);
  return output;
}

test('WebGL1/2 pixel reads are stable, coordinate-consistent and match opaque Canvas exports without mutating the GPU source', () => {
  for (const type of ['webgl', 'webgl2']) {
    const env = environment(), canvas = populatedCanvas(env), native = [...canvas.pixels];
    const gl = canvas.getContext(type);
    const full = readGl(gl, 0, 0, 6, 4), top = topRows(full, 6, 4);
    assert.deepEqual([...readGl(gl, 0, 0, 6, 4)], [...full]);
    assert.notDeepEqual(top, native);
    assert.deepEqual(topRows(readGl(gl, 2, 1, 3, 2),3,2), crop({width:6,data:Uint8Array.from(top)},2,1,3,2));
    const exported = JSON.parse(canvas.toDataURL()).data;
    for (let i = 0; i < native.length; i += 4) {
      assert.equal(top[i + 3], native[i + 3]);
      if (native[i + 3] === 255) assert.deepEqual(top.slice(i,i+4),exported.slice(i,i+4));
      else assert.deepEqual(top.slice(i,i+4),native.slice(i,i+4));
    }
    assert.deepEqual([...canvas.pixels], native);
    assert.equal(env.calls.glErrors, 0, 'the wrapper must never consume the native GL error state');
    env.reinject();
    assert.deepEqual([...readGl(gl,0,0,6,4)],[...full]);
    const another = environment({seed:'different-pixel-seed'}), otherCanvas = populatedCanvas(another);
    assert.notDeepEqual(topRows(readGl(otherCanvas.getContext(type),0,0,6,4),6,4),top);
  }
});

test('WebGL2 preserves pack padding, destination offsets and bytes outside the requested rectangle', () => {
  const env = environment(), canvas = populatedCanvas(env), gl = canvas.getContext('webgl2');
  const expected = topRows(readGl(gl,2,1,3,2),3,2);
  gl.parameters.set(3333,8);
  const output = new Uint8Array(44).fill(211);
  gl.readPixels(2,1,3,2,6408,5121,output,5);
  assert.deepEqual(topRows(output,3,2,16,5),expected);
  for (const index of [...Array(5).keys(),17,18,19,20,...Array.from({length:11},(_,i)=>33+i)]) assert.equal(output[index],211);
  assert.equal(env.calls.glErrors,0);
});

test('WebGL2 row length and skipped rows/pixels match ordinary reads and preserve view boundaries', () => {
  for (const worker of [false,true]) for (const [alignment,rowLength,skipRows,skipPixels,stride,first] of [
    [1,7,2,2,28,69], [2,7,2,2,28,69], [4,7,2,2,28,69], [8,7,2,2,32,77], [8,0,1,0,16,21],
  ]) {
    const env = environment({},[],worker), canvas = populatedCanvas(env,true), gl = canvas.getContext('webgl2');
    const expected = topRows(readGl(gl,2,1,3,2),3,2);
    for (const [key,value] of [[3333,alignment],[3330,rowLength],[3331,skipRows],[3332,skipPixels]]) gl.parameters.set(key,value);
    const backing = new Uint8Array(180).fill(211), output = new Uint8Array(backing.buffer,7,160);
    gl.error = 0x0500;
    gl.readPixels(2,1,3,2,6408,5121,output,5);
    assert.deepEqual(topRows(output,3,2,stride,first),expected);
    const touched = new Set(Array.from({length:2},(_,row)=>Array.from({length:12},(_,i)=>7+first+row*stride+i)).flat());
    for (let i=0;i<backing.length;i++) if (!touched.has(i)) assert.equal(backing[i],211);
    assert.equal(env.calls.glErrors,0);
    assert.equal(gl.getError(),0x0500); assert.equal(gl.getError(),0);
  }
});

test('WebGL2 invalid pack subregions and insufficient skipped destinations retain native errors and bytes', () => {
  const env = environment(), gl = populatedCanvas(env).getContext('webgl2');
  for (const [rowLength,skipRows,skipPixels,size,offset] of [[4,0,2,96,0],[7,2,2,80,5],[0,0,1,96,0]]) {
    for (const [key,value] of [[3330,rowLength],[3331,skipRows],[3332,skipPixels]]) gl.parameters.set(key,value);
    const output = new Uint8Array(size).fill(255);
    gl.readPixels(2,1,3,2,6408,5121,output,offset);
    assert.ok(output.every(v=>v===255)); assert.equal(gl.getError(),0x0502); assert.equal(gl.getError(),0);
  }
});

function renderbufferFixture(env, attachment = 0x8ce0) {
  const canvas = populatedCanvas(env), gl = canvas.getContext('webgl2');
  const reference = new env.HTMLCanvasElement(5,3);
  for (let i=0;i<reference.pixels.length;i+=4) reference.pixels.set([50+i,100,180,255],i);
  const buffer = {kind:'renderbuffer',width:5,height:3,format:0x8058,samples:0,pixels:reference.pixels};
  const framebuffer = {attachments:new Map([[attachment,{type:0x8d41,buffer}]])};
  gl.parameters.set(36010,framebuffer); gl.parameters.set(3074,attachment);
  return {gl,buffer,framebuffer,reference};
}

test('WebGL2 RGBA8 renderbuffer reads use the selected read attachment extent and restore unrelated bindings', () => {
  for (const attachment of [0x8ce0,0x8ce1]) for (const bound of ['null','other','same']) {
    const env = environment(), {gl,buffer,framebuffer,reference} = renderbufferFixture(env,attachment);
    const native = [...buffer.pixels], expected = JSON.parse(reference.toDataURL()).data;
    const previous = bound==='null' ? null : bound==='same' ? buffer : {kind:'renderbuffer',width:1,height:1};
    gl.parameters.set(36007,previous); gl.parameters.set(36006,{}); // distinct draw framebuffer
    const full = readGl(gl,0,0,5,3);
    assert.deepEqual(topRows(full,5,3),expected);
    assert.deepEqual(topRows(readGl(gl,1,1,3,2),3,2),crop({width:5,data:Uint8Array.from(expected)},1,0,3,2));
    gl.parameters.set(3330,7); gl.parameters.set(3331,1); gl.parameters.set(3332,2); gl.parameters.set(3333,8);
    const packed = new Uint8Array(100).fill(203);
    gl.readPixels(1,1,3,2,6408,5121,packed,3);
    assert.deepEqual(topRows(packed,3,2,32,43),topRows(readGlWithDefaultPack(gl,1,1,3,2),3,2));
    assert.equal(gl.parameters.get(36007),previous); assert.equal(gl.parameters.get(36010),framebuffer);
    assert.deepEqual([...buffer.pixels],native);
    assert.equal(env.calls.glErrors,0); assert.equal(gl.getError(),0);
  }
});

function readGlWithDefaultPack(gl,x,y,width,height) {
  for (const key of [3330,3331,3332]) gl.parameters.set(key,0);
  gl.parameters.set(3333,4); return readGl(gl,x,y,width,height);
}

test('WebGL2 unsupported renderbuffer formats and multisampling preserve native results, errors and bindings', () => {
  const env = environment(), {gl,buffer} = renderbufferFixture(env);
  buffer.format = 0x8056;
  assert.deepEqual(topRows(readGl(gl,0,0,5,3),5,3),[...buffer.pixels]);
  assert.equal(gl.parameters.get(36007),null); assert.equal(gl.getError(),0);
  buffer.format = 0x8058; buffer.samples = 4;
  const output = new Uint8Array(60).fill(255);
  gl.readPixels(0,0,5,3,6408,5121,output);
  assert.ok(output.every(v=>v===255)); assert.equal(gl.parameters.get(36007),null);
  assert.equal(gl.getError(),0x0502); assert.equal(gl.getError(),0);
});

test('WebGL invalid/no-op reads, native errors, receiver checks, coercion and security exceptions are preserved', () => {
  const env = environment(), canvas = populatedCanvas(env), gl = canvas.getContext('webgl2');
  assert.throws(()=>env.WebGLRenderingContext.prototype.readPixels.call({},0,0,1,1,6408,5121,new Uint8Array(4)),TypeError);
  const tiny = new Uint8Array(3).fill(77);
  gl.readPixels(0,0,1,1,6408,5121,tiny);
  assert.deepEqual([...tiny],[77,77,77]); assert.equal(gl.getError(),0x0502);
  const sentinel = new Uint8Array(96).fill(77);
  gl.readPixels(0,0,6,4,6407,5121,sentinel);
  assert.ok(sentinel.every(v=>v===77)); assert.equal(gl.getError(),0x0502);
  gl.parameters.set(3074,0);
  gl.readPixels(0,0,6,4,6408,5121,sentinel);
  assert.ok(sentinel.every(v=>v===77)); assert.equal(gl.getError(),0x0502);
  gl.parameters.set(3074,1029);
  gl.parameters.set(35738,5126);
  gl.readPixels(0,0,6,4,6408,5121,sentinel);
  assert.ok(sentinel.every(v=>v===77)); assert.equal(gl.getError(),0x0502);
  gl.parameters.set(35738,5121);
  gl.error = 0x0500;
  readGl(gl,0,0,6,4); assert.equal(gl.getError(),0x0500, 'a pre-existing error must survive a valid read');
  let coerced = 0;
  const x = {valueOf(){coerced++;return 0;}};
  const output = new Uint8Array(96);
  gl.readPixels(x,0,6,4,6408,5121,output);
  assert.equal(coerced,1); assert.deepEqual(topRows(output,6,4),[...canvas.pixels]);
  canvas.tainted = true;
  assert.throws(()=>readGl(gl,0,0,6,4),{name:'SecurityError'});
});

test('WebGL unsupported buffers/states and disabled noise keep native pixel results in pages and Workers', () => {
  for (const worker of [false,true]) {
    const env = environment({},[],worker), canvas = populatedCanvas(env,true), gl = canvas.getContext('webgl2');
    const native = [...canvas.pixels];
    gl.parameters.set(36010,{attachments:new Map([[0x8ce0,{type:0x1702,buffer:canvas}]])});
    gl.parameters.set(3074,0x8ce0);
    assert.deepEqual(topRows(readGl(gl,0,0,6,4),6,4),native);
    gl.parameters.set(36010,null); gl.parameters.set(3074,1029);
    gl.colorSpace = 'display-p3';
    assert.deepEqual(topRows(readGl(gl,0,0,6,4),6,4),native);
    gl.colorSpace = 'srgb'; gl.lost = true;
    const output = new Uint8Array(96).fill(199);
    gl.readPixels(0,0,6,4,6408,5121,output);
    assert.ok(output.every(v=>v===199)); gl.lost = false;
    assert.notDeepEqual(topRows(readGl(gl,0,0,6,4),6,4),native);
    const disabled = environment({spoof_canvas:false},[],worker), clean = populatedCanvas(disabled,true);
    assert.deepEqual(topRows(readGl(clean.getContext('webgl2'),0,0,6,4),6,4),[...clean.pixels]);
  }
});

test('WebGL recognizes cross-realm byte arrays and leaves shared-memory readback native', () => {
  const env = environment(), canvas = populatedCanvas(env), gl = canvas.getContext('webgl2');
  const expected = [...readGl(gl,0,0,6,4)];
  const foreign = vm.runInNewContext('new Uint8Array(96)');
  gl.readPixels(0,0,6,4,6408,5121,foreign);
  assert.deepEqual([...foreign],expected);
  if (typeof SharedArrayBuffer !== 'undefined') {
    const shared = new Uint8Array(new SharedArrayBuffer(96));
    gl.readPixels(0,0,6,4,6408,5121,shared);
    assert.deepEqual(topRows(shared,6,4),[...canvas.pixels]);
  }
});

test('WebGL incomplete read buffers do not gain extra errors or change caller bytes', () => {
  const env = environment(), canvas = populatedCanvas(env), gl = canvas.getContext('webgl2');
  gl.incomplete = true;
  const bytes = new Uint8Array(96).fill(81);
  gl.readPixels(0,0,6,4,6408,5121,bytes);
  assert.ok(bytes.every(v=>v===81));
  assert.equal(gl.getError(),0x0506);
  assert.equal(gl.getError(),0, 'read-format queries must not add INVALID_OPERATION on incomplete buffers');
});
