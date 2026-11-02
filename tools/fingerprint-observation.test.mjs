import assert from 'node:assert/strict';
import test from 'node:test';
import { assess, parseObservation, browserArgs, observationPage } from './fingerprint-observation.mjs';
const expected = { languages: ['pl-PL','pl'], hardware_concurrency: 3, device_memory: 0.5, screen_width: 1777, screen_height: 1111 };
function fixture() {
  const page = {userAgent: 'Browser/150', platform: 'Win32', languages: expected.languages, hardwareConcurrency: 3,
    deviceMemory: 0.5, screen: {width:1777, height:1111}, timezone:'Europe/Warsaw', clientHints: {platform:'Windows'}};
  return {page, iframe: structuredClone(page), worker: structuredClone(page), headers: {'user-agent':'Browser/150','accept-language':'pl-PL,pl;q=0.9','sec-ch-ua-platform':'"Windows"'}, canvas:{first:'pixels',second:'pixels'}};
}
test('observation comparison reports cross-context mismatch without hiding hook success', () => {
  const observed = fixture(); observed.worker.hardwareConcurrency = 16;
  const report = assess(observed, expected);
  assert.equal(report.hooksObserved, true); assert.equal(report.consistent, false); assert.equal(report.complete, false);
  assert.equal(report.checks.find(c=>c.key==='worker.hardwareConcurrency').status,'fail');
});
test('missing context evidence is never recorded as a pass', () => {
  const report = assess({}, expected);
  assert.equal(report.hooksObserved, false); assert.equal(report.consistent, false);
  assert.equal(report.checks.find(c=>c.key==='worker.userAgent').status,'not_checked');
});
test('network headers are compared to page declarations, not inferred from configuration', () => {
  const observed = fixture(); observed.headers['accept-language'] = 'en-US,en'; observed.headers['user-agent'] = 'Different';
  const report = assess(observed, expected);
  assert.equal(report.checks.find(c=>c.key==='request.userAgent').status,'fail');
  assert.equal(report.checks.find(c=>c.key==='request.primaryLanguage').status,'fail');
  assert.equal(report.checks.find(c=>c.key==='dnsLeak').status,'not_checked');
  assert.equal(report.checks.find(c=>c.key==='webrtcPublicEgress').status,'not_checked');
});
test('Canvas repeat readings must agree, independent of matching declarations', () => {
  const observed=fixture(); observed.canvas.second='changed';
  assert.equal(assess(observed,expected).checks.find(c=>c.key==='canvas.repeatStable').status,'fail');
});
test('only complete bounded hex observations are accepted', () => {
  const observed=fixture(); const hex=Buffer.from(JSON.stringify(observed)).toString('hex');
  assert.deepEqual(parseObservation(`<pre id="result">${hex}</pre>`),observed);
  for (const html of ['<pre id="result">pending</pre>','<pre id="result">1</pre>',`<pre id="result">${'aa'.repeat(150000)}</pre>`]) assert.throws(()=>parseObservation(html));
});
test('local measurement never disables sandboxing or exposes a debugging TCP port', () => {
  const args=browserArgs('/temporary','http://127.0.0.1/token/page','/extension');
  assert.ok(args.includes('--load-extension=/extension'));
  assert.ok(args.every(a=>!a.startsWith('--no-sandbox')&&!a.startsWith('--remote-debugging-port')));
  assert.ok(observationPage('nonce').includes('e.source === document.getElementById'));
});
