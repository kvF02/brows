// CI-only WebView2 acceptance through ordinary W3C WebDriver interactions.
// No app IPC calls, database writes or arbitrary browser-command forwarding.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";

const [appArg, driverArg, reportArg] = process.argv.slice(2);
assert.equal(process.platform, "win32", "Windows CI fixture only");
assert.equal(process.env.GITHUB_ACTIONS, "true", "Do not run against user data");
assert.equal(process.env.RUNNER_ENVIRONMENT, "github-hosted", "Hosted Windows fixture only");
assert.ok(appArg && driverArg && reportArg, "Expected application, driver and report paths");
const app = path.resolve(appArg);
const installRoot = path.resolve(process.env.GITHUB_WORKSPACE, "target", "installed");
assert.equal(path.dirname(app).toLowerCase(), installRoot.toLowerCase());
assert.equal(path.basename(app).toLowerCase(), "brows-desktop.exe");
const reportPath = path.resolve(reportArg);
await mkdir(path.dirname(reportPath), { recursive: true });
const server = net.createServer();
server.listen(0, "127.0.0.1");
await once(server, "listening");
const port = server.address().port;
await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
const driver = spawn(path.resolve(driverArg), ["--port=" + port, "--allowed-ips=127.0.0.1"], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
let driverError;
let driverLog = "";
for (const stream of [driver.stdout, driver.stderr]) stream.on("data", chunk => { driverLog = (driverLog + chunk.toString()).slice(-8192); });
driver.on("error", error => { driverError = error; });
const origin = "http://127.0.0.1:" + port;
const elementKey = "element-6066-11e4-a52e-4f735466cecf";
const checks = [];
let session;
let failure;
let proxy;
let crashRecovery;
let stage = "driver-start";
async function request(method, route, body, timeout = 20000) {
  const response = await fetch(origin + route, {
    method, headers: { "Content-Type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(timeout),
  });
  const data = await response.json();
  if (!response.ok || data.value?.error) {
    // Error messages may echo a typed password. Report only protocol error type.
    throw new Error("WebDriver " + route.split("/").at(-1) + ": " + (data.value?.error ?? response.status) + (route === "/session" ? " " + String(data.value?.message ?? "").slice(0, 2000) : ""));
  }
  return data.value;
}
const command = (method, route, body) => request(method, "/session/" + session + route, body);
async function waitFor(label, read, predicate = Boolean, budget = 20000) {
  const deadline = Date.now() + budget;
  let last;
  while (Date.now() < deadline) {
    try { const value = await read(); if (predicate(value)) return value; }
    catch (error) { last = error; }
    if (driverError) throw driverError;
    if (driver.exitCode !== null) throw new Error("WebDriver exited");
    await delay(200);
  }
  throw new Error("Timed out: " + label + (last ? " (" + last.message + ")" : ""));
}
async function find(using, value) {
  const element = await command("POST", "/element", { using, value });
  return element[elementKey];
}
const css = selector => find("css selector", selector);
const xpath = expression => find("xpath", expression);
const button = text => xpath("//button[normalize-space(.)='" + text + "']");
async function click(id) { await command("POST", "/element/" + id + "/click", {}); }
async function fill(selector, value) {
  const id = await waitFor(selector, () => css(selector));
  await command("POST", "/element/" + id + "/clear", {});
  await command("POST", "/element/" + id + "/value", { text: value });
}
async function nav(label) {
  await click(await waitFor("navigation " + label, () => xpath("//button[contains(@class,'nav-item')][.//strong[normalize-space(.)='" + label + "']]")));
}
const text = selector => css(selector).then(id => command("GET", "/element/" + id + "/text"));
const value = selector => css(selector).then(id => command("GET", "/element/" + id + "/property/value"));
async function namedCard(name) {
  return xpath(profileCard(name));
}
const profileCard = name => "//article[contains(@class,'profile-card')][.//h2[@title='" + name + "']]";
const cardText = async (name, part) => command("GET", "/element/" + await xpath(profileCard(name) + part) + "/text");
async function cardAction(name, action) {
  const id = await xpath(profileCard(name) + "//button[normalize-space(.)='" + action + "']");
  await command("POST", "/execute/sync", {
    script: "arguments[0].scrollIntoView({block:'center',inline:'center'});",
    args: [{ [elementKey]: id }],
  });
  await click(id);
}
async function notice(fragment) {
  await waitFor("notice " + fragment, () => text("[role='status']"), result => result.includes(fragment), 45000);
}
async function openApp() {
  const created = await request("POST", "/session", { capabilities: { alwaysMatch: {
    browserName: "webview2",
    "ms:edgeOptions": { binary: app },
  } } }, 90000);
  session = created.sessionId;
  assert.ok(session, "No WebDriver session");
  await command("POST", "/timeouts", { implicit: 0, script: 5000, pageLoad: 30000 });
  await waitFor("packaged app ready", () => css(".app-shell"));
  const address = await command("GET", "/url");
  assert.match(address, /^(?:https?:\/\/tauri\.localhost|tauri:\/\/localhost)(?:[/:?#]|$)/);
  assert.equal(await command("POST", "/execute/sync", { script: "return window.__BROWS_IS_TAURI__ === true;", args: [] }), true);
}

const fixtureProxyName = "CI authenticated proxy";
const fixtureProxyUser = "brows-fixture";
const fixtureProxyPassword = "fixture-only-proxy-password";
const fixtureExitIp = "203.0.113.9";
const proxyProof = { authenticatedConnects: 0, authenticatedExitRequests: 0, rejectedConnects: 0 };
let rejectConnect = false;
function createFixtureProxy() {
  const expected = "Basic " + Buffer.from(fixtureProxyUser + ":" + fixtureProxyPassword).toString("base64");
  const server = createServer((req, res) => {
    if (req.headers["proxy-authorization"] === expected && req.method === "GET" && req.url === "http://checkip.amazonaws.com/") {
      proxyProof.authenticatedExitRequests++;
      res.writeHead(200, { "Content-Type": "text/plain", Connection: "close" }).end(fixtureExitIp + "\n");
    } else {
      res.writeHead(407, { Connection: "close" }).end();
    }
  });
  server.on("connect", (req, socket) => {
    if (req.url === "www.google.com:443" && req.headers["proxy-authorization"] === expected) {
      if (!rejectConnect) {
        proxyProof.authenticatedConnects++;
        socket.end("HTTP/1.1 200 Connection Established\r\n\r\n");
        return;
      }
      proxyProof.rejectedConnects++;
    }
    socket.end("HTTP/1.1 407 Proxy Authentication Required\r\nConnection: close\r\n\r\n");
  });
  return server;
}
async function testProxy() {
  const test = "//tr[.//strong[normalize-space(.)='" + fixtureProxyName + "']]//button[normalize-space(.)='测试']";
  await click(await waitFor("fixture proxy test button", () => xpath(test)));
  await click(await button("确认并测试"));
}
async function waitCardStatus(name, status) {
  await waitFor(name + " " + status, () => cardText(name, "//*[contains(@class,'status-pill')]"), result => result === status, 45000);
}

try {
  await waitFor("WebDriver ready", () => request("GET", "/status", undefined, 2000));
  stage = "packaged-launch";
  await openApp();
  checks.push("embedded-ui-and-real-tauri-adapter");
  stage = "settings";
  await nav("设置");
  await fill("#max-concurrent", "2");
  await click(await button("保存并发设置"));
  await notice("设置已保存");
  await command("POST", "/refresh", {});
  await nav("设置");
  await waitFor("persisted concurrency", () => value("#max-concurrent"), result => result === "2");
  checks.push("concurrency-edit-save-and-ui-reload");
  stage = "profile-create";
  await nav("环境");
  await click(await button("新建环境"));
  await fill("#profile-name", "UI acceptance profile");
  await fill("#profile-tags", "ci, beta");
  await click(await button("创建环境"));
  await waitFor("created profile", () => namedCard("UI acceptance profile"));
  stage = "profile-edit";
  await cardAction("UI acceptance profile", "编辑");
  await fill("#profile-name", "UI acceptance renamed");
  await click(await button("保存修改"));
  await waitFor("edited profile", () => namedCard("UI acceptance renamed"));
  stage = "profile-archive";
  await cardAction("UI acceptance renamed", "归档");
  await click(await css(".archive-filter input"));
  const restore = await waitFor("archived card", () => xpath("//article[contains(@class,'profile-card')][.//h2[@title='UI acceptance renamed']]//button[normalize-space(.)='恢复']"));
  await click(restore);
  await waitFor("restored card", () => text(".profile-card .status-pill"), result => result.includes("已停止"));
  checks.push("create-edit-archive-and-restore");
  stage = "browser-launch-and-stop";
  await cardAction("UI acceptance renamed", "启动");
  await waitFor("running browser", () => text(".profile-card .status-pill"), result => result.includes("运行中"), 45000);
  await cardAction("UI acceptance renamed", "停止");
  await waitFor("stopped browser", () => text(".profile-card .status-pill"), result => result.includes("已停止"), 45000);
  checks.push("system-native-browser-launch-and-stop");
  stage = "encrypted-backup";
  await nav("备份");
  await fill("#backup-name", "ui-acceptance.browsbak");
  const fixturePassword = "fixture-only-backup-phrase-2026";
  await fill("#backup-password", fixturePassword);
  await fill("#backup-password-confirm", fixturePassword);
  await click(await button("导出加密备份"));
  await notice("加密备份已创建");
  await fill("#restore-name", "ui-acceptance.browsbak");
  await fill("#restore-password", fixturePassword);
  await click(await button("恢复为新环境"));
  await notice("已恢复环境");
  await nav("环境");
  await waitFor("independent restored profile", () => command("POST", "/elements", {
    using: "xpath", value: "//article[contains(@class,'profile-card')][.//h2[@title='UI acceptance renamed']]",
  }), elements => elements.length === 2);
  const cards = await command("POST", "/elements", { using: "css selector", value: ".profile-id" });
  const ids = await Promise.all(cards.map(element => command("GET", "/element/" + element[elementKey] + "/text")));
  assert.equal(new Set(ids).size, 2, "Restored profile must use a new ID");
  await command("POST", "/refresh", {});
  await waitFor("persisted profiles", () => command("POST", "/elements", {
    using: "xpath", value: "//article[contains(@class,'profile-card')][.//h2[@title='UI acceptance renamed']]",
  }), elements => elements.length === 2);
  checks.push("encrypted-export-and-restore-with-new-profile-id", "profile-metadata-survives-ui-reload");
  stage = "local-authenticated-proxy";
  proxy = createFixtureProxy();
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  await nav("代理");
  await click(await button("＋ 添加代理"));
  await fill("#proxy-name", fixtureProxyName);
  await fill("#proxy-host", "127.0.0.1");
  await fill("#proxy-port", String(proxy.address().port));
  await fill("#proxy-user", fixtureProxyUser);
  await fill("#proxy-password", fixtureProxyPassword);
  await click(await button("保存代理"));
  await notice("代理已保存");
  await testProxy();
  await waitFor("authenticated fixture exit", () => text(".data-table tbody tr"), result => result.includes(fixtureExitIp));
  assert.equal(proxyProof.authenticatedConnects, 1);
  assert.equal(proxyProof.authenticatedExitRequests, 1);
  rejectConnect = true;
  await testProxy();
  await waitFor("proxy rejection surfaced", () => text(".data-table tbody tr"), result => result.includes("认证或出口失败"));
  assert.equal(proxyProof.rejectedConnects, 1);
  assert.equal(proxyProof.authenticatedExitRequests, 1, "rejected CONNECT must not run an exit request");
  rejectConnect = false;
  checks.push("local-proxy-authentication-and-rejection-through-ui");

  stage = "installed-ui-ten-open";
  await nav("设置");
  await fill("#max-concurrent", "10");
  await click(await button("保存并发设置"));
  await notice("设置已保存");
  await nav("环境");
  const names = Array.from({ length: 11 }, (_, i) => "Installed UI recovery " + (i + 1));
  const profileIds = [];
  for (const [index, name] of names.entries()) {
    stage = "installed-ui-create-" + (index + 1);
    await click(await waitFor("new environment button", () => xpath("//button[contains(normalize-space(.),'新建环境')]")));
    await fill("#profile-name", name);
    await click(await xpath("//select[@id='profile-runtime']/option[starts-with(normalize-space(.),'Microsoft Edge')][not(@disabled)]"));
    if (index === 0) {
      await click(await xpath("//select[@id='profile-proxy']/option[starts-with(normalize-space(.),'" + fixtureProxyName + "')]"));
    }
    await click(await button("创建环境"));
    await waitFor("created " + name, () => namedCard(name));
    profileIds.push(await cardText(name, "//*[contains(@class,'profile-id')]"));
  }
  assert.equal(new Set(profileIds).size, names.length);
  for (const [index, name] of names.entries()) {
    stage = "installed-ui-start-" + (index + 1);
    await cardAction(name, "启动");
    await waitCardStatus(name, index < 10 ? "运行中" : "排队中");
  }
  checks.push("installed-ui-ten-running-and-one-queued");

  stage = "installed-ui-host-crash";
  let stdout;
  try {
    ({ stdout } = await promisify(execFile)("pwsh.exe", [
      "-NoLogo", "-NoProfile", "-NonInteractive", "-File",
      path.join(import.meta.dirname, "windows-installed-ui-crash.ps1"), "-Application", app,
    ], { timeout: 55000, windowsHide: true }));
  } catch (error) {
    throw new Error("Installed app process audit: " + String(error.stderr || error.code).trim().slice(-220));
  }
  crashRecovery = JSON.parse(stdout);
  assert.equal(crashRecovery.browserRoots, 10);
  assert.ok(crashRecovery.observedProcessesReclaimed > 10);
  checks.push("installed-host-crash-reclaims-observed-browser-trees");
  try { await request("DELETE", "/session/" + session, undefined, 10000); } catch {}
  session = undefined;

  stage = "installed-ui-reopen";
  await openApp();
  await nav("环境");
  for (const [index, name] of names.entries()) {
    await waitCardStatus(name, "已停止");
    assert.equal(await cardText(name, "//*[contains(@class,'profile-id')]"), profileIds[index]);
  }
  await cardAction(names[0], "启动");
  await waitCardStatus(names[0], "运行中");
  await cardAction(names[0], "停止");
  await waitCardStatus(names[0], "已停止");
  await nav("代理");
  const beforeRetest = proxyProof.authenticatedExitRequests;
  await testProxy();
  await waitFor("authenticated fixture exit after reopening", () => text(".data-table tbody tr"), result => result.includes(fixtureExitIp));
  assert.ok(proxyProof.authenticatedExitRequests > beforeRetest, "reopen must perform a new authenticated request");
  checks.push("eleven-profiles-survive-reopen-and-proxy-credential-reused");
  console.log("Installed app crash reclaimed " + crashRecovery.observedProcessesReclaimed + " observed Edge processes across ten roots");
  stage = "screenshot";
  const screenshot = await command("GET", "/screenshot");
  await writeFile(reportPath.replace(/\.json$/, ".png"), Buffer.from(screenshot, "base64"));
} catch (error) {
  failure = { stage, message: error.message.slice(0, 250) };
  if (session) {
    try { await writeFile(reportPath.replace(/\.json$/, ".png"), Buffer.from(await command("GET", "/screenshot"), "base64")); } catch {}
  }
  if (stage === "packaged-launch") console.log("WebDriver startup diagnostics: " + driverLog);
} finally {
  if (session) {
    try { await request("DELETE", "/session/" + session, undefined, 10000); } catch {}
  }
  proxy?.close();
  driver.kill();
  await writeFile(reportPath, JSON.stringify({
    schemaVersion: 1, assertionsPassed: !failure, complete: false, checks,
    commit: process.env.GITHUB_SHA, windowsRelease: os.release(), webView2Version: process.env.BROWS_UI_WEBVIEW_VERSION,
    crashRecovery, proxyProof,
    notChecked: ["Windows 11 client", "public proxy exit, DNS and ICE", "SeedableNoise UI", "hours-long load", "power-loss recovery", "real site login restoration"],
    ...(failure ? { failure } : {}),
  }, null, 2) + "\n");
}
if (failure) throw new Error(failure.stage + ": " + failure.message);
console.log("Packaged WebView2 UI acceptance passed: " + checks.join(", "));
