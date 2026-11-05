# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from controller_failure_diagnostics import collect, project
from native_api import CORE, Failure, core
from test_observation_diagnostics import Fixture

PRIVATE = "private-controller-canary"
POD = core(CORE, "pods", "kars-controller-pod")


def fixture():
    value = Fixture()
    value.objects[POD]["status"] = {"containerStatuses": [{
        "name": "controller", "restartCount": 3,
        "state": {"waiting": {"reason": "CrashLoopBackOff", "message": PRIVATE}},
        "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137, "signal": 9,
                                       "message": PRIVATE, "containerID": PRIVATE}},
    }]}
    return value


class ControllerFailureDiagnosticsTests(unittest.TestCase):
    def test_crash_loop_retains_termination_and_only_reads_previous_logs(self):
        with patch("controller_failure_diagnostics.command", return_value=PRIVATE) as call:
            value = collect(SimpleNamespace(admin=fixture()))
        self.assertTrue(value["available"])
        self.assertTrue(value["diagnosticOnly"])
        self.assertEqual(value["pods"][0]["lastState"], {
            "phase": "terminated", "reason": "OOMKilled", "exitCode": 137, "signal": 9})
        self.assertEqual(value["pods"][0]["restartCount"], 3)
        self.assertEqual(call.call_count, 1)
        for argument in ("--previous", "--limit-bytes=131072", "--tail=512", "--request-timeout=10s"):
            self.assertIn(argument, call.call_args.args)
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_log_failure_does_not_hide_termination_evidence(self):
        with patch("controller_failure_diagnostics.command", side_effect=Failure(PRIVATE)):
            value = collect(SimpleNamespace(admin=fixture()))
        self.assertTrue(value["available"])
        self.assertFalse(value["pods"][0]["previousLogs"]["available"])
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_running_restarted_controller_collects_both_processes(self):
        f = fixture()
        f.objects[POD]["status"]["containerStatuses"][0]["state"] = {"running": {"startedAt": PRIVATE}}
        with patch("controller_failure_diagnostics.command", return_value="") as call:
            value = collect(SimpleNamespace(admin=f))
        self.assertTrue(value["available"])
        self.assertEqual(call.call_count, 2)
        self.assertNotIn("--previous", call.call_args_list[0].args)
        self.assertIn("--previous", call.call_args_list[1].args)

    def test_changed_snapshot_discards_process_projection(self):
        f = fixture()
        def changed(*args, **kwargs):
            f.objects[POD]["metadata"]["resourceVersion"] = "changed"
            return PRIVATE
        with patch("controller_failure_diagnostics.command", side_effect=changed):
            value = collect(SimpleNamespace(admin=f))
        self.assertFalse(value["available"])
        self.assertNotIn("pods", value)

    def test_log_projection_never_retains_errors_threads_or_unchecked_paths(self):
        source = "controller/src/main.rs"
        raw = "\n".join([
            f"thread '{PRIVATE}' panicked at {source}:77:9:", PRIVATE,
            f"thread 'main' panicked at controller/src/{PRIVATE}.rs:1:1:",
            "Error: leader-election: lease renewal failed",
            "Error: leader-election: lease renewal failed " + PRIVATE,
            json.dumps({"level": "ERROR", "target": "kars_controller", "fields": {"error": PRIVATE}}),
            "[]", "null",
        ])
        value = project(raw, {source})
        self.assertEqual(value["panicLocations"], [{"source": source, "line": 77}])
        self.assertEqual(value["leaderFailures"], ["renewal-failed"])
        self.assertEqual(value["errorRecords"], 1)
        self.assertNotIn(PRIVATE, json.dumps(value))
        with self.assertRaises(Failure):
            project("x" * 131073, {source})


if __name__ == "__main__":
    unittest.main()
