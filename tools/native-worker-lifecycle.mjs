// Real first-script observations. No Brows injection or replacement getters.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { NativePipe, killOwned, sleep } from './test_support/native_pipe.mjs';

const args = process.argv.slice(2), options = {};
assert.equal(args.length % 2, 0, 'arguments require name/value pairs');
for (let i = 0; i < args.length; i += 2) {
  assert(/^--(browser|sha256|expect|report)$/.test(args[i]), 'unknown argument');
  const key = args[i].slice(2); assert(!(key in options), 'duplicate argument'); options[key] = args[i + 1];
}
assert(options.browser && /^[a-f0-9]{64}$/.test(options.sha256 ?? ''), '--browser and --sha256 required');
assert(['stock', 'native'].includes(options.expect), '--expect must be stock or native');
assert(options.report, '--report required');
const browser = resolve(options.browser), reportPath = resolve(options.report);
const host = { win32: 'Win32', linux: 'Linux x86_64', darwin: 'MacIntel' }[process.platform];
assert(host, 'unsupported test host');
const configured = host === 'Win32' ? 'Linux x86_64' : 'Win32';
const kinds = ['page', 'worker', 'module_worker', 'shared_worker', 'module_shared_worker', 'service_worker', 'module_service_worker'];
const serviceKinds = ['service_worker', 'module_service_worker'];
const report = { schemaVersion: 1, scope: 'isolated native Worker lifecycle; no production runtime admission',
  expectation: options.expect, hostPlatform: host, configuredPlatform: configured, executableSha256: options.sha256,
  assertionsPassed: false, nativeLifecyclePassed: false, runtimeAdmitted: false, complete: false,
  checkpoints: [], observations: [], failure: null };
const prefix = `/brows-${randomUUID()}/`;
let child, pipe, server, root, origin, failure, phase = 'initial', processIndex = 0, revision = 1, closing = false;
let targets = new Map(), paused = new Set(), versions = new Map(), registrations = new Map(), metadata = new Map();
let userAgent, pageSession, useOverride = true;
const proofs = [], seenBoots = new Set();
const scope = kind => `${origin}${prefix}${kind === 'service_worker' ? 'classic-sw/' : 'module-sw/'}`;

async function waitFor(condition, label, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (!condition()) {
    if (failure) throw failure;
    if (pipe?.closed && !closing) throw pipe.closed;
    if (Date.now() >= deadline) throw new Error(`timeout: ${label}`);
    await sleep(20);
  }
  if (failure) throw failure;
}

async function executableDigest() {
  const hash = createHash('sha256');
  for await (const bytes of createReadStream(browser)) hash.update(bytes);
  assert.equal(hash.digest('hex'), options.sha256, 'test executable changed');
}

function validate(first, overriding = useOverride) {
  const label = `${first.phase ?? phase}/${first.kind}`;
  assert.equal(first.nativeGetter, true, `${label}: original native getter`);
  assert.equal(first.receiver, true, `${label}: native receiver validation`);
  assert.equal(first.raw, first.platform, `${label}: raw getter value`);
  const expected = overriding && (first.kind === 'page' || options.expect === 'native') ? configured : host;
  assert.equal(first.platform, expected, `${label}: platform`);
}

function samplesSince(index, kind) { return proofs.slice(index).filter(first => first.kind === kind); }

async function checkpoint(name, start, requiredKinds, expectedRevision) {
  await waitFor(() => requiredKinds.every(kind => samplesSince(start, kind).some(first => expectedRevision === undefined || first.revision === expectedRevision)), name);
  const observations = proofs.slice(start);
  for (const first of observations) validate(first);
  report.checkpoints.push({ name, processIndex, observations: observations.length, assertionsPassed: true });
}

function capture(kind, scriptRevision = revision) {
  return `const getter=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator),'platform').get;
globalThis.__originalPlatformGetter=getter;
let receiver=false;try{Reflect.apply(getter,{},[])}catch(e){receiver=e instanceof TypeError}
const first={kind:${JSON.stringify(kind)},revision:${scriptRevision},boot:crypto.randomUUID(),platform:navigator.platform,
raw:Reflect.apply(getter,navigator,[]),nativeGetter:Function.prototype.toString.call(getter).includes('[native code]'),receiver};
const proof=fetch(${JSON.stringify(`${prefix}proof`)},{method:'POST',body:JSON.stringify(first)});`;
}

async function serve(request, response) {
  response.setHeader('Cache-Control', 'no-store');
  const url = new URL(request.url, origin);
  if (!url.pathname.startsWith(prefix)) { response.writeHead(404).end(); return; }
  const path = url.pathname.slice(prefix.length);
  if (request.method === 'POST' && ['proof', 'metadata', 'fault'].includes(path)) {
    let body = '';
    for await (const bytes of request) { body += bytes; if (body.length > 4096) { response.writeHead(413).end(); return; } }
    const value = JSON.parse(body);
    if (path === 'fault') throw new Error(`owned fixture error: ${value.kind}`);
    if (path === 'metadata') metadata.set(processIndex, value);
    else {
      assert(kinds.includes(value.kind), 'unexpected observation kind');
      assert(typeof value.boot === 'string' && !seenBoots.has(value.boot), 'first-script context reused');
      assert(proofs.length < 100, 'first-script sample limit'); seenBoots.add(value.boot);
      const first = { ...value, phase, processIndex }; proofs.push(first);
      const { boot, ...observation } = first; report.observations.push(observation);
    }
    response.end('ok'); return;
  }
  response.setHeader('Content-Type', path === 'page' ? 'text/html' : 'text/javascript');
  const kind = url.searchParams.get('kind'); let content;
  if (path === 'page') content = `<!doctype html><script>${capture('page')}
addEventListener('DOMContentLoaded',async()=>{try{
const before=(await navigator.serviceWorker.getRegistrations()).filter(r=>r.scope.startsWith(location.origin+${JSON.stringify(prefix)}));
await fetch(${JSON.stringify(`${prefix}metadata`)},{method:'POST',body:JSON.stringify({registrations:before.length,active:before.filter(r=>r.active).length})});
const start=${JSON.stringify(prefix)};
globalThis.ownedWorkers=[new Worker(start+'worker.js'),new Worker(start+'module.mjs',{type:'module'}),
new SharedWorker(start+'shared.js',{name:'brows-native-classic'}),new SharedWorker(start+'module-shared.mjs',{type:'module',name:'brows-native-module'})];
for(const worker of ownedWorkers.slice(2))worker.port.start();
await Promise.all([navigator.serviceWorker.register(start+'classic-sw/sw.js',{scope:start+'classic-sw/'}),
navigator.serviceWorker.register(start+'module-sw/sw.mjs',{scope:start+'module-sw/',type:'module'})]);
}catch(e){fetch(${JSON.stringify(`${prefix}fault`)},{method:'POST',body:JSON.stringify({kind:'registration-or-worker-start'})})}});</script>`;
  if (path === 'worker.js') content = `${capture('worker')}setInterval(()=>{},1000);`;
  if (path === 'shared.js') content = `${capture('shared_worker')}onconnect=e=>e.ports[0].start();`;
  if (path === 'first.mjs' && kinds.includes(kind)) content = `${capture(kind)}export{proof};`;
  if (path === 'module.mjs') content = `import './first.mjs?kind=module_worker';setInterval(()=>{},1000);`;
  if (path === 'module-shared.mjs') content = `import './first.mjs?kind=module_shared_worker';onconnect=e=>e.ports[0].start();`;
  const lifecycle = `addEventListener('install',e=>e.waitUntil(proof.then(()=>skipWaiting())));addEventListener('activate',e=>e.waitUntil(clients.claim()));addEventListener('message',e=>e.waitUntil(proof));`;
  if (path === 'classic-sw/sw.js') content = `${capture('service_worker')}${lifecycle}`;
  if (path === 'module-sw/sw.mjs') content = `import{proof}from '../first.mjs?kind=module_service_worker&revision=${revision}';${lifecycle}`;
  if (content === undefined) response.writeHead(404).end(); else response.end(content);
}

const autoAttach = types => ({ autoAttach: true, waitForDebuggerOnStart: true, flatten: true, filter: types.map(type => ({ type })) });

async function launch(profile, overriding) {
  await executableDigest(); useOverride = overriding; closing = false; processIndex++;
  targets = new Map(); paused = new Set(); versions = new Map(); registrations = new Map();
  child = spawn(browser, ['--headless=new', '--no-startup-window', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--disable-component-update', '--disable-sync', '--no-proxy-server',
    '--remote-debugging-pipe', `--user-data-dir=${profile}`],
  { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32', windowsHide: true });
  pipe = new NativePipe(child);
  const version = await pipe.call('Browser.getVersion'); userAgent = version.userAgent;
  if (report.runtimeVersion) assert.equal(version.product, report.runtimeVersion, 'runtime version changed');
  else report.runtimeVersion = version.product;
  async function apply(event) {
    const { sessionId, targetInfo } = event.params;
    // A fresh Edge profile can start built-in extension Workers. They are not
    // owned fixture contexts and must resume without receiving test overrides.
    if (targetInfo.url && targetInfo.type !== 'page' && !targetInfo.url.startsWith(`${origin}${prefix}`)) {
      await pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId); return;
    }
    const target = { id: targetInfo.targetId, session: sessionId, kind: targetInfo.type, url: targetInfo.url, ready: false };
    assert(targets.size < 64, 'target limit'); targets.set(target.id, target);
    try {
      if (target.kind === 'service_worker') {
        await Promise.all([pipe.call('Debugger.enable', {}, sessionId), pipe.call('Debugger.pause', {}, sessionId),
          pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId)]);
        await waitFor(() => paused.has(sessionId), 'Service Worker native policy pause', 8000);
      }
      if (overriding) await pipe.call('Emulation.setUserAgentOverride', { userAgent, platform: configured }, sessionId);
      if (target.kind === 'page') {
        await pipe.call('Target.setAutoAttach', autoAttach(['worker', 'iframe']), sessionId);
        await pipe.call('Page.enable', {}, sessionId); await pipe.call('ServiceWorker.enable', {}, sessionId);
      }
      if (target.kind === 'service_worker') await pipe.call('Debugger.disable', {}, sessionId);
      else await pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId);
      target.ready = true;
    } catch (error) {
      // A stopped target cannot contribute proof. The phase's required first
      // scripts still have to arrive and pass; no detached context counts as success.
      if (!closing && [...targets.values()].some(current => current.session === sessionId)) throw error;
    }
  }
  pipe.on('event', event => {
    if (closing) return;
    if (event.method === 'Debugger.paused') paused.add(event.sessionId);
    if (event.method === 'Target.attachedToTarget') apply(event).catch(error => { failure ??= error; });
    if (event.method === 'Target.targetInfoChanged') {
      const info = event.params.targetInfo, target = targets.get(info.targetId);
      if (target) target.url = info.url;
    }
    if (event.method === 'Target.detachedFromTarget') {
      const session = event.params.sessionId; paused.delete(session);
      for (const target of targets.values()) if (target.session === session) targets.delete(target.id);
    }
    if (event.method === 'ServiceWorker.workerVersionUpdated') {
      for (const item of event.params.versions) versions.set(item.versionId, item);
      if (versions.size > 64) failure ??= new Error('Service Worker version limit');
    }
    if (event.method === 'ServiceWorker.workerRegistrationUpdated') {
      for (const item of event.params.registrations) registrations.set(item.registrationId, item);
    }
  });
  await pipe.call('Target.setAutoAttach', autoAttach(['page', 'shared_worker', 'service_worker']));
  const created = await pipe.call('Target.createTarget', { url: 'about:blank' });
  await waitFor(() => targets.get(created.targetId)?.ready, 'blank page native policy');
  pageSession = targets.get(created.targetId).session;
  const navigation = await pipe.call('Page.navigate', { url: `${origin}${prefix}page` }, pageSession);
  assert(!navigation.errorText, 'owned page navigation failed');
}

async function cleanRestart() {
  closing = true;
  await pipe.call('Browser.close').catch(error => { if (!pipe.closed) throw error; });
  await waitFor(() => child.exitCode !== null || child.signalCode !== null, 'graceful browser exit', 8000);
  assert.equal(child.exitCode, 0, 'browser must flush its owned profile before reopening');
}

function activeVersion(kind) {
  const registration = [...registrations.values()].find(item => !item.isDeleted && item.scopeURL === scope(kind));
  return registration && [...versions.values()].find(item => item.registrationId === registration.registrationId && item.status === 'activated');
}

try {
  root = await mkdtemp(join(tmpdir(), 'brows-native-lifecycle-'));
  server = createServer((request, response) => { serve(request, response).catch(error => { failure ??= error; if (!response.writableEnded) response.writeHead(500).end(); }); });
  await new Promise(resolveServer => server.listen(0, '127.0.0.1', resolveServer));
  origin = `http://127.0.0.1:${server.address().port}`;
  const profile = join(root, 'persistent-owned-profile');
  await launch(profile, true);
  await checkpoint('initial-six-worker-first-scripts', 0, kinds, 1);
  await waitFor(() => serviceKinds.every(kind => activeVersion(kind)), 'initial active registrations');
  assert.equal(metadata.get(processIndex)?.registrations, 0, 'first profile must be fresh');
  const initialServiceBoots = serviceKinds.map(kind => proofs.find(first => first.kind === kind).boot);

  phase = 'clear-and-restore';
  const workerTargets = [...targets.values()].filter(target => ['worker', 'shared_worker', 'service_worker'].includes(target.kind) && target.url.startsWith(`${origin}${prefix}`));
  assert.equal(workerTargets.length, 6, 'all six live Worker targets required');
  const inspect = `(()=>{const getter=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator),'platform').get;return{platform:navigator.platform,sameGetter:getter===globalThis.__originalPlatformGetter,nativeGetter:Function.prototype.toString.call(getter).includes('[native code]')}})()`;
  for (const target of workerTargets) {
    for (const platform of ['', configured]) {
      await pipe.call('Emulation.setUserAgentOverride', { userAgent, platform }, target.session);
      const result = await pipe.call('Runtime.evaluate', { expression: inspect, returnByValue: true }, target.session);
      assert(!result.exceptionDetails, 'native getter inspection threw');
      assert.equal(result.result.value.platform, platform && options.expect === 'native' ? configured : host);
      assert.equal(result.result.value.sameGetter, true); assert.equal(result.result.value.nativeGetter, true);
    }
  }
  report.checkpoints.push({ name: 'clear-and-restore-all-six-native-getters', processIndex, targets: 6, assertionsPassed: true });

  phase = 'page-reload'; let start = proofs.length;
  await pipe.call('Page.reload', { ignoreCache: true }, pageSession);
  await checkpoint('reload-dedicated-worker-first-scripts', start, ['page', 'worker', 'module_worker'], 1);
  await waitFor(() => metadata.get(processIndex)?.registrations === 2, 'reload retains registrations');
  const shared = [...targets.values()].filter(target => target.kind === 'shared_worker' && target.url.startsWith(`${origin}${prefix}`));
  assert.equal(shared.length, 2, 'both Shared Workers required after reload');
  for (const target of shared) {
    const result = await pipe.call('Runtime.evaluate', { expression: inspect, returnByValue: true }, target.session);
    assert(!result.exceptionDetails, 'reloaded Shared Worker getter inspection threw');
    assert.equal(result.result.value.platform, options.expect === 'native' ? configured : host);
    assert.equal(result.result.value.sameGetter, true); assert.equal(result.result.value.nativeGetter, true);
  }
  report.checkpoints.push({ name: 'reload-shared-worker-native-getters', processIndex, targets: 2, assertionsPassed: true });

  phase = 'service-worker-wake'; start = proofs.length;
  const beforeWake = serviceKinds.map(kind => activeVersion(kind));
  for (const item of beforeWake) await pipe.call('ServiceWorker.stopWorker', { versionId: item.versionId }, pageSession);
  await waitFor(() => beforeWake.every(item => versions.get(item.versionId)?.runningStatus === 'stopped'), 'both Service Workers stopped');
  // Wake through real extendable message events. Merely starting a Worker via
  // CDP does not keep an asynchronous first-script fetch alive.
  const wake = await pipe.call('Runtime.evaluate', { expression: `(async()=>{const registrations=(await navigator.serviceWorker.getRegistrations()).filter(r=>r.scope.startsWith(location.origin+${JSON.stringify(prefix)}));if(registrations.length!==2||registrations.some(r=>!r.active))throw new Error('owned registrations unavailable');for(const r of registrations)r.active.postMessage('owned-native-wake');return registrations.length})()`, awaitPromise: true, returnByValue: true }, pageSession);
  assert(!wake.exceptionDetails, 'owned wake message dispatch failed'); assert.equal(wake.result.value, 2);
  await checkpoint('stopped-service-worker-first-scripts-after-wake', start, serviceKinds, 1);
  for (let i = 0; i < serviceKinds.length; i++) assert.notEqual(samplesSince(start, serviceKinds[i])[0].boot, initialServiceBoots[i]);

  phase = 'service-worker-update'; start = proofs.length; revision = 2;
  const beforeUpdate = serviceKinds.map(kind => activeVersion(kind).versionId);
  for (const kind of serviceKinds) await pipe.call('ServiceWorker.updateRegistration', { scopeURL: scope(kind) }, pageSession);
  await checkpoint('updated-service-worker-first-scripts', start, serviceKinds, 2);
  await waitFor(() => serviceKinds.every((kind, index) => { const active = activeVersion(kind); return active && active.versionId !== beforeUpdate[index]; }), 'both updates activated');

  phase = 'process-reopen'; start = proofs.length;
  await cleanRestart(); await launch(profile, true);
  await checkpoint('reopened-profile-six-worker-first-scripts', start, kinds, 2);
  assert.equal(metadata.get(processIndex)?.registrations, 2, 'owned registrations must persist across clean process restart');
  assert.equal(metadata.get(processIndex)?.active, 2, 'both persisted registrations must be active');

  phase = 'fresh-profile-no-override'; start = proofs.length;
  await cleanRestart(); await launch(join(root, 'fresh-host-control'), false);
  await checkpoint('fresh-profile-host-platform-without-override', start, kinds, 2);
  assert.equal(metadata.get(processIndex)?.registrations, 0, 'fresh profile must not inherit registrations');
  await cleanRestart();
  for (const first of proofs) validate(first, first.phase !== 'fresh-profile-no-override');
  report.ownedProcessLaunches = processIndex;
  report.assertionsPassed = true; report.nativeLifecyclePassed = options.expect === 'native';
} catch (error) {
  report.failure = error.message; report.failedPhase = phase;
  report.ownedWorkerState = [...targets.values()].filter(target => target.url.startsWith(`${origin}${prefix}`)).map(target => ({ kind: target.kind, ready: target.ready }));
  report.serviceWorkerState = serviceKinds.map(kind => { const item = activeVersion(kind); return { kind, status: item?.status ?? 'missing', runningStatus: item?.runningStatus ?? 'missing' }; });
} finally {
  closing = true; pipe?.close(new Error('owned lifecycle fixture cleanup'));
  try { await killOwned(child); } finally {
    server?.closeAllConnections(); if (server) await new Promise(resolveServer => server.close(resolveServer));
    if (root) await rm(root, { recursive: true, force: true });
    await mkdir(resolve(reportPath, '..'), { recursive: true });
    await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    console.log(JSON.stringify(report));
  }
}
process.exitCode = report.assertionsPassed ? 0 : 1;
