"""Explicit operator metadata review/resubmission, not transport or status retries."""

import copy
import json

from native_api import require

REVIEW = "/api/operator/credentials/review"
WRITE = "/api/operator/credentials"


def _review(value, request):
    require(isinstance(value, dict) and isinstance(value.get("token"), str),
            "Credential review token missing")
    metadata = value.get("metadata", {})
    require(isinstance(metadata, dict), "Credential review metadata missing")
    target, grant, source = (metadata.get(name, {}) for name in ("target", "grant", "source"))
    require(all(isinstance(item, dict) for item in (target, grant, source)),
            "Credential review identities malformed")
    require(target.get("kind") == request["kind"] and target.get("namespace") == request["namespace"]
            and target.get("name") == request["target"] and target.get("uid") == request["targetUid"]
            and metadata.get("key") == request["key"], "Credential review target/key differs")
    require(type(target.get("generation")) is int and target["generation"] > 0
            and isinstance(target.get("version"), str) and isinstance(target.get("intent"), str)
            and isinstance(grant.get("uid"), str) and type(grant.get("generation")) is int
            and isinstance(grant.get("intent"), str) and isinstance(grant.get("workspaceUid"), str)
            and isinstance(grant.get("legacyInventory"), str), "Credential review authority missing")
    require(isinstance(source.get("name"), str) and isinstance(source.get("keys"), list)
            and type(value.get("expiresAt")) is int and type(value.get("submission")) is int
            and value["submission"] in (1, 2, 3)
            and isinstance(value.get("continuation"), bool) and isinstance(value.get("bindingOnly"), bool),
            "Credential review metadata malformed")
    return copy.deepcopy(value)


def _same_review(previous, current, receipt):
    old, new = previous["metadata"], current["metadata"]
    for kind in ("target", "grant"):
        left, right = copy.deepcopy(old[kind]), copy.deepcopy(new[kind])
        left.pop("version", None)
        right.pop("version", None)
        require(left == right, "Credential target or grant authority changed during re-review")
    require(current["expiresAt"] == previous["expiresAt"]
            and current["submission"] == previous["submission"] + 1
            and current["continuation"] is True, "Credential continuation bound changed")
    require(old["key"] == new["key"], "Credential key changed during review")
    source = receipt.get("source")
    if receipt.get("outcome") == "no-write-attempted":
        require(source is None and not current["bindingOnly"] and old["source"] == new["source"],
                "Unwritten source changed; no adoption is permitted")
    else:
        require(receipt.get("outcome") == "source-stored" and isinstance(source, dict)
                and current["bindingOnly"], "No acknowledged partial write authorizes continuation")
        require(all(new["source"].get(key) == source.get(key)
                    for key in ("name", "uid", "version", "metadataDigest"))
                and isinstance(source.get("uid"), str) and isinstance(source.get("version"), str),
                "Acknowledged partial source identity/version changed")
        require(sorted(new["source"]["keys"]) == sorted(set(old["source"]["keys"] + [old["key"]])),
                "Acknowledged source key scope changed")


def reviewed_credential_write(bff, request, expected_grant, expected_generation, report=None):
    """At most three separately reviewed submissions; no credential readback."""
    if report is None:
        report = lambda fact: print("CREDENTIAL-REVIEW " + json.dumps(fact, sort_keys=True))
    metadata_input = {key: request[key] for key in ("namespace", "kind", "target", "targetUid", "key")}
    reviewed = _review(bff.call("POST", REVIEW, metadata_input), metadata_input)
    require(reviewed["submission"] == 1 and not reviewed["continuation"] and not reviewed["bindingOnly"]
            and reviewed["metadata"]["target"]["generation"] == expected_generation
            and reviewed["metadata"]["grant"]["uid"] == expected_grant["metadata"]["uid"]
            and reviewed["metadata"]["grant"]["generation"] == expected_grant["metadata"]["generation"],
            "Initial operator review differs from the created target or enrolled grant")
    for attempt in range(1, 4):
        report({"stage": "operator-metadata-reviewed", "submission": attempt,
                "sameAuthority": True, "bindingOnly": reviewed["bindingOnly"]})
        code, result = bff.call("POST", WRITE, {**request, "review": reviewed["token"]},
                                expected=(200, 409), include_status=True)
        require(isinstance(result, dict), "Credential result malformed")
        report({"stage": "reviewed-credential-submission", "submission": attempt, "httpStatus": code})
        if code == 200:
            require(result.get("stored") is True
                    and result.get("source", {}).get("name") == reviewed["metadata"]["source"]["name"],
                    "Reviewed credential write was not confirmed")
            if reviewed["metadata"]["source"].get("uid") is not None:
                require(result["source"].get("uid") == reviewed["metadata"]["source"]["uid"],
                        "Completion changed the reviewed source UID")
            return result
        require(code == 409 and result.get("error", {}).get("code") == "conflict",
                "Only a typed conflict may request explicit re-review")
        receipt = result["error"].get("credentialContinuation")
        require(attempt < 3 and isinstance(receipt, dict) and isinstance(receipt.get("token"), str),
                "No server-owned continuation proof or submission budget remains")
        refresh_code, refreshed_result = bff.call("POST", REVIEW, {
            **metadata_input, "continuation": receipt["token"],
        }, expected=(200, 409), include_status=True)
        if refresh_code == 409:
            # A fresh metadata-only view diagnoses the refusal; its ticket is
            # never submitted and cannot rebase this already-reviewed write.
            current_code, current = bff.call("POST", REVIEW, metadata_input,
                                             expected=(200, 409), include_status=True)
            facts = {"stage": "operator-re-review-rejected", "httpStatus": refresh_code,
                     "currentMetadataAvailable": current_code == 200, "writeResubmitted": False}
            if current_code == 200:
                current = _review(current, metadata_input)["metadata"]
                previous = copy.deepcopy(reviewed["metadata"])
                if receipt.get("outcome") == "source-stored" and isinstance(receipt.get("source"), dict):
                    previous["source"] = {
                        **receipt["source"],
                        "keys": sorted(set(previous["source"]["keys"] + [previous["key"]])),
                    }
                facts["unchanged"] = {
                    area: {field: previous[area].get(field) == current[area].get(field)
                           for field in fields}
                    for area, fields in (
                        ("target", ("uid", "generation", "version", "intent")),
                        ("grant", ("uid", "generation", "version", "intent", "workspaceUid", "legacyInventory")),
                        ("source", ("uid", "version", "metadataDigest", "keys")),
                    )
                }
            report(facts)
            require(False, "Credential re-review was rejected; no authority or source was rebased")
        require(refresh_code == 200, "Credential re-review response was not accepted")
        refreshed = _review(refreshed_result, metadata_input)
        _same_review(reviewed, refreshed, receipt)
        report({"stage": "operator-metadata-refreshed", "submission": refreshed["submission"],
                "sameAuthority": True, "acknowledgedSource": receipt.get("source") is not None})
        reviewed = refreshed
    raise AssertionError("Credential submission bound exhausted")
