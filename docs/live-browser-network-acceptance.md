# Actual browser network acceptance

The opt-in **Live browser network acceptance** workflow exercises `AppState::production` on a disposable Windows runner with a signature-admitted Microsoft Edge. Supply one or two authorized `IPv4:port` addresses and the same repository Secrets used by adapter acceptance:

- `BROWS_PROXY_ACCEPTANCE_USERNAME`
- `BROWS_PROXY_ACCEPTANCE_PASSWORD`

The workflow compiles before exposing Secrets. The ignored test consumes and removes those environment variables before launching Edge. Proxy passwords are saved through the actual Windows credential store; the browser receives a credential-free loopback broker address. Cleanup stops owned sessions, detaches the test profiles and removes the owned proxy credentials. User-specific addresses and credentials do not belong in source or reports containing request headers.

Each endpoint is tested as HTTP and SOCKS5, with SystemNative and SeedableNoise profiles. The test repeats after dropping and recreating the production application state, reloads persisted metadata, and verifies HTTPS exits, Canvas repeatability, distinct seeds and stable fingerprint signals after reopen. The known endpoint IPv4 is the expected exit for this acceptance fixture; rotating gateways need a different explicit expectation before using this fixture. A direct browser provides a separate exit and STUN control. The sole explicit bypass is `127.0.0.1` for the owned test page.

The test page uses fixed public endpoints:

- `https://api.ipify.org/?format=json`: the browser's HTTPS exit, with certificate verification enforced by Edge.
- Fresh random names under `dns4.browserleaks.net` and `dns6.browserleaks.net`: resolver observations returned by the operator's authoritative DNS test. Requests use no credentials or cookies. These are two observations per browser round, not the operator's full 50-query site test.
- `stun:stun.l.google.com:19302`: ICE candidate gathering without media permissions. The report retains candidate types/protocols and public server-reflexive addresses, never SDP or ICE credentials.

HTTP requests have ten-second deadlines and an 8 KiB response limit; ICE gathering stops after seven seconds. Each browser round has a 45-second supervisor deadline. The artifact is updated after each observation, so an interrupted run cannot look complete.

`allBrowserChecksPassed` covers the actual exits and fingerprint/reopen checks. `webrtcUdpPolicy` passes only when direct browsers actually obtained STUN server-reflexive candidates in both rounds and no proxied browser exposed a UDP candidate. Without a positive control it stays `not_checked`; an exposed proxied UDP candidate fails the test. This checks the selected UDP suppression policy with one public STUN service, not every WebRTC or TURN transport.

`dnsAcceptance` remains `not_checked` until independently established expected proxy resolver ranges are supplied to the delegated service described in [network-probe-service.md](network-probe-service.md). The report includes validated resolver IPs and overlap with the direct browser, but observations are never converted into an allowlist or a blanket no-leak claim. A missing IPv6 result is also incomplete evidence, not a pass.

Public services and proxy availability can change. Live tests are deliberately excluded from normal PR CI. Node fixture checks and Windows compilation/lint still run on every PR. Preserve the Actions run, product commit, signed Edge version and sanitized artifact when recording a live result; Windows Server runner results do not establish Windows 10/11 client coverage.

The production launch argument is `--webrtc-ip-handling-policy=disable_non_proxied_udp`, matching Chromium’s current `kWebRtcIPHandlingPolicy` in [chrome_switches.h](https://chromium.googlesource.com/chromium/src/+/main/chrome/common/chrome_switches.h). The earlier `--force-webrtc-ip-handling-policy` spelling did not suppress direct public STUN candidates on signed Edge 156 in the pre-fix live run. Regressions must be checked using actual candidates and direct controls, not the presence of a command-line argument alone.
