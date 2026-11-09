// Owned loopback fixture and private CDP pipe. No Brows hooks or JS getter replacement.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';

const options = Object.fromEntries(process.argv.slice(2).reduce((pairs, value, index, all) => {
  if (index % 2 === 0) pairs.push([value.replace(/^--/, ''), all[index + 1]]);
  return pairs;
}, []));
assert(options.browser && /^[a-f0-9]{64}$/.test(options.sha256 ?? ''), '--browser and --sha256 are required');
assert(['native', 'stock'].includes(options.expect), '--expect must be native or stock');
assert(options.report, '--report is required');
const browser = resolve(options.browser), reportPath = resolve(options.report);
const host = { win32: 'Win32', linux: 'Linux x86_64', darwin: 'MacIntel' }[process.platform];
assert(host, 'unsupported host');
const configured = host === 'Win32' ? 'Linux x86_64' : 'Win32';
const report = { schemaVersion: 1, scope: 'isolated test executable; no production runtime admission',
  expectation: options.expect, hostPlatform: host, configuredPlatform: configured,
  assertionsPassed: false, compiledByThisTool: false, nativeWorkerPlatform: false,
  runtimeAdmitted: false, complete: false, observations: [], failure: null };
const sleep = ms => new Promise(resolveSleep => setTimeout(resolveSleep, ms));

class Pipe extends EventEmitter {
  constructor(child) {
    super(); this.child = child; this.next = 0; this.pending = new Map(); this.bytes = Buffer.alloc(0);
    child.stdio[4].on('data', bytes => {
      this.bytes = Buffer.concat([this.bytes, bytes]);
      if (this.bytes.length > 2 * 1024 * 1024) return this.close(new Error('protocol frame too large'));
      let end;
      while ((end = this.bytes.indexOf(0)) >= 0) {
        const frame = this.bytes.subarray(0, end); this.bytes = this.bytes.subarray(end + 1);
        let value;
        try { value = JSON.parse(frame); } catch { return this.close(new Error('invalid protocol frame')); }
        if (value.id) {
          const waiter = this.pending.get(value.id); if (!waiter) continue;
          this.pending.delete(value.id); clearTimeout(waiter.timer);
          if (value.error) waiter.reject(new Error(`${waiter.method}: protocol error ${value.error.code}`));
          else waiter.resolve(value.result);
        } else this.emit('event', value);
      }
    });
    child.on('exit', () => this.close(new Error('test browser exited')));
    child.on('error', () => this.close(new Error('test browser could not start')));
    child.stdio[3].on('error', () => this.close(new Error('protocol writer closed')));
  }
  close(error) {
    this.closed ??= error;
    for (const waiter of this.pending.values()) { clearTimeout(waiter.timer); waiter.reject(this.closed); }
    this.pending.clear();
  }
  call(method, params = {}, sessionId) {
    if (this.closed) return Promise.reject(this.closed);
    if (this.pending.size >= 64) return Promise.reject(new Error('protocol pending limit'));
    return new Promise((resolveCall, reject) => {
      const id = ++this.next;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${method}: timeout`)); }, 8000);
      this.pending.set(id, { resolve: resolveCall, reject, timer, method });
      this.child.stdio[3].write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
    });
  }
}

const capture = kind => `const getter=Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator),'platform').get;
globalThis.__nativePlatformGetter=getter;
let receiver=false;try{Reflect.apply(getter,{},[])}catch(error){receiver=error instanceof TypeError}
const first={kind:${JSON.stringify(kind)},platform:navigator.platform,raw:Reflect.apply(getter,navigator,[]),
nativeGetter:Function.prototype.toString.call(getter).includes('[native code]'),receiver};
const proof=fetch('/proof',{method:'POST',body:JSON.stringify(first)});`;
const kinds = ['page', 'worker', 'module_worker', 'shared_worker', 'module_shared_worker', 'service_worker', 'module_service_worker'];
let child, pipe, server, directory, origin, policyFailure;
const targets = new Map(), paused = new Set(), proofs = new Map();

async function waitFor(condition, description, milliseconds = 15000) {
  const deadline = Date.now() + milliseconds;
  while (!condition()) {
    if (policyFailure) throw policyFailure;
    if (pipe?.closed) throw pipe.closed;
    if (Date.now() >= deadline) throw new Error(`timeout: ${description}`);
    await sleep(20);
  }
}

try {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(browser)) hash.update(chunk);
  assert.equal(hash.digest('hex'), options.sha256, 'test executable digest mismatch');
  report.executableSha256 = options.sha256;
  directory = await mkdtemp(join(tmpdir(), 'brows-native-platform-'));
  server = createServer(async (request, response) => {
    response.setHeader('Cache-Control', 'no-store');
    const url = new URL(request.url, origin);
    if (request.method === 'POST' && url.pathname === '/proof') {
      let body = '';
      for await (const chunk of request) { body += chunk; if (body.length > 4096) { response.writeHead(413).end(); return; } }
      try { const first = JSON.parse(body); if (kinds.includes(first.kind)) proofs.set(first.kind, first); } catch {}
      response.end('ok'); return;
    }
    let content;
    const kind = url.searchParams.get('kind');
    if (url.pathname === '/page') {
      response.setHeader('Content-Type', 'text/html');
      content = `<!doctype html><script>${capture('page')}
addEventListener('DOMContentLoaded',()=>{
globalThis.ownedWorkers=[new Worker('/worker.js?kind=worker'),new Worker('/module.mjs?kind=module_worker',{type:'module'}),
new SharedWorker('/shared.js?kind=shared_worker'),new SharedWorker('/module-shared.mjs?kind=module_shared_worker',{type:'module'})];
for(const worker of ownedWorkers.slice(2))worker.port.start();
navigator.serviceWorker.register('/classic-sw/sw.js',{scope:'/classic-sw/'});
navigator.serviceWorker.register('/module-sw/sw.mjs',{scope:'/module-sw/',type:'module'});
});</script>`;
    } else {
      response.setHeader('Content-Type', 'text/javascript');
      if (url.pathname === '/worker.js' && kind === 'worker') content = `${capture(kind)}setInterval(()=>{},1000);`;
      if (url.pathname === '/shared.js' && kind === 'shared_worker') content = `${capture(kind)}onconnect=e=>e.ports[0].start();`;
      if (url.pathname === '/first.mjs' && kinds.includes(kind)) content = `${capture(kind)}export{first,proof};`;
      if (url.pathname === '/module.mjs') content = `import '/first.mjs?kind=module_worker';setInterval(()=>{},1000);`;
      if (url.pathname === '/module-shared.mjs') content = `import '/first.mjs?kind=module_shared_worker';onconnect=e=>e.ports[0].start();`;
      const lifecycle = `addEventListener('install',e=>e.waitUntil(proof.then(()=>skipWaiting())));addEventListener('activate',e=>e.waitUntil(clients.claim()));`;
      if (url.pathname === '/classic-sw/sw.js') content = `${capture('service_worker')}${lifecycle}`;
      if (url.pathname === '/module-sw/sw.mjs') content = `import{proof}from '/first.mjs?kind=module_service_worker';${lifecycle}`;
    }
    if (content === undefined) response.writeHead(404).end(); else response.end(content);
  });
  await new Promise(resolveServer => server.listen(0, '127.0.0.1', resolveServer));
  origin = `http://127.0.0.1:${server.address().port}`;
  child = spawn(browser, ['--headless=new', '--no-startup-window', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--no-proxy-server', '--remote-debugging-pipe', `--user-data-dir=${directory}`],
  { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32', windowsHide: true });
  pipe = new Pipe(child);
  const version = await pipe.call('Browser.getVersion'); report.runtimeVersion = version.product;
  const autoAttach = types => ({ autoAttach: true, waitForDebuggerOnStart: true, flatten: true, filter: types.map(type => ({ type })) });
  async function apply(event) {
    const { sessionId, targetInfo } = event.params;
    if (targetInfo.url.startsWith('chrome-extension://')) { await pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId); return; }
    const target = { id: targetInfo.targetId, session: sessionId, kind: targetInfo.type, url: targetInfo.url, ready: false };
    targets.set(target.id, target);
    if (target.kind === 'service_worker') {
      const enabled = pipe.call('Debugger.enable', {}, sessionId), stopped = pipe.call('Debugger.pause', {}, sessionId);
      await pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId);
      await Promise.all([enabled, stopped]);
      await waitFor(() => paused.has(sessionId), 'Service Worker pause', 8000);
    }
    await pipe.call('Emulation.setUserAgentOverride', { userAgent: version.userAgent, platform: configured }, sessionId);
    if (target.kind === 'page') {
      await pipe.call('Target.setAutoAttach', autoAttach(['worker', 'iframe']), sessionId);
      await pipe.call('Page.enable', {}, sessionId);
      await pipe.call('ServiceWorker.enable', {}, sessionId);
    }
    if (target.kind === 'service_worker') await pipe.call('Debugger.disable', {}, sessionId);
    else await pipe.call('Runtime.runIfWaitingForDebugger', {}, sessionId);
    target.ready = true;
  }
  pipe.on('event', event => {
    if (event.method === 'Debugger.paused') paused.add(event.sessionId);
    if (event.method === 'Target.attachedToTarget') apply(event).catch(error => { policyFailure ??= error; });
  });
  await pipe.call('Target.setAutoAttach', autoAttach(['page', 'shared_worker', 'service_worker']));
  const page = await pipe.call('Target.createTarget', { url: 'about:blank' });
  await waitFor(() => targets.get(page.targetId)?.ready, 'owned page policy');
  await pipe.call('Page.navigate', { url: `${origin}/page` }, targets.get(page.targetId).session);
  await waitFor(() => kinds.every(kind => proofs.has(kind)), 'seven first-script observations');
  report.observations = kinds.map(kind => proofs.get(kind));
  for (const first of report.observations) {
    assert.equal(first.nativeGetter, true, `${first.kind}: original native getter`);
    assert.equal(first.receiver, true, `${first.kind}: native receiver validation`);
    assert.equal(first.raw, first.platform, `${first.kind}: raw/value match`);
    assert.equal(first.platform, first.kind === 'page' || options.expect === 'native' ? configured : host, `${first.kind}: platform`);
  }
  const worker = [...targets.values()].find(target => target.kind === 'worker' && target.url.includes('/worker.js'));
  assert(worker, 'classic Dedicated Worker target missing');
  await pipe.call('Emulation.setUserAgentOverride', { userAgent: version.userAgent, platform: '' }, worker.session);
  const cleared = await pipe.call('Runtime.evaluate', { expression: `({value:navigator.platform,sameGetter:Object.getOwnPropertyDescriptor(WorkerNavigator.prototype,'platform').get===globalThis.__nativePlatformGetter})`, returnByValue: true }, worker.session);
  assert.equal(cleared.result.value.value, host); assert.equal(cleared.result.value.sameGetter, true);
  report.clearOverride = cleared.result.value;
  report.nativeWorkerPlatform = options.expect === 'native'; report.assertionsPassed = true;
} catch (error) {
  report.failure = error.message;
  report.observations = [...proofs.values()];
} finally {
  pipe?.close(new Error('owned fixture cleanup'));
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    if (process.platform === 'win32') {
      await new Promise(done => { const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true }); killer.on('exit', done); killer.on('error', done); });
    } else { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }
    if (child.exitCode === null) await Promise.race([new Promise(done => child.once('exit', done)), sleep(5000)]);
  }
  server?.closeAllConnections(); if (server) await new Promise(done => server.close(done));
  if (directory) await rm(directory, { recursive: true, force: true });
  await mkdir(resolve(reportPath, '..'), { recursive: true });
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report));
}
process.exitCode = report.assertionsPassed ? 0 : 1;
