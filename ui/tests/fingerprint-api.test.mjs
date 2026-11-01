import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import test from "node:test";
import vm from "node:vm";

// Test the real adapters without a webview, third-party runner, or emitted files.
async function loadAdapter(name) {
  const context = vm.createContext({ console });
  const types = new vm.SourceTextModule(stripTypeScriptTypes(await readFile(new URL("../src/types.ts", import.meta.url), "utf8"), { mode: "transform" }), { context });
  const tauri = new vm.SyntheticModule(["invoke"], function () {
    this.setExport("invoke", () => { throw new Error("Unexpected real Tauri invocation"); });
  }, { context });
  const adapter = new vm.SourceTextModule(stripTypeScriptTypes(await readFile(new URL(`../src/api/${name}.ts`, import.meta.url), "utf8"), { mode: "transform" }), { context });
  await adapter.link((specifier) => {
    if (specifier === "../types") return types;
    if (specifier === "@tauri-apps/api/core") return tauri;
    throw new Error(`Unexpected import: ${specifier}`);
  });
  await adapter.evaluate();
  return adapter.namespace;
}

const { TauriApiClient } = await loadAdapter("tauri");
const { MockApiClient } = await loadAdapter("mock");
const plain = (value) => JSON.parse(JSON.stringify(value));
const seedable = {
  kind: "seedable_noise",
  seedable: {
    seed: "stable-identity", platform: "windows", hardwareConcurrency: 4,
    deviceMemory: "0.25", touchPoints: 0, screenWidth: 1920, screenHeight: 1080,
    colorDepth: 24, deviceScaleFactor: 1.25, webglVendor: "", webglRenderer: "",
    spoofCanvas: true, spoofAudio: false,
  },
};
const input = {
  name: "Fingerprint test", tags: [], presetId: "system-native", runtimeId: "selected-runtime",
  persona: seedable, acceptLanguages: ["fr-FR", "fr"], windowWidth: 1024, windowHeight: 768,
};

function createBridge() {
  const calls = [];
  return {
    calls,
    invoke: async (handler, args) => {
      calls.push({ handler, args });
      if (handler === "profile_create") return { ...plain(args.request.profile), id: "profile-test", revision: 1, archived: false };
      if (handler === "profile_update") return { ...plain(args.request.profile), id: "profile-test", revision: args.request.expectedRevision + 1, archived: false };
      if (handler === "fingerprint_preview") return { marker: "preview-response" };
      throw new Error(`Unexpected handler: ${handler}`);
    },
  };
}

test("Tauri create round-trips real languages, window and persona", async () => {
  const bridge = createBridge();
  const api = new TauriApiClient(bridge);
  const profile = await api.createProfile(input);
  assert.deepEqual(plain(profile.acceptLanguages), input.acceptLanguages);
  assert.equal(profile.windowWidth, 1024);
  assert.equal(profile.windowHeight, 768);
  assert.deepEqual(plain(profile.persona), seedable);
  const draft = bridge.calls[0].args.request.profile;
  assert.equal(draft.persona.seedable_noise.screen.width, 1920);
  assert.equal(draft.window.width, 1024, "window must not be substituted with declared screen size");
  assert.equal(draft.browser.value.buildId, "selected-runtime");
});

test("Tauri updates preserve omitted configuration and can switch back to system native", async () => {
  const bridge = createBridge();
  const api = new TauriApiClient(bridge);
  const created = await api.createProfile(input);
  const renamed = await api.updateProfile(created.id, created.revision, { name: "Renamed" });
  assert.deepEqual(plain(renamed.persona), seedable);
  assert.deepEqual(plain(renamed.acceptLanguages), ["fr-FR", "fr"]);
  assert.equal(renamed.windowHeight, 768);
  const updated = await api.updateProfile(renamed.id, renamed.revision, {
    persona: { kind: "system_native" }, acceptLanguages: ["zh-CN", "zh"], windowWidth: 1200,
  });
  assert.deepEqual(plain(updated.persona), { kind: "system_native" });
  assert.deepEqual(plain(updated.acceptLanguages), ["zh-CN", "zh"]);
  assert.equal(updated.windowWidth, 1200);
  assert.equal(updated.windowHeight, 768);
});

test("Tauri preview sends the selected runtime and actual locale/window", async () => {
  const bridge = createBridge();
  const api = new TauriApiClient(bridge);
  await api.fingerprintPreview({ persona: seedable, runtimeId: input.runtimeId, acceptLanguages: input.acceptLanguages, windowWidth: 1024, windowHeight: 768 });
  const request = bridge.calls[0].args.request;
  assert.equal(request.runtimeId, "selected-runtime");
  assert.deepEqual(plain(request.acceptLanguages), ["fr-FR", "fr"]);
  assert.equal(request.windowWidth, 1024);
  assert.equal(request.windowHeight, 768);
  assert.equal(request.persona.seedable_noise.screen.width, 1920);
  assert.equal(request.persona.seedable_noise.screen.deviceScaleFactorHundredths, 125);
});

test("Mock persists and clones configuration while labeling preview as simulation", async () => {
  const api = new MockApiClient();
  const created = await api.createProfile({ ...input, runtimeId: "chrome-testing-stable" });
  const cloned = await api.cloneProfile(created.id);
  assert.deepEqual(plain(cloned.persona), seedable);
  assert.deepEqual(plain(cloned.acceptLanguages), ["fr-FR", "fr"]);
  assert.equal(cloned.windowWidth, 1024);
  const preview = await api.fingerprintPreview({ persona: seedable, runtimeId: created.runtimeId, acceptLanguages: input.acceptLanguages, windowWidth: 1024, windowHeight: 768 });
  assert.equal(preview.runtimeVersion, "127.0.6533.89");
  assert.equal(preview.effectiveScreenWidth, 1920);
  assert.equal(preview.effectiveWindowWidth, 1024);
  assert.equal(preview.deviceScaleFactorHundredths, 125);
  assert.match(preview.effectiveUserAgent, /模拟/);
  assert.match(preview.runtimeWarnings.join(" "), /未启动浏览器/);
  await assert.rejects(api.fingerprintPreview({ persona: seedable, runtimeId: "missing", acceptLanguages: ["fr"], windowWidth: 1024, windowHeight: 768 }));
});

test("Tauri runtime listing maps fingerprint support separately from trust", async () => {
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "list_runtimes");
    assert.equal(args, undefined);
    return [
      { id: "verified", name: "Browser", version: "150", channel: "stable", trusted: true, active: true, fingerprintSupport: "unsupported" },
      { id: "legacy", name: "Browser", version: "149", channel: "stable", trusted: true, active: false },
    ];
  } });
  const runtimes = await api.listRuntimes();
  assert.equal(runtimes[0].trusted, true);
  assert.equal(runtimes[0].fingerprintSupport, "unsupported");
  assert.equal(runtimes[1].fingerprintSupport, "unverified");
});

test("Tauri runtime probe forwards only the selected runtime id and returns evidence", async () => {
  const result = {
    runtime: { id: "selected-runtime", name: "Browser", version: "150", channel: "stable", trusted: true, active: true, fingerprintSupport: "supported" },
    detail: "Local extension injection observed; not full anti-detect",
  };
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "probe_runtime_fingerprint");
    assert.deepEqual(plain(args), { request: { runtimeId: "selected-runtime" } });
    return result;
  } });
  assert.deepEqual(plain(await api.probeRuntimeFingerprint("selected-runtime")), result);
});

test("Mock runtimes remain unverified and cannot claim a real probe", async () => {
  const api = new MockApiClient();
  assert.equal((await api.listRuntimes())[0].fingerprintSupport, "unverified");
  await assert.rejects(api.probeRuntimeFingerprint("chrome-testing-stable"), (error) => error.code === "CAPABILITY_UNSUPPORTED");
});
