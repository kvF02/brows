# Brows

Brows is a Windows-first, local profile isolation browser manager. The first
release focuses on reliable multi-session isolation: each profile has its own
Chromium user-data directory, cookies, storage, cache and optional proxy.

Brows does not currently spoof Canvas, WebGL, Audio, GPU, fonts or other
host-level signals. Those values are deliberately reported as shared instead
of being presented as unique fingerprints.

## Current Status

The repository contains the first implementation milestone:

- framework-independent profile, proxy, runtime and session domain models;
- a ten-session FIFO scheduler with duplicate-profile protection;
- versioned, allow-listed UI command and event contracts;
- application services and ports with in-memory test adapters;
- SQLite profile persistence and a verified Chromium runtime adapter;
- a Windows Credential Manager `SecretStore` adapter with redacted errors,
  idempotent cleanup and a non-Windows unavailable stub;
- a React management UI with a real, allow-listed Tauri client and a mock used
  only by browser development;
- a Tauri 2 composition root covering the Profile and Session vertical slice;
- the complete architecture and security plan in
  [`docs/architecture-plan.md`](docs/architecture-plan.md).

The phase-one Windows desktop path now uses SQLite persistence and launches the
locally installed, catalog-pinned Google Chrome runtime. It has passed the
repeatable two-profile smoke test documented in
[`docs/phase-1-acceptance.md`](docs/phase-1-acceptance.md). It is still an MVP:
DPAPI-backed secret migration, the authenticated proxy broker, encrypted
backup, managed runtime installation/update and controlled fingerprint
profiles remain later milestones.

The Windows secret adapter stores only Generic Credentials in the current
user's Credential Manager namespace. It does not change browser or host-level
fingerprint signals; profile isolation and multi-open behavior remain the
primary v0.1 guarantee.

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
- Browser executables are confined to the managed runtime root and checked
  against their registered SHA-256 digest before launch.
- Profile paths are allocated by the core and checked for traversal and
  reparse-point escapes.
- Telemetry is disabled by design.

See [`docs/architecture-plan.md`](docs/architecture-plan.md) for the complete
threat model, public contracts, phased roadmap and acceptance tests.
