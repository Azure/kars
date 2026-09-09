# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Same-Kind API proof of historical CRD field ownership; no workload execution."""

import copy
from pathlib import Path
import sys
import time

from sre_authority.bootstrap_probe import converted_objects
from sre_authority.common import require
from sre_authority.fixtures import LEGACY_COMMIT
from sre_authority.legacy_crds import validate_historical_crd
from sre_authority.registration_schema import (
    CONTEXT, CRD_PATH, command, kind_proxy, request, write_report,
)

NAME = "karssreactions.kars.azure.com"
PATH = f"{CRD_PATH}/{NAME}"
IDENTITY = ("karssreactions", "KarsSREAction", "Namespaced")


def conflict_report(code, body):
    return {"httpStatus": code, "fieldManagerConflict": bool(
        code == 409 and isinstance(body, dict) and body.get("kind") == "Status"
        and body.get("reason") == "Conflict"
        and any(cause.get("reason") == "FieldManagerConflict"
                for cause in body.get("details", {}).get("causes", [])))}


def cleanup(port, uid):
    code, current = request(port, "GET", PATH)
    require(code == 200 and current.get("metadata", {}).get("uid") == uid,
            "Historical CRD ownership fixture changed before cleanup")
    code, _ = request(port, "DELETE", PATH, {
        "apiVersion": "v1", "kind": "DeleteOptions",
        "preconditions": {"uid": uid, "resourceVersion": current["metadata"]["resourceVersion"]},
    })
    require(code in (200, 202), "Historical CRD ownership cleanup failed")
    end = time.monotonic() + 30
    while time.monotonic() < end:
        code, _ = request(port, "GET", PATH)
        if code == 404:
            return
        require(code == 200, "Historical CRD cleanup inspection failed")
        time.sleep(0.2)
    raise AssertionError("Historical CRD ownership cleanup timed out")


def exercise(root):
    command("legacy-fetch", ["git", "fetch", "--no-tags", "--depth=1",
            "https://github.com/Azure/kars.git", LEGACY_COMMIT], root=root)
    historical = command("legacy-source", ["git", "show",
        f"{LEGACY_COMMIT}:deploy/helm/kars/templates/crd-karssreaction.yaml"], root=root)
    current = command("current-render", ["helm", "template", "kars", str(root / "deploy/helm/kars"),
        "--namespace", "kars-system", "--show-only", "templates/crd-karssreaction.yaml"], root=root)
    objects = []
    for rendered in (historical, current):
        parsed = converted_objects(command("legacy-conversion", [
            "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
            "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
        ], root=root, data=rendered))
        require(len(parsed) == 1, "Historical field ownership proof requires one public CRD")
        validate_historical_crd(parsed[0], IDENTITY)
        objects.append(parsed[0])
    old, new = objects
    with kind_proxy(root) as (port, version):
        reports = []
        for manager, expected in (("kubectl-create", 409), ("helm", 200)):
            code, _ = request(port, "GET", PATH)
            require(code == 404, "Historical field ownership proof refuses an existing CRD")
            desired = copy.deepcopy(old)
            desired["metadata"]["labels"]["app.kubernetes.io/managed-by"] = "Helm"
            desired["metadata"]["annotations"] = {
                "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system"}
            code, created = request(port, "POST", CRD_PATH + f"?fieldManager={manager}", desired)
            require(code == 201 and created.get("metadata", {}).get("uid"),
                    "Historical ownership fixture CREATE failed")
            uid = created["metadata"]["uid"]
            try:
                code, _ = request(port, "PATCH", PATH + "?fieldManager=helm&force=false", desired,
                                  content_type="application/apply-patch+yaml")
                require(code == 200, "Unchanged historical Helm apply failed")
                updated = copy.deepcopy(new)
                updated["metadata"] = desired["metadata"]
                code, body = request(port, "PATCH", PATH + "?fieldManager=helm&force=false&dryRun=All", updated,
                                     content_type="application/apply-patch+yaml")
                report = {"bootstrapManager": manager, "expectedStatus": expected, **conflict_report(code, body)}
                reports.append(report)
                write_report(root, "legacy-crd-ownership.json", {
                    "apiServer": version, "legacyCommit": LEGACY_COMMIT, "resource": NAME, "cases": reports})
                require(code == expected and (code != 409 or report["fieldManagerConflict"]),
                        "Historical Helm field ownership did not match the expected API result")
            finally:
                failed = sys.exc_info()[0] is not None
                try:
                    cleanup(port, uid)
                except Exception:
                    if not failed:
                        raise
                    print("SRE-CRD-OWNERSHIP-CLEANUP-FAIL", flush=True)


if __name__ == "__main__":
    try:
        exercise(Path(__file__).resolve().parents[3])
    except Exception as error:
        print(f"SRE-CRD-OWNERSHIP-FAIL category={type(error).__name__}", flush=True)
        raise SystemExit(1) from None
