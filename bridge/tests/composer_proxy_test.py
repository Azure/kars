# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Real Helm lookup against a loopback API; no customer kubeconfig is used."""

import copy
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

import yaml

BRIDGE = Path(__file__).resolve().parents[1]
CHART = BRIDGE / "deploy/helm/kars-bridge"
SPEC = importlib.util.spec_from_file_location("composer_proxy", BRIDGE / "deploy/configure-orchestrator-proxy.py")
CONFIG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONFIG)
NAMESPACE_UID = "11111111-1111-4111-8111-111111111111"
SANDBOX_UID = "22222222-2222-4222-8222-222222222222"
NS_PATH = "/api/v1/namespaces/kars-bridge-orchestrator"
SANDBOX_PATH = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/bridge-orchestrator"
AGENT_PATH = "/apis/apps/v1/namespaces/kube-system/deployments/konnectivity-agent"


def objects():
    return {
        NS_PATH: {"apiVersion": "v1", "kind": "Namespace", "metadata": {
            "name": CONFIG.RUNTIME, "uid": NAMESPACE_UID, "annotations": {
                "kars.azure.com/sandbox-name": CONFIG.SANDBOX,
                "kars.azure.com/sandbox-namespace": "kars-system",
                "kars.azure.com/sandbox-uid": SANDBOX_UID,
            }}},
        SANDBOX_PATH: {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
                       "metadata": {"name": CONFIG.SANDBOX, "namespace": "kars-system",
                                    "uid": SANDBOX_UID, "labels": {
                                        "kars.azure.com/managed-by": "kars-bridge",
                                        "kars.azure.com/orchestrator": "true"}}},
        AGENT_PATH: {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {
            "name": "konnectivity-agent", "namespace": "kube-system"},
            "spec": {"selector": {"matchLabels": {"app": "konnectivity-agent"}},
                     "template": {"metadata": {"labels": {"app": "konnectivity-agent"}},
                                  "spec": {"hostNetwork": False}}}},
    }


class Api:
    def __init__(self, inventory=None, denied=None):
        self.objects = objects() if inventory is None else inventory
        self.denied = denied
        self.calls = []

    def __enter__(self):
        state = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                path = urlsplit(self.path).path
                state.calls.append(("GET", path))
                code, value = state.get(path)
                raw = json.dumps(value).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.directory = tempfile.TemporaryDirectory()
        self.kubeconfig = Path(self.directory.name) / "config"
        self.kubeconfig.write_text(json.dumps({
            "apiVersion": "v1", "kind": "Config", "current-context": "composer-fixture",
            "clusters": [{"name": "fixture", "cluster": {
                "server": f"http://127.0.0.1:{self.server.server_port}"}}],
            "contexts": [{"name": "composer-fixture", "context": {"cluster": "fixture", "user": "fixture"}}],
            "users": [{"name": "fixture", "user": {}}],
        }))
        self.kubeconfig.chmod(0o600)
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.directory.cleanup()

    def get(self, path):
        versions = {"apps": "v1", "kars.azure.com": "v1alpha1",
                    "networking.k8s.io": "v1", "rbac.authorization.k8s.io": "v1"}
        resources = {
            "/api/v1": ("v1", [("namespaces", "Namespace", False), ("secrets", "Secret", True),
                              ("serviceaccounts", "ServiceAccount", True), ("services", "Service", True),
                              ("configmaps", "ConfigMap", True)]),
            "/apis/apps/v1": ("apps/v1", [("deployments", "Deployment", True)]),
            "/apis/kars.azure.com/v1alpha1": ("kars.azure.com/v1alpha1", [("karssandboxes", "KarsSandbox", True)]),
            "/apis/networking.k8s.io/v1": ("networking.k8s.io/v1", [("networkpolicies", "NetworkPolicy", True)]),
            "/apis/rbac.authorization.k8s.io/v1": ("rbac.authorization.k8s.io/v1", [
                ("roles", "Role", True), ("rolebindings", "RoleBinding", True),
                ("clusterroles", "ClusterRole", False), ("clusterrolebindings", "ClusterRoleBinding", False)]),
        }
        if path == "/version":
            return 200, {"major": "1", "minor": "35", "gitVersion": "v1.35.0"}
        if path == "/api":
            return 200, {"kind": "APIVersions", "versions": ["v1"]}
        if path == "/apis":
            return 200, {"kind": "APIGroupList", "groups": [
                {"name": group, "versions": [{"groupVersion": group + "/" + version, "version": version}],
                 "preferredVersion": {"groupVersion": group + "/" + version, "version": version}}
                for group, version in versions.items()
            ]}
        if path in resources:
            version, entries = resources[path]
            return 200, {"kind": "APIResourceList", "groupVersion": version, "resources": [
                {"name": name, "kind": kind, "namespaced": namespaced, "verbs": ["get", "list"]}
                for name, kind, namespaced in entries
            ]}
        if path == self.denied:
            return 403, {"apiVersion": "v1", "kind": "Status", "code": 403, "reason": "Forbidden"}
        if path in self.objects:
            return 200, self.objects[path]
        return 404, {"apiVersion": "v1", "kind": "Status", "code": 404, "reason": "NotFound"}


def render(api=None, enabled=True, *extra):
    args = ["helm", "template", "kars-bridge", str(CHART), "--namespace", "kars-system"]
    if api:
        args += ["--kubeconfig", str(api.kubeconfig), "--kube-context", "composer-fixture",
                 "--dry-run=server", "--disable-openapi-validation"]
    else:
        args += ["--kubeconfig", "/dev/null"]
    if enabled:
        args += ["--set", "networkPolicy.orchestratorProxy.enabled=true",
                 "--set-string", "networkPolicy.orchestratorProxy.namespaceUid=" + NAMESPACE_UID,
                 "--set-string", "networkPolicy.orchestratorProxy.sandboxUid=" + SANDBOX_UID]
    return subprocess.run([*args, *extra], capture_output=True, text=True, timeout=30)


class ComposerProxyTests(unittest.TestCase):
    def test_default_and_old_values_do_not_change_core_or_contact_proxy(self):
        with Api() as api:
            result = render(api, False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("orchestrator-proxy", result.stdout)
            self.assertNotIn(("GET", AGENT_PATH), api.calls)
        legacy = render(None, False, "--set", "networkPolicy.orchestratorProxy=null")
        self.assertEqual(legacy.returncode, 0, legacy.stderr)
        self.assertNotIn("orchestrator-proxy", legacy.stdout)

    def test_proxy_rule_is_additive_namespaced_and_port_scoped(self):
        with Api() as api:
            before, after = render(api, False), render(api)
            self.assertEqual(after.returncode, 0, after.stderr)
            CONFIG.require_policy_only(before.stdout, after.stdout, "kars-bridge")
            policy = next(obj for obj in yaml.safe_load_all(after.stdout)
                          if obj and obj["metadata"]["name"] == "kars-bridge-orchestrator-proxy")
            self.assertEqual(policy["metadata"]["namespace"], CONFIG.RUNTIME)
            self.assertEqual(policy["spec"], {
                "podSelector": {"matchLabels": {"kars.azure.com/component": "sandbox",
                                              "kars.azure.com/sandbox": CONFIG.SANDBOX}},
                "policyTypes": ["Ingress"],
                "ingress": [{"from": [{
                    "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                    "podSelector": {"matchLabels": {"app": "konnectivity-agent"}},
                }], "ports": [{"port": 8443, "protocol": "TCP"}]}],
            })
            self.assertNotIn("helm.sh/hook", policy["metadata"].get("annotations", {}))
            self.assertNotIn("helm.sh/resource-policy", policy["metadata"].get("annotations", {}))
            self.assertEqual(policy["metadata"]["labels"]["app.kubernetes.io/managed-by"], "Helm")
            self.assertTrue(all(method == "GET" for method, _ in api.calls))
            for path in [NS_PATH, SANDBOX_PATH, AGENT_PATH]:
                self.assertIn(("GET", path), api.calls)

    def test_missing_foreign_replaced_terminating_and_host_network_targets_fail(self):
        cases = []
        for path in [NS_PATH, SANDBOX_PATH, AGENT_PATH]:
            value = objects()
            del value[path]
            cases.append(value)
        mutations = [
            (NS_PATH, ["metadata", "uid"], "recreated"),
            (NS_PATH, ["metadata", "annotations", "kars.azure.com/sandbox-uid"], "foreign"),
            (NS_PATH, ["metadata", "annotations", "kars.azure.com/sandbox-namespace"], "other-workspace"),
            (NS_PATH, ["metadata", "deletionTimestamp"], "2026-09-17T00:00:00Z"),
            (SANDBOX_PATH, ["metadata", "uid"], "recreated"),
            (SANDBOX_PATH, ["metadata", "labels", "kars.azure.com/managed-by"], "other"),
            (AGENT_PATH, ["spec", "template", "spec", "hostNetwork"], True),
            (AGENT_PATH, ["spec", "selector", "matchLabels"], {}),
            (AGENT_PATH, ["spec", "selector", "matchLabels"], {"app": "konnectivity-agent", "other": "source"}),
        ]
        for path, fields, replacement in mutations:
            value = objects()
            target = value[path]
            for field in fields[:-1]:
                target = target[field]
            target[fields[-1]] = replacement
            cases.append(value)
        for inventory in cases:
            with self.subTest(inventory=inventory), Api(inventory) as api:
                result = render(api)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("orchestratorProxy", result.stderr)

    def test_no_offline_or_permission_denied_success(self):
        self.assertNotEqual(render().returncode, 0)
        with Api(denied=NS_PATH) as api:
            self.assertNotEqual(render(api).returncode, 0)

    def test_configuration_refuses_every_unrelated_release_change(self):
        resource = {"apiVersion": "apps/v1", "kind": "Deployment",
                    "metadata": {"name": "kars-bridge-bff", "namespace": "kars-system"},
                    "spec": {"template": {"spec": {"containers": [{"name": "bff", "image": "old"}]}}}}
        before = yaml.safe_dump(resource)
        changed = copy.deepcopy(resource)
        changed["spec"]["template"]["spec"]["containers"][0]["image"] = "new"
        for after in ["", yaml.safe_dump(changed), before + "---\n" + before]:
            with self.assertRaises(ValueError):
                CONFIG.require_policy_only(before, after, "kars-bridge")
        CONFIG.require_policy_only(before, before, "kars-bridge")


class ConfigurationTests(unittest.TestCase):
    def configure(self, fault=None, disable=False, check=False):
        namespace = objects()[NS_PATH]
        sandbox = objects()[SANDBOX_PATH]
        deployment = {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {
            "name": "kars-bridge-bff", "namespace": "kars-system"}, "spec": {
                "template": {"spec": {"containers": [{"name": "bff", "image": "immutable"}]}}}}
        policy = {"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                  "metadata": {"name": "kars-bridge-orchestrator-proxy", "namespace": CONFIG.RUNTIME},
                  "spec": {"podSelector": {"matchLabels": {"kars.azure.com/sandbox": CONFIG.SANDBOX}},
                           "policyTypes": ["Ingress"], "ingress": []}}
        base = yaml.safe_dump(deployment)
        enabled = base + "---\n" + yaml.safe_dump(policy)
        before, after = (enabled, base) if disable else (base, enabled)
        if fault == "workload-drift":
            after = after.replace("immutable", "different-image")
        values = {"core": {"namespace": "kars-system"}, "private": {"secret": "never-print-this"},
                  "idp": {"roleMap": {"removed-default": None}}}
        if fault == "replaced-target":
            values["networkPolicy"] = {"orchestratorProxy": {"namespaceUid": "replaced"}}
        self.calls = []
        self.writes = []
        self.private_path = None
        status_reads = 0
        inventory_reads = 0

        def run(command, **kwargs):
            nonlocal status_reads, inventory_reads
            self.calls.append(command)
            self.assertEqual(command[1:3], ["--kubeconfig", "/explicit/config"])
            self.assertIn("explicit-context", command)
            self.assertTrue(kwargs["capture_output"])
            self.assertGreater(kwargs["timeout"], 0)
            output = ""
            if command[0] == "helm":
                if "status" in command:
                    status_reads += 1
                    revision = 2 if self.writes or (fault == "concurrent-release" and status_reads > 1) else 1
                    output = json.dumps({"version": revision, "info": {"status": "deployed"}})
                elif "manifest" in command:
                    output = after if self.writes else before
                elif "values" in command:
                    self.assertNotIn("--all", command, "computed values discard null deletions")
                    output = json.dumps(values)
                elif "template" in command or "upgrade" in command:
                    path = Path(command[command.index("--values") + 1])
                    self.private_path = path
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                    stored = json.loads(path.read_text())
                    self.assertEqual(stored["private"], values["private"])
                    self.assertEqual(stored["idp"], values["idp"])
                    self.assertEqual(stored["networkPolicy"]["orchestratorProxy"]["enabled"], not disable)
                    self.assertNotIn("never-print-this", " ".join(command))
                    if "template" in command:
                        output = after
                    elif "--dry-run=server" in command:
                        if fault == "admission-denied":
                            return subprocess.CompletedProcess(command, 1, "never-print-this", "never-print-this")
                    else:
                        self.writes.append(command)
                        if fault == "lost-upgrade-response":
                            return subprocess.CompletedProcess(command, 1, "never-print-this", "never-print-this")
                else:
                    self.fail(f"unexpected Helm operation: {command}")
            elif command[0] == "kubectl":
                if "-f" in command:
                    inventory_reads += 1
                    resource = copy.deepcopy(deployment)
                    resource["metadata"].update({"uid": "bff-uid", "resourceVersion": "one"})
                    if fault == "live-image-drift" or (fault == "post-upgrade-image-drift" and self.writes):
                        resource["spec"]["template"]["spec"]["containers"][0]["image"] = "drifted"
                    if fault == "changed-live-snapshot" and inventory_reads > 1:
                        resource["metadata"]["resourceVersion"] = "two"
                    output = json.dumps({"apiVersion": "v1", "kind": "List",
                                         "items": [] if fault == "missing-live-workload" else [resource]})
                elif "--raw" in command:
                    self.assertEqual(command[-1],
                                     f"/api/v1/namespaces/{CONFIG.RUNTIME}/pods/router-ready:8443/proxy/healthz")
                    if fault == "proxy-unavailable":
                        raise subprocess.TimeoutExpired(command, 20)
                    output = "ok"
                elif "namespace" in command:
                    output = json.dumps(namespace)
                elif "karssandboxes.kars.azure.com" in command:
                    output = json.dumps(sandbox)
                elif "networkpolicy" in command:
                    output = "" if disable else json.dumps(policy)
                elif "pods" in command:
                    output = json.dumps({"items": [{
                        "metadata": {"name": "router-ready"},
                        "status": {"phase": "Running", "containerStatuses": [
                            {"name": "inference-router", "ready": True}]}},
                    ]})
                else:
                    self.fail(f"unexpected Kubernetes operation: {command}")
            else:
                self.fail(f"unexpected tool: {command}")
            return subprocess.CompletedProcess(command, 0, output, "")

        args = argparse.Namespace(kubeconfig="/explicit/config", context="explicit-context",
                                  namespace="kars-system", release="kars-bridge", disable=disable,
                                  check=check, timeout=1)
        with patch.object(CONFIG.subprocess, "run", side_effect=run):
            return CONFIG.Configuration(args).configure()

    def test_enable_checks_same_release_preserves_private_values_and_probes_exact_path(self):
        result = self.configure()
        self.assertEqual(len(self.writes), 1)
        self.assertEqual(result["proxyAllowance"], "installed")
        self.assertTrue(result["proxyHealth"])
        self.assertIn("not performed", result["functionalAcceptance"])
        self.assertFalse(self.private_path.exists())

    def test_check_preflights_without_applying_or_probing(self):
        result = self.configure(check=True)
        self.assertEqual(self.writes, [])
        self.assertEqual(result["proxyAllowance"], "not changed")
        self.assertTrue(result["preflightPassed"])
        self.assertFalse(any("--raw" in command for command in self.calls))
        self.assertFalse(self.private_path.exists())

    def test_disable_removes_only_the_policy_and_skips_inference(self):
        result = self.configure(disable=True)
        self.assertEqual(len(self.writes), 1)
        self.assertEqual(result["proxyAllowance"], "removed")
        self.assertIsNone(result["proxyHealth"])
        self.assertFalse(any("--raw" in command for command in self.calls))
        self.assertFalse(self.private_path.exists())

    def test_invalid_or_concurrent_changes_are_rejected_before_writes(self):
        for fault in ["workload-drift", "replaced-target", "concurrent-release", "admission-denied",
                      "live-image-drift", "missing-live-workload", "changed-live-snapshot"]:
            with self.subTest(fault=fault), self.assertRaises((ValueError, RuntimeError)) as raised:
                self.configure(fault)
            self.assertEqual(self.writes, [])
            self.assertNotIn("never-print-this", str(raised.exception))
            if self.private_path:
                self.assertFalse(self.private_path.exists())

    def test_ambiguous_upgrade_is_not_retried_and_never_reports_success(self):
        with self.assertRaises(RuntimeError) as raised:
            self.configure("lost-upgrade-response")
        self.assertEqual(len(self.writes), 1)
        self.assertNotIn("never-print-this", str(raised.exception))
        self.assertFalse(self.private_path.exists())

    def test_post_upgrade_drift_fails_without_rolling_back_or_claiming_qualification(self):
        with self.assertRaisesRegex(RuntimeError, "Non-policy live resources changed"):
            self.configure("post-upgrade-image-drift")
        self.assertEqual(len(self.writes), 1)

    def test_live_comparison_preserves_server_defaults_and_secret_stringdata_semantics(self):
        declared = {"apiVersion": "v1", "kind": "Secret", "metadata": {
            "name": "oidc", "namespace": "kars-system"},
            "type": "Opaque", "stringData": {"client-secret": "fixture-only"}}
        current = copy.deepcopy(declared)
        current.pop("stringData")
        current["metadata"].update({"uid": "secret-uid", "resourceVersion": "one",
                                    "annotations": {"external": "preserve"}})
        current["data"] = {"client-secret": "Zml4dHVyZS1vbmx5"}
        live = {CONFIG.live_key(current): current}
        CONFIG.require_live_compatible(yaml.safe_dump(declared), live, "kars-bridge")
        current["data"]["client-secret"] = "Y2hhbmdlZA=="
        with self.assertRaises(ValueError):
            CONFIG.require_live_compatible(yaml.safe_dump(declared), live, "kars-bridge")

    def test_health_failure_after_install_is_not_functional_success(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            self.configure("proxy-unavailable")
        self.assertEqual(len(self.writes), 1)
        self.assertFalse(self.private_path.exists())


if __name__ == "__main__":
    unittest.main()
