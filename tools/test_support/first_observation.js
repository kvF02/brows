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
// Inspect the real getters at the first executable statement; the controller
// does not replace Function.prototype.toString or these getter functions.
const navigatorPrototype = typeof document === 'undefined' ? WorkerNavigator.prototype : Navigator.prototype;
first.nativeNavigator = Object.fromEntries(['hardwareConcurrency','platform','language','languages','maxTouchPoints']
  .filter(name => name in navigator)
  .map(name => {
    const getter = Object.getOwnPropertyDescriptor(navigatorPrototype, name)?.get;
    let brandCheck = false;
    try { Reflect.apply(getter, {}, []); } catch (error) { brandCheck = error instanceof TypeError; }
    const value = Reflect.apply(getter, navigator, []);
    return [name, {nativeGetter: Function.prototype.toString.call(getter).includes('[native code]'),
      brandCheck, value: Array.isArray(value) ? Array.from(value) : value}];
  }));
if (typeof document !== 'undefined') {
  const names = ['width','height','availWidth','availHeight','colorDepth','pixelDepth'];
  globalThis.__ownedScreenGetters = Object.fromEntries(names.map(name=>[name,Object.getOwnPropertyDescriptor(Screen.prototype,name).get]));
  first.screen = {width:screen.width,height:screen.height,availWidth:screen.availWidth,availHeight:screen.availHeight,
    orientation:screen.orientation.type,dpr:devicePixelRatio,
    deviceMedia:matchMedia(`(device-width: ${screen.width}px) and (device-height: ${screen.height}px)`).matches,
    wrongDeviceMedia:matchMedia(`(device-width: ${screen.width+1}px)`).matches,
    getters:Object.fromEntries(names.map(name=>{
      const getter=globalThis.__ownedScreenGetters[name];let brandCheck=false;
      try{Reflect.apply(getter,{},[])}catch(error){brandCheck=error instanceof TypeError}
      return[name,{nativeGetter:Function.prototype.toString.call(getter).includes('[native code]'),brandCheck,value:Reflect.apply(getter,screen,[])}];
    }))};
  first.touch = {points:navigator.maxTouchPoints,coarse:matchMedia('(pointer: coarse)').matches,
    hoverNone:matchMedia('(hover: none)').matches,api:'ontouchstart' in globalThis};
  if (location.pathname === '/native-hardware/page') {
    addEventListener('DOMContentLoaded', () => {
      const target = document.createElement('button');
      target.textContent = 'Owned touch target';
      target.style.cssText = 'position:fixed;left:0;top:0;width:64px;height:64px;z-index:2147483647;touch-action:none';
      document.body.append(target);
    }, {once:true});
    const captureTouch = event => {
      if (event.touches.length !== 2) return;
      removeEventListener('touchstart', captureTouch);
      fetch('./touch_input/proof', {method:'POST',body:JSON.stringify({kind:'touch_input',
        trusted:event.isTrusted,touches:event.touches.length,nativeEvent:event instanceof TouchEvent})});
    };
    addEventListener('touchstart', captureTouch);
  }
}
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
__BROWS_GPU_OBSERVATION__
const send = () => gpuProof.then(() => fetch('./' + first.kind + '/proof', {
  method: 'POST', body: JSON.stringify(first), keepalive: typeof document !== 'undefined',
}));
const proof = send().catch(() => send());
