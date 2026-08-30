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
  allow-listed arguments. The production composition root records the current
  local Chrome digest at startup; publisher-signature verification is not yet
  implemented.
- The scheduler permits up to ten concurrent profiles, rejects a second active
  session for the same profile and preserves FIFO ordering beyond capacity.
- Stopping a session reaps its tracked process. The real smoke test confirmed
  that no Chrome process referencing its temporary profile roots remained.

## Runtime integrity record

The local Chrome entry is admitted using trust-on-first-use for the current
application run:

| Field | Accepted value |
| --- | --- |
| Executable | `C:\Program Files\Google\Chrome\Application\chrome.exe` |
| Product version | Read from the installed Chrome version directory |
| Authenticode status | Not checked yet |
| Publisher | Recorded as `Google LLC`; not cryptographically verified yet |
| SHA-256 | Calculated at app startup and rechecked before each launch |

If Chrome changes during one app run, Brows refuses to start a profile because
the recorded digest no longer matches. Restarting the app deliberately adopts
the newly installed file digest.

## Verification performed

The normal workspace suite, clippy checks, UI type checking and the Vite
production build all pass. The ignored, opt-in smoke test exercises the real
Windows Chrome path.

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

The default `SystemNative` persona gives every environment different cookies,
cache, site storage, login state and profile preferences, but does not claim a
different physical host fingerprint. The opt-in `SeedableNoise` persona adds
deterministic UA/platform/WebGL and navigator hooks; Canvas, Audio and Client
Hints remain incomplete and high-strength fingerprint tests can still identify
the browser.

The next phase may introduce controlled, internally consistent personas. It
must not silently randomize individual signals or weaken the Chromium sandbox.
