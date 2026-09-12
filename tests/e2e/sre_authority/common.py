# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
import contextlib
import copy
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
from .schema_preparation_diagnostics import schema_preparation_failure

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
    stages = {
        "registrar", "controller-review", "release-inventory", "prerequisite-chart-render",
        "action-schema-review", "helm-compatibility", "action-schema-migration",
        "core-schema-preparation", "helm-server-dry-run", "helm-upgrade",
        "template-ownership-review", "schema-publication", "template-authority-write",
        "controller-rollout",
    }
    observed = set(re.findall(r"^SRE-STAGE-FAILURE ([a-z-]+)$", stderr, re.MULTILINE)) & stages
    if observed:
        return "sre-stage:" + (next(iter(observed)) if len(observed) == 1 else "ambiguous")
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
                             ("conflict occurred while applying object", "server-side-apply-conflict"),
                             ("the server doesn't have a resource type", "api-discovery"),
                             ("timed out waiting", "wait-timeout")):
        if needle in text:
            return category
    return "unclassified"


def authority_failure_site(root, detail):
    """Report checked-in rejection coordinates, never the status detail itself."""
    if not isinstance(detail, str) or not detail:
        return {"category": "unclassified"}
    result = {"category": "controller-rejection"}
    message = detail
    status = re.fullmatch(r"(.+): Kubernetes status ([1-5][0-9]{2})", detail)
    if status:
        message = status.group(1)
        result = {"category": "kubernetes-status", "httpStatus": int(status.group(2))}
    elif detail.endswith(": Kubernetes transport/serialization failure"):
        message = detail.removesuffix(": Kubernetes transport/serialization failure")
        result = {"category": "kubernetes-transport"}
    # Only an exact literal in our own production source can identify a site.
    # Untrusted API messages, names, credentials and interpolated suffixes are
    # not echoed, even if they contain a familiar rejection substring.
    literal = json.dumps(message, ensure_ascii=False)
    paths = [root / "controller/src/sre_authority.rs", root / "controller/src/sre_registration.rs",
             root / "shared/sre_privacy.rs"]
    paths += sorted((root / "controller/src/sre_authority").glob("*.rs"))
    for path in paths:
        if path.name.endswith("tests.rs"):
            continue
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if literal in line:
                return {**result, "source": str(path.relative_to(root)), "line": number}
    return {"category": "unclassified"}


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
            if result.returncode != expected:
                facts = schema_preparation_failure(stderr)
                if facts:
                    print("SRE-SCHEMA-PREPARATION-FACTS " + json.dumps(facts, sort_keys=True), flush=True)
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
        from .bootstrap_diagnostics import failure_facts
        self.deadline = max(self.deadline, time.monotonic() + 90)
        # Status and identities only; never dump Secret bodies or whole Pods.
        for kind, name, namespace in [("karssreregistrations.kars.azure.com", "canonical", None),
                                      ("karssandbox", "sre", SYSTEM), ("deployment", "sre", RUNTIME),
                                      ("namespace", RUNTIME, None)]:
            try:
                obj = self.get(kind, name, namespace)
                if obj:
                    status = obj.get("status", {})
                    print("SRE-DIAG", json.dumps({"kind": kind, "name": name,
                        "uid": obj["metadata"].get("uid"), "phase": status.get("phase"),
                        "observedGeneration": status.get("observedGeneration"),
                        "generation": obj["metadata"].get("generation"),
                        "availableReplicas": status.get("availableReplicas"),
                        **({"authorityFailure": authority_failure_site(self.root, status.get("detail"))}
                           if kind == "karssreregistrations.kars.azure.com" and status.get("phase") == "Blocked" else {}),
                        "conditions": [{"type": condition.get("type"), "status": condition.get("status"),
                                        "reason": condition.get("reason"),
                                        **failure_facts(condition.get("message"),
                                                        {f"kars-sre-{name}": {} for name in POLICIES})}
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
                        "restartCount": container.get("restartCount"),
                        "state": {kind: {key: value.get(key) for key in ("reason", "exitCode") if key in value}
                                  for kind, value in container.get("state", {}).items()}}
                        for container in containers]}), flush=True)
                router = next((container for container in status.get("containerStatuses", [])
                               if container["name"] == "inference-router"), None)
                if router and not router.get("ready") and "running" in router.get("state", {}):
                    self.readiness_diagnostics(pod)
                elif router and router.get("ready"):
                    from .bootstrap_diagnostics import router_readiness_facts
                    try:
                        logs = self.k("logs", "-n", RUNTIME, pod["metadata"]["name"], "-c", "inference-router",
                                      "--tail=150", timeout=10)
                        print("SRE-DIAG", json.dumps({"kind": "RouterAuthorityLog", "podUid": pod["metadata"]["uid"],
                            "authorityChecks": router_readiness_facts(logs)}), flush=True)
                    except Exception:
                        print("SRE-DIAG Ready router authority log unavailable", flush=True)
        except Exception:
            print("SRE-DIAG runtime Pod status unavailable", flush=True)

    def readiness_diagnostics(self, pod):
        from .bootstrap_diagnostics import probe_command_result, router_log_summary, router_readiness_facts
        facts = {"kind": "RouterReadiness", "podUid": pod["metadata"]["uid"]}
        router = next(item for item in pod["spec"]["containers"] if item["name"] == "inference-router")
        facts["privateApiEnabled"] = any(entry.get("name") == "KARS_SRE_API_ENABLED"
                                        and entry.get("value") == "true" for entry in router.get("env", []))
        facts["routerUid1001"] = router.get("securityContext", {}).get("runAsUser") == 1001
        facts["expectedImage"] = router.get("image") == "kars-inference-router:e2e"
        facts["progressLoggingEnabled"] = any(entry.get("name") == "RUST_LOG"
                                            and "inference_router=debug" in entry.get("value", "")
                                            for entry in router.get("env", []))
        try:
            facts["apiConnectivity"] = self.connectivity_diagnostics(pod)
        except Exception as error:
            facts["connectivityDiagnosticError"] = type(error).__name__
        try:
            source = self.get("karssandbox", "sre", SYSTEM)
            policy = self.get("networkpolicy", "sandbox-policy", RUNTIME)
            service = self.get("service", "kubernetes", "default")
            service_ip = service["spec"]["clusterIP"]
            rules = policy.get("spec", {}).get("egress", []) if policy else []
            facts["sourceLabelSre"] = source.get("metadata", {}).get("labels", {}).get("kars.azure.com/role") == "sre"
            facts["explicitApiServiceRule"] = any(
                any(peer.get("ipBlock", {}).get("cidr") == f"{service_ip}/32" for peer in rule.get("to", []))
                and any(port.get("port") == 443 and port.get("protocol", "TCP") == "TCP"
                        for port in rule.get("ports", [])) for rule in rules)
            agents = json.loads(self.k("get", "daemonsets", "-n", "kube-system", "-o", "json"))["items"]
            facts["knownNetworkAgents"] = sorted(agent["metadata"]["name"] for agent in agents
                if agent["metadata"]["name"] in ("kindnet", "cilium", "calico-node", "antrea-agent", "kube-flannel-ds"))
            facts["minimalKindnetImage"] = any(
                container.get("image", "").removeprefix("docker.io/").startswith("kindest/kindnetd:")
                for agent in agents if agent["metadata"]["name"] == "kindnet"
                for container in agent.get("spec", {}).get("template", {}).get("spec", {}).get("containers", []))
        except Exception as error:
            facts["networkPolicyDiagnosticError"] = type(error).__name__
        try:
            facts["firewall"] = self.firewall_diagnostics(pod)
        except Exception as error:
            facts["firewallDiagnosticError"] = type(error).__name__
        try:
            for label, executable in (("configuredCommand", "kars-inference-router"),
                                      ("absoluteCommand", "/usr/local/bin/kars-inference-router")):
                result = self.k("exec", "-n", RUNTIME, pod["metadata"]["name"], "-c", "inference-router",
                                "--", executable, "sre-ready", expected=None, timeout=10)
                facts[label] = probe_command_result(result.returncode, result.stdout + result.stderr)
            ca = self.k("exec", "-n", RUNTIME, pod["metadata"]["name"], "-c", "agent",
                        "--", "cat", "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt", timeout=10)
            context = ssl.create_default_context(cadata=ca)
            with self.port_forward(pod["metadata"]["name"]) as port, \
                    self.httpx.Client(verify=context, timeout=25, trust_env=False) as client:
                response = client.get(f"https://127.0.0.1:{port}/readyz")
                facts["verifiedLoopbackTlsStatus"] = response.status_code
        except Exception as error:
            facts["diagnosticError"] = type(error).__name__
        try:
            logs = self.k("logs", "-n", RUNTIME, pod["metadata"]["name"], "-c", "inference-router",
                          "--tail=150", timeout=10)
            facts["authorityChecks"] = router_readiness_facts(logs)
            facts["logSummary"] = router_log_summary(logs, self.root)
        except Exception:
            facts["authorityChecksUnavailable"] = True
        print("SRE-DIAG", json.dumps(facts), flush=True)

    def firewall_diagnostics(self, pod):
        from .bootstrap_diagnostics import firewall_summary
        node = pod["spec"].get("nodeName")
        require(node in ("kars-e2e-worker", "kars-e2e-control-plane")
                and not pod["spec"].get("hostNetwork"), "Firewall diagnostic requires the owned Kind Pod network")
        cluster = self.run(["docker", "inspect", node, "--format",
                            '{{index .Config.Labels "io.x-k8s.kind.cluster"}}'], timeout=10).strip()
        require(cluster == "kars-e2e", "Firewall diagnostic refuses a foreign node")
        items = json.loads(self.run(["docker", "exec", node, "crictl", "pods",
                                    "--label", f'io.kubernetes.pod.uid={pod["metadata"]["uid"]}',
                                    "-o", "json"], timeout=10))["items"]
        matches = [item for item in items if item.get("metadata", {}).get("uid") == pod["metadata"]["uid"]
                   and item["metadata"].get("namespace") == RUNTIME and item.get("state") == "SANDBOX_READY"]
        require(len(matches) == 1 and re.fullmatch(r"[0-9a-f]{64}", matches[0]["id"]),
                "Firewall diagnostic could not identify the exact live Pod sandbox")
        sandbox = matches[0]["id"]
        def identity():
            obj = json.loads(self.run(["docker", "exec", node, "crictl", "inspectp", "-o", "json", sandbox], timeout=10))
            require(obj.get("status", {}).get("metadata", {}).get("uid") == pod["metadata"]["uid"],
                    "Firewall diagnostic sandbox UID changed")
            pid = obj.get("info", {}).get("pid")
            require(type(pid) is int and 0 < pid < 2**31, "Firewall diagnostic sandbox PID is invalid")
            return pid
        pid = identity()
        facts = {}
        for backend in ("nft", "legacy"):
            output = self.run(["docker", "exec", node, "nsenter", "--target", str(pid), "--net", "--",
                               f"iptables-{backend}-save", "-c"], timeout=10, expected=None)
            facts[backend] = firewall_summary(output.stdout) if output.returncode == 0 else {"available": False}
        require(identity() == pid, "Firewall diagnostic sandbox changed during the read")
        return facts

    def connectivity_diagnostics(self, pod):
        import ipaddress
        service = self.get("service", "kubernetes", "default")
        endpoint = self.get("endpoints", "kubernetes", "default")
        service_ip = service["spec"]["clusterIP"]
        service_port = next(port["port"] for port in service["spec"]["ports"] if port["name"] == "https")
        subset = endpoint["subsets"][0]
        endpoint_ip = subset["addresses"][0]["ip"]
        endpoint_port = next(port["port"] for port in subset["ports"] if port["name"] == "https")
        require(all(ipaddress.ip_address(address).is_private for address in (service_ip, endpoint_ip))
                and all(type(port) is int and 0 < port < 65536 for port in (service_port, endpoint_port)),
                "Connectivity diagnostic refuses non-private or invalid API targets")
        current = self.get("pod", pod["metadata"]["name"], RUNTIME)
        require(current["metadata"]["uid"] == pod["metadata"]["uid"],
                "Diagnostic Pod changed before connectivity inspection")
        require(current["spec"].get("automountServiceAccountToken") is False
                and current["spec"].get("shareProcessNamespace") is not True,
                "Connectivity diagnostic requires isolated processes and no ambient token")
        name = "sre-e2e-network-diagnostic"
        existing = current["spec"].get("ephemeralContainers", [])
        require(not any(item["name"] == name for item in existing), "Diagnostic container name is already occupied")
        script = """
if ! command -v timeout >/dev/null || ! command -v bash >/dev/null || ! command -v id >/dev/null; then
  printf '{"available":false}\\n'; exit 0
fi
uid=false
[ "$(id -u)" = 1001 ] && uid=true
timeout 6 bash -c 'exec 3<>/dev/tcp/"$1"/"$2"' sre-tcp "$1" "$2" >/dev/null 2>&1
service=$?
timeout 6 bash -c 'exec 3<>/dev/tcp/"$1"/"$2"' sre-tcp "$3" "$4" >/dev/null 2>&1
endpoint=$?
printf '{"available":true,"uidMatches1001":%s,"serviceExit":%s,"endpointExit":%s}\\n' "$uid" "$service" "$endpoint"
"""
        probe = {"name": name, "image": STANDIN, "imagePullPolicy": "IfNotPresent",
                 "command": ["/bin/sh", "-c", script, "sre-connectivity", service_ip, str(service_port),
                             endpoint_ip, str(endpoint_port)],
                 "securityContext": {"runAsUser": 1001, "runAsNonRoot": True,
                                     "allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
                                     "capabilities": {"drop": ["ALL"]}}}
        self.api("PATCH", f"/api/v1/namespaces/{RUNTIME}/pods/{pod['metadata']['name']}/ephemeralcontainers",
                 body={"metadata": {"uid": current["metadata"]["uid"],
                                    "resourceVersion": current["metadata"]["resourceVersion"]},
                       "spec": {"ephemeralContainers": existing + [probe]}}, status=200)
        def completed():
            current = self.get("pod", pod["metadata"]["name"], RUNTIME)
            require(current and current["metadata"]["uid"] == pod["metadata"]["uid"],
                    "Diagnostic Pod changed during connectivity inspection")
            return any(item.get("name") == name and "terminated" in item.get("state", {})
                       for item in current.get("status", {}).get("ephemeralContainerStatuses", []))
        self.poll("bounded UID-1001 API TCP probes", completed, seconds=20, interval=0.5)
        result = self.parse_connectivity_result(self.k("logs", "-n", RUNTIME, pod["metadata"]["name"], "-c", name, "--tail=5"))
        try:
            result["sameNodeControl"] = self.control_connectivity_diagnostics(pod, probe)
        except Exception as error:
            result["controlDiagnosticError"] = type(error).__name__
        result["guardControls"] = {}
        for variant in ("full", "filter-only", "legacy-full"):
            try:
                result["guardControls"][variant] = self.control_connectivity_diagnostics(pod, probe, variant)
            except Exception as error:
                result["guardControls"][variant] = {"diagnosticError": type(error).__name__}
        try:
            result["policyControl"] = self.policy_connectivity_diagnostics(pod, probe)
        except Exception as error:
            result["policyControl"] = {"diagnosticError": type(error).__name__}
        try:
            result["denyAllPolicyControl"] = self.policy_connectivity_diagnostics(pod, probe, deny_all=True)
        except Exception as error:
            result["denyAllPolicyControl"] = {"diagnosticError": type(error).__name__}
        try:
            result["apiEndpointPolicyControl"] = self.policy_connectivity_diagnostics(pod, probe, api_endpoint=True)
        except Exception as error:
            result["apiEndpointPolicyControl"] = {"diagnosticError": type(error).__name__}
        return result

    @staticmethod
    def parse_connectivity_result(output, uid=1001):
        require(uid in (1000, 1001), "Unsupported connectivity probe UID")
        uid_key = f"uidMatches{uid}"
        result = json.loads(output)
        require(isinstance(result, dict) and type(result.get("available")) is bool
                and set(result) == ({"available", uid_key, "serviceExit", "endpointExit"}
                                    if result["available"] else {"available"})
                and (not result["available"] or type(result.get(uid_key)) is bool)
                and all(type(value) is int and 0 <= value <= 255
                        for key, value in result.items() if key not in ("available", uid_key)),
                "Connectivity diagnostic produced an unexpected result")
        return result

    def control_connectivity_diagnostics(self, pod, container, variant=None, *, policy=False, deny_all=False, api_endpoint=False):
        from .network_diagnostics import guard_variant
        suffix = "-endpoint" if api_endpoint else "-deny-all" if deny_all else "-policy" if policy else f"-{variant}" if variant else ""
        name = "sre-e2e-api-connectivity" + suffix
        require(pod["spec"].get("nodeName"), "Connectivity comparison requires the actual sandbox node")
        spec = {"nodeName": pod["spec"]["nodeName"], "automountServiceAccountToken": False,
                "restartPolicy": "Never", "securityContext": copy.deepcopy(pod["spec"].get("securityContext", {})),
                "containers": [copy.deepcopy(container)]}
        if variant or api_endpoint:
            spec["initContainers"] = [guard_variant(self.root, pod, variant or "full")]
        if api_endpoint:
            agent = copy.deepcopy(container)
            agent["name"] = "agent-network"
            agent["securityContext"]["runAsUser"] = 1000
            script = agent["command"][2]
            require(script.count("= 1001") == 1 and script.count("uidMatches1001") == 1,
                    "Agent network comparison requires the exact diagnostic script")
            agent["command"][2] = script.replace("= 1001", "= 1000").replace("uidMatches1001", "uidMatches1000")
            spec["containers"].append(agent)
        metadata = {"name": name, "namespace": TENANT}
        if policy:
            value = "endpoint" if api_endpoint else "deny-all" if deny_all else "true"
            metadata["labels"] = {"kars.azure.com/e2e-policy-control": value}
            for probe in spec["containers"]:
                probe["command"][2] = "sleep 10\n" + probe["command"][2]
        created = self.create({"apiVersion": "v1", "kind": "Pod", "metadata": metadata, "spec": spec})
        uid = created["metadata"]["uid"]
        try:
            def completed():
                current = self.get("pod", name, TENANT)
                require(current and current["metadata"]["uid"] == uid, "Connectivity control Pod was replaced")
                status = current.get("status", {})
                return status.get("phase") in ("Succeeded", "Failed") or any(
                    item.get("state", {}).get("terminated", {}).get("exitCode", 0) != 0
                    for item in status.get("initContainerStatuses", []))
            self.poll("same-node credential-free TCP comparison", completed, seconds=35, interval=0.5)
            current = self.get("pod", name, TENANT)
            for item in current.get("status", {}).get("initContainerStatuses", []):
                code = item.get("state", {}).get("terminated", {}).get("exitCode", 0)
                if code != 0:
                    return {"available": False, "initExit": code}
            result = self.parse_connectivity_result(
                self.k("logs", "-n", TENANT, name, "-c", container["name"], "--tail=5"))
            if api_endpoint:
                result["agentUid1000"] = self.parse_connectivity_result(
                    self.k("logs", "-n", TENANT, name, "-c", "agent-network", "--tail=5"), uid=1000)
            return result
        finally:
            try:
                current = self.get("pod", name, TENANT)
                require(current and current["metadata"]["uid"] == uid, "Connectivity control cleanup identity changed")
                self.api("DELETE", f"/api/v1/namespaces/{TENANT}/pods/{name}", body={
                    "apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": uid, "resourceVersion": current["metadata"]["resourceVersion"]}},
                    status=(200, 202))
            except Exception:
                print("SRE-DIAG owned connectivity control cleanup unavailable", flush=True)

    def policy_connectivity_diagnostics(self, pod, container, *, deny_all=False, api_endpoint=False):
        import ipaddress
        source = self.get("networkpolicy", "sandbox-policy", RUNTIME)
        require(source is not None, "SRE network policy is unavailable for a read-only comparison")
        spec = {"policyTypes": ["Egress"], "egress": []} if deny_all else copy.deepcopy(source["spec"])
        if api_endpoint:
            endpoint = self.get("endpoints", "kubernetes", "default")
            rules = []
            for subset in endpoint.get("subsets", []):
                for address in subset.get("addresses", []):
                    ip = ipaddress.ip_address(address["ip"])
                    require(ip.is_private and not ip.is_loopback, "Unexpected diagnostic API endpoint address")
                    for port in subset.get("ports", []):
                        if port.get("name") == "https" and port.get("protocol", "TCP") == "TCP":
                            require(type(port["port"]) is int and 0 < port["port"] < 65536,
                                    "Invalid diagnostic API endpoint port")
                            rules.append({"to": [{"ipBlock": {"cidr": f"{ip}/{ip.max_prefixlen}"}}],
                                          "ports": [{"protocol": "TCP", "port": port["port"]}]})
            require(0 < len(rules) <= 16, "Diagnostic API endpoint inventory is empty or unbounded")
            spec["egress"] = spec.get("egress", []) + rules
        value = "endpoint" if api_endpoint else "deny-all" if deny_all else "true"
        spec["podSelector"] = {"matchLabels": {"kars.azure.com/e2e-policy-control": value}}
        name = "sre-e2e-policy-control" + ("-endpoint" if api_endpoint else "-deny-all" if deny_all else "")
        created = self.create({"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                              "metadata": {"name": name, "namespace": TENANT}, "spec": spec})
        uid = created["metadata"]["uid"]
        try:
            return self.control_connectivity_diagnostics(pod, container, policy=True,
                                                         deny_all=deny_all, api_endpoint=api_endpoint)
        finally:
            try:
                current = self.get("networkpolicy", name, TENANT)
                require(current and current["metadata"]["uid"] == uid, "Connectivity policy cleanup identity changed")
                self.api("DELETE", f"/apis/networking.k8s.io/v1/namespaces/{TENANT}/networkpolicies/{name}", body={
                    "apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": uid, "resourceVersion": current["metadata"]["resourceVersion"]}},
                    status=(200, 202))
            except Exception:
                print("SRE-DIAG owned connectivity policy cleanup unavailable", flush=True)

    def close(self):
        for process in self.processes:
            process.terminate()
        for client in self.clients.values():
            client.close()


def fingerprint(secret, key="control-token"):
    return hashlib.sha256(base64.b64decode(secret["data"][key])).hexdigest()
