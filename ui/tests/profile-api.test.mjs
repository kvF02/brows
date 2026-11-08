import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { setImmediate } from "node:timers/promises";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";

const { TauriApiClient } = await loadAdapter("tauri");
const plain = (value) => JSON.parse(JSON.stringify(value));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function profile(overrides = {}) {
  return {
    id: "profile-test", revision: 1, archived: false, name: "Test", tags: [],
    presetId: "system-native", presetVersion: 1,
    browser: { mode: "pinned", value: { buildId: "runtime-test" } },
    persona: { system_native: { preserveSystemTimezone: true, preserveSystemFonts: true, preserveSystemGpu: true, preserveSystemAudio: true } },
    locale: { acceptLanguages: ["zh-CN", "zh"] },
    window: { width: 1280, height: 800, startMaximized: false },
    startupUrls: [],
    ...overrides,
  };
}

function session(state, overrides = {}) {
  return { id: "session-test", profileId: "profile-test", state, createdAtMs: 100, updatedAtMs: 200, ...overrides };
}

function readBridge(profiles, sessions) {
  return { invoke: async (handler, args) => {
    if (handler === "profile_list") return profiles;
    if (handler === "profile_get") return profiles.find((value) => value.id === args.request.profileId);
    if (handler === "session_list") return sessions;
    throw new Error(`Unexpected handler: ${handler}`);
  } };
}

test("failed sessions remain visible and terminal sessions retain last-used time", async () => {
  for (const state of ["failed", "stopped"]) {
    const api = new TauriApiClient(readBridge([profile()], [session(state)]));
    const listed = (await api.listProfiles())[0];
    const fetched = await api.getProfile("profile-test");
    assert.equal(listed.status, state);
    assert.equal(fetched.status, state);
    assert.equal(listed.lastUsedAt, new Date(200).toISOString());
    assert.equal(fetched.lastUsedAt, listed.lastUsedAt);
  }
});

test("current session wins over older history regardless of response order", async () => {
  const sessions = [
    session("failed", { id: "older", createdAtMs: 10, updatedAtMs: 900 }),
    session("running", { id: "current", createdAtMs: 20, updatedAtMs: 30 }),
    session("failed", { id: "unrelated", profileId: "other", createdAtMs: 1000 }),
  ];
  for (const ordered of [sessions, [...sessions].reverse()]) {
    const api = new TauriApiClient(readBridge([profile()], ordered));
    const value = (await api.listProfiles())[0];
    assert.equal(value.status, "running");
    assert.equal(value.lastUsedAt, new Date(30).toISOString());
  }
});

test("newest terminal session determines status without reviving an old failure", async () => {
  const api = new TauriApiClient(readBridge([profile()], [
    session("stopped", { id: "newer", createdAtMs: 20, updatedAtMs: 30 }),
    session("failed", { id: "older", createdAtMs: 10, updatedAtMs: 90 }),
  ]));
  const value = (await api.listProfiles())[0];
  assert.equal(value.status, "stopped");
  assert.equal(value.lastUsedAt, new Date(30).toISOString());
});

test("archived and never-launched profiles retain their own status", async () => {
  const api = new TauriApiClient(readBridge([
    profile({ archived: true }), profile({ id: "never-launched" }),
  ], [session("failed")]));
  const values = await api.listProfiles(true);
  assert.equal(values[0].status, "archived");
  assert.equal(values[0].lastUsedAt, new Date(200).toISOString());
  assert.equal(values[1].status, "stopped");
  assert.equal(values[1].lastUsedAt, undefined);
});

test("a late list response cannot roll back fields during a later partial update", async () => {
  let stored = profile();
  const oldSnapshot = plain(stored);
  const delayedList = deferred();
  const updates = [];
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    if (handler === "profile_list") return delayedList.promise;
    if (handler === "profile_get") return plain(stored);
    if (handler === "session_list") return [];
    if (handler === "profile_update") {
      assert.equal(args.request.expectedRevision, stored.revision);
      updates.push(plain(args.request));
      stored = { ...plain(args.request.profile), id: stored.id, revision: stored.revision + 1, archived: false };
      return plain(stored);
    }
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  await api.getProfile(stored.id);
  const pending = api.listProfiles();
  const resized = await api.updateProfile(stored.id, 1, { windowWidth: 1600 });
  delayedList.resolve([oldSnapshot]);
  await pending;
  await api.updateProfile(stored.id, resized.revision, { name: "Renamed" });
  assert.equal(updates[1].profile.window.width, 1600, "rename must preserve the previously saved window width");
  assert.equal(stored.window.width, 1600);
});

test("partial updates preserve fields from the requested revision even after external edits", async () => {
  let stored = profile();
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    if (handler === "profile_get") return plain(stored);
    if (handler === "session_list") return [];
    if (handler === "profile_update") {
      assert.equal(args.request.expectedRevision, 2);
      assert.deepEqual(plain(args.request.profile.locale.acceptLanguages), ["fr-FR"]);
      assert.deepEqual(plain(args.request.profile.startupUrls), ["https://example.com/"]);
      return { ...plain(args.request.profile), id: stored.id, revision: 3, archived: false };
    }
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  await api.getProfile(stored.id);
  stored = profile({ revision: 2, locale: { acceptLanguages: ["fr-FR"] }, startupUrls: ["https://example.com/"] });
  await api.updateProfile(stored.id, 2, { name: "Renamed" });
});

test("proxy clearing serializes absence instead of an invalid empty proxy ID", async () => {
  for (const changes of [{ proxyId: "" }, { proxyId: undefined }, { proxyId: "proxy-new" }, { name: "Renamed" }]) {
    const current = profile({ proxyId: "proxy-original", tags: ["preserved"] });
    let sent;
    const api = new TauriApiClient({ invoke: async (handler, args) => {
      if (handler === "profile_get") return plain(current);
      if (handler === "session_list") return [];
      if (handler === "profile_update") {
        sent = plain(args.request.profile); // The actual JSON IPC boundary.
        assert.notEqual(sent.proxyId, "", "Rust ProxyId rejects empty strings");
        return { ...sent, id: current.id, revision: 2, archived: false };
      }
      throw new Error(`Unexpected handler: ${handler}`);
    } });
    const updated = await api.updateProfile(current.id, 1, changes);
    const expected = Object.hasOwn(changes, "proxyId") ? changes.proxyId || undefined : current.proxyId;
    assert.equal(sent.proxyId, expected);
    assert.equal(updated.proxyId, expected);
    assert.deepEqual(sent.persona, plain(current.persona));
    assert.deepEqual(sent.tags, current.tags);
  }
});

test("stale edit revisions still fail optimistic concurrency instead of retrying silently", async () => {
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    if (handler === "profile_get") return profile({ revision: 2 });
    if (handler === "session_list") return [];
    if (handler === "profile_update") {
      assert.equal(args.request.expectedRevision, 1);
      throw { code: "REVISION_CONFLICT", message: "Profile changed" };
    }
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  await assert.rejects(api.updateProfile("profile-test", 1, { name: "Stale" }), (error) => error.code === "REVISION_CONFLICT");
});

test("stopping uses an active session and never targets terminal history", async () => {
  for (const state of ["queued", "starting", "running", "stopping"]) {
    const active = session(state, { id: "active" });
    const calls = [];
    const api = new TauriApiClient({ invoke: async (handler, args) => {
      calls.push({ handler, args });
      if (handler === "session_list") return [session("failed", { id: "old", createdAtMs: 50 }), active];
      if (handler === "profile_get") return profile();
      if (handler === "session_stop" || handler === "session_cancel_queued") {
        assert.equal(args.request.sessionId, "active");
        return { ...active, state: "stopped", updatedAtMs: 300 };
      }
      throw new Error(`Unexpected handler: ${handler}`);
    } });
    const stopped = await api.stopSession("profile-test");
    assert.equal(stopped.status, "stopped");
    assert.equal(stopped.lastUsedAt, new Date(300).toISOString());
    assert.ok(calls.some(({ handler }) => handler === (state === "queued" ? "session_cancel_queued" : "session_stop")));
  }
  const api = new TauriApiClient(readBridge([profile()], [session("failed")]));
  await assert.rejects(api.stopSession("profile-test"), (error) => error.code === "NOT_FOUND");
});

test("a profile page finishing after a stop cannot revive its earlier session snapshot", async () => {
  const delayedProfiles = deferred();
  let stored = session("running");
  const api = new TauriApiClient({ invoke: async (handler) => {
    if (handler === "profile_get") return profile();
    if (handler === "profile_list") return delayedProfiles.promise;
    if (handler === "session_list") return [plain(stored)];
    if (handler === "session_stop") return stored = session("stopped", { updatedAtMs: 300 });
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  await api.getProfile("profile-test");
  const pending = api.listProfiles();
  await setImmediate();
  assert.equal((await api.stopSession("profile-test")).status, "stopped");
  delayedProfiles.resolve([profile()]);
  assert.equal((await pending)[0].status, "stopped");
});

test("an older session-list response cannot replace a newer completed read", async () => {
  const delayedSessions = deferred();
  let reads = 0;
  const api = new TauriApiClient({ invoke: async (handler) => {
    if (handler === "profile_get") return profile();
    if (handler === "session_list") return reads++ === 0 ? delayedSessions.promise : [session("stopped")];
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  const older = api.getProfile("profile-test");
  assert.equal((await api.getProfile("profile-test")).status, "stopped");
  delayedSessions.resolve([session("running")]);
  assert.equal((await older).status, "stopped");
});

test("launch completion invalidates an earlier pending session-list read", async () => {
  const delayedSessions = deferred();
  const api = new TauriApiClient({ invoke: async (handler) => {
    if (handler === "profile_get") return profile();
    if (handler === "session_list") return delayedSessions.promise;
    if (handler === "session_launch") return session("running");
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  const pending = api.getProfile("profile-test");
  assert.equal((await api.launchSession("profile-test")).status, "running");
  delayedSessions.resolve([]);
  assert.equal((await pending).status, "running");
});

test("profile pagination preserves filters and includes session data for every page", async () => {
  const first = Array.from({ length: 200 }, (_, index) => profile({ id: `profile-${index}` }));
  const pages = [];
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    if (handler === "session_list") return [session("failed")];
    if (handler === "profile_list") {
      pages.push(plain(args.request));
      return args.request.page === 1 ? first : [profile()];
    }
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  const values = await api.listProfiles(true);
  assert.equal(values.length, 201);
  assert.equal(values[200].status, "failed");
  assert.deepEqual(pages, [1, 2].map((page) => ({ page, pageSize: 200, search: null, includeArchived: true })));
});

test("session-list errors reject immediately while profile pagination is still pending", async () => {
  const delayedList = deferred();
  const api = new TauriApiClient({ invoke: async (handler) => {
    if (handler === "profile_list") return delayedList.promise;
    if (handler === "session_list") throw { code: "INTERNAL", message: "Session list failed" };
    throw new Error(`Unexpected handler: ${handler}`);
  } });
  let error;
  const pending = api.listProfiles().catch((cause) => { error = cause; });
  try {
    await setImmediate();
    assert.equal(error?.message, "Session list failed");
  } finally {
    delayedList.resolve([]);
    await pending;
  }
});

test("parallel list failures never escape as unhandled promise rejections", () => {
  // Isolate strict process-level rejection handling from the node:test runner.
  const helper = new URL("./load-adapter.mjs", import.meta.url).href;
  for (const profileFails of [true, false]) {
    const child = spawnSync(process.execPath, ["--experimental-vm-modules", "--unhandled-rejections=strict", "--input-type=module", "-e", `
      import assert from 'node:assert/strict';
      import { loadAdapter } from ${JSON.stringify(helper)};
      const { TauriApiClient } = await loadAdapter('tauri');
      const api = new TauriApiClient({ invoke: async (handler) => {
        if (handler === 'session_list') throw { code: 'INTERNAL', message: 'Session failure' };
        await new Promise((resolve) => setTimeout(resolve, 20));
        if (${profileFails}) throw { code: 'INTERNAL', message: 'Profile failure' };
        return [];
      } });
      await assert.rejects(api.listProfiles());
      await new Promise((resolve) => setTimeout(resolve, 30));
    `], { encoding: "utf8", timeout: 5000 });
    assert.equal(child.status, 0, child.stderr);
  }
});
