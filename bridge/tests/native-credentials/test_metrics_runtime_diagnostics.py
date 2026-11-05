# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import ast
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from credential_diagnostics import (
    GOVERNED_CATEGORIES, METRICS_SCRAPE_ERRORS, condition_category, metrics_log_categories, metrics_runtime,
)
from native_api import CORE, Failure, core, resource
from test_observation_diagnostics import Fixture

PRIVATE = "DO-NOT-EMIT-TOKENS-CERTIFICATES-ENDPOINTS-OR-ERROR-BODIES"
POD = core(CORE, "pods", "kars-metrics-server-pod")
DEPLOYMENT = resource(CORE, "deployments", "kars-metrics-server", "/apis/apps/v1")
REPLICA_SET = resource(CORE, "replicasets", "kars-metrics-server-rs", "/apis/apps/v1")


def log(text=PRIVATE, *, file="scraper", message=None):
    return (f'E1105 14:03:00.123456       1 {file}.go:149] '
            + (message or f'"Failed to scrape node" err="{text}" node="{PRIVATE}"'))


def fixture():
    value = Fixture()
    for path, obj in list(value.objects.items()):
        if "kars-controller" not in path:
            continue
        converted = json.loads(json.dumps(obj).replace("kars-controller", "kars-metrics-server"))
        if converted.get("kind") == "Pod":
            converted["metadata"]["labels"] = {"app": "kars-metrics-server", "kars.azure.com/metrics-managed": "true"}
        if "/deployments/" in path:
            converted["spec"]["template"]["metadata"]["labels"] = {
                "app": "kars-metrics-server", "kars.azure.com/metrics-managed": "true"}
        value.objects[path.replace("kars-controller", "kars-metrics-server")] = converted
    value.objects[POD]["status"] = {"containerStatuses": [{
        "name": "metrics-server", "ready": False, "restartCount": 0,
        "state": {"running": {"startedAt": PRIVATE}}, "lastState": {},
    }]}
    return value


class MetricsRuntimeDiagnosticsTests(unittest.TestCase):
    def test_scrape_and_storage_failures_retain_only_closed_categories(self):
        for text, category in METRICS_SCRAPE_ERRORS.items():
            result = metrics_log_categories(log(PRIVATE + " " + text + " " + PRIVATE))
            self.assertEqual(result["categories"], [category])
            self.assertEqual(result["scrapeErrorRecords"], 1)
            self.assertNotIn(PRIVATE, json.dumps(result))
        raw = log(file="server", message='"Failed probe" probe="metric-storage-ready" err="no metrics to serve"')
        self.assertEqual(metrics_log_categories(raw)["categories"], ["metric-storage-not-ready"])
        self.assertEqual(metrics_log_categories(log())["categories"], ["kubelet-scrape-unclassified"])
        for raw in (PRIVATE, "x509: certificate signed by unknown authority", log(file="other"),
                    log(message='"Private message" err="certificate signed by unknown authority"')):
            self.assertEqual(metrics_log_categories(raw)["categories"], [])
        self.assertEqual(metrics_log_categories("")["scrapeErrorRecords"], 0)

    def test_tail_and_byte_bounds_are_enforced(self):
        raw = log("certificate signed by unknown authority") + "\n" + "\n".join([PRIVATE] * 512)
        self.assertEqual(metrics_log_categories(raw)["categories"], [])
        for value in (None, "x" * 131073, "é" * 65537):
            with self.assertRaises(Failure):
                metrics_log_categories(value)

    def test_running_unready_pod_has_lineage_fenced_scrape_evidence_without_private_values(self):
        value = fixture()
        with patch("credential_diagnostics.command", return_value=log("x509: " + PRIVATE + " because it doesn't contain any IP SANs")) as read:
            result = metrics_runtime(SimpleNamespace(admin=value))
        self.assertTrue(result["available"])
        pod = result["pods"][0]
        self.assertFalse(pod["ready"])
        self.assertEqual(pod["restartCount"], 0)
        self.assertEqual(pod["logs"]["categories"], ["kubelet-ip-san-missing"])
        self.assertEqual(pod["previousLogs"], {"available": False, "category": "no-previous-process"})
        self.assertEqual(read.call_args.args, (
            "kubectl", "logs", "-n", CORE, "kars-metrics-server-pod", "-c", "metrics-server",
            "--tail=512", "--limit-bytes=131072", "--request-timeout=10s"))
        self.assertEqual(read.call_args.kwargs, {"timeout": 15})
        self.assertNotIn(PRIVATE, json.dumps(result))

    def test_restart_logs_are_projected_and_read_errors_are_never_published(self):
        value = fixture()
        state = value.objects[POD]["status"]["containerStatuses"][0]
        state.update(restartCount=1, state={"waiting": {"reason": "CrashLoopBackOff", "message": PRIVATE}})
        with patch("credential_diagnostics.command", return_value=log()) as read:
            result = metrics_runtime(SimpleNamespace(admin=value))
        self.assertTrue(result["available"])
        pod = result["pods"][0]
        self.assertFalse(pod["logs"]["available"])
        self.assertEqual(pod["previousLogs"]["categories"], ["kubelet-scrape-unclassified"])
        self.assertIn("--previous", read.call_args.args)
        for error in (Failure(PRIVATE), TimeoutError(PRIVATE)):
            with patch("credential_diagnostics.command", side_effect=error):
                result = metrics_runtime(SimpleNamespace(admin=value))
            self.assertEqual(result["pods"][0]["previousLogs"],
                             {"available": False, "category": "log-read-unavailable"})
            self.assertNotIn(PRIVATE, json.dumps(result))

    def test_foreign_lineage_and_post_read_identity_drift_discard_evidence(self):
        value = fixture()
        value.objects[REPLICA_SET]["metadata"]["ownerReferences"][0]["uid"] = "foreign"
        with patch("credential_diagnostics.command") as read:
            result = metrics_runtime(SimpleNamespace(admin=value))
        read.assert_not_called()
        self.assertFalse(result["available"])
        for path in (POD, DEPLOYMENT, REPLICA_SET, core(CORE, "serviceaccounts", "kars-metrics-server")):
            value = fixture()
            def drift(*_args, **_kwargs):
                value.objects[path]["metadata"]["resourceVersion"] = "2"
                return log("certificate signed by unknown authority")
            with patch("credential_diagnostics.command", side_effect=drift):
                result = metrics_runtime(SimpleNamespace(admin=value))
            self.assertEqual(result, {"available": False, "diagnosticOnly": True,
                                      "category": "identity-or-state-unavailable"})

    def test_invalid_readiness_and_missing_status_are_not_success(self):
        for state in ({}, {"containerStatuses": []}, {"containerStatuses": [
            {"name": "metrics-server", "ready": PRIVATE, "restartCount": 0}]}):
            value = fixture()
            value.objects[POD]["status"] = state
            with patch("credential_diagnostics.command") as read:
                result = metrics_runtime(SimpleNamespace(admin=value))
            read.assert_not_called()
            self.assertFalse(result["available"])
            self.assertNotIn(PRIVATE, json.dumps(result))

    def test_maintained_failure_capture_calls_metrics_and_never_reads_secret_values(self):
        directory = Path(__file__).resolve().parent
        module = ast.parse((directory / "run.py").read_text())
        diagnostics = next(node for node in module.body if isinstance(node, ast.FunctionDef) and node.name == "diagnostics")
        self.assertTrue(any(isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                            and node.func.id == "metrics_runtime" for node in ast.walk(diagnostics)))
        value = fixture()
        reads = []
        def get(path):
            reads.append(path)
            return value.get(path)
        with patch("credential_diagnostics.command", return_value=""):
            result = metrics_runtime(SimpleNamespace(admin=SimpleNamespace(get=get)))
        self.assertTrue(result["available"])
        self.assertFalse(any("/secrets" in path or "/configmaps" in path for path in reads))

    def test_task_recheck_subcategories_remain_closed_and_reject_suffixes(self):
        categories = {"target_changed", "target_bundle_reference", "target_task_authority",
                      "target_task_metadata", "target_task_execution", "target_task_caller"}
        self.assertTrue(categories <= GOVERNED_CATEGORIES)
        for category in categories:
            message = f"governed credential source or operator grant is unavailable [{category}; code=None]"
            self.assertEqual(condition_category(message), "source_or_grant:" + category)
            self.assertEqual(condition_category(message + PRIVATE), "unclassified")


if __name__ == "__main__":
    unittest.main()
