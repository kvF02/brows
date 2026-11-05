import assert from "node:assert/strict";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";

const { TauriApiClient } = await loadAdapter("tauri");
const plain = value => JSON.parse(JSON.stringify(value));

test("settings read the backend and save a typed partial update", async () => {
  let saved = { maxConcurrentSessions: 3, retainArchivedDays: 7, telemetryEnabled: false, stopSessionsOnExit: true };
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    if (handler === "settings_get") {
      assert.equal(args, undefined);
      return { ...saved };
    }
    assert.equal(handler, "settings_update");
    assert.deepEqual(plain(args), { request: { changes: { maxConcurrentSessions: 2 } } });
    saved = { ...saved, ...args.request.changes };
    return { ...saved };
  } });
  assert.equal((await api.getSettings()).maxConcurrentSessions, 3);
  assert.equal((await api.updateSettings({ maxConcurrentSessions: 2 })).maxConcurrentSessions, 2);
  assert.equal((await api.getSettings()).maxConcurrentSessions, 2);
});

test("settings reject backend failure instead of claiming success", async () => {
  const api = new TauriApiClient({ invoke: async () => {
    throw { code: "INVALID_PROFILE_STATE", message: "stop all sessions first" };
  } });
  await assert.rejects(api.updateSettings({ maxConcurrentSessions: 2 }), error => error.code === "INVALID_PROFILE_STATE");
});
