import json
from pathlib import Path
import tempfile
import unittest

from native_qualification_result import combine


class QualificationResultTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.sha = "a" * 64
        self.head = "b" * 40
        self.run_id = "44462941292"
        self.build = "source/native-build.json"
        self.candidate = "controller/native-candidate-qualification.json"
        self.write(self.build, testExecutableSha256=self.sha, compiled=True, runtimeBundleReady=True,
                   nativeFirstScriptsPassed=True, nativeLifecyclePassed=True, nativeScreenPassed=True,
                   nativeAcceptancePassed=True, compileCheckpointOnly=False, compileChangedSourcesOnly=False,
                   runtimeAdmitted=False, complete=False, failure=None)
        for suffix, flags in [
            ("acceptance", {"nativeWorkerPlatform": True}), ("lifecycle", {"nativeLifecyclePassed": True}),
            ("screen-acceptance", {"nativeScreenPassed": True, "oopifObserved": True}),
        ]:
            self.write(f"{self.build}.{suffix}.json", executableSha256=self.sha, expectation="native",
                       assertionsPassed=True, runtimeAdmitted=False, complete=False, failure=None, **flags)
        self.write(self.candidate, sourceRunId=self.run_id, sourceHead=self.head, executableSha256=self.sha,
                   assertionsPassed=True, nativeProbePassed=True, nativeControllerPassed=True,
                   runtimeAdmitted=False, complete=False)

    def write(self, name, **report):
        path = self.root / name
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps(report), encoding="utf-8-sig")

    def check(self, source="success", controller="success"):
        return combine(self.root, self.run_id, self.head, source, controller)

    def test_matching_reports_return_the_test_digest(self):
        self.assertEqual(self.check(), self.sha)

    def test_failed_or_cancelled_jobs_cannot_publish_a_pass(self):
        for statuses in [("failure", "success"), ("cancelled", "success"), ("success", "failure")]:
            with self.subTest(statuses=statuses), self.assertRaises(ValueError):
                self.check(*statuses)

    def test_stale_stock_or_incomplete_evidence_fails(self):
        for name, key, value in [
            (self.build, "compileCheckpointOnly", True), (self.build, "nativeLifecyclePassed", False),
            (f"{self.build}.acceptance.json", "expectation", "stock"),
            (f"{self.build}.lifecycle.json", "executableSha256", "c" * 64),
            (f"{self.build}.screen-acceptance.json", "oopifObserved", 1),
            (self.candidate, "sourceRunId", "another run"), (self.candidate, "sourceHead", "d" * 40),
            (self.candidate, "executableSha256", "e" * 64), (self.candidate, "nativeControllerPassed", False),
        ]:
            path = self.root / name
            original = json.loads(path.read_text(encoding="utf-8-sig"))
            with self.subTest(name=name, key=key), self.assertRaises(ValueError):
                self.write(name, **{**original, key: value})
                self.check()
            self.write(name, **original)

    def test_missing_evidence_fails(self):
        (self.root / f"{self.build}.screen-acceptance.json").unlink()
        with self.assertRaises(FileNotFoundError):
            self.check()


if __name__ == "__main__":
    unittest.main()
