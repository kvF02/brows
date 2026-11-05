// First executable statement of the owned page/Worker, or imported module.
const first = {
  kind: typeof document === 'undefined'
    ? (typeof ServiceWorkerGlobalScope !== 'undefined' && self instanceof ServiceWorkerGlobalScope
      ? 'service_worker' : typeof SharedWorkerGlobalScope !== 'undefined' && self instanceof SharedWorkerGlobalScope
        ? 'shared_worker' : 'worker')
    : window === top ? 'page' : location.hostname === 'localhost' ? 'cross_frame' : 'frame',
  fixtureVersion: __BROWS_FIXTURE_VERSION__,
  capturePhase: '__BROWS_FIXTURE_PHASE__',
  userAgent: navigator.userAgent,
  platform: navigator.platform,
  languages: Array.from(navigator.languages),
  hardwareConcurrency: navigator.hardwareConcurrency,
  deviceMemory: navigator.deviceMemory,
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  offsetWinter: new Date('2024-01-15T12:00:00Z').getTimezoneOffset(),
  offsetSummer: new Date('2024-07-15T12:00:00Z').getTimezoneOffset(),
  hints: navigator.userAgentData?.toJSON(),
};
if (first.capturePhase === 'module_dependency') first.kind = 'module_' + first.kind;
const canvas = typeof document === 'undefined' ? new OffscreenCanvas(16, 16) : document.createElement('canvas');
canvas.width = 16; canvas.height = 16;
const context = canvas.getContext('2d');
context.fillStyle = '#a42367'; context.fillRect(0, 0, 16, 16);
const read = () => Array.from(context.getImageData(0, 0, 16, 16).data, b => b.toString(16).padStart(2, '0')).join('');
first.canvas = read(); first.repeat = read();
const gl = new OffscreenCanvas(1, 1).getContext('webgl');
const extension = gl?.getExtension('WEBGL_debug_renderer_info');
first.webgl = extension ? {
  vendor: gl.getParameter(extension.UNMASKED_VENDOR_WEBGL),
  renderer: gl.getParameter(extension.UNMASKED_RENDERER_WEBGL),
} : null;
gl?.getExtension('WEBGL_lose_context')?.loseContext();
const send = () => fetch('./' + first.kind + '/proof', {
  method: 'POST', body: JSON.stringify(first), keepalive: typeof document !== 'undefined',
});
const proof = send().catch(() => send());
