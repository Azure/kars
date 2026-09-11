"""Case-bounded metadata audit outcomes for two UID-proven native actors."""

from collections import Counter
from datetime import datetime, timezone
import json
import subprocess

from native_api import CORE, ROOT, require, resource
from observation_diagnostics import READ_ERRORS, UNAVAILABLE, identity, recheck_actor, resolve_actor

MAX_BYTES = 4 * 1024 * 1024
MAX_LINE_BYTES = 65536
ACTORS = {"bff_writer": "bff", "observer_router": "router"}
POD_UID = "authentication.kubernetes.io/pod-uid"
POD_NAME = "authentication.kubernetes.io/pod-name"


def read_audit_tail():
    result = subprocess.run(
        ["docker", "exec", "bridge-native-control-plane", "tail", "-c", str(MAX_BYTES),
         "/var/log/kars-native-audit/audit.log"],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=False,
    )
    require(result.returncode == 0 and len(result.stdout) <= MAX_BYTES, "Metadata audit unavailable")
    return result.stdout


def timestamp(value):
    if not isinstance(value, str) or len(value) > 64:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed if parsed.tzinfo is not None else None
    except ValueError:
        return None


def status_category(code):
    if 200 <= code < 300:
        return "successful-response"
    return {
        401: "unauthenticated-response", 403: "denied-response", 404: "not-found-response",
        409: "conflict-response", 422: "invalid-response", 429: "rate-limited-response",
        408: "timeout-response", 504: "timeout-response",
    }.get(code, "server-error-response" if code >= 500 else "other-response")


def rules_for(setup, kind, target, actor):
    require(isinstance(target, dict) and isinstance(target.get("uid"), str) and target["uid"], UNAVAILABLE)
    namespace, name = target["workspace"], target["sandbox"]
    require((kind == "observer_router" and namespace == CORE)
            or (kind == "bff_writer" and namespace == "native-delivery"
                and name in ("native-delivery-a", "native-delivery-b")), UNAVAILABLE)
    path = resource(namespace, "karssandboxes", name)
    value = setup.admin.get(path)
    require(identity(value)[0] == target["uid"]
            and value["metadata"].get("name") == name
            and value["metadata"].get("namespace") == namespace, UNAVAILABLE)
    actor["anchors"][path] = value
    actor["targetUid"] = identity(value)[0]
    rules = {("get", "kars.azure.com", "v1alpha1", "karssandboxes", namespace, name)}
    if kind == "bff_writer":
        namespace_path = f"/api/v1/namespaces/{namespace}"
        workspace = setup.admin.get(namespace_path)
        identity(workspace)
        require(workspace["metadata"].get("name") == namespace, UNAVAILABLE)
        actor["anchors"][namespace_path] = workspace
        source = f"kars-credential-input-sandbox-{name}"
        rules.update({
            ("get", "kars.azure.com", "v1alpha1", "karscredentialgrants", namespace, "workspace"),
            ("patch", "kars.azure.com", "v1alpha1", "karssandboxes", namespace, name),
            ("get", "", "v1", "namespaces", None, namespace),
            ("get", "", "v1", "namespaces", None, f"kars-{name}"),
            ("create", "", "v1", "secrets", namespace, source),
            ("get", "", "v1", "secrets", namespace, source),
            ("patch", "", "v1", "secrets", namespace, source),
            # Authorization may reject CREATE before an object name is decoded.
            ("create", "", "v1", "secrets", namespace, None),
        })
    return rules


def project(raw, actor, rules, since, until):
    require(isinstance(raw, bytes) and len(raw) <= MAX_BYTES, "Metadata audit unavailable")
    counts = Counter()
    pods = {pod["uid"]: pod["name"] for pod in actor["pods"]}
    for line in raw.splitlines():
        if len(line) > MAX_LINE_BYTES:
            continue
        try:
            event = json.loads(line)
            if (not isinstance(event, dict) or event.get("level") != "Metadata"
                    or event.get("stage") != "ResponseComplete" or event.get("impersonatedUser") is not None):
                continue
            user = event.get("user") or {}
            extra = user.get("extra") or {}
            pod_uid = extra.get(POD_UID)
            if (user.get("username") != actor["username"] or user.get("uid") != actor["serviceAccountUid"]
                    or not isinstance(pod_uid, list) or len(pod_uid) != 1
                    or not isinstance(pod_uid[0], str) or pod_uid[0] not in pods
                    or (POD_NAME in extra and extra[POD_NAME] != [pods[pod_uid[0]]])):
                continue
            started, completed = timestamp(event.get("requestReceivedTimestamp")), timestamp(event.get("stageTimestamp"))
            if started is None or completed is None or not since <= started <= completed <= until:
                continue
            ref = event.get("objectRef") or {}
            if ref.get("subresource"):
                continue
            key = (event.get("verb"), ref.get("apiGroup", ""), ref.get("apiVersion"),
                   ref.get("resource"), ref.get("namespace") or None, ref.get("name") or None)
            if key not in rules:
                continue
            if ref.get("resource") == "karssandboxes" and ref.get("uid") not in (None, "", actor["targetUid"]):
                continue
            code = (event.get("responseStatus") or {}).get("code")
            if type(code) is not int or not 100 <= code <= 599:
                continue
            counts[(*key[:4], code, status_category(code))] += 1
        except (ValueError, TypeError, AttributeError):
            continue
    return [
        {"verb": key[0], "apiGroup": key[1], "apiVersion": key[2], "resource": key[3],
         "http_status": key[4], "category": key[5], "count": count}
        for key, count in list(counts.items())[-24:]
    ]


def collect(setup, kind, target, since):
    result = {"actor": kind if kind in ACTORS else "unknown", "available": False,
              "category": "source-unavailable", "coverage": "bounded-metadata-tail", "outcomes": []}
    try:
        until = datetime.now(timezone.utc)
        require(isinstance(since, datetime) and since.tzinfo is not None
                and 0 <= (until - since).total_seconds() <= 600, UNAVAILABLE)
        actor = resolve_actor(setup, ACTORS[kind], target)
        rules = rules_for(setup, kind, target, actor)
        recheck_actor(setup, actor)
    except READ_ERRORS:
        return result
    try:
        raw = read_audit_tail()
        outcomes = project(raw, actor, rules, since, until)
    except READ_ERRORS:
        result["category"] = "audit-unavailable"
        return result
    try:
        recheck_actor(setup, actor)
    except READ_ERRORS:
        return result
    result.update(available=bool(outcomes), outcomes=outcomes,
                  category="outcomes-retained" if outcomes else "no-matching-evidence")
    return result
