# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import ast
import copy
import json
from pathlib import Path
import types
import unittest

from credential_diagnostics import (
    CATEGORIES, NAMESPACE_CONDITIONS, PREFIX, condition_category, metrics_discovery, namespace_retirement, runtime_scope,
)
from native_api import Failure

PRIVATE = "DO-NOT-EMIT-PRIVATE-VALUES-OR-API-BODIES"


class CredentialDiagnosticsTests(unittest.TestCase):
    def test_only_exact_known_messages_become_fixed_categories(self):
        for message, category in CATEGORIES.items():
            self.assertEqual(condition_category(message), category)
            self.assertEqual(condition_category("CredentialSourceUnavailable: " + message), category)
            self.assertEqual(condition_category(message + PRIVATE), "unclassified")
        for message in (None, {}, PRIVATE, "prefix" + next(iter(CATEGORIES))):
            self.assertEqual(condition_category(message), "unclassified")

    def test_governed_failure_details_only_retain_known_categories_and_http_codes(self):
        prefix = "CredentialSourceUnavailable: governed credential source or operator grant is unavailable "
        self.assertEqual(condition_category(prefix + "[target_api; code=Some(404)]"),
                         "source_or_grant:target_api:404")
        self.assertEqual(condition_category(prefix + "[bundle_owner; code=None]"),
                         "source_or_grant:bundle_owner")
        for suffix in ("[target_api; code=Some(999)]", "[private_value; code=None]",
                       "[target_api; code=Some(404)]" + PRIVATE, PRIVATE):
            self.assertEqual(condition_category(prefix + suffix), "unclassified")

    def test_scope_capture_reports_only_booleans_and_fixed_states(self):
        sandbox = {"metadata": {"name": "test", "namespace": "work", "uid": "sandbox",
                               "annotations": {"kars.azure.com/namespace-uid": "runtime"}}}
        namespace = {"metadata": {"uid": "runtime", "annotations": {
            PREFIX + "state": "Qualified", PREFIX + "epoch": PRIVATE,
            "kars.azure.com/sandbox-namespace": "work", "kars.azure.com/sandbox-uid": "sandbox",
            "unrelated-private": PRIVATE,
        }}}
        grant = {"metadata": {"generation": 2}, "spec": {"privateActivation": {"namespaces": [
            {"namespace": {"name": "kars-test", "uid": "runtime"}, "epoch": PRIVATE},
        ]}}, "status": {"observedGeneration": 2, "conditions": [
            {"type": "WriterReady", "status": "True", "message": PRIVATE},
            {"type": "PrivateConsumptionReady", "status": "True"},
        ]}}
        responses = {"/api/v1/namespaces/kars-test": namespace,
                     "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace": grant}
        setup = types.SimpleNamespace(admin=types.SimpleNamespace(optional=responses.__getitem__))
        result = runtime_scope(setup, sandbox)
        for key in ("available", "namespacePresent", "grantPresent", "namespaceUidMatches",
                    "namespaceOwnerMatches", "grantScopeIncluded", "epochMatches",
                    "writerReady", "privateConsumptionReady"):
            self.assertIs(result[key], True, key)
        self.assertNotIn(PRIVATE, json.dumps(result))
        stale = copy.deepcopy(grant)
        stale["status"]["observedGeneration"] = 1
        responses[next(path for path in responses if path.startswith("/apis/"))] = stale
        result = runtime_scope(setup, sandbox)
        self.assertFalse(result["writerReady"])
        self.assertFalse(result["privateConsumptionReady"])
        stale["spec"]["privateActivation"]["namespaces"].clear()
        result = runtime_scope(setup, sandbox)
        self.assertFalse(result["grantScopeIncluded"])
        self.assertFalse(result["epochMatches"])
        self.assertNotIn(PRIVATE, json.dumps(result))

    def test_missing_or_failed_metadata_never_claims_qualified_scope(self):
        sandbox = {"metadata": {"name": "test", "namespace": "work"}}
        setup = types.SimpleNamespace(admin=types.SimpleNamespace(optional=lambda _path: None))
        self.assertEqual(runtime_scope(setup, sandbox), {
            "available": True, "namespacePresent": False, "grantPresent": False,
        })
        def failed(_path):
            raise Failure(PRIVATE)
        setup.admin.optional = failed
        result = runtime_scope(setup, sandbox)
        self.assertEqual(result, {"available": False, "category": "api-unavailable"})
        self.assertNotIn(PRIVATE, json.dumps(result))


class NamespaceRetirementDiagnosticsTests(unittest.TestCase):
    def fixture(self):
        return {"metadata": {"name": "kars-native-team-principal",
                             "uid": "ef6ef911-2d71-4e82-bd70-93b5748d89f1",
                             "deletionTimestamp": PRIVATE,
                             "annotations": {"unrelated": PRIVATE},
                             "finalizers": [PRIVATE]},
                "spec": {"finalizers": ["kubernetes", PRIVATE]},
                "status": {"phase": "Terminating", "conditions": [
                    {"type": kind, "status": "True", "reason": sorted(reasons)[0],
                     "message": PRIVATE, "lastTransitionTime": PRIVATE}
                    for kind, reasons in NAMESPACE_CONDITIONS.items()
                ]}}

    def collect(self, objects):
        paths = []
        def get(path):
            paths.append(path)
            return {"items": objects}
        result = namespace_retirement(types.SimpleNamespace(admin=types.SimpleNamespace(get=get)))
        expected = ["/api/v1/namespaces"]
        if result["available"]:
            expected.append("/apis/apiregistration.k8s.io/v1/apiservices/v1beta1.metrics.k8s.io")
        self.assertEqual(paths, expected)
        self.assertNotIn(PRIVATE, json.dumps(result))
        return result

    def test_discovery_dependency_is_closed_and_does_not_emit_api_messages(self):
        item = self.fixture()
        item["status"]["conditions"][0].update({"type": "NamespaceDeletionDiscoveryFailure", "status": "True",
            "message": "metrics.k8s.io/v1beta1: " + PRIVATE})
        def get(path):
            if path == "/api/v1/namespaces":
                return {"items": [item]}
            return {"spec": {"group": "metrics.k8s.io"}, "status": {"conditions": [
                {"type": "Available", "status": "False", "reason": "MissingEndpoints", "message": PRIVATE}]}}
        result = namespace_retirement(types.SimpleNamespace(admin=types.SimpleNamespace(get=get)))
        self.assertTrue(result["namespaces"][0]["metricsDiscoveryMentioned"])
        self.assertEqual(result["metricsApi"], {"available": True, "registered": True,
                                              "status": "False", "reason": "MissingEndpoints"})
        self.assertNotIn(PRIVATE, json.dumps(result))
        def failing_metrics(path):
            if path == "/api/v1/namespaces":
                return {"items": [item]}
            raise Failure(PRIVATE)
        result = namespace_retirement(types.SimpleNamespace(admin=types.SimpleNamespace(get=failing_metrics)))
        self.assertTrue(result["available"])
        self.assertFalse(result["metricsApi"]["available"])
        self.assertNotIn(PRIVATE, json.dumps(result))

    def test_metrics_api_unknown_fields_and_failure_bodies_are_redacted(self):
        for payload in ({"status": {"conditions": [{"type": "Available", "status": PRIVATE, "reason": PRIVATE}]}},
                        {"status": {"conditions": PRIVATE}}, {"status": {"conditions": [{"type": "Available", "reason": {}}]}}):
            result = metrics_discovery(types.SimpleNamespace(admin=types.SimpleNamespace(get=lambda _path: payload)))
            self.assertNotIn(PRIVATE, json.dumps(result))
            if result["available"]:
                self.assertEqual(result["status"], "OtherOrAbsent")
                self.assertEqual(result["reason"], "OtherOrAbsent")

    def test_orphaned_runtime_retains_lifecycle_without_sandbox_or_private_values(self):
        result = self.collect([self.fixture()])
        self.assertTrue(result["available"])
        self.assertTrue(result["diagnosticOnly"])
        value = result["namespaces"][0]
        self.assertTrue(value["deleting"])
        self.assertEqual(value["phase"], "Terminating")
        self.assertEqual(value["metadataFinalizers"], {"known": [], "otherCount": 1})
        self.assertEqual(value["specFinalizers"], {"known": ["kubernetes"], "otherCount": 1})
        self.assertEqual({item["type"] for item in value["conditions"]}, set(NAMESPACE_CONDITIONS))
        for condition in value["conditions"]:
            self.assertIn(condition["reason"], NAMESPACE_CONDITIONS[condition["type"]])

    def test_known_condition_reasons_are_exact_and_type_bound(self):
        for kind, reasons in NAMESPACE_CONDITIONS.items():
            for reason in reasons:
                item = self.fixture()
                item["status"]["conditions"] = [{"type": kind, "status": "False", "reason": reason}]
                self.assertEqual(self.collect([item])["namespaces"][0]["conditions"], [
                    {"type": kind, "status": "False", "reason": reason}])
        item["status"] = {"phase": PRIVATE, "conditions": [
            {"type": "NamespaceFinalizersRemaining", "status": PRIVATE, "reason": "DiscoveryFailed"},
            {"type": PRIVATE, "status": "True", "reason": PRIVATE},
            {"type": "NamespaceContentRemaining", "status": "Unknown", "reason": "SomeResourcesRemain" + PRIVATE},
        ]}
        value = self.collect([item])["namespaces"][0]
        self.assertEqual(value["phase"], "OtherOrAbsent")
        self.assertTrue(all(c["reason"] == "OtherOrAbsent" for c in value["conditions"]))
        self.assertEqual(value["conditions"][0]["status"], "OtherOrAbsent")
        self.assertEqual(value["conditions"][1]["type"], "OtherOrAbsent")

    def test_missing_namespaces_and_unrelated_namespaces_do_not_imply_blockers(self):
        self.assertEqual(self.collect([])["namespaces"], [])
        item = self.fixture()
        item["metadata"]["name"] = "kube-system"
        self.assertEqual(self.collect([item])["namespaces"], [])
        item["metadata"]["name"] = "kars-runtime"
        item["metadata"]["annotations"]["kars.azure.com/sandbox-name"] = PRIVATE
        item["metadata"].pop("deletionTimestamp")
        value = self.collect([item])["namespaces"][0]
        self.assertEqual(value["name"], "kars-runtime")
        self.assertFalse(value["deleting"])

    def test_output_is_bounded_and_truncation_is_explicit(self):
        items = []
        for index in range(70):
            item = self.fixture()
            item["metadata"]["name"] = f"kars-native-{index:03d}"
            item["status"]["conditions"] *= 5
            item["spec"]["finalizers"] *= 100
            items.append(item)
        result = self.collect(items)
        self.assertTrue(result["truncated"])
        self.assertEqual(len(result["namespaces"]), 64)
        self.assertTrue(all(item["conditionsTruncated"] for item in result["namespaces"]))
        self.assertEqual(len(result["namespaces"][0]["conditions"]), 16)
        self.assertEqual(result["namespaces"][0]["specFinalizers"], {"known": ["kubernetes"], "otherCount": 100})

    def test_api_and_transport_errors_are_fixed_and_never_escape(self):
        for error in (Failure("403 " + PRIVATE), TimeoutError(PRIVATE), ValueError(PRIVATE)):
            def get(_path):
                raise error
            result = namespace_retirement(types.SimpleNamespace(admin=types.SimpleNamespace(get=get)))
            self.assertEqual(result, {"available": False, "diagnosticOnly": True, "category": "api-unavailable"})
            self.assertNotIn(PRIVATE, json.dumps(result))

    def test_malformed_metadata_discards_partial_projection(self):
        for field, value in (("uid", PRIVATE), ("name", "kars-native-" + PRIVATE), ("finalizers", PRIVATE)):
            item = self.fixture()
            item["metadata"][field] = value
            result = self.collect([self.fixture(), item])
            self.assertEqual(result, {"available": False, "diagnosticOnly": True, "category": "malformed-metadata"})

    def test_maintained_failure_collector_and_both_projection_reads_use_safe_diagnostics(self):
        directory = Path(__file__).resolve().parent
        module = ast.parse((directory / "run.py").read_text())
        diagnostics = next(node for node in module.body if isinstance(node, ast.FunctionDef) and node.name == "diagnostics")
        self.assertTrue(any(isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                            and node.func.id == "namespace_retirement" for node in ast.walk(diagnostics)))
        source = (directory.parents[2] / "controller/src/reconciler/credential_sources.rs").read_text()
        self.assertEqual(source.count(".map_err(diagnostics::governed)?;"), 2)
        self.assertNotIn('Error::Invalid("governed credential inputs are no longer authorized")', source)


if __name__ == "__main__":
    unittest.main()
