# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise actual shell assertions with controlled, revision-consistent replies."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from cr_status import StatusObservationError, observe, project


ROOT = Path(__file__).resolve().parent
FIXTURE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
while args and args[0].startswith("--"):
    if args[0] == "--context":
        assert args[1] == "kind-kars-e2e"
        args = args[2:]
    elif args[0].startswith("--request-timeout="):
        args = args[1:]
    else:
        raise AssertionError("Unexpected global option")
if args[0] == "apply":
    sys.stdin.read()
    sys.exit(0)
if args[0] == "delete" or args[1] == "configmap":
    sys.exit(0)
kind = args[1]
form = args[args.index("-o") + 1]
if "metadata.finalizers" in form:
    print('["kars.azure.com/egress-approval-cleanup"]')
    sys.exit(0)
record = Path(os.environ["STATUS_READS"])
reads = json.loads(record.read_text()) if record.exists() else []
index = len(reads)
reads.append(form)
record.write_text(json.dumps(reads))
if os.environ.get("STATUS_MODE") == "read-failure":
    print("PRIVATE-TRANSPORT-FIXTURE", file=sys.stderr)
    sys.exit(19)
phases = {"karsmemory": "Compiled", "inferencepolicy": "Compiled",
          "karseval": "Pending", "egressapproval": "Pending"}
reasons = {"karsmemory": "NoSandboxesReferencing",
           "inferencepolicy": "AwaitingRouterEnforcement",
           "karseval": "Reconciled", "egressapproval": "BlockedOnSandbox"}
status = {"phase": phases[kind], "observedGeneration": 1, "hostCount": 2,
          "conditions": [{"type": "Ready", "status": "False",
                          "reason": reasons[kind], "observedGeneration": 1}]}
if os.environ.get("STATUS_MODE") == "invalid":
    status["phase"] = "Failed"
    status["conditions"][0]["reason"] = "CompileFailed"
elif index == 0:
    status = {}
value = {"metadata": {"uid": "fixture-cr", "generation": 1,
                      "resourceVersion": str(index + 10)}, "status": status}
if form == "json":
    assert "--context" in sys.argv
    print(json.dumps(value))
elif "observedGeneration" in form:
    print(status.get("observedGeneration", ""), end="")
elif "hostCount" in form:
    print(status.get("hostCount", ""), end="")
elif ".status.phase" in form:
    print(status.get("phase", ""), end="")
elif form.endswith(".status}"):
    print(status.get("conditions", [{}])[0].get("status", ""), end="")
elif form.endswith(".reason}"):
    print(status.get("conditions", [{}])[0].get("reason", ""), end="")
else:
    raise AssertionError("Unexpected status projection")
'''


def shell_function(source, name):
    start = source.index(f"{name}() {{")
    return source[start:source.index("\n}\n", start) + 3]


class ShellStatusSnapshotTests(unittest.TestCase):
    def run_case(self, name, mode="transition"):
        source = (ROOT / "run.sh").read_text()
        script = """
set -euo pipefail
SCRIPT_DIR="$1"
FAIL=0
pass() { printf '[PASS] %s\\n' "$1"; }
fail() { printf '[FAIL] %s\\n' "$1"; FAIL=$((FAIL + 1)); }
dump_cr_diagnostics() { printf 'Public CR diagnostic requested\\n' >&2; }
"""
        script += shell_function(source, "wait_for_resource")
        script += shell_function(source, name)
        script += f"\n{name}\ntest \"$FAIL\" = 0\n"
        with tempfile.TemporaryDirectory(prefix="kars-cr-status-") as directory:
            root = Path(directory)
            executable = root / "kubectl"
            executable.write_text(FIXTURE)
            executable.chmod(0o700)
            record = root / "reads.json"
            result = subprocess.run(
                ["bash", "-c", script, "cr-status-test", str(ROOT)],
                env={**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                     "STATUS_READS": str(record), "STATUS_MODE": mode},
                capture_output=True, text=True, timeout=10,
            )
            reads = json.loads(record.read_text()) if record.exists() else []
        return result, reads

    def test_memory_never_combines_pre_status_phase_with_published_ready_reason(self):
        result, reads = self.run_case("test_crd_kars_memory")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("phase=Compiled ready=False reason=NoSandboxesReferencing", result.stdout)
        self.assertEqual(reads, ["json", "json"])

    def test_related_honest_state_assertions_use_complete_single_response_snapshots(self):
        for name in ("test_crd_inference_policy", "test_crd_kars_eval", "test_crd_egress_approval"):
            with self.subTest(name=name):
                result, reads = self.run_case(name)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(reads, ["json", "json"])

    def test_invalid_complete_memory_state_is_still_rejected(self):
        result, reads = self.run_case("test_crd_kars_memory", "invalid")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected honest-state", result.stdout)
        self.assertEqual(reads, ["json"])

    def test_read_failure_is_not_hidden_as_a_success_or_unbounded_poll(self):
        result, reads = self.run_case("test_crd_kars_memory", "read-failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("PRIVATE-TRANSPORT-FIXTURE", result.stdout + result.stderr)
        self.assertIn("status read failed", result.stderr)
        self.assertEqual(reads, ["json"])


def resource(phase="Compiled", ready="False", reason="NoSandboxesReferencing"):
    return {
        "metadata": {"uid": "fixture-cr", "generation": 1, "resourceVersion": "12"},
        "status": {"phase": phase, "observedGeneration": 1, "conditions": [
            {"type": "Ready", "status": ready, "reason": reason, "observedGeneration": 1},
        ]},
    }


class StatusProjectionTests(unittest.TestCase):
    def test_supported_memory_states_are_not_rewritten(self):
        for fields in (
            ("Compiled", "False", "NoSandboxesReferencing"),
            ("Compiled", "False", "AwaitingRouterEnforcement"),
            ("Ready", "True", "RouterEnforcing"),
            ("Failed", "False", "CompileFailed"),
        ):
            with self.subTest(fields=fields):
                self.assertEqual(project(resource(*fields)), (("fixture-cr", 1), fields))

    def test_unpublished_status_does_not_fabricate_any_field(self):
        for status in (None, {}, {"conditions": []}, {"conditions": None}):
            value = resource()
            value["status"] = status
            self.assertEqual(project(value), (("fixture-cr", 1), None))
        for key in ("phase", "observedGeneration"):
            value = resource()
            del value["status"][key]
            self.assertIsNone(project(value)[1])
        for key in ("status", "reason", "observedGeneration"):
            value = resource()
            del value["status"]["conditions"][0][key]
            self.assertIsNone(project(value)[1])

    def test_status_and_ready_must_both_observe_the_current_generation(self):
        for target in ("status", "ready"):
            value = resource()
            current = value["status"] if target == "status" else value["status"]["conditions"][0]
            current["observedGeneration"] = 0
            self.assertIsNone(project(value)[1])

    def test_duplicate_ready_and_malformed_metadata_are_rejected(self):
        value = resource()
        value["status"]["conditions"].append(copy.deepcopy(value["status"]["conditions"][0]))
        with self.assertRaises(StatusObservationError):
            project(value)
        for key, invalid in (("uid", ""), ("resourceVersion", None), ("generation", True),
                             ("generation", 0), ("generation", "1")):
            value = resource()
            value["metadata"][key] = invalid
            with self.subTest(key=key, value=invalid), self.assertRaises(StatusObservationError):
                project(value)

    def test_projection_cannot_inject_delimiters_or_coerce_status_types(self):
        for invalid in (True, "False|RouterEnforcing", "False\nReady", ["False"]):
            value = resource()
            value["status"]["conditions"][0]["status"] = invalid
            with self.subTest(value=invalid), self.assertRaises(StatusObservationError):
                project(value)

    def test_egress_host_count_comes_from_the_same_snapshot_without_defaulting(self):
        value = resource("Pending", "False", "BlockedOnSandbox")
        self.assertIsNone(project(value, require_hosts=True)[1])
        for count in (0, 2):
            value["status"]["hostCount"] = count
            self.assertEqual(project(value, require_hosts=True)[1][-1], str(count))
        for invalid in (-1, True, "2"):
            value["status"]["hostCount"] = invalid
            with self.assertRaises(StatusObservationError):
                project(value, require_hosts=True)


class StatusDeadlineTests(unittest.TestCase):
    def setUp(self):
        self.now = 0.0
        self.start_patch(patch("cr_status.time.time", return_value=1000.0))
        self.start_patch(patch("cr_status.time.monotonic", side_effect=lambda: self.now))
        self.start_patch(patch("cr_status.time.sleep", side_effect=self.sleep))
        self.run = self.start_patch(patch("cr_status.subprocess.run"))

    def start_patch(self, patcher):
        self.addCleanup(patcher.stop)
        return patcher.start()

    def sleep(self, duration):
        self.now += duration

    def replies(self, *values):
        self.run.side_effect = [
            subprocess.CompletedProcess([], 0, json.dumps(value), "") for value in values
        ]

    def observe(self, deadline=1003.0, kind="karsmemory"):
        return observe(kind, "e2e-fixture", "kars-system", deadline)

    def test_pending_then_complete_uses_one_json_read_per_snapshot(self):
        pending = resource()
        pending["status"] = {}
        self.replies(pending, resource())
        self.assertEqual(self.observe(), ("Compiled", "False", "NoSandboxesReferencing"))
        self.assertEqual(self.run.call_count, 2)
        for index, call in enumerate(self.run.call_args_list):
            self.assertEqual(call.args[0][:3], ["kubectl", "--context", "kind-kars-e2e"])
            self.assertEqual(call.args[0][-2:], ["-o", "json"])
            self.assertEqual(call.kwargs["timeout"], 3 - index)

    def test_identity_or_intent_changes_are_not_adopted_during_settlement(self):
        for key, value in (("uid", "replacement"), ("generation", 2)):
            self.now = 0
            pending = resource()
            pending["status"] = {}
            changed = resource()
            changed["metadata"][key] = value
            self.replies(pending, changed)
            with self.assertRaisesRegex(StatusObservationError, "identity or intent changed"):
                self.observe()

    def test_evaluator_preserves_its_existing_pending_false_settlement(self):
        self.replies(resource("Running", "False", "Reconciled"),
                     resource("Pending", "False", "Reconciled"))
        self.assertEqual(self.observe(kind="karseval"), ("Pending", "False", "Reconciled"))
        self.assertEqual(self.run.call_count, 2)

    def test_existing_deadline_expires_without_an_extra_read(self):
        pending = resource()
        pending["status"] = {}
        self.run.return_value = subprocess.CompletedProcess([], 0, json.dumps(pending), "")
        with self.assertRaisesRegex(StatusObservationError, "before the deadline"):
            self.observe()
        self.assertEqual(self.run.call_count, 3)
        self.assertEqual(self.now, 3)

    def test_expired_or_unbounded_deadline_does_not_start_a_request(self):
        for deadline in (999.0, 1000.0, float("inf"), float("nan"), 1046.0):
            with self.subTest(deadline=deadline), self.assertRaises(StatusObservationError):
                self.observe(deadline)
        self.run.assert_not_called()

    def test_late_complete_response_cannot_pass_after_the_deadline(self):
        def late(*_args, **_kwargs):
            self.now = 3
            return subprocess.CompletedProcess([], 0, json.dumps(resource()), "")
        self.run.side_effect = late
        with self.assertRaisesRegex(StatusObservationError, "before the deadline"):
            self.observe()
        self.assertEqual(self.run.call_count, 1)

    def test_api_failures_and_invalid_json_are_explicit_and_not_retried(self):
        for response in (
            subprocess.CompletedProcess([], 19, "", "PRIVATE-FIXTURE"),
            subprocess.CompletedProcess([], 0, "PRIVATE-FIXTURE", ""),
        ):
            self.run.reset_mock()
            self.run.return_value = response
            with self.assertRaises(StatusObservationError) as caught:
                self.observe()
            self.assertNotIn("PRIVATE-FIXTURE", str(caught.exception))
            self.assertEqual(self.run.call_count, 1)

    def test_subprocess_deadline_is_enforced_without_exposing_private_output(self):
        self.run.side_effect = subprocess.TimeoutExpired("kubectl", 3, stderr="PRIVATE-FIXTURE")
        with self.assertRaisesRegex(StatusObservationError, "read exceeded its deadline"):
            self.observe()
        self.assertEqual(self.run.call_count, 1)

    def test_only_fixed_fixture_kinds_and_safe_resource_names_can_be_read(self):
        for kind, name, namespace in (
            ("secret", "fixture", "kars-system"),
            ("karsmemory", "--raw=/api/v1/secrets", "kars-system"),
            ("karsmemory", "fixture", "../other"),
        ):
            with self.assertRaises(StatusObservationError):
                observe(kind, name, namespace, 1003.0)
        self.run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
