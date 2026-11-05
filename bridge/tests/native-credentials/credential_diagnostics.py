# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Secret-free failure categories and private-scope metadata booleans."""

import re

from native_api import Failure, resource

PREFIX = "kars.azure.com/private-"
CATEGORIES = {
    "governed credential source or operator grant is unavailable": "source_or_grant",
    "Sandbox identity/reference changed": "sandbox_rv_drift",
    "runtime namespace authority changed": "namespace_authority",
    "governed credential inputs are no longer authorized": "inputs_unavailable",
    "Private target namespace requires reviewed grant activation before issuance or reuse": "runtime_scope_unreviewed",
    "Private capability is unqualified; regenerate and apply the reviewed grant activation": "private_qualification_invalid",
    "SRE privacy qualification is still pending; no credential issued or reused": "privacy_pending",
}
GOVERNED_CATEGORIES = {
    "binding_shape", "grant_identity", "target_identity", "owner_authority",
    "source_identity", "source_metadata", "source_owner", "legacy_review",
    "bundle_owner", "bundle_missing", "target_changed", "grant_changed", "source_changed",
    "grant_api", "namespace_api", "target_api", "source_metadata_api", "source_value_api",
    "bundle_read_api", "bundle_create_api", "bundle_bind_api", "bundle_write_api",
    "source_recheck_api", "source_bind_api", "legacy_import_api", "legacy_identity_api",
    "legacy_namespace_api", "legacy_metadata_api", "legacy_target_api", "legacy_value_api",
    "unclassified",
}


def condition_category(message):
    if not isinstance(message, str):
        return "unclassified"
    message = message.removeprefix("CredentialSourceUnavailable: ")
    detail = re.fullmatch(
        r"governed credential source or operator grant is unavailable "
        r"\[([a-z_]+); code=(?:None|Some\(([1-5][0-9]{2})\))\]", message,
    )
    if detail and detail[1] in GOVERNED_CATEGORIES:
        return "source_or_grant:" + detail[1] + (":" + detail[2] if detail[2] else "")
    return CATEGORIES.get(message, "unclassified")


NAMESPACE_CONDITIONS = {
    "NamespaceDeletionDiscoveryFailure": {"DiscoveryFailed", "ResourcesDiscovered"},
    "NamespaceDeletionGroupVersionParsingFailure": {"GroupVersionParsingFailed", "ParsedGroupVersions"},
    "NamespaceDeletionContentFailure": {"ContentDeletionFailed", "ContentDeleted"},
    "NamespaceContentRemaining": {"SomeResourcesRemain", "ContentRemoved"},
    "NamespaceFinalizersRemaining": {"SomeFinalizersRemain", "ContentHasNoFinalizers"},
}
NAMESPACE_FINALIZERS = {"kubernetes", "kars.azure.com/namespace-cleanup"}


def namespace_finalizers(values):
    if not isinstance(values, list) or not all(isinstance(value, str) for value in values):
        raise TypeError("Malformed finalizer metadata")
    return {"known": sorted(set(values) & NAMESPACE_FINALIZERS),
            "otherCount": sum(value not in NAMESPACE_FINALIZERS for value in values)}


def metrics_discovery(setup):
    """The chart's aggregated API can block otherwise empty Namespace retirement."""
    try:
        api = setup.admin.get("/apis/apiregistration.k8s.io/v1/apiservices/v1beta1.metrics.k8s.io")
        conditions = (api.get("status") or {}).get("conditions") or []
        available = [item for item in conditions if item.get("type") == "Available"]
        condition = available[0] if len(available) == 1 else {}
        reasons = {"Passed", "Local", "ServiceNotFound", "ServiceAccessError", "ServicePortError",
                   "EndpointsNotFound", "EndpointsAccessError", "MissingEndpoints", "FailedDiscoveryCheck"}
        return {"available": True, "registered": api.get("spec", {}).get("group") == "metrics.k8s.io",
                "status": condition.get("status") if condition.get("status") in ("True", "False", "Unknown") else "OtherOrAbsent",
                "reason": condition.get("reason") if condition.get("reason") in reasons else "OtherOrAbsent"}
    except Exception:
        return {"available": False, "category": "api-unavailable"}


def namespace_retirement(setup):
    """Failure-only lifecycle facts, including orphaned native runtime namespaces."""
    try:
        objects = setup.admin.get("/api/v1/namespaces")["items"]
        if not isinstance(objects, list):
            raise TypeError("Malformed namespace list")
        selected = []
        for item in objects:
            meta = item["metadata"]
            name = meta["name"]
            annotations = meta.get("annotations") or {}
            if name.startswith("kars-native-") or "kars.azure.com/sandbox-name" in annotations:
                selected.append(item)
        selected.sort(key=lambda item: item["metadata"]["name"])
        namespaces = []
        for item in selected[:64]:
            meta = item["metadata"]
            if not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", meta["name"]) or not re.fullmatch(
                r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}", meta["uid"],
            ):
                raise TypeError("Malformed namespace identity")
            status = item.get("status") or {}
            conditions = status.get("conditions") or []
            phase = status.get("phase")
            namespaces.append({
                "name": meta["name"], "uid": meta["uid"],
                "deleting": meta.get("deletionTimestamp") is not None,
                "phase": phase if phase in ("Active", "Terminating") else "OtherOrAbsent",
                "metadataFinalizers": namespace_finalizers(meta.get("finalizers", [])),
                "specFinalizers": namespace_finalizers((item.get("spec") or {}).get("finalizers", [])),
                "conditions": [{
                    "type": condition["type"] if condition.get("type") in NAMESPACE_CONDITIONS else "OtherOrAbsent",
                    "status": condition["status"] if condition.get("status") in ("True", "False", "Unknown") else "OtherOrAbsent",
                    "reason": condition["reason"] if condition.get("reason") in NAMESPACE_CONDITIONS.get(
                        condition.get("type"), set()) else "OtherOrAbsent",
                } for condition in conditions[:16]],
                "conditionsTruncated": len(conditions) > 16,
                "metricsDiscoveryMentioned": any(c.get("type") == "NamespaceDeletionDiscoveryFailure"
                    and c.get("status") == "True" and isinstance(c.get("message"), str)
                    and "metrics.k8s.io/v1beta1" in c["message"] for c in conditions[:16]),
            })
        return {"available": True, "diagnosticOnly": True, "namespaces": namespaces,
                "metricsApi": metrics_discovery(setup), "truncated": len(selected) > 64}
    except (KeyError, TypeError, AttributeError):
        return {"available": False, "diagnosticOnly": True, "category": "malformed-metadata"}
    except Exception:
        return {"available": False, "diagnosticOnly": True, "category": "api-unavailable"}


def runtime_scope(setup, sandbox):
    try:
        meta = sandbox["metadata"]
        name = "kars-" + meta["name"]
        namespace = setup.admin.optional("/api/v1/namespaces/" + name)
        grant = setup.admin.optional(resource(meta["namespace"], "karscredentialgrants", "workspace"))
        if namespace is None:
            return {"available": True, "namespacePresent": False, "grantPresent": grant is not None}
        annotations = namespace["metadata"].get("annotations") or {}
        state = annotations.get(PREFIX + "state")
        epoch = annotations.get(PREFIX + "epoch")
        grant_meta = grant.get("metadata", {}) if grant else {}
        grant_status = (grant.get("status") or {}) if grant else {}
        activation = (grant["spec"].get("privateActivation") or {}) if grant else {}
        scopes = activation.get("namespaces") or []
        matching = [scope for scope in scopes if scope.get("namespace", {}).get("name") == name
                    and scope["namespace"].get("uid") == namespace["metadata"]["uid"]]
        current = grant_meta.get("generation") is not None and (
            grant_status.get("observedGeneration") == grant_meta["generation"])
        conditions = grant_status.get("conditions") or []
        return {
            "available": True, "namespacePresent": True, "grantPresent": grant is not None,
            "privateState": state if state in ("Pending", "Qualified") else "OtherOrAbsent",
            "namespaceUidMatches": bool(namespace["metadata"].get("uid")) and (
                namespace["metadata"]["uid"] == (meta.get("annotations") or {}).get("kars.azure.com/namespace-uid")),
            "namespaceOwnerMatches": annotations.get("kars.azure.com/sandbox-namespace") == meta["namespace"]
                and annotations.get("kars.azure.com/sandbox-uid") == meta["uid"],
            "grantScopeIncluded": len(matching) == 1,
            "epochMatches": len(matching) == 1 and isinstance(epoch, str) and bool(epoch)
                and matching[0].get("epoch") == epoch,
            "writerReady": current and any(c.get("type") == "WriterReady" and c.get("status") == "True"
                                          for c in conditions),
            "privateConsumptionReady": current and any(
                c.get("type") == "PrivateConsumptionReady" and c.get("status") == "True" for c in conditions),
        }
    except Failure:
        return {"available": False, "category": "api-unavailable"}
    except (KeyError, TypeError, AttributeError):
        return {"available": False, "category": "malformed-metadata"}
