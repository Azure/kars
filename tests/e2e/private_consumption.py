# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Native named-RBAC qualification after reviewed generic private activation.

Private execution attempts are server-side dry-runs. The old-reference Job
fixture has zero parallelism and is suspended; no fixture can schedule an
executable workload.
"""

import copy
import re
import uuid

from sre_authority.common import TENANT, require

POLICY = "kars-private-consumption"
PREFIX = "kars.azure.com/private-"
KINDS = (
    ("Deployment", "apps", "v1", "deployments"),
    ("ReplicaSet", "apps", "v1", "replicasets"),
    ("StatefulSet", "apps", "v1", "statefulsets"),
    ("DaemonSet", "apps", "v1", "daemonsets"),
    ("ReplicationController", "", "v1", "replicationcontrollers"),
    ("Job", "batch", "v1", "jobs"),
    ("CronJob", "batch", "v1", "cronjobs"),
)
PRIVATE = ("router-services-admin", "router-services-observer", "router-services-observer-identity",
           "router-github-app", "kars-observation-privacy-tls", "sre-api-router-identity")


def path(namespace, plural, group="", name=None):
    prefix = f"/apis/{group}/v1" if group else "/api/v1"
    return f"{prefix}/namespaces/{namespace}/{plural}" + (f"/{name}" if name else "")


def workload(kind, name, namespace):
    label = {"private-consumption-test": name}
    pod = {
        "automountServiceAccountToken": False,
        "schedulerName": "private-consumption-never-schedule",
        "nodeSelector": {"private-consumption.test/never-schedule": name},
        "securityContext": {"runAsNonRoot": True, "runAsUser": 10001,
                            "seccompProfile": {"type": "RuntimeDefault"}},
        "containers": [{"name": "probe", "image": f"private-consumption-never-pull-{name}:latest",
                        "imagePullPolicy": "Never", "command": ["/bin/true"],
                        "securityContext": {"allowPrivilegeEscalation": False,
                                            "capabilities": {"drop": ["ALL"]}}}],
    }
    template = {"metadata": {"labels": label}, "spec": pod}
    group = "" if kind == "ReplicationController" else "batch" if kind in ("Job", "CronJob") else "apps"
    spec = {"template": template}
    if kind in ("Job", "CronJob"):
        pod["restartPolicy"] = "Never"
        spec.update(parallelism=0, suspend=True)
        if kind == "CronJob":
            spec = {"schedule": "0 0 * * *", "suspend": True, "jobTemplate": {"spec": spec}}
    else:
        spec["selector"] = label if kind == "ReplicationController" else {"matchLabels": label}
        if kind != "DaemonSet":
            spec["replicas"] = 0
        if kind == "StatefulSet":
            spec["serviceName"] = name
    return {"apiVersion": f"{group}/v1" if group else "v1", "kind": kind,
            "metadata": {"name": name, "namespace": namespace}, "spec": spec}


def pod_spec(value):
    return (value["spec"]["jobTemplate"]["spec"]["template"]["spec"]
            if value["kind"] == "CronJob" else value["spec"]["template"]["spec"])


def variants(value):
    values = []
    for name in PRIVATE:
        current = copy.deepcopy(value)
        pod_spec(current)["volumes"] = [{"name": "private", "secret": {"secretName": name, "optional": True}}]
        values.append(current)
    for form in ("projected", "env", "envFrom", "init", "csi", "imagePull"):
        current = copy.deepcopy(value)
        pod = pod_spec(current)
        name = "router-services-observer-identity"
        if form == "projected":
            pod["volumes"] = [{"name": "private", "projected": {"sources": [{"secret": {"name": name}}]}}]
        elif form == "env":
            pod["containers"][0]["env"] = [{"name": "PRIVATE_PROBE",
                                           "valueFrom": {"secretKeyRef": {"name": name, "key": "config.json"}}}]
        elif form == "envFrom":
            pod["containers"][0]["envFrom"] = [{"secretRef": {"name": name}}]
        elif form == "init":
            init = copy.deepcopy(pod["containers"][0])
            init.update(name="init-probe", envFrom=[{"secretRef": {"name": name}}])
            pod["initContainers"] = [init]
        elif form == "csi":
            pod["volumes"] = [{"name": "private", "csi": {"driver": "private-consumption.test",
                                                        "nodePublishSecretRef": {"name": name}}}]
        else:
            pod["imagePullSecrets"] = [{"name": name}]
        values.append(current)
    return values


def denied(response):
    message = response.json().get("message", "")
    require(response.status_code == 403 and isinstance(message, str)
            and re.search(r"(?<![a-z0-9-])kars-private-consumption(?![a-z0-9-])", message) is not None,
            "Expected the exact private consumption admission denial, not RBAC/schema failure")


def named_cases(h, namespace):
    current = h.api("GET", f"/api/v1/namespaces/{namespace}", status=200).json()
    annotations = current["metadata"].get("annotations", {})
    require(annotations.get(PREFIX + "enabled") == "true"
            and annotations.get(PREFIX + "state") == "Qualified",
            "The existing operator activation flow must qualify this namespace first")
    actor = "private-boundary-" + uuid.uuid4().hex[:10]
    h.token_identity(actor, TENANT, actor)
    identity = h.api("POST", "/apis/authentication.k8s.io/v1/selfsubjectreviews",
                     body={"apiVersion": "authentication.k8s.io/v1", "kind": "SelfSubjectReview"},
                     user=actor, status=201).json()["status"]["userInfo"]
    require(bool(identity.get("uid")), "Native named-workload actor UID is missing")
    account = h.api("GET", path(TENANT, "serviceaccounts", name=actor), status=200).json()
    require(account["metadata"]["uid"] == identity["uid"], "Native named-workload actor identity changed")
    created = [(path(TENANT, "serviceaccounts", name=actor), identity["uid"])]
    try:
        for kind, group, version, plural in KINDS:
            name = "private-" + kind.lower() + "-" + uuid.uuid4().hex[:8]
            base = workload(kind, name, namespace)
            role_name = name + "-role"
            role_path = path(namespace, "roles", "rbac.authorization.k8s.io", role_name)
            role = {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
                    "metadata": {"name": role_name, "namespace": namespace},
                    "rules": [{"apiGroups": [group], "resources": [plural], "verbs": ["create"]},
                              {"apiGroups": [group], "resources": [plural], "verbs": ["get", "update", "patch"],
                               "resourceNames": [name]}]}
            observed = h.api("POST", path(namespace, "roles", "rbac.authorization.k8s.io"), body=role, status=201).json()
            created.append((role_path, observed["metadata"]["uid"]))
            binding = {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
                       "metadata": {"name": role_name, "namespace": namespace},
                       "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": role_name},
                       "subjects": [{"kind": "User", "name": identity["username"]}]}
            observed = h.api("POST", path(namespace, "rolebindings", "rbac.authorization.k8s.io"),
                             body=binding, status=201).json()
            created.append((path(namespace, "rolebindings", "rbac.authorization.k8s.io", role_name),
                            observed["metadata"]["uid"]))
            collection = path(namespace, plural, group)
            require(h.api("POST", collection + "?dryRun=All", body=base, user=actor).status_code == 201,
                    "Ordinary non-consuming workload CREATE was not preserved")
            for invalid in variants(base):
                denied(h.api("POST", collection + "?dryRun=All", body=invalid, user=actor))
            nodes = h.api("GET", "/api/v1/nodes?labelSelector=private-consumption.test%2Fnever-schedule%3D" + name,
                          status=200).json()
            require(not nodes.get("items"), "Admission fixture must have no eligible node")
            observed = h.api("POST", collection, body=base, status=201).json()
            object_path = collection + "/" + name
            created.append((object_path, observed["metadata"]["uid"]))
            current_role = h.api("GET", role_path, status=200).json()
            h.api("PATCH", role_path, body={"metadata": {
                "uid": current_role["metadata"]["uid"], "resourceVersion": current_role["metadata"]["resourceVersion"]},
                "rules": role["rules"][1:]}, status=200)
            for verb in ("patch", "update"):
                for named in (False, True):
                    attributes = {"group": group, "resource": plural, "verb": verb, "namespace": namespace}
                    if named:
                        attributes["name"] = name
                    review = h.api("POST", "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews", body={
                        "apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
                        "spec": {"resourceAttributes": attributes}}, user=actor, status=201).json()
                    require(review.get("status", {}).get("allowed") is named,
                            "Named RBAC must allow the exact mutation while the unnamed SAR denies it")
            live = h.api("GET", object_path, status=200).json()
            ordinary = copy.deepcopy(live)
            ordinary["metadata"].setdefault("annotations", {})["private-consumption.test/ordinary"] = "reviewed"
            require(h.api("PUT", object_path + "?dryRun=All", body=ordinary, user=actor).status_code == 200,
                    "Ordinary named UPDATE was not preserved")
            updates = variants(live)
            if kind == "Job":
                # Job pod templates are immutable. Exercise its mutable
                # metadata with old protected consumption instead of counting
                # an unrelated immutable-template rejection as admission.
                h.api("DELETE", object_path, body={"apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": live["metadata"]["uid"], "resourceVersion": live["metadata"]["resourceVersion"]}},
                    status=(200, 202))
                h.poll("owned zero-parallelism Job retirement", lambda:
                       h.api("GET", object_path).status_code == 404)
                private_job = variants(base)[0]
                live = h.api("POST", collection, body=private_job, status=201).json()
                created[-1] = (object_path, live["metadata"]["uid"])
                update = copy.deepcopy(live)
                update["metadata"].setdefault("annotations", {})["private-consumption.test/update"] = "private"
                updates = [update]
            for invalid in updates:
                denied(h.api("PUT", object_path + "?dryRun=All", body=invalid, user=actor))
                denied(h.api("PATCH", object_path + "?dryRun=All", body={
                    "metadata": {"uid": live["metadata"]["uid"], "resourceVersion": live["metadata"]["resourceVersion"],
                                 "annotations": invalid["metadata"].get("annotations", {})},
                    "spec": invalid["spec"]}, user=actor))
        pod_and_controller_cases(h, namespace, actor, identity, annotations, created)
        h.passed("private-consumption named RBAC dry-run matrix")
    finally:
        for object_path, expected_uid in reversed(created):
            response = h.api("GET", object_path)
            if response.status_code == 404:
                continue
            require(response.status_code == 200, "Owned admission fixture cleanup read failed")
            value = response.json()
            require(value["metadata"]["uid"] == expected_uid, "Admission fixture was replaced; foreign object preserved")
            h.api("DELETE", object_path, body={"apiVersion": "v1", "kind": "DeleteOptions",
                "preconditions": {"uid": expected_uid, "resourceVersion": value["metadata"]["resourceVersion"]}},
                status=(200, 202))


def pod_and_controller_cases(h, namespace, actor, identity, annotations, created):
    name = "private-pod-" + uuid.uuid4().hex[:8]
    base = workload("Deployment", name, namespace)
    pod = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name, "namespace": namespace},
           "spec": copy.deepcopy(pod_spec(base))}
    role_name = name + "-role"
    role = {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {"name": role_name, "namespace": namespace},
            "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["create"]},
                      {"apiGroups": [""], "resources": ["pods", "pods/ephemeralcontainers"],
                       "verbs": ["get", "patch", "update"], "resourceNames": [name]}]}
    observed = h.api("POST", path(namespace, "roles", "rbac.authorization.k8s.io"), body=role, status=201).json()
    created.append((path(namespace, "roles", "rbac.authorization.k8s.io", role_name), observed["metadata"]["uid"]))
    observed = h.api("POST", path(namespace, "rolebindings", "rbac.authorization.k8s.io"), body={
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": {"name": role_name, "namespace": namespace},
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": role_name},
        "subjects": [{"kind": "User", "name": identity["username"]}]}, status=201).json()
    created.append((path(namespace, "rolebindings", "rbac.authorization.k8s.io", role_name), observed["metadata"]["uid"]))
    collection = path(namespace, "pods")
    require(h.api("POST", collection + "?dryRun=All", body=pod, user=actor).status_code == 201,
            "Ordinary Pod dry-run CREATE was not preserved")
    for invalid in variants(base):
        private_pod = copy.deepcopy(pod)
        private_pod["spec"] = pod_spec(invalid)
        private_pod["metadata"]["annotations"] = {PREFIX + "epoch": annotations[PREFIX + "epoch"]}
        denied(h.api("POST", collection + "?dryRun=All", body=private_pod, user=actor))
    nodes = h.api("GET", "/api/v1/nodes?labelSelector=private-consumption.test%2Fnever-schedule%3D" + name,
                  status=200).json()
    require(not nodes.get("items"), "The inert Pod fixture must have no eligible node")
    live = h.api("POST", collection, body=pod, status=201).json()
    created.append((collection + "/" + name, live["metadata"]["uid"]))
    ephemeral = copy.deepcopy(live)
    ephemeral["spec"]["ephemeralContainers"] = [{
        "name": "private-debug", "image": pod["spec"]["containers"][0]["image"],
        "imagePullPolicy": "Never", "command": ["/bin/true"],
        "envFrom": [{"secretRef": {"name": "router-services-observer-identity"}}],
    }]
    denied(h.api("PUT", collection + "/" + name + "/ephemeralcontainers?dryRun=All",
                 body=ephemeral, user=actor))
    root_namespace = annotations[PREFIX + "root-namespace"]
    if namespace != root_namespace:
        return
    deployments = h.api("GET", path(namespace, "deployments", "apps"), status=200).json()["items"]
    root = next((item for item in deployments if item["metadata"]["uid"] == annotations[PREFIX + "root-deployment-uid"]), None)
    require(root is not None, "Reviewed root Deployment is missing")
    replicasets = h.api("GET", path(namespace, "replicasets", "apps"), status=200).json()["items"]
    owned = [item for item in replicasets if any(
        owner.get("controller") and owner.get("kind") == "Deployment" and owner.get("uid") == root["metadata"]["uid"]
        for owner in item["metadata"].get("ownerReferences", []))]
    require(bool(owned), "A real controller-owned ReplicaSet is required for handoff qualification")
    replica_set = owned[0]
    source = copy.deepcopy(replica_set["spec"]["template"])
    source.update(apiVersion="v1", kind="Pod")
    source["metadata"].update(name=name + "-handoff", namespace=namespace,
                              ownerReferences=[{"apiVersion": "apps/v1", "kind": "ReplicaSet",
                                                "name": replica_set["metadata"]["name"], "uid": replica_set["metadata"]["uid"],
                                                "controller": True}])
    source["metadata"].setdefault("annotations", {})[PREFIX + "epoch"] = annotations[PREFIX + "epoch"]
    source["spec"]["schedulerName"] = "private-consumption-never-schedule"
    source["spec"]["nodeSelector"] = {"private-consumption.test/never-schedule": name}
    profile = annotations[PREFIX + "profile"]
    if profile == "service-accounts":
        username = "system:serviceaccount:kube-system:replicaset-controller"
        actor_uid = annotations[PREFIX + "replicaset-controller-uid"]
        groups = ["system:authenticated", "system:serviceaccounts", "system:serviceaccounts:kube-system"]
    else:
        require(profile == "kcm-certificate", "Unknown controller profile")
        username, actor_uid, groups = "system:kube-controller-manager", None, ["system:authenticated"]
    headers = [("Impersonate-User", username), *[("Impersonate-Group", group) for group in groups]]
    if actor_uid:
        headers.append(("Impersonate-Uid", actor_uid))
    response = h.client("admin").request("POST", collection + "?dryRun=All", json=source, headers=headers)
    require(response.status_code == 201, "The exact authenticated controller handoff was not preserved")
    wrong = [(key, value) for key, value in headers if key != "Impersonate-Uid"] + [("Impersonate-Uid", "wrong-uid")]
    denied(h.client("admin").request("POST", collection + "?dryRun=All", json=source, headers=wrong))
