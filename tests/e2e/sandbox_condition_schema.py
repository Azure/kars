# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Hosted, nonexecuting proof of Sandbox Condition schema pruning/retention.

Own the CRD exclusively in the early disposable API lane. Upgrade the exact
pre-fix schema on that same UID; never touch an existing CRD or customer object.
SchemaProbe=Unknown is deliberately not readiness or controller authority.
"""

import copy
import hashlib
import json
import os
from pathlib import Path
import time
import uuid

from credential_schema import decode_documents
from sre_authority.registration_schema import (
    CONTEXT, CRD_PATH, command, crd_established, kind_proxy, request,
)

NAME = "karssandboxes.kars.azure.com"
GROUP = "/apis/kars.azure.com/v1alpha1"
LABEL = "kars.azure.com/condition-schema-proof"
OLD_DIGEST = "da674a84c19c8ac64a1d96d04f79435c6899601426e25931feaca483f139b920"
NEW_DIGEST = "5d495b8cfe5e4526741a673161cbae0492812e2650c2f2d08c5e522a3bda946f"
FIELD = "status.conditions[0].observedGeneration"
CASES = {"render", "create", "established", "old-pruned", "upgrade", "new-retained",
         "new-type-denied", "new-optional", "cleanup", "complete"}


class Failure(RuntimeError):
    def __init__(self, case, code=0):
        self.case = case if case in CASES else "complete"
        self.code = code if type(code) is int and 100 <= code <= 599 else 0
        super().__init__("Sandbox condition schema proof failed")


def require(value, case, code=0):
    if not value:
        raise Failure(case, code)


def condition_schema(crd):
    return crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"][
        "properties"]["status"]["properties"]["conditions"]["items"]


def spec_digest(crd):
    # Match CLI normalizedCrd: API-assigned conventional defaults are not drift.
    spec = copy.deepcopy(crd["spec"])
    for version in spec["versions"]:
        if version.get("deprecated") is False:
            del version["deprecated"]
        for column in version.get("additionalPrinterColumns", []):
            if column.get("priority") == 0:
                del column["priority"]
    names = spec["names"]
    for key in ("categories", "shortNames"):
        if names.get(key) == []:
            del names[key]
    if names.get("listKind") == names["kind"] + "List":
        del names["listKind"]
    if names.get("singular") == names["kind"].lower():
        del names["singular"]
    if spec.get("conversion") == {"strategy": "None"}:
        del spec["conversion"]
    if spec.get("preserveUnknownFields") is False:
        del spec["preserveUnknownFields"]
    raw = json.dumps(spec, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(raw.encode()).hexdigest()


def exact_schemas(current):
    require(current.get("metadata", {}).get("name") == NAME
            and spec_digest(current) == NEW_DIGEST, "render")
    item = condition_schema(current)
    require(item["properties"]["observedGeneration"] == {"type": "integer", "format": "int64"}
            and "observedGeneration" not in item.get("required", [])
            and "x-kubernetes-preserve-unknown-fields" not in item, "render")
    old = copy.deepcopy(current)
    del condition_schema(old)["properties"]["observedGeneration"]
    require(spec_digest(old) == OLD_DIGEST, "render")
    return old, copy.deepcopy(current)


def fixture(namespace, token):
    return {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": "schema-probe", "namespace": namespace, "labels": {LABEL: token}},
        "spec": {"runtime": {"kind": "OpenClaw", "openclaw": {}},
                 "sandbox": {"isolation": "enhanced"},
                 "inferenceRef": {"name": "nonexecuting-schema-probe"}, "suspended": True},
    }


def probe_status(generation, *, include_generation=True):
    condition = {"type": "SchemaProbe", "status": "Unknown", "reason": "SchemaRetentionProbe",
                 "message": "Non-authorizing schema fixture",
                 "lastTransitionTime": "2026-01-01T00:00:00Z"}
    if include_generation:
        condition["observedGeneration"] = generation
    return {"observedGeneration": 1, "conditions": [condition]}


def intended_type_denial(code, body, name):
    if (code != 422 or not isinstance(body, dict) or body.get("kind") != "Status"
            or body.get("status") != "Failure" or body.get("reason") != "Invalid"):
        return False
    details = body.get("details", {})
    return (isinstance(details, dict) and details.get("name") == name
            and details.get("group") == "kars.azure.com"
            and isinstance(details.get("causes"), list)
            and any(isinstance(cause, dict) and cause.get("field") == FIELD
                    and cause.get("reason") in ("FieldValueInvalid", "FieldValueTypeInvalid")
                    for cause in details["causes"]))


def wait_for(probe, case, seconds=40):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if probe():
            return
        time.sleep(0.25)
    raise Failure(case)


class Owned:
    def __init__(self, port, token):
        self.port, self.token, self.resources = port, token, []

    def read(self, path, uid, case):
        code, body = request(self.port, "GET", path)
        require(code == 200 and isinstance(body, dict), case, code)
        meta = body.get("metadata", {})
        require(meta.get("uid") == uid and meta.get("resourceVersion")
                and meta.get("labels", {}).get(LABEL) == self.token
                and not meta.get("deletionTimestamp"), case, code)
        return body

    def create(self, path, body):
        body = copy.deepcopy(body)
        body["metadata"].setdefault("labels", {})[LABEL] = self.token
        code, created = request(self.port, "POST", path, body)
        require(code == 201 and isinstance(created, dict), "create", code)
        metadata = created.get("metadata", {})
        require(metadata.get("name") == body["metadata"]["name"]
                and metadata.get("namespace") == body["metadata"].get("namespace")
                and metadata.get("uid") and metadata.get("resourceVersion")
                and metadata.get("labels", {}).get(LABEL) == self.token, "create", code)
        owned_path = path + "/" + metadata["name"]
        self.resources.append((owned_path, metadata["uid"]))
        return self.read(owned_path, metadata["uid"], "create")

    def cleanup(self):
        failures = []
        for path, uid in reversed(self.resources):
            try:
                current = self.read(path, uid, "cleanup")
                code, _ = request(self.port, "DELETE", path, {
                    "apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": uid,
                                      "resourceVersion": current["metadata"]["resourceVersion"]},
                    "propagationPolicy": "Background",
                })
                require(code in (200, 202), "cleanup", code)
                wait_for(lambda: request(self.port, "GET", path)[0] == 404, "cleanup")
            except (Failure, OSError):
                failures.append(path)
        require(not failures, "cleanup")


def patch_status(owned, path, original, status, *, query="", case):
    current = owned.read(path, original["metadata"]["uid"], case)
    require(current["metadata"]["generation"] == original["metadata"]["generation"]
            and current["spec"] == original["spec"], case)
    return request(owned.port, "PATCH", path + "/status" + query, {
        "metadata": {"uid": current["metadata"]["uid"],
                     "resourceVersion": current["metadata"]["resourceVersion"]},
        "status": status,
    })


def assert_roundtrip(owned, path, original, response, status, case):
    current = owned.read(path, original["metadata"]["uid"], case)
    require(current["metadata"]["generation"] == original["metadata"]["generation"]
            and current["spec"] == original["spec"]
            and current.get("status") == status
            and current["metadata"]["resourceVersion"] == response["metadata"]["resourceVersion"],
            case)
    return current


def exercise(port, current, evidence):
    old, new = exact_schemas(current)
    token = uuid.uuid4().hex
    namespace = "kars-condition-schema-" + token
    owned = Owned(port, token)
    try:
        crd = owned.create(CRD_PATH, old)
        crd_path = CRD_PATH + "/" + NAME
        wait_for(lambda: crd_established(*request(port, "GET", crd_path)), "established")
        owned.create("/api/v1/namespaces", {
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace},
        })
        collection = GROUP + "/namespaces/" + namespace + "/karssandboxes"
        original = owned.create(collection, fixture(namespace, token))
        path = collection + "/" + original["metadata"]["name"]
        require(original["metadata"]["generation"] == 1 and original["spec"]["suspended"] is True,
                "create")
        status = probe_status(original["metadata"]["generation"])
        code, result = patch_status(owned, path, original, status, case="old-pruned")
        require(code == 200, "old-pruned", code)
        assert_roundtrip(owned, path, original, result,
                         probe_status(1, include_generation=False), "old-pruned")
        evidence["markers"].append("old-exact-schema-prunes-condition-generation")

        latest = owned.read(crd_path, crd["metadata"]["uid"], "upgrade")
        require(spec_digest(latest) == OLD_DIGEST, "upgrade")
        latest["spec"] = new["spec"]
        code, _ = request(port, "PUT", crd_path, latest)
        require(code == 200, "upgrade", code)
        require(spec_digest(owned.read(crd_path, crd["metadata"]["uid"], "upgrade")) == NEW_DIGEST,
                "upgrade")

        def serving_new_schema():
            code, body = patch_status(owned, path, original, status,
                                      query="?dryRun=All", case="upgrade")
            return code == 200 and body.get("status") == status
        wait_for(serving_new_schema, "upgrade")
        code, result = patch_status(owned, path, original, status, case="new-retained")
        require(code == 200, "new-retained", code)
        stable = assert_roundtrip(owned, path, original, result, status, "new-retained")
        evidence["markers"].append("new-schema-retains-integer-after-status-write-and-get")
        code, body = patch_status(owned, path, original, probe_status("not-an-integer"),
                                  case="new-type-denied")
        require(intended_type_denial(code, body, original["metadata"]["name"]),
                "new-type-denied", code)
        require(owned.read(path, original["metadata"]["uid"], "new-type-denied") == stable,
                "new-type-denied")
        evidence["markers"].append("new-schema-rejects-wrong-type-at-condition-field")
        optional = probe_status(1, include_generation=False)
        code, result = patch_status(owned, path, original, optional, case="new-optional")
        require(code == 200, "new-optional", code)
        assert_roundtrip(owned, path, original, result, optional, "new-optional")
        evidence["markers"].append("condition-generation-remains-optional-without-default")
    finally:
        owned.cleanup()
        evidence["markers"].append("owned-fixtures-uid-resource-version-cleanup")


def main():
    root = Path(__file__).resolve().parents[2]
    evidence = {"markers": [], "oldDigest": OLD_DIGEST, "newDigest": NEW_DIGEST,
                "controllerReadinessQualified": False, "result": "failed"}
    try:
        require(os.environ.get("GITHUB_ACTIONS") == "true", "complete")
        with kind_proxy(root) as (port, version):
            evidence["apiVersion"] = version
            yaml = command("sandbox-render", [
                "helm", "template", "condition-schema-proof", str(root / "deploy/helm/kars"),
                "--namespace", "kars-system", "--show-only", "templates/crd.yaml",
            ], root=root)
            raw = command("sandbox-convert", [
                "kubectl", "--context", CONTEXT, "create", "--dry-run=client",
                "--validate=false", "-f", "-", "-o", "json",
            ], root=root, data=yaml)
            current = decode_documents(raw).get(("CustomResourceDefinition", NAME), {})
            exercise(port, current, evidence)
        evidence["result"] = "passed"
    except Failure as error:
        evidence["failure"] = {"case": error.case, "httpStatus": error.code}
    except Exception:
        evidence["failure"] = {"case": "complete", "httpStatus": 0}
    directory = root / "e2e-sre-schema-diag"
    directory.mkdir(exist_ok=True)
    (directory / "sandbox-condition-generation.json").write_text(
        json.dumps(evidence, indent=2) + "\n")
    print("SANDBOX-CONDITION-SCHEMA " + json.dumps(evidence, sort_keys=True), flush=True)
    return 0 if evidence["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
