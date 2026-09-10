# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import contextlib
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import standalone_diagnostics as diagnostic

PRIVATE = "DO-NOT-LOG-SECRET-ENV-ARGV-API-BODY"
NAMESPACE = {"metadata": {"name": "kube-system", "uid": "cluster-uid"}}


def pod():
    return {
        "metadata": {"name": "budget-money-left-pod", "namespace": "kars-budget-money-left",
                     "uid": "pod-uid", "resourceVersion": "4", "generation": 2,
                     "deletionTimestamp": "2026-09-10T00:00:00Z",
                     "annotations": {"private": PRIVATE}, "finalizers": ["kars.azure.com/fixture"],
                     "ownerReferences": [{"kind": "ReplicaSet", "name": "budget-money-left-rs", "uid": "rs-uid"}]},
        "spec": {"containers": [{"name": "router", "env": [{"name": "TOKEN", "value": PRIVATE}],
                                "args": [PRIVATE]}]},
        "status": {"phase": "Pending", "message": PRIVATE,
                   "conditions": [{"type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                                   "message": "Insufficient cpu " + PRIVATE}],
                   "containerStatuses": [{"name": "inference-router", "restartCount": 2,
                       "state": {"waiting": {"reason": "CreateContainerConfigError", "message": PRIVATE}},
                       "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137, "message": PRIVATE}}}]},
    }


class StandaloneDiagnosticTests(unittest.TestCase):
    def test_projection_retains_native_identity_reason_and_capacity_facts_only(self):
        value = diagnostic.object_status("Pod", pod(), ["kars-fixture-policy"])
        encoded = json.dumps(value)
        self.assertNotIn(PRIVATE, encoded)
        self.assertNotIn('"spec"', encoded)
        self.assertNotIn('"env"', encoded)
        self.assertNotIn('"args"', encoded)
        self.assertEqual(value["metadata"]["uid"], "pod-uid")
        self.assertEqual(value["metadata"]["generation"], 2)
        self.assertEqual(value["metadata"]["owners"][0]["uid"], "rs-uid")
        self.assertEqual(value["status"]["containerStatuses"][0]["state"]["reason"], "CreateContainerConfigError")
        self.assertIn("insufficient-cpu", value["status"]["conditions"][0]["categories"])

    def test_namespace_finalization_and_node_capacity_do_not_export_specs(self):
        value = diagnostic.object_status("Namespace", {"metadata": {"name": "kars-e2e-source"},
            "spec": {"finalizers": ["kubernetes"], "private": PRIVATE},
            "status": {"phase": "Terminating", "conditions": [{
                "type": "NamespaceDeletionDiscoveryFailure", "status": "True",
                "reason": "DiscoveryFailed", "message": "discovery failed " + PRIVATE}]}}, [])
        self.assertEqual(value["namespaceFinalizers"], ["kubernetes"])
        self.assertEqual(value["status"]["conditions"][0]["reason"], "DiscoveryFailed")
        node = diagnostic.object_status("Node", {
            "metadata": {"name": "kars-e2e-control-plane"}, "spec": {"providerID": PRIVATE},
            "status": {"capacity": {"cpu": "4", "memory": "8192Ki", "pods": "110", "private": PRIVATE}}}, [])
        self.assertEqual(node["status"]["capacity"], {"cpu": "4", "memory": "8192Ki", "pods": "110"})
        self.assertNotIn(PRIVATE, json.dumps([value, node]))

    def test_absent_native_status_arrays_do_not_destroy_the_snapshot(self):
        value = diagnostic.object_status("Pod", {
            "metadata": {"name": "pod", "finalizers": None, "ownerReferences": None},
            "status": {"conditions": None, "containerStatuses": None, "phase": None},
            "spec": {"env": PRIVATE}}, [])
        self.assertEqual(value["status"]["conditions"], [])
        self.assertEqual(value["status"]["containerStatuses"], [])
        self.assertNotIn(PRIVATE, json.dumps(value))

    def test_policy_facts_never_echo_an_api_validation_message_or_warning(self):
        value = diagnostic.facts("kars-fixture-policy: undefined field " + PRIVATE, ["kars-fixture-policy"])
        self.assertEqual(value["policies"], ["kars-fixture-policy"])
        self.assertIn("undefined-field", value["categories"])
        self.assertNotIn(PRIVATE, json.dumps(value))
        self.assertNotIn("validationMessages", value)

    def test_collector_is_get_only_and_never_requests_secrets_or_unfiltered_logs(self):
        calls = []
        def request(_port, method, path):
            calls.append((method, path))
            if path == "/api/v1/namespaces/kube-system":
                return 200, NAMESPACE
            if "validatingadmissionpolicies" in path:
                return 200, {"items": [{"metadata": {"name": "kars-fixture-policy"},
                    "spec": {"validations": [{"message": PRIVATE}]},
                    "status": {"typeChecking": {"expressionWarnings": [{"warning": "undefined field " + PRIVATE}]}}}]}
            if path.startswith("/api/v1/pods?"):
                return 200, {"items": [pod(), {**pod(), "metadata": {"name": "other", "namespace": "unrelated"}}]}
            return 200, {"items": []}
        with patch.object(diagnostic, "request", side_effect=request), \
             patch.object(diagnostic.public, "controller_stack", return_value={
                 "available": True, "current": {"panicCategories": ["nil-pointer"], "publicFrames": []}}):
            report = diagnostic.collect(1, "cluster-uid", "budget")
        self.assertTrue(report["complete"])
        self.assertEqual(len(report["resources"]), 1)
        self.assertNotIn(PRIVATE, json.dumps(report))
        self.assertTrue(all(method == "GET" and "secrets" not in path and "/log" not in path
                            for method, path in calls))

    def test_replaced_cluster_is_rejected_before_inventory(self):
        with patch.object(diagnostic, "request", return_value=(200, NAMESPACE)) as request:
            with self.assertRaises(RuntimeError):
                diagnostic.collect(1, "other-uid", "final")
        self.assertEqual(request.call_count, 1)

    def test_pagination_preserves_later_failure_events_without_exporting_cursors(self):
        calls = []
        def request(_port, _method, path):
            calls.append(path)
            if path == "/api/v1/namespaces/kube-system":
                return 200, NAMESPACE
            if path == "/api/v1/events?limit=256":
                return 200, {"items": [], "metadata": {"continue": "cursor/next"}}
            if path == "/api/v1/events?limit=256&continue=cursor%2Fnext":
                return 200, {"items": [{"metadata": {"namespace": "kars-budget-money-left"},
                    "involvedObject": {"kind": "Pod", "name": "budget-money-left", "uid": "pod"},
                    "reason": "FailedScheduling", "message": "Insufficient cpu " + PRIVATE}]}
            return 200, {"items": []}
        with patch.object(diagnostic, "request", side_effect=request), \
             patch.object(diagnostic.public, "controller_stack", return_value={"available": True}):
            report = diagnostic.collect(1, "cluster-uid", "budget")
        self.assertTrue(report["complete"])
        self.assertEqual(len(report["events"]), 1)
        self.assertIn("insufficient-cpu", report["events"][0]["categories"])
        self.assertNotIn("cursor", json.dumps(report))
        self.assertNotIn(PRIVATE, json.dumps(report))
        self.assertIn("/api/v1/events?limit=256&continue=cursor%2Fnext", calls)

    def test_excess_pages_still_report_incomplete_with_the_same_deadline(self):
        def request(_port, _method, path):
            if path == "/api/v1/namespaces/kube-system":
                return 200, NAMESPACE
            return 200, {"items": [], "metadata": {"continue": "more"}}
        with patch.object(diagnostic, "request", side_effect=request), \
             patch.object(diagnostic.public, "controller_stack", return_value={"available": True}):
            report = diagnostic.collect(1, "cluster-uid", "final")
        self.assertFalse(report["complete"])
        self.assertEqual(sum(item.get("resource") == "Event" for item in report["api"]), 4)

    def test_api_failure_remains_incomplete_without_dumping_its_body(self):
        def request(_port, _method, path):
            return (200, NAMESPACE) if path == "/api/v1/namespaces/kube-system" else (503, {"message": PRIVATE})
        with patch.object(diagnostic, "request", side_effect=request), \
             patch.object(diagnostic.public, "controller_stack", return_value={"available": False}):
            report = diagnostic.collect(1, "cluster-uid", "final")
        self.assertFalse(report["complete"])
        self.assertNotIn(PRIVATE, json.dumps(report))

    def test_diagnostic_failure_is_visible_and_never_prints_the_exception(self):
        output = io.StringIO()
        with patch.object(diagnostic, "kind_proxy", side_effect=RuntimeError(PRIVATE)), \
             patch.object(Path, "mkdir"), patch.object(Path, "write_text") as write, \
             contextlib.redirect_stdout(output):
            result = diagnostic.main(Path("."), "final", "cluster-uid")
        self.assertEqual(result, 1)
        self.assertNotIn(PRIVATE, output.getvalue() + write.call_args.args[0])
        self.assertFalse(json.loads(write.call_args.args[0])["complete"])


if __name__ == "__main__":
    unittest.main()
