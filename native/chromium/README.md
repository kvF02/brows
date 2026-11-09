# Controlled Chromium candidate: native Worker platform

This directory is an experimental source/build path for issue [#35](https://github.com/kvF02/brows/issues/35).
It is **not enabled in the desktop application**. The production runtime publisher,
signature and digest admission rules are unchanged. A test executable digest does
not admit a custom browser into production.

The source pin is Chromium **158.0.8127.18**, commit
`6449d9f9a6a906edff724b7b220199b3fde4615c`. Depot tools are separately pinned in
`source-manifest.json`. Update the source pin and rerun the complete native build
and runtime gate when taking security updates; this pin is not an update service.

## Native change

The patch connects the existing `Emulation.setUserAgentOverride` platform value
to `NavigatorBase::platform` through Blink's generated probe infrastructure.
It changes the native C++ getter implementation, rather than replacing the JS
getter. Empty/unset overrides retain the host platform; agent disable already
clears the stored value. Page Navigator's existing settings override is retained.
The patch does not implement font filtering, GPU/audio-device isolation or a
different TLS/HTTP2 stack.

## Separate evidence gates

1. **Source check:** each input is verified against its pinned Git blob;
   `git apply --check` and the upstream `make_instrumenting_probes.py` generator
   must succeed on both Linux and Windows. This does not compile or execute Chromium.
2. **Stock negative control:** the signed installed Edge runs in an isolated
   profile over a private CDP pipe, with no Brows hooks. It must retain the host
   Worker platform despite the page override. A passing negative control records
   `nativeWorkerPlatform: false`.
3. **Build prerequisites:** read-only checks record x64, RAM, NTFS space, VS C++ /
   ATL-MFC, SDK and debugger versions. Missing observations keep the build blocked.
4. **Candidate build and native gate:** only a ready host may fetch the pinned
   source/depot tools, apply the patch and build `headless_shell`. The candidate
   must return the configured platform through native getters in classic/module
   Dedicated, Shared and Service Worker first scripts, and clear the override
   without changing the getter object. Only this gate can report
   `nativeAcceptancePassed: true`.

Every gate keeps `complete: false`. Runtime trust/update/signing integration,
actual Windows app admission and full stop/wake/update acceptance remain separate
work. Do not close #35 from a source/generator check or stock control alone.

## Commands

Source check (downloads eleven pinned source/generator inputs, not a full checkout):

```sh
python -m pip install -r native/chromium/requirements.txt
python native/chromium/verify_patch.py --report target/native-source-check.json
```

Windows preflight, before allocating a new source/build root:

```powershell
./native/chromium/windows-preflight.ps1 -Report target/native-windows-preflight.json
```

The pinned [Windows build instructions](https://chromium.googlesource.com/chromium/src/+/6449d9f9a6a906edff724b7b220199b3fde4615c/docs/windows_build_instructions.md)
require x64 Windows 10+, at least 8 GiB RAM, 100 GiB free NTFS space, VS 2026 with
Desktop C++ and ATL/MFC, SDK 10.0.28000.2270 and SDK debugging tools
10.0.26100.3323+. More RAM and disk are preferable. The checker is conservative:
an observed header directory without a sufficient tool version does not establish
the required SDK revision. It never installs tools, deletes caches or disables
security settings to make a host appear ready.

On an admitted host with Python, Git, Node and the Python requirements installed:

```powershell
./native/chromium/build-windows.ps1 -BuildRoot C:\brows-native-source -Report target/native-build.json
```

Use a new root without spaces. The driver creates a pinned checkout and leaves
it available for diagnosis; a large source/build tree is expected. The CI build
job is skipped when prerequisites cannot be established, and the preflight
artifact explains why. A skipped build is not a successful native build.
Source checkout uses process-scoped Git settings for LF bytes and long paths;
the inherited settings are restored when the driver ends. The patch itself also
has an LF checkout attribute, so its pinned digest is portable across Git hosts.

On disposable GitHub-hosted Windows runners, CI may supply
`-PrepareHostedSdk` to the build driver. If the SDK is the only missing
prerequisite, this separately invoked setup downloads the fixed Microsoft SDK
installer recorded in the manifest, checks its SHA-256 and valid Microsoft
signature, installs only the desktop SDK, and repeats the read-only preflight
on the actual build host. Both inventories and the installer result are saved.
It does not run on personal or self-hosted computers, install Visual Studio,
remove existing software, or turn a failed preflight into native acceptance.

An executable produced by a build can also be checked explicitly:

```powershell
node native/chromium/platform-acceptance.mjs --browser C:\path\headless_shell.exe --sha256 <sha256> --expect native --report target/native-platform.json
```

The browser is started only with fresh temporary user data and an owned loopback
site. The fixture never changes platform getters or disables the sandbox. It
terminates only its owned process tree and removes its temporary profile.

Chromium-derived patch context, generator inputs and generated bindings use the
upstream BSD-style license, reproduced in `UPSTREAM_LICENSE`.
