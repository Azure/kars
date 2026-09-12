# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""The same bounded negative PATCH/restore used by full and early native tests."""

from contextlib import contextmanager
import copy
import json

from .common import SYSTEM, require
from .registration_schema import CRD_PATH, write_report
from .ssa_diagnostics import manager_class

TASK_NAME = "karstasks.kars.azure.com"
TASK_PATH = f"{CRD_PATH}/{TASK_NAME}"


def read_task_schema(h):
    obj = h.api("GET", TASK_PATH, status=200).json()
    require(isinstance(obj, dict) and obj.get("kind") == "CustomResourceDefinition"
            and obj.get("apiVersion") == "apiextensions.k8s.io/v1",
            "Task fixture did not read the actual CRD")
    meta = obj.get("metadata", {})
    require(meta.get("name") == TASK_NAME and not meta.get("deletionTimestamp")
            and all(isinstance(meta.get(key), str) and meta[key] for key in ("uid", "resourceVersion")),
            "Task fixture lost its stable CRD identity")
    return obj


def require_task_owner(obj):
    meta = obj["metadata"]
    annotations = meta.get("annotations", {})
    require(meta.get("labels", {}).get("app.kubernetes.io/managed-by") == "Helm"
            and annotations.get("meta.helm.sh/release-name") == "kars"
            and annotations.get("meta.helm.sh/release-namespace") == SYSTEM
            and not meta.get("ownerReferences"), "Task fixture refuses foreign CRD ownership")


def task_manager_facts(obj):
    entries = obj["metadata"].get("managedFields", [])
    require(isinstance(entries, list) and len(entries) <= 64, "Task managedFields evidence is unbounded or malformed")
    result = []
    for entry in entries:
        require(isinstance(entry, dict), "Task managedFields entry is malformed")
        fields = entry.get("fieldsV1", {})
        require(isinstance(fields, dict), "Task managedFields field set is malformed")
        spec = fields.get("f:spec", {})
        metadata = fields.get("f:metadata", {})
        require(isinstance(spec, dict) and isinstance(metadata, dict), "Task managedFields evidence has invalid field roots")
        annotations = metadata.get("f:annotations", {})
        require(isinstance(annotations, dict), "Task managedFields annotations are malformed")
        versions = spec.get("f:versions")
        require(versions is None or isinstance(versions, dict), "Task version field ownership is malformed")
        if versions is not None or "f:meta.helm.sh/release-name" in annotations:
            result.append({
                "managerClass": manager_class(entry.get("manager")),
                "operation": entry.get("operation") if entry.get("operation") in ("Apply", "Update") else "other",
                "subresource": entry.get("subresource", "") if entry.get("subresource", "") in ("", "status") else "other",
                "versionsClaim": "absent" if versions is None else "whole" if not versions or "." in versions else "nested",
                "releaseNameClaim": "f:meta.helm.sh/release-name" in annotations,
            })
    return sorted(result, key=lambda item: json.dumps(item, sort_keys=True))


def _values(obj):
    return {"spec": obj["spec"], "metadata": {
        key: value for key, value in obj["metadata"].items()
        if key not in ("resourceVersion", "managedFields", "generation")}}


@contextmanager
def task_schema_conflict(h, fault):
    require(fault in ("owner", "schema"), "Unknown Task negative fixture")
    original = read_task_schema(h)
    require_task_owner(original)
    expected = copy.deepcopy(original)
    patch = {"metadata": {"uid": original["metadata"]["uid"],
                          "resourceVersion": original["metadata"]["resourceVersion"]}}
    if fault == "owner":
        patch["metadata"]["annotations"] = {"meta.helm.sh/release-name": "foreign-fixture"}
        expected["metadata"]["annotations"]["meta.helm.sh/release-name"] = "foreign-fixture"
    else:
        expected["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "Unreviewed public fixture description"
        patch["spec"] = expected["spec"]
    before_managers = task_manager_facts(original)
    # Deliberately preserve the original default-manager PATCH contract until
    # native evidence establishes whether it changes SSA field ownership.
    h.api("PATCH", TASK_PATH, body=patch, status=200)
    try:
        changed = read_task_schema(h)
        require(_values(changed) == _values(expected), "Task negative fixture changed outside its exact intended delta")
        yield changed
    finally:
        live = read_task_schema(h)
        require(_values(live) == _values(expected), "Task changed externally; fixture restoration was not issued")
        restore = {"metadata": {"uid": original["metadata"]["uid"],
                                "resourceVersion": live["metadata"]["resourceVersion"]},
                   "spec": original["spec"]}
        if fault == "owner":
            restore["metadata"]["annotations"] = {
                "meta.helm.sh/release-name": original["metadata"]["annotations"]["meta.helm.sh/release-name"]}
        h.api("PATCH", TASK_PATH, body=restore, status=200)
        restored = read_task_schema(h)
        require(_values(restored) == _values(original), "Task fixture did not restore its exact original values and UID")
        require_task_owner(restored)
        write_report(h.root, f"migration-seed-task-{fault}-restore.json", {
            "kind": "KarsTask", "case": fault, "valuesAndUidRestored": True,
            "beforeManagers": before_managers, "afterManagers": task_manager_facts(restored),
        })
