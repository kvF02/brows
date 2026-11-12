// Owned loopback pages and original getters only. No Brows rendering hooks.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
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
const report = { schemaVersion: 1, scope: 'isolated native screen geometry, first script and live OOPIF propagation',
  expectation: options.expect, executableSha256: options.sha256, assertionsPassed: false,
  nativeScreenPassed: false, runtimeAdmitted: false, compiledByThisTool: false, complete: false,
  checkpoints: [], failure: null };
const names = ['width', 'height', 'availWidth', 'availHeight'];
const prefix = `/screen-${randomUUID()}/`;
let child, pipe, server, directory, origin, crossOrigin, failure, current;
const targets = new Map(), proofs = [];

// The first document script captures getter identity and raw receiver behavior.
const readSource = `(()=>{
const names=${JSON.stringify(names)};
const originals=Object.fromEntries(names.map(name=>[name,Object.getOwnPropertyDescriptor(Screen.prototype,name).get]));
globalThis.__readOwnedScreen=()=>({
  values:Object.fromEntries(names.map(name=>[name,screen[name]])),
  getters:Object.fromEntries(names.map(name=>{const getter=Object.getOwnPropertyDescriptor(Screen.prototype,name).get;let receiver=false;
    try{Reflect.apply(getter,{},[])}catch(error){receiver=error instanceof TypeError}
    return[name,{nativeGetter:Function.prototype.toString.call(getter).includes('[native code]'),sameGetter:getter===originals[name],receiver,value:Reflect.apply(getter,screen,[])}];})),
  orientation:screen.orientation.type,dpr:devicePixelRatio,
  viewport:[innerWidth,innerHeight,outerWidth,outerHeight],
  deviceMedia:matchMedia('(device-width: '+screen.width+'px) and (device-height: '+screen.height+'px)').matches,
  wrongDeviceMedia:matchMedia('(device-width: '+(screen.width+1)+'px)').matches
});return globalThis.__readOwnedScreen();})()`;

async function waitFor(condition, label, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (!condition()) {
    if (failure) throw failure;
    if (pipe?.closed) throw pipe.closed;
    if (Date.now() >= deadline) throw new Error(`timeout: ${label}`);
    await sleep(20);
  }
  if (failure) throw failure;
}
async function read(session, expression = '__readOwnedScreen()') {
  const result = await pipe.call('Runtime.evaluate', { expression, returnByValue: true }, session);
  assert(!result.exceptionDetails && result.result?.value, 'owned screen reading failed');
  return result.result.value;
}
function validate(sample, expected) {
  assert.deepEqual(sample.values, expected.values, 'screen geometry');
  assert.equal(sample.orientation, expected.orientation, 'screen orientation');
  assert.equal(sample.deviceMedia, true, 'CSS device dimensions');
  assert.equal(sample.wrongDeviceMedia, false, 'CSS rejects wrong device dimensions');
  assert.equal(sample.dpr, report.host.dpr, 'native DPR');
  for (const name of names) {
    assert.equal(sample.getters[name].nativeGetter, true, `${name}: original native getter`);
    assert.equal(sample.getters[name].sameGetter, true, `${name}: getter identity`);
    assert.equal(sample.getters[name].receiver, true, `${name}: native receiver validation`);
    assert.equal(sample.getters[name].value, sample.values[name], `${name}: raw getter value`);
  }
}
function expected(kind, metrics) {
  if (!metrics || (options.expect === 'stock' && ['cross_frame', 'dynamic_frame'].includes(kind))) return report.host;
  return { values: { width: metrics.screenWidth, height: metrics.screenHeight,
    availWidth: metrics.screenWidth, availHeight: metrics.screenHeight },
  orientation: metrics.screenWidth < metrics.screenHeight ? 'portrait-primary' : 'landscape-primary' };
}
function metrics(width, height) {
  return { width: 0, height: 0, deviceScaleFactor: 0, mobile: false, dontSetVisibleSize: true,
    screenWidth: width, screenHeight: height,
    screenOrientation: { type: width < height ? 'portraitPrimary' : 'landscapePrimary', angle: 0 } };
}
async function firstCheckpoint(name, start, kinds, configured) {
  await waitFor(() => kinds.every(kind => proofs.slice(start).some(proof => proof.kind === kind)), name);
  const observations = kinds.map(kind => proofs.slice(start).find(proof => proof.kind === kind));
  const checkpoint = { name, assertionsPassed: false, observations };
  report.checkpoints.push(checkpoint);
  for (const proof of observations) validate(proof.screen, expected(proof.kind, configured));
  checkpoint.assertionsPassed = true;
}
async function liveCheckpoint(name, sessions, configured, initial) {
  const observations = [];
  // Observe actual propagation, with a bounded wait for the existing visual
  // properties IPC. Never resume first scripts to collect a corrected sample.
  const deadline = Date.now() + 5000;
  for (const [kind, session] of sessions) {
    let sample;
    do {
      if (failure) throw failure;
      sample = await read(session);
      if (JSON.stringify(sample.values) === JSON.stringify(expected(kind, configured).values)) break;
      if (Date.now() >= deadline) break;
      await sleep(20);
    } while (true);
    observations.push({ kind, screen: sample });
  }
  const checkpoint = { name, assertionsPassed: false, observations };
  report.checkpoints.push(checkpoint);
  for (const proof of observations) {
    validate(proof.screen, expected(proof.kind, configured));
    assert.deepEqual(proof.screen.viewport, initial.get(proof.kind).viewport, 'viewport/window geometry preserved');
  }
  checkpoint.assertionsPassed = true;
}

try {
  const hash = createHash('sha256'); for await (const bytes of createReadStream(browser)) hash.update(bytes);
  assert.equal(hash.digest('hex'), options.sha256, 'test executable digest mismatch');
  directory = await mkdtemp(join(tmpdir(), 'brows-native-screen-'));
  server = createServer(async (request, response) => {
    try {
      response.setHeader('Cache-Control', 'no-store');
      const url = new URL(request.url, origin);
      if (!url.pathname.startsWith(prefix)) { response.writeHead(404).end(); return; }
      if (request.method === 'POST' && url.pathname === `${prefix}proof`) {
        let body = ''; for await (const bytes of request) { body += bytes; if (body.length > 8192) throw new Error('proof limit'); }
        const proof = JSON.parse(body);
        assert(['page', 'frame', 'cross_frame', 'dynamic_frame'].includes(proof.kind), 'unexpected proof kind');
        proofs.push(proof); response.end('ok'); return;
      }
      const kind = url.searchParams.get('kind') ?? 'page';
      response.setHeader('Content-Type', 'text/html');
      const capture = `<script>const first=${readSource};fetch(${JSON.stringify(`${prefix}proof`)},{method:'POST',body:JSON.stringify({kind:${JSON.stringify(kind)},screen:first})});</script>`;
      const frames = kind === 'page' ? `<iframe src="${origin}${prefix}frame?kind=frame"></iframe><iframe src="${crossOrigin}${prefix}frame?kind=cross_frame"></iframe>
        <script>globalThis.__createOwnedCross=()=>{const frame=document.createElement('iframe');frame.src=${JSON.stringify(`${crossOrigin}${prefix}frame?kind=dynamic_frame`)};document.body.append(frame);};</script>` : '';
      response.end(`<!doctype html>${capture}<body>${frames}</body>`);
    } catch (error) { failure ??= error; response.writeHead(500).end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const port = server.address().port;
  origin = `http://127.0.0.1:${port}`; crossOrigin = `http://localhost:${port}`;
  child = spawn(browser, ['--headless=new', '--no-startup-window', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--no-proxy-server', '--site-per-process', '--host-resolver-rules=MAP localhost 127.0.0.1',
    '--window-size=760,600', '--force-device-scale-factor=1.25', '--remote-debugging-pipe', `--user-data-dir=${directory}`],
  { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32', windowsHide: true });
  pipe = new NativePipe(child);
  report.runtimeVersion = (await pipe.call('Browser.getVersion')).product;
  const attach = types => ({ autoAttach: true, waitForDebuggerOnStart: true, flatten: true, filter: types.map(type => ({ type })) });
  pipe.on('event', event => {
    if (event.method !== 'Target.attachedToTarget') return;
    (async () => {
      const { sessionId: session, targetInfo: info } = event.params;
      const target = { ...info, session, ready: false }; targets.set(info.targetId, target);
      await pipe.call('Target.setAutoAttach', attach(['iframe']), session);
      if (info.type === 'page') {
        await pipe.call('Page.enable', {}, session);
        if (current) await pipe.call('Emulation.setDeviceMetricsOverride', current, session);
      }
      await pipe.call('Runtime.runIfWaitingForDebugger', {}, session); target.ready = true;
    })().catch(error => { failure ??= error; });
  });
  await pipe.call('Target.setAutoAttach', attach(['page']));
  const { targetId } = await pipe.call('Target.createTarget', { url: 'about:blank' });
  await waitFor(() => targets.get(targetId)?.ready, 'owned page attachment');
  const page = targets.get(targetId);
  report.host = await read(page.session, readSource);
  validate(report.host, report.host);
  const wide = Math.max(report.host.values.width, report.host.values.height) + 137;
  const short = Math.min(report.host.values.width, report.host.values.height) + 73;
  current = metrics(wide, short);
  await pipe.call('Emulation.setDeviceMetricsOverride', current, page.session);
  await pipe.call('Page.navigate', { url: `${origin}${prefix}page` }, page.session);
  await firstCheckpoint('initial-landscape', 0, ['page', 'frame', 'cross_frame'], current);
  const targetInfos = (await pipe.call('Target.getTargets')).targetInfos;
  const crossInfo = targetInfos.find(target => target.type === 'iframe' && target.url.startsWith(crossOrigin));
  const cross = crossInfo && targets.get(crossInfo.targetId);
  assert(cross?.ready, 'cross-origin sample must come from a distinct OOPIF target');
  report.oopifObserved = true;
  const sessions = [['page', page.session], ['cross_frame', cross.session]];
  const initial = new Map(); for (const [kind, session] of sessions) initial.set(kind, await read(session));
  current = metrics(short, wide);
  await pipe.call('Emulation.setDeviceMetricsOverride', current, page.session);
  await liveCheckpoint('live-portrait-update', sessions, current, initial);
  let start = proofs.length;
  await pipe.call('Runtime.evaluate', { expression: '__createOwnedCross()' }, page.session);
  await firstCheckpoint('dynamic-oopif-creation', start, ['dynamic_frame'], current);
  await pipe.call('Emulation.clearDeviceMetricsOverride', {}, page.session);
  await liveCheckpoint('clear-to-host', sessions, null, initial);
  current = metrics(wide, short);
  await pipe.call('Emulation.setDeviceMetricsOverride', current, page.session);
  await liveCheckpoint('restore-landscape', sessions, current, initial);
  start = proofs.length;
  await pipe.call('Page.reload', { ignoreCache: true }, page.session);
  await firstCheckpoint('reload-first-script', start, ['page', 'frame', 'cross_frame'], current);
  report.assertionsPassed = true; report.nativeScreenPassed = options.expect === 'native';
} catch (error) { report.failure = error.message; }
finally {
  pipe?.close(new Error('owned fixture cleanup')); await killOwned(child);
  server?.closeAllConnections(); if (server) await new Promise(done => server.close(done));
  if (directory) await rm(directory, { recursive: true, force: true });
  await mkdir(resolve(reportPath, '..'), { recursive: true });
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ assertionsPassed: report.assertionsPassed, nativeScreenPassed: report.nativeScreenPassed,
    checkpoints: report.checkpoints.map(item => ({ name: item.name, passed: item.assertionsPassed })), failure: report.failure }));
}
process.exitCode = report.assertionsPassed ? 0 : 1;
