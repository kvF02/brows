# Native OOPIF screen geometry candidate

**Not compiled, not accepted, not enabled in an admitted production runtime.**
The private-controller screen cutover in PR #38 must not merge until a patched
runtime passes the positive checks and actual capability-probe qualification.

Signed Edge 156.0.4314.58 on Windows Server 2022 failed the unchanged strict
`private_native_screen_acceptance` in [job 131277480640](https://github.com/kvF02/brows/actions/runs/43245656940/job/131277480640):
the page requested 1440×900 at 1.25×, but the first script in the cross-process
iframe read a native screen width of 656. This is a real capability gap.

Pinned Chromium 158.0.8127.18 source confirms two independent paths:
`WebRemoteFrameImpl::InitializeFrameVisualProperties` initializes a child from
`GetOriginalScreenInfos`; `WebFrameWidgetImpl::DidUpdateSurfaceAndScreen` also
propagates only original screen info, and skips propagation when only emulated
geometry changes. Applying a CDP metrics command to the child is rejected by
the browser's top-target restriction.

The patch supplies exposed rectangle, available rectangle and orientation to
both creation and live-update paths. Device scale, color data and all other
compositor screen fields remain from the original screen info. The existing
RemoteFrame visual-properties comparison deduplicates unchanged IPC. The
controller deliberately uses zero viewport dimensions and zero DSF override;
this candidate does not claim arbitrary mobile/DSF emulation, fonts, HDR,
physical multi-monitor topology or ScreenDetails isolation.

`source-manifest.json` pins all three upstream Git blob IDs and the patch SHA256.
Source checking downloads the exact revision, verifies every byte and applies
the patch. Source validation always records `compiled: false` and
`nativeAcceptancePassed: false`.

```sh
python native/chromium/screen/verify_patch.py --report target/native-screen-source-check.json
```

The combined build driver applies this independent patch after the Worker
platform patch and before `gn gen`; the three screen input blobs must still
match the pinned upstream. The earlier PR #36 build ended at its four-hour limit
without a usable binary and did not contain this screen patch. The next combined
build includes both patches and all three positive gates, using a debug component
profile for functional qualification. It is not a release-performance result.

```sh
python native/chromium/screen/verify_patch.py --source /path/to/chromium/src --apply --report target/native-screen-source-check.json
```

The no-hook fixture `tools/native-screen-acceptance.mjs` records original raw
getters, receiver rejection and identity; CSS device media queries; first scripts
in page/same-origin frame/OOPIF; portrait updates in existing targets; dynamic
OOPIF creation; clear-to-host, restoration and page reload. A distinct OOPIF
target is required. It also checks DPR and live viewport/window preservation.
Use `--expect stock` for a baseline that explicitly expects the OOPIF gap, or
`--expect native` only on the patched binary. Both require the executable SHA256
and write observations before assertions; a stock pass never means native
acceptance. This isolated tool does not admit an unsigned production runtime.

```powershell
$digest = (Get-FileHash $binary -Algorithm SHA256).Hash.ToLowerInvariant()
node tools/native-screen-acceptance.mjs --browser $binary --sha256 $digest --expect native --report target/native-screen-positive.json
```

The actual private capability probe now waits for a separately controlled
cross-origin iframe's first-script observation, authenticates its message origin,
window and nonce, and checks native raw getters and CSS in both frames. A stock
native-screen mismatch returns Unsupported. A dedicated signed-Edge negative
test verifies this actual probe path; ordinary extension-mode probing is unchanged.
Unit cases reject missing OOPIF proof, host geometry, wrapped getters and CSS
mismatch even when the top frame matches. The Windows negative result must be
read from its own report; parser unit success alone is not browser qualification.

Before merging: compile the combined patches; pass the no-hook positive fixture
and strict production-controller screen acceptance; qualify the actual capability
probe and signed-runtime integration; retain runtime/package/lifecycle
regression results. Publisher/signature/full-distribution integrity gates stay
required for production.
