"""Verify the pinned Chromium patch and run its real probe generator.

This is source validation. It never marks a browser compiled or accepted.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent
MANIFEST = ROOT / "source-manifest.json"


def git_blob(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()


def run(args, cwd):
    try:
        return subprocess.check_output(args, cwd=cwd, text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"source tool failed: {error.output[-3000:]}") from error


def safe_file(root, relative):
    parts = pathlib.PurePosixPath(relative)
    if parts.is_absolute() or ".." in parts.parts:
        raise ValueError("invalid source path")
    path = root.joinpath(*parts.parts)
    for entry in [path, *path.parents]:
        if entry == root:
            break
        if entry.is_symlink() or getattr(entry, "is_junction", lambda: False)():
            raise ValueError("source path traverses a link")
    return path


def check(manifest, source=None, apply=False):
    patch = safe_file(ROOT, manifest["patch"]["path"])
    if hashlib.sha256(patch.read_bytes()).hexdigest() != manifest["patch"]["sha256"]:
        raise ValueError("patch digest mismatch")
    revision = manifest["upstream"]["revision"]
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("invalid pinned revision")
    if apply and source is None:
        raise ValueError("--apply requires a full pinned source checkout")
    if source:
        source = source.resolve(strict=True)
        if run(["git", "rev-parse", "HEAD"], source).strip() != revision:
            raise ValueError("source HEAD does not match the pinned revision")
        if pathlib.Path(run(["git", "rev-parse", "--show-toplevel"], source).strip()).resolve() != source:
            raise ValueError("source must be the Chromium repository root")
        if run(["git", "status", "--porcelain", "--untracked-files=no"], source).strip():
            raise ValueError("source checkout must be clean before applying this patch")
    with tempfile.TemporaryDirectory(prefix="brows-chromium-source-") as temporary:
        snapshot = pathlib.Path(temporary)
        for entry in manifest["sources"]:
            relative = entry["path"]
            target = safe_file(snapshot, relative)
            target.parent.mkdir(parents=True, exist_ok=True)
            if source:
                data = safe_file(source, relative).read_bytes()
            else:
                url = f"https://raw.githubusercontent.com/chromium/chromium/{revision}/{relative}"
                with urllib.request.urlopen(url, timeout=30) as response:
                    data = response.read(256 * 1024 + 1)
            if len(data) > 256 * 1024 or git_blob(data) != entry["gitBlob"]:
                raise ValueError(f"pinned source mismatch: {relative}")
            target.write_bytes(data)
        run(["git", "init", "-q"], snapshot)
        run(["git", "apply", "--check", str(patch)], snapshot)
        run(["git", "apply", str(patch)], snapshot)
        generated = snapshot / "gen"
        generated.mkdir()
        generator = [
            sys.executable,
            "third_party/blink/renderer/build/scripts/make_instrumenting_probes.py",
            "third_party/blink/renderer/core/probe/core_probes.pidl",
            "--config", "third_party/blink/renderer/core/probe/core_probes.json5",
            "--output_dir", str(generated),
        ]
        # Prove the real generator rejects incomplete wiring, then restore the
        # observer and generate the actual candidate bindings.
        config = snapshot / "third_party/blink/renderer/core/probe/core_probes.json5"
        complete_config = config.read_text()
        needle = '"ApplyNavigatorPlatformOverride",'
        if complete_config.count(needle) != 1:
            raise ValueError("new probe observer is missing or duplicated")
        config.write_text(complete_config.replace(needle, "", 1))
        try:
            run(generator, snapshot)
        except RuntimeError as error:
            if "Unused probes" not in str(error) or "ApplyNavigatorPlatformOverride" not in str(error):
                raise
        else:
            raise ValueError("upstream generator accepted an unobserved probe")
        finally:
            config.write_text(complete_config)
        run(generator, snapshot)
        inline = (generated / "core_probes_inl.h").read_text()
        implementation = (generated / "core_probes_impl.cc").read_text()
        if "ApplyNavigatorPlatformOverride" not in inline or "ApplyNavigatorPlatformOverride" not in implementation:
            raise ValueError("the upstream generator did not emit the new probe")
        if apply:
            run(["git", "apply", "--check", str(patch)], source)
            run(["git", "apply", str(patch)], source)
        return {"sourceRevision": revision, "sourceFilesVerified": len(manifest["sources"]),
                "patchApplicabilityPassed": True, "probeGeneratorPassed": True,
                "missingObserverRejected": True,
                "patchAppliedToCheckout": apply, "compiled": False,
                "nativeAcceptancePassed": False, "complete": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=pathlib.Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    report = {"scope": "pinned source and generated probe only", "assertionsPassed": False,
              "compiled": False, "nativeAcceptancePassed": False, "complete": False}
    try:
        report.update(check(json.loads(MANIFEST.read_text()), args.source, args.apply))
        report["assertionsPassed"] = True
    except Exception as error:
        report["failure"] = str(error)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    return 0 if report["assertionsPassed"] else 1


if __name__ == "__main__":
    sys.exit(main())
