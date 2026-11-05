import assert from "node:assert/strict";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";
const { TauriApiClient } = await loadAdapter("tauri");
const { MockApiClient } = await loadAdapter("mock");
const plain = value => JSON.parse(JSON.stringify(value));

test("runtime management forwards only an admitted build ID", async () => {
  const calls = [];
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    calls.push({ handler, args: plain(args) });
    return handler === "runtime_capture" ? { id: "managed-edge-156", managed: true, active: false, fingerprintSupport: "unverified" } : undefined;
  } });
  const captured = await api.captureRuntime("installed-edge");
  assert.equal(captured.managed, true);
  assert.equal(captured.fingerprintSupport, "unverified");
  await api.activateRuntime(captured.id);
  assert.deepEqual(calls, [
    { handler: "runtime_capture", args: { request: { runtimeId: "installed-edge" } } },
    { handler: "runtime_activate", args: { request: { runtimeId: "managed-edge-156" } } },
  ]);
});

test("runtime list preserves managed source, digest and actual default selection", async () => {
  const api = new TauriApiClient({ invoke: async () => [
    { id: "installed", name: "Edge", version: "156", channel: "stable", trusted: true, active: false },
    { id: "snapshot", name: "Edge", version: "156", channel: "stable", trusted: true, active: true, managed: true, sha256: "a".repeat(64) },
  ] });
  const runtimes = await api.listRuntimes();
  assert.equal(runtimes[0].active, false);
  assert.equal(runtimes[1].managed, true);
  assert.equal(runtimes[1].sha256, "a".repeat(64));
});

test("support export is a fixed read operation and mock cannot claim native work", async () => {
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "support_report"); assert.equal(args, undefined);
    return { formatVersion: 1, sessions: { running: 2 } };
  } });
  assert.equal((await api.getSupportReport()).sessions.running, 2);
  const mock = new MockApiClient();
  for (const action of [() => mock.captureRuntime("edge"), () => mock.activateRuntime("edge"), () => mock.getSupportReport()]) {
    await assert.rejects(action(), error => error.code === "CAPABILITY_UNSUPPORTED");
  }
});
