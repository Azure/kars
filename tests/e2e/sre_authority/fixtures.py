# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import io
import json
import tarfile

from .common import (
    AGENT, CLAIM_VERSION, CONTEXT, FIELD_MANAGER, NAMESPACE_UID, OPERATORS,
    PRIVATE, RUNTIME, SOURCE_NAME, SOURCE_NS, SOURCE_UID, STANDIN, SYSTEM, TENANT,
    enrollment_json, fingerprint, require,
)
from .credential_paths import seed_privacy_gaps

LEGACY_COMMIT = "8b206065608593667a40665b3f48225ef9ce278d"
CONTROL = "e2e-control-rotation"
CONTROL_NS = f"kars-{CONTROL}"
GROUP_BINDING = "e2e-sre-legacy-group"


def namespace_claim(h, source, name):
    namespace = h.get("namespace", name)
    if not namespace:
        namespace = h.create({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name}})
    uid = namespace["metadata"]["uid"]
    h.api("PATCH", f"/api/v1/namespaces/{name}", body={
        "metadata": {"uid": uid, "resourceVersion": namespace["metadata"]["resourceVersion"],
                     "annotations": {CLAIM_VERSION: "v1", SOURCE_NS: SYSTEM,
                                     SOURCE_NAME: source["metadata"]["name"], SOURCE_UID: source["metadata"]["uid"]}},
    }, status=200)
    h.k("patch", "karssandbox", source["metadata"]["name"], "-n", SYSTEM, "--type=merge", "-p",
        json.dumps({"metadata": {"uid": source["metadata"]["uid"],
                               "annotations": {NAMESPACE_UID: uid}}}))
    return uid


def service_account(h, name, namespace):
    return h.create({"apiVersion": "v1", "kind": "ServiceAccount",
                     "metadata": {"name": name, "namespace": namespace}})


def role_binding(h, name, role, account, namespace=None, account_namespace=OPERATORS):
    return h.create({"apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "RoleBinding" if namespace else "ClusterRoleBinding",
        "metadata": {"name": name, **({"namespace": namespace} if namespace else {})},
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": role},
        "subjects": [{"kind": "ServiceAccount", "name": account, "namespace": account_namespace}]})


def prepare_legacy(h):
    require(not h.get("validatingadmissionpolicy", "kars-sre-source-authority"),
            "Legacy fixtures must precede the new SRE admission guards")
    require(not h.get("namespace", RUNTIME), "Disposable cluster contains a pre-existing SRE namespace")
    h.run(["git", "fetch", "--no-tags", "--depth=1", "https://github.com/Azure/kars.git", LEGACY_COMMIT], timeout=90)
    # git archive is binary; invoke separately without decoding tar as text.
    import subprocess
    data = subprocess.run(["git", "archive", LEGACY_COMMIT, "deploy/helm/kars"],
                          cwd=h.root, capture_output=True, timeout=30, check=True).stdout
    destination = h.work / "legacy-chart"
    destination.mkdir(exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as chart:
        for member in chart.getmembers():
            require((member.name.startswith("deploy/helm/kars/")
                     or member.name.rstrip("/") in ("deploy", "deploy/helm", "deploy/helm/kars"))
                    and ".." not in member.name.split("/")
                    and (member.isdir() or member.isfile()), "Unsafe historical chart archive")
        chart.extractall(destination, filter="data")
    h.run(["helm", "install", "kars", str(destination / "deploy/helm/kars"),
           "--kube-context", CONTEXT, "--namespace", SYSTEM, "--create-namespace",
           "--set", "controller.replicas=0", "--set", "controller.image.repository=kars-controller",
           "--set", "controller.image.tag=e2e", "--set", "controller.image.pullPolicy=Never",
           "--set", "inferenceRouter.image.repository=kars-inference-router",
           "--set", "inferenceRouter.image.tag=e2e", "--set", "sandbox.image.repository=kars-sandbox-e2e",
           "--set", "sandbox.image.tag=dev", "--set-string", f"runtimes.hermes.image={STANDIN}",
           "--set", "sre.enabled=false", "--set-string", "inferenceRouter.azure.openai.endpoint=https://e2e-fake.invalid/",
           "--set-string", "foundry.endpoint=https://e2e-fake.invalid/"], timeout=180)
    h.k("wait", "--for=condition=Established", "crd/karssandboxes.kars.azure.com", "--timeout=60s", timeout=70)
    h.run(["helm", "upgrade", "kars", str(destination / "deploy/helm/kars"), "--kube-context", CONTEXT,
           "--namespace", SYSTEM, "--reuse-values", "--set", "sre.enabled=true"], timeout=120)
    source = h.get("karssandbox", "sre", SYSTEM)
    require(source is not None, "Historical chart did not create its source CR")
    h.state["legacy_source_uid"] = source["metadata"]["uid"]
    h.state["legacy_namespace_uid"] = namespace_claim(h, source, RUNTIME)
    h.state["system_uid"] = h.get("namespace", SYSTEM)["metadata"]["uid"]
    h.create({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": OPERATORS}})
    h.create({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": TENANT}})
    service_account(h, "sandbox", RUNTIME)
    for account in ("registrar", "unrelated"):
        service_account(h, account, OPERATORS)
    for account in ("tenant", "normal"):
        service_account(h, account, TENANT)
    h.token_identity("old-agent", RUNTIME, "sandbox")
    h.token_identity("unrelated", OPERATORS, "unrelated")
    binding = h.get("clusterrolebinding", "kars-sre-reader")
    require(binding and binding.get("subjects"), "Historical reader grant is absent")
    unrelated = {"kind": "ServiceAccount", "name": "unrelated", "namespace": OPERATORS}
    h.k("patch", "clusterrolebinding", "kars-sre-reader", "--type=merge", "-p", json.dumps({
        "metadata": {"uid": binding["metadata"]["uid"], "resourceVersion": binding["metadata"]["resourceVersion"]},
        "subjects": binding["subjects"] + [unrelated],
    }))
    h.state["unrelated_subject"] = unrelated
    h.create({"apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "sre", "namespace": RUNTIME, "labels": {
            "kars.azure.com/component": "sandbox", "kars.azure.com/sandbox": "sre",
            "kars.azure.com/parent-namespace": SYSTEM}},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"kars.azure.com/sandbox": "sre"}},
            "template": {"metadata": {"labels": {"kars.azure.com/sandbox": "sre"}},
                "spec": {"serviceAccountName": "sandbox",
                    "securityContext": {"runAsUser": 1000, "runAsNonRoot": True, "seccompProfile": {"type": "RuntimeDefault"}},
                    "containers": [{"name": "agent", "image": STANDIN, "imagePullPolicy": "IfNotPresent",
                                    "command": ["/bin/sh", "-c", "echo legacy-consumer-fixture; sleep infinity"]}]}}}})
    h.k("rollout", "status", "deployment/sre", "-n", RUNTIME, "--timeout=120s", timeout=130)
    seed_control_consumer(h)
    h.state["legacy_review_before_guards"] = enrollment_json(h.cli("authority", "preview"))
    h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
        "metadata": {"name": GROUP_BINDING},
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "kars-sre-reader"},
        "subjects": [{"kind": "Group", "name": "system:serviceaccounts:kars-sre", "apiGroup": "rbac.authorization.k8s.io"}]})
    seed_privacy_gaps(h)
    require(not h.get("validatingadmissionpolicy", "kars-sre-source-authority"),
            "Legacy grants were not seeded before admission")
    # Bootstrap only the new cluster API, not any of its admission policies.
    # This makes kubectl's real discovery usable by authority stage's can-i.
    rendered = h.run(["helm", "template", "kars", str(h.root / "deploy/helm/kars"),
                     "--namespace", SYSTEM, "--show-only", "templates/crd-karssreregistration.yaml"])
    converter = "const y=require('node:module').createRequire(process.cwd()+'/cli/package.json')('yaml'),f=require('fs');console.log(JSON.stringify(y.parse(f.readFileSync(0,'utf8'))));"
    obj = json.loads(h.run(["node", "-e", converter], data=rendered, timeout=20))
    obj["metadata"].setdefault("labels", {})["app.kubernetes.io/managed-by"] = "Helm"
    obj["metadata"].setdefault("annotations", {}).update({
        "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": SYSTEM})
    h.create(obj)
    h.k("wait", "--for=condition=Established", "crd/karssreregistrations.kars.azure.com", "--timeout=60s", timeout=70)
    before = h.get("clusterrolebinding", "kars-sre-reader")
    h.cli("authority", "stage", "--controller-image", "kars-controller:e2e",
          "--router-image", "kars-inference-router:e2e", "--dry-run", timeout=150)
    after = h.get("clusterrolebinding", "kars-sre-reader")
    require(before["metadata"]["uid"] == after["metadata"]["uid"] and before["subjects"] == after["subjects"],
            "Authority stage preview changed legacy grants")
    h.cli("authority", "stage", "--controller-image", "kars-controller:e2e",
          "--router-image", "kars-inference-router:e2e", timeout=180)
    h.save()
    h.passed("Legacy source/grants/consumer seeded using real UIDs before new policies; immutable old chart only, no old binary execution")
    h.passed("Actual authority stage preview/apply retains legacy grants without private issuance")


def seed_control_consumer(h):
    source = h.create({"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": CONTROL, "namespace": SYSTEM},
        "spec": {"runtime": {"kind": "BYO", "byo": {"image": STANDIN, "contractVersion": "v1",
                 "command": ["/bin/sh"], "args": ["-c", "sleep infinity"]}},
                 "sandbox": {"isolation": "standard"}}})
    namespace_uid = namespace_claim(h, source, CONTROL_NS)
    secret = h.create({"apiVersion": "v1", "kind": "Secret",
        "metadata": {"name": "router-services-admin", "namespace": CONTROL_NS,
            "labels": {"app.kubernetes.io/managed-by": "kars-controller"},
            "annotations": {SOURCE_UID: source["metadata"]["uid"], NAMESPACE_UID: namespace_uid}},
        "stringData": {"control-token": "legacy-exposed-control-fixture-" + "x" * 40}})
    h.state["control_digest"] = fingerprint(secret)
    h.state["control_secret_uid"] = secret["metadata"]["uid"]
    h.state["control_source_uid"] = source["metadata"]["uid"]
    h.create({"apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": CONTROL, "namespace": CONTROL_NS, "labels": {
            "kars.azure.com/component": "sandbox", "kars.azure.com/sandbox": CONTROL,
            "kars.azure.com/parent-namespace": SYSTEM}},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"kars.azure.com/sandbox": CONTROL}},
            "template": {"metadata": {"labels": {"kars.azure.com/sandbox": CONTROL}},
                "spec": {"securityContext": {"runAsUser": 1000, "runAsNonRoot": True, "fsGroup": 1000,
                                             "seccompProfile": {"type": "RuntimeDefault"}},
                    "containers": [{"name": "credential-consumer", "image": STANDIN,
                    "command": ["/bin/sh", "-c", "sha256sum /credential/control-token > /proof/cached.sha; echo control-consumer-fixture; sleep infinity"],
                    "volumeMounts": [{"name": "old-control", "mountPath": "/credential", "readOnly": True},
                                     {"name": "proof", "mountPath": "/proof"}]}],
                    "volumes": [{"name": "old-control", "secret": {"secretName": "router-services-admin"}},
                                {"name": "proof", "emptyDir": {}}]}}}}, manager="sre-e2e-fixture")
    # Proven controller ownership of spec, without taking ownership of the
    # fixture's extra cache-observation sidecar or its volumes.
    h.k("apply", "--server-side", f"--field-manager={FIELD_MANAGER}", "-f", "-", data=json.dumps({
        "apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": CONTROL, "namespace": CONTROL_NS},
        "spec": {"replicas": 1}}))
    h.k("rollout", "status", f"deployment/{CONTROL}", "-n", CONTROL_NS, "--timeout=120s", timeout=130)
    h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin", user="old-agent", status=200)
    h.passed("Held legacy agent principal can genuinely read an owned control credential before migration")


def delegate_operators(h):
    h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": {"name": "e2e-sre-review-reader"}, "rules": [
            {"apiGroups": [""], "resources": ["namespaces"], "verbs": ["get", "list"]},
            {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get", "list"]},
            {"apiGroups": ["kars.azure.com"], "resources": ["karssandboxes"], "verbs": ["get", "list"]},
            {"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "verbs": ["get"]},
            {"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["clusterrolebindings", "rolebindings", "clusterroles", "roles"], "verbs": ["get", "list"]},
        ]})
    role_binding(h, "e2e-sre-review-reader", "e2e-sre-review-reader", "registrar")
    role_binding(h, "e2e-sre-registrar", "kars-sre-registrar", "registrar")
    h.token_identity("registrar", OPERATORS, "registrar")
    h.token_identity("tenant", TENANT, "tenant")
    h.token_identity("normal", TENANT, "normal")
    for namespace in (RUNTIME, TENANT):
        h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
            "metadata": {"name": "e2e-sre-admission-probe", "namespace": namespace}, "rules": [
                {"apiGroups": [""], "resources": ["pods", "pods/ephemeralcontainers", "pods/exec", "pods/attach", "pods/portforward", "pods/proxy", "serviceaccounts", "serviceaccounts/token", "secrets"], "verbs": ["create", "patch", "update"]},
                {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list"]},
                {"apiGroups": [""], "resources": ["pods/exec", "pods/attach", "pods/portforward", "pods/proxy"], "verbs": ["get"]},
                {"apiGroups": ["apps"], "resources": ["deployments", "replicasets", "statefulsets", "daemonsets"], "verbs": ["create", "patch", "update"]},
                {"apiGroups": ["batch"], "resources": ["jobs", "cronjobs"], "verbs": ["create"]},
                {"apiGroups": ["kars.azure.com"], "resources": ["karssandboxes", "karssreactions"], "verbs": ["create", "patch", "update"]},
            ]})
        h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": {"name": "e2e-sre-admission-probe", "namespace": namespace},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "e2e-sre-admission-probe"},
            "subjects": [{"kind": "ServiceAccount", "name": "tenant", "namespace": TENANT}]})
    h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": {"name": "e2e-sre-registration-probe"}, "rules": [
            {"apiGroups": ["kars.azure.com"], "resources": ["karssreregistrations"], "verbs": ["create", "patch", "update"]}]})
    role_binding(h, "e2e-sre-registration-probe", "e2e-sre-registration-probe", "tenant", account_namespace=TENANT)
    # Port-forward/exec are normal deployment rights, separate from registrar use.
    h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": {"name": "e2e-sre-registrar-runtime", "namespace": RUNTIME},
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "e2e-sre-admission-probe"},
        "subjects": [{"kind": "ServiceAccount", "name": "registrar", "namespace": OPERATORS}]})
    require(h.k("auth", "can-i", "use", "karssreregistrations.kars.azure.com/canonical",
                user="registrar").strip() == "yes", "Delegated registrar cannot use enrollment")
    result = h.k("auth", "can-i", "create", "clusterrolebindings", user="registrar", expected=None)
    require(result.returncode == 1 and result.stdout.strip() == "no", "Registrar fixture accidentally has cluster-admin-equivalent RBAC")
    h.passed("Explicit registrar delegation works without granting cluster-admin to the registrar or any agent")
