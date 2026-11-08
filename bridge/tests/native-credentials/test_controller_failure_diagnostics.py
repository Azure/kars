# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from controller_failure_diagnostics import GRANT_STAGES, collect, project
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


def stage_record(stage="parent", phase="before"):
    return {"level": "INFO", "target": "kars_controller::credential_grants",
            "fields": {"message": "Credential grant lifecycle stage", "stage": stage, "phase": phase}}


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

    def test_stack_overflow_is_fixed_classification_not_a_thread_or_message(self):
        raw = "\n".join([f"thread '{PRIVATE}' has overflowed its stack",
                          "fatal runtime error: stack overflow, aborting"] * 100)
        value = project(raw, set())
        self.assertEqual(value["fatalRuntimeFailures"], ["stack-overflow"])
        self.assertEqual(value["panicLocations"], [])
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_embedded_or_extended_fatal_text_is_not_runtime_evidence(self):
        raw = "\n".join([
            "fatal runtime error: stack overflow, aborting " + PRIVATE,
            PRIVATE + " fatal runtime error: stack overflow, aborting",
            f"thread '{PRIVATE}' has overflowed its stack {PRIVATE}",
            json.dumps({"message": "fatal runtime error: stack overflow, aborting"}),
            "Segmentation fault " + PRIVATE,
        ])
        value = project(raw, set())
        self.assertEqual(value["fatalRuntimeFailures"], [])
        self.assertNotIn(PRIVATE, json.dumps(value))
        self.assertEqual(project("fatal runtime error: stack overflow, aborting\n" + "ignored\n" * 512, set())["fatalRuntimeFailures"], [])

    def test_grant_stages_are_closed_and_keep_only_fixed_fields(self):
        expected = {"parent", "validation", "writer-authority", "observer-issuance",
                    "observer-revocation", "control", "publication", "writer-reconcile",
                    "writer-rbac-apply", "writer-rbac-revoke", "writer-observer-revoke",
                    "writer-release", "observer-tls-ensure", "observer-ensure",
                    "observer-retire", "observer-tls-retire"}
        self.assertEqual(GRANT_STAGES, expected)
        for stage in expected:
            for phase in ("before", "returned", *(["entered"] if stage == "parent" else [])):
                with self.subTest(stage=stage, phase=phase):
                    record = stage_record(stage, phase)
                    record.update(timestamp=PRIVATE, threadName=PRIVATE, span={"name": PRIVATE})
                    record["fields"].update(error=PRIVATE, namespace=PRIVATE, token=PRIVATE,
                                            configuration=PRIVATE, privateKey=PRIVATE)
                    value = project(json.dumps(record), set())
                    self.assertEqual(value["grantStages"], [{"stage": stage, "phase": phase}])
                    self.assertEqual(value["fatalRuntimeFailures"], [])
                    self.assertEqual(value["grantStageInterpretation"], "concurrent-work-precludes-crash-causation")
                    self.assertNotIn(PRIVATE, json.dumps(value))

    def test_grant_stages_reject_contaminated_or_nonliteral_envelopes(self):
        records = []
        for key, values in {
            "stage": [PRIVATE, "parent " + PRIVATE, "Parent", [], {}, None, True, 1],
            "phase": [PRIVATE, "before " + PRIVATE, "BEFORE", [], {}, None, True, 1],
            "message": [PRIVATE, "Credential grant lifecycle stage " + PRIVATE, [], {}, None],
        }.items():
            for value in values:
                record = stage_record()
                record["fields"][key] = value
                records.append(record)
            record = stage_record()
            del record["fields"][key]
            records.append(record)
        for key, values in {
            "target": [PRIVATE, "kars_controller", "kars_controller::credential_grants::" + PRIVATE, [], None],
            "level": [PRIVATE, "DEBUG", "ERROR", None],
            "fields": [[], PRIVATE, None, {"message": "Credential grant lifecycle stage"}],
        }.items():
            for value in values:
                record = stage_record()
                record[key] = value
                records.append(record)
            record = stage_record()
            del record[key]
            records.append(record)
        records.extend([stage_record(stage, "entered") for stage in GRANT_STAGES - {"parent"}])
        records.extend([[], None, {"message": "Credential grant lifecycle stage", "stage": "parent", "phase": "before"}])
        raw = "\n".join(json.dumps(record) for record in records) + "\n{malformed " + PRIVATE
        value = project(raw, set())
        self.assertEqual(value["grantStages"], [])
        self.assertEqual(value["fatalRuntimeFailures"], [])
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_grant_stage_order_duplicates_and_final_32_cap(self):
        events = [stage_record(stage, phase)
                  for _ in range(9)
                  for stage in ("parent", "observer-ensure")
                  for phase in ("before", "returned")]
        events.extend([stage_record("writer-release", "before")] * 2)
        raw = "\n".join(json.dumps(event) + "\n" + PRIVATE for event in events)
        value = project(raw, set())
        expected = [{key: event["fields"][key] for key in ("stage", "phase")} for event in events]
        self.assertEqual(value["grantStages"], expected[-32:])
        self.assertEqual(value["grantStages"][-1], value["grantStages"][-2])
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_grant_stages_do_not_relax_log_byte_or_line_bounds(self):
        record = json.dumps(stage_record())
        self.assertEqual(project(record + "\n" + "ignored\n" * 512, set())["grantStages"], [])
        with self.assertRaises(Failure):
            project("é" * 65537, set())
        value = project(record + "\n" + "x" * (131072 - len(record) - 1), set())
        self.assertEqual(value["grantStages"], [{"stage": "parent", "phase": "before"}])

    def test_progress_is_not_fatal_evidence_and_fatal_does_not_imply_a_stage(self):
        raw = json.dumps(stage_record("observer-ensure"))
        self.assertEqual(project(raw, set())["fatalRuntimeFailures"], [])
        fatal = "fatal runtime error: stack overflow, aborting"
        self.assertEqual(project(fatal, set())["grantStages"], [])
        value = project(raw + "\n" + fatal, set())
        self.assertEqual(value["fatalRuntimeFailures"], ["stack-overflow"])
        self.assertEqual(value["grantStages"], [{"stage": "observer-ensure", "phase": "before"}])
        self.assertEqual(value["grantStageInterpretation"], "concurrent-work-precludes-crash-causation")
        self.assertNotIn("cause", value)

    def test_stage_histories_remain_separate_between_processes(self):
        f = fixture()
        f.objects[POD]["status"]["containerStatuses"][0]["state"] = {"running": {}}
        logs = [json.dumps(stage_record("parent", "returned")),
                json.dumps(stage_record("writer-release")) + "\nfatal runtime error: stack overflow, aborting"]
        with patch("controller_failure_diagnostics.command", side_effect=logs):
            value = collect(SimpleNamespace(admin=f))
        self.assertTrue(value["available"])
        pod = value["pods"][0]
        self.assertEqual(pod["logs"]["grantStages"], [{"stage": "parent", "phase": "returned"}])
        self.assertEqual(pod["logs"]["fatalRuntimeFailures"], [])
        self.assertEqual(pod["previousLogs"]["grantStages"], [{"stage": "writer-release", "phase": "before"}])
        self.assertEqual(pod["previousLogs"]["fatalRuntimeFailures"], ["stack-overflow"])

    def test_replaced_or_restarted_process_discards_even_valid_stage_history(self):
        for field in ("uid", "resourceVersion"):
            with self.subTest(field=field):
                f = fixture()
                def changed(*args, **kwargs):
                    f.objects[POD]["metadata"][field] = "changed"
                    f.objects[POD]["status"]["containerStatuses"][0]["restartCount"] += 1
                    return json.dumps(stage_record())
                with patch("controller_failure_diagnostics.command", side_effect=changed):
                    value = collect(SimpleNamespace(admin=f))
                self.assertFalse(value["available"])
                self.assertNotIn("pods", value)
                self.assertNotIn("grantStages", value)

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
