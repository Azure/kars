"""Administrative fixture setup, kept separate from the native BFF actor."""

import base64
from contextlib import contextmanager
import hashlib
import hmac
import http.client
import json
import secrets
import subprocess
import time

from native_api import BRIDGE, CORE, ROOT, STATE, command, core, private_file, require, until
from loaded_images import loaded_image


def install_core(setup):
    namespace = setup.namespace(CORE)
    setup.admin.patch(f"/api/v1/namespaces/{CORE}", {
        "metadata": {"labels": {"app.kubernetes.io/managed-by": "Helm"},
                     "annotations": {"meta.helm.sh/release-name": "kars",
                                     "meta.helm.sh/release-namespace": CORE}},
    })
    values = {
        "controller": {
            "replicas": 1,
            "image": {"repository": "docker.io/library/kars-native-controller",
                      "tag": "latest", "pullPolicy": "IfNotPresent"},
            "resources": {"requests": {"cpu": "100m", "memory": "256Mi"},
                          "limits": {"cpu": "2", "memory": "2Gi"}},
            "extraEnv": [{"name": "RUST_LOG", "value": "warn"},
                         {"name": "LEADER_ELECTION_ENABLED", "value": "true"}],
        },
        "sandbox": {
            "image": loaded_image("kars-native-runtime"),
            "nodeSelector": {"kubernetes.io/hostname": "bridge-native-worker"},
        },
        "inferenceRouter": {"image": loaded_image("kars-native-router")},
        "observationPrivacyRpc": {"enabled": True},
        "foundry": {"endpoint": "https://native.invalid"},
        "sre": {"enabled": False}, "agentMesh": {"enabled": False},
        "meshPeer": {"enabled": False},
        "monitoring": {"enabled": False, "prometheus": {"enabled": False}},
    }
    private_file("core-values.json", json.dumps(values))
    command("helm", "install", "kars", ".native/core/deploy/helm/kars",
            "--namespace", CORE, "--values", ".native/core-values.json", "--timeout", "180s")
    command("kubectl", "rollout", "status", "deployment/kars-controller",
            "-n", CORE, "--timeout=180s", timeout=195)
    require(not setup.admin.get("/apis/kars.azure.com/v1alpha1/karssreregistrations")["items"],
            "Initial lane must not have active or retired SRE registration")
    return namespace


def install_bridge(setup):
    setup.namespace(BRIDGE)
    signing_key = secrets.token_hex(32)
    setup.admin.create(core(BRIDGE, "secrets"), {
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {"name": "native-principal", "namespace": BRIDGE},
        "data": {"session-secret": base64.b64encode(signing_key.encode()).decode()},
    })
    values = {
        "namespace": BRIDGE, "createNamespace": False, "core": {"namespace": CORE},
        "auth": {"principalSecretName": "native-principal"},
        "bff": {
            "image": {"repository": "docker.io/library/kars-native-bff",
                      "tag": "latest", "pullPolicy": "IfNotPresent"},
            "extraEnv": [
                {"name": "BRIDGE_ORCHESTRATOR_ENDPOINT", "value": "http://127.0.0.1:9"},
                {"name": "BRIDGE_ENGINEERING_POLLER_SECONDS", "value": "86400"},
                {"name": "RUST_LOG", "value": "warn"},
            ],
        },
    }
    private_file("bridge-values.json", json.dumps(values))
    manifest = command(
        "helm", "template", "bridge-native", "deploy/helm/kars-bridge",
        "--namespace", BRIDGE, "--values", ".native/bridge-values.json",
        "--show-only", "templates/rbac.yaml", "--show-only", "templates/bff.yaml",
    )
    command("kubectl", "apply", "-f", "-", stdin=manifest)
    command("kubectl", "rollout", "status", "deployment/kars-bridge-bff",
            "-n", BRIDGE, "--timeout=180s", timeout=195)
    return signing_key


def principal(key):
    def encode(value):
        return base64.urlsafe_b64encode(json.dumps(value, separators=(",", ":")).encode()).rstrip(b"=")
    payload = b".".join([
        encode({"alg": "HS256", "typ": "JWT"}),
        encode({"sub": "native-operator", "name": "Native qualification",
                "roles": ["operator", "user"], "exp": int(time.time()) + 3600}),
    ])
    signature = base64.urlsafe_b64encode(
        hmac.new(key.encode(), payload, hashlib.sha256).digest(),
    ).rstrip(b"=")
    return (payload + b"." + signature).decode()


class Bff:
    def __init__(self, key):
        self.token = principal(key)

    def call(self, method, path, body=None, expected=200, authenticated=True, include_status=False):
        connection = http.client.HTTPConnection("127.0.0.1", 18081, timeout=60)
        headers = {"Content-Type": "application/json"}
        if authenticated:
            headers["x-kars-principal-token"] = self.token
        try:
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            raw = response.read(1024 * 1024)
            allowed = expected if isinstance(expected, tuple) else (expected,)
            require(response.status in allowed, f"BFF {method} returned {response.status}, expected {expected}")
            require(len(raw) < 1024 * 1024, "BFF response exceeded bound")
            value = json.loads(raw) if raw else {}
            return (response.status, value) if include_status else value
        finally:
            connection.close()

    def channel(self, namespace, value, channel="slack", expected=200):
        return self.call("POST", f"/api/namespaces/{namespace}/channels",
                         {"channel": channel, "token": value}, expected)

    def ready(self):
        try:
            result = self.call("GET", "/readyz", expected=(200, 503), authenticated=False)
            return result.get("status") == "ok" and result.get("cluster_configured") is True
        except (ConnectionError, OSError):
            return False


@contextmanager
def bridge_connection(key):
    with (STATE / "port-forward.log").open("w") as output:
        process = subprocess.Popen(
            ["kubectl", "port-forward", "-n", BRIDGE, "service/kars-bridge-bff",
             "18081:8081", "--address=127.0.0.1"],
            cwd=ROOT, stdin=subprocess.DEVNULL, stdout=output, stderr=output,
        )
        try:
            client = Bff(key)
            def ready():
                require(process.poll() is None, "BFF port-forward terminated")
                try:
                    client.call("GET", "/readyz", authenticated=False)
                    return True
                except (ConnectionError, OSError):
                    return False
            until("real BFF readiness", ready, 30)
            yield client
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
