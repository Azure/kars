# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
import os
import unittest
from unittest.mock import patch

import receipt_log_rotation as probe


def head():
    return {
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": probe.HEAD, "namespace": probe.NS, "uid": "head-uid",
                     "resourceVersion": "10", "annotations": {"operator-note": "retain"}},
        "data": {"chain.json": json.dumps([{"seq": 0, "entryHash": "original-root"}])},
    }


def status(code, reason, name=probe.HEAD):
    return {"apiVersion": "v1", "kind": "Status", "status": "Failure",
            "code": code, "reason": reason,
            "details": {"name": name, "kind": "ConfigMap"}}


class PaddingTests(unittest.TestCase):
    def test_pads_only_json_whitespace_and_keeps_actual_uid_rv_metadata(self):
        original = head()
        entries = json.loads(original["data"]["chain.json"])
        calls = []

        def request(_port, method, path, obj=None):
            calls.append((method, path, copy.deepcopy(obj)))
            if method == "GET":
                return 200, copy.deepcopy(original)
            self.assertEqual(method, "PUT")
            self.assertEqual(obj["metadata"], original["metadata"])
            self.assertEqual(json.loads(obj["data"]["chain.json"]), entries)
            self.assertEqual(len(obj["data"]["chain.json"].encode()), probe.THRESHOLD)
            self.assertEqual(obj["data"]["chain.json"].rstrip(), original["data"]["chain.json"])
            saved = copy.deepcopy(obj)
            saved["metadata"]["resourceVersion"] = "11"
            return 200, saved

        with patch.object(probe.api, "request", side_effect=request), patch.object(
                probe, "cli", return_value={"intact": True, "entries": entries}):
            saved, prefix = probe.pad_head("/unused", 1, original)
        self.assertEqual(prefix, entries)
        self.assertEqual(saved["metadata"]["uid"], "head-uid")
        self.assertEqual([c[0] for c in calls], ["GET", "PUT"])

    def test_only_exact_conflicts_retry_with_fresh_resource_version(self):
        for code in (403, 409, 422, 500):
            with self.subTest(code=code):
                original = head()
                calls = []

                def request(_port, method, _path, obj=None):
                    calls.append((method, copy.deepcopy(obj)))
                    if method == "GET":
                        current = copy.deepcopy(original)
                        current["metadata"]["resourceVersion"] = str(len(calls))
                        return 200, current
                    return code, status(code, "Conflict" if code == 409 else "Forbidden")

                with patch.object(probe.api, "request", side_effect=request), patch.object(
                        probe, "cli", return_value={"intact": True, "entries": json.loads(
                            original["data"]["chain.json"])}):
                    with self.assertRaises(probe.ProbeFailure):
                        probe.pad_head("/unused", 1, original)
                writes = [c[1] for c in calls if c[0] == "PUT"]
                self.assertEqual(len(writes), 3 if code == 409 else 1)
                if code == 409:
                    self.assertEqual([w["metadata"]["resourceVersion"] for w in writes],
                                     ["1", "3", "5"])

    def test_no_write_on_replaced_modified_sealed_foreign_or_unverified_head(self):
        for fault in ("uid", "prefix", "immutable", "owner", "cli", "oversized"):
            with self.subTest(fault=fault):
                original = head()
                current = copy.deepcopy(original)
                if fault == "uid":
                    current["metadata"]["uid"] = "replacement"
                elif fault == "prefix":
                    current["data"]["chain.json"] = '[{"seq":0,"entryHash":"different"}]'
                elif fault == "immutable":
                    current["immutable"] = True
                elif fault == "owner":
                    current["metadata"]["ownerReferences"] = [{"uid": "foreign"}]
                elif fault == "oversized":
                    current["data"]["chain.json"] += " " * probe.THRESHOLD
                observed = {"intact": fault != "cli",
                            "entries": json.loads(current["data"]["chain.json"])}
                with patch.object(probe.api, "request", return_value=(200, current)) as request, \
                        patch.object(probe, "cli", return_value=observed):
                    with self.assertRaises(probe.ProbeFailure):
                        probe.pad_head("/unused", 1, original)
                self.assertEqual([c.args[1] for c in request.call_args_list], ["GET"])

    def test_failed_padding_ack_is_not_accepted(self):
        original = head()
        with patch.object(probe.api, "request", side_effect=[
            (200, original), (200, original),
        ]), patch.object(probe, "cli", return_value={
            "intact": True, "entries": json.loads(original["data"]["chain.json"]),
        }):
            with self.assertRaisesRegex(probe.ProbeFailure, "padding-ack"):
                probe.pad_head("/unused", 1, original)


class RetirementTests(unittest.TestCase):
    def task(self):
        return {"kind": "KarsTask", "metadata": {
            "name": "owned-task", "namespace": probe.NS, "uid": "task-uid",
            "resourceVersion": "11", "generation": 1,
        }, "spec": {"objective": "original"}}

    def test_deletes_only_owned_unchanged_task_using_both_preconditions(self):
        task = self.task()
        missing = status(404, "NotFound", "owned-task")
        with patch.object(probe.api, "request", side_effect=[
            (200, task), (202, task), (404, missing),
        ]) as request:
            probe.cleanup_task(1, task)
        self.assertEqual(request.call_args_list[1].args[1], "DELETE")
        self.assertEqual(request.call_args_list[1].args[3]["preconditions"],
                         {"uid": "task-uid", "resourceVersion": "11"})

    def test_refuses_cleanup_of_replacement_or_changed_intent(self):
        for key in ("uid", "generation", "spec"):
            with self.subTest(key=key):
                task = self.task()
                current = copy.deepcopy(task)
                if key == "spec":
                    current["spec"]["objective"] = "changed"
                else:
                    current["metadata"][key] = "changed"
                with patch.object(probe.api, "request", return_value=(200, current)) as request:
                    with self.assertRaises(probe.ProbeFailure):
                        probe.cleanup_task(1, task)
                self.assertEqual(request.call_count, 1)

    def test_does_not_treat_arbitrary_delete_error_as_retirement(self):
        task = self.task()
        with patch.object(probe.api, "request", side_effect=[
            (200, task), (403, status(403, "Forbidden", "owned-task")),
        ]) as request:
            with self.assertRaisesRegex(probe.ProbeFailure, "cleanup-delete"):
                probe.cleanup_task(1, task)
        self.assertEqual(request.call_count, 2)


class DenialTests(unittest.TestCase):
    def test_requires_actual_data_immutability_denial(self):
        body = status(422, "Invalid")
        body["details"]["causes"] = [{
            "field": "data", "reason": "FieldValueForbidden",
            "message": "field is immutable when immutable is set",
        }]
        self.assertTrue(probe.immutable_denial(422, body))
        for field, value in (("field", "metadata.uid"), ("reason", "FieldValueInvalid"),
                             ("message", "different validation error")):
            altered = copy.deepcopy(body)
            altered["details"]["causes"][0][field] = value
            self.assertFalse(probe.immutable_denial(422, altered))
        self.assertFalse(probe.immutable_denial(403, status(403, "Forbidden")))
        self.assertFalse(probe.immutable_denial(422, None))

    def test_refuses_non_ci_execution_before_api_access(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
                patch("sys.argv", ["probe", "--root", "/unused"]), \
                patch.object(probe.api, "kind_proxy") as proxy:
            with self.assertRaisesRegex(probe.ProbeFailure, "ci-only"):
                probe.main()
        proxy.assert_not_called()


class CheckpointConvergenceTests(unittest.TestCase):
    def report(self, detail=None):
        return {"ok": detail is None, "checks": [
            {"name": "signature", "ok": True},
            {"name": "inclusion", "ok": True},
            {"name": "checkpoint", "ok": detail is None, "detail": detail or "valid"},
        ]}

    def lag(self):
        return self.report(
            "checkpoint (size 1) diverges from the live log (size 2)"
            " \u2014 history may have been rewritten")

    def test_retries_only_precise_authenticated_checkpoint_lag(self):
        self.assertTrue(probe.checkpoint_lag(self.lag()))
        for reason in ("signed checkpoint signature INVALID", "untrusted key", "other"):
            self.assertFalse(probe.checkpoint_lag(self.report(reason)))
        invalid = self.lag()
        invalid["checks"][0]["ok"] = False
        self.assertFalse(probe.checkpoint_lag(invalid))
        invalid = self.lag()
        invalid["checks"][1]["ok"] = False
        self.assertFalse(probe.checkpoint_lag(invalid))
        for size in (2, 3):
            self.assertFalse(probe.checkpoint_lag(self.report(
                f"checkpoint (size {size}) diverges from the live log (size 2)"
                " \u2014 history may have been rewritten")))

    def test_actual_cli_nonzero_contract_accepts_only_lag(self):
        for report, accepted in ((self.lag(), True), (self.report("signature INVALID"), False)):
            result = type("Result", (), {"returncode": 2, "stdout": json.dumps(report)})()
            with patch.object(probe.subprocess, "run", return_value=result):
                if accepted:
                    self.assertIsNone(probe.cli("/unused", "verify", "task", allow_checkpoint_lag=True))
                else:
                    with self.assertRaises(probe.ProbeFailure):
                        probe.cli("/unused", "verify", "task", allow_checkpoint_lag=True)

    def checkpoint(self):
        obj = head()
        obj["metadata"]["name"] = "kars-receipt-checkpoint"
        obj["metadata"]["uid"] = "checkpoint-uid"
        obj["data"] = {k: "value" for k in ("treeSize", "rootHash", "keyId", "signature", "note")}
        return obj

    def test_append_visible_before_checkpoint_converges_without_new_deadline(self):
        task = RetirementTests().task()
        prefix = [{"seq": 0, "entryHash": "original-root"}]
        log = {"intact": True, "entries": prefix + [{"seq": 1}]}
        responses = [log, None, log, self.report()]
        with patch.object(probe, "read", return_value=task), \
                patch.object(probe.api, "request", return_value=(200, self.checkpoint())), \
                patch.object(probe, "cli", side_effect=responses) as cli, \
                patch.object(probe.time, "monotonic", side_effect=[1, 2]), \
                patch.object(probe.time, "sleep"):
            result = probe.await_verified_receipt("/unused", 1, task, prefix, 3)
        self.assertEqual(result, log)
        self.assertEqual(cli.call_count, 4)

    def test_forbidden_checkpoint_read_is_immediately_fatal(self):
        task = RetirementTests().task()
        with patch.object(probe, "read", return_value=task), \
                patch.object(probe, "cli", return_value={"intact": True, "entries": []}) as cli, \
                patch.object(probe.api, "request", return_value=(403, status(403, "Forbidden"))), \
                patch.object(probe.time, "monotonic", return_value=1):
            with self.assertRaisesRegex(probe.ProbeFailure, "checkpoint-read"):
                probe.await_verified_receipt("/unused", 1, task, [], 2)
        self.assertEqual(cli.call_count, 1)

    def test_signature_failure_is_not_retried_as_publication_lag(self):
        task = RetirementTests().task()
        with patch.object(probe, "read", return_value=task), \
                patch.object(probe.api, "request", return_value=(200, self.checkpoint())), \
                patch.object(probe, "cli", side_effect=[
                    {"intact": True, "entries": []}, probe.ProbeFailure("signature-invalid"),
                ]) as cli, patch.object(probe.time, "monotonic", return_value=1):
            with self.assertRaisesRegex(probe.ProbeFailure, "signature-invalid"):
                probe.await_verified_receipt("/unused", 1, task, [], 2)
        self.assertEqual(cli.call_count, 2)

    def test_lag_exhausts_original_deadline_without_overwriting_checkpoint(self):
        task = RetirementTests().task()
        with patch.object(probe, "read", return_value=task), \
                patch.object(probe.api, "request", return_value=(200, self.checkpoint())) as api, \
                patch.object(probe, "cli", side_effect=[{"intact": True, "entries": []}, None]), \
                patch.object(probe.time, "monotonic", side_effect=[1, 3]), \
                patch.object(probe.time, "sleep"):
            with self.assertRaisesRegex(probe.ProbeFailure, "convergence-deadline"):
                probe.await_verified_receipt("/unused", 1, task, [], 3)
        self.assertEqual([c.args[1] for c in api.call_args_list], ["GET"])

    def test_refuses_unowned_kubeconfig_before_api_access(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "KUBECONFIG": "/other/config"}), \
                patch("sys.argv", ["probe", "--root", "/unused"]), \
                patch.object(probe.api, "kind_proxy") as proxy:
            with self.assertRaisesRegex(probe.ProbeFailure, "owned-kind"):
                probe.main()
        proxy.assert_not_called()


if __name__ == "__main__":
    unittest.main()
