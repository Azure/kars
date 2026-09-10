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

    def test_refuses_unowned_kubeconfig_before_api_access(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "KUBECONFIG": "/other/config"}), \
                patch("sys.argv", ["probe", "--root", "/unused"]), \
                patch.object(probe.api, "kind_proxy") as proxy:
            with self.assertRaisesRegex(probe.ProbeFailure, "owned-kind"):
                probe.main()
        proxy.assert_not_called()


if __name__ == "__main__":
    unittest.main()
