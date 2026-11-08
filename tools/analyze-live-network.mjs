// Recompute scoped conclusions from bounded raw observations, never producer pass flags.
import {BlockList, isIP} from 'node:net';
import {readFile, writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const providers = ['google', 'cloudflare'];
const canonicalAddress = value => {
  if (typeof value !== 'string' || !isIP(value) || value.includes('%')) return null;
  return isIP(value) === 6 ? new URL(`http://[${value}]/`).hostname.slice(1, -1) : value;
};
const checked = value => ['complete', 'timeout'].includes(value?.status);
const addresses = values => Array.isArray(values) && values.length > 0 && values.length <= 32 && values.every(ip => canonicalAddress(ip));
const ipv4 = sample => sample?.https?.status === 'observed' && isIP(sample.https.ip) === 4 ? sample.https.ip : null;
const ipv6 = sample => sample?.https6?.status === 'observed' && isIP(sample.https6.ip) === 6 ? canonicalAddress(sample.https6.ip) : null;
const probe = (sample, service) => sample?.stun?.find(value => value.service === service);
function policyRules(policy, name) {
  const cidrs = policy[name];
  if (!cidrs?.length) return null;
  if (!Array.isArray(cidrs) || cidrs.length > 64 || typeof policy.source !== 'string' || !policy.source.startsWith('https://')) throw Error('independent policy source and bounded CIDRs required');
  const rules = new BlockList();
  for (const cidr of cidrs) {
    if (typeof cidr !== 'string') throw Error('invalid CIDR');
    const [ip, prefix, extra] = cidr.split('/'), family = isIP(ip);
    if (extra !== undefined || !family || !/^\d{1,3}$/.test(prefix ?? '') || Number(prefix) > (family === 4 ? 32 : 128)) throw Error('invalid CIDR');
    rules.addSubnet(ip, Number(prefix), family === 4 ? 'ipv4' : 'ipv6');
  }
  return ip => rules.check(ip, isIP(ip) === 4 ? 'ipv4' : 'ipv6');
}
export function analyzeLiveNetwork(report, policy = {}) {
  if (report?.schemaVersion !== 2 || report.complete !== true || !Array.isArray(report.direct) || report.direct.length !== 2 || !Array.isArray(report.cases) || ![1,2].includes(report.requestedEndpoints) || report.cases.length !== report.requestedEndpoints * 8) throw Error('incomplete or unsupported browser evidence');
  const cases = report.cases, labels = new Set(), rounds = new Set(), endpoints = new Map();
  for (const item of cases) {
    if (!/^[12]-(http|socks5)-(native|seeded)$/.test(item.label) || ![0,1].includes(item.round) || item.sample?.schemaVersion !== 2 || isIP(item.expectedExit) !== 4) throw Error('invalid case identity or expectation');
    const key = `${item.label}/${item.round}`;
    if (rounds.has(key)) throw Error('duplicate measurement');
    rounds.add(key); labels.add(item.label);
    const endpoint = item.label[0];
    if (endpoints.has(endpoint) && endpoints.get(endpoint) !== item.expectedExit) throw Error('inconsistent endpoint expectation');
    endpoints.set(endpoint, item.expectedExit);
    if (!Array.isArray(item.sample.stun) || item.sample.stun.length !== 2 || providers.some(name => item.sample.stun.filter(v => v.service === name).length !== 1)) throw Error('both STUN providers required');
  }
  if (endpoints.size !== report.requestedEndpoints || !endpoints.has('1') || [...endpoints.keys()].some(id => ['http','socks5'].some(scheme => ['native','seeded'].some(mode => !labels.has(`${id}-${scheme}-${mode}`))))) throw Error('incomplete case matrix');
  const direct4 = report.direct.map(ipv4), exitsBad = cases.filter(c => !ipv4(c.sample) || ipv4(c.sample) !== c.expectedExit || direct4.includes(ipv4(c.sample))).map(c => `${c.label}/${c.round}`);
  const browserIpv4 = {status: exitsBad.length ? 'fail' : direct4.every(Boolean) ? 'pass' : 'not_checked', observations: cases.length, failedCases: exitsBad};
  const stun = providers.map(service => {
    const controls = report.direct.map(s => probe(s, service));
    const positive = controls.every(p => checked(p) && addresses(p.srflx) && p.protocols?.includes('udp'));
    const samples = cases.map(c => probe(c.sample, service));
    const udp = samples.some(p => p?.protocols?.includes('udp') || p?.srflx?.length);
    return {service, status: udp ? 'fail' : positive && samples.every(checked) ? 'pass' : 'not_checked', directPositiveControl: positive, proxiedUdpCandidateObserved: udp};
  });
  const resolverRule = policyRules(policy, 'dnsResolverCidrs');
  let dnsObserved = 0, dnsInvalid = 0, outside = 0;
  const controlResolvers = new Set(report.direct.flatMap(s => s.dns ?? []).flatMap(d => Array.isArray(d.resolvers) ? d.resolvers : []).map(canonicalAddress).filter(Boolean));
  const overlaps = new Set();
  for (const item of cases) {
    if (!Array.isArray(item.sample.dns) || item.sample.dns.length !== 2 || ['dns4','dns6'].some(f => item.sample.dns.filter(d => d.family === f).length !== 1)) throw Error('both DNS query families required');
    for (const dns of item.sample.dns) {
      if (dns.status !== 'observed') continue;
      if (!addresses(dns.resolvers)) { dnsInvalid++; continue; }
      dnsObserved++;
      for (const ip of dns.resolvers) {
        if (controlResolvers.has(canonicalAddress(ip))) overlaps.add(canonicalAddress(ip));
        if (resolverRule && !resolverRule(ip)) outside++;
      }
    }
  }
  const resolverPolicy = {status: dnsInvalid || outside ? 'fail' : resolverRule && dnsObserved === cases.length * 2 ? 'pass' : 'not_checked', observedSamples: dnsObserved, expectedSamples: cases.length * 2, invalidSamples: dnsInvalid, outsidePolicyAddresses: outside, directResolverOverlap: [...overlaps], reason: !resolverRule ? 'independent_resolver_policy_missing' : 'observed_challenge_ranges_only', scope: 'Observed challenge resolvers only; overlap alone does not prove leakage'};
  const direct6 = report.direct.map(ipv6), samples6 = cases.map(c => ipv6(c.sample));
  const ipv6Rule = policyRules(policy, 'proxyIpv6Cidrs');
  const direct6Leak = samples6.some(ip => ip && direct6.includes(ip));
  const ipv6Outside = samples6.some(ip => ip && ipv6Rule && !ipv6Rule(ip));
  const ipv6Egress = {status: direct6Leak || ipv6Outside ? 'fail' : direct6.every(Boolean) && samples6.every(Boolean) && ipv6Rule ? 'pass' : 'not_checked', directPositiveControl: direct6.every(Boolean), proxiedObservations: samples6.filter(Boolean).length, reason: !direct6.every(Boolean) ? 'no_direct_ipv6_https_control' : !ipv6Rule ? 'independent_proxy_ipv6_policy_missing' : 'observed_https_egress_only'};
  const turn = {status: 'not_checked', reason: 'authenticated_browser_relay_and_route_evidence_required', scope: 'A native TURN protocol preflight cannot establish browser TURN routing'};
  const results = [browserIpv4, ...stun, resolverPolicy, ipv6Egress, turn];
  return {schemaVersion: 1, productCommit: report.productCommit, browserVersion: report.browserVersion, completeEvidence: true, allDimensionsAccepted: results.every(r => r.status === 'pass'), failedDimensions: results.filter(r => r.status === 'fail').length, browserIpv4, stun, resolverPolicy, ipv6Egress, turn};
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const bytes = await readFile(process.argv[2]);
  if (bytes.length > 1024 * 1024) throw Error('evidence file exceeds limit');
  const policy = process.argv[4] ? JSON.parse(await readFile(process.argv[4], 'utf8')) : {};
  const result = analyzeLiveNetwork(JSON.parse(bytes), policy);
  await writeFile(process.argv[3] ?? 'target/live-network-analysis.json', JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
  if (result.failedDimensions) process.exitCode = 1;
}
