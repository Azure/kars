# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fixed public admission cases for the image-free schema candidate."""

import copy
import json
import time
from urllib.error import HTTPError
from urllib.request import Request, build_opener, ProxyHandler

from .bootstrap_diagnostics import api_result, object_status
from .bootstrap_probe import upsert
from .registration_schema import request

USER = "system:serviceaccount:e2e-sre-bootstrap:tenant"
DEPLOYMENT_CONTROLLER = "system:serviceaccount:kube-system:deployment-controller"


def as_tenant(port, path, obj, *, user=USER, method="POST", uid=None):
    headers = {"Content-Type": "application/json", "Accept": "application/json",
               "Impersonate-User": user}
    if uid is not None:
        if not isinstance(uid, str) or not uid or len(uid) > 128 or not all(c.isalnum() or c == "-" for c in uid):
            raise RuntimeError("Admission fixture UID is invalid")
        headers["Impersonate-Uid"] = uid
    req = Request(f"http://127.0.0.1:{port}{path}",
                  data=None if method == "GET" else json.dumps(obj).encode(), method=method,
                  headers=headers)
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
                   {"apiGroups": [""], "resources": ["replicationcontrollers"], "verbs": ["create", "update"]},
                   {"apiGroups": ["apps"], "resources": ["deployments", "replicasets"], "verbs": ["create", "update"]},
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
    for kind, plural in (("Deployment", "deployments"), ("ReplicaSet", "replicasets"),
                         ("ReplicationController", "replicationcontrollers")):
        for is_private in (False, True):
            replication_controller = kind == "ReplicationController"
            obj = {"apiVersion": "v1" if replication_controller else "apps/v1", "kind": kind,
                   "metadata": {"name": "e2e-template", "namespace": "kars-sre"},
                   "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "e2e-probe"}},
                            "template": {"metadata": {"labels": {"app": "e2e-probe"}},
                                         "spec": copy.deepcopy((private if is_private else pod)["spec"])}}}
            if replication_controller:
                obj["spec"]["selector"] = {"app": "e2e-probe"}
                obj["spec"]["replicas"] = 0
            cases.append((f"{kind}-{'private' if is_private else 'ordinary'}",
                          f"{'/api/v1' if replication_controller else '/apis/apps/v1'}/namespaces/kars-sre/{plural}", obj,
                          403 if is_private else 201, "kars-sre-private-workloads" if is_private else None))
            if replication_controller and not is_private:
                no_template = copy.deepcopy(obj)
                no_template["spec"].pop("template")
                cases.append(("ReplicationController-no-template",
                              "/api/v1/namespaces/kars-sre/replicationcontrollers", no_template, 422, None))
                private_account = copy.deepcopy(obj)
                private_account["spec"]["template"]["spec"]["serviceAccountName"] = "sre-api-router"
                cases.append(("ReplicationController-private-account",
                              "/api/v1/namespaces/kars-sre/replicationcontrollers",
                              private_account, 403, "kars-sre-private-workloads"))
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
        if name == "ReplicationController-no-template":
            result["nativeTemplateRequired"] = response.get("reason") == "Invalid" and any(
                cause.get("field") == "spec.template" and cause.get("reason") == "FieldValueRequired"
                for cause in response.get("details", {}).get("causes", []))
            result["matched"] = result["matched"] and result["nativeTemplateRequired"]
        reports.append(result)
    return reports


def deployment_controller_cases(port, policies):
    code, account = request(port, "GET", "/api/v1/namespaces/kube-system/serviceaccounts/deployment-controller")
    if code != 200 or not account.get("metadata", {}).get("uid"):
        raise RuntimeError("The actual Kubernetes Deployment controller ServiceAccount is absent")
    authorization = {}
    for label, group, resource, verb, name in (
        ("createReplicaSetsClusterWide", "apps", "replicasets", "create", None),
        ("createPodsClusterWide", "", "pods", "create", None),
        ("useRegistrar", "kars.azure.com", "karssreregistrations", "use", "canonical"),
    ):
        attributes = {"group": group, "resource": resource, "verb": verb}
        if name:
            attributes["name"] = name
        code, body = request(port, "POST", "/apis/authorization.k8s.io/v1/subjectaccessreviews", {
            "apiVersion": "authorization.k8s.io/v1", "kind": "SubjectAccessReview",
            "spec": {"user": DEPLOYMENT_CONTROLLER, "resourceAttributes": attributes},
        })
        if code != 201 or not isinstance(body.get("status", {}).get("allowed"), bool):
            raise RuntimeError("Deployment controller authorization proof failed")
        authorization[label] = body["status"]["allowed"]
    reports = []
    for private in (False, True):
        pod = {"serviceAccountName": "sandbox", "automountServiceAccountToken": False,
               "schedulerName": "kars-e2e-admission-never-schedule",
               "containers": [{"name": "probe", "image": "registry.invalid/kars-admission-proof:never",
                               "imagePullPolicy": "Never"}]}
        if private:
            pod["volumes"] = [{"name": "private", "secret": {"secretName": "sre-api-router-identity"}}]
        obj = {"apiVersion": "apps/v1", "kind": "ReplicaSet",
               "metadata": {"name": "e2e-deployment-controller", "namespace": "kars-sre"},
               "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "e2e-controller"}},
                        "template": {"metadata": {"labels": {"app": "e2e-controller"}}, "spec": pod}}}
        code, body = as_tenant(port, "/apis/apps/v1/namespaces/kars-sre/replicasets?dryRun=All",
                               obj, user=DEPLOYMENT_CONTROLLER)
        result = api_result(code, body, policies)
        result.update({"case": f"deployment-controller-{'private' if private else 'ordinary'}-replicaset",
                       "expectedStatus": 201, "matched": code == 201,
                       "actualControllerAccountExists": True, "authorization": authorization,
                       "identityMode": "admin-impersonation-of-built-in-controller"})
        reports.append(result)
    return reports


def private_controller_chain(port, policies, report):
    namespace, name = "kars-sre", "e2e-private-controller-chain"
    selector = {"app": name}
    deployment = {"apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": name, "namespace": namespace},
        "spec": {"replicas": 1, "selector": {"matchLabels": selector},
                 "template": {"metadata": {"labels": selector}, "spec": {
                     "serviceAccountName": "sandbox", "automountServiceAccountToken": False,
                     "schedulerName": "kars-e2e-admission-never-schedule",
                     "containers": [{"name": "probe", "image": "registry.invalid/kars-admission-proof:never",
                                     "imagePullPolicy": "Never",
                                     "volumeMounts": [{"name": "private", "mountPath": "/private", "readOnly": True}]}],
                     "volumes": [{"name": "private", "secret": {"secretName": "sre-api-router-identity"}}],
                 }}}}
    path = f"/apis/apps/v1/namespaces/{namespace}/deployments"
    code, created = request(port, "POST", path, deployment)
    if code != 201 or not created.get("metadata", {}).get("uid"):
        report({"deploymentCreate": api_result(code, created, policies)})
        raise RuntimeError("Registrar-authorized private Deployment CREATE failed")
    uid = created["metadata"]["uid"]
    deadline = time.monotonic() + 45
    snapshot = {}
    while time.monotonic() < deadline:
        code, current = request(port, "GET", f"{path}/{name}")
        if code != 200 or current.get("metadata", {}).get("uid") != uid:
            raise RuntimeError("Private controller-chain Deployment disappeared or was replaced")
        code, replicasets = request(port, "GET",
            f"/apis/apps/v1/namespaces/{namespace}/replicasets?labelSelector=app%3D{name}")
        if code != 200 or not isinstance(replicasets.get("items"), list):
            raise RuntimeError("Private controller-chain ReplicaSet inspection failed")
        owned = [obj for obj in replicasets["items"] if any(
            owner.get("uid") == uid and owner.get("controller") is True
            for owner in obj.get("metadata", {}).get("ownerReferences", []))]
        owners = {obj["metadata"]["uid"] for obj in owned}
        code, pods = request(port, "GET", f"/api/v1/namespaces/{namespace}/pods?labelSelector=app%3D{name}")
        if code != 200 or not isinstance(pods.get("items"), list):
            raise RuntimeError("Private controller-chain Pod inspection failed")
        children = [obj for obj in pods["items"] if any(
            owner.get("uid") in owners and owner.get("controller") is True
            for owner in obj.get("metadata", {}).get("ownerReferences", []))]
        snapshot = {"deployment": object_status(current, policies),
                    "replicaSets": [object_status(dict(obj, kind="ReplicaSet"), policies) for obj in owned],
                    "pods": [object_status(dict(obj, kind="Pod"), policies) for obj in children],
                    "privateMountPreserved": False, "noWorkloadExecution": False}
        if children:
            snapshot["privateMountPreserved"] = all(
                obj["spec"].get("volumes") and any(
                    volume.get("secret", {}).get("secretName") == "sre-api-router-identity"
                    for volume in obj["spec"]["volumes"])
                and any(mount.get("name") == "private" and mount.get("mountPath") == "/private"
                        and mount.get("readOnly") is True
                        for container in obj["spec"].get("containers", [])
                        for mount in container.get("volumeMounts", []))
                for obj in children)
            snapshot["noWorkloadExecution"] = all(
                obj["spec"].get("schedulerName") == "kars-e2e-admission-never-schedule"
                and not obj["spec"].get("nodeName") and not obj.get("status", {}).get("containerStatuses")
                for obj in children)
            report(snapshot)
            if not snapshot["privateMountPreserved"] or not snapshot["noWorkloadExecution"]:
                raise RuntimeError("Private controller-chain proof changed its protected template or executed a workload")
            return created
        time.sleep(0.5)
    report(snapshot)
    raise RuntimeError("Actual private Deployment/ReplicaSet controllers did not create the admission-only Pod")


def namespace_cleanup_cases(port, policies, owned_consumer=None):
    path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre"
    code, current = request(port, "GET", path)
    if owned_consumer is not None:
        if (code != 200 or current.get("metadata", {}).get("uid") != owned_consumer["metadata"]["uid"]
                or current.get("spec") != owned_consumer.get("spec")):
            raise RuntimeError("Earlier owned cleanup fixture changed; no adoption permitted")
        created = current
    elif code != 404:
        raise RuntimeError("Namespace cleanup proof refuses an existing canonical Deployment")
    obj = {"apiVersion": "apps/v1", "kind": "Deployment",
           "metadata": {"name": "sre", "namespace": "kars-sre"},
           "spec": {"replicas": 0, "selector": {"matchLabels": {"app": "e2e-retired-consumer"}},
                    "template": {"metadata": {"labels": {"app": "e2e-retired-consumer"}}, "spec": {
                        "automountServiceAccountToken": False, "schedulerName": "kars-e2e-admission-never-schedule",
                        "containers": [{"name": "probe", "image": "registry.invalid/kars-admission-proof:never",
                                        "imagePullPolicy": "Never"}]}}}}
    if owned_consumer is None:
        code, created = request(port, "POST", path.rsplit("/", 1)[0], obj)
        if code != 201 or not created.get("metadata", {}).get("uid"):
            raise RuntimeError("Canonical no-execution cleanup fixture CREATE failed")
    uid = created["metadata"]["uid"]
    reports = []
    primary_failure = False
    try:
        for account, namespace, expected in (("namespace-controller", "kube-system", 403),
                                               ("kars-controller", "kars-system", 200)):
            principal = f"system:serviceaccount:{namespace}:{account}"
            code, sa = request(port, "GET", f"/api/v1/namespaces/{namespace}/serviceaccounts/{account}")
            if code != 200 or not sa.get("metadata", {}).get("uid"):
                raise RuntimeError("Cleanup proof principal does not exist")
            code, current = request(port, "GET", path)
            if code != 200 or current.get("metadata", {}).get("uid") != uid:
                raise RuntimeError("Cleanup proof Deployment was replaced before its dry-run")
            code, response = as_tenant(port, path, {
                "apiVersion": "v1", "kind": "DeleteOptions", "dryRun": ["All"],
                "preconditions": {"uid": uid, "resourceVersion": current["metadata"]["resourceVersion"]}},
                user=principal, method="DELETE")
            result = api_result(code, response, policies)
            result.update({"case": f"{account}-canonical-consumer-delete", "expectedStatus": expected,
                           "matched": code == expected and (code != 403
                               or "kars-sre-consumer-authority" in result.get("policies", []))})
            reports.append(result)
        return reports
    except BaseException:
        primary_failure = True
        raise
    finally:
        try:
            code, current = request(port, "GET", path)
            if code != 200 or current.get("metadata", {}).get("uid") != uid:
                raise RuntimeError("Cleanup proof Deployment identity changed")
            code, _ = request(port, "DELETE", path, {"apiVersion": "v1", "kind": "DeleteOptions",
                "preconditions": {"uid": uid, "resourceVersion": current["metadata"]["resourceVersion"]}})
            if code not in (200, 202):
                raise RuntimeError("Cleanup proof could not remove its owned Deployment")
        except Exception:
            if not primary_failure:
                raise
            print("SRE-DIAG owned canonical cleanup fixture removal unavailable", flush=True)
