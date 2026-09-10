# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import importlib.util
import json
import os
import re
import sys
import types

from .common import AGENT, EPOCH, OPERATORS, PRIVATE, RUNTIME, STANDIN, SYSTEM, require
from .admission import runtime_denials
from .credential_paths import token_secret_denials

MARKER = "e2e-secret-value-must-not-cross-filter"
SA_PATH = "/var/run/secrets/kubernetes.io/serviceaccount"


def install_metrics(h):
    # Real metrics-server on the disposable Kind cluster. This is not a fake
    # metrics API and does not change any production chart or SRE policy.
    if not h.get("apiservice", "v1beta1.metrics.k8s.io"):
        h.k("apply", "-f", "https://github.com/kubernetes-sigs/metrics-server/releases/download/v0.7.2/components.yaml",
            timeout=90)
        h.k("patch", "deployment", "metrics-server", "-n", "kube-system", "--type=json", "-p",
            json.dumps([{"op": "add", "path": "/spec/template/spec/containers/0/args/-",
                         "value": "--kubelet-insecure-tls"}]))
        h.k("rollout", "status", "deployment/metrics-server", "-n", "kube-system", "--timeout=120s", timeout=130)
    def collected():
        response = h.api("GET", "/apis/metrics.k8s.io/v1beta1/nodes")
        return response.status_code == 200 and bool(response.json().get("items"))
    h.poll("real Kind node metrics", collected, seconds=100, interval=3)


def alive_pinned_source(h):
    source = h.get("karssandbox", "sre", SYSTEM)
    uid = source["metadata"]["uid"]
    # An explicit, valid BYO image pin exercises projection preservation. This
    # is a sleeping test consumer, not an old binary or a live model/agent run.
    h.k("patch", "karssandbox", "sre", "-n", SYSTEM, "--type=merge", "-p", json.dumps({
        "metadata": {"uid": uid, "resourceVersion": source["metadata"]["resourceVersion"]},
        "spec": {"agent": None, "runtime": {"kind": "BYO", "hermes": None,
            "byo": {"image": STANDIN, "contractVersion": "v1", "command": ["/bin/sh"],
                    "args": ["-c", "echo sre-standin-alive; sleep infinity"]}}}}))
    def current():
        deployment = h.get("deployment", "sre", RUNTIME)
        if not deployment:
            return False
        template = deployment["spec"]["template"]["spec"]
        agent = next((item for item in template["containers"] if item["name"] == "agent"), {})
        runtime = next((entry.get("value") for entry in agent.get("env", []) if entry["name"] == "KARS_RUNTIME_KIND"), None)
        return deployment if agent.get("image") == STANDIN and runtime == "BYO" else False
    h.poll("registered BYO stand-in projection", current, seconds=150)
    h.k("rollout", "status", "deployment/sre", "-n", RUNTIME, "--timeout=150s", timeout=160)
    def ready():
        pods = json.loads(h.k("get", "pods", "-n", RUNTIME, "-l", "kars.azure.com/sandbox=sre", "-o", "json"))["items"]
        live = [pod for pod in pods if not pod["metadata"].get("deletionTimestamp")
                and any(condition.get("type") == "Ready" and condition.get("status") == "True"
                        for condition in pod.get("status", {}).get("conditions", []))]
        return live[0] if len(live) == 1 else False
    pod = h.poll("private TLS router and alive stand-in Pod Ready", ready, seconds=100)
    require(h.get("karssandbox", "sre", SYSTEM)["metadata"]["uid"] == uid, "Runtime pin replaced the registered source")
    return pod


def projection_and_files(h, pod):
    spec = pod["spec"]
    require(spec["serviceAccountName"] == "sandbox", "Projection changed Azure's federated ServiceAccount subject")
    require(spec.get("automountServiceAccountToken") is False, "Pod still auto-mounts a Kubernetes JWT")
    registration = h.get("karssreregistrations.kars.azure.com", "canonical")
    router_sa = h.get("serviceaccount", "sre-api-router", RUNTIME)
    require(router_sa and router_sa.get("automountServiceAccountToken") is False
            and router_sa["metadata"]["uid"] == registration["status"]["routerServiceAccountUid"],
            "Private router identity is not the registered non-automounting ServiceAccount")
    agent = next(item for item in spec["containers"] if item["name"] == "agent")
    router = next(item for item in spec["containers"] if item["name"] == "inference-router")
    require(agent["image"] == STANDIN and router["image"] == "kars-inference-router:e2e", "Selected image pin/artifact changed")
    agent_mounts = {mount["name"]: mount["mountPath"] for mount in agent["volumeMounts"]}
    router_mounts = {mount["name"]: mount["mountPath"] for mount in router["volumeMounts"]}
    require(agent_mounts.get("sre-api-agent") == SA_PATH, "Agent standard paths are not projected from the safe identity")
    require("sre-api-private" not in agent_mounts and "router-kubernetes" not in agent_mounts
            and "azure-identity-token" not in agent_mounts, "Agent received a private or projected Azure/Kubernetes credential")
    require(router_mounts.get("sre-api-private") == "/etc/kars/sre-api"
            and router_mounts.get("router-kubernetes") == SA_PATH, "Router lost its private/API credential isolation")
    volumes = {volume["name"]: volume for volume in spec["volumes"]}
    require(volumes["sre-api-agent"]["secret"]["secretName"] == AGENT, "Agent mapping references wrong Secret")
    require(volumes["sre-api-private"]["secret"]["secretName"] == PRIVATE, "Router mapping references wrong Secret")
    require({item["key"] for item in volumes["sre-api-agent"]["secret"]["items"]} == {"token", "ca.crt", "namespace"},
            "Agent safe projection exposes unexpected keys")
    env = {item["name"]: item.get("value") for item in agent["env"]}
    require(env.get("KUBERNETES_SERVICE_HOST") == "127.0.0.1"
            and env.get("KUBERNETES_SERVICE_PORT") == "9446", "Legacy HTTPS endpoint is not the local private proxy")
    skipped = pod["metadata"].get("annotations", {}).get("azure.workload.identity/skip-containers", "").split(",")
    require("agent" in skipped and "egress-guard" in skipped, "Azure token injection is not excluded from the agent")
    for name in ("token", "ca.crt", "namespace"):
        # Only safe agent files are copied. Private credentials are never
        # copied to the host, printed or placed in argv.
        value = h.k("exec", "-n", RUNTIME, pod["metadata"]["name"], "-c", "agent",
                    "--", "cat", f"{SA_PATH}/{name}", user="registrar", timeout=25)
        h.write(f"agent/{name}", value)
    require((h.work / "agent/namespace").read_text().strip() == RUNTIME, "Standard namespace file changed")
    token = (h.work / "agent/token").read_text().strip()
    h.token_config("opaque", token)
    response = h.api("GET", "/api/v1/namespaces", user="opaque")
    require(response.status_code == 401, "Opaque agent credential authenticated to the real Kubernetes API")
    review = h.api("POST", "/apis/authentication.k8s.io/v1/tokenreviews", body={
        "apiVersion": "authentication.k8s.io/v1", "kind": "TokenReview", "spec": {"token": token}},
        status=201).json()
    require(review["status"].get("authenticated") is not True, "TokenReview accepted the opaque agent credential")
    h.passed("Real Pod preserves Sandbox/Azure identity and image pin; only safe standard files reach the agent")
    h.passed("Opaque agent credential is rejected by both real API authentication and TokenReview without admin-cert fallback")


def load_unchanged_hermes(h, port):
    root = h.root / "runtimes/hermes/src/kars_runtime_hermes/plugin"
    package = "_kars_e2e_unchanged_sre"
    module = types.ModuleType(package)
    module.__path__ = [str(root)]
    sys.modules[package] = module
    loaded = {}
    for name in ("sre_kube", "sre"):
        spec = importlib.util.spec_from_file_location(f"{package}.{name}", root / f"{name}.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        loaded[name] = module
    # Test-only host relocation of the unchanged client's standard file root.
    loaded["sre_kube"]._SA_DIR = h.work / "agent"
    os.environ["KUBERNETES_SERVICE_HOST"] = "127.0.0.1"
    os.environ["KUBERNETES_SERVICE_PORT"] = str(port)
    os.environ["NO_PROXY"] = "127.0.0.1,localhost"
    return loaded["sre_kube"], loaded["sre"]


def assert_filtered(secret):
    require(secret.get("kind") == "Secret", "Filtered GET did not return a Secret projection")
    require(set(secret.get("data", {})) == {"operator-token", "password"}, "Secret key names were lost")
    require(all(value == "" for value in secret["data"].values()), "Secret values crossed the proxy")
    require("stringData" not in secret, "stringData crossed the proxy")
    for key in ("annotations", "labels", "managedFields"):
        require(key not in secret["metadata"], f"Secret {key} copies crossed the proxy")
    require(MARKER not in json.dumps(secret), "Secret material survived projection")


def secret_list_wire_facts(value, uid):
    require(isinstance(value, dict) and value.get("kind") == "SecretList"
            and isinstance(value.get("items"), list) and len(value["items"]) == 1
            and isinstance(value["items"][0], dict) and isinstance(value["items"][0].get("metadata"), dict)
            and value["items"][0]["metadata"].get("uid") == uid,
            "Native wire proof must select only the exact owned synthetic Secret")
    item = value["items"][0]
    return {"envelopeKind": "SecretList", "itemHasKind": "kind" in item,
            "itemHasApiVersion": "apiVersion" in item, "itemMetadataObject": isinstance(item.get("metadata"), dict)}


def log_reader_facts(root, value):
    from .bootstrap_diagnostics import response_summary
    text = value.get("logs")
    facts = {"hasError": "error" in value, "hasText": isinstance(text, str),
             "standinMarker": isinstance(text, str) and "sre-standin-alive" in text}
    error = value.get("error")
    status = re.match(r"^([1-5][0-9]{2}) ", error) if isinstance(error, str) else None
    if status:
        try:
            body = json.loads(value.get("body", ""))
        except (ValueError, TypeError):
            body = None
        facts.update(response_summary(int(status[1]), body, root))
    return facts


def proxy_acceptance(h):
    install_metrics(h)
    pod = alive_pinned_source(h)
    projection_and_files(h, pod)
    runtime_denials(h, pod["metadata"]["name"])
    token_secret_denials(h)
    fixture = h.create({"apiVersion": "v1", "kind": "Secret",
        "metadata": {"name": f"sre-filter-{h.phase}", "namespace": OPERATORS,
            "labels": {"copy": MARKER},
            "annotations": {"kubectl.kubernetes.io/last-applied-configuration": json.dumps({"data": {"copy": MARKER}})}},
        "stringData": {"operator-token": MARKER, "password": MARKER}})
    name = f"sre-filter-{h.phase}"
    native = h.api("GET", f"/api/v1/namespaces/{OPERATORS}/secrets?fieldSelector=metadata.name%3D{name}",
                   status=200).json()
    print("SRE-WIRE", json.dumps(secret_list_wire_facts(native, fixture["metadata"]["uid"])), flush=True)
    with h.port_forward(pod["metadata"]["name"]) as port:
        kube_module, sre = load_unchanged_hermes(h, port)
        kube = kube_module.client()
        try:
            secret = kube.get(f"/api/v1/namespaces/{OPERATORS}/secrets/{name}")
            assert_filtered(secret)
            listing = kube.get(f"/api/v1/namespaces/{OPERATORS}/secrets", params={"fieldSelector": f"metadata.name={name}"})
            require(listing.get("kind") == "SecretList" and len(listing["items"]) == 1, "Filtered LIST did not reach the real API")
            assert_filtered(listing["items"][0])
            h.passed("Unchanged Hermes HTTPS client GET/LIST retains Secret key names but no values or metadata copies")
            logs = sre._impl_sre_logs(namespace=RUNTIME, pod=pod["metadata"]["name"], container="agent", tail=20)
            print("SRE-LOG-WIRE", json.dumps(log_reader_facts(h.root, logs)), flush=True)
            require("error" not in logs and "sre-standin-alive" in logs.get("logs", ""), "Unchanged Hermes raw log reader failed")
            metrics = kube.get("/apis/metrics.k8s.io/v1beta1/nodes")
            require(metrics.get("kind") == "NodeMetricsList" and metrics.get("items"), "Real metrics did not pass the filtered proxy")
            action = sre._create_karssreaction_cr(
                action={"type": "RolloutRestart", "namespace": SYSTEM, "kind": "Deployment", "name": "kars-controller"},
                diagnosis="Kind acceptance only", rationale="Pending proposal; never approve or execute", ttl_minutes=5)
            created = h.get("karssreaction", action, RUNTIME)
            require(created and created["spec"]["approval"] == {"state": "Pending"}, "Real Hermes proposal did not persist Pending")
            h.passed("Actual logs, real metrics and unchanged Hermes Pending proposal builder work over Pod TLS; no model run claimed")
            raw = kube._ensure_client()
            base = f"https://127.0.0.1:{port}"
            for path in [
                "/api/v1/namespaces/%6bars-sre/pods",
                "/api/v1/namespaces/kars-sre/pods/../secrets",
                "/api/v1/namespaces/kars-sre/pods?watch=true",
                f"/api/v1/namespaces/kars-sre/pods/{pod['metadata']['name']}/proxy",
                f"/api/v1/namespaces/kars-sre/pods/{pod['metadata']['name']}/log?follow=true",
                "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token",
            ]:
                # Encode dot segments explicitly so the HTTP client cannot
                # normalize an escape into a different legitimate request.
                path = path.replace("/../", "/%2e%2e/")
                require(raw.get(base + path).status_code == 403, "Unsafe proxy path/query was not rejected")
            target = f"/apis/kars.azure.com/v1alpha1/namespaces/{RUNTIME}/karssreactions/{action}"
            require(raw.patch(base + target, json={"spec": {"approval": {"state": "Approved"}}}).status_code == 403,
                    "Self-approval PATCH was not rejected")
            require(raw.post(base + f"/api/v1/namespaces/{RUNTIME}/serviceaccounts/sre-api-router/token",
                             json={"spec": {"audiences": [], "expirationSeconds": 600}}).status_code == 403,
                    "Token creation escaped the filtered proxy")
            require(raw.post(base + f"/api/v1/namespaces/{RUNTIME}/configmaps",
                             json={"metadata": {"name": "must-not-create"}}).status_code == 403,
                    "Arbitrary writes escaped the filtered proxy")
            invalid = {**created, "metadata": {"name": "self-approved", "namespace": RUNTIME},
                       "spec": {**created["spec"], "approval": {"state": "Approved"}}}
            invalid.pop("status", None)
            require(raw.post(base + f"/apis/kars.azure.com/v1alpha1/namespaces/{RUNTIME}/karssreactions",
                             json=invalid).status_code == 403, "Approved proposal creation was not rejected")
            h.passed("Encoded/proxy/watch/token/write/self-approval paths are rejected, not mistaken for missing API resources")
        finally:
            kube.close()
