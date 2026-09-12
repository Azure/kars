# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Shared historical seed bodies and strict API probes, not schema migration."""

import copy
import json
import re

from .common import SYSTEM, require
from .registration_schema import write_report

SEEDS = (
    ("karstask", "karstasks", "KarsTask"),
    ("karsteam", "karsteams", "KarsTeam"),
    ("mcpserver", "mcpservers", "McpServer"),
    ("karseval", "karsevals", "KarsEval"),
    ("karssreaction", "karssreactions", "KarsSREAction"),
)
WORKLOADS = (
    "/api/v1/pods", "/api/v1/replicationcontrollers",
    "/apis/apps/v1/deployments", "/apis/apps/v1/replicasets",
    "/apis/apps/v1/statefulsets", "/apis/apps/v1/daemonsets",
    "/apis/batch/v1/jobs", "/apis/batch/v1/cronjobs",
)


def seed_definitions():
    envelope = {"tier": 1, "authorityCeiling": 1,
                "budget": {"tokens": 20, "usdMicros": 0}}
    specs = [
        {"objective": "Inert migration data", "envelope": envelope, "execution": {"launch": False}},
        {"charter": "Inert migration data", "envelope": envelope, "roster": []},
        {"url": "https://migration-fixture.invalid/", "productionMode": False},
        {"corpus": {"builtin": "sre"}, "targetSandboxRef": {"name": "sre"}},
        {"action": {"type": "ScaleDeployment", "params": {
            "namespace": SYSTEM, "name": "kars-controller", "replicas": 0,
            "opaque": {"nested": [1, "retained", True]},
        }}, "approval": {"state": "Rejected"}},
    ]
    return [(resource, {"apiVersion": "kars.azure.com/v1alpha1", "kind": kind,
                        "metadata": {"name": f"e2e-migration-{resource}", "namespace": SYSTEM},
                        "spec": copy.deepcopy(spec)})
            for (resource, _plural, kind), spec in zip(SEEDS, specs)]


def collection_path(resource):
    plural = next((plural for singular, plural, _kind in SEEDS if singular == resource), None)
    require(plural is not None, "Unrecognized historical migration seed")
    return f"/apis/kars.azure.com/v1alpha1/namespaces/{SYSTEM}/{plural}"


def _field_paths(value, path=""):
    result = {path} if path else set()
    if isinstance(value, dict):
        for key, item in value.items():
            result |= _field_paths(item, f"{path}.{key}" if path else key)
    elif isinstance(value, list):
        for index, item in enumerate(value):
            result |= _field_paths(item, f"{path}[{index}]")
    return result


def seed_status(resource, code, body):
    expected = dict(seed_definitions())[resource]
    allowed = _field_paths(expected)
    report = {"kind": expected["kind"], "httpStatus": code, "category": "unexpected-response",
              "fields": [], "validation": []}
    if not isinstance(body, dict) or body.get("kind") != "Status":
        return report
    reasons = {"Invalid", "Forbidden", "Unauthorized", "NotFound", "AlreadyExists",
               "Conflict", "BadRequest", "InternalError", "ServiceUnavailable"}
    if isinstance(body.get("reason"), str) and body["reason"] in reasons:
        report["category"] = body["reason"]
    fields, validation = set(), set()
    details = body.get("details")
    causes = details.get("causes", []) if isinstance(details, dict) else []
    if isinstance(causes, list):
        for cause in causes[:32]:
            if not isinstance(cause, dict):
                continue
            field = cause.get("field")
            if isinstance(field, str) and field in allowed:
                fields.add(field)
            category = {"FieldValueRequired": "required-field", "FieldValueInvalid": "invalid-field",
                        "FieldValueNotSupported": "unsupported-field"}.get(
                            cause["reason"]) if isinstance(cause.get("reason"), str) else None
            if category:
                validation.add(category)
    message = body.get("message")
    if isinstance(message, str):
        # BadRequest strict-decoding errors often have no structured causes.
        # Only exact field paths in our fixed public bodies may leave this parser.
        for field in re.findall(r'unknown field "([^"\r\n]{1,256})"', message[:16384]):
            if field in allowed:
                fields.add(field)
        for needle, category in (("unknown field", "unknown-field"), ("strict decoding error", "strict-decoding"),
                                 ("cannot unmarshal", "type-mismatch")):
            if needle in message[:16384]:
                validation.add(category)
    report["fields"], report["validation"] = sorted(fields), sorted(validation)
    return report


def _retains_input(expected, actual):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(key in actual and _retains_input(value, actual[key])
                                                for key, value in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(expected) == len(actual) and all(
            _retains_input(left, right) for left, right in zip(expected, actual))
    return type(expected) is type(actual) and expected == actual


class SeedRejected(AssertionError):
    pass


def request_seed(h, resource, obj, *, dry_run):
    expected = dict(seed_definitions()).get(resource)
    require(expected is not None and json.dumps(obj, sort_keys=True) == json.dumps(expected, sort_keys=True),
            "Only the exact public historical seed body may be submitted")
    mode = "server-dry-run" if dry_run else "create"
    filename = f"migration-seed-{resource}-{mode}.json"
    write_report(h.root, filename, {"kind": expected["kind"], "mode": mode,
                                   "httpStatus": None, "category": "requesting"})
    path = (collection_path(resource) + "?fieldManager=kubectl-create&fieldValidation=Strict"
            + ("&dryRun=All" if dry_run else ""))
    response = h.api("POST", path, body=obj)
    try:
        body = response.json()
    except (ValueError, TypeError):
        body = None
    report = seed_status(resource, response.status_code, body)
    report["mode"] = mode
    if response.status_code == 201 and isinstance(body, dict) and body.get("kind") == expected["kind"]:
        meta = body.get("metadata")
        identity = isinstance(meta, dict) and all(meta.get(key) == expected["metadata"][key]
                                                 for key in ("name", "namespace"))
        if not dry_run:
            identity = identity and all(isinstance(meta.get(key), str) and meta[key] for key in ("uid", "resourceVersion"))
        if (identity and body.get("apiVersion") == expected["apiVersion"]
                and _retains_input(expected["spec"], body.get("spec")) and not body.get("status")):
            report["category"] = "accepted"
        else:
            report["category"] = "identity-or-data-round-trip"
    write_report(h.root, filename, report)
    if report["category"] != "accepted":
        raise SeedRejected(f"Historical seed {expected['kind']} {mode} rejected: "
                           f"HTTP {response.status_code}; category={report['category']}")
    return body


def _inventory(h, path):
    response = h.api("GET", path + "?limit=513", status=200)
    body = response.json()
    require(isinstance(body, dict) and isinstance(body.get("items"), list)
            and isinstance(body.get("metadata", {}), dict)
            and not body.get("metadata", {}).get("continue") and len(body["items"]) <= 512,
            "Historical seed inventory must be complete and bounded")
    items = body["items"]
    require(all(isinstance(item, dict) and isinstance(item.get("metadata"), dict)
                and all(isinstance(item["metadata"].get(key), str) and item["metadata"][key]
                        for key in ("name", "uid", "resourceVersion")) for item in items),
            "Historical seed inventory lacks real API identities")
    require(len({item["metadata"]["uid"] for item in items}) == len(items),
            "Historical seed inventory contains duplicate identities")
    return sorted(items, key=lambda item: item["metadata"]["uid"])


def _snapshot(h):
    controller = h.get("deployment", "kars-controller", SYSTEM)
    require(controller and controller.get("spec", {}).get("replicas") == 0
            and all(controller.get("status", {}).get(key, 0) == 0
                    for key in ("replicas", "readyReplicas", "availableReplicas", "updatedReplicas"))
            and all(controller.get("metadata", {}).get(key) for key in ("uid", "resourceVersion")),
            "Historical seed dry-runs require the actual controller paused with a stable identity")
    state = {"controller": controller}
    for resource, _plural, _kind in SEEDS:
        state[resource] = _inventory(h, collection_path(resource))
        require(not any(obj["metadata"]["name"] == f"e2e-migration-{resource}" for obj in state[resource]),
                "Historical seed already exists; no collision or adoption is permitted")
    for path in WORKLOADS:
        objects = _inventory(h, path)
        if path == "/api/v1/pods":
            require(not any(obj["metadata"].get("namespace") == SYSTEM
                            and obj.get("spec", {}).get("serviceAccountName") == "kars-controller" for obj in objects),
                    "Controller Pods remain during historical seed dry-runs")
        # Kubelet/controller status updates are unrelated to dry-run persistence.
        # Pin every workload UID, desired spec and non-server-managed metadata.
        state[path] = [{**{key: obj.get(key) for key in ("apiVersion", "kind", "spec")},
                        "metadata": {key: value for key, value in obj["metadata"].items()
                                     if key not in ("resourceVersion", "managedFields")}} for obj in objects]
    encoded = json.dumps(state, sort_keys=True, separators=(",", ":"))
    require(len(encoded.encode()) <= 8 * 1024 * 1024, "Historical seed inventory exceeds its 8 MiB bound")
    return encoded


def dry_run_seed_data(h):
    before = _snapshot(h)
    rejected = 0
    try:
        for resource, obj in seed_definitions():
            try:
                request_seed(h, resource, obj, dry_run=True)
            except SeedRejected:
                rejected += 1
    finally:
        require(_snapshot(h) == before, "Historical seed dry-runs changed stored data, identity or workload intent")
    require(rejected == 0, f"{rejected} historical seed bodies failed strict server dry-run; see fixed kind/field diagnostics")
    h.passed("All five historical seed bodies passed strict server dry-run without persistence or workload changes")
