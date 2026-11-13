# Native Worker platform lifecycle gate

This complements the pinned Chromium candidate in PR #36 and tracks the lifecycle
requirements in #35. It launches only owned browser processes with temporary
profiles and an owned loopback fixture over a private CDP pipe. It does not load
Brows hooks, replace getters, disable the sandbox, or visit external websites.

The gate checks classic and module Dedicated, Shared and Service Workers:

| Phase | Required evidence |
|---|---|
| Initial load | Page plus six Worker first scripts, raw getter values, native function source and native receiver rejection |
| Clear and restore | All six live Worker getters keep their identity while the native override is cleared and restored |
| Page reload | New Dedicated Worker first scripts and retained or recreated Shared Worker native getters |
| Stop and wake | Both Service Workers are observed stopped, then execute fresh first scripts after an explicit wake |
| Update | Both Service Worker script revisions change, new first scripts execute, and new versions become active |
| Process reopen | The owned browser exits cleanly; its profile is reopened in a new process with both active registrations persisted; all six Worker first scripts are checked again |
| Fresh profile control | A third process uses a new profile without a platform override; native page and Worker getters must return the host platform and registrations must start empty |

Each first script records a unique context marker internally. A repeated marker
is rejected; markers, temporary URLs and process IDs are not included in the
report. Phase failures retain partial observations and do not count as success.
The test uses bounded waits and runs each phase once, without retrying failures.

```powershell
node tools/native-worker-lifecycle.mjs --browser C:\path\headless_shell.exe --sha256 <sha256> --expect native --report target/native-worker-lifecycle.json
```

`--expect stock` is the negative control. It requires the overridden page to
return the configured platform while all stock Worker native getters still
return the host. The signed installed Edge in CI exercises this control.
A passing stock report always has `nativeLifecyclePassed: false`.

Only `--expect native` against an actual patched build can establish the native
lifecycle behavior. The executable digest pins this isolated test input; it does
not admit a custom runtime into the application. Both modes keep
`runtimeAdmitted: false` and `complete: false`. A passing stock control, a source
check or an unfinished kernel build must not close #35 or certify independent
fingerprinting. Production signing, trusted distribution and other native
fingerprint dimensions remain separate work.
