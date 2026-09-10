# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real nameless collection DELETE checks without removing any guarded fixture."""

import copy
import time
from urllib.parse import quote

from .bootstrap_cases import as_tenant
from .bootstrap_diagnostics import api_result
from .common import require
from .registration_schema import request

NAMESPACE_CONTROLLER = "system:serviceaccount:kube-system:namespace-controller"


def stable_object(value):
    value = copy.deepcopy(value)
    value.pop("status", None)
    value.get("metadata", {}).pop("resourceVersion", None)
    value.get("metadata", {}).pop("managedFields", None)
    return value


def dry_run_delete(port, collection, item_path, current, admin):
    for attempt in range(3):
        options = {"apiVersion": "v1", "kind": "DeleteOptions", "dryRun": ["All"],
                   "preconditions": {"uid": current["metadata"]["uid"],
                                     "resourceVersion": current["metadata"]["resourceVersion"]}}
        if admin:
            code, body = request(port, "DELETE", collection, options)
        else:
            code, body = as_tenant(port, collection, options,
                                   user=NAMESPACE_CONTROLLER, method="DELETE")
        if (code != 409 or not isinstance(body, dict) or body.get("reason") != "Conflict"
                or attempt == 2):
            return code, body
        status, refreshed = request(port, "GET", item_path)
        require(status == 200 and stable_object(refreshed) == stable_object(current),
                "Collection dry-run target changed beyond controller status; no retry permitted")
        if refreshed["metadata"]["resourceVersion"] == current["metadata"]["resourceVersion"]:
            return code, body
        current = refreshed
        time.sleep(0.2)
    raise AssertionError("Collection dry-run exceeded its fixed attempt bound")


def cases(port, policies):
    fixtures, reports = [], []
    primary_failure = False
    definitions = [
        ("v1", "ServiceAccount", "/api/v1/namespaces/kars-sre/serviceaccounts",
         "sre-api-router", "kars-sre-private-identity", {"automountServiceAccountToken": False}),
        ("rbac.authorization.k8s.io/v1", "Role", "/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/roles",
         "sre-api-self-renew", "kars-sre-role-authority", {"rules": []}),
        ("apps/v1", "Deployment", "/apis/apps/v1/namespaces/kars-sre/deployments",
         "sre", "kars-sre-consumer-authority", {"spec": {
             "replicas": 0, "paused": True, "selector": {"matchLabels": {"app": "e2e-collection"}},
             "template": {"metadata": {"labels": {"app": "e2e-collection"}}, "spec": {
                 "automountServiceAccountToken": False, "schedulerName": "kars-e2e-admission-never-schedule",
                 "containers": [{"name": "probe", "image": "registry.invalid/kars-admission-proof:never",
                                 "imagePullPolicy": "Never"}]}}}}),
    ]
    try:
        for version, kind, path, protected_name, policy, fields in definitions:
            for protected in (False, True):
                name = protected_name if protected else "e2e-collection-" + kind.lower()
                obj = {"apiVersion": version, "kind": kind,
                       "metadata": {"name": name, "namespace": "kars-sre"}, **fields}
                code, created = request(port, "POST", path, obj)
                require(code == 201 and created.get("metadata", {}).get("uid"),
                        "Collection proof fixture CREATE failed; no adoption allowed")
                item_path = path + "/" + name
                fixtures.append((item_path, created))
                collection = path + "?fieldSelector=" + quote("metadata.name=" + name, safe="")
                for admin in (False, True) if protected else (False,):
                    code, current = request(port, "GET", item_path)
                    require(code == 200 and current["metadata"]["uid"] == created["metadata"]["uid"],
                            "Collection proof fixture identity changed")
                    code, body = dry_run_delete(port, collection, item_path, current, admin)
                    expected = 403 if protected and not admin else 200
                    result = api_result(code, body, policies)
                    result.update({
                        "case": f"{kind}-{'protected' if protected else 'ordinary'}-{'admin' if admin else 'namespace-controller'}-collection",
                        "expectedStatus": expected,
                        "matched": code == expected and (expected != 403 or (
                            policy in result.get("policies", [])
                            and not set(result.get("categories", [])) & {"evaluation", "no-such-key", "compilation"})),
                    })
                    reports.append(result)
                    code, after = request(port, "GET", item_path)
                    require(code == 200 and after["metadata"]["uid"] == created["metadata"]["uid"]
                            and not after["metadata"].get("deletionTimestamp"),
                            "Collection DELETE dry-run mutated a fixture")
        return reports
    except BaseException:
        primary_failure = True
        raise
    finally:
        for path, original in reversed(fixtures):
            try:
                code, current = request(port, "GET", path)
                require(code == 200 and current["metadata"]["uid"] == original["metadata"]["uid"],
                        "Collection fixture cleanup identity changed")
                require(stable_object(current) == stable_object(original),
                        "Collection fixture content changed; cleanup preserved it")
                code, _ = request(port, "DELETE", path, {"apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": current["metadata"]["uid"],
                                      "resourceVersion": current["metadata"]["resourceVersion"]}})
                require(code in (200, 202), "Collection fixture cleanup failed")
            except Exception:
                if not primary_failure:
                    raise
                print("SRE-DIAG Owned collection fixture cleanup unavailable", flush=True)
