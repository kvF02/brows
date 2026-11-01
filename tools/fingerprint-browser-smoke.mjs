// Optional real-browser capability check. Uses only localhost and a fresh
// workspace-owned profile; it never opens the user's browser data or App.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const browser = process.argv[2] || 'C:/Program Files/Google/Chrome/Application/chrome.exe';
if (!existsSync(browser)) {
  console.error('Browser executable not found. Pass an absolute Chromium executable path as the first argument.');
  process.exit(2);
}
const target = join(root, 'target');
mkdirSync(target, { recursive: true });
const targetReal = realpathSync(target);
const temporary = mkdtempSync(join(targetReal, 'fingerprint-smoke-'));
const extension = join(temporary, 'extension');
mkdirSync(extension);
const persona = {
  seed: 'local-browser-capability-smoke', platform: 'windows',
  languages: ['pl-PL', 'pl'], hardware_concurrency: 3, device_memory: 0.5,
  touch_points: 0, screen_width: 1777, screen_height: 1111, color_depth: 24,
  scale_factor: 1, webgl_vendor: 'Brows smoke vendor', webgl_renderer: 'Brows smoke renderer',
  spoof_canvas: true, spoof_audio: true,
};
const templateRoot = join(root, 'crates', 'application', 'src', 'stealth_template');
const serialized = JSON.stringify(persona).replaceAll('\\', '\\\\').replaceAll("'", "\\'");
writeFileSync(join(extension, 'content.js'), readFileSync(join(templateRoot, 'content.template.js'), 'utf8').replace('{{PERSONA_JSON}}', serialized));
writeFileSync(join(extension, 'manifest.json'), readFileSync(join(templateRoot, 'manifest.template.json'), 'utf8').replaceAll('{{profile_id}}', 'local-capability-check'));
writeFileSync(join(extension, 'background.js'), readFileSync(join(templateRoot, 'background.template.js'), 'utf8'));

const html = '<!doctype html><meta charset="utf-8"><pre id="result">pending</pre><script>' +
  'const gl=document.createElement("canvas").getContext("webgl");' +
  'const debug=gl&&gl.getExtension("WEBGL_debug_renderer_info");' +
  'document.getElementById("result").textContent=JSON.stringify({' +
  'userAgent:navigator.userAgent,language:navigator.language,languages:[...navigator.languages],' +
  'hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory,' +
  'deviceMemoryType:typeof navigator.deviceMemory,screenWidth:screen.width,screenHeight:screen.height,' +
  'webglVendor:debug?gl.getParameter(debug.UNMASKED_VENDOR_WEBGL):null});</script>';
const server = createServer((request, response) => {
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(html);
});

function runBrowser(url, withExtension) {
  return new Promise((resolveRun, rejectRun) => {
    const args = [
      '--headless=new', '--dump-dom', '--no-first-run', '--no-default-browser-check',
      '--disable-background-networking', '--disable-component-update', '--disable-sync',
      '--disable-default-apps', '--disable-features=OptimizationHints,MediaRouter',
      '--window-size=900,700', '--virtual-time-budget=2000',
      '--user-data-dir=' + join(temporary, withExtension ? 'profile-extension' : 'profile-baseline'),
    ];
    if (withExtension) args.push('--load-extension=' + extension);
    args.push(url);
    const processHandle = spawn(browser, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '', errorOutput = '', settled = false;
    const timeout = setTimeout(() => { processHandle.kill(); finish(new Error('Browser smoke timed out after 30 seconds')); }, 30_000);
    function finish(error, value) {
      if (settled) return;
      settled = true; clearTimeout(timeout);
      if (error) rejectRun(error); else resolveRun(value);
    }
    processHandle.stdout.on('data', data => { output += data; });
    processHandle.stderr.on('data', data => { errorOutput += data; });
    processHandle.on('error', error => finish(error));
    processHandle.on('close', code => {
      if (code !== 0) return finish(new Error('Browser exited with code ' + code + ': ' + errorOutput.slice(-1500)));
      const match = output.match(/<pre id="result">([^<]*)<\/pre>/);
      if (!match || match[1] === 'pending') return finish(new Error('Browser did not produce localhost observation: ' + errorOutput.slice(-1500)));
      try {
        const decoded = match[1].replaceAll('&quot;', '"').replaceAll('&#39;', "'").replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&');
        finish(null, JSON.parse(decoded));
      } catch (error) { finish(error); }
    });
  });
}

try {
  await new Promise((resolveListen, rejectListen) => {
    server.once('error', rejectListen);
    server.listen(0, '127.0.0.1', resolveListen);
  });
  const url = 'http://127.0.0.1:' + server.address().port + '/';
  const baseline = await runBrowser(url, false);
  const withExtension = await runBrowser(url, true);
  const hooksObserved = withExtension.language === 'pl-PL' &&
    JSON.stringify(withExtension.languages) === '["pl-PL","pl"]' &&
    withExtension.hardwareConcurrency === 3 && withExtension.deviceMemory === 0.5 &&
    withExtension.screenWidth === 1777 && withExtension.screenHeight === 1111;
  console.log(JSON.stringify({ browser, hooksObserved, baseline, withExtension }, null, 2));
  if (!hooksObserved) {
    console.error('FAIL: --load-extension MAIN-world hooks were not observed on this runtime. Generated scripts alone are not proof of runtime support.');
    process.exitCode = 1;
  }
} catch (error) {
  console.error('Browser capability check failed: ' + error.message);
  process.exitCode = 2;
} finally {
  await new Promise(resolveClose => server.close(resolveClose));
  // Delete only this script's newly-created directory, never the target root.
  if (dirname(realpathSync(temporary)) !== targetReal) throw new Error('Refusing unexpected cleanup target');
  try { rmSync(temporary, { recursive: true, force: true }); }
  catch (error) { console.error('Temporary browser files retained at ' + temporary + ': ' + error.message); }
}
