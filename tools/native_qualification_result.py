"""Combine the pinned build and controller reports after both workflows finish."""
import json
import os
import re
from pathlib import Path


def read_report(path):
    with path.open(encoding="utf-8-sig") as stream:
        report = json.load(stream)
    if not isinstance(report, dict):
        raise ValueError(f"Invalid report in {path.name}")
    return report


def require_fields(report, path, **expected):
    for name, value in expected.items():
        if name not in report or type(report[name]) is not type(value) or report[name] != value:
            raise ValueError(f"Unexpected or missing {name} in {path}")


def combine(root, run_id, head, source_result, controller_result):
    if source_result != "success" or controller_result != "success":
        raise ValueError(f"Source run {source_result}, controller job {controller_result}")
    build = read_report(root / "source/native-build.json")
    sha = build.get("testExecutableSha256")
    if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{64}", sha):
        raise ValueError("Missing compiled executable SHA-256")
    require_fields(build, "native-build.json", compiled=True, runtimeBundleReady=True,
                   nativeFirstScriptsPassed=True, nativeLifecyclePassed=True, nativeScreenPassed=True,
                   nativeAcceptancePassed=True, compileCheckpointOnly=False, compileChangedSourcesOnly=False,
                   runtimeAdmitted=False, complete=False, failure=None)
    for name, flags in [
        ("acceptance", {"nativeWorkerPlatform": True}),
        ("lifecycle", {"nativeLifecyclePassed": True}),
        ("screen-acceptance", {"nativeScreenPassed": True, "oopifObserved": True}),
    ]:
        filename = f"native-build.json.{name}.json"
        require_fields(read_report(root / "source" / filename), filename, expectation="native",
                       executableSha256=sha, assertionsPassed=True, runtimeAdmitted=False,
                       complete=False, failure=None, **flags)
    filename = "native-candidate-qualification.json"
    require_fields(read_report(root / "controller" / filename), filename, sourceRunId=run_id,
                   sourceHead=head, executableSha256=sha, assertionsPassed=True, nativeControllerPassed=True,
                   nativeProbePassed=True, runtimeAdmitted=False, complete=False)
    return sha


def main():
    root = Path("evidence")
    root.mkdir(exist_ok=True)
    result = {"assertionsPassed": False, "runtimeAdmitted": False, "complete": False,
              "sourceRunId": os.environ["SOURCE_ID"], "sourceHead": os.environ["SOURCE_HEAD"],
              "sourceResult": os.environ["SOURCE_RESULT"], "controllerResult": os.environ["CONTROLLER_RESULT"]}
    try:
        result["executableSha256"] = combine(root, result["sourceRunId"], result["sourceHead"],
                                             result["sourceResult"], result["controllerResult"])
        result["assertionsPassed"] = True
    except (ValueError, OSError) as error:
        result["failure"] = str(error)
        # An early build failure may have produced only a checkpoint report.
        for directory in ("source", "checkpoint"):
            try:
                build = read_report(root / directory / "native-build.json")
            except (ValueError, OSError):
                continue
            result["buildEvidence"] = {key: build[key] for key in
                                       ("stage", "failure", "changedCppObjectsCompiled", "actualCppCachePassed",
                                        "restoredCppCachePassed", "compiled", "runtimeBundleReady") if key in build}
            break
    output = json.dumps(result, indent=2)
    (root / "combined-result.json").write_text(output + "\n", encoding="utf-8")
    print(output)
    return 0 if result["assertionsPassed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
