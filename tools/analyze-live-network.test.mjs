import test from 'node:test';
import assert from 'node:assert/strict';
import {analyzeLiveNetwork} from './analyze-live-network.mjs';
const sample = (direct = false) => ({schemaVersion: 2, https: {status: 'observed', ip: direct ? '198.51.100.9' : '203.0.113.9'}, https6: {status: 'not_checked'}, dns: ['dns4','dns6'].map(family => ({family,status:'observed',resolvers:['192.0.2.53']})), stun: ['google','cloudflare'].map(service => ({service,status:'complete',protocols: direct ? ['udp'] : [],srflx: direct ? ['198.51.100.9'] : []}))});
function report() {
  return {schemaVersion:2,complete:true,requestedEndpoints:1,direct:[sample(true),sample(true)],allBrowserChecksPassed:true,cases:['http','socks5'].flatMap(p => ['native','seeded'].flatMap(m => [0,1].map(round => ({label:`1-${p}-${m}`,round,expectedExit:'203.0.113.9',sample:sample()}))))};
}
test('recomputes scope; missing independent policy, IPv6 and TURN stay unaccepted', () => {
  const r = analyzeLiveNetwork(report());
  assert.equal(r.browserIpv4.status, 'pass');
  assert.deepEqual(r.stun.map(s=>s.status), ['pass','pass']);
  assert.equal(r.resolverPolicy.observedSamples,16);
  assert.equal(r.resolverPolicy.status,'not_checked');
  assert.equal(r.ipv6Egress.status,'not_checked');
  assert.equal(r.turn.status,'not_checked');
  assert.equal(r.allDimensionsAccepted,false);
});
test('detects a second-provider UDP leak even when producer pass flags are true', () => {
  const r=report(); r.cases[3].sample.stun[1].protocols=['udp'];
  assert.equal(analyzeLiveNetwork(r).stun[1].status,'fail');
  r.cases[3].sample.stun[1].protocols=[];
  r.direct[0].stun[1].srflx=[];
  assert.equal(analyzeLiveNetwork(r).stun[1].status,'not_checked');
});
test('rejects incomplete matrices, duplicate records and forged direct exits', () => {
  const r=report(); r.cases.pop(); assert.throws(()=>analyzeLiveNetwork(r));
  const omitted=report(); omitted.requestedEndpoints=2; assert.throws(()=>analyzeLiveNetwork(omitted));
  const d=report(); d.cases[0]=d.cases[1]; assert.throws(()=>analyzeLiveNetwork(d));
  const e=report(); e.cases[0].sample.https.ip='198.51.100.9'; assert.equal(analyzeLiveNetwork(e).browserIpv4.status,'fail');
});
test('checks independently supplied IPv4 and IPv6 resolver ranges; never learns them from observations', () => {
  const r=report(), policy={source:'https://operator.example/resolver-policy',dnsResolverCidrs:['192.0.2.0/24','2001:db8::/32']};
  r.cases[0].sample.dns[1].resolvers=['2001:db8::53'];
  assert.equal(analyzeLiveNetwork(r,policy).resolverPolicy.status,'pass');
  r.cases[0].sample.dns[1].resolvers=['2001:db9::53'];
  assert.equal(analyzeLiveNetwork(r,policy).resolverPolicy.status,'fail');
  assert.throws(()=>analyzeLiveNetwork(r,{dnsResolverCidrs:['192.0.2.0/24']}));
  assert.throws(()=>analyzeLiveNetwork(r,{...policy,dnsResolverCidrs:['192.0.2.0/99']}));
});
test('DNS6 observations cannot manufacture a direct IPv6 HTTPS control', () => {
  const r=report(); for (const c of r.cases) c.sample.https6={status:'observed',ip:'2001:db8::5'};
  assert.equal(analyzeLiveNetwork(r).ipv6Egress.directPositiveControl,false);
  r.direct.forEach(s=>{s.https6={status:'observed',ip:'2001:db8:1::9'};});
  const policy={source:'https://operator.example/egress',proxyIpv6Cidrs:['2001:db8::/48']};
  assert.equal(analyzeLiveNetwork(r,policy).ipv6Egress.status,'pass');
  r.cases[0].sample.https6.ip='2001:db8:1::9';
  assert.equal(analyzeLiveNetwork(r,policy).ipv6Egress.status,'fail');
});
