# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fixed public admission cases for the image-free schema candidate."""

import copy
import json
from urllib.error import HTTPError
from urllib.request import Request, build_opener, ProxyHandler

from .bootstrap_diagnostics import api_result
from .bootstrap_probe import upsert
from .registration_schema import request

USER = "system:serviceaccount:e2e-sre-bootstrap:tenant"


def as_tenant(port, path, obj):
    req = Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(obj).encode(), method="POST",
                  headers={"Content-Type": "application/json", "Accept": "application/json",
                           "Impersonate-User": USER})
    try:
        response = build_opener(ProxyHandler({})).open(req, timeout=15)
    except HTTPError as error:
        response = error
    with response:
        return response.code, json.loads(response.read(1024 * 1024))


def admission_cases(port, policies):
    code, _ = request(port, "POST", "/api/v1/namespaces", {
        "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "e2e-sre-bootstrap"}})
    if code not in (201, 409):
        raise RuntimeError("Disposable admission principal namespace unavailable")
    setup = [
        {"apiVersion": "v1", "kind": "ServiceAccount",
         "metadata": {"name": "tenant", "namespace": "e2e-sre-bootstrap"}},
        {"apiVersion": "v1", "kind": "ServiceAccount",
         "metadata": {"name": "sandbox", "namespace": "kars-sre"}},
        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
         "metadata": {"name": "e2e-bootstrap-probe", "namespace": "kars-sre"},
         "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["create"]},
                   {"apiGroups": ["apps"], "resources": ["deployments", "replicasets"], "verbs": ["create"]},
                   {"apiGroups": ["kars.azure.com"], "resources": ["karssreactions"], "verbs": ["create"]}]},
        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
         "metadata": {"name": "e2e-bootstrap-probe", "namespace": "kars-sre"},
         "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "e2e-bootstrap-probe"},
         "subjects": [{"kind": "ServiceAccount", "name": "tenant", "namespace": "e2e-sre-bootstrap"}]},
    ]
    for obj in setup:
        if not upsert(port, obj, policies).get("accepted"):
            raise RuntimeError("Disposable namespaced admission principal setup failed")
    pod = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "e2e-ordinary", "namespace": "kars-sre"},
           "spec": {"serviceAccountName": "sandbox", "automountServiceAccountToken": False,
                    "schedulerName": "kars-e2e-admission-never-schedule",
                    "containers": [{"name": "probe", "image": "registry.invalid/kars-admission-proof:never",
                                    "imagePullPolicy": "Never"}]}}
    cases = [("ordinary-tenant-pod", "/api/v1/namespaces/kars-sre/pods", pod, 201, None)]
    private = copy.deepcopy(pod)
    private["spec"]["volumes"] = [{"name": "private", "secret": {"secretName": "sre-api-router-identity"}}]
    cases.append(("private-volume", "/api/v1/namespaces/kars-sre/pods", private, 403, "kars-sre-private-mounts"))
    env = copy.deepcopy(pod)
    env["spec"]["containers"][0]["envFrom"] = [{"secretRef": {"name": "sre-api-router-identity"}}]
    cases.append(("private-env", "/api/v1/namespaces/kars-sre/pods", env, 403, "kars-sre-private-mounts"))
    for kind, plural in (("Deployment", "deployments"), ("ReplicaSet", "replicasets")):
        for is_private in (False, True):
            obj = {"apiVersion": "apps/v1", "kind": kind,
                   "metadata": {"name": "e2e-template", "namespace": "kars-sre"},
                   "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "e2e-probe"}},
                            "template": {"metadata": {"labels": {"app": "e2e-probe"}},
                                         "spec": copy.deepcopy((private if is_private else pod)["spec"])}}}
            cases.append((f"{kind}-{'private' if is_private else 'ordinary'}",
                          f"/apis/apps/v1/namespaces/kars-sre/{plural}", obj,
                          403 if is_private else 201, "kars-sre-private-workloads" if is_private else None))
    params = {"namespace": "example", "name": "demo", "replicas": 1,
              "nested": {"array": [True, None, 1, "text"], "object": {"key": "value"}}}
    action = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSREAction",
              "metadata": {"name": "e2e-proposal", "namespace": "kars-sre"},
              "spec": {"action": {"type": "ScaleDeployment", "params": params},
                       "approval": {"state": "Pending"}, "ttlMinutes": 5}}
    cases.append(("pending-json-params-preserved", "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions",
                  action, 201, None))
    approved = copy.deepcopy(action)
    approved["spec"]["approval"]["state"] = "Approved"
    cases.append(("preapproved-action-denied", "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions",
                  approved, 403, "kars-sre-pending-proposals"))
    reports = []
    for name, path, obj, expected, policy in cases:
        code, response = as_tenant(port, path + "?dryRun=All", obj)
        result = api_result(code, response, policies)
        result.update({"case": name, "expectedStatus": expected, "matched": code == expected
                       and (policy is None or policy in result.get("policies", []))})
        if name == "pending-json-params-preserved":
            result["paramsPreserved"] = response.get("spec", {}).get("action", {}).get("params") == params
            result["matched"] = result["matched"] and result["paramsPreserved"]
        reports.append(result)
    return reports
