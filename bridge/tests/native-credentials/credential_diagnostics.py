"""Secret-free failure categories and private-scope metadata booleans."""

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


def condition_category(message):
    if not isinstance(message, str):
        return "unclassified"
    return CATEGORIES.get(message.removeprefix("CredentialSourceUnavailable: "), "unclassified")


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
