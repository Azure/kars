# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Helm/API lifecycle only; deliberately unavailable images are NOT kernel proof.

Opt-in solely for the existing hosted disposable Kind job. Never use an ambient
kubeconfig, customer cluster, or a test-only fake image as a production fallback.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
CHART = ROOT / "deploy/helm/kars-datapath-witness"
CONTEXT = "kind-bridge-addon-lifecycle"
RELEASE = "kars-datapath-witness"
NS = "kars-witness-gadget"
IMAGE = "example.invalid/witness@sha256:" + "a" * 64


@unittest.skipUnless(os.environ.get("WITNESS_TEST_KIND_LIFECYCLE") == "1", "hosted disposable Kind only")
class KindLifecycleTests(unittest.TestCase):
    @classmethod
    def command(cls, tool, *args, body=None, check=True):
        result = subprocess.run(
            [tool, "--kubeconfig", cls.kubeconfig, "--context" if tool == "kubectl" else "--kube-context", CONTEXT, *args],
            input=None if body is None else json.dumps(body), text=True,
            capture_output=True, timeout=90,
        )
        if check and result.returncode:
            raise AssertionError(result.stderr + result.stdout)
        return result

    @classmethod
    def create(cls, value):
        cls.command("kubectl", "create", "-f", "-", body=value)

    @classmethod
    def setUpClass(cls):
        cls.kubeconfig = os.environ["WITNESS_TEST_KUBECONFIG"]
        config = json.loads(cls.command("kubectl", "config", "view", "--minify", "-o", "json").stdout)
        if config["current-context"] != CONTEXT or not re.fullmatch(r"https://127\.0\.0\.1:\d+", config["clusters"][0]["cluster"]["server"]):
            raise AssertionError("refusing anything except the explicit disposable Kind API")
        for namespace in ["kars-system", "kars-demo", "witness-lifecycle-models"]:
            found = cls.command("kubectl", "get", "namespace", namespace, "--ignore-not-found", "-o", "name").stdout
            if not found:
                cls.create({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace}})
        cls.create({"apiVersion": "v1", "kind": "ConfigMap",
                    "metadata": {"name": "witness-core-sentinel", "namespace": "kars-system"}, "data": {"preserve": "core"}})
        cls.create({"apiVersion": "v1", "kind": "ConfigMap",
                    "metadata": {"name": "model-sentinel", "namespace": "witness-lifecycle-models"}, "data": {"preserve": "model"}})
        cls.create({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "witnesssentinels.witness.kars.test"},
            "spec": {"group": "witness.kars.test", "scope": "Namespaced",
                     "names": {"plural": "witnesssentinels", "singular": "witnesssentinel", "kind": "WitnessSentinel"},
                     "versions": [{"name": "v1", "served": True, "storage": True,
                                   "schema": {"openAPIV3Schema": {"type": "object"}}}]},
        })

    def uid(self, kind, name, namespace=None):
        scope = [] if namespace is None else ["-n", namespace]
        return self.command("kubectl", *scope, "get", kind, name, "-o", "jsonpath={.metadata.uid}").stdout

    def helm(self, *args, check=True):
        return self.command("helm", *args, "--namespace", "kars-system", check=check)

    def absent_workloads(self):
        for _ in range(45):
            result = self.command("kubectl", "-n", NS, "get", "daemonsets,deployments,pods",
                                  "-l", "app.kubernetes.io/instance=" + RELEASE, "-o", "json")
            if not json.loads(result.stdout)["items"]:
                return
            time.sleep(1)
        self.fail("release workloads have not terminated")

    def test_operator_enable_error_disable_reenable_and_preservation(self):
        preserved = [
            ("namespace", "kars-system", None), ("configmap", "witness-core-sentinel", "kars-system"),
            ("namespace", "witness-lifecycle-models", None), ("configmap", "model-sentinel", "witness-lifecycle-models"),
            ("crd", "witnesssentinels.witness.kars.test", None),
        ]
        before = [self.uid(*identity) for identity in preserved]
        self.helm("upgrade", "--install", RELEASE, str(CHART))
        off = json.loads(self.command("kubectl", "-n", "kars-system", "get", "cm", RELEASE + "-settings", "-o", "json").stdout)
        self.assertFalse(json.loads(off["data"]["settings.json"])["enabled"])
        self.assertEqual(self.command("kubectl", "get", "ns", NS, "--ignore-not-found", "-o", "name").stdout, "")

        # A raw, unscheduled third-party IG DaemonSet must not be adopted even
        # when no root witness ConfigMap exists.
        legacy = {"apiVersion": "apps/v1", "kind": "DaemonSet",
                  "metadata": {"name": "legacy-ig", "namespace": "witness-lifecycle-models", "labels": {"k8s-app": "gadget"}},
                  "spec": {"selector": {"matchLabels": {"test": "legacy-ig"}},
                           "template": {"metadata": {"labels": {"test": "legacy-ig"}},
                                        "spec": {"nodeSelector": {"witness-test-never-schedule": "true"},
                                                 "containers": [{"name": "ig", "image": IMAGE}]}}}}
        self.create(legacy)
        legacy_uid = self.uid("ds", "legacy-ig", "witness-lifecycle-models")
        on_args = ("upgrade", RELEASE, str(CHART), "--set", "enabled=true", "--set", "sandboxes={demo}",
                   "--set", "aggregator.image=" + IMAGE)
        conflict = self.helm(*on_args, check=False)
        self.assertNotEqual(conflict.returncode, 0)
        self.assertIn("existing Inspektor Gadget", conflict.stderr)
        self.assertEqual(self.uid("ds", "legacy-ig", "witness-lifecycle-models"), legacy_uid)
        # Delete only this test-owned sentinel after proving refusal.
        self.command("kubectl", "-n", "witness-lifecycle-models", "delete", "ds", "legacy-ig", "--wait=true")

        failed = self.helm(*on_args, "--wait", "--timeout", "15s", check=False)
        self.assertNotEqual(failed.returncode, 0, "unavailable operator image must never pass readiness")
        ds_uid = self.uid("ds", "gadget", NS)
        self.helm(*on_args)
        self.assertEqual(self.uid("ds", "gadget", NS), ds_uid)
        off_args = ("upgrade", RELEASE, str(CHART), "--reuse-values", "--set", "enabled=false", "--wait", "--timeout", "45s")
        self.helm(*off_args)
        self.absent_workloads()
        self.helm(*off_args)
        self.helm(*on_args)
        self.assertNotEqual(self.uid("ds", "gadget", NS), ds_uid)
        self.helm(*off_args)
        self.absent_workloads()
        namespace_uid = self.uid("namespace", NS)
        self.helm("uninstall", RELEASE, "--wait", "--timeout", "45s")
        self.assertEqual(self.uid("namespace", NS), namespace_uid)
        self.assertEqual([self.uid(*identity) for identity in preserved], before)
        self.assertEqual(self.command("kubectl", "-n", "kars-system", "get", "cm", RELEASE,
                                      "--ignore-not-found", "-o", "name").stdout, "")
        remaining = self.command("kubectl", "get", "clusterrole,clusterrolebinding",
                                 "-l", "kars.azure.com/witness-addon=true", "-o", "json")
        self.assertEqual(json.loads(remaining.stdout)["items"], [])


if __name__ == "__main__":
    unittest.main()
