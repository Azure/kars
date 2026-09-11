"""Bounded real Kubernetes clients; actor contexts never inherit admin keys."""

import base64
import http.client
import json
import os
from pathlib import Path
import re
import ssl
import subprocess
import time
import urllib.parse

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / ".native"
CORE = "kars-system"
BRIDGE = "bridge-native"
WRITER = "kars-bridge"
GROUP = "/apis/kars.azure.com/v1alpha1"


class Failure(Exception):
    """Only static, secret-free diagnostics may enter this exception."""


def require(condition, message):
    if not condition:
        raise Failure(message)


def command(*args, stdin=None, timeout=180):
    result = subprocess.run(
        args, input=stdin, cwd=ROOT, capture_output=True, text=True,
        timeout=timeout, check=False,
    )
    require(result.returncode == 0, f"Setup command {args[0]} failed ({result.returncode})")
    return result.stdout


def private_file(name, value):
    path = STATE / name
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(path, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, "w") as output:
        output.write(value)
    return path


def until(description, operation, timeout=180):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = operation()
        if value:
            return value
        time.sleep(1)
    raise Failure(f"Deadline: {description}")


def resource(namespace, plural, name=None, group=GROUP):
    path = f"{group}/namespaces/{namespace}/{plural}"
    return path + (f"/{name}" if name else "")


def core(namespace, plural, name=None):
    return resource(namespace, plural, name, "/api/v1")


def uid(value):
    return value["metadata"]["uid"]


def status_detail(value):
    if not isinstance(value, dict):
        return "invalid-status"
    details = []
    reason = value.get("reason", "")
    if isinstance(reason, str) and re.fullmatch(r"[A-Za-z]{1,64}", reason):
        details.append(reason)
    message = value.get("message", "")
    if isinstance(message, str):
        policy = re.search(r"ValidatingAdmissionPolicy ['\"]([a-z0-9-]{1,253})['\"]", message)
        if policy:
            details.append(f"policy={policy[1]}")
        missing = re.search(r"no such key: ([A-Za-z_][A-Za-z0-9_]{0,127})", message)
        if missing:
            details.append(f"missing-field={missing[1]}")
    causes = value.get("details")
    causes = causes.get("causes") if isinstance(causes, dict) else []
    for cause in (causes if isinstance(causes, list) else [])[:8]:
        if not isinstance(cause, dict):
            continue
        field = cause.get("field", "")
        reason = cause.get("reason", "")
        if isinstance(field, str) and re.fullmatch(r"[A-Za-z0-9_.\[\]-]{1,256}", field):
            details.append(f"field={field}")
        if isinstance(reason, str) and re.fullmatch(r"[A-Za-z]{1,64}", reason):
            details.append(f"cause={reason}")
    return ", ".join(details) or "status-without-safe-detail"


def scheduling_detail(pod):
    condition = next((item for item in pod.get("status", {}).get("conditions", [])
                      if item.get("type") == "PodScheduled"), None)
    if condition is None:
        return {"status": "Unknown", "categories": ["unavailable"]}
    status = condition.get("status")
    if status != "False":
        return {"status": status if status in ["True", "Unknown"] else "Unknown", "categories": []}
    message = condition.get("message", "")
    message = message.lower() if isinstance(message, str) else ""
    patterns = [
        ("insufficient cpu", "insufficient-cpu"),
        ("insufficient memory", "insufficient-memory"),
        ("insufficient ephemeral-storage", "insufficient-ephemeral-storage"),
        ("too many pods", "pod-capacity"),
        ("node affinity/selector", "node-selection"),
        ("didn't match node selector", "node-selection"),
        ("untolerated taint", "untolerated-taint"),
        ("volume node affinity conflict", "volume-affinity"),
        ("unbound immediate persistentvolumeclaims", "unbound-volume"),
        ("pod anti-affinity", "pod-affinity"),
    ]
    categories = sorted({category for pattern, category in patterns if pattern in message})
    return {"status": "False", "categories": categories or ["unclassified"]}


class Api:
    def __init__(self, server, context, token=None):
        endpoint = urllib.parse.urlsplit(server)
        require(endpoint.scheme == "https" and not endpoint.username and not endpoint.password,
                "Kubernetes endpoint must be authenticated HTTPS")
        self.host = endpoint.hostname
        self.port = endpoint.port or 443
        self.context = context
        self.token = token
        self.server = server

    def request(self, method, path, body=None, expected=(200,), patch_type=None):
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if body is not None:
            headers["Content-Type"] = patch_type or "application/json"
        connection = http.client.HTTPSConnection(
            self.host, self.port, context=self.context, timeout=15,
        )
        try:
            connection.request(method, path, body=None if body is None else json.dumps(body),
                               headers=headers)
            response = connection.getresponse()
            raw = response.read(2 * 1024 * 1024)
            require(len(raw) < 2 * 1024 * 1024, "Kubernetes response exceeded the bound")
            value = json.loads(raw) if raw else {}
            if response.status not in expected:
                raise Failure(
                    f"Kubernetes {method} {path.split('?')[0]} returned {response.status} ({status_detail(value)})")
            return response.status, value
        finally:
            connection.close()

    def get(self, path):
        return self.request("GET", path)[1]

    def optional(self, path):
        status, body = self.request("GET", path, expected=(200, 404))
        return body if status == 200 else None

    def create(self, path, value, expected=(201,)):
        return self.request("POST", path, value, expected)[1]

    def patch(self, path, value):
        current = self.get(path)
        metadata = value.setdefault("metadata", {})
        metadata.update(uid=uid(current), resourceVersion=current["metadata"]["resourceVersion"])
        return self.request("PATCH", path, value, patch_type="application/merge-patch+json")[1]

    def delete(self, path):
        current = self.get(path)
        return self.request("DELETE", path, {
            "apiVersion": "v1", "kind": "DeleteOptions",
            "preconditions": {"uid": uid(current),
                             "resourceVersion": current["metadata"]["resourceVersion"]},
        }, expected=(200, 202))[1]


class Setup:
    def __init__(self):
        require(os.environ.get("GITHUB_ACTIONS") == "true",
                "Native qualification runs only in its disposable hosted job")
        config = json.loads(command("kubectl", "config", "view", "--minify", "--raw", "-o", "json"))
        cluster = config["clusters"][0]["cluster"]
        user = config["users"][0]["user"]
        self.ca = base64.b64decode(cluster["certificate-authority-data"]).decode()
        certificate = private_file("admin.crt", base64.b64decode(user["client-certificate-data"]).decode())
        key = private_file("admin.key", base64.b64decode(user["client-key-data"]).decode())
        context = ssl.create_default_context(cadata=self.ca)
        context.load_cert_chain(certificate, key)
        self.admin = Api(cluster["server"], context)
        self.cluster = cluster

    def actor(self, namespace, name):
        token = self.admin.create(core(namespace, "serviceaccounts", name) + "/token", {
            "apiVersion": "authentication.k8s.io/v1", "kind": "TokenRequest",
            "spec": {"expirationSeconds": 3600},
        })["status"]["token"]
        # A fresh TLS context is essential: an admin client certificate would
        # take precedence over this bearer and invalidate every RBAC assertion.
        return Api(self.cluster["server"], ssl.create_default_context(cadata=self.ca), token)

    def namespace(self, name):
        return self.admin.create("/api/v1/namespaces", {
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name},
        })

    def account(self, namespace, name):
        return self.admin.create(core(namespace, "serviceaccounts"), {
            "apiVersion": "v1", "kind": "ServiceAccount",
            "metadata": {"name": name, "namespace": namespace},
        })

    def grant(self, namespace, writer, keys=None):
        workspace = self.admin.get(f"/api/v1/namespaces/{namespace}")
        result = self.admin.create(resource(namespace, "karscredentialgrants"), {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
            "metadata": {"name": "workspace", "namespace": namespace},
            "spec": {"workspaceUid": uid(workspace), "enabled": True,
                     "writers": [{"namespace": BRIDGE, "name": WRITER, "uid": uid(writer)}],
                     "agentKeys": keys or ["SLACK_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"],
                     "integrationStores": [], "legacyImports": [],
                     "observationTargets": [], "githubConnections": []},
        })
        self.ready_grant(namespace)
        return result

    def ready_grant(self, namespace):
        def ready():
            grant = self.admin.get(resource(namespace, "karscredentialgrants", "workspace"))
            status = grant.get("status", {})
            return grant if (
                status.get("phase") == "Ready"
                and status.get("observedGeneration") == grant["metadata"]["generation"]
                and any(condition["type"] == "WriterReady" and condition["status"] == "True"
                        for condition in status.get("conditions", []))
            ) else None
        return until(f"current native grant and writer in {namespace}", ready)

    def audit(self, namespace=None):
        raw = command("docker", "exec", "bridge-native-control-plane",
                      "cat", "/var/log/kars-native-audit/audit.log")
        return [
            event for line in raw.splitlines() if line
            for event in [json.loads(line)]
            if event.get("stage") == "ResponseComplete"
            and event.get("user", {}).get("username") == f"system:serviceaccount:{BRIDGE}:{WRITER}"
            and (namespace is None or event.get("objectRef", {}).get("namespace") == namespace)
        ]

    def audit_barrier(self, actor, namespace):
        actor.get(resource(namespace, "karscredentialgrants", "workspace"))
        time.sleep(1)
        return self.audit(namespace)
