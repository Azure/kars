# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""UID-fenced, read-only standalone snapshots; never export workload specs."""

import argparse
import json
import os
from pathlib import Path
import re
import time
from urllib.error import URLError
from urllib.parse import quote

from sre_authority import bootstrap_diagnostics as public
from sre_authority.registration_schema import CONTEXT, kind_proxy, request

ROOT = Path(__file__).resolve().parents[2]
STAGES = {"final", "setup", "services", "mcp", "credentials", "budget", "collections", "sandbox-cleanup",
          "test_crd_installed", "test_controller_running", "test_controller_metrics_endpoint",
          "test_admission_policies_installed", "test_operator_default_deny_np",
          "test_create_sandbox", "test_sandbox_deployment_exists", "test_sandbox_pod_starts"}
NAMESPACES = {"kars-system", "kube-system", "kars-sre", "kars-e2e-test", "kars-e2e-source",
              "kars-e2e-managed-mcp", "e2e-mcp-workspace", "kars-mcp", "budget-provider-fixture"}
KINDS = {"Namespace", "Node", "KarsTask", "KarsSandbox", "McpServer", "KarsBudgetAccount",
         "Deployment", "ReplicaSet", "Pod"}
PHASES = {"Active", "Terminating", "Pending", "Ready", "Running", "Succeeded", "Failed",
          "Unknown", "Degraded", "Blocked", "Bootstrap", "Frozen", "Closing", "Closed",
          "Launching", "Stopping", "PausingCredentials", "CredentialsPaused", "Idle", "Complete"}
REASONS = public.REASONS | {
    "PodInitializing", "ContainerCreating", "CreateContainerConfigError", "CreateContainerError",
    "CrashLoopBackOff", "ImagePullBackOff", "ErrImagePull", "InvalidImageName", "RunContainerError",
    "ErrImageNeverPull", "Completed", "Error", "OOMKilled", "ContainerCannotRun", "StartError",
    "DeadlineExceeded", "Evicted", "NodeLost", "UnexpectedAdmissionError", "Unschedulable",
    "DiscoveryFailed", "ResourcesDiscovered", "GroupVersionParsingFailed", "ParsedGroupVersions",
    "ContentDeletionFailed", "ContentRemoved", "SomeResourcesRemain", "SomeFinalizersRemain",
    "FinalizersRemoved", "KubeletReady", "KubeletNotReady", "KubeletHasSufficientMemory",
    "KubeletHasNoDiskPressure", "KubeletHasSufficientPID", "KubeletHasInsufficientMemory",
    "KubeletHasDiskPressure", "KubeletHasInsufficientPID", "Valid", "Reconciled", "Creating",
    "Created", "Reconciling", "SpecInvalid", "DependencyMissing", "TimedOut",
    "CredentialAuthorityUnavailable", "CredentialRebindPending", "CredentialSourceUnavailable",
    "AdmissionUnavailable", "LedgerAvailable", "LedgerInvalid", "BootstrapPending",
    "BudgetReserved", "BudgetExhausted", "AuthorityRevoked", "AccountRetired", "ContractBreach",
}
COLLECTIONS = [
    ("Namespace", "/api/v1/namespaces"), ("Node", "/api/v1/nodes"),
    ("KarsTask", "/apis/kars.azure.com/v1alpha1/karstasks"),
    ("KarsSandbox", "/apis/kars.azure.com/v1alpha1/karssandboxes"),
    ("McpServer", "/apis/kars.azure.com/v1alpha1/mcpservers"),
    ("KarsBudgetAccount", "/apis/kars.azure.com/v1alpha1/karsbudgetaccounts"),
    ("Deployment", "/apis/apps/v1/deployments"), ("ReplicaSet", "/apis/apps/v1/replicasets"),
    ("Pod", "/api/v1/pods"), ("Event", "/api/v1/events"),
]

def mapping(value):
    return value if isinstance(value, dict) else {}


def items(value):
    return value if isinstance(value, list) else []


def selected(namespace):
    return namespace in NAMESPACES or bool(
        isinstance(namespace, str) and re.fullmatch(r"kars-budget-(?:money|token)-(?:root|left|right)", namespace))


def integer(value):
    return value if type(value) is int and 0 <= value <= 2**63 - 1 else None


def stamp(value):
    return value if isinstance(value, str) and re.fullmatch(r"\d{4}-\d\d-\d\dT[\d:.]+Z", value) else None


def reason(value):
    return value if isinstance(value, str) and value in REASONS else "unclassified"


def facts(message, policies):
    # Supplying names only deliberately disables the helper's raw public
    # validation-message echo. No API-provided validation text is trusted.
    value = public.failure_facts(message, {name: {} for name in policies})
    output = {key: value.get(key, []) for key in ("policies", "categories", "missingFields")}
    if isinstance(message, str):
        lowered = message.lower()
        output["categories"] += [label for label, needle in (
            ("insufficient-cpu", "insufficient cpu"), ("insufficient-memory", "insufficient memory"),
            ("too-many-pods", "too many pods"), ("node-selector", "node selector"),
            ("node-affinity", "node affinity"), ("untolerated-taint", "untolerated taint"),
            ("pod-security", "podsecurity"), ("volume-pending", "unbound immediate persistentvolumeclaims"),
            ("api-unavailable", "service unavailable"), ("api-discovery", "discovery failed"),
            ("remaining-content", "resources are remaining"), ("remaining-finalizers", "finalizers"),
        ) if needle in lowered]
    return output


def metadata(value):
    obj = value if isinstance(value, dict) else {}
    result = {key: public.identifier(obj.get(key)) for key in ("name", "namespace", "uid", "resourceVersion")}
    result["generation"] = integer(obj.get("generation"))
    result["deletionTimestamp"] = stamp(obj.get("deletionTimestamp"))
    result["finalizers"] = [item for item in items(obj.get("finalizers"))[:32]
                            if public.identifier(item)]
    result["owners"] = [
        {"kind": owner.get("kind") if owner.get("kind") in KINDS else "other",
         "name": public.identifier(owner.get("name")), "uid": public.identifier(owner.get("uid")),
         "controller": owner.get("controller") is True}
        for owner in items(obj.get("ownerReferences"))[:16] if isinstance(owner, dict)
    ]
    return result


def condition(value, policies):
    if not isinstance(value, dict):
        return {}
    return {
        "type": public.identifier(value.get("type")),
        "status": value.get("status") if value.get("status") in ("True", "False", "Unknown") else None,
        "reason": reason(value.get("reason")), "observedGeneration": integer(value.get("observedGeneration")),
        "lastTransitionTime": stamp(value.get("lastTransitionTime")), **facts(value.get("message"), policies),
    }


def container(value):
    value = mapping(value)
    result = {"name": public.identifier(value.get("name")), "ready": value.get("ready") is True,
              "restartCount": integer(value.get("restartCount"))}
    for key in ("state", "lastState"):
        states = mapping(value.get(key))
        result[key] = {}
        for state in ("waiting", "running", "terminated"):
            if state not in states:
                continue
            detail = mapping(states[state])
            result[key] = {"state": state, "reason": reason(detail.get("reason")),
                           "exitCode": integer(detail.get("exitCode")),
                           "startedAt": stamp(detail.get("startedAt")), "finishedAt": stamp(detail.get("finishedAt"))}
    return result


def object_status(kind, obj, policies):
    status = mapping(obj.get("status"))
    result = {"kind": kind, "metadata": metadata(obj.get("metadata")), "status": {}}
    for field in ("phase", "executionPhase"):
        value = status.get(field)
        result["status"][field] = value if isinstance(value, str) and value in PHASES else None
    for field in ("observedGeneration", "replicas", "readyReplicas", "availableReplicas", "updatedReplicas"):
        result["status"][field] = integer(status.get(field))
    result["status"]["conditions"] = [condition(c, policies) for c in items(status.get("conditions"))[:32]]
    if kind == "Pod":
        for field in ("containerStatuses", "initContainerStatuses"):
            result["status"][field] = [container(c) for c in items(status.get(field))[:16]]
    if kind == "Namespace":
        result["namespaceFinalizers"] = [item for item in items(mapping(obj.get("spec")).get("finalizers"))[:32]
                                         if public.identifier(item)]
    if kind == "Node":
        result["unschedulable"] = mapping(obj.get("spec")).get("unschedulable") is True
        for field in ("capacity", "allocatable"):
            result["status"][field] = {key: value for key, value in mapping(status.get(field)).items()
                if key in ("cpu", "memory", "pods", "ephemeral-storage")
                and isinstance(value, str) and re.fullmatch(r"[0-9]+(?:\.[0-9]+)?[A-Za-z]*", value)}
    return result


def collect(port, expected_uid, stage, seconds=70):
    deadline = time.monotonic() + seconds
    report = {"stage": stage, "complete": True, "resources": [], "events": [], "api": [], "policies": []}

    def get(path):
        if time.monotonic() >= deadline:
            report["complete"] = False
            return 0, None
        try:
            code, obj = request(port, "GET", path)
        except (OSError, URLError, TimeoutError, ValueError):
            code, obj = 0, None
        if code not in (200, 404):
            report["complete"] = False
        return code, obj

    code, namespace = get("/api/v1/namespaces/kube-system")
    if code != 200 or not isinstance(namespace, dict) or namespace.get("metadata", {}).get("uid") != expected_uid:
        raise RuntimeError("Standalone diagnostic cluster UID is unverified")
    for endpoint in ("/livez", "/readyz"):
        code, _ = get(endpoint)
        report["api"].append({"endpoint": endpoint, "httpStatus": code})
    code, body = get("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies?limit=256")
    policies = []
    if code == 200 and isinstance(body, dict):
        for obj in items(body.get("items"))[:256]:
            name = obj.get("metadata", {}).get("name", "")
            if not name.startswith("kars-") or not public.identifier(name):
                continue
            policies.append(name)
            status = mapping(obj.get("status"))
            report["policies"].append({
                "metadata": metadata(obj.get("metadata")), "observedGeneration": integer(status.get("observedGeneration")),
                "typeChecked": isinstance(status.get("typeChecking"), dict),
                "warnings": [facts(mapping(warning).get("warning"), [name])
                             for warning in items(mapping(status.get("typeChecking")).get("expressionWarnings"))[:32]],
            })
    for kind, path in COLLECTIONS:
        continuation = ""
        for _ in range(4):
            query = "?limit=256" + ("&continue=" + quote(continuation, safe="") if continuation else "")
            code, body = get(path + query)
            report["api"].append({"resource": kind, "httpStatus": code})
            if code != 200 or not isinstance(body, dict):
                break
            for obj in items(body.get("items"))[:256]:
                meta = obj.get("metadata", {})
                if kind == "Namespace":
                    if not selected(meta.get("name")):
                        continue
                elif kind != "Node" and not selected(meta.get("namespace")):
                    continue
                if kind == "Event":
                    involved = mapping(obj.get("involvedObject"))
                    if involved.get("kind") in KINDS:
                        report["events"].append({
                            "kind": involved["kind"], "name": public.identifier(involved.get("name")),
                            "namespace": public.identifier(involved.get("namespace")),
                            "uid": public.identifier(involved.get("uid")), "reason": reason(obj.get("reason")),
                            "count": integer(obj.get("count")), "lastTimestamp": stamp(obj.get("lastTimestamp")),
                            **facts(obj.get("message"), policies),
                        })
                else:
                    report["resources"].append(object_status(kind, obj, policies))
            continuation = mapping(body.get("metadata")).get("continue", "")
            if not continuation:
                break
            if not isinstance(continuation, str):
                report["complete"] = False
                break
        else:
            report["complete"] = False
    if time.monotonic() < deadline:
        report["controllerManager"] = public.controller_stack(
            CONTEXT, "kube-controller-manager-kars-e2e-control-plane")
        if not report["controllerManager"]["available"]:
            report["complete"] = False
    else:
        report["complete"] = False
    return report


def main(root, stage, expected_uid):
    safe_stage = stage if stage in STAGES else "unknown"
    report = {"stage": safe_stage, "complete": False, "category": "unavailable"}
    try:
        if stage not in STAGES or not public.identifier(expected_uid):
            raise RuntimeError("Standalone diagnostic arguments are invalid")
        with kind_proxy(root) as (port, _version):
            report = collect(port, expected_uid, stage)
    except (OSError, URLError, RuntimeError, ValueError, TypeError, KeyError, TimeoutError):
        pass
    directory = root / "e2e-diag" / "standalone"
    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        (directory / f"{safe_stage}.json").write_text(json.dumps(report, indent=2) + "\n")
    except OSError:
        report["complete"] = False
    print("STANDALONE-DIAG " + json.dumps({
        "stage": stage if stage in STAGES else "unknown", "complete": report["complete"],
        "resources": len(report.get("resources", [])), "events": len(report.get("events", [])),
    }, sort_keys=True), flush=True)
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=sorted(STAGES))
    args = parser.parse_args()
    raise SystemExit(main(ROOT, args.stage, os.environ.get("KARS_STANDALONE_CLUSTER_UID", "")))
