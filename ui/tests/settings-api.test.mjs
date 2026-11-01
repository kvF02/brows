import assert from "node:assert/strict";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";

const { TauriApiClient } = await loadAdapter("tauri");
const { MockApiClient } = await loadAdapter("mock");
const plain = (value) => JSON.parse(JSON.stringify(value));

test("Tauri settings read and update use the real allow-listed commands", async () => {
  let settings = { maxConcurrentSessions: 2, retainArchivedDays: 7, stopSessionsOnExit: true, telemetryEnabled: false };
  const calls = [];
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    calls.push({ handler, args: plain(args ?? null) });
    if (handler === "settings_get") return { ...settings };
    assert.equal(handler, "settings_update");
    settings = { ...settings, ...plain(args.request.changes) };
    return { ...settings };
  } });
  assert.equal((await api.getSettings()).maxConcurrentSessions, 2);
  assert.equal((await api.updateSettings({ maxConcurrentSessions: 4 })).maxConcurrentSessions, 4);
  assert.equal((await api.getSettings()).maxConcurrentSessions, 4);
  assert.deepEqual(calls, [
    { handler: "settings_get", args: null },
    { handler: "settings_update", args: { request: { changes: { maxConcurrentSessions: 4 } } } },
    { handler: "settings_get", args: null },
  ]);
});

test("failed settings persistence is reported without a fabricated saved result", async () => {
  const api = new TauriApiClient({ invoke: async () => {
    throw { code: "INTERNAL", message: "settings database unavailable", violations: [] };
  } });
  await assert.rejects(api.updateSettings({ maxConcurrentSessions: 2 }), (error) => error.code === "INTERNAL");
  await assert.rejects(api.getSettings(), (error) => error.code === "INTERNAL");
});

test("mock rejects unsupported policies and invalid limits without mutating input", async () => {
  const api = new MockApiClient();
  for (const maxConcurrentSessions of [0, 11, 1.5, NaN, Infinity]) {
    await assert.rejects(api.updateSettings({ maxConcurrentSessions }), (error) => error.code === "VALIDATION_FAILED");
  }
  for (const changes of [{ stopSessionsOnExit: false }, { telemetryEnabled: true }, { retainArchivedDays: 1 }, { activeRuntimeId: "invented" }]) {
    await assert.rejects(api.updateSettings(changes), (error) => error.code === "CAPABILITY_UNSUPPORTED");
  }
  assert.equal((await api.getSettings()).maxConcurrentSessions, 10);
  const changes = Object.freeze({ maxConcurrentSessions: 3 });
  assert.equal((await api.updateSettings(changes)).maxConcurrentSessions, 3);
  assert.equal((await api.getSettings()).stopSessionsOnExit, true);
});

test("mock live resizing promotes FIFO and drains without stopping running profiles", async () => {
  const api = new MockApiClient();
  const profiles = await api.listProfiles();
  await api.updateSettings({ maxConcurrentSessions: 1 });
  for (const profile of profiles) await api.launchSession(profile.id);
  assert.deepEqual(plain((await api.listProfiles()).map((profile) => profile.status)), ["running", "queued", "queued"]);
  await api.updateSettings({ maxConcurrentSessions: 2 });
  assert.deepEqual(plain((await api.listProfiles()).map((profile) => profile.status)), ["running", "running", "queued"]);
  await api.updateSettings({ maxConcurrentSessions: 1 });
  await api.stopSession(profiles[0].id);
  assert.deepEqual(plain((await api.listProfiles()).map((profile) => profile.status)), ["stopped", "running", "queued"]);
  await api.stopSession(profiles[1].id);
  assert.deepEqual(plain((await api.listProfiles()).map((profile) => profile.status)), ["stopped", "stopped", "running"]);
});
