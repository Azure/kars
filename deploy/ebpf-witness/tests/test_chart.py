# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Render and server-side Helm lookup tests. The only API is loopback fake data."""
import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from urllib.parse import urlsplit

import yaml

ROOT = Path(__file__).resolve().parents[3]
CHART = ROOT / "deploy/helm/kars-datapath-witness"
IMAGE = "example.invalid/witness@sha256:" + "a" * 64
NS = "kars-witness-gadget"


def render(*extra, kubeconfig=None):
    command = ["helm", "template", "kars-datapath-witness", str(CHART),
               "--namespace", "kars-system"]
    if kubeconfig:
        # The fake serves lookup data, not Kubernetes' OpenAPI schema. Production
        # commands and the disposable Kind lifecycle test keep schema validation.
        command += ["--dry-run=server", "--disable-openapi-validation",
                    "--kubeconfig", str(kubeconfig), "--kube-context", "witness-fake"]
    else:
        command += ["--kubeconfig", "/dev/null"]
    return subprocess.run(command + list(extra), capture_output=True, text=True, timeout=45)


def enabled(*extra, **kwargs):
    return render("--set", "enabled=true", "--set", "sandboxes={demo}",
                  "--set", "aggregator.image=" + IMAGE, *extra, **kwargs)


def documents(result):
    if result.returncode:
        raise AssertionError(result.stderr)
    return [obj for obj in yaml.safe_load_all(result.stdout) if obj]


RESOURCES = {
    "v1": [("namespaces", "Namespace", False), ("configmaps", "ConfigMap", True),
           ("serviceaccounts", "ServiceAccount", True)],
    "apps/v1": [("daemonsets", "DaemonSet", True), ("deployments", "Deployment", True)],
    "rbac.authorization.k8s.io/v1": [("roles", "Role", True), ("rolebindings", "RoleBinding", True),
                                    ("clusterroles", "ClusterRole", False), ("clusterrolebindings", "ClusterRoleBinding", False)],
}


class FakeApi:
    def __init__(self, objects=(), denied=False):
        self.objects = list(objects)
        self.denied = denied
        self.calls = []

    def __enter__(self):
        state = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                path = urlsplit(self.path).path
                state.calls.append(path)
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
            "apiVersion": "v1", "kind": "Config", "current-context": "witness-fake",
            "clusters": [{"name": "fake", "cluster": {"server": f"http://127.0.0.1:{self.server.server_port}"}}],
            "contexts": [{"name": "witness-fake", "context": {"cluster": "fake", "user": "test"}}],
            "users": [{"name": "test", "user": {}}],
        }))
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.directory.cleanup()

    def get(self, path):
        if path == "/version":
            return 200, {"major": "1", "minor": "31", "gitVersion": "v1.31.0"}
        if path == "/api":
            return 200, {"kind": "APIVersions", "versions": ["v1"]}
        if path == "/apis":
            return 200, {"kind": "APIGroupList", "groups": [
                {"name": group, "versions": [{"groupVersion": group + "/v1", "version": "v1"}],
                 "preferredVersion": {"groupVersion": group + "/v1", "version": "v1"}}
                for group in ("apps", "rbac.authorization.k8s.io")
            ]}
        for version, resources in RESOURCES.items():
            base = "/api/v1" if version == "v1" else "/apis/" + version
            if path == base:
                return 200, {"kind": "APIResourceList", "groupVersion": version, "resources": [
                    {"name": plural, "kind": kind, "namespaced": namespaced, "verbs": ["get", "list"]}
                    for plural, kind, namespaced in resources
                ]}
            if not path.startswith(base + "/"):
                continue
            rest = path[len(base) + 1:].split("/")
            namespace = None
            if len(rest) >= 3 and rest[0] == "namespaces":
                namespace, rest = rest[1], rest[2:]
            for plural, kind, _ in resources:
                if rest[0] != plural:
                    continue
                if self.denied:
                    return 403, {"kind": "Status", "apiVersion": "v1", "code": 403, "reason": "Forbidden", "message": "test denied"}
                candidates = [o for o in self.objects if o["kind"] == kind
                              and (namespace is None or o["metadata"].get("namespace") == namespace)]
                if len(rest) == 1:
                    return 200, {"kind": kind + "List", "apiVersion": version, "metadata": {}, "items": candidates}
                for obj in candidates:
                    if obj["metadata"]["name"] == rest[1]:
                        return 200, obj
        return 404, {"kind": "Status", "apiVersion": "v1", "code": 404, "reason": "NotFound", "message": "not found"}


class ChartTests(unittest.TestCase):
    def test_package_is_renderable_without_external_chart_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(["helm", "package", str(CHART), "--destination", directory], check=True, capture_output=True)
            archive = Path(directory) / "kars-datapath-witness-0.1.0.tgz"
            result = subprocess.run(["helm", "template", "kars-datapath-witness", str(archive),
                                     "--namespace", "kars-system", "--kubeconfig", "/dev/null"],
                                    capture_output=True, text=True, check=True)
            self.assertEqual(len(documents(result)), 1)

    def test_default_off_has_only_documented_nonprivileged_intent(self):
        objects = documents(render())
        self.assertEqual([(o["kind"], o["metadata"]["name"]) for o in objects],
                         [("ConfigMap", "kars-datapath-witness-settings")])
        self.assertFalse(json.loads(objects[0]["data"]["settings.json"])["enabled"])

    def test_enabled_manifest_and_minimal_authority(self):
        objects = documents(enabled())
        self.assertEqual(sum(o["kind"] == "DaemonSet" for o in objects), 1)
        self.assertFalse(any(o["kind"] in ("Secret", "Job", "CustomResourceDefinition", "PersistentVolumeClaim") for o in objects))
        namespace = next(o for o in objects if o["kind"] == "Namespace")
        self.assertEqual(namespace["metadata"]["name"], NS)
        self.assertEqual(namespace["metadata"]["labels"]["pod-security.kubernetes.io/enforce"], "privileged")
        self.assertEqual(namespace["metadata"]["annotations"]["helm.sh/resource-policy"], "keep")
        ds = next(o for o in objects if o["kind"] == "DaemonSet")["spec"]["template"]["spec"]
        self.assertIn("SYS_ADMIN", ds["containers"][0]["securityContext"]["capabilities"]["add"])
        self.assertIn("--check-btf", ds["initContainers"][0]["command"])
        self.assertFalse(ds["hostNetwork"])
        self.assertFalse(ds["hostPID"])
        self.assertFalse(any(v.get("hostPath", {}).get("path") in ("/etc", "/opt") for v in ds["volumes"]))
        for obj in objects:
            if obj["kind"] not in ("Role", "ClusterRole"):
                continue
            for rule in obj["rules"]:
                if "update" in rule["verbs"]:
                    self.assertEqual(obj["metadata"]["namespace"], "kars-system")
                    self.assertEqual(rule["resourceNames"], ["kars-datapath-witness"])
                    self.assertEqual(rule["verbs"], ["get", "update"])
                if "create" in rule["verbs"]:
                    self.assertEqual(obj["metadata"]["namespace"], NS)
                    self.assertEqual(rule["resources"], ["pods/portforward"])
                self.assertNotIn("secrets", rule["resources"])
                self.assertNotIn("*", rule["apiGroups"])
        reader = next(o for o in objects if o["kind"] == "Role" and o["metadata"]["namespace"] == "kars-demo")
        self.assertEqual(reader["rules"][0]["resourceNames"], ["karssandbox-demo-egress-allowlist"])
        self.assertEqual(reader["rules"][0]["verbs"], ["get"])

    def test_schema_no_dev_fallback_or_ambiguous_release(self):
        for args in [
            ("--set", "enabled=true"),
            ("--set", "enabled=true", "--set", "aggregator.image=repo:dev"),
            ("--set", "enabled=true", "--set", "sandboxes={../evil}"),
            ("--set", "gadget.image=unreviewed:latest"),
            ("--set", "aggregator.windowSeconds=0"),
            ("--namespace", "gadget"),
        ]:
            self.assertNotEqual(render(*args).returncode, 0, args)

    def test_real_helm_lookups_refuse_legacy_even_without_report(self):
        for kind, name, namespace, image in [
            ("DaemonSet", "gadget", "gadget", "ghcr.io/inspektor-gadget/inspektor-gadget:v0.53.2"),
            ("DaemonSet", "third-party", "shared", "ghcr.io/inspektor-gadget/inspektor-gadget:v0.53.2"),
            ("Deployment", "kars-witness-aggregator", "gadget", "registry/witness-aggregator:alpha"),
        ]:
            obj = {"apiVersion": "apps/v1", "kind": kind,
                   "metadata": {"name": name, "namespace": namespace, "uid": "do-not-adopt"},
                   "spec": {"template": {"spec": {"containers": [{"name": "observer", "image": image}]}}}}
            with FakeApi([obj]) as api:
                result = enabled(kubeconfig=api.kubeconfig)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("existing", result.stderr)
                self.assertTrue(api.calls)

    def test_enable_disable_idempotency_and_removal_identity_fences(self):
        initial = documents(enabled())
        with FakeApi(initial) as api:
            self.assertEqual(documents(enabled(kubeconfig=api.kubeconfig)), initial)
            off = documents(render("--set", "enabled=false", kubeconfig=api.kubeconfig))
            self.assertEqual(len(off), 1)
        for kind in ("ConfigMap", "DaemonSet", "Deployment", "Role", "ClusterRole", "Namespace"):
            objects = copy.deepcopy(initial)
            obj = next(o for o in objects if o["kind"] == kind)
            obj["metadata"]["annotations"]["meta.helm.sh/release-name"] = "other-owner"
            with FakeApi(objects) as api:
                result = render("--set", "enabled=false", kubeconfig=api.kubeconfig)
                self.assertNotEqual(result.returncode, 0, kind)
                self.assertIn("ownership conflict", result.stderr)
        with FakeApi(denied=True) as api:
            result = enabled(kubeconfig=api.kubeconfig)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("error calling lookup", result.stderr)

    def test_removed_scope_and_malformed_previous_settings_refuse_unsafe_removal(self):
        objects = documents(enabled())
        role = next(o for o in objects if o["kind"] == "Role" and o["metadata"]["namespace"] == "kars-demo")
        role["metadata"]["annotations"]["meta.helm.sh/release-name"] = "foreign"
        with FakeApi(objects) as api:
            result = enabled("--set", "sandboxes={new}", kubeconfig=api.kubeconfig)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ownership conflict", result.stderr)
        for data in ["{}", "invalid-json"]:
            objects = documents(enabled())
            intent = next(o for o in objects if o["kind"] == "ConfigMap" and o["metadata"]["name"].endswith("-settings"))
            intent["data"]["settings.json"] = data
            with FakeApi(objects) as api:
                self.assertNotEqual(render("--set", "enabled=false", kubeconfig=api.kubeconfig).returncode, 0)


if __name__ == "__main__":
    unittest.main()
