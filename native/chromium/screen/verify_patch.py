"""Check exact upstream blobs and patch application, never C++/runtime success."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent


def safe_path(root, relative):
    parts = PurePosixPath(relative)
    if parts.is_absolute() or not parts.parts or ".." in parts.parts:
        raise ValueError("invalid source path")
    path = root.joinpath(*parts.parts)
    for entry in [path, *path.parents]:
        if entry == root:
            break
        if entry.is_symlink() or getattr(entry, "is_junction", lambda: False)():
            raise ValueError("source path traverses a link")
    return path


def git(args, directory):
    return subprocess.check_output(["git", *args], cwd=directory, text=True,
                                   stderr=subprocess.STDOUT).strip()


def check(source=None, apply=False):
    manifest = json.loads((ROOT / "source-manifest.json").read_text())
    revision = manifest["upstream"]["revision"]
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("invalid pinned source revision")
    patch = safe_path(ROOT, manifest["patch"]["path"])
    if hashlib.sha256(patch.read_bytes()).hexdigest() != manifest["patch"]["sha256"]:
        raise ValueError("patch digest mismatch")
    if apply and source is None:
        raise ValueError("--apply requires --source")
    if source:
        source = source.resolve(strict=True)
        if git(["rev-parse", "HEAD"], source) != revision:
            raise ValueError("source HEAD differs from pinned revision")
        if Path(git(["rev-parse", "--show-toplevel"], source)).resolve() != source:
            raise ValueError("source must be the Chromium root")
    with tempfile.TemporaryDirectory(prefix="brows-native-screen-") as temp:
        snapshot = Path(temp)
        for entry in manifest["sources"]:
            name = entry["path"]
            if source:
                data = safe_path(source, name).read_bytes()
            else:
                url = f"https://raw.githubusercontent.com/chromium/chromium/{revision}/{name}"
                with urllib.request.urlopen(url, timeout=30) as response:
                    data = response.read(256 * 1024 + 1)
            blob = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
            if len(data) > 256 * 1024 or blob != entry["gitBlob"]:
                raise ValueError(f"upstream blob mismatch: {name}")
            target = safe_path(snapshot, name)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        git(["init", "-q"], snapshot)
        git(["apply", "--check", "--whitespace=error", str(patch)], snapshot)
        git(["apply", "--whitespace=error", str(patch)], snapshot)
        if apply:
            # The Worker patch changes other files. These three input blobs
            # still have to match the unmodified pinned source byte for byte.
            git(["apply", "--check", "--whitespace=error", str(patch)], source)
            git(["apply", "--whitespace=error", str(patch)], source)
    return {"sourceRevision": revision, "patchSha256": manifest["patch"]["sha256"],
            "pinnedBlobsChecked": len(manifest["sources"]), "patchApplies": True,
            "patchAppliedToCheckout": apply, "compiled": False,
            "nativeAcceptancePassed": False, "runtimeAdmitted": False, "complete": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    report = {"patchApplies": False, "compiled": False, "nativeAcceptancePassed": False,
              "runtimeAdmitted": False, "complete": False}
    try:
        report.update(check(args.source, args.apply))
    except Exception as error:
        report["failure"] = str(error)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    raise SystemExit(0 if report["patchApplies"] else 1)
