import assert from "node:assert/strict";
import test from "node:test";
import { loadAdapter } from "./load-adapter.mjs";
const { TauriApiClient } = await loadAdapter("tauri");
const { MockApiClient } = await loadAdapter("mock");
const plain = (value) => JSON.parse(JSON.stringify(value));

test("official runtime installation uses a fixed command without executable paths or download URLs", async () => {
  const result = { runtime: { id: "cft-current", fingerprintSupport: "unsupported" }, detail: "installed, probe not passed" };
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "runtime_install_official"); assert.equal(args, undefined); return result;
  } });
  assert.deepEqual(plain(await api.installOfficialRuntime()), result);
});

test("installation status preserves measured bytes, commit boundaries and pending cancellation", async () => {
  for (const phase of ["downloading", "verifying", "committing", "probing", "cleaning", "unsupported", "ready"]) {
    const status = { available: true, running: !["unsupported", "ready"].includes(phase), phase, downloadedBytes: 12345, totalBytes: 67890, canCancel: phase === "downloading", cancelRequested: phase === "downloading", detail: "observed status" };
    const api = new TauriApiClient({ invoke: async (handler, args) => {
      assert.equal(handler, "runtime_install_status"); assert.equal(args, undefined); return status;
    } });
    assert.deepEqual(plain(await api.runtimeInstallStatus()), status);
  }
});

test("cancellation uses no filesystem or network arguments and propagates commit refusal", async () => {
  const api = new TauriApiClient({ invoke: async (handler, args) => {
    assert.equal(handler, "runtime_install_cancel"); assert.equal(args, undefined);
  } });
  await api.cancelRuntimeInstall();
  const committing = new TauriApiClient({ invoke: async () => { throw { code: "VALIDATION_FAILED", message: "cannot cancel after commit" }; } });
  await assert.rejects(committing.cancelRuntimeInstall(), (error) => error.code === "VALIDATION_FAILED");
});

test("download and signature errors propagate without claiming a usable runtime", async () => {
  for (const code of ["RUNTIME_UNTRUSTED", "OPERATION_CANCELLED", "INTERNAL"]) {
    const api = new TauriApiClient({ invoke: async () => { throw { code, message: "fixed installation error" }; } });
    await assert.rejects(api.installOfficialRuntime(), (error) => error.code === code);
  }
});

test("browser preview does not claim real installation or probe support", async () => {
  const api = new MockApiClient();
  const status = await api.runtimeInstallStatus();
  assert.equal(status.available, false); assert.equal(status.running, false);
  await assert.rejects(api.installOfficialRuntime(), (error) => error.code === "CAPABILITY_UNSUPPORTED");
  await assert.rejects(api.cancelRuntimeInstall(), (error) => error.code === "NOT_FOUND");
});
