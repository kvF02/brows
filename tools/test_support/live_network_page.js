// Test-only page. Fixed public services, no credentials, permissions or remote code.
void (async () => {
  const report = {kind: 'network', schemaVersion: 1};
  const json = async url => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10000);
    try {
      const response = await fetch(url, {
        signal: controller.signal, credentials: 'omit', cache: 'no-store', redirect: 'error',
      });
      if (response.status !== 200) throw new Error('http_' + response.status);
      const reader = response.body.getReader();
      const chunks = []; let size = 0;
      for (;;) {
        const {done, value} = await reader.read();
        if (done) break;
        size += value.length;
        if (size > 8192) { controller.abort(); throw new Error('size'); }
        chunks.push(value);
      }
      const bytes = new Uint8Array(size); let offset = 0;
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
      return JSON.parse(new TextDecoder().decode(bytes));
    } finally { clearTimeout(timer); }
  };
  const failure = error => {
    if (error?.name === 'AbortError') return 'request_timeout';
    if (error?.message === 'size') return 'response_too_large';
    if (/^http_[1-5][0-9]{2}$/.test(error?.message)) return error.message;
    if (error?.name === 'SyntaxError') return 'invalid_json';
    return 'request_failed';
  };
  const ip = json('https://api.ipify.org/?format=json')
    .then(value => ({status: 'observed', ip: typeof value.ip === 'string' ? value.ip.slice(0, 64) : null}))
    .catch(error => ({status: 'not_checked', reason: failure(error)}));
  const dns = Promise.all(['dns4', 'dns6'].map(async family => {
    const nonce = Array.from(crypto.getRandomValues(new Uint8Array(6)), n => n.toString(16).padStart(2, '0')).join('');
    try {
      // Operator-owned authoritative DNS test. A new hostname prevents cache reuse.
      const value = await json(`https://${nonce}.${family}.browserleaks.net/`);
      const resolvers = value && typeof value === 'object' && !Array.isArray(value) ? Object.keys(value) : [];
      return {family, status: resolvers.length ? 'observed' : 'not_checked', resolvers: resolvers.slice(0, 32)};
    } catch (error) { return {family, status: 'not_checked', reason: failure(error), resolvers: []}; }
  }));
  const rtc = new Promise(resolve => {
    let peer, timer, finished = false;
    const types = new Set(), protocols = new Set(), srflx = new Set();
    const finish = status => {
      if (finished) return;
      finished = true; clearTimeout(timer); peer?.close();
      resolve({status, types: [...types].sort(), protocols: [...protocols].sort(), srflx: [...srflx].sort()});
    };
    if (typeof RTCPeerConnection !== 'function') { finish('unavailable'); return; }
    try {
      peer = new RTCPeerConnection({iceServers: [{urls: 'stun:stun.l.google.com:19302'}]});
      timer = setTimeout(() => finish('timeout'), 7000);
      peer.onicecandidate = event => {
        if (!event.candidate) { finish('complete'); return; }
        const candidate = event.candidate;
        if (['host', 'srflx', 'relay', 'prflx'].includes(candidate.type)) types.add(candidate.type);
        if (['udp', 'tcp'].includes(candidate.protocol)) protocols.add(candidate.protocol);
        // No SDP, ICE credentials, private host addresses or device identifiers.
        if (candidate.type === 'srflx' && typeof candidate.address === 'string') srflx.add(candidate.address.slice(0, 64));
      };
      peer.createDataChannel('brows-network-acceptance');
      peer.createOffer().then(offer => peer.setLocalDescription(offer)).catch(() => finish('error'));
    } catch { finish('error'); }
  });
  try {
    const canvas = document.createElement('canvas'); canvas.width = 16; canvas.height = 16;
    const context = canvas.getContext('2d'); context.fillStyle = '#a42367'; context.fillRect(0, 0, 16, 16);
    const read = () => Array.from(context.getImageData(0, 0, 16, 16).data, b => b.toString(16).padStart(2, '0')).join('');
    report.fingerprint = {
      platform: navigator.platform, hardwareConcurrency: navigator.hardwareConcurrency,
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, canvas: read(), repeat: read(),
    };
  } catch { report.fingerprint = {status: 'error'}; }
  [report.https, report.dns, report.webrtc] = await Promise.all([ip, dns, rtc]);
  await fetch(location.pathname.replace(/page$/, 'proof'), {
    method: 'POST', body: JSON.stringify(report), credentials: 'omit',
  });
})();
