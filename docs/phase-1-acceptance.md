# Phase 1 Acceptance — Isolated Multi-Open MVP

Status: **passed on Windows on 2026-08-02**.

## Delivered boundary

- The Tauri composition root uses `SqliteProfileRepository`; profile metadata
  survives a manager restart.
- Each profile receives a core-allocated directory below the canonical managed
  profile root. Chromium gets that directory only through the typed
  `UserDataDirectory` launch argument.
- `ManagedChromiumRuntime` launches the executable directly without a shell,
  verifies SHA-256 before every launch and accepts only registered builds and
  allow-listed arguments.
- The scheduler permits up to ten concurrent profiles, rejects a second active
  session for the same profile and preserves FIFO ordering beyond capacity.
- Stopping a session reaps its tracked process. The real smoke test confirmed
  that no Chrome process referencing its temporary profile roots remained.

## Runtime trust record

The first local catalog entry is deliberately fail-closed:

| Field | Accepted value |
| --- | --- |
| Executable | `C:\Program Files\Google\Chrome\Application\chrome.exe` |
| Product version | `150.0.7871.188` |
| Authenticode status | Valid |
| Publisher | Google LLC |
| Certificate thumbprint | `607A3EDAA64933E94422FC8F0C80388E0590986C` |
| SHA-256 | `36BD4EB0917605DCDDA6458215D5657B320B0CEF1B29BC03A1AA10976159162A` |

If Chrome updates and the executable hash changes, Brows refuses to start a
profile until the local catalog is deliberately updated after signature and
hash verification.

## Verification performed

The normal workspace suite passed 46 tests across the domain, contracts,
application services, SQLite, Chromium runtime, Windows Credential Manager and
desktop command boundary. UI type checking and the Vite production build also
passed.

The ignored, opt-in `phase_one_real_chrome_smoke_test` then exercised the real
production composition root:

1. create two profiles in a new SQLite database;
2. launch two signed Chrome processes concurrently;
3. confirm two distinct profile directories and Preferences files;
4. confirm a duplicate launch for profile A is rejected;
5. stop and reap both sessions;
6. reopen the composition root and confirm both profiles still exist;
7. confirm no test-profile Chrome process remains.

Run it explicitly with:

```powershell
cargo test -p brows-desktop phase_one_real_chrome_smoke_test -- --ignored --nocapture
```

## Honest fingerprint scope

Phase 1 gives every environment different cookies, cache, site storage, login
state and profile preferences. It does **not** claim a different physical host
fingerprint. Canvas, WebGL, GPU, installed fonts, AudioContext and other
host-derived signals remain shared and are labelled as such in the UI.

The next phase may introduce controlled, internally consistent personas. It
must not silently randomize individual signals or weaken the Chromium sandbox.
