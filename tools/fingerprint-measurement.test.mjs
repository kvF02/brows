// Collector program contracts in a VM; this does not execute Chrome.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
const template = readFileSync(new URL("../crates/adapters-runtime/src/measurement/capture.js", import.meta.url), "utf8");
const config = { token: "0123456789abcdef0123456789abcdef", runId: "a0", kind: "worker", base: "/owned/a0", marker: "owned-a" };
async function observe(navigator = {}, extras = {}) {
  let resolve;
  const result = new Promise((done) => { resolve = done; });
  const context = vm.createContext({ navigator, TextEncoder, setTimeout, clearTimeout, postMessage: resolve, ...extras });
  vm.runInContext(template.replace("__MEASUREMENT_CONFIG__", JSON.stringify(config)), context);
  return JSON.parse(JSON.stringify(await result));
}

test("unavailable browser APIs remain null and are never filled with sample expectations", async () => {
  const { signal } = await observe();
  assert.equal(signal.memory, null); assert.equal(signal.memoryType, "undefined");
  assert.equal(signal.concurrency, null); assert.equal(signal.webglVendor, null);
  assert.equal(signal.canvas, null); assert.equal(signal.screenWidth, null);
  assert.equal(signal.clientHints, null); assert.deepEqual(signal.languages, []);
});

test("reads retain actual values and types even when they disagree with the test profile", async () => {
  const { signal } = await observe({ userAgent: "actual-native-UA", platform: "actual-platform", language: "en-US", languages: ["en-US"], hardwareConcurrency: 64, deviceMemory: "0.5" });
  assert.equal(signal.userAgent, "actual-native-UA"); assert.equal(signal.platform, "actual-platform");
  assert.equal(signal.concurrency, 64); assert.equal(signal.memory, null); assert.equal(signal.memoryType, "string");
});

test("navigator reads happen synchronously before awaiting high entropy Client Hints", async () => {
  let resolveHints, resolveResult;
  const highEntropy = new Promise((done) => { resolveHints = done; });
  const result = new Promise((done) => { resolveResult = done; });
  const nav = { userAgent: "first-script-UA", hardwareConcurrency: 3, deviceMemory: 0.5, userAgentData: { brands: [{ brand: "Actual", version: "156" }], platform: "Windows", mobile: false, getHighEntropyValues: () => highEntropy } };
  vm.runInContext(template.replace("__MEASUREMENT_CONFIG__", JSON.stringify(config)), vm.createContext({ navigator: nav, TextEncoder, setTimeout, clearTimeout, postMessage: resolveResult }));
  nav.userAgent = "late-UA"; nav.hardwareConcurrency = 99;
  resolveHints({ architecture: "x86", bitness: "64", uaFullVersion: "156.0.8078.80" });
  const { signal } = await result;
  assert.equal(signal.userAgent, "first-script-UA"); assert.equal(signal.concurrency, 3);
  assert.equal(signal.clientHints.architecture, "x86"); assert.equal(signal.clientHints.fullVersion, "156.0.8078.80");
});
