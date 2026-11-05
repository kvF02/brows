import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
test('desktop capability authorizes the runtime probe exposed by the UI', async () => {
  const capability=JSON.parse(await readFile(new URL('../crates/desktop/capabilities/main-window.json',import.meta.url)));
  const permissions=await readFile(new URL('../crates/desktop/permissions/default.toml',import.meta.url),'utf8');
  assert.ok(capability.permissions.includes('allow-probe-runtime-fingerprint'));
  assert.match(permissions,/identifier = "allow-probe-runtime-fingerprint"\s+description = [^\n]+\s+commands = \{ allow = \["probe_runtime_fingerprint"\] \}/);
  for (const command of ['session_observe', 'network_probe_service', 'session_probe_network', 'settings_get', 'settings_update', 'runtime_capture', 'runtime_activate', 'support_report']) {
    const identifier = 'allow-' + command.replaceAll('_', '-');
    assert.ok(capability.permissions.includes(identifier), identifier);
    assert.ok(permissions.includes(`commands = { allow = ["${command}"] }`), command);
  }
});
