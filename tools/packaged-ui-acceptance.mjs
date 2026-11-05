// CI-only WebView2 acceptance through ordinary W3C WebDriver interactions.
// No app IPC calls, database writes or arbitrary browser-command forwarding.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const [appArg, driverArg, reportArg] = process.argv.slice(2);
assert.equal(process.platform, "win32", "Windows CI fixture only");
assert.equal(process.env.GITHUB_ACTIONS, "true", "Do not run against user data");
assert.ok(appArg && driverArg && reportArg, "Expected application, driver and report paths");
const app = path.resolve(appArg);
const releaseRoot = path.resolve(process.env.GITHUB_WORKSPACE, "target", "release");
assert.equal(path.dirname(app).toLowerCase(), releaseRoot.toLowerCase());
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
  return xpath("//article[contains(@class,'profile-card')][.//h2[@title='" + name + "']]");
}
async function cardAction(name, action) {
  const id = await xpath("//article[contains(@class,'profile-card')][.//h2[@title='" + name + "']]//button[normalize-space(.)='" + action + "']");
  await click(id);
}
async function notice(fragment) {
  await waitFor("notice " + fragment, () => text("[role='status']"), result => result.includes(fragment), 45000);
}
try {
  await waitFor("WebDriver ready", () => request("GET", "/status", undefined, 2000));
  stage = "packaged-launch";
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
  driver.kill();
  await writeFile(reportPath, JSON.stringify({
    schemaVersion: 1, assertionsPassed: !failure, complete: false, checks,
    commit: process.env.GITHUB_SHA, windowsRelease: os.release(), webView2Version: process.env.BROWS_UI_WEBVIEW_VERSION,
    notChecked: ["current windows-latest WebView2 driver compatibility", "proxy workflows and SeedableNoise UI", "hours-long load", "power-loss recovery", "cross-machine login portability"],
    ...(failure ? { failure } : {}),
  }, null, 2) + "\n");
}
if (failure) throw new Error(failure.stage + ": " + failure.message);
console.log("Packaged WebView2 UI acceptance passed: " + checks.join(", "));
