// Isolated read-only measurements of the production hook, never a user's data.
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const readSignals = () => ({
  userAgent: navigator.userAgent, platform: navigator.platform,
  languages: Array.from(navigator.languages), hardwareConcurrency: navigator.hardwareConcurrency,
  deviceMemory: navigator.deviceMemory, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  clientHints: navigator.userAgentData?.toJSON(),
  screen: typeof screen === 'undefined' ? null : { width: screen.width, height: screen.height },
});
const readCanvas = () => {
  const canvas = document.createElement('canvas'); canvas.width = 32; canvas.height = 16;
  const ctx = canvas.getContext('2d'); ctx.fillStyle = '#a42367'; ctx.fillRect(0, 0, 32, 16);
  const data = () => Array.from(ctx.getImageData(0, 0, 32, 16).data).join(',');
  return { first: data(), second: data(), exported: canvas.toDataURL() };
};
const digest = (value) => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const equal = (a, b) => JSON.stringify(a) === JSON.stringify(b);

export function assess(observed, expected) {
  const checks = [];
  const add = (key, wanted, actual, scope = 'observed') => checks.push({ key, scope, expected: wanted, actual, status: wanted === undefined || actual === undefined || actual === null ? 'not_checked' : equal(wanted, actual) ? 'pass' : 'fail' });
  const page = observed.page ?? {};
  add('page.languages', expected.languages, page.languages);
  add('page.hardwareConcurrency', expected.hardware_concurrency, page.hardwareConcurrency);
  add('page.deviceMemory', expected.device_memory, page.deviceMemory);
  add('page.screen', { width: expected.screen_width, height: expected.screen_height }, page.screen);
  for (const field of ['userAgent', 'platform', 'languages', 'hardwareConcurrency', 'deviceMemory', 'clientHints', 'timezone']) {
    add(`iframe.${field}`, page[field], observed.iframe?.[field]);
    add(`worker.${field}`, page[field], observed.worker?.[field]);
  }
  const request = observed.headers ?? {};
  add('request.userAgent', page.userAgent, request['user-agent']);
  const primary = request['accept-language']?.split(',')[0]?.split(';')[0];
  add('request.primaryLanguage', expected.languages[0], primary);
  const chPlatform = request['sec-ch-ua-platform']?.replaceAll('"', '');
  if (page.clientHints?.platform) add('request.clientHintsPlatform', page.clientHints.platform, chPlatform);
  else checks.push({ key: 'request.clientHintsPlatform', scope: 'uncovered', status: 'not_checked' });
  if (observed.canvas) add('canvas.repeatStable', digest(observed.canvas.first), digest(observed.canvas.second));
  for (const key of ['dnsLeak', 'webrtcPublicEgress', 'tlsHttp2', 'fonts', 'gpuPixels']) checks.push({ key, scope: 'uncovered', status: 'not_checked' });
  return { checks, hooksObserved: checks.slice(0, 4).every(c => c.status === 'pass'),
    consistent: checks.filter(c => c.scope === 'observed').every(c => c.status === 'pass'), complete: false };
}

export function observationPage(token, frame = false) {
  const script = frame ? `parent.postMessage({token:${JSON.stringify(token)}, iframe:(${readSignals})()}, location.origin);` : `
    const page = (${readSignals})(); const canvas = (${readCanvas})();
    const iframe = new Promise(resolve => addEventListener('message', e => {
      if (e.origin === location.origin && e.source === document.getElementById('frame').contentWindow && e.data?.token === ${JSON.stringify(token)}) resolve(e.data.iframe);
    }));
    const worker = new Promise((resolve, reject) => { const w = new Worker('./worker');
      w.onmessage = e => { w.terminate(); resolve(e.data); }; w.onerror = reject; });
    Promise.all([iframe, worker, fetch('./headers').then(r => r.json())]).then(([iframe, worker, headers]) => {
      const result = {page, iframe, worker, headers, canvas};
      document.getElementById('result').textContent = Array.from(new TextEncoder().encode(JSON.stringify(result)), b => b.toString(16).padStart(2,'0')).join('');
    }).catch(() => { document.getElementById('result').textContent = 'error'; });`;
  return `<!doctype html><meta charset="utf-8"><script>${script}</script><pre id="result">pending</pre>${frame ? '' : '<iframe id="frame" src="./frame"></iframe>'}`;
}

export function parseObservation(html) {
  const match = html.match(/<pre id="result">([a-f0-9]+)<\/pre>/);
  if (!match || match[1].length % 2 || match[1].length > 256 * 1024) throw new Error('Incomplete or oversized browser observation');
  return JSON.parse(Buffer.from(match[1], 'hex').toString('utf8'));
}

export function browserArgs(directory, url, extension, language = 'pl-PL') {
  return ['--headless=new', '--dump-dom', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--disable-component-update', '--disable-sync',
    '--webrtc-ip-handling-policy=disable_non_proxied_udp', '--window-size=900,700',
    '--virtual-time-budget=5000', `--lang=${language}`, `--user-data-dir=${directory}`,
    ...(extension ? [`--load-extension=${extension}`] : []), url];
}

async function dump(browser, args) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(browser, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
    let size = 0; const chunks = []; let error;
    const timer = setTimeout(() => { error = new Error('Browser observation timed out'); child.kill(); }, 30000);
    child.stdout.on('data', b => { size += b.length; if (size > 512 * 1024) { error = new Error('Browser observation too large'); child.kill(); } else chunks.push(b); });
    child.on('error', cause => { clearTimeout(timer); rejectRun(cause); });
    child.on('close', code => { clearTimeout(timer); if (error || code !== 0) rejectRun(error ?? new Error(`Browser exit ${code}`)); else resolveRun(parseObservation(Buffer.concat(chunks).toString())); });
  });
}

export async function observeBrowser(browser, outputPath) {
  const target = join(root, 'target'); await mkdir(target, { recursive: true });
  const temporary = await mkdtemp(join(target, 'observation-'));
  const token = randomUUID();
  const persona = { seed: 'brows-observation-fixture', platform: process.platform === 'win32' ? 'windows' : 'linux', languages: ['pl-PL', 'pl'], hardware_concurrency: 3,
    device_memory: 0.5, touch_points: 0, screen_width: 1777, screen_height: 1111, color_depth: 24,
    webgl_vendor: 'Brows observation vendor', webgl_renderer: 'Brows observation renderer', spoof_canvas: true, spoof_audio: true };
  const server = createServer((req, res) => {
    const route = req.url;
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('Accept-CH', 'Sec-CH-UA-Platform, Sec-CH-UA-Full-Version-List');
    if (req.method !== 'GET' || !route.startsWith(`/${token}/`)) { res.writeHead(404).end(); return; }
    const endpoint = route.slice(token.length + 2);
    if (endpoint === 'worker') { res.setHeader('Content-Type', 'text/javascript'); res.end(`postMessage((${readSignals})());`); }
    else if (endpoint === 'headers') { res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify(Object.fromEntries(['user-agent', 'accept-language', 'sec-ch-ua-platform', 'sec-ch-ua-full-version-list'].map(k => [k, req.headers[k] ?? null])))); }
    else if (endpoint === 'page' || endpoint === 'frame') { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(observationPage(token, endpoint === 'frame')); }
    else res.writeHead(404).end();
  });
  try {
    const extension = join(temporary, 'extension'); await mkdir(extension);
    const templates = join(root, 'crates/application/src/stealth_template');
    const json = JSON.stringify(persona).replaceAll('\\', '\\\\').replaceAll("'", "\\'");
    await writeFile(join(extension, 'content.js'), (await readFile(join(templates, 'content.template.js'), 'utf8')).replace('{{PERSONA_JSON}}', json));
    await writeFile(join(extension, 'manifest.json'), (await readFile(join(templates, 'manifest.template.json'), 'utf8')).replaceAll('{{profile_id}}', 'observation'));
    await writeFile(join(extension, 'background.js'), await readFile(join(templates, 'background.template.js')));
    // Production language preferences, not --lang alone, determine requests.
    for (const name of ['baseline', 'first', 'restart']) {
      await mkdir(join(temporary, name, 'Default'), { recursive: true });
      await writeFile(join(temporary, name, 'Default/Preferences'), JSON.stringify({intl: {accept_languages: persona.languages.join(',')}}));
    }
    await new Promise((yes, no) => { server.once('error', no); server.listen(0, '127.0.0.1', yes); });
    const url = `http://127.0.0.1:${server.address().port}/${token}/page`;
    const baseline = await dump(browser, browserArgs(join(temporary, 'baseline'), url));
    const major = baseline.page.userAgent.match(/(?:HeadlessChrome|Chrome)\/(\d+)/)?.[1];
    if (!major) throw new Error('Cannot determine the real Chromium version');
    const platform = process.platform === 'win32' ? 'Windows NT 10.0; Win64; x64' : 'X11; Linux x86_64';
    const userAgent = `Mozilla/5.0 (${platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${major}.0.0.0 Safari/537.36`;
    const configuredArgs = name => {
      const args = browserArgs(join(temporary, name), url, extension);
      args.splice(args.length - 1, 0, `--user-agent=${userAgent}`, '--force-device-scale-factor=1');
      return args;
    };
    const first = await dump(browser, configuredArgs('first'));
    const restart = await dump(browser, configuredArgs('restart'));
    const result = assess(first, persona);
    const repeatStable = digest(first.canvas) === digest(restart.canvas) && equal(first.page, restart.page);
    const report = { formatVersion: 1, generatedAt: new Date().toISOString(), browser: resolve(browser),
      scope: 'isolated temporary profile using production hook and UA derivation; not an observation of an existing user session',
      runtimeVersion: baseline.page.userAgent.match(/(?:HeadlessChrome|Chrome)\/([\d.]+)/)?.[1],
      expected: persona, baseline, observed: first, repeated: restart, repeatStable, ...result };
    if (outputPath) { await mkdir(dirname(resolve(outputPath)), { recursive: true }); await writeFile(outputPath, JSON.stringify(report, null, 2)); }
    return report;
  } finally {
    await new Promise(yes => server.close(yes));
    await rm(temporary, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2); const browser = args[0];
  if (!browser) { console.error('Usage: node tools/fingerprint-observation.mjs <browser.exe> [report.json] [--require-consistent]'); process.exitCode = 2; }
  else try {
    const report = await observeBrowser(browser, args[1]?.startsWith('--') ? undefined : args[1]);
    console.log(JSON.stringify(report, null, 2));
    if (!report.hooksObserved || !report.repeatStable || (args.includes('--require-consistent') && !report.consistent)) process.exitCode = 1;
  } catch (error) { console.error(error.message); process.exitCode = 2; }
}
