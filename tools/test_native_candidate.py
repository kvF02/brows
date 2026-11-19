import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_candidate_input", Path(__file__).with_name("verify-native-candidate.py"))
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)
from stage_runtime import INDEX, digest, stage


class CandidateBindingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        out = root / "out"
        out.mkdir()
        self.args = 'is_debug=true is_component_build=true'
        for name, data in [("headless_shell.exe", b"unit fixture, not a browser"),
                           ("LICENSE.headless_shell", b"unit license"), ("args.gn", self.args.encode())]:
            (out / name).write_bytes(data)
        self.runtime = root / "runtime"
        self.sha = digest(out / "headless_shell.exe")
        self.inventory = stage(out, ["headless_shell.exe"], self.runtime, self.sha)
        self.report = root / "build.json"
        self.build = {"compiled": True, "runtimeBundleReady": True, "runtimeAdmitted": False,
                      "complete": False, "sourceRevision": self.inventory["sourceRevision"],
                      "testExecutableSha256": self.sha, "gnArgs": self.args,
                      "nativeAcceptancePassed": True}
        self.save()

    def save(self):
        self.report.write_text(json.dumps(self.build))

    def test_validated_input_never_becomes_native_acceptance_or_admission(self):
        result = candidate.validate(self.runtime, self.report)
        self.assertTrue(result["validationPassed"])
        for key in ["nativeAcceptancePassed", "runtimeAdmitted", "complete"]:
            self.assertFalse(result[key])

    def test_binary_existence_cannot_replace_compilation_or_hash_evidence(self):
        for changes in [{"compiled": False}, {"testExecutableSha256": "0" * 64}, {"runtimeAdmitted": True}]:
            with self.subTest(changes=changes):
                original = self.build.copy()
                self.build.update(changes)
                self.save()
                with self.assertRaises(ValueError):
                    candidate.validate(self.runtime, self.report)
                self.build = original

    def test_self_consistent_inventory_cannot_change_source_or_patch_pins(self):
        for field in ["sourceRevision", "patchSha256", "screenPatchSha256"]:
            with self.subTest(field=field):
                changed = self.inventory.copy()
                changed[field] = "0" * len(changed[field])
                (self.runtime / INDEX).write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    candidate.validate(self.runtime, self.report)

    def test_observed_build_arguments_must_match_packaged_arguments(self):
        self.build["gnArgs"] = "is_debug=false"
        self.save()
        with self.assertRaises(ValueError):
            candidate.validate(self.runtime, self.report)


if __name__ == "__main__":
    unittest.main()
