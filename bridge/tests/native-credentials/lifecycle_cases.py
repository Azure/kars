"""Real consumer lifecycle, namespace-resource continuity and ephemeral workspaces."""

import base64
import copy
import json
import secrets
import time

from credential_cases import KEY, SOURCE, sandbox, selection
from credential_review import reviewed_credential_write
from native_api import BRIDGE, CORE, WRITER, command, core, require, resource, uid, until
from runtime_state import assert_agent_exec_denied, assert_ephemeral_workspace, runtime_state

APPS = "/apis/apps/v1"
RBAC = "/apis/rbac.authorization.k8s.io/v1"


def owner_is(value, kind, identity):
    return any(owner["kind"] == kind and owner["uid"] == identity and owner.get("controller")
               for owner in value["metadata"].get("ownerReferences", []))


def deployment_path(name):
    return resource(f"kars-{name}", "deployments", name, APPS)


def reviewable_target(setup, created):
    workspace, name = created["metadata"]["namespace"], created["metadata"]["name"]
    runtime_name = f"kars-{name}"
    expected_spec = copy.deepcopy(created["spec"])
    expected_uid = uid(created)
    generation = created["metadata"]["generation"]

    def check():
        current = setup.admin.get(resource(workspace, "karssandboxes", name))
        require(uid(current) == expected_uid and current["spec"] == expected_spec
                and current["metadata"].get("generation") == generation
                and not current["metadata"].get("deletionTimestamp"),
                "Credential target changed before initial operator review")
        namespace = setup.admin.optional(f"/api/v1/namespaces/{runtime_name}")
        if namespace is None:
            return None
        require(namespace.get("kind") == "Namespace"
                and namespace["metadata"].get("name") == runtime_name
                and not namespace["metadata"].get("deletionTimestamp"),
                "Credential runtime namespace is not current")
        owner = namespace["metadata"].get("annotations", {}).get("kars.azure.com/sandbox-uid")
        binding = current["metadata"].get("annotations", {}).get("kars.azure.com/namespace-uid")
        require(owner == expected_uid, "Credential runtime namespace has a different owner")
        if binding is None:
            return None
        require(binding == uid(namespace), "Credential target namespace binding changed")
        if "kars.azure.com/namespace-cleanup" not in current["metadata"].get("finalizers", []):
            return None
        return current

    # Namespace ownership is initialized through metadata writes after CREATE.
    # It must be part of the first review, not silently rebased after submission.
    return until("core-owned target before initial credential review", check, 60)


def running(setup, workspace, name):
    def check():
        value = setup.admin.get(resource(workspace, "karssandboxes", name))
        deployment = setup.admin.optional(deployment_path(name))
        if not deployment or deployment.get("status", {}).get("availableReplicas", 0) != 1:
            return None
        namespace = setup.admin.get(f"/api/v1/namespaces/kars-{name}")
        annotations = deployment["metadata"].get("annotations", {})
        require(annotations.get("kars.azure.com/credential-sandbox-uid") == uid(value)
                and annotations.get("kars.azure.com/credential-namespace-uid") == uid(namespace)
                and namespace["metadata"].get("annotations", {}).get("kars.azure.com/sandbox-uid") == uid(value),
                "Running deployment does not bind its current Sandbox and runtime namespace UIDs")
        pods = setup.admin.get(core(f"kars-{name}", "pods"))["items"]
        replicasets = setup.admin.get(resource(f"kars-{name}", "replicasets", group=APPS))["items"]
        owners = {uid(item) for item in replicasets if owner_is(item, "Deployment", uid(deployment))}
        candidates = [
            pod for pod in pods
            if any(owner_is(pod, "ReplicaSet", identity) for identity in owners)
            and not pod["metadata"].get("deletionTimestamp")
            and len(pod.get("status", {}).get("containerStatuses", [])) >= 2
            and all(item.get("ready") for item in pod["status"]["containerStatuses"])
            and all(item.get("ready") for item in pod.get("status", {}).get("initContainerStatuses", []))
        ]
        return (value, deployment, candidates[0]) if len(candidates) == 1 else None
    return until(f"real owned router and runtime for {name}", check, 240)


def halted(setup, name):
    def check():
        deployment = setup.admin.optional(deployment_path(name))
        if deployment and deployment["spec"].get("replicas", 1) != 0:
            return False
        pods = setup.admin.get(core(f"kars-{name}", "pods"))["items"]
        return not pods
    until(f"all consumers, including terminating Pods, stopped for {name}", check)


class LifecycleCases:
    def __init__(self, setup, bff, credentials):
        self.setup, self.bff, self.credentials = setup, bff, credentials
        self.targets = {}
        self.team_target = None
        self.credential_diagnostic_target = None

    def create_delivery(self):
        workspace = "native-delivery"
        grant = self.credentials.workspace(workspace)
        for name in ["native-delivery-a", "native-delivery-b"]:
            value = sandbox(self.setup, workspace, name)
            self.credential_diagnostic_target = {"workspace": workspace, "sandbox": name, "uid": uid(value)}
            value = reviewable_target(self.setup, value)
            token = secrets.token_hex(24)
            reviewed_credential_write(self.bff, {
                "namespace": workspace, "kind": "KarsSandbox", "target": name,
                "targetUid": uid(value), "key": KEY, "value": token,
            }, grant, value["metadata"]["generation"])
            path = resource(workspace, "karssandboxes", name)
            current = self.setup.admin.get(path)
            selected = current["spec"]["credentialBindings"]["sources"][-1]
            require(selected["owner"]["uid"] == uid(value), "Target selection did not bind actual CREATE UID")
            self.setup.admin.patch(path, {"spec": {"suspended": False}})
            value, deployment, pod = running(self.setup, workspace, name)
            projection = self.setup.admin.get(core(f"kars-{name}", "secrets", f"{name}-credential-projection"))
            require(base64.b64decode(projection["data"][KEY]).decode() == token,
                    "Real core projection did not deliver selected source value")
            runtime = self.setup.admin.get(f"/api/v1/namespaces/kars-{name}")
            require(owner_is(projection, "Namespace", uid(runtime))
                    and projection["metadata"]["annotations"].get("kars.azure.com/credential-sandbox-uid") == uid(value)
                    and projection["metadata"]["annotations"].get("kars.azure.com/credential-projection-uid") == uid(projection),
                    "Core projection is not sealed to current namespace/Sandbox/projection UIDs")
            proof = runtime_state(f"kars-{name}", pod["metadata"]["name"])
            require(proof["slackPresent"], "Projected credential did not reach the real agent environment")
            self.targets[name] = {"workspace": workspace, "grant": grant, "sandbox": value,
                                  "deployment": deployment, "pod": pod, "selection": selected,
                                  "source": self.setup.admin.get(core(workspace, "secrets", selected["source"]["name"]))}

    def source_uid_fence(self):
        selected = self.targets["native-delivery-a"]
        workspace = selected["workspace"]
        old = selected["source"]
        path = core(workspace, "secrets", old["metadata"]["name"])
        self.setup.admin.delete(path)
        halted(self.setup, "native-delivery-a")
        survivor = running(self.setup, workspace, "native-delivery-b")
        require(uid(survivor[0]) == uid(self.targets["native-delivery-b"]["sandbox"]),
                "Deleting one selected source recreated another consumer")
        replacement = copy.deepcopy(old)
        replacement["metadata"] = {
            key: value for key, value in old["metadata"].items()
            if key in ["name", "namespace", "annotations", "labels", "ownerReferences"]
        }
        replacement["data"][KEY] = base64.b64encode(secrets.token_hex(24).encode()).decode()
        new = self.setup.admin.create(core(workspace, "secrets"), replacement)
        require(uid(new) != uid(old), "Recreated source unexpectedly retained UID")
        self.setup.ready_grant(workspace)
        time.sleep(5)
        halted(self.setup, "native-delivery-a")
        current = self.setup.admin.get(resource(workspace, "karssandboxes", "native-delivery-a"))
        require(current["spec"]["credentialBindings"]["sources"][-1]["source"]["uid"] == uid(old),
                "Controller silently adopted a source replacement")
        running(self.setup, workspace, "native-delivery-b")

    def team_rebind(self):
        self.setup.grant(CORE, self.credentials.writer)
        self.bff.channel(CORE, secrets.token_hex(24))
        grant = self.setup.ready_grant(CORE)
        source = self.setup.admin.get(core(CORE, "secrets", SOURCE))
        team = self.setup.admin.create(resource(CORE, "karsteams"), {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTeam",
            "metadata": {"name": "native-team", "namespace": CORE,
                         "annotations": {"kars.azure.com/owner-sub": "native-operator"}},
            "spec": {"charter": "Native credential continuity", "paused": False,
                     "envelope": {"tier": 2, "authorityCeiling": 2, "delegationDepth": 2},
                     "blueprint": {"runtime": "OpenClaw", "isolation": "standard",
                                   "model": {"provider": "azure-openai", "deployment": "native"},
                                   "credentialBindings": selection(grant, source)}},
        })
        def principal():
            tasks = self.setup.admin.get(resource(CORE, "karstasks"))["items"]
            return next((task for task in tasks if owner_is(task, "KarsTeam", uid(team))
                         and task["metadata"].get("annotations", {}).get("kars.azure.com/team-role") == "principal"), None)
        task = until("real Team-owned principal task", principal)
        task_name = task["metadata"]["name"]
        task_path = resource(CORE, "karstasks", task_name)
        self.setup.admin.patch(task_path, {
            "metadata": {"annotations": {"kars.azure.com/owner-sub": "native-operator"}},
            "spec": {"execution": {"launch": True}},
        })
        def bound():
            current = self.setup.admin.get(task_path)
            return current if current.get("status", {}).get("sandboxRef", {}).get("name") else None
        task = until("Task actual execution Sandbox", bound)
        name = task["status"]["sandboxRef"]["name"]
        value, deployment, pod = running(self.setup, CORE, name)
        namespace = self.setup.admin.get(f"/api/v1/namespaces/kars-{name}")
        self.team_target = {"task": task_name, "sandbox": name, "workspace": CORE,
                            "taskUid": uid(task), "teamUid": uid(team),
                            "sandboxUid": uid(value), "namespaceUid": uid(namespace)}
        datum = self.setup.admin.create(core(f"kars-{name}", "configmaps"), {
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "native-continuity", "namespace": f"kars-{name}"},
            "data": {"sentinel": "retained-user-data"},
        })
        assert_agent_exec_denied(f"kars-{name}", pod["metadata"]["name"])
        assert_ephemeral_workspace(pod)
        before_state = runtime_state(f"kars-{name}", pod["metadata"]["name"])
        require(before_state["dataWritable"] and before_state["dataMarker"],
                "Controlled agent cannot write its ephemeral sandbox workspace")
        pod_path = core(f"kars-{name}", "pods", pod["metadata"]["name"])
        old_finalizers = pod["metadata"].get("finalizers", [])
        self.setup.admin.patch(pod_path, {
            "metadata": {"finalizers": old_finalizers + ["native.kars.test/continuity"]},
        })
        before_digest = self.setup.admin.get(task_path)["status"].get("envelopeDigest")
        self.bff.channel(CORE, secrets.token_hex(24), channel="telegram")
        def pausing():
            current = self.setup.admin.get(task_path)
            deployment_now = self.setup.admin.get(deployment_path(name))
            return current if (
                current.get("status", {}).get("executionPhase") == "PausingCredentials"
                and not current["status"].get("envelopeDigest")
                and any(condition["type"] == "Ready" and condition["status"] == "False"
                        for condition in current["status"].get("conditions", []))
                and deployment_now["spec"].get("replicas") == 0
            ) else None
        paused = until("real rebind pause while old terminating Pod remains", pausing)
        require(paused["spec"]["execution"]["launch"] is True,
                "Credential rebind used destructive unlaunch")
        require(self.setup.admin.optional(resource(CORE, "karsreceipts", task_name)) is None,
                "Old current receipt survived revoked rebind authority")
        old_pod = self.setup.admin.get(pod_path)
        require(old_pod["metadata"].get("deletionTimestamp"), "Rebind did not retire the old consumer")
        require("TELEGRAM_BOT_TOKEN" not in paused["spec"]["blueprint"]["credentialBindings"]["sources"][0]["keys"],
                "New authority was installed before old terminating consumers disappeared")
        self.setup.admin.patch(pod_path, {"metadata": {"finalizers": old_finalizers}})
        def resumed():
            current = self.setup.admin.get(task_path)
            status = current.get("status", {})
            return current if (
                status.get("phase") == "Ready"
                and status.get("observedGeneration") == current["metadata"]["generation"]
                and status.get("envelopeDigest") and status["envelopeDigest"] != before_digest
                and status.get("executionPhase") == "Running"
            ) else None
        current = until("current reattested Team credentials and resumed execution", resumed, 240)
        after, after_deployment, after_pod = running(self.setup, CORE, name)
        require(uid(current) == uid(task) and uid(after) == uid(value)
                and uid(after_deployment) == uid(deployment)
                and uid(self.setup.admin.get(f"/api/v1/namespaces/kars-{name}")) == uid(namespace),
                "Rebind recreated durable Task/Sandbox/Deployment/namespace identity")
        after_datum = self.setup.admin.get(core(f"kars-{name}", "configmaps", "native-continuity"))
        require(uid(after_datum) == uid(datum) and after_datum["data"] == datum["data"],
                "Rebind deleted namespace-owned stored data")
        receipt = self.setup.admin.get(resource(CORE, "karsreceipts", task_name))
        require(owner_is(receipt, "KarsTask", uid(task))
                and current["status"]["envelopeDigest"] in json.dumps(receipt["spec"]),
                "Resumed receipt does not bind current Task authority")
        require(uid(after_pod) != uid(pod), "Old consumer was reused after authority change")
        assert_ephemeral_workspace(after_pod)
        after_state = runtime_state(f"kars-{name}", after_pod["metadata"]["name"])
        require(after_state["telegramPresent"], "Resumed agent did not receive its new credential authority")
        require(after_state["dataWritable"] and after_state["dataMarker"]
                and after_state["dataMarker"] != before_state["dataMarker"],
                "Replacement Pod did not receive its own writable ephemeral workspace")

    def release_team_fixture(self):
        require(self.team_target is not None, "No owned Team execution fixture to release")
        target = self.team_target
        task_path = resource(target["workspace"], "karstasks", target["task"])
        sandbox_path = resource(target["workspace"], "karssandboxes", target["sandbox"])
        namespace_path = f"/api/v1/namespaces/kars-{target['sandbox']}"

        def current():
            task = self.setup.admin.get(task_path)
            require(uid(task) == target["taskUid"] and owner_is(task, "KarsTeam", target["teamUid"]),
                    "Team execution fixture ownership changed; no cleanup performed")
            sandbox_now = self.setup.admin.optional(sandbox_path)
            namespace_now = self.setup.admin.optional(namespace_path)
            require(sandbox_now is None or uid(sandbox_now) == target["sandboxUid"],
                    "Team Sandbox fixture was replaced; no cleanup performed")
            require(namespace_now is None or uid(namespace_now) == target["namespaceUid"],
                    "Team namespace fixture was replaced; no cleanup performed")
            return task, sandbox_now, namespace_now

        task, _, _ = current()
        self.setup.admin.request("PATCH", task_path, {
            "metadata": {"uid": uid(task), "resourceVersion": task["metadata"]["resourceVersion"]},
            "spec": {"execution": {"launch": False}},
        }, patch_type="application/merge-patch+json")

        def released():
            task, sandbox_now, namespace_now = current()
            require(task["spec"]["execution"]["launch"] is False,
                    "Completed Team fixture was relaunched during cleanup")
            return sandbox_now is None and namespace_now is None

        until("normal owned Team execution cleanup before independent observer", released)

    def writer_uninstall(self):
        retained = self.targets["native-delivery-b"]
        name, workspace = "native-delivery-b", retained["workspace"]
        before = running(self.setup, workspace, name)
        source_path = core(workspace, "secrets", retained["source"]["metadata"]["name"])
        source = self.setup.admin.get(source_path)
        controller = self.setup.admin.get(core(CORE, "serviceaccounts", "kars-controller"))
        namespace = self.setup.admin.get(f"/api/v1/namespaces/{CORE}")
        writer_path = core(BRIDGE, "serviceaccounts", WRITER)
        writer = self.setup.admin.get(writer_path)
        require(any(item.startswith("kars.azure.com/credential-reader-")
                    for item in writer["metadata"].get("finalizers", [])),
                "Native writer has no name-continuity hold")
        self.setup.admin.patch(resource(BRIDGE, "deployments", "kars-bridge-bff", APPS),
                               {"spec": {"replicas": 0}})
        # Freeze reconciliation to make the otherwise fast native name-hold
        # transition observable; never change the controller's election setting.
        controller_path = resource(CORE, "deployments", "kars-controller", APPS)
        self.setup.admin.patch(controller_path, {"spec": {"replicas": 0}})
        until("controller stopped before held-name race", lambda:
              not self.setup.admin.get(core(CORE, "pods") +
                  "?labelSelector=app.kubernetes.io%2Fcomponent%3Dcontroller")["items"])
        self.setup.admin.delete(writer_path)
        held = self.setup.admin.get(writer_path)
        require(uid(held) == uid(writer) and held["metadata"].get("deletionTimestamp"),
                "Writer deletion bypassed its native name hold")
        self.setup.admin.request("POST", core(BRIDGE, "serviceaccounts"), {
            "apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": WRITER, "namespace": BRIDGE},
        }, expected=(409,))
        self.setup.admin.patch(controller_path, {"spec": {"replicas": 1}})
        until("all old writer reads revoked before name release",
              lambda: self.setup.admin.optional(writer_path) is None, 240)
        review = self.setup.admin.create("/apis/authorization.k8s.io/v1/subjectaccessreviews", {
            "apiVersion": "authorization.k8s.io/v1", "kind": "SubjectAccessReview",
            "spec": {"user": f"system:serviceaccount:{BRIDGE}:{WRITER}",
                     "groups": ["system:serviceaccounts", f"system:serviceaccounts:{BRIDGE}",
                                "system:authenticated"],
                     "resourceAttributes": {"namespace": workspace, "verb": "get", "group": "",
                                            "resource": "secrets", "name": source["metadata"]["name"]}},
        })
        require(review["status"].get("allowed") is False
                and not review["status"].get("evaluationError"),
                "Native name was released before its Secret read authority was revoked")
        recreated = self.setup.account(BRIDGE, WRITER)
        require(uid(recreated) != uid(writer), "Name reuse retained a deleted ServiceAccount UID")
        replacement_actor = self.setup.actor(BRIDGE, WRITER)
        replacement_actor.request("GET", source_path, expected=(403,))
        replacement_actor.request("GET", core(workspace, "secrets"), expected=(403,))
        after = running(self.setup, workspace, name)
        latest_source = self.setup.admin.get(source_path)
        require(uid(after[0]) == uid(before[0]) and uid(after[1]) == uid(before[1])
                and uid(latest_source) == uid(source) and latest_source["data"] == source["data"]
                and uid(self.setup.admin.get(f"/api/v1/namespaces/{CORE}")) == uid(namespace)
                and uid(self.setup.admin.get(core(CORE, "serviceaccounts", "kars-controller"))) == uid(controller),
                "Writer uninstall destroyed valid source/consumer/core identity")
        grant = self.setup.admin.get(resource(workspace, "karscredentialgrants", "workspace"))
        require(grant["status"]["phase"] == "Ready"
                and any(item["type"] == "WriterReady" and item["status"] == "False"
                        for item in grant["status"].get("conditions", [])),
                "Writer revocation incorrectly revoked independent delivery authority")

    def grant_revocation(self):
        retained = self.targets["native-delivery-b"]
        workspace, name = retained["workspace"], "native-delivery-b"
        self.setup.admin.patch(resource(workspace, "karscredentialgrants", "workspace"),
                               {"spec": {"enabled": False}})
        halted(self.setup, name)
        source = self.setup.admin.get(core(workspace, "secrets", retained["source"]["metadata"]["name"]))
        require(uid(source) == uid(retained["source"]) and source["data"] == retained["source"]["data"],
                "Revocation deleted durable native source values")
