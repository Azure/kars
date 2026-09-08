# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
import contextlib
import hashlib
import inspect
import json
import os
from pathlib import Path
import re
import socket
import signal
import ssl
import subprocess
import time

CONTEXT = "kind-kars-e2e"
SYSTEM = "kars-system"
RUNTIME = "kars-sre"
OPERATORS = "e2e-sre-operators"
TENANT = "e2e-sre-tenant"
REGISTRATION = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical"
PRIVATE = "sre-api-router-identity"
AGENT = "sre-api-agent"
CLAIM_VERSION = "kars.azure.com/namespace-claim-version"
SOURCE_NS = "kars.azure.com/sandbox-namespace"
SOURCE_NAME = "kars.azure.com/sandbox-name"
SOURCE_UID = "kars.azure.com/sandbox-uid"
NAMESPACE_UID = "kars.azure.com/namespace-uid"
EPOCH = "kars.azure.com/sre-privacy-epoch"
OWNER = "kars.azure.com/sre-registration-uid"
PRIVACY_REVISION = "kars.azure.com/sre-privacy/v2"
FIELD_MANAGER = "kars-controller/karssandbox"
STANDIN = "kars-sandbox-e2e:dev"
POLICIES = (
    "source-authority", "registration-authority", "private-identity",
    "binding-authority", "private-material", "source-retirement",
    "pending-proposals", "consumer-authority", "private-mounts",
    "private-workloads", "private-cronjobs", "private-connect", "role-authority", "no-legacy-tokens",
)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def command_site():
    # Only source coordinates from our harness, never argv, frame locals,
    # absolute filesystem paths or a traceback containing API response data.
    frame = inspect.currentframe()
    try:
        while frame:
            source = Path(frame.f_code.co_filename)
            if source.parent == Path(__file__).parent and source.name != "common.py":
                return f"{source.name}:{frame.f_code.co_name}:{frame.f_lineno}"
            frame = frame.f_back
        return "harness"
    finally:
        del frame


def command_error_category(stderr):
    status = re.search(r"Error from server \((Forbidden|Unauthorized|Invalid|NotFound|"
                       r"AlreadyExists|Conflict|BadRequest|InternalError|ServiceUnavailable)\)", stderr)
    if status:
        return status.group(1)
    if re.search(r'The CustomResourceDefinition "[^"\r\n]+" is invalid:', stderr):
        return "Invalid"
    text = stderr.lower()
    for needle, category in (("error validating", "schema-validation"),
                             ("unknown flag", "cli-argument"),
                             ("required value", "required-field"),
                             ("no matches for kind", "api-discovery"),
                             ("the server doesn't have a resource type", "api-discovery"),
                             ("timed out waiting", "wait-timeout")):
        if needle in text:
            return category
    return "unclassified"


def printed_object(output):
    start = output.find("{")
    require(start >= 0, "CLI omitted its JSON object")
    value, _ = json.JSONDecoder().raw_decode(output[start:])
    require(isinstance(value, dict), "CLI returned a non-object JSON value")
    return value


def enrollment_json(output):
    value = printed_object(output)
    require(all(key in value for key in ("controller", "sandbox", "runtimeNamespace", "legacyBindings")),
            "CLI preview returned an unexpected object")
    return value


def assert_claim(source, namespace, core, system_uid):
    require(source and namespace and core, "Source/namespace/core identity is missing")
    src, ns = source["metadata"], namespace["metadata"]
    require(src.get("uid") and ns.get("uid") and src.get("resourceVersion") and ns.get("resourceVersion"),
            "Source or namespace lacks its real API identity")
    require(src.get("namespace") == SYSTEM and src.get("name") == "sre" and ns.get("name") == RUNTIME,
            "Claim points outside the canonical source/runtime namespace")
    require(not src.get("deletionTimestamp") and not ns.get("deletionTimestamp") and not ns.get("ownerReferences"),
            "Claim is terminating or has a foreign owner")
    annotations = ns.get("annotations", {})
    require(all(annotations.get(key) == value for key, value in {
        CLAIM_VERSION: "v1", SOURCE_NS: SYSTEM, SOURCE_NAME: "sre", SOURCE_UID: src["uid"],
    }.items()) and "kars.azure.com/namespace-prestage" not in annotations,
        "Actual namespace lacks the complete current-source claim")
    require(src.get("annotations", {}).get(NAMESPACE_UID) == ns["uid"],
            "Actual source lacks the exact runtime-namespace UID backlink")
    require(core["metadata"].get("uid") == system_uid, "Core namespace identity changed")


def review_args(spec, registration=None):
    args = ["--sandbox-uid", spec["sandbox"]["uid"],
            "--namespace-uid", spec["runtimeNamespace"]["uid"]]
    for binding in spec["legacyBindings"]:
        args += ["--binding", f'{binding["kind"]}/{binding.get("namespace", "")}/{binding["name"]}='
                 f'{binding["uid"]}@{binding["resourceVersion"]}']
    if spec.get("legacyConsumer"):
        consumer = spec["legacyConsumer"]
        args += ["--consumer", f'{consumer["uid"]}@{consumer["resourceVersion"]}']
    if registration:
        args += ["--registration-uid", registration["metadata"]["uid"],
                 "--resource-version", registration["metadata"]["resourceVersion"]]
    return args


def assert_denial(response, label, policy=None):
    # A missing CRD/resource, transport failure or malformed fixture is NOT proof.
    require(response.status_code == 403, f"{label}: expected Forbidden, got HTTP {response.status_code}")
    body = response.json()
    require(body.get("kind") == "Status" and body.get("reason") == "Forbidden",
            f"{label}: response was not a Kubernetes authorization/admission denial")
    if policy:
        require(policy in body.get("message", ""), f"{label}: wrong admission policy rejected the fixture")


class Harness:
    def __init__(self, phase):
        import httpx
        os.umask(0o077)
        self.httpx = httpx
        self.root = Path(__file__).resolve().parents[3]
        self.work = self.root / ".e2e-sre-authority"
        self.work.mkdir(mode=0o700, exist_ok=True)
        os.chmod(self.work, 0o700)
        self.deadline = time.monotonic() + 650
        self.phase = phase
        self.clients = {}
        self.processes = []
        self.state_path = self.work / "state.json"
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {}
        config_path = self.work / "admin.json"
        if not config_path.exists():
            raw = self.run(["kubectl", "--context", CONTEXT, "--request-timeout=15s",
                            "config", "view", "--raw", "--minify", "-o", "json"], timeout=20)
            config = json.loads(raw)
            require(config["contexts"][0]["name"] == CONTEXT, "Refusing a non-Kind Kubernetes context")
            config["current-context"] = CONTEXT
            cluster = config["clusters"][0]["cluster"]
            from urllib.parse import urlsplit
            require(urlsplit(cluster["server"]).hostname in ("127.0.0.1", "::1", "localhost"),
                    "Refusing a non-loopback Kind API server")
            self.write("admin.json", json.dumps(config))
        self.config = json.loads(config_path.read_text())
        cluster = self.config["clusters"][0]["cluster"]
        self.server = cluster["server"]
        self.write("api-ca.pem", base64.b64decode(cluster["certificate-authority-data"]))
        admin = self.config["users"][0]["user"]
        require("client-certificate-data" in admin and "client-key-data" in admin,
                "Kind admin configuration must use its client certificate")
        self.write("admin-cert.pem", base64.b64decode(admin["client-certificate-data"]))
        self.write("admin-key.pem", base64.b64decode(admin["client-key-data"]))

    def write(self, name, value):
        path = self.work / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(value if isinstance(value, bytes) else value.encode())
        os.chmod(path, 0o600)
        return path

    def save(self):
        self.write("state.json", json.dumps(self.state, indent=2))

    def run(self, args, *, data=None, user="admin", timeout=35, expected=0):
        env = {**os.environ, "KARS_KUBE_CONTEXT": CONTEXT, "NO_COLOR": "1", "FORCE_COLOR": "0"}
        config = self.work / f"{user}.json"
        if config.exists():
            env["KUBECONFIG"] = str(config)
        else:
            require(user == "admin", "Probe principal has no isolated kubeconfig")
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "SRE acceptance phase exceeded its bounded deadline")
        process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, cwd=self.root,
                                   env=env, start_new_session=True)
        try:
            stdout, stderr = process.communicate(input=data, timeout=min(timeout, remaining))
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=5)
            raise AssertionError(f"Command exceeded its bounded timeout at {command_site()}") from None
        result = subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
        if expected is not None:
            # Never echo command output or argv: token/Secret reads are captured.
            require(result.returncode == expected,
                    f"Command failed during {self.phase} at {command_site()}; "
                    f"exit={result.returncode}; category={command_error_category(stderr)}")
        return result if expected is None else result.stdout

    def k(self, *args, data=None, user="admin", timeout=35, expected=0):
        request_timeout = max(1, int(min(timeout - 2, self.deadline - time.monotonic())))
        return self.run(["kubectl", "--context", CONTEXT, f"--request-timeout={request_timeout}s", *args],
                        data=data, user=user, timeout=timeout, expected=expected)

    def cli(self, *args, user="admin", expected=0, timeout=240):
        return self.run(["node", str(self.root / "cli/dist/index.js"), "sre", *args,
                         "--context", CONTEXT, "--namespace", SYSTEM, "--release", "kars"],
                        user=user, expected=expected, timeout=timeout)

    def client(self, user="admin"):
        if user not in self.clients:
            context = ssl.create_default_context(cafile=str(self.work / "api-ca.pem"))
            headers = {"Accept": "application/json"}
            if user == "admin":
                context.load_cert_chain(str(self.work / "admin-cert.pem"), str(self.work / "admin-key.pem"))
            else:
                # Deliberately separate SSLContext: wrong-token probes MUST NOT
                # retain an admin certificate, exec plugin or auth-provider.
                config = json.loads((self.work / f"{user}.json").read_text())
                account = config["users"][0]["user"]
                require(set(account) == {"token"}, "Non-admin probe retained privileged kubeconfig auth")
                headers["Authorization"] = f'Bearer {account["token"]}'
            self.clients[user] = self.httpx.Client(base_url=self.server, verify=context,
                                                 headers=headers, timeout=15, trust_env=False)
        return self.clients[user]

    def api(self, method, path, *, body=None, user="admin", status=None):
        headers = {"Content-Type": "application/merge-patch+json"} if method == "PATCH" else None
        response = self.client(user).request(method, path, json=body, headers=headers)
        if status is not None:
            require(response.status_code in (status if isinstance(status, tuple) else (status,)),
                    f"{method} {path.split('?')[0]}: HTTP {response.status_code}, expected {status}")
        return response

    def get(self, kind, name, namespace=None):
        args = ["get", kind, name, "--ignore-not-found", "-o", "json"]
        if namespace:
            args += ["-n", namespace]
        raw = self.k(*args)
        return json.loads(raw) if raw.strip() else None

    def create(self, obj, manager=None):
        args = ["create", "-f", "-", "-o", "json"]
        if manager:
            args += ["--field-manager", manager]
        return json.loads(self.k(*args, data=json.dumps(obj)))

    def token_identity(self, name, namespace, account):
        token = self.k("create", "token", account, "-n", namespace, "--duration=1h").strip()
        require(token, "TokenRequest returned no credential")
        return self.token_config(name, token)

    def token_config(self, name, token):
        cluster = self.config["clusters"][0]
        self.write(f"{name}.json", json.dumps({
            "apiVersion": "v1", "kind": "Config", "current-context": CONTEXT,
            "clusters": [cluster], "users": [{"name": name, "user": {"token": token}}],
            "contexts": [{"name": CONTEXT, "context": {"cluster": cluster["name"], "user": name}}],
        }))
        old = self.clients.pop(name, None)
        if old:
            old.close()

    def poll(self, label, predicate, seconds=150, interval=1):
        end = min(self.deadline, time.monotonic() + seconds)
        while time.monotonic() < end:
            value = predicate()
            if value:
                return value
            time.sleep(interval)
        raise AssertionError(f"{label} did not converge before its deadline")

    def wait_ready(self):
        def ready():
            reg = self.get("karssreregistrations.kars.azure.com", "canonical")
            if not reg:
                return False
            status = reg.get("status", {})
            require(not (status.get("phase") == "Blocked" and status.get("observedGeneration") == reg["metadata"]["generation"]),
                    "SRE authority reported Blocked; see sanitized status diagnostics")
            return reg if (status.get("phase") == "Ready"
                and status.get("observedGeneration") == reg["metadata"]["generation"]
                and status.get("privacyRevision") == PRIVACY_REVISION
                and status.get("legacySecretAccessDenied") is True) else False
        return self.poll("SRE Ready", ready, seconds=240)

    def namespace_delete(self, name, uid):
        obj = self.get("namespace", name)
        require(obj and obj["metadata"]["uid"] == uid, "Fixture namespace changed before cleanup")
        self.api("DELETE", f"/api/v1/namespaces/{name}", body={
            "apiVersion": "v1", "kind": "DeleteOptions",
            "preconditions": {"uid": uid, "resourceVersion": obj["metadata"]["resourceVersion"]},
        }, status=(200, 202))
        self.poll(f"namespace {name} cleanup", lambda: self.get("namespace", name) is None, seconds=120)

    @contextlib.contextmanager
    def port_forward(self, pod):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        log = self.work / "port-forward.log"
        with log.open("w") as output:
            process = subprocess.Popen(["kubectl", "--context", CONTEXT, "--request-timeout=20s",
                "-n", RUNTIME, "port-forward", f"pod/{pod}", f"{port}:9446", "--address=127.0.0.1"],
                cwd=self.root, env={**os.environ, "KUBECONFIG": str(self.work / "registrar.json")},
                stdout=output, stderr=output)
            self.processes.append(process)
            try:
                self.poll("private TLS port-forward", lambda: process.poll() is None
                          and log.exists() and "Forwarding from" in log.read_text(), seconds=25)
                yield port
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                self.processes.remove(process)

    def passed(self, message):
        print(f"SRE-PASS {message}", flush=True)

    def diagnostics(self):
        self.deadline = max(self.deadline, time.monotonic() + 50)
        # Status and identities only; never dump Secret bodies or whole Pods.
        for kind, name, namespace in [("karssreregistrations.kars.azure.com", "canonical", None),
                                      ("karssandbox", "sre", SYSTEM), ("deployment", "sre", RUNTIME)]:
            try:
                obj = self.get(kind, name, namespace)
                if obj:
                    status = obj.get("status", {})
                    print("SRE-DIAG", json.dumps({"kind": kind, "name": name,
                        "uid": obj["metadata"].get("uid"), "phase": status.get("phase"),
                        "observedGeneration": status.get("observedGeneration"),
                        "generation": obj["metadata"].get("generation"),
                        "availableReplicas": status.get("availableReplicas"),
                        "conditions": [{"type": condition.get("type"), "status": condition.get("status"),
                                        "reason": condition.get("reason")}
                                       for condition in status.get("conditions", [])]}), flush=True)
            except Exception:
                print(f"SRE-DIAG {kind}/{name} unavailable", flush=True)
        try:
            pods = json.loads(self.k("get", "pods", "-n", RUNTIME, "-o", "json", timeout=10))["items"]
            for pod in pods:
                status = pod.get("status", {})
                containers = status.get("initContainerStatuses", []) + status.get("containerStatuses", [])
                print("SRE-DIAG", json.dumps({"kind": "Pod", "name": pod["metadata"]["name"],
                    "uid": pod["metadata"]["uid"], "phase": status.get("phase"),
                    "containers": [{"name": container["name"], "ready": container.get("ready"),
                        "state": {kind: {key: value.get(key) for key in ("reason", "exitCode") if key in value}
                                  for kind, value in container.get("state", {}).items()}}
                        for container in containers]}), flush=True)
        except Exception:
            print("SRE-DIAG runtime Pod status unavailable", flush=True)

    def close(self):
        for process in self.processes:
            process.terminate()
        for client in self.clients.values():
            client.close()


def fingerprint(secret, key="control-token"):
    return hashlib.sha256(base64.b64decode(secret["data"][key])).hexdigest()
