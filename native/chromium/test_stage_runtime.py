import json
import pathlib
import tempfile
import unittest

from stage_runtime import INDEX, digest, stage, verify


class TestRuntimeStaging(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.out = self.root / "out"
        self.out.mkdir()
        for name, data in [("headless_shell.exe", b"owned test executable"), ("icudtl.dat", b"runtime data"),
                           ("LICENSE.headless_shell", b"test-only license fixture"), ("args.gn", b'is_debug=false\n')]:
            (self.out / name).write_bytes(data)
        self.expected = digest(self.out / "headless_shell.exe")
        self.destination = self.root / "bundle"

    def make_bundle(self):
        return stage(self.out, ["./headless_shell.exe", "icudtl.dat"], self.destination, self.expected)

    def test_portable_payload_has_exact_bytes_and_no_admission(self):
        result = self.make_bundle()
        self.assertEqual((self.destination / "icudtl.dat").read_bytes(), b"runtime data")
        self.assertFalse(result["runtimeAdmitted"])
        self.assertFalse(result["complete"])
        self.assertEqual(result["screenPatchSha256"],
                         json.loads((self.destination / "brows-screen-source-manifest.json").read_text())["patch"]["sha256"])
        self.assertEqual(digest(self.destination / "brows-native-oopif-screen.patch"), result["screenPatchSha256"])
        self.assertEqual(verify(self.destination, self.expected), result)
        self.assertEqual(json.loads((self.destination / INDEX).read_text()), result)

    def test_corruption_and_extra_dll_are_rejected(self):
        self.make_bundle()
        (self.destination / "icudtl.dat").write_bytes(b"changed data")
        with self.assertRaises(ValueError):
            verify(self.destination, self.expected)
        (self.destination / "icudtl.dat").write_bytes(b"runtime data")
        (self.destination / "unexpected.dll").write_bytes(b"unexpected")
        with self.assertRaises(ValueError):
            verify(self.destination, self.expected)

    def test_outside_build_paths_are_rejected_before_publication(self):
        for path in ["../outside.dll", "C:\\outside.dll", "/outside.dll", "data/../../outside.dll"]:
            with self.subTest(path=path), self.assertRaises(ValueError):
                stage(self.out, [path], self.destination, self.expected)
            self.assertFalse(self.destination.exists())

    def test_missing_dependency_does_not_publish_partial_bundle(self):
        with self.assertRaises(ValueError):
            stage(self.out, ["headless_shell.exe", "missing.dll"], self.destination, self.expected)
        self.assertFalse(self.destination.exists())

    def test_symlink_dependency_cannot_export_an_external_file(self):
        external = self.root / "outside.dll"
        external.write_bytes(b"outside")
        try:
            (self.out / "linked.dll").symlink_to(external)
        except OSError:
            self.skipTest("host does not permit symlink creation")
        with self.assertRaises(ValueError):
            stage(self.out, ["headless_shell.exe", "linked.dll"], self.destination, self.expected)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
