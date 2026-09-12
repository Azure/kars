"""Retain the actual governed Task bundle, not an absent legacy credentialsRef."""

import copy
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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
                "kars.azure.com/credential-bundle-uid": "bundle-uid"}}, "spec": {}},
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
            core(namespace, "secrets", "router-services-admin"): {"metadata": {"uid": "admin-uid"}},
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


if __name__ == "__main__":
    unittest.main()
