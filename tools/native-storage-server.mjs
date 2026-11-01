// Localhost-only storage observation for the ignored real Windows browser test.
// The test supplies a freshly created directory; no existing browser data is read.
import { randomBytes } from 'node:crypto';
import { appendFileSync, realpathSync, writeFileSync, renameSync } from 'node:fs';
import { createServer } from 'node:http';
import { basename, dirname, join, resolve, toNamespacedPath } from 'node:path';
import { fileURLToPath } from 'node:url';

const target = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), '../target'));
const directory = realpathSync(process.argv[2]);
// Rust canonicalize uses the Windows extended-length prefix; Node's script
// directory normally has the drive spelling. Compare the same namespace.
if (toNamespacedPath(dirname(directory)) !== toNamespacedPath(target) || !/^native-browser-[0-9a-f-]{36}$/.test(basename(directory))) {
  throw new Error('Expected a fresh native-browser test directory immediately under target.');
}
const token = randomBytes(16).toString('hex');
const runs = new Set(['a0', 'b0', 'c0', 'a1', 'b1', 'c1']);
const observed = new Set();
let origin;

function page(runId) {
  const config = JSON.stringify({ runId, marker: runId[0].toUpperCase(), token });
  return `<!doctype html><meta charset="utf-8"><title>Brows storage acceptance</title>
<pre id="result">Testing isolated browser storage…</pre><script nonce="${token}">
const config=${config};
const key='brows_native_acceptance';
const cookie=()=>document.cookie.split(';').map(v=>v.trim()).find(v=>v.startsWith(key+'='))?.slice(key.length+1)??null;
async function indexedStorage(marker) {
  const db=await new Promise((resolve,reject)=>{
    const request=indexedDB.open(key,1);
    request.onupgradeneeded=()=>request.result.createObjectStore('markers');
    request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
  });
  try {
    return await new Promise((resolve,reject)=>{
      const tx=db.transaction('markers','readwrite');const store=tx.objectStore('markers');
      let before=null;let after=null;
      const get=store.get('value');get.onsuccess=()=>{
        before=get.result??null;store.put(marker,'value');
        const reread=store.get('value');reread.onsuccess=()=>{after=reread.result??null;};
      };
      tx.oncomplete=()=>resolve({before,after});tx.onerror=()=>reject(tx.error);tx.onabort=()=>reject(tx.error??new Error('IndexedDB aborted'));
    });
  } finally { db.close(); }
}
(async()=>{
  let observation={runId:config.runId,userAgent:navigator.userAgent};
  try {
    observation.localBefore=localStorage.getItem(key);observation.cookieBefore=cookie();
    localStorage.setItem(key,config.marker);
    document.cookie=key+'='+config.marker+'; Path=/; Max-Age=86400; SameSite=Strict';
    observation.localAfter=localStorage.getItem(key);observation.cookieAfter=cookie();
    const idb=await indexedStorage(config.marker);observation.indexedBefore=idb.before;observation.indexedAfter=idb.after;
  } catch(error) { observation.error=String(error); }
  const response=await fetch('/'+config.token+'/observe/'+config.runId,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(observation)});
  document.getElementById('result').textContent=response.ok?JSON.stringify(observation):'Observation rejected: '+response.status;
})();</script>`;
}

const server = createServer({ maxHeaderSize: 8192 }, (request, response) => {
  const send = (status, body = '') => {
    response.writeHead(status, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store',
      'Content-Security-Policy': `default-src 'none'; script-src 'nonce-${token}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'` });
    response.end(body);
  };
  if (request.headers.host !== new URL(origin).host || !request.socket.remoteAddress?.endsWith('127.0.0.1')) return send(403);
  const parts = request.url.split('/');
  if (parts[1] !== token) return send(404);
  if (request.method === 'GET' && parts.length === 3 && runs.has(parts[2])) return send(200, page(parts[2]));
  const runId = parts[3];
  if (request.method !== 'POST' || parts.length !== 4 || parts[2] !== 'observe' || !runs.has(runId)) return send(404);
  if (request.headers.origin !== origin || request.headers['content-type'] !== 'application/json') return send(403);
  let size = 0;
  const chunks = [];
  request.on('data', chunk => { size += chunk.length; if (size > 16384) request.destroy(); else chunks.push(chunk); });
  request.on('end', () => {
    try {
      const observation = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (observation.runId !== runId || observed.has(runId)) return send(409);
      observation.httpCookie = request.headers.cookie ?? '';
      appendFileSync(join(directory, 'observations.jsonl'), JSON.stringify(observation) + '\n');
      observed.add(runId);
      send(200, 'Recorded');
    } catch { send(400); }
  });
});
server.maxConnections = 16;
server.requestTimeout = 5000;
server.headersTimeout = 5000;
server.setTimeout(5000, socket => socket.destroy());
server.on('clientError', (_error, socket) => socket.destroy());
server.listen(0, '127.0.0.1', () => {
  origin = 'http://127.0.0.1:' + server.address().port;
  const ready = join(directory, 'server.json');
  writeFileSync(ready + '.tmp', JSON.stringify({ origin, token }));
  renameSync(ready + '.tmp', ready);
});
