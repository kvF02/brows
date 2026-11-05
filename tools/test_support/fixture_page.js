const reportError = message => fetch('./error', { method: 'POST', body: String(message) }).catch(() => {});
addEventListener('error', event => reportError(event.message));
addEventListener('unhandledrejection', event => reportError(event.reason));
const watch = worker => {
  worker.addEventListener('error', event => reportError(event.message || 'Worker load failed'));
  return worker;
};
globalThis.worker = watch(new Worker('./worker.js'));
globalThis.shared = watch(new SharedWorker('./shared.js'));
shared.port.start();
const registrations = [navigator.serviceWorker.register('./service.js')];
if (__BROWS_FIXTURE_MODULES__) {
  globalThis.moduleWorker = watch(new Worker('./worker.mjs', { type: 'module' }));
  globalThis.moduleShared = watch(new SharedWorker('./shared.mjs', { type: 'module', name: 'module-shared' }));
  moduleShared.port.start();
  registrations.push(navigator.serviceWorker.register('./module/service.mjs', { type: 'module', scope: './module/' }));
}
let revision = first.fixtureVersion;
setInterval(() => fetch('./version', { cache: 'no-store' })
  .then(response => response.json()).then(async next => {
    if (next !== revision) {
      revision = next;
      await Promise.all(registrations.map(async registration => (await registration).update()));
    }
  }).catch(error => reportError(error?.message || error)), 500);
