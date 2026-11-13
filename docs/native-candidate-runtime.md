# Native candidate through the real controller

This is functional qualification of the existing pinned experimental build.
It does not rebuild Chromium, register an unsigned production runtime, claim
release performance or certify full independent fingerprinting.

`.github/workflows/native-candidate-runtime.yml` binds input to build run
43348531237, source head `a20791f0b13e14c147f49315eb596378815fbdf7`, the expected
workflow ID and the same repository. It requires the immutable test-runtime and
build-evidence artifacts after that run completes. Missing or expired artifacts
produce an explicit failing/blocked qualification report, never a native pass.
Once the artifact exists, rerun this qualification job; the native build can
remain unchanged. A different source run/head requires reviewing and updating
both pins in this workflow.

Before browser execution, `tools/verify-native-candidate.py` checks every packaged
file and rejects extra files/links/corruption. It binds the binary digest to
observed successful compilation and staging, verifies both source manifests and
patch hashes against this checkout, and checks the packaged GN arguments against
the build evidence. Four contract tests cover false compilation/hash evidence,
forged source/patch metadata and argument mismatch. Input validation alone always
leaves native acceptance and runtime admission false.

Two explicitly ignored Windows tests then execute against that exact digest:

- `private_native_candidate_capability_acceptance` invokes the actual private
  capability probe. Top-page and separately controlled OOPIF first scripts must
  report original screen getters, receiver checks and matching CSS geometry.
- `private_native_candidate_controller_acceptance` uses the actual production
  Controller. A Linux persona on Windows forces cross-platform Worker platform
  support to be real. All classic/module Dedicated, Shared and Service Worker
  platform/CPU/language getters must be native; a script fallback fails. Two
  screen configurations cover landscape/portrait, 1.25x/2x DPR, CPU 3/7 and touch
  0/5. The test retains the existing screen/CSS/receiver/getter-identity,
  viewport/window-preservation, clear/restore and reload assertions across all
  nine first-script context kinds. Rendering checks remain behavioral checks of
  the declared script layer, not claims of native GPU isolation.

The candidate entry point exists only under `cfg(all(test, windows))`, requires
explicit candidate path/hash/version inputs, verifies the digest before each
launch and keeps `RuntimeTrust::Untrusted`. It calls the fixed internal probe and
controller directly and never registers a runtime. Existing signed-Edge tests
still require the Microsoft signature and use separate environment variables;
production publisher, signature and full-distribution integrity gates are not
changed.

After execution the entire payload is checked again. The final qualification
report becomes positive only when both Rust tests and the post-check pass with
matching executable digests. It still records `runtimeAdmitted: false` and
`complete: false`. Memory, color-depth mismatches and rendering retain their
stated script fallbacks; fonts, physical GPU/audio devices, TLS/HTTP2 and the
trusted update/signing/application integration remain separate requirements.

Windows compile/unit/Clippy and the actual signed-Edge OOPIF negative are separate
jobs, so candidate-only code is checked even while compilation is still running.
Their success does not substitute for a missing positive artifact.
