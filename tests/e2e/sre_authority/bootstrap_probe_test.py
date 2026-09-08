# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import unittest

from .bootstrap_diagnostics import api_result, collect, failure_facts, object_status, policy_status
from .bootstrap_probe import builtin_documents, converted_objects, safe_controller

POLICIES = {"kars-sre-private-mounts": {"spec": {"validations": [
    {"message": "Private SRE material requires authority"}]}}}


class BootstrapProofTests(unittest.TestCase):
    def test_failure_metadata_keeps_public_cause_not_body_or_credentials(self):
        message = ('Error creating Pod: kars-sre-private-mounts evaluation failed: no such key: namespace; '
                   'token=do-not-publish argv=do-not-publish')
        result = object_status({
            "kind": "ReplicaSet", "metadata": {"name": "kars-controller-abc", "uid": "rs-uid",
                "annotations": {"credential": "do-not-publish"}},
            "spec": {"template": {"spec": {"containers": [{"env": ["do-not-publish"]}]}}},
            "status": {"replicas": 0, "conditions": [{"type": "ReplicaFailure", "status": "True",
                "reason": "FailedCreate", "message": message}]},
        }, POLICIES)
        self.assertNotIn("do-not-publish", json.dumps(result))
        self.assertEqual(result["conditions"][0]["reason"], "FailedCreate")
        self.assertEqual(result["conditions"][0]["policies"], ["kars-sre-private-mounts"])
        self.assertEqual(result["conditions"][0]["missingFields"], ["namespace"])
        self.assertNotIn("do-not-publish", json.dumps(api_result(
            403, {"kind": "Status", "reason": "Forbidden", "message": message, "details": "do-not-publish"}, POLICIES)))

    def test_type_warning_scope_is_known_public_policy_field_only(self):
        obj = {"metadata": {"name": "kars-sre-private-mounts", "generation": 3},
               "status": {"observedGeneration": 3, "typeChecking": {"expressionWarnings": [
                   {"fieldRef": "spec.variables[0].expression", "warning": "undefined field namespace"},
                   {"fieldRef": "spec.containers[0].env", "warning": "do-not-publish"},
               ]}}}
        result = policy_status(obj, POLICIES)
        self.assertTrue(result["typeChecked"])
        self.assertEqual(len(result["warnings"]), 1)
        self.assertNotIn("do-not-publish", json.dumps(result))
        obj["metadata"]["name"] = "unrelated-policy"
        with self.assertRaises(AssertionError):
            policy_status(obj, POLICIES)

    def test_no_scheduling_pulls_or_execution_but_original_controller_shape_remains(self):
        original = {"kind": "Deployment", "metadata": {"name": "kars-controller"},
                    "spec": {"replicas": 0, "template": {"spec": {
                        "serviceAccountName": "kars-controller", "automountServiceAccountToken": True,
                        "containers": [{"name": "controller", "image": "original", "env": [{"name": "X", "value": "Y"}]}],
                        "initContainers": [{"name": "init", "image": "original-init"}],
                    }}}}
        before = copy.deepcopy(original)
        result = safe_controller(original)
        self.assertEqual(original, before)
        pod = result["spec"]["template"]["spec"]
        self.assertEqual(result["spec"]["replicas"], 1)
        self.assertEqual(pod["serviceAccountName"], "kars-controller")
        self.assertEqual(pod["schedulerName"], "kars-e2e-admission-never-schedule")
        self.assertEqual(pod["containers"][0]["env"], [{"name": "X", "value": "Y"}])
        self.assertTrue(all(c["imagePullPolicy"] == "Never" and c["image"].startswith("registry.invalid/")
                            for c in pod["containers"] + pod["initContainers"]))
        with self.assertRaises(AssertionError):
            safe_controller({"kind": "Deployment", "metadata": {"name": "unrelated"}})

    def test_chart_selection_never_executes_other_workloads_or_loads_secret_data(self):
        rendered = "---\nkind: Secret\nmetadata:\n  name: private\nstringData:\n  token: do-not-publish\n"
        rendered += "---\nkind: Job\nmetadata:\n  name: execute\n---\nkind: ValidatingAdmissionPolicy\nmetadata:\n  name: public\n"
        result = builtin_documents(rendered)
        self.assertNotIn("do-not-publish", result)
        self.assertNotIn("kind: Job", result)
        self.assertIn("kind: ValidatingAdmissionPolicy", result)

    def test_kubectl_multiple_json_objects_and_list_are_both_parsed(self):
        first = {"kind": "ServiceAccount", "metadata": {"name": "kars-controller"}}
        second = {"kind": "ValidatingAdmissionPolicy", "metadata": {"name": "public"}}
        self.assertEqual(converted_objects(json.dumps(first) + "\n" + json.dumps(second)), [first, second])
        self.assertEqual(converted_objects(json.dumps({"kind": "List", "items": [first, second]})), [first, second])
        with self.assertRaises(RuntimeError):
            converted_objects(json.dumps({"kind": "Secret", "data": "do-not-publish"}))

    def test_collection_tracks_real_uid_chain_without_logging_other_pods(self):
        def request(_port, _method, path):
            if path.endswith("/deployments"):
                return 200, {"items": [{"metadata": {"name": "kars-controller", "uid": "dep"}}]}
            if path.endswith("/replicasets"):
                return 200, {"items": [{"metadata": {"name": "kars-controller-rs", "uid": "rs",
                    "ownerReferences": [{"uid": "dep"}]}, "status": {"replicas": 0}}]}
            if path.endswith("/pods"):
                return 200, {"items": [{"metadata": {"name": "created", "uid": "pod",
                    "ownerReferences": [{"uid": "rs"}]}},
                    {"metadata": {"name": "unrelated", "uid": "other"}, "spec": {"private": "do-not-publish"}}]}
            return 200, {"items": []}
        result = collect(1, {}, request)
        self.assertEqual([o["kind"] for o in result["workloads"]], ["Deployment", "ReplicaSet", "Pod"])
        self.assertNotIn("do-not-publish", json.dumps(result))
        self.assertNotIn("unrelated", json.dumps(result))

    def test_failure_diagnostics_precede_teardown_without_pod_spec_or_log_dump(self):
        source = (Path(__file__).resolve().parents[1] / "run.sh").read_text()
        install = source.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        self.assertIn("sre_authority.bootstrap_probe --diagnostics-only", install)
        self.assertNotIn("kubectl describe pod", install)
        self.assertNotIn("kubectl logs", install)
        self.assertIn("return 1", install)


if __name__ == "__main__":
    unittest.main()
