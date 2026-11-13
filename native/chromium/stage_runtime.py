"""Stage GN runtime dependencies for isolated tests, never production admission."""
import argparse
import hashlib
import json
import pathlib
import shutil
import stat
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent
INDEX = "brows-test-runtime-manifest.json"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def checked(root, name):
    name = name.replace("\\", "/")
    relative = pathlib.PurePosixPath(name)
    if (not relative.parts or relative.is_absolute() or ".." in relative.parts
            or any(any(c in part for c in '<>:"|?*') or any(ord(c) < 32 for c in part)
                   or part.endswith((".", " ")) for part in relative.parts)):
        raise ValueError("runtime dependency must be a safe build-relative path")
    path = root.joinpath(*relative.parts)
    for item in [path, *path.parents]:
        if item == root:
            break
        # Python 3.11 lacks Path.is_junction; Windows lstat attributes still
        # expose junctions and other reparse points without following them.
        try:
            attributes = getattr(item.lstat(), "st_file_attributes", 0)
        except FileNotFoundError:
            attributes = 0
        if item.is_symlink() or attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400):
            raise ValueError("runtime dependency traverses a link")
    return path


def stage(out, dependencies, destination, expected_executable):
    out = out.resolve(strict=True)
    destination = destination.absolute()
    if destination.exists() or destination.is_relative_to(out) or out.is_relative_to(destination):
        raise ValueError("use a new staging destination outside the build output")
    entries = {}

    def add(name, path):
        checked(out, name)
        key = name.casefold()
        if key == INDEX.casefold() or (key in entries and entries[key][0] != name):
            raise ValueError("runtime archive name collision")
        if not path.is_file() or path.suffix.lower() in {".obj", ".lib", ".pdb", ".a", ".o"}:
            raise ValueError("missing or non-runtime dependency")
        entries[key] = (name, path)
        if len(entries) > 2048:
            raise ValueError("runtime file-count limit")

    for line in dependencies:
        if not line.strip():
            continue
        source = checked(out, line.strip())
        if source.is_dir():
            for child in source.rglob("*"):
                relative = child.relative_to(out).as_posix()
                checked(out, relative)
                if child.is_file():
                    add(relative, child)
        else:
            add(source.relative_to(out).as_posix(), source)
    add("LICENSE.headless_shell", checked(out, "LICENSE.headless_shell"))
    add("brows-build-args.gn", checked(out, "args.gn"))
    add("brows-source-manifest.json", ROOT / "source-manifest.json")
    add("brows-native-worker-platform.patch", ROOT / "patches/0001-native-worker-platform.patch")
    add("brows-screen-source-manifest.json", ROOT / "screen/source-manifest.json")
    add("brows-native-oopif-screen.patch", ROOT / "screen/patches/0001-native-oopif-screen-geometry.patch")
    if "headless_shell.exe" not in entries or digest(entries["headless_shell.exe"][1]) != expected_executable:
        raise ValueError("compiled test executable digest mismatch")
    if sum(path.stat().st_size for _, path in entries.values()) > 2 * 1024**3:
        raise ValueError("runtime size limit")
    manifest = json.loads((ROOT / "source-manifest.json").read_text())
    screen_manifest = json.loads((ROOT / "screen/source-manifest.json").read_text())
    result = {"schemaVersion": 1, "scope": "unsigned isolated test runtime; no production admission",
              "sourceRevision": manifest["upstream"]["revision"], "patchSha256": manifest["patch"]["sha256"],
              "screenPatchSha256": screen_manifest["patch"]["sha256"],
              "executableSha256": expected_executable, "runtimeAdmitted": False, "complete": False, "files": []}
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = pathlib.Path(tempfile.mkdtemp(prefix="brows-runtime-stage-", dir=destination.parent))
    try:
        for name, source in sorted(entries.values()):
            target = temporary / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            result["files"].append({"path": name, "bytes": target.stat().st_size, "sha256": digest(target)})
        (temporary / INDEX).write_text(json.dumps(result, indent=2) + "\n")
        verify(temporary, expected_executable)
        temporary.rename(destination)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    return result


def verify(directory, expected_executable):
    directory = directory.resolve(strict=True)
    result = json.loads(checked(directory, INDEX).read_text())
    if result["runtimeAdmitted"] is not False or result["complete"] is not False:
        raise ValueError("test runtime cannot claim production admission")
    expected = {INDEX}
    for entry in result["files"]:
        path = checked(directory, entry["path"])
        if entry["path"] in expected or path.stat().st_size != entry["bytes"] or digest(path) != entry["sha256"]:
            raise ValueError("runtime payload changed")
        expected.add(entry["path"])
    actual = set()
    for path in directory.rglob("*"):
        checked(directory, path.relative_to(directory).as_posix())
        if path.is_file():
            actual.add(path.relative_to(directory).as_posix())
    if actual != expected or digest(directory / "headless_shell.exe") != expected_executable:
        raise ValueError("unexpected runtime payload or executable digest")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=pathlib.Path)
    parser.add_argument("--deps", type=pathlib.Path)
    parser.add_argument("--dest", type=pathlib.Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    if args.verify:
        result = verify(args.dest, args.sha256)
    else:
        if not args.out or not args.deps:
            parser.error("--out and --deps are required when staging")
        result = stage(args.out, args.deps.read_text(encoding="utf-8-sig").splitlines(), args.dest, args.sha256)
    print(json.dumps({"filesVerified": len(result["files"]), "runtimeAdmitted": False, "complete": False}))


if __name__ == "__main__":
    main()
