// A disposable first-party site. Tokens stay in memory; evidence contains only
// account labels and booleans. This does not use or inspect real accounts.
import {createServer as httpsServer} from 'node:https';
import {createServer as httpServer} from 'node:http';
import {randomBytes} from 'node:crypto';
import {once} from 'node:events';
import {readFile, writeFile, rename} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import path from 'node:path';

// Never replace a file the Rust poller may currently have open on Windows.
// Each published snapshot is immutable; only complete .json files are read.
export function createProofPublisher(directory) {
  let sequence = 0;
  let writes = Promise.resolve();
  return value => {
    const file = path.join(directory, `proofs-${++sequence}.json`);
    const bytes = JSON.stringify(value);
    writes = writes.then(async () => {
      await writeFile(file + '.tmp', bytes, {flag: 'wx'});
      await rename(file + '.tmp', file);
    });
    return writes;
  };
}

export async function startLoginFixture({tls, onProof = () => {}} = {}) {
  const tokens = new Map(['a','b'].map(account => [randomBytes(32).toString('hex'), account]));
  const accounts = new Map([...tokens].map(([token, account]) => [account, token]));
  const proofs = {};
  const cookieName = '__Host-brows-fixture';
  const record = async (key, value) => { proofs[key] = value; await onProof({...proofs}); };
  const handler = async (req, res) => {
    res.setHeader('Cache-Control', 'no-store');
    const url = new URL(req.url, 'https://localhost');
    const token = req.headers.cookie?.split(';').map(s => s.trim()).find(s => s.startsWith(cookieName+'='))?.slice(cookieName.length+1);
    const account = tokens.get(token) ?? null;
    const match = /^\/(enroll|check|proof|closed)\/([a-z0-9-]{1,80})\/([ab])$/.exec(url.pathname);
    if (!match) { res.writeHead(404).end(); return; }
    const [, action, round, expected] = match;
    const key = round+'/'+expected;
    if (action === 'closed' && req.method === 'POST') {
      await record('closed/'+key, true); res.end('ok'); return;
    }
    if (action === 'proof' && req.method === 'POST') {
      const bytes = []; let size = 0;
      for await (const chunk of req) { size += chunk.length; if (size > 4096) { res.writeHead(413).end(); return; } bytes.push(chunk); }
      let safe;
      try {
        const body = JSON.parse(Buffer.concat(bytes));
        safe = {account, local: body.local ?? null, indexed: body.indexed ?? null, cache: body.cache ?? null,
          httpOnly: body.httpOnly === true, secureContext: body.secureContext === true, error: body.error ? 'site-storage-error' : null};
        if (![safe.local,safe.indexed,safe.cache].every(v => v === null || v === 'a' || v === 'b')) throw Error('Invalid marker');
      } catch { res.writeHead(400).end(); return; }
      await record(key, safe); res.end('ok');
      return;
    }
    if (req.method !== 'GET' || !['enroll','check'].includes(action)) { res.writeHead(405).end(); return; }
    const enroll = action === 'enroll';
    if (enroll) res.setHeader('Set-Cookie', `${cookieName}=${accounts.get(expected)}; Path=/; Max-Age=86400; Secure; HttpOnly; SameSite=Strict`);
    const script = (await readFile(new URL('./login_page.js', import.meta.url), 'utf8'))
      .replace('__BROWS_LOGIN_CONFIG__', JSON.stringify({enroll, account: expected, round, cookieName}));
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    res.end('<!doctype html><meta charset="utf-8"><title>Brows disposable login fixture</title><p>Disposable site storage</p><script>'+script+'</script>');
  };
  const server = tls ? httpsServer(tls, (req,res) => void handler(req,res).catch(() => {res.writeHead(500).end();}))
    : httpServer((req,res) => void handler(req,res).catch(() => {res.writeHead(500).end();}));
  server.requestTimeout = 5000; server.headersTimeout = 5000;
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  return {origin: `${tls ? 'https' : 'http'}://localhost:${server.address().port}`, proofs,
    close: async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }};
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  if (process.platform !== 'win32' || process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted') throw Error('Hosted Windows fixture only');
  const [pfx, directory] = process.argv.slice(2);
  if (!pfx || !directory) throw Error('Expected fixture certificate and directory');
  let writes = Promise.resolve();
  const publish = createProofPublisher(directory);
  const fixture = await startLoginFixture({tls: {pfx: await readFile(pfx), passphrase: 'brows-ci-fixture-only'},
    onProof: value => {
      writes = publish(value);
      writes.catch(error => { process.stderr.write(`Login proof publication failed: ${error.code ?? 'unknown'}\n`); process.exitCode = 1; });
      return writes;
    }});
  await writeFile(path.join(directory, 'ready.json'), JSON.stringify({origin: fixture.origin}));
  process.stdin.resume(); process.stdin.on('end', async () => { await fixture.close(); await writes.catch(() => {}); });
}
