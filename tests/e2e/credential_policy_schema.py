# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded hosted Kind regression for credential authority and policy repairs.

Uses disposable admin expression fixtures, not controller bearer authentication.
Controlled Task status is NOT proof of workload quiescence. No controller, Pod,
public Service or Ingress is installed. Gateway API rules are preserved but their
CRDs are absent and those kinds are not exercised. Secret wire representations
are submitted to the real API, which may normalize stringData before admission.
"""

import base64
import contextlib
import copy
import json
import os
from pathlib import Path
import re
import signal
import time
import uuid

import credential_schema as shared
from sre_authority.registration_schema import CONTEXT, command, kind_proxy, request

CRDS = shared.CRDS
ADMISSION = shared.ADMISSION
LABEL = shared.LABEL
GRANT = shared.CRD
TASK = "karstasks.kars.azure.com"
POLICIES = {
    "store": "kars-credential-enrolled-store-shape",
    "rebind": "kars-credential-rebind-authority",
    "exposure": "kars-no-public-router-exposure",
    "authority": "kars-credential-grant-authority",
}
TEMPLATES = ("credential-store-admission.yaml", "credential-rebind-admission.yaml",
             "admission-no-public-router-exposure.yaml", "crd-karscredentialgrant.yaml",
             "crd-karstask.yaml", "credential-grant-admission.yaml")
STORE_ANNOTATION = "kars.azure.com/credential-store-grant-uid"
PENDING = "kars.azure.com/credential-rebind-pending"
REPORT = "e2e-sre-schema-diag/credential-policy-typechecking.json"
CASES = {"render", "fixtures", "controller-free", "grant-schema", "task-schema",
         "grant-primary", "grant-primary-update", "grant-status",
         "store-enroll", "store-unchanged", "task-status", "task-unchanged",
         "exposure-unpersisted", "cleanup", "deadline", "complete"}
CASES.update(f"{key}-policy" for key in POLICIES)
CASES.update(f"store-{representation}-{key}" for representation in
             ("data", "string", "mixed-data", "mixed-string")
             for key in ("allowed", "path", "control"))
CASES.update(("store-grant-uid", "store-positive-after"))
CASES.update(f"rebind-{case}" for case in
             ("absent", "null", "digest", "wrong-phase", "stale-generation", "ready-true",
              "positive-after"))
CASES.update(f"exposure-{case}" for case in
             ("cluster-ip", "load-balancer", "node-port", "ingress", "private-cidr",
              "ipv4-public", "ipv6-public", "non-strict", "positive-after"))
CATEGORIES = {"accepted", "intended-denial", "cleaned", "passed", "failed",
              "type-warning", "native-error", "unexpected-acceptance"}


class Failure(RuntimeError):
    def __init__(self, case, code=0, category="failed"):
        self.case = case if case in CASES else "complete"
        self.code = code if type(code) is int and 100 <= code <= 599 else 0
        self.category = category if category in CATEGORIES else "failed"
        super().__init__("Credential policy native API proof failed")


def require(value, case, code=0, category="failed"):
    if not value:
        raise Failure(case, code, category)


def evidence(case, code, category):
    require(case in CASES and category in CATEGORIES and type(code) is int
            and (code == 0 or 100 <= code <= 599), "complete")
    item = {"case": case, "httpStatus": code, "category": category}
    print("CREDENTIAL-POLICY-SCHEMA " + json.dumps(item, sort_keys=True), flush=True)
    return item


@contextlib.contextmanager
def time_limit(seconds, case):
    """Unix CI ceiling, including blocked subprocesses/HTTP; cleanup gets 45s."""
    started = time.monotonic()
    previous_handler = signal.getsignal(signal.SIGALRM)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)

    def expired(_signal, _frame):
        raise Failure(case)

    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
        if previous_timer[0]:
            remaining = max(0.001, previous_timer[0] - (time.monotonic() - started))
            signal.setitimer(signal.ITIMER_REAL, remaining, previous_timer[1])


def select_shipped(raw):
    objects = shared.decode_documents(raw)
    crds = []
    for name, kind, plural in ((GRANT, "KarsCredentialGrant", "karscredentialgrants"),
                               (TASK, "KarsTask", "karstasks")):
        obj = objects.get(("CustomResourceDefinition", name), {})
        spec = obj.get("spec", {})
        require(obj.get("apiVersion") == "apiextensions.k8s.io/v1"
                and spec.get("group") == "kars.azure.com" and spec.get("scope") == "Namespaced"
                and spec.get("names", {}).get("kind") == kind
                and spec["names"].get("plural") == plural, "render")
        crds.append(obj)
    result = {}
    for key, name in POLICIES.items():
        policy = objects.get(("ValidatingAdmissionPolicy", name), {})
        bindings = [obj for (kind, _), obj in objects.items()
                    if kind == "ValidatingAdmissionPolicyBinding"
                    and obj.get("spec", {}).get("policyName") == name]
        spec = policy.get("spec", {})
        validations = spec.get("validations", [])
        require(policy.get("apiVersion") == "admissionregistration.k8s.io/v1"
                and spec.get("failurePolicy") == "Fail" and len(bindings) == 1
                and len(validations) == {"store": 1, "rebind": 3, "exposure": 3, "authority": 4}[key]
                and all(isinstance(v.get("expression"), str) and v["expression"].strip()
                        and isinstance(v.get("message"), str) and v["message"]
                        and v.get("reason", "Invalid") in ("Invalid", "Forbidden")
                        for v in validations)
                and spec.get("matchConstraints", {}).get("resourceRules")
                and "Deny" in bindings[0]["spec"].get("validationActions", []), "render")
        result[key] = (policy, bindings[0])
    return crds, result


def render(root, namespace, version):
    args = ["helm", "template", "credential-policy-proof", str(root / "deploy/helm/kars"),
            "--namespace", namespace, "--kube-version", version]
    for template in TEMPLATES:
        args += ["--show-only", "templates/" + template]
    yaml = command("credential-policy-render", args, root=root)
    raw = command("credential-policy-convert", [
        "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
        "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
    ], root=root, data=yaml)
    return select_shipped(raw)


class Fixtures(shared.Owned):
    def create(self, path, obj, case="fixtures"):
        try:
            return super().create(path, obj)
        except shared.Failure as error:
            raise Failure(case, error.code, "native-error") from None

    def cleanup(self):
        # A replaced child must also protect its containing namespace/CRD from
        # cascading deletion. The shared helper UID-fences every GET and DELETE.
        leaves, parents = shared.Owned(self.port), shared.Owned(self.port)
        for resource in self.resources:
            parent = resource[0].startswith(CRDS + "/") or (
                resource[0].startswith("/api/v1/namespaces/") and resource[0].count("/") == 4)
            (parents if parent else leaves).resources.append(resource)
        try:
            leaves.cleanup()
            parents.cleanup()
        except shared.Failure as error:
            raise Failure("cleanup", error.code) from None


def wait_for(probe, predicate, case, seconds=30):
    deadline, code = time.monotonic() + seconds, 0
    while time.monotonic() < deadline:
        code, body = probe()
        if predicate(code, body):
            return code, body
        time.sleep(0.25)
    raise Failure(case, code)


def unchanged(port, path, original, case):
    code, body = request(port, "GET", path)
    require(code == 200 and body == original, case, code)


def accepted(code, body, fixture, method):
    if code != (200 if method == "PUT" else 201) or not isinstance(body, dict):
        return False
    meta, expected = body.get("metadata", {}), fixture["metadata"]
    if not isinstance(meta, dict):
        return False
    if fixture["kind"] == "Secret":
        data = {**fixture.get("data", {}), **{
            key: base64.b64encode(value.encode()).decode()
            for key, value in fixture.get("stringData", {}).items()}}
        if body.get("type") != fixture.get("type") or body.get("data", {}) != data:
            return False
    if fixture["kind"] == "KarsTask" and (
            body.get("spec") != fixture.get("spec") or body.get("status") != fixture.get("status")):
        return False
    return (body.get("apiVersion") == fixture["apiVersion"] and body.get("kind") == fixture["kind"]
            and all(meta.get(key) == expected[key] for key in ("name", "namespace"))
            and (method != "PUT" or meta.get("uid") == expected["uid"])
            and meta.get("annotations", {}) == expected.get("annotations", {}))


def dry_run(port, method, path, fixture, case, results, policy=None, validation=None, warm=False):
    require(method in ("POST", "PUT") and "?" not in path, case)
    name = policy["metadata"]["name"] if policy else None

    def check(code, body):
        good = accepted(code, body, fixture, method)
        if validation is None:
            require(good, case, code, "native-error")
        else:
            denied = shared.intended_denial(code, body, name, name, validation,
                                            fixture["metadata"]["name"])
            if warm and good:
                return False
            require(denied, case, code, "unexpected-acceptance" if good else "native-error")
        return True

    call = lambda: request(port, method, path + "?dryRun=All", fixture)
    if warm:
        # Only acceptance may be retried for informer propagation. An arbitrary
        # 403/422, CRD validation failure or CEL runtime error fails immediately.
        code, _ = wait_for(call, check, case)
    else:
        code, body = call()
        check(code, body)
    if results is not None:
        results.append(evidence(case, code, "intended-denial" if validation else "accepted"))


def no_custom_controllers(port):
    code, body = request(port, "GET", "/api/v1/pods?limit=100")
    require(code == 200 and isinstance(body, dict) and body.get("kind") == "PodList"
            and not body.get("metadata", {}).get("continue")
            and isinstance(body.get("items"), list), "controller-free", code)
    # Only the pinned Kind cluster's own system pods may exist. Do not print any
    # Pod data; this is a fail-closed fixture safety check, not diagnostics.
    for pod in body["items"]:
        meta = pod.get("metadata", {})
        ns, name = meta.get("namespace"), meta.get("name", "")
        system = ns == "kube-system" and re.fullmatch(
            r"(?:(?:etcd|kube-apiserver|kube-controller-manager|kube-scheduler)-kars-e2e-control-plane"
            r"|(?:coredns|kindnet|kube-proxy)-[a-z0-9-]+)", name)
        storage = ns == "local-path-storage" and re.fullmatch(
            r"local-path-provisioner-[a-z0-9-]+", name)
        require(system or storage, "controller-free", code)


def install(port, owned, crds, shipped, namespace, token, results):
    for crd in crds:
        case = "grant-schema" if crd["metadata"]["name"] == GRANT else "task-schema"
        installed = owned.create(CRDS, crd, case)

        def established(code, obj):
            require(code == 200 and isinstance(obj, dict)
                    and obj.get("metadata", {}).get("uid") == installed["metadata"]["uid"],
                    case, code, "native-error")
            return any(c.get("type") == "Established" and c.get("status") == "True"
                       for c in obj.get("status", {}).get("conditions", []))

        wait_for(lambda: request(port, "GET", CRDS + "/" + crd["metadata"]["name"]),
                 established, case)
        results.append(evidence(case, 201, "accepted"))
    policies = {}
    for key, (source, binding) in shipped.items():
        policy, binding = shared.scoped(source, binding, token, namespace, key)
        case = key + "-policy"
        installed = owned.create(ADMISSION + "/validatingadmissionpolicies", policy, case)
        name = installed["metadata"]["name"]

        def typed(code, obj):
            require(code == 200 and isinstance(obj, dict)
                    and obj.get("metadata", {}).get("uid") == installed["metadata"]["uid"],
                    case, code, "native-error")
            status = obj.get("status", {})
            if (status.get("observedGeneration") != installed["metadata"]["generation"]
                    or obj["metadata"].get("generation") != installed["metadata"]["generation"]
                    or not isinstance(status.get("typeChecking"), dict)):
                return False
            require(status["typeChecking"].get("expressionWarnings", []) == [],
                    case, code, "type-warning")
            return True

        code, _ = wait_for(lambda: request(port, "GET",
                                          ADMISSION + "/validatingadmissionpolicies/" + name),
                           typed, case)
        owned.create(ADMISSION + "/validatingadmissionpolicybindings", binding, case)
        policies[key] = policy
        results.append(evidence(case, code, "accepted"))
    return policies


def store_payload(stored, representation, key):
    obj = copy.deepcopy(stored)
    obj.pop("data", None)
    obj.pop("stringData", None)
    encoded = base64.b64encode(b"public-admission-fixture-only").decode()
    obj["data" if representation.endswith("data") else "stringData"] = {
        key: encoded if representation.endswith("data") else "public-admission-fixture-only"}
    if representation.startswith("mixed-"):
        other = "stringData" if representation.endswith("data") else "data"
        obj[other] = {"FOUNDRY_API_KEY": "public-admission-fixture-only"
                      if other == "stringData" else encoded}
    return obj


def prove_store(port, owned, namespace, namespace_uid, policy, results):
    path = f"/api/v1/namespaces/{namespace}/secrets"
    stored = owned.create(path, {"apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {"name": "kars-foundry-credentials", "namespace": namespace}})
    grant_path = f"/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/karscredentialgrants"
    grant = owned.create(grant_path, {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
        "metadata": {"name": "workspace", "namespace": namespace},
        "spec": {"workspaceUid": namespace_uid, "enabled": True, "writers": [],
                 "integrationStores": [{"purpose": "foundry", "secret": {
                     "name": stored["metadata"]["name"],
                     "uid": stored["metadata"]["uid"]}}]}}, "grant-primary")
    results.append(evidence("grant-primary", 201, "accepted"))
    primary = copy.deepcopy(grant)
    primary["metadata"]["annotations"] = {"kars.azure.com/admission-proof": "fixture"}
    code, grant = request(port, "PUT", grant_path + "/workspace", primary)
    require(accepted(code, grant, primary, "PUT") and grant.get("spec") == primary["spec"],
            "grant-primary-update", code, "native-error")
    results.append(evidence("grant-primary-update", code, "accepted"))
    status = copy.deepcopy(grant)
    status["status"] = {"phase": "AdmissionFixture",
                        "observedGeneration": grant["metadata"]["generation"]}
    code, grant = request(port, "PUT", grant_path + "/workspace/status", status)
    require(accepted(code, grant, status, "PUT") and grant.get("status") == status["status"]
            and grant.get("spec") == status["spec"], "grant-status", code, "native-error")
    results.append(evidence("grant-status", code, "accepted"))
    path += "/" + stored["metadata"]["name"]
    enrolled = copy.deepcopy(stored)
    enrolled["metadata"]["annotations"] = {STORE_ANNOTATION: grant["metadata"]["uid"]}
    code, stored = request(port, "PUT", path, enrolled)
    require(accepted(code, stored, enrolled, "PUT"), "store-enroll", code, "native-error")
    results.append(evidence("store-enroll", code, "accepted"))
    validation = policy["spec"]["validations"][0]
    bad = store_payload(stored, "data", "PATH")
    dry_run(port, "PUT", path, bad, "store-data-path", None, policy, validation, warm=True)
    for key_name, key in (("allowed", "FOUNDRY_API_KEY"), ("path", "PATH"), ("control", "NODE_OPTIONS")):
        for representation in ("data", "string", "mixed-data", "mixed-string"):
            obj = store_payload(stored, representation, key)
            dry_run(port, "PUT", path, obj, f"store-{representation}-{key_name}", results,
                    policy, None if key_name == "allowed" else validation)
            unchanged(port, path, stored, "store-unchanged")
    bad = store_payload(stored, "data", "FOUNDRY_API_KEY")
    bad["metadata"]["annotations"][STORE_ANNOTATION] = "not-the-enrolled-grant-uid"
    dry_run(port, "PUT", path, bad, "store-grant-uid", results, policy, validation)
    dry_run(port, "PUT", path, store_payload(stored, "data", "FOUNDRY_API_KEY"),
            "store-positive-after", results)
    unchanged(port, path, stored, "store-unchanged")
    results.append(evidence("store-unchanged", 200, "passed"))


def paused_status(task, case):
    generation = task["metadata"]["generation"]
    status = {"executionPhase": "CredentialsPaused", "observedGeneration": generation,
              "conditions": [{"type": "Ready", "status": "False", "reason": "AdmissionFixture",
                              "message": "Admin expression fixture, not workload quiescence",
                              "observedGeneration": generation,
                              "lastTransitionTime": "2026-01-01T00:00:00Z"}]}
    if case == "null":
        status["envelopeDigest"] = None
    elif case == "digest":
        status["envelopeDigest"] = "sha256:" + "0" * 64
    elif case == "wrong-phase":
        status["executionPhase"] = "Running"
    elif case == "stale-generation":
        status["observedGeneration"] = generation - 1
    elif case == "ready-true":
        status["conditions"][0]["status"] = "True"
    return status


def prove_rebind(port, owned, namespace, policy, results):
    no_custom_controllers(port)
    path = f"/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/karstasks"
    task = owned.create(path, {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
        "metadata": {"name": "credential-pause-expression", "namespace": namespace,
                     "annotations": {PENDING: "true"}},
        "spec": {"objective": "Admin admission fixture only; never execute a workload",
                 "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 0},
                 "execution": {"launch": True}}})
    path += "/" + task["metadata"]["name"]
    validation = policy["spec"]["validations"][1]
    # Warm up with the non-null negative, then verify positives on both sides of
    # all final negatives. No resume annotation change is ever persisted.
    for iteration, case in enumerate(("digest", "absent", "null", "digest", "wrong-phase",
                                      "stale-generation", "ready-true", "positive-after")):
        obj = copy.deepcopy(task)
        obj["status"] = paused_status(task, case)
        code, current = request(port, "PUT", path + "/status", obj)
        require(accepted(code, current, obj, "PUT")
                and current.get("status") == obj["status"]
                and current.get("spec") == task["spec"]
                and current["metadata"]["generation"] == task["metadata"]["generation"],
                "task-status", code, "native-error")
        task = current
        resumed = copy.deepcopy(task)
        resumed["metadata"]["annotations"].pop(PENDING)
        warm = iteration == 0
        negative = case in ("digest", "wrong-phase", "stale-generation", "ready-true")
        dry_run(port, "PUT", path, resumed, "rebind-" + case,
                None if warm else results, policy, validation if negative else None, warm=warm)
        unchanged(port, path, task, "task-unchanged")
    results.append(evidence("task-unchanged", 200, "passed"))


def exposure_fixtures(namespace, other):
    service = {"apiVersion": "v1", "kind": "Service",
               "metadata": {"name": "exposure-expression", "namespace": namespace},
               "spec": {"type": "ClusterIP", "ports": [{"port": 80, "targetPort": 8080}]}}
    yield "cluster-ip", "services", service, None
    for case, kind in (("load-balancer", "LoadBalancer"), ("node-port", "NodePort"),
                       ("non-strict", "LoadBalancer")):
        obj = copy.deepcopy(service)
        obj["spec"]["type"] = kind
        if case == "non-strict":
            obj["metadata"]["namespace"] = other
        yield case, "services", obj, None if case == "non-strict" else 0
    yield "ingress", "ingresses", {
        "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
        "metadata": {"name": "exposure-expression", "namespace": namespace},
        "spec": {"defaultBackend": {"service": {"name": "never-created", "port": {"number": 80}}}}}, 1
    for case, cidr in (("private-cidr", "10.42.0.0/16"), ("ipv4-public", "0.0.0.0/0"),
                       ("ipv6-public", "::/0")):
        yield case, "networkpolicies", {
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": {"name": "exposure-expression", "namespace": namespace},
            "spec": {"podSelector": {}, "policyTypes": ["Ingress"],
                     "ingress": [{"from": [{"ipBlock": {"cidr": cidr}}]}]}}, (
                         None if case == "private-cidr" else 2)
    yield "positive-after", "services", service, None


def prove_exposure(port, namespace, other, policy, results):
    fixtures = list(exposure_fixtures(namespace, other))
    for iteration, (case, resource, obj, index) in enumerate([fixtures[1], *fixtures]):
        ns = obj["metadata"]["namespace"]
        prefix = "/api/v1" if resource == "services" else "/apis/networking.k8s.io/v1"
        path = f"{prefix}/namespaces/{ns}/{resource}"
        warm = iteration == 0
        validation = None if index is None else policy["spec"]["validations"][index]
        dry_run(port, "POST", path, obj, "exposure-" + case, None if warm else results,
                policy, validation, warm=warm)
        code, _ = request(port, "GET", path + "/" + obj["metadata"]["name"])
        require(code == 404, "exposure-unpersisted", code)
    results.append(evidence("exposure-unpersisted", 404, "passed"))


def exercise(root, port, version, token, results):
    namespace, other = "kars-policy-cel-" + token, "kars-policy-cel-" + token + "-normal"
    crds, shipped = render(root, namespace, version)
    owned = Fixtures(port)
    try:
        no_custom_controllers(port)
        results.append(evidence("controller-free", 200, "passed"))
        namespaces = []
        for name in (namespace, other):
            labels = {LABEL: token}
            if name == namespace:
                labels["kars.azure.com/isolated"] = "strict"
            namespaces.append(owned.create("/api/v1/namespaces", {
                "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name, "labels": labels}}))
        policies = install(port, owned, crds, shipped, namespace, token, results)
        prove_store(port, owned, namespace, namespaces[0]["metadata"]["uid"], policies["store"], results)
        prove_rebind(port, owned, namespace, policies["rebind"], results)
        prove_exposure(port, namespace, other, policies["exposure"], results)
        no_custom_controllers(port)
    finally:
        with time_limit(45, "cleanup"):
            owned.cleanup()
        results.append(evidence("cleanup", 0, "cleaned"))


def main(root):
    results, exit_code = [], 1
    try:
        # 150s attempt deadline; cleanup has a separate 45s safety window.
        with time_limit(150, "deadline"), kind_proxy(root) as (port, version):
            exercise(root, port, version["gitVersion"], uuid.uuid4().hex[:12], results)
        results.append(evidence("complete", 0, "passed"))
        exit_code = 0
    except Failure as error:
        results.append(evidence(error.case, error.code, error.category))
    except (OSError, RuntimeError, ValueError):
        # Fail closed without a traceback or general API/credential body logger.
        results.append(evidence("complete", 0, "failed"))
    try:
        require(len(results) <= 64, "complete")
        data = json.dumps({"cases": results}, indent=2) + "\n"
        require(len(data) <= 16384, "complete")
        path = root / REPORT
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_text(data)
    except (OSError, Failure):
        evidence("complete", 0, "failed")
        exit_code = 1
    return exit_code


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main(Path(__file__).resolve().parents[2]))
