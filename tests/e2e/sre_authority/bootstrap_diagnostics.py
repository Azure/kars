# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Allowlisted public admission evidence; never dump Pod specs, argv or bodies."""

import re

REASONS = {
    "Forbidden", "Invalid", "InternalError", "BadRequest", "NotFound", "AlreadyExists",
    "Unauthorized", "Conflict", "ServiceUnavailable", "FailedCreate", "ReplicaFailure",
    "MinimumReplicasUnavailable", "MinimumReplicasAvailable", "NewReplicaSetCreated",
    "FoundNewReplicaSet", "ReplicaSetUpdated", "NewReplicaSetAvailable",
    "ReplicaSetCreateError", "ProgressDeadlineExceeded", "FailedScheduling",
    "FailedMount", "Failed", "SuccessfulCreate", "ScalingReplicaSet",
}
FIELDS = {
    "namespace", "name", "labels", "annotations", "spec", "status", "subjects",
    "enabled", "serviceAccountName", "containers", "initContainers", "ephemeralContainers",
    "securityContext", "operation", "subResource", "container", "kind", "apiVersion",
}


def identifier(value):
    return value if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.:/-]{1,253}", value) else None


def failure_facts(message, policies):
    if not isinstance(message, str):
        return {}
    message = message[:65536]
    facts = {
        "policies": sorted(name for name in policies if name in message),
        "validationMessages": [],
        "categories": [label for label, needle in [
            ("compilation", "compilation"), ("evaluation", "evaluation"),
            ("no-such-key", "no such key"), ("undefined-field", "undefined field"),
            ("undeclared-reference", "undeclared reference"), ("forbidden", "forbidden"),
            ("not-found", "not found"), ("quota", "quota"), ("type-checking", "type checking"),
        ] if needle in message.lower()],
        "missingFields": sorted(set(re.findall(r"no such key: ([A-Za-z_][A-Za-z0-9_]*)", message)) & FIELDS),
    }
    for name, policy in policies.items():
        for index, validation in enumerate(policy.get("spec", {}).get("validations", [])):
            known = validation.get("message")
            if isinstance(known, str) and known in message:
                facts["validationMessages"].append({"policy": name, "index": index, "message": known})
    facts["serviceAccountMissing"] = 'serviceaccount "kars-controller" not found' in message.lower()
    return facts


def api_result(code, body, policies):
    report = {"httpStatus": code}
    if isinstance(body, dict) and body.get("kind") == "Status":
        reason = body.get("reason")
        report["reason"] = reason if reason in REASONS else "unclassified"
        report.update(failure_facts(body.get("message"), policies))
    return report


def object_status(obj, policies):
    metadata, status = obj.get("metadata", {}), obj.get("status", {})
    report = {"kind": obj.get("kind"), "name": identifier(metadata.get("name")),
              "namespace": identifier(metadata.get("namespace")), "uid": identifier(metadata.get("uid"))}
    for field in ("replicas", "readyReplicas", "availableReplicas", "observedGeneration"):
        if isinstance(status.get(field), int):
            report[field] = status[field]
    conditions = []
    for condition in status.get("conditions", [])[:16]:
        if not isinstance(condition, dict):
            continue
        entry = {"type": identifier(condition.get("type")), "status": condition.get("status")
                 if condition.get("status") in ("True", "False", "Unknown") else None,
                 "reason": condition.get("reason") if condition.get("reason") in REASONS else "unclassified"}
        entry.update(failure_facts(condition.get("message"), policies))
        conditions.append(entry)
    report["conditions"] = conditions
    return report


def policy_status(obj, expected):
    name, status = obj.get("metadata", {}).get("name"), obj.get("status", {})
    if name not in expected:
        raise AssertionError("Only rendered public policies may publish type-check diagnostics")
    warnings = []
    for warning in status.get("typeChecking", {}).get("expressionWarnings", [])[:32]:
        field, text = warning.get("fieldRef"), warning.get("warning")
        if (isinstance(field, str) and re.fullmatch(
                r"spec\.(matchConditions|validations|variables)\[\d+\]\.expression", field)
                and isinstance(text, str)):
            warnings.append({"fieldRef": field, "warning": re.sub(
                r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]", "?", text)[:16384]})
    return {"name": name, "generation": obj.get("metadata", {}).get("generation"),
            "observedGeneration": status.get("observedGeneration"),
            "typeChecked": "typeChecking" in status, "warnings": warnings}


def collect(port, policies, request):
    report = {"scope": "controller-Pod-creation-only", "policies": [], "workloads": [], "events": []}
    for name in sorted(policies):
        code, obj = request(port, "GET", f"/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/{name}")
        report["policies"].append(policy_status(obj, policies) if code == 200 else {"name": name, "httpStatus": code})
    owners = set()
    for group, plural in [("apis/apps/v1", "deployments"), ("apis/apps/v1", "replicasets"),
                          ("api/v1", "pods")]:
        code, body = request(port, "GET", f"/{group}/namespaces/kars-system/{plural}")
        if code != 200 or not isinstance(body, dict):
            report["workloads"].append({"kind": plural, "httpStatus": code})
            continue
        for obj in body.get("items", [])[:256]:
            metadata = obj.get("metadata", {})
            if not identifier(metadata.get("uid")):
                continue
            if metadata.get("name") == "kars-controller" or any(
                    owner.get("uid") in owners for owner in metadata.get("ownerReferences", [])):
                owners.add(metadata.get("uid"))
                obj = dict(obj, kind={"deployments": "Deployment", "replicasets": "ReplicaSet", "pods": "Pod"}[plural])
                report["workloads"].append(object_status(obj, policies))
    code, body = request(port, "GET", "/api/v1/namespaces/kars-system/events?fieldSelector=type%3DWarning")
    if code == 200 and isinstance(body, dict):
        for event in body.get("items", [])[:256]:
            involved = event.get("involvedObject", {})
            if involved.get("uid") not in owners or event.get("reason") not in REASONS:
                continue
            entry = {"kind": involved.get("kind") if involved.get("kind") in ("Deployment", "ReplicaSet", "Pod") else None,
                     "name": identifier(involved.get("name")),
                     "reason": event.get("reason"), "count": event.get("count")}
            entry.update(failure_facts(event.get("message"), policies))
            report["events"].append(entry)
    return report
