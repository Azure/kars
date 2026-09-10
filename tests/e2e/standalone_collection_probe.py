# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run the approved nine collection cases in an exclusively owned empty namespace."""

import json
import os
from pathlib import Path

from credential_schema import Owned, wait_for
from sre_authority.collection_delete_probe import cases
from sre_authority.registration_schema import kind_proxy, request

POLICIES = ("kars-sre-private-identity", "kars-sre-role-authority", "kars-sre-consumer-authority")


def exercise(port, expected_uid):
    code, cluster = request(port, "GET", "/api/v1/namespaces/kube-system")
    if code != 200 or not expected_uid or cluster.get("metadata", {}).get("uid") != expected_uid:
        raise RuntimeError("Collection proof refuses unverified cluster identity")
    owned, failed = Owned(port), False
    try:
        owned.create("/api/v1/namespaces", {
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "kars-sre"}})
        policies = {}
        for name in POLICIES:
            _, policy = wait_for(
                lambda: request(port, "GET", f"/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/{name}"),
                lambda code, obj: code == 200 and isinstance(obj, dict)
                and obj.get("status", {}).get("observedGeneration") == obj["metadata"].get("generation")
                and isinstance(obj["status"].get("typeChecking"), dict)
                and not obj["status"]["typeChecking"].get("expressionWarnings"),
                "fixtures")
            policies[name] = policy
        results = cases(port, policies)
        return [{key: result[key] for key in ("case", "httpStatus", "expectedStatus", "matched")}
                for result in results]
    except (OSError, RuntimeError, ValueError, TypeError, KeyError, AssertionError):
        failed = True
        raise
    finally:
        try:
            owned.cleanup()
        except (OSError, RuntimeError, ValueError, TypeError, KeyError):
            if not failed:
                raise
            print("COLLECTION-DIAG owned namespace cleanup unavailable", flush=True)


def main(root):
    report = {"complete": False, "cases": []}
    try:
        with kind_proxy(root) as (port, _):
            report["cases"] = exercise(port, os.environ.get("KARS_STANDALONE_CLUSTER_UID", ""))
        report["complete"] = len(report["cases"]) == 9 and all(case["matched"] for case in report["cases"])
    except (OSError, RuntimeError, ValueError, TypeError, KeyError, AssertionError):
        pass
    for case in report["cases"]:
        print("COLLECTION-CASE " + json.dumps(case, sort_keys=True), flush=True)
    path = root / "e2e-diag/standalone/collection-delete-cases.json"
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, indent=2) + "\n")
    except OSError:
        report["complete"] = False
    print("COLLECTION-RESULT " + json.dumps({"complete": report["complete"]}), flush=True)
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main(Path(__file__).resolve().parents[2]))
