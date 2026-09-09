# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual UPDATE/owner-reference checks for the narrow ReplicaSet capability."""

import copy

from .bootstrap_cases import DEPLOYMENT_CONTROLLER, USER, as_tenant
from .bootstrap_diagnostics import api_result
from .common import require
from .registration_schema import request


def cases(port, policies, parent):
    namespace = "kars-sre"
    parent_path = f"/apis/apps/v1/namespaces/{namespace}/deployments/{parent['metadata']['name']}"
    code, current = request(port, "GET", parent_path)
    require(code == 200 and current["metadata"]["uid"] == parent["metadata"]["uid"]
            and current["spec"] == parent["spec"], "Approved private parent changed before update proof")
    owner = {"apiVersion": "apps/v1", "kind": "Deployment", "name": parent["metadata"]["name"],
             "uid": parent["metadata"]["uid"], "controller": True}
    private_pod = copy.deepcopy(parent["spec"]["template"]["spec"])
    ordinary_pod = copy.deepcopy(private_pod)
    ordinary_pod.pop("volumes", None)
    for container in ordinary_pod["containers"]:
        container.pop("volumeMounts", None)
    paths = "/apis/apps/v1/namespaces/kars-sre/replicasets"
    fixtures, reports = [], []
    primary_failure = False
    try:
        for suffix, template in (("ordinary", ordinary_pod), ("private", private_pod)):
            name = f"e2e-update-{suffix}"
            selector = {"app": name}
            obj = {"apiVersion": "apps/v1", "kind": "ReplicaSet",
                   "metadata": {"name": name, "namespace": namespace},
                   "spec": {"replicas": 0, "selector": {"matchLabels": selector},
                            "template": {"metadata": {"labels": selector}, "spec": template}}}
            code, created = request(port, "POST", paths, obj)
            require(code == 201 and created.get("metadata", {}).get("uid"),
                    "Owned update fixture CREATE failed")
            fixtures.append((f"{paths}/{name}", created))
        ordinary_path, ordinary = fixtures[0]
        private_path, private = fixtures[1]
        probes = [
            ("tenant-private-rs-forged-parent-create", USER, "POST", paths, private, True, True, 403),
            ("tenant-ordinary-rs-update", USER, "PUT", ordinary_path, ordinary, False, False, 200),
            ("tenant-private-rs-forged-parent-update", USER, "PUT", ordinary_path, ordinary, True, True, 403),
            ("tenant-owned-private-rs-ownerref-update", USER, "PUT", private_path, private, True, True, 403),
            ("deployment-controller-private-rs-update", DEPLOYMENT_CONTROLLER, "PUT", private_path, private, True, True, 200),
            ("tenant-private-parent-update", USER, "PUT", parent_path, parent, True, False, 403),
        ]
        for label, principal, method, path, original, is_private, reference, expected in probes:
            if method == "PUT":
                code, obj = request(port, "GET", path)
                require(code == 200 and obj["metadata"]["uid"] == original["metadata"]["uid"],
                        "Update proof target UID changed")
                obj = copy.deepcopy(obj)
            else:
                obj = copy.deepcopy(original)
                obj["metadata"] = {"name": "e2e-forged-private-create", "namespace": namespace}
                obj.pop("status", None)
            if is_private:
                obj["spec"]["template"]["spec"] = copy.deepcopy(private_pod)
            if reference:
                obj["metadata"]["ownerReferences"] = [owner]
            obj["spec"]["template"].setdefault("metadata", {}).setdefault("annotations", {})["e2e-update-proof"] = "true"
            code, body = as_tenant(port, path + "?dryRun=All", obj, user=principal, method=method)
            result = api_result(code, body, policies)
            result.update({"case": label, "expectedStatus": expected,
                           "matched": code == expected and (expected != 403
                               or "kars-sre-private-workloads" in result.get("policies", []))
                           and (expected == 403 or body.get("kind") == obj["kind"]
                                and body.get("metadata", {}).get("uid") == original["metadata"]["uid"])})
            reports.append(result)
        for path, original in fixtures + [(parent_path, parent)]:
            code, current = request(port, "GET", path)
            require(code == 200 and current["metadata"]["uid"] == original["metadata"]["uid"]
                    and current["spec"] == original["spec"]
                    and current["metadata"].get("ownerReferences") == original["metadata"].get("ownerReferences"),
                    "Owner/update dry-run changed a real workload")
        return reports
    except BaseException:
        primary_failure = True
        raise
    finally:
        for path, original in reversed(fixtures):
            try:
                code, current = request(port, "GET", path)
                require(code == 200 and current["metadata"]["uid"] == original["metadata"]["uid"],
                        "Update fixture cleanup identity changed")
                code, _ = request(port, "DELETE", path, {"apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": current["metadata"]["uid"],
                                      "resourceVersion": current["metadata"]["resourceVersion"]}})
                require(code in (200, 202), "Owned update fixture cleanup failed")
            except Exception:
                if not primary_failure:
                    raise
                print("SRE-DIAG Owned update fixture cleanup unavailable", flush=True)
