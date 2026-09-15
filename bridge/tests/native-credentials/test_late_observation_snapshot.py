# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Retain the actual governed Task bundle, not an absent legacy credentialsRef."""

import base64
from contextlib import nullcontext
import copy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import observation_cases as observation
from credential_cases import SOURCE
from native_api import CORE, Failure, core, resource


class LateObservationSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.name = "native-observation-task"
        namespace = "kars-" + self.name
        self.owner = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
                      "name": self.name, "uid": "task-uid", "controller": True}
        self.sandbox = {
            "metadata": {"name": self.name, "namespace": CORE, "uid": "sandbox-uid",
                         "ownerReferences": [copy.deepcopy(self.owner)]},
            "spec": {"credentialBindings": {"grant": {"name": "workspace", "uid": "grant-uid"}}},
        }
        self.deployment = {"metadata": {"name": self.name, "uid": "deployment-uid"}, "spec": {}}
        self.pod = {"metadata": {"name": "pod", "uid": "pod-uid"}, "spec": {"containers": [
            {"name": "openclaw", "envFrom": [{"secretRef": {"name": "projection", "optional": False}}]},
            {"name": "inference-router"},
        ]}}
        self.task_path = resource(CORE, "karstasks", self.name)
        self.bundle_path = core(CORE, "secrets", "kars-credential-bundle-karstask-" + self.name)
        self.projection_path = core(namespace, "secrets", "projection")
        self.source_path = core(CORE, "secrets", SOURCE)
        self.objects = {
            self.task_path: {"metadata": {"name": self.name, "uid": "task-uid", "annotations": {
                "kars.azure.com/credential-bundle-uid": "bundle-uid"}}, "spec": {},
                "status": {"envelopeDigest": "sha256:" + "a" * 64}},
            self.bundle_path: {"metadata": {"uid": "bundle-uid", "ownerReferences": [copy.deepcopy(self.owner)],
                "annotations": {"kars.azure.com/credential-purpose": "agent-bundle-v2",
                                "kars.azure.com/credential-target-kind": "KarsTask",
                                "kars.azure.com/credential-target-uid": "task-uid"}},
                "data": {"SLACK_BOT_TOKEN": "cHJpdmF0ZS1maXh0dXJl"}},
            self.projection_path: {"metadata": {"uid": "projection-uid", "annotations": {
                "kars.azure.com/credential-source-uid": "bundle-uid"}},
                "data": {"SLACK_BOT_TOKEN": "cHJpdmF0ZS1maXh0dXJl"}},
            self.source_path: {"metadata": {"uid": "source-uid"},
                               "data": {"SLACK_BOT_TOKEN": "cHJpdmF0ZS1maXh0dXJl"}},
            "/api/v1/namespaces/" + CORE: {"metadata": {"uid": "root-uid", "annotations": {
                "kars.azure.com/private-epoch": "root-epoch"}}},
            "/api/v1/namespaces/" + namespace: {"metadata": {"uid": "namespace-uid"}},
            core(namespace, "pods"): {"items": [self.pod]},
            core(namespace, "secrets", "router-services-admin"): {"metadata": {"uid": "admin-uid"},
                "data": {"control-token": base64.b64encode(b"old-operator-token").decode()}},
            resource(CORE, "deployments", "kars-controller", "/apis/apps/v1"): {
                "metadata": {"uid": "controller-uid", "generation": 1}, "spec": {}},
        }
        self.reads = []

        def get(path):
            self.reads.append(path)
            return copy.deepcopy(self.objects[path])

        self.setup = SimpleNamespace(admin=SimpleNamespace(get=get))
        self.cases = observation.ObservationCases(self.setup, None, None)
        self.cases.observer_target = {"task": self.name, "sandbox": self.name,
                                      "workspace": CORE, "uid": "sandbox-uid"}

    def snapshot(self):
        with patch.object(observation, "running", return_value=(
                self.sandbox, self.deployment, self.pod)):
            return self.cases.late_runtime_before()

    def test_v2_without_legacy_reference_retains_source_task_bundle_and_projection(self):
        self.assertNotIn("credentialsRef", self.sandbox["spec"])
        result = self.snapshot()
        self.assertEqual([path for path, _ in result["stored"]],
                         [self.source_path, self.bundle_path, self.projection_path])
        for path, saved in result["stored"]:
            self.assertEqual(saved, self.objects[path])
        self.assertEqual(result["task"], self.objects[self.task_path])
        self.assertEqual(result["pods"], {"pod-uid"})

    def test_replaced_or_unanchored_bundle_and_projection_are_refused(self):
        for changed in ("task-uid", "missing-anchor", "bundle-uid", "bundle-owner",
                        "bundle-purpose", "bundle-kind", "bundle-target", "projection-source"):
            with self.subTest(changed=changed):
                self.setUp()
                if changed == "task-uid":
                    self.objects[self.task_path]["metadata"]["uid"] = "replacement"
                elif changed == "missing-anchor":
                    self.objects[self.task_path]["metadata"]["annotations"] = {}
                elif changed == "bundle-uid":
                    self.objects[self.bundle_path]["metadata"]["uid"] = "replacement"
                elif changed == "bundle-owner":
                    self.objects[self.bundle_path]["metadata"]["ownerReferences"][0]["uid"] = "replacement"
                elif changed == "projection-source":
                    self.objects[self.projection_path]["metadata"]["annotations"]["kars.azure.com/credential-source-uid"] = "replacement"
                else:
                    key = {"bundle-purpose": "purpose", "bundle-kind": "target-kind",
                           "bundle-target": "target-uid"}[changed]
                    self.objects[self.bundle_path]["metadata"]["annotations"]["kars.azure.com/credential-" + key] = "replacement"
                with self.assertRaises(Failure):
                    self.snapshot()

    def test_missing_or_ambiguous_projection_is_explicitly_rejected(self):
        for sources in ([], [{"secretRef": {"name": "optional", "optional": True}}],
                        [{"secretRef": {"name": "one", "optional": False}},
                         {"secretRef": {"name": "two", "optional": False}}]):
            with self.subTest(sources=sources):
                self.pod["spec"]["containers"][0]["envFrom"] = sources
                with self.assertRaises(Failure):
                    self.snapshot()

    def retired(self, captured):
        before = copy.deepcopy(self.snapshot())
        namespace = "kars-" + self.name
        self.current_pod = copy.deepcopy(self.pod)
        self.current_pod["metadata"].update(name="current-pod", uid="current-pod-uid")
        self.objects[core(namespace, "pods")]["items"] = [self.current_pod]
        self.objects[core(namespace, "secrets", "router-services-admin")]["data"]["control-token"] = (
            base64.b64encode(b"new-operator-token").decode())
        self.receipt = {
            "version": 4, "phase": "Qualified", "captured": captured,
            "runtime": {"workspace": CORE, "sandbox": {"name": self.name, "uid": "sandbox-uid"},
                        "task": {"object": {"name": self.name, "uid": "task-uid"},
                                 "authorization": before["task"]["status"]["envelopeDigest"]}},
            "deployment": {"name": self.name, "uid": "deployment-uid"},
            "baseline": {"object": {"uid": "admin-uid"}},
        }
        return before

    def after(self, before, responses=(401, 200)):
        namespace = "kars-" + self.name
        self.objects["/api/v1/namespaces/" + namespace]["metadata"]["annotations"] = {
            "kars.azure.com/private-root-retirement": json.dumps(self.receipt)}
        connections = [Mock() for _ in responses]
        for connection, status in zip(connections, responses):
            connection.getresponse.return_value = SimpleNamespace(status=status, read=lambda _: b"")
        with patch.object(observation, "running", return_value=(
                self.sandbox, self.deployment, self.current_pod)), \
             patch.object(observation, "forward", return_value=nullcontext()), \
             patch.object(observation.http.client, "HTTPConnection", side_effect=connections):
            self.cases.late_runtime_after(before)
        self.assertEqual([connection.request.call_args.args for connection in connections],
                         [("GET", "/internal/access-requests")] * 2)
        self.assertEqual([connection.request.call_args.kwargs["headers"]["Authorization"]
                          for connection in connections],
                         ["Bearer old-operator-token", "Bearer new-operator-token"])
        self.assertTrue(all(connection.close.called for connection in connections))

    def test_writer_and_private_retirement_can_capture_different_pod_generations(self):
        before = self.retired(["post-writer-pod-uid"])
        self.assertEqual(before["pods"], {"pod-uid"})
        self.assertFalse(before["pods"].issubset(set(self.receipt["captured"])))
        self.after(before)

    def test_private_retirement_still_accepts_the_original_pod_generation(self):
        self.after(self.retired(["pod-uid"]))

    def test_private_retirement_can_start_after_writer_retirement_left_no_live_pods(self):
        self.after(self.retired([]))

    def test_neither_retirement_phase_may_leave_a_captured_pod_alive(self):
        for survivor in ("pod-uid", "post-writer-pod-uid"):
            with self.subTest(survivor=survivor):
                self.setUp()
                before = self.retired(["post-writer-pod-uid"])
                self.objects[core("kars-" + self.name, "pods")]["items"].append(
                    {"metadata": {"uid": survivor}})
                with self.assertRaises(Failure):
                    self.after(before)

    def test_private_receipt_must_bind_the_reviewed_runtime_and_original_admin_key(self):
        for path, value in (
            (("version",), 3), (("phase",), "Rotating"),
            (("runtime", "workspace"), "foreign"),
            (("runtime", "sandbox", "uid"), "foreign"),
            (("runtime", "task", "object", "uid"), "foreign"),
            (("runtime", "task", "authorization"), "sha256:" + "b" * 64),
            (("deployment", "uid"), "foreign"), (("baseline", "object", "uid"), "foreign"),
            (("captured",), None), (("captured",), ["duplicate", "duplicate"]),
            (("captured",), [None]),
        ):
            with self.subTest(path=path, value=value):
                self.setUp()
                before = self.retired(["post-writer-pod-uid"])
                target = self.receipt
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                with self.assertRaises(Failure):
                    self.after(before)

    def test_actual_old_key_denial_and_new_key_acceptance_remain_required(self):
        for responses in ((200, 200), (401, 401)):
            with self.subTest(responses=responses):
                self.setUp()
                with self.assertRaises(Failure):
                    self.after(self.retired(["post-writer-pod-uid"]), responses)

    def test_runtime_data_shared_root_and_admin_key_guards_are_preserved(self):
        for fault in ("sandbox-intent", "task-intent", "source-data", "projection-uid",
                      "root-epoch", "root-deployment", "admin-key", "admin-uid"):
            with self.subTest(fault=fault):
                self.setUp()
                before = self.retired(["post-writer-pod-uid"])
                if fault == "sandbox-intent":
                    self.sandbox["spec"]["unreviewed"] = True
                elif fault == "task-intent":
                    self.objects[self.task_path]["spec"]["unreviewed"] = True
                elif fault == "source-data":
                    self.objects[self.source_path]["data"]["SLACK_BOT_TOKEN"] = "changed"
                elif fault == "projection-uid":
                    self.objects[self.projection_path]["metadata"]["uid"] = "replacement"
                elif fault == "root-epoch":
                    self.objects["/api/v1/namespaces/" + CORE]["metadata"]["annotations"]["kars.azure.com/private-epoch"] = "changed"
                elif fault == "root-deployment":
                    self.objects[resource(CORE, "deployments", "kars-controller", "/apis/apps/v1")]["metadata"]["generation"] += 1
                else:
                    admin = self.objects[core("kars-" + self.name, "secrets", "router-services-admin")]
                    if fault == "admin-key":
                        admin["data"] = copy.deepcopy(before["admin"]["data"])
                    else:
                        admin["metadata"]["uid"] = "replacement"
                with self.assertRaises(Failure):
                    self.after(before)


if __name__ == "__main__":
    unittest.main()
