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
NESTED_FIELD = "spec.action.params.opaque.nested"
PENDING_POLICY = "kars-sre-pending-proposals"
PENDING_REQUIREMENT = "SRE actions must be created Pending; approval is a separate operator action"


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
            "opaque": "retained",
        }}, "approval": {"state": "Rejected"}},
    ]
    return [(resource, {"apiVersion": "kars.azure.com/v1alpha1", "kind": kind,
                        "metadata": {"name": f"e2e-migration-{resource}", "namespace": SYSTEM},
                        "spec": copy.deepcopy(spec)})
            for (resource, _plural, kind), spec in zip(SEEDS, specs)]


def nested_action_definition(*, after_migration):
    obj = dict(seed_definitions())["karssreaction"]
    suffix = "after" if after_migration else "before"
    obj["metadata"]["name"] += f"-nested-{suffix}"
    obj["spec"]["action"]["params"]["opaque"] = {"nested": [1, "retained", True]}
    if after_migration:
        # The new CREATE-only guard requires Pending; the stored legacy
        # Rejected action is preserved and is never approved or rewritten.
        obj["spec"]["approval"]["state"] = "Pending"
    return obj


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
    if resource == "karssreaction":
        allowed |= _field_paths(nested_action_definition(after_migration=False))
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
        if (resource == "karssreaction" and code == 403 and body.get("reason") == "Forbidden"
                and PENDING_REQUIREMENT in message[:16384]
                and any(quoted in message[:16384] for quoted in (f"'{PENDING_POLICY}'", f'"{PENDING_POLICY}"'))):
            report["admissionRules"] = [PENDING_POLICY]
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


def _write_seed_report(h, resource, mode, report):
    report["mode"] = mode
    write_report(h.root, f"migration-seed-{resource}-{mode}.json", report)


def _submit_seed(h, resource, expected, *, dry_run, mode):
    _write_seed_report(h, resource, mode, {"kind": expected["kind"],
                                         "httpStatus": None, "category": "requesting"})
    path = (collection_path(resource) + "?fieldManager=kubectl-create&fieldValidation=Strict"
            + ("&dryRun=All" if dry_run else ""))
    response = h.api("POST", path, body=expected)
    try:
        body = response.json()
    except (ValueError, TypeError):
        body = None
    report = seed_status(resource, response.status_code, body)
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
    return body, report


def request_seed(h, resource, obj, *, dry_run):
    expected = dict(seed_definitions()).get(resource)
    require(expected is not None and json.dumps(obj, sort_keys=True) == json.dumps(expected, sort_keys=True),
            "Only the exact public historical seed body may be submitted")
    mode = "server-dry-run" if dry_run else "create"
    body, report = _submit_seed(h, resource, expected, dry_run=dry_run, mode=mode)
    _write_seed_report(h, resource, mode, report)
    if report["category"] != "accepted":
        raise SeedRejected(f"Historical seed {expected['kind']} {mode} rejected: "
                           f"HTTP {report['httpStatus']}; category={report['category']}")
    return body


def _request_nested_params(h, *, after_migration):
    crd = h.get("crd", "karssreactions.kars.azure.com")
    versions = crd.get("spec", {}).get("versions", []) if isinstance(crd, dict) else []
    require(len(versions) == 1, "Nested params probe requires the single reviewed action API version")
    params = (versions[0].get("schema", {}).get("openAPIV3Schema", {}).get("properties", {}).get("spec", {})
              .get("properties", {}).get("action", {}).get("properties", {}).get("params"))
    expected_schema = ({"type": "object", "x-kubernetes-preserve-unknown-fields": True} if after_migration
                       else {"type": "object", "additionalProperties": True})
    require(isinstance(params, dict) and {key: value for key, value in params.items() if key != "description"} == expected_schema,
            "Nested params probe is on the wrong side of the actual schema migration")
    expected = nested_action_definition(after_migration=after_migration)
    mode = "nested-after-server-dry-run" if after_migration else "nested-before-server-dry-run"
    body, report = _submit_seed(h, "karssreaction", expected, dry_run=True, mode=mode)
    if after_migration:
        matched = (report["category"] == "accepted"
                   and json.dumps(body["spec"]["action"]["params"], sort_keys=True)
                   == json.dumps(expected["spec"]["action"]["params"], sort_keys=True))
    else:
        message = body.get("message") if isinstance(body, dict) else None
        matched = (report["httpStatus"] == 400 and report["category"] == "BadRequest"
                   and report["fields"] == [NESTED_FIELD]
                   and report["validation"] == ["strict-decoding", "unknown-field"]
                   and isinstance(message, str) and len(message) <= 16384
                   and re.findall(r'unknown field "([^"\r\n]*)"', message) == [NESTED_FIELD])
    report.update(expectedHttpStatus=201 if after_migration else 400, matched=bool(matched))
    _write_seed_report(h, "karssreaction", mode, report)
    require(matched, f"Nested action params {mode} did not satisfy the exact expected API result")


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


def _snapshot(h, *, after_migration=False):
    controller = h.get("deployment", "kars-controller", SYSTEM)
    require(controller and controller.get("spec", {}).get("replicas") == 0
            and all(controller.get("status", {}).get(key, 0) == 0
                    for key in ("replicas", "readyReplicas", "availableReplicas", "updatedReplicas"))
            and all(controller.get("metadata", {}).get(key) for key in ("uid", "resourceVersion")),
            "Historical seed dry-runs require the actual controller paused with a stable identity")
    state = {"controller": controller}
    action_crd = h.get("crd", "karssreactions.kars.azure.com")
    require(action_crd and all(action_crd.get("metadata", {}).get(key) for key in ("uid", "resourceVersion")),
            "Nested params probe requires the real action CRD identity")
    state["actionSchema"] = action_crd
    for resource, _plural, _kind in SEEDS:
        state[resource] = _inventory(h, collection_path(resource))
        absent = {f"e2e-migration-{resource}"} if not after_migration else set()
        if resource == "karssreaction":
            absent |= {nested_action_definition(after_migration=phase)["metadata"]["name"] for phase in (False, True)}
        require(not any(obj["metadata"]["name"] in absent for obj in state[resource]),
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
        require(rejected == 0, f"{rejected} historical seed bodies failed strict server dry-run; see fixed kind/field diagnostics")
        _request_nested_params(h, after_migration=False)
    finally:
        require(_snapshot(h) == before, "Historical seed dry-runs changed stored data, identity or workload intent")
    h.passed("All five historical seed bodies passed strict server dry-run without persistence or workload changes")
    h.passed("Historical nested action params rejected at the exact observed unknown field without persistence")


def prove_nested_params_support(h):
    before = _snapshot(h, after_migration=True)
    try:
        _request_nested_params(h, after_migration=True)
    finally:
        require(_snapshot(h, after_migration=True) == before,
                "Post-migration nested params dry-run changed stored data, identity or workload intent")
    h.passed("Migrated action API retained nested params unchanged in a nonexecuting, nonpersistent server dry-run")
