# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""In-memory API fixtures; the native probe never imports this module."""

import copy

import eval_pod_admission as probe

JOB = f"karseval-{probe.EVAL_NAME}-runnow-0123456789"
PRIVATE = "private-body-must-never-cross-the-report-boundary"


def status(code, reason, name, kind, message=PRIVATE):
    return {"apiVersion": "v1", "kind": "Status", "status": "Failure",
            "code": code, "reason": reason, "message": message,
            "details": {"name": name, "kind": kind}}


def objects():
    parent = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsEval",
              "metadata": {"name": probe.EVAL_NAME, "namespace": probe.SOURCE_NAMESPACE,
                           "uid": "eval-uid", "resourceVersion": "40", "generation": 2},
              "spec": {"targetSandboxRef": {"name": "e2e-test"},
                       "corpus": {"builtin": "jailbreak-baseline"}, "schedule": "*/15 * * * *"}}
    pod_spec = {
        "restartPolicy": "Never",
        "securityContext": {"runAsNonRoot": True, "seccompProfile": {"type": "RuntimeDefault"}},
        "containers": [{
            "name": "runner", "image": "custom-numeric-nonroot-runner:latest",
            "securityContext": {
                "runAsNonRoot": True, "allowPrivilegeEscalation": False,
                "capabilities": {"drop": ["ALL"]}, "seccompProfile": {"type": "RuntimeDefault"},
            },
            "args": ["--corpus", "/etc/kars/eval-corpus/corpus.json",
                     "--router-base", "http://e2e-test.kars-e2e-test.svc.cluster.local:8443", "--output", "/dev/stdout"],
            "env": [{"name": "KARS_EVAL_NAME", "value": probe.EVAL_NAME}],
            "volumeMounts": [{"name": "corpus", "mountPath": "/etc/kars/eval-corpus", "readOnly": True}],
        }],
        "volumes": [{"name": "corpus", "configMap": {"name": "real-produced-corpus"}}],
    }
    owner = {"apiVersion": parent["apiVersion"], "kind": parent["kind"],
             "name": probe.EVAL_NAME, "uid": "eval-uid", "controller": True, "blockOwnerDeletion": True}
    labels = {"app.kubernetes.io/managed-by": "kars-controller", "kars.azure.com/karseval": probe.EVAL_NAME}
    def producer(kind, name):
        return {"apiVersion": "batch/v1", "kind": kind,
                "metadata": {"name": name, "namespace": probe.SOURCE_NAMESPACE,
                             "uid": f"{kind}-uid", "generation": 1, "resourceVersion": "41",
                             "ownerReferences": [copy.deepcopy(owner)], "labels": labels.copy()},
                "spec": {}}
    template = {"metadata": {"labels": labels.copy()}, "spec": pod_spec}
    job = producer("Job", JOB)
    job["spec"] = {"backoffLimit": 0, "template": copy.deepcopy(template)}
    # Real Job admission adds identity labels which the CronJob template lacks.
    job["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/controller-uid"] = "Job-uid"
    cron = producer("CronJob", probe.CRON_NAME)
    cron["spec"] = {"schedule": "*/15 * * * *", "jobTemplate": {"spec": {
        "backoffLimit": 0, "template": copy.deepcopy(template),
    }}}
    return parent, job, cron


class Cluster:
    def __init__(self):
        parent, job, cron = objects()
        self.sources = {
            f"/apis/kars.azure.com/v1alpha1/namespaces/{probe.SOURCE_NAMESPACE}/karsevals/{probe.EVAL_NAME}": parent,
            f"/apis/batch/v1/namespaces/{probe.SOURCE_NAMESPACE}/jobs/{JOB}": job,
            f"/apis/batch/v1/namespaces/{probe.SOURCE_NAMESPACE}/cronjobs/{probe.CRON_NAME}": cron,
        }
        self.namespace = None
        self.calls = []
        self.before = None
        self.after = None
        self.pod_response = None
        self.delete_response = None
        self.service_account_response = None

    def request(self, _port, method, path, obj=None):
        self.calls.append((method, path, copy.deepcopy(obj)))
        if self.before:
            self.before(method, path, obj)
        result = self.respond(method, path, obj)
        if self.after:
            self.after(method, path, obj)
        return copy.deepcopy(result)

    def respond(self, method, path, obj):
        if path in self.sources:
            assert method == "GET", "The probe must not write to a source object"
            return 200, self.sources[path]
        if path == probe.NAMESPACES:
            assert method == "POST" and self.namespace is None
            self.namespace = copy.deepcopy(obj)
            self.namespace["metadata"].update({"uid": "owned-namespace", "resourceVersion": "50"})
            self.namespace["metadata"]["labels"]["kubernetes.io/metadata.name"] = obj["metadata"]["name"]
            self.namespace["spec"] = {"finalizers": ["kubernetes"]}
            return 201, self.namespace
        name = path[len(probe.NAMESPACES) + 1:].split("/")[0]
        if path.endswith("/serviceaccounts/default"):
            assert method == "GET"
            if self.service_account_response:
                return self.service_account_response
            return 200, {"apiVersion": "v1", "kind": "ServiceAccount",
                         "metadata": {"name": "default", "namespace": name,
                                      "uid": "sa-uid", "resourceVersion": "51"}}
        if "/pods" in path:
            assert method == "POST" and path.endswith("/pods?dryRun=All")
            if self.pod_response:
                return self.pod_response(obj)
            if "securityContext" in obj["spec"]:
                return 201, obj
            message = (f'pods "{obj["metadata"]["name"]}" is forbidden: '
                       'violates PodSecurity "restricted:v1.31": ' + ", ".join(probe.PSS_FAILURES))
            return 403, status(403, "Forbidden", obj["metadata"]["name"], "pods", message)
        assert path == f"{probe.NAMESPACES}/{name}"
        if method == "GET":
            if self.namespace:
                return 200, self.namespace
            return 404, status(404, "NotFound", name, "namespaces")
        assert method == "DELETE" and self.namespace is not None
        assert obj["preconditions"] == {
            key: self.namespace["metadata"][key] for key in ("uid", "resourceVersion")
        }
        if self.delete_response:
            return self.delete_response
        removed = self.namespace
        removed["metadata"]["deletionTimestamp"] = "2026-09-10T20:00:00Z"
        self.namespace = None
        return 202, removed
