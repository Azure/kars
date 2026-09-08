# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from .bootstrap_diagnostics import api_result, collect, control_plane_status, failure_facts, object_status, policy_status, public_stack_facts
from .bootstrap_probe import builtin_documents, converted_objects, exercise, safe_controller

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

    def test_control_plane_metadata_and_go_frames_never_publish_logs_or_arguments(self):
        pod = {"metadata": {"name": "kube-controller-manager-kars-e2e-control-plane"},
            "spec": {"containers": [{"args": ["do-not-publish"]}]},
            "status": {"containerStatuses": [{"name": "kube-controller-manager", "ready": False,
                "restartCount": 4, "state": {"waiting": {"message": "do-not-publish"}},
                "lastState": {"terminated": {"exitCode": 2, "reason": "Error", "message": "do-not-publish"}}}]}}
        status = control_plane_status(pod)
        self.assertEqual(status["containers"][0]["restartCount"], 4)
        self.assertNotIn("do-not-publish", json.dumps(status))
        facts = public_stack_facts(
            "panic: runtime error: invalid memory address or nil pointer dereference\n"
            "token=do-not-publish request body do-not-publish\n"
            "k8s.io/apiserver/pkg/admission/plugin/policy/validating.(*TypeChecker).Check(do-not-publish)\n"
            "panic: do-not-publish\n")
        self.assertIn("nil-pointer", facts["panicCategories"])
        self.assertIn("panic-redacted", facts["panicCategories"])
        self.assertEqual(facts["publicFrames"], [
            "k8s.io/apiserver/pkg/admission/plugin/policy/validating.(*TypeChecker).Check"])
        self.assertNotIn("do-not-publish", json.dumps(facts))

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

    def test_observation_timeout_collects_pod_evidence_but_still_fails(self):
        deployment = {"kind": "Deployment", "metadata": {"name": "kars-controller"},
                      "spec": {"template": {"metadata": {"labels": {}}, "spec": {"containers": []}}}}
        unobserved = {"policies": [{"generation": 1, "typeChecked": False}], "workloads": []}
        created = dict(unobserved, workloads=[{"kind": "Pod"}])
        with patch("sre_authority.bootstrap_probe.request", return_value=(201, {})) as api, \
                patch("sre_authority.bootstrap_probe.upsert", return_value={"accepted": True}), \
                patch("sre_authority.bootstrap_probe.collect", side_effect=[unobserved, created]), \
                patch("sre_authority.bootstrap_probe.time.monotonic", side_effect=[0, 1, 91, 100, 101]), \
                patch("sre_authority.bootstrap_probe.time.sleep"), \
                patch("sre_authority.bootstrap_probe.write_report") as report:
            with self.assertRaisesRegex(RuntimeError, "observation timed out"):
                exercise(Path("."), 1, [deployment], POLICIES)
        self.assertTrue(any(call.args[2].endswith("pods?dryRun=All") for call in api.call_args_list))
        result = next(call.args[2] for call in report.call_args_list if call.args[1] == "bootstrap-result.json")
        self.assertEqual(result["podCreation"], "accepted")
        self.assertFalse(result["allPoliciesObservedBeforeCreate"])
        self.assertEqual(result["readiness"], "not-claimed")

    def test_failure_diagnostics_precede_teardown_without_pod_spec_or_log_dump(self):
        source = (Path(__file__).resolve().parents[1] / "run.sh").read_text()
        install = source.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        self.assertIn("sre_authority.bootstrap_probe --diagnostics-only", install)
        self.assertNotIn("kubectl describe pod", install)
        self.assertNotIn("kubectl logs", install)
        self.assertIn("return 1", install)


if __name__ == "__main__":
    unittest.main()
