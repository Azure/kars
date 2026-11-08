# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise benchmark trust, comparisons, and shell failure propagation without Rust."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from ci import bench_regression as gate

ROOT = Path(__file__).resolve().parents[1]
BASE = "a" * 40
HEAD = "b" * 40
POLICY = {"_threshold_pct": 25, "groups": {"group/bench": {"median_ns": 2}}}


class IdentityTests(unittest.TestCase):
    def event(self, kind="push"):
        event = {"repository": {"full_name": "Azure/kars"}}
        if kind == "push":
            event.update(before=BASE, after=HEAD, deleted=False)
        if kind == "pull_request":
            event["pull_request"] = {"base": {"sha": BASE, "repo": {"full_name": "Azure/kars"}}}
        return event

    def resolve(self, kind, event=None, explicit=""):
        return gate.resolve_base(kind, event or self.event(kind), HEAD, "Azure/kars", explicit)

    def test_event_identity_and_ancestry(self):
        for kind in ("push", "pull_request", "workflow_dispatch"):
            with self.subTest(kind=kind), patch.object(gate, "git", side_effect=[HEAD, BASE]) as git, \
                    patch.object(gate.subprocess, "run") as run:
                self.assertEqual(self.resolve(kind, explicit=BASE), BASE)
                self.assertEqual(git.call_args_list[1].args,
                                 ("rev-parse", "--verify", BASE + "^{commit}"))
                run.assert_called_once_with(
                    ["git", "merge-base", "--is-ancestor", BASE, HEAD], check=True)

    def test_push_and_pr_cannot_override_event_baseline(self):
        for kind in ("push", "pull_request"):
            with patch.object(gate, "git", side_effect=[HEAD, BASE]), patch.object(gate.subprocess, "run"):
                self.assertEqual(self.resolve(kind, explicit="c" * 40), BASE)

    def test_reject_invalid_refs_before_git(self):
        for ref in ("", "0" * 40, "main", "HEAD~1", "a" * 39, "a" * 41,
                    "--upload-pack=evil", "$(touch owned)", BASE + "\nother=x", None):
            with self.subTest(ref=ref), patch.object(gate, "git") as git:
                with self.assertRaises(ValueError):
                    self.resolve("workflow_dispatch", explicit=ref)
                git.assert_not_called()

    def test_reject_new_branch_missing_dispatch_and_unknown_event(self):
        event = self.event()
        event["before"] = "0" * 40
        with self.assertRaises(ValueError):
            self.resolve("push", event)
        with self.assertRaises(ValueError):
            self.resolve("workflow_dispatch")
        with self.assertRaises(ValueError):
            self.resolve("pull_request_target")

    def test_reject_wrong_repository_push_identity_and_self_comparison(self):
        for field, value in (("before", HEAD), ("after", "c" * 40), ("deleted", True)):
            event = self.event()
            event[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.resolve("push", event)
        event = self.event()
        event["repository"]["full_name"] = "attacker/kars"
        with self.assertRaises(ValueError):
            self.resolve("push", event)
        event = self.event("pull_request")
        event["pull_request"]["base"]["repo"]["full_name"] = "attacker/kars"
        with self.assertRaises(ValueError):
            self.resolve("pull_request", event)

    def test_reject_wrong_checkout_or_unavailable_base(self):
        for results in (("c" * 40,), (HEAD, "c" * 40)):
            with patch.object(gate, "git", side_effect=results), self.assertRaises(ValueError):
                self.resolve("push")
        with patch.object(gate, "git", side_effect=subprocess.CalledProcessError(128, ["git"])):
            with self.assertRaises(subprocess.CalledProcessError):
                self.resolve("push")

    def test_reject_nonancestor(self):
        with patch.object(gate, "git", side_effect=[HEAD, BASE]), \
                patch.object(gate.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["git"])):
            with self.assertRaises(subprocess.CalledProcessError):
                self.resolve("push")

    def test_git_does_not_use_a_shell(self):
        with patch.object(gate.subprocess, "check_output", return_value=HEAD + "\n") as command:
            self.assertEqual(gate.git("rev-parse", "HEAD"), HEAD)
            command.assert_called_once_with(["git", "rev-parse", "HEAD"], text=True)


class HarnessTests(unittest.TestCase):
    manifest = '[workspace.dependencies]\ncriterion = "0.8"\n'
    lock = '[[package]]\nname = "criterion"\nversion = "0.8.2"\nsource = "registry"\nchecksum = "abc"\n'

    def check(self, manifest=None, lock=None):
        with patch.object(gate, "git", return_value=BASE), patch.object(
                Path, "read_text", side_effect=[self.manifest, self.lock,
                                               manifest or self.manifest, lock or self.lock]):
            gate.check_harness(Path(".bench-baseline"), BASE)

    def test_matching_harness(self):
        self.check()

    def test_changed_harness_or_checksum_rejected(self):
        with self.assertRaises(ValueError):
            self.check(manifest=self.manifest.replace("0.8", "0.5"))
        with self.assertRaises(ValueError):
            self.check(lock=self.lock.replace("0.8.2", "0.5.1"))
        with self.assertRaises(ValueError):
            self.check(lock=self.lock.replace('"abc"', '"different"'))
        with self.assertRaises(ValueError):
            self.check(lock=self.lock.replace('"criterion"', '"other"'))

    def test_wrong_baseline_checkout_rejected(self):
        with patch.object(gate, "git", return_value=HEAD), self.assertRaises(ValueError):
            gate.check_harness(Path(".bench-baseline"), BASE)


class ComparisonTests(unittest.TestCase):
    def compare(self, baseline, candidate, policy=POLICY):
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return gate.compare(policy, baseline, candidate)

    def test_same_runner_baseline_not_historical_median(self):
        self.assertEqual(self.compare({"group/bench": 4615}, {"group/bench": 4615}), 0)
        self.assertEqual(self.compare({"group/bench": 4615}, {"group/bench": 6000}), 1)

    def test_retained_hosted_failure_is_still_a_regression_against_its_reference(self):
        baseline = {"reconcile_decision/n=0": 2, "reconcile_decision/n=100": 3185,
                    "reconcile_decision/n=1000": 37488}
        hosted = {"reconcile_decision/n=0": 3, "reconcile_decision/n=100": 4615,
                  "reconcile_decision/n=1000": 51224}
        policy = {"_threshold_pct": 25, "groups": dict.fromkeys(baseline, {})}
        self.assertEqual(self.compare(baseline, hosted, policy), 1)
        self.assertEqual(self.compare(hosted, hosted, policy), 0)

    def test_exact_threshold_and_zero_scale_regression(self):
        self.assertEqual(self.compare({"group/bench": 100}, {"group/bench": 125}), 0)
        self.assertEqual(self.compare({"group/bench": 100}, {"group/bench": 126}), 1)
        self.assertEqual(self.compare({"group/bench": 2}, {"group/bench": 3}), 1)

    def test_no_measurements_missing_extra_and_nonpositive_fail_closed(self):
        valid = {"group/bench": 100}
        for invalid in ({}, {"other": 100}, {**valid, "extra": 100},
                        *({"group/bench": value} for value in
                          (0, -1, True, None, "1", float("nan"), float("inf"), float("-inf")))):
            for baseline, candidate in ((valid, invalid), (invalid, valid)):
                with self.subTest(baseline=baseline, candidate=candidate), self.assertRaises(ValueError):
                    self.compare(baseline, candidate)

    def test_empty_inventory_and_threshold_changes_rejected(self):
        for policy in ({"groups": {}, "_threshold_pct": 25},
                       {**POLICY, "_threshold_pct": 50}, {**POLICY, "_threshold_pct": "25"}):
            with self.assertRaises(ValueError):
                self.compare({"group/bench": 100}, {"group/bench": 100}, policy)

    def test_subnanosecond_measurements_keep_the_same_threshold(self):
        self.assertEqual(self.compare({"group/bench": 0.4}, {"group/bench": 0.5}), 0)
        self.assertEqual(self.compare({"group/bench": 0.4}, {"group/bench": 0.501}), 1)

    def write_estimate(self, directory, name="group/bench", value=0.456789):
        directory.mkdir(parents=True)
        (directory / "benchmark.json").write_text(json.dumps({"full_id": name}))
        (directory / "estimates.json").write_text(json.dumps({
            "median": {"point_estimate": value}, "mean": {"point_estimate": 99}}))

    def test_reads_full_precision_medians_and_ignores_saved_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_estimate(root / "group/bench/new")
            self.write_estimate(root / "group/bench/baseline", value=99)
            self.assertEqual(gate.read_estimates(root), {"group/bench": 0.456789})
            self.write_estimate(root / "duplicate/new")
            with self.assertRaisesRegex(ValueError, "duplicate measurement"):
                gate.read_estimates(root)

    def test_missing_malformed_and_empty_estimates_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                gate.read_estimates(root / "missing")
            with self.assertRaises(ValueError):
                self.compare(gate.read_estimates(root), {"group/bench": 1})
            current = root / "group/bench/new"
            self.write_estimate(current)
            for bad in ({}, {"median": {}}, {"median": {"point_estimate": "0.5"}}):
                (current / "estimates.json").write_text(json.dumps(bad))
                with self.assertRaises((ValueError, KeyError)):
                    self.compare(gate.read_estimates(root), {"group/bench": 1})
            (current / "estimates.json").unlink()
            with self.assertRaises(OSError):
                gate.read_estimates(root)

    def test_cli_returns_regression_and_invalid_input_failures(self):
        for candidate, expected in ((0.5, 0), (0.501, 1)):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "policy.json").write_text(json.dumps(POLICY))
                self.write_estimate(root / "base/group/bench/new", value=0.4)
                self.write_estimate(root / "candidate/group/bench/new", value=candidate)
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(gate.main(["bench_regression.py", str(root / "policy.json"),
                                               str(root / "base"), str(root / "candidate")]), expected)
        for error in (OSError("missing log"), ValueError("malformed policy")):
            with patch.object(Path, "read_text", side_effect=error), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(gate.main(["bench_regression.py", "policy", "base", "candidate"]), 2)
        with patch.object(gate, "check_harness", side_effect=subprocess.CalledProcessError(128, ["git"])), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(gate.main(["bench_regression.py", "--check-harness", "base", BASE]), 2)

    def test_workflow_isolates_and_retains_full_precision_outputs(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn('export CRITERION_HOME="$GITHUB_WORKSPACE/bench-evidence/$revision-$component-criterion"', workflow)
        self.assertIn('mkdir "$CRITERION_HOME"', workflow)
        self.assertNotIn('mkdir -p "$CRITERION_HOME"', workflow)
        for component in ("controller", "router"):
            self.assertIn(f"bench-evidence/baseline-{component}-criterion bench-evidence/candidate-{component}-criterion", workflow)
        self.assertIn("path: bench-evidence/", workflow)

    def test_benchmark_job_denies_cache_writes_at_the_token_boundary(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  bench-regression:\n", 1)[1].split("    steps:\n", 1)[0]
        self.assertIn("\n    cache-mode: read\n", job)
        self.assertEqual(job.count("cache-mode:"), 1)
        self.assertIn("\n    permissions:\n      contents: read\n", job)

    def test_baseline_materializes_only_the_exact_local_commit(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        step = workflow.split("      - name: Materialize validated local benchmark baseline\n", 1)[1]
        step = step.split("\n      - ", 1)[0]
        self.assertIn("BENCH_BASE_SHA: ${{ steps.baseline.outputs.base_sha }}", step)
        command = step.split("        run: ", 1)[1].strip()
        self.assertEqual(command, 'git -c protocol.allow=never worktree add --detach '
                         '--no-guess-remote -- .bench-baseline "$BENCH_BASE_SHA"')
        self.assertNotIn("ref: ${{ steps.baseline.outputs.base_sha }}", workflow)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git_env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}

            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, env=git_env, text=True,
                                               stderr=subprocess.STDOUT).strip()

            git("init", "--quiet")
            for text in ("baseline", "candidate"):
                (root / "revision.txt").write_text(text)
                git("add", "revision.txt")
                git("-c", "user.name=Benchmark Test", "-c", "user.email=benchmark@example.invalid",
                    "commit", "--quiet", "-m", text)
            base = git("rev-parse", "HEAD^")
            candidate = git("rev-parse", "HEAD")
            git("merge-base", "--is-ancestor", base, candidate)
            env = {**git_env, "BENCH_BASE_SHA": base}
            result = subprocess.run(["bash", "-c", command], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(git("-C", ".bench-baseline", "rev-parse", "HEAD"), base)
            self.assertEqual((root / ".bench-baseline/revision.txt").read_text(), "baseline")
            self.assertEqual(git("rev-parse", "HEAD"), candidate)
            self.assertEqual((root / "revision.txt").read_text(), "candidate")
            self.assertEqual(git("status", "--porcelain", "--untracked-files=no"), "")
            (root / ".bench-baseline/revision.txt").write_text("retained evidence")
            repeat = subprocess.run(["bash", "-c", command], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertNotEqual(repeat.returncode, 0)
            self.assertEqual((root / ".bench-baseline/revision.txt").read_text(), "retained evidence")
            git("worktree", "remove", "--force", ".bench-baseline")
            missing = subprocess.run(["bash", "-c", command], cwd=root,
                                     env={**env, "BENCH_BASE_SHA": "f" * 40},
                                     capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertFalse((root / ".bench-baseline").exists())

    def test_hosted_measurement_pipeline_propagates_cargo_failure(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        command = workflow.split('(cd "$source" && cargo bench', 1)[1]
        command = '(cd "$source" && cargo bench' + command.split('\n            done', 1)[0]
        script = '''set -euo pipefail
source=.; package=unused; bench=unused; revision=unused; component=unused
cargo() { echo 'deliberate cargo failure' >&2; return 17; }
tee() { cat; }
''' + command + "\necho unexpected-success\n"
        result = subprocess.run(["bash", "-c", script], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertIn("deliberate cargo failure", result.stdout)
        self.assertNotIn("unexpected-success", result.stdout)


if __name__ == "__main__":
    unittest.main()
