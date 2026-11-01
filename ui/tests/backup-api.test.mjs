import assert from "node:assert/strict";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";

const { TauriApiClient } = await loadAdapter("tauri");
const { MockApiClient } = await loadAdapter("mock");
const plain = (value) => JSON.parse(JSON.stringify(value));

test("backup history preserves durable interrupted outcomes and cleanup notes", async () => {
  const jobs = [{ id: "restore-7", profileId: "new-profile", profileName: "恢复任务 #7", status: "interrupted",
    createdAt: "1000", note: "上次恢复被中断；暂存清理待重试" }];
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "backup_list"); assert.equal(args, undefined); return jobs;
  } });
  assert.deepEqual(plain(await api.listBackups()), jobs);
});

test("cleanup retry uses a fixed command without accepting filesystem paths", async () => {
  for (const cleanupPending of [true, false]) {
    const api = new TauriApiClient({ invoke: async (handler, args) => {
      assert.equal(handler, "backup_cleanup"); assert.equal(args, undefined); return { cleanupPending };
    } });
    assert.deepEqual(plain(await api.cleanupBackupStaging()), { cleanupPending });
  }
});

test("cleanup errors are propagated and browser preview does not claim restore support", async () => {
  const api = new TauriApiClient({ invoke: async () => { throw { code: "INTERNAL", message: "journal unavailable" }; } });
  await assert.rejects(api.cleanupBackupStaging(), (error) => error.code === "INTERNAL");
  const mock = new MockApiClient();
  assert.deepEqual(plain(await mock.cleanupBackupStaging()), { cleanupPending: false });
  await assert.rejects(mock.importBackup("test.browsbak", "test-only-password"), (error) => error.code === "CAPABILITY_UNSUPPORTED");
});
