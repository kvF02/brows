# Brows desktop boundary

This crate is the Tauri 2 composition root. It owns no domain rules and stores
only `ProfileApplicationService` and `SessionApplicationService` in Tauri
state. The current prototype assembles in-memory ports; SQLite and the verified
Chromium runtime can replace those ports without changing any command handler.

The IPC allow-list is intentionally explicit:

- `profile_list`, `profile_get`, `profile_create`, `profile_update`
- `profile_clone`, `profile_archive`, `profile_restore`
- `session_list`, `session_launch`, `session_stop`, `session_cancel_queued`

Handlers accept `brows-contracts` request DTOs and return contract DTOs or a
stable `ErrorDto`. There is no generic dispatch, path, URL, shell command, or
Chromium flag argument.

The main window is created in Rust so every top-level navigation passes the
exact-origin guard. Packaged assets and the exact Vite loopback origin in debug
are permitted; remote sites and `file:` URLs are rejected. The capability has
no plugin permissions, and the project does not depend on Tauri shell, HTTP,
filesystem, opener, or dialog plugins.
