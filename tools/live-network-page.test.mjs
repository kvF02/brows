import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
import {webcrypto} from 'node:crypto';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = await readFile(new URL('./test_support/live_network_page.js', import.meta.url), 'utf8');
async function run({ip = {ip: '203.0.113.9'}, dns = {'192.0.2.53': []}, rtc = 'complete', oversized = false} = {}) {
  const requests = [], peers = [];
  let receive;
  const proof = new Promise(resolve => { receive = resolve; });
  class Peer {
    constructor(config) { this.config = config; peers.push(this); }
    createDataChannel() {}
    async createOffer() { if (rtc === 'error') throw new Error('offer refused'); return {}; }
    async setLocalDescription() {
      if (rtc === 'srflx') this.onicecandidate({candidate: {type: 'srflx', protocol: 'udp', address: '198.51.100.6'}});
      this.onicecandidate({candidate: null});
    }
    close() { this.closed = true; }
  }
  const context = {
    fetch: async (url, options) => {
      requests.push({url, options});
      if (url.startsWith('/live-network/')) { receive(JSON.parse(options.body)); return new Response('ok'); }
      if (options.signal.aborted) throw new Error('aborted');
      if (url.includes('ipify')) return new Response(oversized ? 'x'.repeat(8193) : JSON.stringify(ip));
      return new Response(JSON.stringify(dns));
    },
    navigator: {platform: 'Win32', hardwareConcurrency: 3},
    document: {createElement: () => ({getContext: () => ({fillRect() {}, getImageData: () => ({data: new Uint8Array(1024)})})})},
    location: {pathname: '/live-network/example/page'},
    crypto: webcrypto, AbortController, TextDecoder, Uint8Array, setTimeout, clearTimeout, Intl,
  };
  if (rtc !== 'unavailable') context.RTCPeerConnection = Peer;
  vm.runInNewContext(source, context);
  return {report: await proof, requests, peers};
}

test('collects actual exit, fresh DNS names and positive STUN evidence without credentials', async () => {
  const {report, requests, peers} = await run({rtc: 'srflx'});
  assert.equal(report.https.ip, '203.0.113.9');
  assert.deepEqual(report.dns.map(v => v.status), ['observed', 'observed']);
  const names = requests.filter(v => v.url.includes('browserleaks.net')).map(v => new URL(v.url).hostname);
  assert.equal(names.length, 2);
  for (const name of names) assert.match(name, /^[0-9a-f]{12}\.dns[46]\.browserleaks\.net$/);
  assert.notEqual(names[0].split('.')[0], names[1].split('.')[0]);
  assert.deepEqual(report.webrtc.srflx, ['198.51.100.6']);
  assert.deepEqual(report.webrtc.protocols, ['udp']);
  assert.equal(peers[0].closed, true);
  assert.equal(report.fingerprint.canvas.length, 2048);
  assert.equal(report.fingerprint.canvas, report.fingerprint.repeat);
  for (const request of requests) {
    assert.equal(request.options.credentials, 'omit');
    assert.equal(request.options.headers, undefined);
  }
  assert.equal(JSON.stringify(report).includes('sdp'), false);
});

test('missing resolver and ICE evidence remains incomplete', async () => {
  const {report} = await run({ip: {}, dns: {}, rtc: 'unavailable'});
  assert.equal(report.https.ip, null);
  assert.deepEqual(report.dns.map(v => v.status), ['not_checked', 'not_checked']);
  assert.equal(report.webrtc.status, 'unavailable');
  assert.deepEqual(report.webrtc.srflx, []);
});

test('oversized public responses and RTC errors do not become successful measurements', async () => {
  const {report, peers} = await run({oversized: true, rtc: 'error'});
  assert.equal(report.https.status, 'not_checked');
  assert.equal(report.https.reason, 'response_too_large');
  assert.equal(report.webrtc.status, 'error');
  assert.equal(peers[0].closed, true);
});
