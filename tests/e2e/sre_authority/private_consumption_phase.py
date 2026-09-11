# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real namespace-aware admission, not runtime enrollment or credential proof."""

import copy
import json
from pathlib import Path
import uuid

import credential_schema as shared
from private_consumption import KINDS, POLICY, PREFIX, variants, workload
from .bootstrap_cases import as_tenant
from .registration_schema import request

STAGES = (
    ("deployment-controller", "ReplicaSet", "Deployment"),
    ("cronjob-controller", "Job", "CronJob"),
    ("replicaset-controller", "Pod", "ReplicaSet"),
    ("replication-controller", "Pod", "ReplicationController"),
    ("statefulset-controller", "Pod", "StatefulSet"),
    ("daemon-set-controller", "Pod", "DaemonSet"),
    ("job-controller", "Pod", "Job"),
)
CONNECTIONS = ("exec", "attach", "portforward", "proxy")
ABSENT_POD = "phase-connect-absent"


def require(value):
    if not value:
        raise RuntimeError("Private consumption namespace-phase proof failed")


def template(obj):
    if obj["kind"] == "Pod":
        return obj
    if obj["kind"] == "CronJob":
        return obj["spec"]["jobTemplate"]["spec"]["template"]
    return obj["spec"]["template"]


def shape(kind, namespace, private=False, epoch=None):
    name = "phase-" + kind.lower()
    obj = workload("Deployment" if kind == "Pod" else kind, name, namespace)
    if private:
        obj = variants(obj)[0]
    if kind == "Pod":
        obj = {"apiVersion": "v1", "kind": "Pod", "metadata": obj["metadata"],
               "spec": template(obj)["spec"]}
    template(obj)["spec"]["serviceAccountName"] = "sandbox"
    if epoch:
        template(obj)["metadata"]["annotations"] = {PREFIX + "epoch": epoch}
    return obj


def collection(obj):
    version = obj["apiVersion"]
    prefix = "/api/v1" if version == "v1" else "/apis/" + version
    plural = "pods" if obj["kind"] == "Pod" else next(item[3] for item in KINDS if item[0] == obj["kind"])
    return f"{prefix}/namespaces/{obj['metadata']['namespace']}/{plural}"


def source_policy(objects, name=POLICY):
    bundle = json.loads((Path(__file__).resolve().parents[3] / "deploy/helm/kars/files/private-consumption.json").read_text())
    expected = [obj for obj in bundle["objects"] if obj["metadata"]["name"] == name]
    selected = []
    for canonical in expected:
        values = [obj for obj in objects if obj["kind"] == canonical["kind"] and obj["metadata"]["name"] == name]
        require(len(values) == 1 and values[0]["spec"] == canonical["spec"])
        selected.append(values[0])
    require(len(selected) == 2)
    return selected


def patch_fence(port, namespace, fields):
    path = "/api/v1/namespaces/" + namespace["metadata"]["name"]
    code, current = request(port, "GET", path)
    require(code == 200 and current["metadata"]["uid"] == namespace["metadata"]["uid"])
    code, updated = request(port, "PATCH", path, {"metadata": {
        "uid": current["metadata"]["uid"], "resourceVersion": current["metadata"]["resourceVersion"],
        "annotations": fields}})
    require(code == 200 and updated["metadata"]["uid"] == namespace["metadata"]["uid"])


def cases(port, objects, emit):
    policy, binding = source_policy(objects)
    connect_policy, connect_binding = source_policy(objects, POLICY + "-connect")
    namespace = "kars-cel-" + uuid.uuid4().hex
    token = uuid.uuid4().hex
    epoch = uuid.uuid4().hex + uuid.uuid4().hex
    owned = shared.Owned(port)
    reports = []

    def missing_metadata(code, body, name):
        message = body.get("message", "") if isinstance(body, dict) else ""
        return (code == 422 and body.get("reason") == "Invalid"
                and f"ValidatingAdmissionPolicy '{name}'" in message and "no such key: metadata" in message)

    def record(case, code, expected, matched):
        reports.append({"case": case, "httpStatus": code, "expectedStatus": expected, "matched": matched})
        emit({"cases": reports, "workloadExecution": "not-attempted", "runtimeQualification": "not-claimed"})
        require(matched)

    def probe(case, obj, expected, actor=None, method="POST", fault=None):
        if fault:
            obj = copy.deepcopy(obj)
            obj["metadata"]["name"] += "-missing"
        path = collection(obj) + ("/" + obj["metadata"]["name"] if method == "PUT" else "") + "?dryRun=All"
        code, body = (as_tenant(port, path, obj, user=actor[0], uid=actor[1], method=method)
                      if actor else request(port, method, path, obj))
        if expected == 201:
            matched = shared.allowed(code, body, obj)
        elif expected == 200:
            matched = code == 200 and body.get("kind") == obj["kind"]
        elif fault:
            matched = missing_metadata(code, body, fault)
        else:
            matched = shared.intended_denial(code, body, POLICY, POLICY,
                                            policy["spec"]["validations"][0], obj["metadata"]["name"])
        record(case, code, expected, matched)

    def connect(case, ns_name, subresource, expected, actor=None, fault=None):
        path = f"/api/v1/namespaces/{ns_name}/pods/{ABSENT_POD}"
        require(request(port, "GET", path)[0] == 404)
        # No command, stream, port or target Pod exists. Admission precedes the
        # connector's Pod lookup; a matched 404 is not a working connection.
        code, body = (as_tenant(port, path + "/" + subresource, {}, user=actor[0], uid=actor[1], method="GET")
                      if actor else request(port, "GET", path + "/" + subresource))
        require(request(port, "GET", path)[0] == 404)
        if fault:
            matched = missing_metadata(code, body, fault)
        elif expected == 404:
            matched = (code == 404 and body.get("reason") == "NotFound"
                       and body.get("details", {}).get("name") == ABSENT_POD
                       and body.get("details", {}).get("kind") == "pods")
        else:
            matched = shared.intended_denial(code, body, POLICY + "-connect", POLICY + "-connect",
                                            connect_policy["spec"]["validations"][0], ABSENT_POD)
        record(case, code, expected, matched)

    try:
        ns = owned.create("/api/v1/namespaces", {"apiVersion": "v1", "kind": "Namespace",
                          "metadata": {"name": namespace, "labels": {shared.LABEL: token}}})
        ordinary_namespace = namespace + "-ordinary"
        owned.create("/api/v1/namespaces", {"apiVersion": "v1", "kind": "Namespace",
                     "metadata": {"name": ordinary_namespace, "labels": {shared.LABEL: token}}})
        owned.create(f"/api/v1/namespaces/{ordinary_namespace}/serviceaccounts", {
            "apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": "sandbox", "namespace": ordinary_namespace}})
        accounts = {}
        for name in ("sandbox", "tenant", "root"):
            account = owned.create(f"/api/v1/namespaces/{namespace}/serviceaccounts", {
                "apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": name, "namespace": namespace}})
            accounts[name] = (f"system:serviceaccount:{namespace}:{name}", account["metadata"]["uid"])
        rules = [{"apiGroups": [group], "resources": resources, "verbs": ["create", "update"]}
                 for group, resources in (("", ["pods", "replicationcontrollers"]),
                                          ("apps", [item[3] for item in KINDS if item[1] == "apps"]),
                                          ("batch", ["jobs", "cronjobs"]))]
        rules.append({"apiGroups": [""], "resources": ["pods/" + name for name in CONNECTIONS],
                      "resourceNames": [ABSENT_POD], "verbs": ["get"]})
        for name in ("tenant", "root"):
            role_rules = copy.deepcopy(rules)
            if name == "root":
                role_rules.append({"apiGroups": ["kars.azure.com"], "resources": ["karscredentialgrants"],
                                   "resourceNames": ["workspace"], "verbs": ["project-credentials"]})
            owned.create(f"{shared.RBAC}/namespaces/{namespace}/roles", {
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
                "metadata": {"name": name, "namespace": namespace}, "rules": role_rules})
            owned.create(f"{shared.RBAC}/namespaces/{namespace}/rolebindings", {
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
                "metadata": {"name": name, "namespace": namespace},
                "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": name},
                "subjects": [{"kind": "ServiceAccount", "name": name, "namespace": namespace}]})
        kinds = ["Pod", *(item[0] for item in KINDS)]
        for kind in kinds:
            probe(kind + "-ordinary-unactivated", shape(kind, namespace), 201, accounts["tenant"])
        for subresource in CONNECTIONS:
            connect(subresource + "-unactivated-admission", namespace, subresource, 404, accounts["tenant"])
        controller_uids = {}
        for controller, _, _ in STAGES:
            code, account = request(port, "GET", f"/api/v1/namespaces/kube-system/serviceaccounts/{controller}")
            require(code == 200 and account.get("metadata", {}).get("uid"))
            controller_uids[controller] = account["metadata"]["uid"]
        fields = {PREFIX + "enabled": "true", PREFIX + "state": "Qualified",
                  PREFIX + "namespace-uid": ns["metadata"]["uid"], PREFIX + "epoch": epoch,
                  PREFIX + "root-namespace": namespace, PREFIX + "root-account": "root",
                  PREFIX + "root-user": accounts["root"][0], PREFIX + "root-uid": accounts["root"][1],
                  PREFIX + "profile": "service-accounts"}
        fields.update({PREFIX + name + "-uid": uid for name, uid in controller_uids.items()})
        patch_fence(port, ns, fields)
        for subresource in CONNECTIONS:
            connect(subresource + "-private-tenant", namespace, subresource, 403, accounts["tenant"])
            connect(subresource + "-private-root-admission", namespace, subresource, 404, accounts["root"])
        parents = {}
        for kind in kinds:
            ordinary, private = shape(kind, namespace), shape(kind, namespace, True, epoch)
            probe(kind + "-ordinary-activated", ordinary, 201, accounts["tenant"])
            probe(kind + "-private-tenant", private, 403, accounts["tenant"])
            probe(kind + "-private-root", private, 201, accounts["root"])
            probe(kind + "-wrong-root-uid", private, 403, (accounts["root"][0], "wrong-uid"))
            if kind != "Pod":
                if kind == "DaemonSet":
                    selector = template(private)["spec"]["nodeSelector"]["private-consumption.test/never-schedule"]
                    code, nodes = request(port, "GET", "/api/v1/nodes?labelSelector=private-consumption.test%2Fnever-schedule%3D" + selector)
                    require(code == 200 and not nodes.get("items"))
                parents[kind] = owned.create(collection(private), private)
        patch_fence(port, ns, {PREFIX + "parent-" + parent["metadata"]["uid"]: epoch for parent in parents.values()})
        for controller, kind, owner_kind in STAGES:
            obj = shape(kind, namespace, True, epoch)
            owner = parents[owner_kind]
            obj["metadata"]["ownerReferences"] = [{
                "apiVersion": owner["apiVersion"], "kind": owner_kind,
                "name": owner["metadata"]["name"], "uid": owner["metadata"]["uid"], "controller": True}]
            # Distinct dry-run name avoids AlreadyExists on the inert parent.
            obj["metadata"]["name"] += "-child"
            actor = (f"system:serviceaccount:kube-system:{controller}", controller_uids[controller])
            probe(controller + "-private-child", obj, 201, actor)
            probe(controller + "-wrong-uid", obj, 403, (actor[0], "wrong-uid"))
        path = collection(parents["Deployment"]) + "/" + parents["Deployment"]["metadata"]["name"]
        code, current = request(port, "GET", path)
        require(code == 200 and current["metadata"]["uid"] == parents["Deployment"]["metadata"]["uid"])
        removed = copy.deepcopy(current)
        template(removed)["spec"].pop("volumes")
        template(removed)["metadata"].get("annotations", {}).pop(PREFIX + "epoch", None)
        probe("old-private-reference-update", removed, 403, accounts["tenant"], method="PUT")
        probe("authorized-private-update", current, 200, accounts["root"], method="PUT")

        # A scoped copy supplies an unavailable namespace input only for this
        # negative proof. The shipped policy and every authority clause remain intact.
        for label, original_policy, original_binding in (("workloads", policy, binding),
                                                         ("connections", connect_policy, connect_binding)):
            fault, fault_binding = shared.scoped(original_policy, original_binding, token, namespace, "missing-" + label)
            original = next(value for value in fault["spec"]["variables"] if value["name"] == "a")
            original["expression"] = "dyn({}).metadata.?annotations.orValue({})"
            created = owned.create(shared.ADMISSION + "/validatingadmissionpolicies", fault)
            shared.wait_for(lambda: request(port, "GET", shared.ADMISSION + "/validatingadmissionpolicies/" + created["metadata"]["name"]),
                            lambda code, obj: code == 200 and obj.get("status", {}).get("observedGeneration") == obj["metadata"]["generation"]
                            and "typeChecking" in obj.get("status", {})
                            and not obj["status"]["typeChecking"].get("expressionWarnings"), "fixtures")
            owned.create(shared.ADMISSION + "/validatingadmissionpolicybindings", fault_binding)
            warmup = shape("Pod", ordinary_namespace)
            if label == "workloads":
                pending = lambda: request(port, "POST", collection(warmup) + "?dryRun=All", warmup)
            else:
                pending = lambda: request(port, "GET", f"/api/v1/namespaces/{ordinary_namespace}/pods/{ABSENT_POD}/proxy")
            shared.wait_for(pending, lambda code, body: missing_metadata(code, body, fault["metadata"]["name"]), "fixtures")
            for active, ns_name in (("active", namespace), ("inactive", ordinary_namespace)):
                if label == "workloads":
                    for kind in kinds:
                        probe(active + "-" + kind + "-missing-metadata-nonconsumer", shape(kind, ns_name), 422,
                              fault=fault["metadata"]["name"])
                        probe(active + "-" + kind + "-missing-metadata-operator", shape(kind, ns_name, True, epoch), 422,
                              fault=fault["metadata"]["name"])
                else:
                    for subresource in CONNECTIONS:
                        connect(active + "-" + subresource + "-missing-metadata", ns_name, subresource, 422,
                                fault=fault["metadata"]["name"])
    finally:
        owned.cleanup()
    return reports
