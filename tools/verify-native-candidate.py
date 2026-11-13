"""Verify a pinned experimental artifact before test-only browser execution.

GitHub workflow checks run/repository/head provenance before downloading it.
This verifier establishes payload/source binding, never native acceptance/trust.
"""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "native/chromium"))
from stage_runtime import checked, digest, verify  # noqa: E402


def validate(runtime, build_report):
    runtime = runtime.resolve(strict=True)
    manifest = json.loads((ROOT / "native/chromium/source-manifest.json").read_text())
    screen = json.loads((ROOT / "native/chromium/screen/source-manifest.json").read_text())
    build = json.loads(build_report.read_text(encoding="utf-8-sig"))
    sha256 = digest(checked(runtime, "headless_shell.exe"))
    inventory = verify(runtime, sha256)
    if (build.get("compiled") is not True or build.get("runtimeBundleReady") is not True
            or build.get("runtimeAdmitted") is not False or build.get("complete") is not False
            or build.get("sourceRevision") != manifest["upstream"]["revision"]
            or build.get("testExecutableSha256") != sha256):
        raise ValueError("build evidence does not bind this compiled isolated executable")
    if (inventory.get("sourceRevision") != manifest["upstream"]["revision"]
            or inventory.get("patchSha256") != manifest["patch"]["sha256"]
            or inventory.get("screenPatchSha256") != screen["patch"]["sha256"]):
        raise ValueError("candidate source or patch pin mismatch")
    for name, expected in [("brows-source-manifest.json", manifest),
                           ("brows-screen-source-manifest.json", screen)]:
        if json.loads(checked(runtime, name).read_text()) != expected:
            raise ValueError("packaged source manifest differs from pinned inputs")
    for name, expected in [("brows-native-worker-platform.patch", manifest["patch"]["sha256"]),
                           ("brows-native-oopif-screen.patch", screen["patch"]["sha256"])]:
        if digest(checked(runtime, name)) != expected:
            raise ValueError("packaged native patch differs from pinned inputs")
    if checked(runtime, "brows-build-args.gn").read_text().strip() != build.get("gnArgs"):
        raise ValueError("candidate build arguments differ from observed build")
    return {"validationPassed": True, "compiled": True, "nativeAcceptancePassed": False,
            "runtimeAdmitted": False, "complete": False, "executableSha256": sha256,
            "runtimeVersion": manifest["upstream"]["version"],
            "executable": str(checked(runtime, "headless_shell.exe")),
            "sourceRevision": manifest["upstream"]["revision"],
            "workerPatchSha256": manifest["patch"]["sha256"],
            "screenPatchSha256": screen["patch"]["sha256"],
            "runtimeFilesVerified": len(inventory["files"])}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--build-report", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    report = {"validationPassed": False, "nativeAcceptancePassed": False,
              "runtimeAdmitted": False, "complete": False}
    try:
        report.update(validate(args.runtime, args.build_report))
    except Exception as error:
        report["failure"] = str(error)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    raise SystemExit(0 if report["validationPassed"] else 1)
