# Brows

Brows is a Windows-first, local profile isolation browser manager. The first
release focuses on reliable multi-session isolation: each profile has its own
Chromium user-data directory, cookies, storage, cache and optional proxy.

Brows supports a conservative `SeedableNoise` persona that applies typed
Chromium flags, preferences and an MV3 JavaScript hook. Its configuration
preview uses the selected runtime version, profile languages and window size;
it is not a measurement of a running browser. Supported page reads include
navigator/screen/WebGL declarations, stable Canvas read/export perturbation
and opt-in offline Audio output perturbation. On Windows, a private session pipe
applies UA metadata, hardware declarations, languages and native timezone policy
before attached page/Worker scripts resume. Both use the same rendering hook,
including Worker OffscreenCanvas reads/exports and WebGL vendor/renderer reads.
Diagnostics can read fixed device fields from an active session. Fonts, actual
GPU rendering, live audio, unavailable Worker audio APIs and the network stack
remain host/runtime signals. Extension loading must be verified for each runtime, especially
official Google Chrome 137+ with `--load-extension` restrictions. Settings offers
an isolated local capability probe, and SeedableNoise launch is blocked unless
it passes. An earlier system Chrome 154 did not load the extension in the
real-browser smoke test. This is not a complete anti-detect implementation;
no detection-site pass rate is promised. The default `SystemNative` persona
remains truthful. See the
current [fingerprint design and acceptance plan](docs/fingerprint-design.md).

Windows 签名运行时选择与实际覆盖验收见
[运行时验收流程](docs/runtime-acceptance.md)。新增的 Microsoft Edge 路径仍需
真实探针通过，跨上下文报告会保留未覆盖与不一致项。
可选公网 DNS / ICE 检查需要部署自己的检测服务并配置预期出口范围；见
[服务配置与证据边界](docs/network-probe-service.md)。未配置或缺少观测时保持未检查。

## Current Status

The repository contains the first implementation milestone:

- framework-independent profile, proxy, runtime and session domain models;
- a ten-session FIFO scheduler with duplicate-profile protection;
- versioned, allow-listed UI command and event contracts;
- application services and ports with in-memory test adapters;
- SQLite profile persistence, including `SeedableNoise` persona fields;
- append-only local SQLite audit persistence for Profile and Session actions;
- a Chromium runtime adapter that confines paths, verifies the Windows
  Authenticode signer against the runtime-specific publisher allowlist and rechecks SHA-256
  before every launch;
- a Windows Credential Manager `SecretStore` adapter with redacted errors,
  idempotent cleanup and a non-Windows unavailable stub;
- a React management UI with a real, allow-listed Tauri client and a mock used
  only by browser development;
- a read-only MoreLogin XLSX preview and metadata-only import path that never
  returns password, 2FA, Cookie, proxy-secret or custom-UA values to the UI;
- a Tauri 2 composition root covering the Profile and Session vertical slice;
- the complete architecture and security plan in
  [`docs/architecture-plan.md`](docs/architecture-plan.md).

The evidence-backed MoreLogin capability comparison and implementation order
are documented in [`docs/morelogin-gap-analysis.md`](docs/morelogin-gap-analysis.md).

The phase-one Windows desktop path now uses SQLite persistence and launches the
locally installed Google Chrome or Microsoft Edge runtime. At startup it validates the local
Authenticode trust decision without network retrieval, requires the leaf
certificate publisher to be `Google LLC` (Chrome) or `Microsoft Corporation` (Edge), records the current file digest and
checks that digest again before each launch. This remains local admission, not
a remotely signed runtime catalog. It has passed the
repeatable two-profile smoke test documented in
[`docs/phase-1-acceptance.md`](docs/phase-1-acceptance.md). It is still an MVP:
DPAPI-backed secret migration, encrypted backup, managed runtime
installation/update, online revocation freshness and high-strength fingerprint
resistance remain later milestones.

The Windows secret adapter stores only Generic Credentials in the current
user's Credential Manager namespace. Profile isolation and multi-open behavior
remain the primary guarantee; optional fingerprint hooks are explicitly
best-effort and do not imply a separate physical device.

## Architecture

```text
React UI
   | typed commands and events only
Tauri desktop boundary
   |
Application use cases
   |
Domain model and launch-plan compiler
   |
Ports: SQLite | Secret Vault | Chromium | Proxy | Diagnostics | Backup
```

Dependencies point inward. The domain crate has no Tauri, database, Windows
or Chromium dependency. Platform behavior is implemented behind small Rust
traits so it can be replaced without changing profile and session rules.

## Prerequisites

- Windows 11 x64
- Node.js 24 or newer
- Rust 1.97.1 MSVC toolchain
- Visual Studio 2022 Build Tools with the C++ workload and Windows SDK
- WebView2 Runtime for the later Tauri desktop build

The platform-independent Rust crates can also be tested with the GNU Windows
toolchain when MSVC Build Tools are unavailable.

## Development

Install and run the UI:

```powershell
npm.cmd install --prefix ui
npm.cmd run ui:dev
```

With `ui:dev` still running in the first terminal, start the Tauri development
shell in a second terminal:

```powershell
cargo run -p brows-desktop
```

Build and audit the UI:

```powershell
npm.cmd run check:ui
```

Build the production desktop executable with Tauri's packaged-asset protocol:

```powershell
npm.cmd run desktop:build
```

Do not use a bare `cargo build --release` for the distributable desktop app;
without the `custom-protocol` feature Tauri intentionally targets the Vite
development URL.

Test the Rust workspace with MSVC:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Or test the current platform-independent crates with GNU:

```powershell
cargo +stable-x86_64-pc-windows-gnu test --workspace
```

## Security Defaults

- No generic shell, file, HTTP or arbitrary-command IPC.
- No localhost management API.
- Chromium sandbox must stay enabled.
- No remote-debugging TCP port for normal profiles.
- Proxy credentials must never enter SQLite, process arguments or logs.
- Browser executables are confined to the managed runtime root, admitted only
  after Windows Authenticode verifies an allow-listed publisher, and checked
  against their registered SHA-256 digest before launch.
- Profile paths are allocated by the core and checked for traversal and
  reparse-point escapes.
- Telemetry is disabled by design.

See [`docs/architecture-plan.md`](docs/architecture-plan.md) for the complete
threat model, public contracts, phased roadmap and acceptance tests.
