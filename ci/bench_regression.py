#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare full-precision Criterion estimates using the trusted base's policy.

Committed medians remain historical evidence, not cross-machine CI ceilings.
Identity resolution and harness checks run before either revision is compiled.
Only Python's standard library is required.
"""

import json
import math
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

SHA_RE = re.compile(r"[0-9a-f]{40}")


def commit_sha(value: str) -> str:
    if not isinstance(value, str) or not SHA_RE.fullmatch(value) or value == "0" * 40:
        raise ValueError("baseline and candidate must be nonzero full commit SHAs")
    return value


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def resolve_base(event_name: str, event: dict, candidate: str, repository: str,
                 explicit: str = "") -> str:
    candidate = commit_sha(candidate)
    if not repository or event["repository"]["full_name"] != repository:
        raise ValueError("event repository does not match this checkout's repository")
    if event_name == "pull_request":
        base = event["pull_request"]["base"]
        if base["repo"]["full_name"] != repository:
            raise ValueError("PR base must belong to this repository")
        baseline = commit_sha(base["sha"])
    elif event_name == "push":
        if event.get("deleted") or event["after"] != candidate:
            raise ValueError("push does not identify the checked-out candidate")
        baseline = commit_sha(event["before"])
    elif event_name == "workflow_dispatch":
        baseline = commit_sha(explicit)
    else:
        raise ValueError(f"no established benchmark baseline for event {event_name!r}")
    if baseline == candidate:
        raise ValueError("baseline must differ from candidate")
    if git("rev-parse", "--verify", "HEAD^{commit}") != candidate:
        raise ValueError("checkout HEAD does not match the event candidate")
    if git("rev-parse", "--verify", f"{baseline}^{{commit}}") != baseline:
        raise ValueError("baseline commit is unavailable")
    subprocess.run(["git", "merge-base", "--is-ancestor", baseline, candidate], check=True)
    return baseline


def check_harness(base: Path, baseline: str) -> None:
    baseline = commit_sha(baseline)
    if git("-C", str(base), "rev-parse", "--verify", "HEAD^{commit}") != baseline:
        raise ValueError("baseline checkout does not match the resolved commit")
    signatures = []
    for root in (base, Path(".")):
        manifest = tomllib.loads((root / "Cargo.toml").read_text())
        lock = tomllib.loads((root / "Cargo.lock").read_text())
        packages = [
            (p["name"], p["version"], p.get("source"), p.get("checksum"))
            for p in lock["package"] if p["name"] in ("criterion", "criterion-plot")
        ]
        if not any(p[0] == "criterion" for p in packages):
            raise ValueError("Criterion is missing from the lockfile")
        signatures.append((manifest["workspace"]["dependencies"]["criterion"], sorted(packages)))
    if signatures[0] != signatures[1]:
        raise ValueError("Criterion harness changed; establish comparable harnesses before gating")


def read_estimates(directory: Path) -> dict[str, float]:
    out: dict[str, float] = {}
    if not directory.is_dir():
        raise ValueError(f"Criterion output directory is unavailable: {directory}")
    for benchmark in sorted(directory.glob("**/new/benchmark.json")):
        name = json.loads(benchmark.read_text())["full_id"]
        if not isinstance(name, str) or not name:
            raise ValueError("Criterion benchmark must have a nonempty full_id")
        if name in out:
            raise ValueError(f"duplicate measurement: {name}")
        estimates = json.loads(benchmark.with_name("estimates.json").read_text())
        out[name] = estimates["median"]["point_estimate"]
    return out


def compare(policy: dict, baseline: dict[str, float], measured: dict[str, float]) -> int:
    groups = policy["groups"]
    threshold_pct = policy["_threshold_pct"]
    if not isinstance(groups, dict) or not groups or threshold_pct != 25:
        raise ValueError("expected a nonempty benchmark inventory and unchanged 25% threshold")
    for label, values in (("baseline", baseline), ("candidate", measured)):
        if set(values) != set(groups):
            raise ValueError(f"{label} measurements do not exactly match the baseline inventory")
        if any(type(value) not in (int, float) or not math.isfinite(value) or value <= 0
               for value in values.values()):
            raise ValueError(f"{label} measurements must be finite positive numbers")
    failures = []
    for name in groups:
        actual = measured[name]
        reference = baseline[name]
        ceiling = reference * 1.25
        regressed = actual * 100 > reference * (100 + threshold_pct)
        status = "REGRESSED" if regressed else "OK"
        print(f"  {status:10s} {name:50s} {actual:10.6g}ns  "
              f"(same-run baseline {reference}ns, ceiling {ceiling:g}ns)")
        if regressed:
            failures.append(name)
    if failures:
        print("Bench regression FAIL: " + ", ".join(failures), file=sys.stderr)
        return 1
    print(f"All {len(groups)} bench groups within +{threshold_pct}% same-run baseline.")
    return 0


def main(argv: list[str]) -> int:
    try:
        if argv[1:] == ["--resolve-base"]:
            event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
            baseline = resolve_base(os.environ["GITHUB_EVENT_NAME"], event,
                                    os.environ["GITHUB_SHA"], os.environ["GITHUB_REPOSITORY"],
                                    os.environ.get("BENCHMARK_BASE_SHA", ""))
            print(f"base_sha={baseline}")
            return 0
        if len(argv) == 4 and argv[1] == "--check-harness":
            check_harness(Path(argv[2]), argv[3])
            return 0
        if len(argv) != 4:
            raise ValueError("usage: bench_regression.py <base-policy.json> <base-criterion-dir> <candidate-criterion-dir>")
        return compare(json.loads(Path(argv[1]).read_text()),
                       read_estimates(Path(argv[2])), read_estimates(Path(argv[3])))
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        print(f"Bench comparison unavailable: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
