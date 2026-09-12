# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Closed SSA diagnostics shared by the early probe and retained CLI facts."""

import json
import re

MANAGERS = {"helm", "python-httpx", "Python-urllib", "kubectl-patch", "kubectl",
            "kubectl-client-side-apply", "kars-schema-stage"}
PATHS = {
    ".spec.versions": "spec/versions",
    ".metadata.annotations.meta.helm.sh/release-name": "metadata/annotations/meta.helm.sh/release-name",
    ".metadata.annotations.meta.helm.sh/release-namespace": "metadata/annotations/meta.helm.sh/release-namespace",
    ".metadata.annotations.kars.azure.com/core-schema-owner": "metadata/annotations/kars.azure.com/core-schema-owner",
    ".metadata.annotations.kars.azure.com/core-schema-spec": "metadata/annotations/kars.azure.com/core-schema-spec",
    ".metadata.labels.app.kubernetes.io/managed-by": "metadata/labels/app.kubernetes.io/managed-by",
}
CONFLICT_KEYS = {"conflictKind", "conflictCount", "conflictFields", "conflictManagers"}


def manager_class(value):
    return value if isinstance(value, str) and value in MANAGERS else "other"


def ssa_conflict(stderr):
    if re.search(r'^(?:error: )?Operation cannot be fulfilled on customresourcedefinitions(?:\.apiextensions\.k8s\.io)? '
                 r'"[^"\r\n]+": the object has been modified; please apply your changes to the latest version and try again\.?$',
                 stderr[:16384], re.M):
        return {"conflictKind": "resource-version"}
    match = re.search(r"^(?:error: )?Apply failed with ([1-9][0-9]?) conflicts?: ", stderr[:16384], re.M)
    if not match or int(match[1]) > 32:
        return None
    text = stderr[match.end():16384].split("\nPlease review the fields above")[0].strip()
    fields, managers = [], set()
    for line in text.splitlines():
        manager = re.match(r'^conflicts? with ("(?:[^"\\]|\\.)*")', line)
        if manager:
            try:
                managers.add(manager_class(json.loads(manager[1])))
            except ValueError:
                return None
            field = re.search(r": (\.[^\r\n]+)$", line)
            if field:
                fields.append(PATHS.get(field[1], "other"))
            elif not line.endswith(":"):
                return None
        elif managers and line.startswith("- ."):
            fields.append(PATHS.get(line[2:], "other"))
        else:
            return None
    if len(fields) != int(match[1]) or not managers or len(managers) > len(fields):
        return None
    return {"conflictKind": "field-manager", "conflictCount": len(fields),
            "conflictFields": sorted(set(fields)), "conflictManagers": sorted(managers)}


def valid_conflict_facts(value):
    if not (set(value) & CONFLICT_KEYS):
        return True
    if value.get("conflictKind") == "resource-version":
        return (set(value) & CONFLICT_KEYS == {"conflictKind"}
                and value.get("category") == "api-rejection" and value.get("reason") == "Conflict")
    return (
        CONFLICT_KEYS <= set(value) and value.get("conflictKind") == "field-manager"
        and value.get("category") == "api-rejection" and value.get("reason") == "Conflict"
        and type(value.get("conflictCount")) is int and 1 <= value["conflictCount"] <= 32
        and all(isinstance(value.get(key), list) and 1 <= len(value[key]) <= value["conflictCount"]
                and all(isinstance(item, str) and item in allowed for item in value[key])
                for key, allowed in (("conflictFields", set(PATHS.values()) | {"other"}),
                                     ("conflictManagers", MANAGERS | {"other"})))
    )
