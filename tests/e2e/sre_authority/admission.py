# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

from .common import POLICIES, PRIVATE, REGISTRATION, RUNTIME, STANDIN, TENANT, assert_denial, require


def policies_ready(h):
    def checked():
        for suffix in POLICIES:
            name = f"kars-sre-{suffix}"
            policy = h.get("validatingadmissionpolicy", name)
            binding = h.get("validatingadmissionpolicybinding", name)
            if not policy or not binding:
                return False
            status = policy.get("status", {})
            if status.get("observedGeneration") != policy["metadata"]["generation"] or "typeChecking" not in status:
                return False
            require(not status["typeChecking"].get("expressionWarnings"), f"CEL type warnings in {name}")
            require(policy["spec"].get("failurePolicy") == "Fail", f"{name} does not fail closed")
            require(binding["spec"]["policyName"] == name and "Deny" in binding["spec"]["validationActions"],
                    f"{name} lacks a matching enforcing binding")
        return True
    h.poll("all observed/enforcing SRE CEL policies", checked, seconds=90)
    h.passed(f"All {len(POLICIES)} real API admission policies are observed, CEL-type-checked without warnings, and Deny-bound")


def pod_spec(private=True):
    spec = {"serviceAccountName": "sandbox", "automountServiceAccountToken": False,
        "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
        "containers": [{"name": "probe", "image": STANDIN, "command": ["/bin/sh", "-c", "sleep infinity"],
            "securityContext": {"allowPrivilegeEscalation": False, "capabilities": {"drop": ["ALL"]}}}]}
    if private:
        spec["volumes"] = [{"name": "private", "secret": {"secretName": PRIVATE}}]
        spec["containers"][0]["volumeMounts"] = [{"name": "private", "mountPath": "/private", "readOnly": True}]
    return spec


def reserved_source_probe():
    return {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": "sre", "namespace": TENANT},
        "spec": {"runtime": {"kind": "BYO", "byo": {"image": STANDIN, "contractVersion": "v1"}},
                 "inferenceRef": {"name": "sre-inference"},
                 "sandbox": {"isolation": "standard"}}}


def admission_cases(h, enrollment):
    registration = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
                    "metadata": {"name": "canonical"}, "spec": enrollment}
    # Normal SA has no cluster enrollment permission. Tenant probe has CREATE
    # but deliberately lacks USE, so the second check exercises actual CEL.
    assert_denial(h.api("POST", REGISTRATION.rsplit("/", 1)[0] + "?dryRun=All",
                       body=registration, user="normal"), "ordinary registration RBAC")
    assert_denial(h.api("POST", REGISTRATION.rsplit("/", 1)[0] + "?dryRun=All",
                       body=registration, user="tenant"), "registration CEL", "kars-sre-registration-authority")
    review = h.api("POST", "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews", user="normal", body={
        "apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
        "spec": {"resourceAttributes": {"group": "kars.azure.com", "resource": "karssreregistrations",
                                      "name": "canonical", "verb": "use"}}}, status=201).json()
    require(review["status"]["allowed"] is False, "Ordinary SA unexpectedly has registrar use")
    h.passed("Real RBAC and CEL independently deny ordinary/tenant registration and registrar use")

    source = reserved_source_probe()
    assert_denial(h.api("POST", f"/apis/kars.azure.com/v1alpha1/namespaces/{TENANT}/karssandboxes?dryRun=All",
                       body=source, user="tenant"), "reserved source", "kars-sre-source-authority")
    source["metadata"]["name"] = "not-canonical"
    source["metadata"]["labels"] = {"kars.azure.com/role": "sre"}
    assert_denial(h.api("POST", f"/apis/kars.azure.com/v1alpha1/namespaces/{TENANT}/karssandboxes?dryRun=All",
                       body=source, user="tenant"), "reserved SRE role", "kars-sre-source-authority")
    h.passed("Namespace authority cannot create a reserved-name or SRE-labelled source")

    sa = {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": "sre-api-router", "namespace": TENANT}}
    assert_denial(h.api("POST", f"/api/v1/namespaces/{TENANT}/serviceaccounts?dryRun=All",
                       body=sa, user="tenant"), "reserved ServiceAccount", "kars-sre-private-identity")
    secret = {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": PRIVATE, "namespace": RUNTIME},
              "stringData": {"kube-token": "not-a-credential"}}
    assert_denial(h.api("POST", f"/api/v1/namespaces/{RUNTIME}/secrets?dryRun=All",
                       body=secret, user="tenant"), "reserved material", "kars-sre-private-material")
    h.passed("Real admission denies reserved identities and private material despite namespaced create permission")

    # An otherwise valid ordinary Pod is admitted in server-side dry-run;
    # private variants must be denied by the intended policy, not schema/RBAC.
    ordinary = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "e2e-ordinary", "namespace": RUNTIME}, "spec": pod_spec(False)}
    h.api("POST", f"/api/v1/namespaces/{RUNTIME}/pods?dryRun=All", body=ordinary, user="tenant", status=201)
    private = {**ordinary, "metadata": {"name": "e2e-private", "namespace": RUNTIME}, "spec": pod_spec()}
    assert_denial(h.api("POST", f"/api/v1/namespaces/{RUNTIME}/pods?dryRun=All",
                       body=private, user="tenant"), "private Pod mount", "kars-sre-private-mounts")
    env_only = pod_spec(False)
    env_only["containers"][0]["envFrom"] = [{"secretRef": {"name": PRIVATE}}]
    assert_denial(h.api("POST", f"/api/v1/namespaces/{RUNTIME}/pods?dryRun=All",
                       body={**private, "spec": env_only}, user="tenant"), "private envFrom", "kars-sre-private-mounts")
    h.passed("A valid ordinary Pod is accepted; private volume/envFrom laundering is denied")

    for kind, group, plural in [("Deployment", "apps/v1", "deployments"), ("ReplicaSet", "apps/v1", "replicasets"),
                                 ("Job", "batch/v1", "jobs"), ("CronJob", "batch/v1", "cronjobs")]:
        template = {"metadata": {"labels": {"app": "e2e-private-template"}}, "spec": pod_spec()}
        if kind in ("Job", "CronJob"):
            template["spec"]["restartPolicy"] = "Never"
        spec = {"template": template}
        if kind in ("Deployment", "ReplicaSet"):
            spec.update({"replicas": 1, "selector": {"matchLabels": template["metadata"]["labels"]}})
        if kind == "CronJob":
            spec = {"schedule": "0 * * * *", "jobTemplate": {"spec": spec}}
        obj = {"apiVersion": group, "kind": kind, "metadata": {"name": "e2e-private-template", "namespace": RUNTIME}, "spec": spec}
        policy = "kars-sre-private-cronjobs" if kind == "CronJob" else "kars-sre-private-workloads"
        assert_denial(h.api("POST", f"/apis/{group}/namespaces/{RUNTIME}/{plural}?dryRun=All",
                           body=obj, user="tenant"), f"{kind} private template", policy)
        h.passed(f"Real {kind} admission prevents private-material laundering through workload controllers")


def runtime_denials(h, pod):
    response = h.api("POST", f"/api/v1/namespaces/{RUNTIME}/serviceaccounts/sre-api-router/token",
                    body={"apiVersion": "authentication.k8s.io/v1", "kind": "TokenRequest",
                          "spec": {"audiences": [], "expirationSeconds": 600}}, user="tenant")
    assert_denial(response, "private TokenRequest", "kars-sre-private-identity")
    for subresource, args in [
        ("exec", ["exec", "-n", RUNTIME, pod, "-c", "agent", "--", "/bin/true"]),
        ("attach", ["attach", "-n", RUNTIME, pod, "-c", "agent"]),
        ("portforward", ["port-forward", "-n", RUNTIME, f"pod/{pod}", "19446:9446"]),
    ]:
        result = h.k(*args, user="tenant", expected=None, timeout=25)
        require(result.returncode != 0 and "kars-sre-private-connect" in result.stderr
                and "forbidden" in result.stderr.lower(), f"{subresource} did not receive the intended connect-admission denial")
    assert_denial(h.api("GET", f"/api/v1/namespaces/{RUNTIME}/pods/{pod}:9446/proxy/api/v1/namespaces",
                       user="tenant"), "Pod proxy connect", "kars-sre-private-connect")
    h.passed("Tenant TokenRequests and actual exec/attach/port-forward/proxy attempts receive intended admission denials")
