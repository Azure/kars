# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded, read-only composed-readiness facts; never raw private router logs.

Exact-rule observations describe policy contents, not CNI enforcement. They do
not establish causality without the runtime transport/readiness evidence.
"""

import ipaddress
import json
from pathlib import Path
import re
import time
from urllib.parse import urlsplit

from .common import CONTEXT, EPOCH, OWNER, RUNTIME, SYSTEM, assert_claim, command_error_category

MAX_LOG_BYTES = 32768
MAX_FACTS = 64
PRIVATE_UID = "kars.azure.com/sre-private-credential-uid"
BACKEND = "kars_inference_router::sre_proxy::backend"
PROXY = "kars_inference_router::sre_proxy"
BACKEND_STAGES = {"registration", "namespace", "source", "service-account",
                  "privacy-review", "credential-metadata"}
REJECTIONS = {"authority-transport", "authority-denied", "registration-stale",
              "namespace-claim", "source-identity", "service-account", "privacy-transport",
              "privacy-denied", "privacy-not-denied", "metadata-transport", "metadata-denied",
              "legacy-alias", "credential-expired", "unclassified"}
STAGES = BACKEND_STAGES | {
    "readiness", "collection", "fixture-identity", "deployment", "pod-selection",
    "pod-router", "pod-lifecycle", "pod-epoch", "pod-template", "pod-fence", "tracing",
    "policy-api-service", "policy-api-endpoint",
    "core-namespace", "pod-list", "replica-set", "pod-log", "policy-read",
    "service-read", "endpoint-read",
}
CATEGORIES = REJECTIONS | {
    "transport-timeout", "transport-connect", "transport-error", "http-denied",
    "authority-slow-accepted", "authority-slow-rejected", "complete", "unavailable",
    "deadline", "refused", "identity-changed", "write-failed", "current", "stale",
    "ready-current", "not-ready", "running-ready", "running-not-ready", "waiting",
    "terminated", "stable", "restarting", "no-known-events", "known-events",
    "exact-rule-present", "exact-rule-absent", "exact-rules-present",
    "exact-rules-partial", "exact-rules-absent",
    "empty-list", "empty-response", "invalid-json", "invalid-shape", "command-timeout",
    "command-forbidden", "command-unauthorized", "command-not-found", "command-conflict",
    "command-service-unavailable", "command-invalid", "command-error",
    "replicaset-create-error", "private-workload-admission-denied",
}
GET_STAGES = {
    "karssreregistrations.kars.azure.com": "registration",
    "karssandbox": "source", "namespace": "namespace", "deployment": "deployment",
    "pods": "pod-list", "pod": "pod-fence", "replicaset": "replica-set",
    "networkpolicy": "policy-read", "service": "service-read", "endpoints": "endpoint-read",
}
COMMAND_CATEGORIES = {
    "wait-timeout": "command-timeout", "Forbidden": "command-forbidden",
    "Unauthorized": "command-unauthorized", "NotFound": "command-not-found",
    "Conflict": "command-conflict", "ServiceUnavailable": "command-service-unavailable",
    "Invalid": "command-invalid",
}


class Unavailable(RuntimeError):
    def __init__(self, stage, category="unavailable", **values):
        self.stage, self.category, self.values = stage, category, values
        super().__init__("Bounded SRE readiness diagnostic unavailable")


def fact(stage, category, **values):
    if (not isinstance(stage, str) or not isinstance(category, str)
            or stage not in STAGES or category not in CATEGORIES
            or set(values) - {"httpStatus", "durationSeconds"}
            or ("httpStatus" in values and not (
                type(values["httpStatus"]) is int and 100 <= values["httpStatus"] <= 599))
            or ("durationSeconds" in values and not (
                type(values["durationSeconds"]) is int and 0 <= values["durationSeconds"] <= 999))):
        raise Unavailable("collection", "refused")
    return {"stage": stage, "category": category, **values}


def router_facts(raw):
    """Only exact JSON tracing events emitted by the shipped SRE modules."""
    if not isinstance(raw, str) or len(raw.encode()) > MAX_LOG_BYTES:
        raise Unavailable("tracing", "refused")
    result = []
    for line in raw.splitlines()[-150:]:
        try:
            event = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(event, dict) or event.get("level") != "WARN":
            continue
        fields = event.get("fields")
        if not isinstance(fields, dict):
            continue
        message, target = fields.get("message"), event.get("target")
        value = None
        if (target == BACKEND and isinstance(fields.get("stage"), str)
                and fields["stage"] in BACKEND_STAGES):
            if (message == "SRE authority transport failure"
                    and set(fields) == {"message", "stage", "timed_out", "connect_error"}
                    and type(fields["timed_out"]) is bool and type(fields["connect_error"]) is bool):
                category = ("transport-timeout" if fields["timed_out"] else
                            "transport-connect" if fields["connect_error"] else "transport-error")
                value = fact(fields["stage"], category)
            elif (message == "SRE authority request denied"
                    and set(fields) == {"message", "stage", "http_status"}
                    and type(fields["http_status"]) is int and 100 <= fields["http_status"] <= 599):
                value = fact(fields["stage"], "http-denied", httpStatus=fields["http_status"])
        elif target == PROXY:
            if (message == "SRE readiness authority rejected" and set(fields) == {"message", "category"}
                    and isinstance(fields.get("category"), str) and fields["category"] in REJECTIONS):
                value = fact("readiness", fields["category"])
            elif (message == "SRE readiness authority slow"
                    and set(fields) == {"message", "elapsed_seconds", "authorized"}
                    and type(fields["elapsed_seconds"]) is int and 0 <= fields["elapsed_seconds"] <= 999
                    and type(fields["authorized"]) is bool):
                value = fact("readiness", "authority-slow-accepted" if fields["authorized"]
                             else "authority-slow-rejected", durationSeconds=fields["elapsed_seconds"])
        if value and value not in result:
            result.append(value)
        if len(result) == 32:
            break
    return result


def context_guard(h):
    config = h.config
    contexts, clusters = config.get("contexts", []), config.get("clusters", [])
    if (config.get("current-context") != CONTEXT or len(contexts) != 1 or len(clusters) != 1
            or contexts[0].get("name") != CONTEXT
            or contexts[0].get("context", {}).get("cluster") != clusters[0].get("name")):
        raise Unavailable("fixture-identity", "refused")
    server = clusters[0].get("cluster", {}).get("server", "")
    url = urlsplit(server)
    if (server != h.server or url.scheme != "https"
            or url.hostname not in ("127.0.0.1", "::1", "localhost")
            or url.username or url.password or url.query or url.fragment):
        raise Unavailable("fixture-identity", "refused")


def controller_owner(obj, kind, name=None):
    owners = [owner for owner in obj.get("metadata", {}).get("ownerReferences", [])
              if owner.get("controller") is True and owner.get("kind") == kind
              and owner.get("apiVersion") == "apps/v1" and owner.get("uid")
              and (name is None or owner.get("name") == name)]
    if len(owners) != 1:
        raise Unavailable("pod-fence", "refused")
    return owners[0]


def pod_identity(pod):
    meta = pod.get("metadata", {})
    annotations = meta.get("annotations", {})
    routers = [c for c in pod.get("spec", {}).get("containers", []) if c.get("name") == "inference-router"]
    if len(routers) != 1 or routers[0].get("image") != "kars-inference-router:e2e":
        raise Unavailable("pod-fence", "refused")
    return (meta.get("uid"), meta.get("name"), meta.get("namespace"),
            tuple(annotations.get(key) for key in (OWNER, EPOCH, PRIVATE_UID)),
            controller_owner(pod, "ReplicaSet"), routers[0]["image"])


def pod_items(value):
    if value is None:
        raise Unavailable("pod-list", "empty-response")
    if (not isinstance(value, dict) or value.get("apiVersion") != "v1"
            or value.get("kind") not in ("List", "PodList") or "items" not in value
            or value.get("metadata", {}).get("continue")):
        raise Unavailable("pod-list", "invalid-shape")
    # Native/CLI list envelopes may serialize a zero-length slice as null.
    # Empty means no Pod evidence, never a successful workload/identity proof.
    items = [] if value["items"] is None else value["items"]
    if not isinstance(items, list) or any(
        not isinstance(item, dict)
        or ("kind" in item and item["kind"] != "Pod")
        or ("apiVersion" in item and item["apiVersion"] != "v1")
        for item in items
    ):
        raise Unavailable("pod-list", "invalid-shape")
    if not items:
        raise Unavailable("pod-list", "empty-list")
    return items


def deployment_failures(deployment):
    result = []
    for condition in deployment.get("status", {}).get("conditions", [])[:8]:
        if (condition.get("type") == "Progressing" and condition.get("status") == "False"
                and condition.get("reason") == "ReplicaSetCreateError"):
            result.append(fact("deployment", "replicaset-create-error"))
            message = condition.get("message", "")
            if (isinstance(message, str) and len(message) <= 65536
                    and "forbidden" in message.lower()
                    and "ValidatingAdmissionPolicy 'kars-sre-private-workloads'" in message):
                result.append(fact("deployment", "private-workload-admission-denied"))
    return result


def exact_api_rules(policy, service, endpoints):
    """Observe only exact API targets; no guesses about selector/CNI semantics."""
    if not all(isinstance(obj, dict) for obj in (policy, service, endpoints)):
        raise Unavailable("policy-api-endpoint")
    for obj in (service, endpoints):
        meta = obj.get("metadata", {})
        if (meta.get("name") != "kubernetes" or meta.get("namespace") != "default"
                or not meta.get("uid") or meta.get("deletionTimestamp")):
            raise Unavailable("policy-api-endpoint", "refused")
    address = ipaddress.ip_address(service["spec"]["clusterIP"])
    ports = [p["port"] for p in service["spec"]["ports"]
             if p.get("name") == "https" and p.get("protocol", "TCP") == "TCP"]
    targets = []
    for subset in endpoints.get("subsets", []):
        for peer in subset.get("addresses", []):
            ip = ipaddress.ip_address(peer["ip"])
            if not ip.is_private or ip.is_loopback or ip.is_unspecified or ip.is_multicast:
                raise Unavailable("policy-api-endpoint", "refused")
            targets += [(ip, p["port"]) for p in subset.get("ports", [])
                        if p.get("name") == "https" and p.get("protocol", "TCP") == "TCP"]
    if len(ports) != 1 or not 0 < len(targets) <= 32:
        raise Unavailable("policy-api-endpoint", "refused")

    def present(ip, port):
        if type(port) is not int or not 0 < port < 65536:
            raise Unavailable("policy-api-endpoint", "refused")
        return any(
            any(peer.get("ipBlock") == {"cidr": f"{ip}/{ip.max_prefixlen}"} for peer in rule.get("to", []))
            and any(p.get("port") == port and p.get("protocol", "TCP") == "TCP"
                    for p in rule.get("ports", []))
            for rule in policy.get("spec", {}).get("egress", []))

    observed = [present(ip, port) for ip, port in targets]
    return [
        fact("policy-api-service", "exact-rule-present" if present(address, ports[0]) else "exact-rule-absent"),
        fact("policy-api-endpoint", "exact-rules-present" if all(observed) else
             "exact-rules-partial" if any(observed) else "exact-rules-absent"),
    ]


class Reader:
    def __init__(self, h):
        context_guard(h)
        self.h = h
        self.end = min(h.deadline, time.monotonic() + 40)
        self.stage = "collection"

    def timeout(self, seconds=3):
        remaining = self.end - time.monotonic()
        if remaining < 2:
            raise Unavailable(self.stage, "deadline")
        return min(seconds, remaining)

    def read(self, stage, args, seconds=3):
        self.stage = stage
        timeout = self.timeout(seconds)
        started = time.monotonic()
        try:
            result = self.h.k(*args, timeout=timeout, expected=None)
        except AssertionError as error:
            category = ("command-timeout" if str(error).startswith(
                "Command exceeded its bounded timeout at ") else "command-error")
            raise Unavailable(stage, category) from None
        duration = min(999, max(0, int(time.monotonic() - started)))
        if result.returncode:
            category = COMMAND_CATEGORIES.get(command_error_category(result.stderr), "command-error")
            raise Unavailable(stage, category, durationSeconds=duration)
        return result.stdout

    def get(self, kind, name=None, namespace=None):
        stage = GET_STAGES[kind]
        if kind == "namespace" and name == SYSTEM:
            stage = "core-namespace"
        args = ["get", kind]
        if name:
            args.append(name)
        if namespace:
            args += ["-n", namespace]
        args += ["--ignore-not-found", "-o", "json"]
        raw = self.read(stage, args)
        try:
            return json.loads(raw) if raw.strip() else None
        except (ValueError, TypeError):
            raise Unavailable(stage, "invalid-json") from None

    def logs(self, pod):
        meta = pod["metadata"]
        identity = pod_identity(pod)
        raw = self.read("pod-log", ["logs", "-n", RUNTIME, meta["name"], "-c", "inference-router",
                        "--tail=150", f"--limit-bytes={MAX_LOG_BYTES}", "--since=120s"], seconds=8)
        current = self.get("pod", meta["name"], RUNTIME)
        if not current or pod_identity(current) != identity:
            raise Unavailable("pod-fence", "identity-changed")
        # Do not project, print or persist any log before the post-read UID fence.
        return router_facts(raw)


def collect(h, sample="failure"):
    phase = h.phase
    if phase not in ("prepare", "legacy", "fresh") or sample not in ("failure", "ready"):
        raise Unavailable("collection", "refused")
    facts = []
    reader = None
    try:
        reader = Reader(h)
        registration = reader.get("karssreregistrations.kars.azure.com", "canonical")
        source = reader.get("karssandbox", "sre", SYSTEM)
        namespace = reader.get("namespace", RUNTIME)
        core = reader.get("namespace", SYSTEM)
        assert_claim(source, namespace, core, h.state["system_uid"])
        if (not registration or registration["spec"]["sandbox"]["uid"] != source["metadata"]["uid"]
                or registration["spec"]["sandbox"].get("namespace") != SYSTEM
                or registration["spec"]["sandbox"].get("name") != "sre"
                or registration["spec"]["runtimeNamespace"].get("name") != RUNTIME
                or registration["spec"]["runtimeNamespace"]["uid"] != namespace["metadata"]["uid"]):
            raise Unavailable("fixture-identity", "refused")
        facts.append(fact("fixture-identity", "current"))
        status = registration.get("status", {})
        facts.append(fact("registration", "ready-current" if status.get("phase") == "Ready"
                          and status.get("observedGeneration") == registration["metadata"]["generation"]
                          else "not-ready"))
        deployment = reader.get("deployment", "sre", RUNTIME)
        template = deployment["spec"]["template"]["metadata"].get("annotations", {})
        if template.get(OWNER) != registration["metadata"]["uid"] or not template.get(PRIVATE_UID):
            raise Unavailable("deployment", "refused")
        facts.append(fact("deployment", "current" if template.get(EPOCH) == status.get("privacyEpoch") else "stale"))
        facts += deployment_failures(deployment)
        pods = pod_items(reader.get("pods", namespace=RUNTIME))
        eligible = [pod for pod in pods if
            pod.get("metadata", {}).get("namespace") == RUNTIME
            and pod["metadata"].get("annotations", {}).get(OWNER) == registration["metadata"]["uid"]
            and pod["metadata"].get("labels", {}).get("kars.azure.com/sandbox") == "sre"
            and not pod["metadata"].get("deletionTimestamp")]
        if not 0 < len(eligible) <= 2:
            raise Unavailable("pod-selection", "unavailable")
        pod = max(eligible, key=lambda obj: obj["metadata"]["annotations"].get(PRIVATE_UID) == template[PRIVATE_UID])
        meta = pod["metadata"]
        if not meta.get("uid") or not re.fullmatch(r"sre-[a-z0-9-]{1,58}", meta["name"]):
            raise Unavailable("pod-fence", "refused")
        owner = controller_owner(pod, "ReplicaSet")
        if not re.fullmatch(r"sre-[a-z0-9-]{1,58}", owner.get("name", "")):
            raise Unavailable("pod-fence", "refused")
        replica = reader.get("replicaset", owner["name"], RUNTIME)
        if (not replica or replica["metadata"]["uid"] != owner["uid"]
                or replica["metadata"].get("name") != owner["name"]
                or replica["metadata"].get("namespace") != RUNTIME
                or ("kind" in replica and replica["kind"] != "ReplicaSet")
                or ("apiVersion" in replica and replica["apiVersion"] != "apps/v1")
                or controller_owner(replica, "Deployment", "sre")["uid"] != deployment["metadata"]["uid"]):
            raise Unavailable("pod-fence", "identity-changed")
        pod_identity(pod)
        states = [c for c in pod.get("status", {}).get("containerStatuses", []) if c.get("name") == "inference-router"]
        if len(states) == 1:
            state = states[0]
            category = ("running-ready" if state.get("ready") else "running-not-ready") if (
                "running" in state.get("state", {})) else (
                    "waiting" if "waiting" in state.get("state", {}) else "terminated")
            facts += [fact("pod-router", category),
                      fact("pod-lifecycle", "restarting" if state.get("restartCount", 0) else "stable")]
        facts += [
            fact("pod-epoch", "current" if meta["annotations"].get(EPOCH) == status.get("privacyEpoch") else "stale"),
            fact("pod-template", "current" if meta["annotations"].get(PRIVATE_UID) == template[PRIVATE_UID] else "stale"),
        ]
        events = reader.logs(pod)
        facts += [fact("pod-fence", "current"), fact("tracing", "known-events" if events else "no-known-events"), *events]
        policy = reader.get("networkpolicy", "sandbox-policy", RUNTIME)
        service = reader.get("service", "kubernetes", "default")
        endpoints = reader.get("endpoints", "kubernetes", "default")
        facts += exact_api_rules(policy, service, endpoints)
        facts.append(fact("collection", "complete"))
    except Unavailable as error:
        facts.append(fact(error.stage, error.category, **error.values))
    except (AssertionError, OSError, ValueError, TypeError, KeyError, AttributeError, RuntimeError):
        facts.append(fact(reader.stage if reader else "fixture-identity", "unavailable"))
    report = {"phase": phase, "sample": sample, "facts": facts[:MAX_FACTS]}
    for item in report["facts"]:
        fact(item["stage"], item["category"], **{k: v for k, v in item.items() if k not in ("stage", "category")})
    data = json.dumps(report, sort_keys=True)
    if len(data) > 16384:
        raise Unavailable("collection", "refused")
    print("SRE-READINESS " + data, flush=True)
    try:
        path = Path(h.root) / "e2e-sre-schema-diag" / f"sre-readiness-{phase}-{sample}.json"
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_text(data + "\n")
        path.chmod(0o600)
    except OSError:
        print("SRE-READINESS " + json.dumps({"phase": phase, "sample": sample,
              "facts": [fact("collection", "write-failed")]}), flush=True)
    return report
