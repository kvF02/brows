"use strict";
// This program reads browser APIs. Hardware, language, GPU and seed expectations
// are intentionally absent from the document and are evaluated by Rust.
const measurement = __MEASUREMENT_CONFIG__;
const read = (fn) => { try { const value = fn(); return value === undefined ? null : value; } catch (_) { return null; } };
const text = (fn) => { const value = read(fn); return typeof value === "string" ? value : null; };
const number = (fn) => { const value = read(fn); return typeof value === "number" && Number.isFinite(value) ? value : null; };
const digest = (bytes) => { let h = 2166136261; for (let i = 0; i < bytes.length; ++i) h = Math.imul(h ^ bytes[i], 16777619); return (h >>> 0).toString(16).padStart(8, "0"); };
const digestText = (value) => digest(new TextEncoder().encode(value));
const bounded = (promise, ms = 4000) => new Promise((resolve) => {
  const timer = setTimeout(() => resolve(null), ms);
  Promise.resolve(promise).then((value) => { clearTimeout(timer); resolve(value); }, () => { clearTimeout(timer); resolve(null); });
});
function canvasObservation() {
  try {
    const canvas = typeof document === "object" ? document.createElement("canvas") : new OffscreenCanvas(32, 32);
    canvas.width = 32; canvas.height = 32;
    const ctx = canvas.getContext("2d"); if (!ctx) return null;
    ctx.fillStyle = "#294c7f"; ctx.fillRect(0, 0, 32, 32);
    ctx.fillStyle = "#e4d9b7"; ctx.fillRect(4, 3, 19, 17);
    ctx.font = "11px sans-serif"; ctx.fillText("Brows", 1, 26);
    const readHash = digest(ctx.getImageData(0, 0, 32, 32).data);
    const repeatReadHash = digest(ctx.getImageData(0, 0, 32, 32).data);
    const exportHash = typeof canvas.toDataURL === "function" ? digestText(canvas.toDataURL()) : null;
    const repeatExportHash = typeof canvas.toDataURL === "function" ? digestText(canvas.toDataURL()) : null;
    const afterExportReadHash = digest(ctx.getImageData(0, 0, 32, 32).data);
    return { readHash, repeatReadHash, exportHash, repeatExportHash, afterExportReadHash };
  } catch (_) { return null; }
}
function capture() {
  const nav = globalThis.navigator;
  let memory; try { memory = nav.deviceMemory; } catch (_) { memory = undefined; }
  let webglVendor = null, webglRenderer = null;
  try {
    const canvas = typeof document === "object" ? document.createElement("canvas") : new OffscreenCanvas(1, 1);
    const gl = canvas.getContext("webgl"); const debug = gl && gl.getExtension("WEBGL_debug_renderer_info");
    if (debug) { webglVendor = text(() => gl.getParameter(debug.UNMASKED_VENDOR_WEBGL)); webglRenderer = text(() => gl.getParameter(debug.UNMASKED_RENDERER_WEBGL)); }
  } catch (_) {}
  let clientHints = null;
  try {
    const hints = nav.userAgentData;
    if (hints) clientHints = {
      brands: Array.from(hints.brands, (item) => `${item.brand}/${item.version}`),
      platform: text(() => hints.platform), mobile: typeof hints.mobile === "boolean" ? hints.mobile : null,
      architecture: null, bitness: null, platformVersion: null, fullVersion: null,
    };
  } catch (_) {}
  return {
    userAgent: text(() => nav.userAgent), platform: text(() => nav.platform), language: text(() => nav.language),
    languages: read(() => Array.from(nav.languages)) || [], concurrency: number(() => nav.hardwareConcurrency),
    memory: typeof memory === "number" && Number.isFinite(memory) ? memory : null, memoryType: typeof memory,
    touchPoints: number(() => nav.maxTouchPoints), screenWidth: number(() => screen.width), screenHeight: number(() => screen.height), colorDepth: number(() => screen.colorDepth),
    innerWidth: number(() => innerWidth), innerHeight: number(() => innerHeight), outerWidth: number(() => outerWidth), outerHeight: number(() => outerHeight), pixelRatio: number(() => devicePixelRatio),
    webglVendor, webglRenderer, timezone: text(() => Intl.DateTimeFormat().resolvedOptions().timeZone), clientHints, canvas: canvasObservation(),
  };
}
async function hints(signal) {
  try {
    if (!signal.clientHints || !navigator.userAgentData) return signal;
    const more = await bounded(navigator.userAgentData.getHighEntropyValues(["architecture", "bitness", "platformVersion", "uaFullVersion"]));
    if (more) for (const [target, source] of [["architecture", "architecture"], ["bitness", "bitness"], ["platformVersion", "platformVersion"], ["fullVersion", "uaFullVersion"]]) signal.clientHints[target] = text(() => more[source]);
  } catch (_) {}
  return signal;
}
// Capture the page/frame/worker synchronously before waiting for the DOM,
// iframe load, worker response or asynchronous audio render.
const initial = capture();

async function renderAudio() {
  const Ctor = globalThis.OfflineAudioContext || globalThis.webkitOfflineAudioContext;
  if (!Ctor) return null;
  async function one() {
    const ctx = new Ctor(1, 4096, 44100);
    const oscillator = ctx.createOscillator(); oscillator.type = "triangle"; oscillator.frequency.value = 440;
    const gain = ctx.createGain(); gain.gain.value = 0.3;
    oscillator.connect(gain); gain.connect(ctx.destination); oscillator.start(0);
    const hash = (buffer) => { const values = buffer.getChannelData(0); return digest(new Uint8Array(values.buffer, values.byteOffset, values.byteLength)); };
    const event = new Promise((resolve) => { ctx.oncomplete = (e) => resolve(hash(e.renderedBuffer)); });
    const rendered = await ctx.startRendering();
    return { promise: hash(rendered), event: await event };
  }
  try { const a = await one(); const b = await one(); return { firstHash: a.promise, repeatHash: b.promise, firstEventHash: a.event, repeatEventHash: b.event }; }
  catch (_) { return null; }
}
function frameObservation() {
  return new Promise((resolve) => {
    const frame = document.createElement("iframe");
    const finish = (value) => { clearTimeout(timer); window.removeEventListener("message", received); frame.remove(); resolve(value); };
    const received = (event) => {
      if (event.origin === location.origin && event.source === frame.contentWindow && event.data?.token === measurement.token && event.data?.runId === measurement.runId && event.data?.kind === "frame") finish(event.data.signal);
    };
    const timer = setTimeout(() => finish(null), 4500);
    window.addEventListener("message", received);
    frame.src = `${measurement.base}/frame`; document.body.appendChild(frame);
  });
}
function workerObservation() {
  try {
    return new Promise((resolve) => {
      const worker = new Worker(`${measurement.base}/worker.js`);
      const finish = (value) => { clearTimeout(timer); worker.terminate(); resolve(value); };
      const timer = setTimeout(() => finish(null), 4500);
      worker.onmessage = (event) => { if (event.data?.token === measurement.token && event.data?.runId === measurement.runId && event.data?.kind === "worker") finish(event.data.signal); };
      worker.onerror = () => finish(null);
    });
  } catch (_) { return Promise.resolve(null); }
}
function storageObservation() {
  let localAvailable = true, cookieAvailable = true;
  let localBefore = null, localAfter = null, cookieBefore = null, cookieAfter = null;
  const key = "brows-owned-measurement-v1";
  const cookie = () => document.cookie.split(";").map((v) => v.trim()).find((v) => v.startsWith(`${key}=`))?.slice(key.length + 1) ?? null;
  try { localBefore = localStorage.getItem(key); localStorage.setItem(key, measurement.marker); localAfter = localStorage.getItem(key); } catch (_) { localAvailable = false; }
  try { cookieBefore = cookie(); document.cookie = `${key}=${measurement.marker}; Path=/; Max-Age=3600; SameSite=Strict`; cookieAfter = cookie(); } catch (_) { cookieAvailable = false; }
  return { localAvailable, cookieAvailable, localBefore, localAfter, cookieBefore, cookieAfter };
}
if (measurement.kind === "worker") {
  hints(initial).then((signal) => postMessage({ token: measurement.token, runId: measurement.runId, kind: "worker", signal }));
} else if (measurement.kind === "frame") {
  hints(initial).then((signal) => parent.postMessage({ token: measurement.token, runId: measurement.runId, kind: "frame", signal }, location.origin));
} else {
  document.addEventListener("DOMContentLoaded", async () => {
    const storage = storageObservation();
    const [page, iframe, worker, audio] = await Promise.all([hints(initial), frameObservation(), workerObservation(), bounded(renderAudio(), 6000)]);
    const result = { token: measurement.token, runId: measurement.runId, page, iframe, worker, audio, storage };
    try { await fetch(`${measurement.base}/collect`, { method: "POST", headers: { "Content-Type": "application/json" }, credentials: "omit", body: JSON.stringify(result) }); }
    catch (_) {}
  }, { once: true });
}
