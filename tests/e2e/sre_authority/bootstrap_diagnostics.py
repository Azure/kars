# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Allowlisted public admission evidence; never dump Pod specs, argv or bodies."""

import json
import re
import subprocess

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


def probe_command_result(code, output):
    category = "unclassified"
    if code == 0:
        category = "succeeded"
    elif "executable file not found" in output:
        category = "executable-not-found"
    elif code == 1 and output.strip() in ("", "command terminated with exit code 1"):
        category = "probe-not-ready"
    return {"exitCode": code, "category": category}


def router_readiness_facts(text):
    facts = []
    stages = ("registration", "namespace", "source", "service-account", "privacy-review", "credential-metadata",
              "enabled", "loaded", "bound", "serving", "tcp-accepted", "tls-accepted", "tls-rejected",
              "tls-timeout", "readiness-entered", "authority-entered")
    steps = ("token", "request", "headers", "complete")
    categories = ("authority-transport", "authority-denied", "registration-stale", "namespace-claim",
                  "source-identity", "service-account", "privacy-transport", "privacy-denied",
                  "privacy-not-denied", "metadata-transport", "metadata-denied", "legacy-alias",
                  "credential-expired", "unclassified")
    messages = ("SRE authority transport failure", "SRE authority request denied",
                "SRE readiness authority rejected", "SRE readiness authority slow",
                "SRE transport progress", "SRE authority progress")
    for line in text.splitlines()[-150:]:
        line = re.sub(r"\x1b\[[0-9;]*m", "", line)
        try:
            event = json.loads(line)
        except ValueError:
            event = None
        if isinstance(event, dict):
            fields = event.get("fields", event)
            if not isinstance(fields, dict) or fields.get("message") not in messages:
                continue
            value = {}
            for key, allowed in (("stage", stages), ("category", categories), ("step", steps)):
                if fields.get(key) in allowed:
                    value[key] = fields[key]
            for key in ("timed_out", "connect_error", "authorized"):
                if isinstance(fields.get(key), bool):
                    value[key] = fields[key]
            if type(fields.get("http_status")) is int and 100 <= fields["http_status"] <= 599:
                value["httpStatus"] = fields["http_status"]
            if type(fields.get("elapsed_seconds")) is int and 0 <= fields["elapsed_seconds"] <= 999:
                value["elapsedSeconds"] = fields["elapsed_seconds"]
            if value and value not in facts:
                facts.append(value)
            continue
        if not any(message in line for message in messages):
            continue
        value = {}
        for key, allowed in (("stage", stages), ("category", categories), ("step", steps)):
            match = re.search(rf'\b{key}="?({"|".join(allowed)})"?(?:\s|$)', line)
            if match:
                value[key] = match[1]
        for key in ("timed_out", "connect_error", "authorized"):
            match = re.search(rf"\b{key}=(true|false)(?:\s|$)", line)
            if match:
                value[key] = match[1] == "true"
        status = re.search(r"\bhttp_status=([1-5][0-9]{2})(?:\s|$)", line)
        if status:
            value["httpStatus"] = int(status[1])
        elapsed = re.search(r"\belapsed_seconds=([0-9]{1,3})(?:\s|$)", line)
        if elapsed:
            value["elapsedSeconds"] = int(elapsed[1])
        if value and value not in facts:
            facts.append(value)
    return facts[:32]


def router_log_summary(text, root):
    paths = [
        root / "inference-router/src/main.rs",
        root / "inference-router/src/routes/mod.rs",
        root / "inference-router/src/sre_proxy/mod.rs",
        root / "inference-router/src/sre_proxy/backend.rs",
    ]
    sources = [(path, path.read_text().splitlines()) for path in paths]
    summary = {"lines": len(text.splitlines()), "jsonEvents": 0, "sourceSites": []}
    for line in text.splitlines()[-150:]:
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if not isinstance(obj, dict):
            continue
        summary["jsonEvents"] += 1
        fields = obj.get("fields", obj)
        if not isinstance(fields, dict) or not isinstance(fields.get("message"), str):
            continue
        literal = json.dumps(fields["message"], ensure_ascii=False)
        for path, lines in sources:
            match = next((number for number, source in enumerate(lines, 1) if literal in source), None)
            if match:
                site = {"source": str(path.relative_to(root)), "line": match}
                if site not in summary["sourceSites"]:
                    summary["sourceSites"].append(site)
                break
    summary["sourceSites"] = summary["sourceSites"][:32]
    return summary


def firewall_summary(text):
    result = {"policies": [], "rules": []}
    table = None
    for line in text.splitlines():
        if line.startswith("*"):
            table = line[1:] if line[1:] in ("filter", "nat", "raw", "mangle", "security") else None
        if not table:
            continue
        policy = re.fullmatch(r":(INPUT|OUTPUT|FORWARD) (ACCEPT|DROP) \[([0-9]+):[0-9]+\]", line)
        if policy:
            result["policies"].append({"table": table, "chain": policy[1],
                                       "policy": policy[2], "packets": int(policy[3])})
        match = re.match(r"(?:\[([0-9]+):[0-9]+\] )?-A ([A-Za-z0-9_-]+) ", line)
        if not match:
            continue
        owner = re.search(r"(! )?--uid-owner (1000|1001)(?:\s|$)", line)
        action = re.search(r" -j (ACCEPT|DROP|REJECT|REDIRECT|RETURN)(?:\s|$)", line)
        result["rules"].append({
            "table": table, "chain": match[2] if match[2] in ("INPUT", "OUTPUT", "FORWARD") else "custom",
            "packets": int(match[1]) if match[1] else None,
            "action": action[1] if action else "jump-or-other",
            "owner": int(owner[2]) if owner else None,
            "ownerNegated": bool(owner and owner[1]),
            "loopback": bool(re.search(r"(?: -[io] lo)(?:\s|$)", line)),
            "established": "--ctstate" in line and "ESTABLISHED" in line,
        })
    result["rules"] = result["rules"][:40]
    return result


def exception_summary(error, root):
    result = {"category": type(error).__name__}
    frame = error.__traceback__
    while frame:
        source = str(frame.tb_frame.f_code.co_filename)
        prefix = str(root / "tests/e2e/sre_authority") + "/"
        if source.startswith(prefix):
            result["callSite"] = {"source": source[len(str(root)) + 1:],
                                  "function": frame.tb_frame.f_code.co_name, "line": frame.tb_lineno}
        frame = frame.tb_next
    response = getattr(error, "response", None)
    status = getattr(response, "status_code", None)
    if type(status) is not int or not 100 <= status <= 599:
        return result
    result["httpStatus"] = status
    try:
        body = response.json()
    except (ValueError, TypeError):
        return result
    result.update(response_summary(status, body, root))
    return result


def response_summary(status, body, root):
    result = {"httpStatus": status}
    if not isinstance(body, dict) or body.get("kind") != "Status":
        return result
    reason = body.get("reason")
    if isinstance(reason, str) and (reason in REASONS or reason == "SREProxyDenied"):
        result["reason"] = reason
    message = body.get("message")
    if not isinstance(message, str) or not 0 < len(message) <= 4096:
        return result
    literal = json.dumps(message, ensure_ascii=False)
    for name in ("mod.rs", "policy.rs", "backend.rs"):
        path = root / "inference-router/src/sre_proxy" / name
        for number, source in enumerate(path.read_text().splitlines(), 1):
            if literal in source:
                result["responseSite"] = {"source": str(path.relative_to(root)), "line": number}
                return result
    return result


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
        report["reason"] = reason if isinstance(reason, str) and reason in REASONS else "unclassified"
        report.update(failure_facts(body.get("message"), policies))
        details = body.get("details", {})
        if (code == 422 and reason == "Invalid" and isinstance(details, dict)
                and details.get("group") == "admissionregistration.k8s.io"
                and details.get("kind") == "ValidatingAdmissionPolicy"
                and isinstance(details.get("name"), str)
                and details.get("name") in policies):
            causes = []
            supplied = details.get("causes", [])
            if not isinstance(supplied, list):
                return report
            for cause in supplied[:32]:
                if not isinstance(cause, dict):
                    continue
                field, message = cause.get("field"), cause.get("message")
                location = re.fullmatch(
                    r"spec\.(matchConditions|variables|validations)\[(\d+)\]\.expression", field
                ) if isinstance(field, str) and len(field) <= 128 else None
                if not location:
                    continue
                if not isinstance(message, str):
                    continue
                allowed = FIELDS | {"metadata", "request", "object", "oldObject", "orValue", "has", "exists"}
                tokens = sorted(set(re.findall(
                    r"(?:undefined field|undeclared reference to) ['\"]([A-Za-z_][A-Za-z0-9_]*)['\"]",
                    message)) & allowed)
                entry = {"field": field, "knownTokens": tokens,
                    "categories": [category for category, needle in [
                        ("overload", "matching overload"), ("syntax", "Syntax error"),
                        ("undefined-field", "undefined field"), ("undeclared-reference", "undeclared reference"),
                        ("optional", "optional"), ("cost", "cost"),
                    ] if needle.lower() in message.lower()]}
                definitions = policies[details["name"]].get("spec", {}).get(location[1], [])
                index = int(location[2])
                expression = definitions[index].get("expression", "") if index < len(definitions) else ""
                vocabulary = allowed | set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", expression)) | {
                    "error", "input", "expression", "must", "evaluate", "evaluates", "return", "returns",
                    "type", "bool", "boolean", "string", "int", "list", "map", "dyn", "invalid", "argument",
                    "macro", "expected", "found", "no", "matching", "overload", "applied", "to", "in",
                    "not", "a", "an", "is", "of", "undeclared", "reference", "undefined", "field",
                    "mismatched", "extraneous", "syntax", "token", "reserved", "identifier", "unsupported",
                    "supported", "allowed", "size", "exceeds", "maximum", "limit", "cost", "compilation",
                }
                headline = message.partition("compilation failed:")[2].splitlines()
                if headline:
                    entry["compilerDescription"] = re.sub(
                        r"[A-Za-z0-9_]+",
                        lambda match: match[0] if match[0] in vocabulary or match[0].lower() in vocabulary else "[redacted]",
                        re.sub(r"[\x00-\x1f\x7f]", "?", headline[0].strip())[:512],
                    )
                causes.append(entry)
            report["compilationCauses"] = causes
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


def control_plane_status(obj):
    containers = []
    for container in obj.get("status", {}).get("containerStatuses", [])[:4]:
        state = container.get("state", {})
        previous = container.get("lastState", {}).get("terminated", {})
        containers.append({"name": identifier(container.get("name")),
            "ready": container.get("ready") is True, "restartCount": container.get("restartCount"),
            "state": next((name for name in ("running", "waiting", "terminated") if name in state), "unknown"),
            "previousExitCode": previous.get("exitCode"),
            "previousReason": previous.get("reason") if previous.get("reason") in ("Error", "Completed", "OOMKilled") else None})
    return {"name": identifier(obj.get("metadata", {}).get("name")), "containers": containers}


def public_stack_facts(text):
    """Only fixed panic classifications and public Go frame names, never log text/args."""
    facts = {"panicCategories": [], "publicFrames": []}
    for line in text.splitlines()[-240:]:
        line = line.strip()
        if line.startswith("panic:") or line.startswith("fatal error:"):
            category = next((name for name, phrase in [
                ("nil-pointer", "nil pointer dereference"),
                ("index-out-of-range", "index out of range"),
                ("interface-conversion", "interface conversion"),
                ("concurrent-map-write", "concurrent map writes"),
            ] if phrase in line), "panic-redacted")
            facts["panicCategories"].append(category)
        frame = re.match(r"^((?:k8s\.io/(?:kubernetes|apiserver|apiextensions-apiserver)|github\.com/google/cel-go)/[A-Za-z0-9_./@*()+-]+)\(", line)
        if frame and frame[1] not in facts["publicFrames"]:
            facts["publicFrames"].append(frame[1][:512])
    facts["panicCategories"] = sorted(set(facts["panicCategories"]))
    facts["publicFrames"] = facts["publicFrames"][:40]
    return facts


def controller_stack(context, name):
    if name != "kube-controller-manager-kars-e2e-control-plane" or context != "kind-kars-e2e":
        return {"available": False}
    result = {"available": False, "current": {}, "previous": {}}
    for previous in (False, True):
        try:
            command = ["kubectl", "--context", context, "--request-timeout=10s", "logs",
                       "-n", "kube-system", name, "--tail=240"]
            if previous:
                command.append("--previous")
            output = subprocess.run(command, capture_output=True, text=True, timeout=15, check=False)
            if output.returncode == 0:
                result["available"] = True
                result["previous" if previous else "current"] = public_stack_facts(output.stdout)
        except (OSError, subprocess.TimeoutExpired):
            pass
    return result


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
    code, body = request(port, "GET", "/api/v1/namespaces/kube-system/pods?labelSelector=component%3Dkube-controller-manager")
    if code == 200 and isinstance(body, dict):
        report["controlPlane"] = [control_plane_status(obj) for obj in body.get("items", [])[:4]
                                  if obj.get("metadata", {}).get("name") == "kube-controller-manager-kars-e2e-control-plane"]
    return report
