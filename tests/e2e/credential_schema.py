# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Hosted Kind proof of shipped credential CEL against native namespace UIDs.

Only fixture policy/binding names and namespace selectors are changed. The
source-writes binding pins one real grant across two owned namespaces to vary
only namespaceObject's actual UID, without racing grant generation/readiness.
This is admission-expression evidence, not bearer authentication, grant lifecycle,
workload execution, network isolation, or private Bridge qualification.
"""

import copy
import json
import os
from pathlib import Path
import re
import time
import uuid
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener

from sre_authority.registration_schema import CONTEXT, command, kind_proxy, request

CRD = "karscredentialgrants.kars.azure.com"
CRDS = "/apis/apiextensions.k8s.io/v1/customresourcedefinitions"
ADMISSION = "/apis/admissionregistration.k8s.io/v1"
RBAC = "/apis/rbac.authorization.k8s.io/v1"
LABEL = "kars.azure.com/credential-cel-proof"
POLICIES = {
    "source-writes": "kars-credential-source-writes",
    "material": "kars-observation-privacy-material",
    "pods": "kars-observation-privacy-pods",
}
TEMPLATES = ("credential-grant-admission.yaml", "observation-privacy.yaml",
             "crd-karscredentialgrant.yaml")
CASES = {"render", "fixtures", "grant-schema", "grant-canonical", "grant-noncanonical",
         "identity", "rbac", "cleanup", "complete"}
CASES.update(f"{key}-{suffix}" for key in POLICIES
             for suffix in ("policy", "positive", "negative", "positive-after"))
CATEGORIES = {"accepted", "intended-denial", "cleaned", "failed", "passed"}
REPORT = "e2e-sre-schema-diag/credential-namespace-uid.json"


class Failure(RuntimeError):
    def __init__(self, case, code=0):
        self.case = case if case in CASES else "complete"
        self.code = code if isinstance(code, int) and 100 <= code <= 599 else 0
        super().__init__("Credential native API proof failed")


def require(value, case, code=0):
    if not value:
        raise Failure(case, code)


def evidence(case, code, category):
    require(case in CASES and category in CATEGORIES and type(code) is int
            and (code == 0 or 100 <= code <= 599), "complete")
    item = {"case": case, "httpStatus": code, "category": category}
    print("CREDENTIAL-NAMESPACE-UID " + json.dumps(item, sort_keys=True), flush=True)
    return item


def decode_documents(raw):
    decoder, objects = json.JSONDecoder(), {}
    while raw.strip():
        obj, end = decoder.raw_decode(raw.lstrip())
        raw = raw.lstrip()[end:]
        for value in obj.get("items", []) if obj.get("kind") == "List" else [obj]:
            key = (value.get("kind"), value.get("metadata", {}).get("name"))
            require(key not in objects, "render")
            objects[key] = value
    return objects


def select_shipped(raw):
    objects = decode_documents(raw)
    crd = objects.get(("CustomResourceDefinition", CRD), {})
    require(crd.get("spec", {}).get("names", {}).get("kind") == "KarsCredentialGrant"
            and crd["spec"].get("scope") == "Namespaced", "render")
    result = {}
    for key, name in POLICIES.items():
        policy = objects.get(("ValidatingAdmissionPolicy", name), {})
        binding = objects.get(("ValidatingAdmissionPolicyBinding", name), {})
        spec = policy.get("spec", {})
        validations = [v for v in spec.get("validations", [])
                       if "namespaceObject" in v.get("expression", "")]
        require(spec.get("failurePolicy") == "Fail" and len(validations) == 1
                and isinstance(validations[0].get("message"), str)
                and binding.get("spec", {}).get("policyName") == name
                and "Deny" in binding["spec"].get("validationActions", []), "render")
        result[key] = (policy, binding, validations[0])
    return crd, result


def render(root, namespace, version):
    args = ["helm", "template", "credential-cel-proof", str(root / "deploy/helm/kars"),
            "--namespace", namespace, "--kube-version", version]
    for template in TEMPLATES:
        args += ["--show-only", "templates/" + template]
    yaml = command("credential-render", args, root=root)
    raw = command("credential-convert", [
        "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
        "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
    ], root=root, data=yaml)
    return select_shipped(raw)


def scoped(policy, binding, token, namespace, key):
    policy, binding = copy.deepcopy(policy), copy.deepcopy(binding)
    name = f"credential-cel-{token}-{key}"
    for obj in (policy, binding):
        obj["metadata"] = {"name": name, "labels": {LABEL: token}}
    selector = policy["spec"]["matchConstraints"].setdefault("namespaceSelector", {})
    selector.setdefault("matchExpressions", []).append(
        {"key": LABEL, "operator": "In", "values": [token]})
    binding["spec"]["policyName"] = name
    if key == "source-writes":
        require(binding["spec"].get("paramRef", {}).get("name") == "workspace"
                and not binding["spec"]["paramRef"].get("namespace"), "render")
        binding["spec"]["paramRef"]["namespace"] = namespace
    return policy, binding


def as_actor(port, path, obj, actor):
    # The guarded Kind proxy authenticates the disposable admin. Impersonation
    # supplies the UID read from a real ServiceAccount; no token is minted/read.
    require(type(port) is int and 0 < port < 65536 and path.startswith("/"), "identity")
    username, uid = actor
    require(re.fullmatch(r"system:serviceaccount:kars-cel-[a-f0-9-]+:[a-z-]+", username)
            and re.fullmatch(r"[A-Za-z0-9-]{1,128}", uid), "identity")
    req = Request(f"http://127.0.0.1:{port}{path}", data=json.dumps(obj).encode(), method="POST",
                  headers={"Content-Type": "application/json", "Accept": "application/json",
                           "Impersonate-User": username, "Impersonate-Uid": uid,
                           "Impersonate-Group": "system:authenticated"})
    try:
        response = build_opener(ProxyHandler({})).open(req, timeout=15)
    except HTTPError as error:
        response = error
    with response:
        code, raw = response.code, response.read(1024 * 1024)
    try:
        return code, json.loads(raw)
    except (ValueError, TypeError):
        return code, None


def allowed(code, body, fixture):
    if code != 201 or not isinstance(body, dict) or body.get("kind") != fixture["kind"]:
        return False
    metadata = body.get("metadata", {})
    expected = fixture["metadata"]
    if not isinstance(metadata, dict):
        return False
    annotations = metadata.get("annotations", {})
    return (metadata.get("name") == expected["name"]
            and metadata.get("namespace") == expected["namespace"]
            and isinstance(annotations, dict) and all(annotations.get(k) == v
                    for k, v in expected.get("annotations", {}).items()))


def intended_denial(code, body, policy, binding, validation, name):
    reason = validation.get("reason", "Invalid")
    expected = {"Forbidden": 403, "Invalid": 422}.get(reason)
    if (expected is None or code != expected or not isinstance(body, dict) or body.get("kind") != "Status"
            or body.get("status") != "Failure" or body.get("reason") != reason):
        return False
    details = body.get("details")
    if not isinstance(details, dict) or details.get("name") != name:
        return False
    message = (f"ValidatingAdmissionPolicy '{policy}' with binding '{binding}' denied request: "
               + validation["message"])
    causes = details.get("causes")
    return isinstance(causes, list) and any(
        isinstance(cause, dict) and cause.get("message") == message for cause in causes)


def wait_for(probe, predicate, case, seconds=40):
    deadline = time.monotonic() + seconds
    code = 0
    while time.monotonic() < deadline:
        code, body = probe()
        if predicate(code, body):
            return code, body
        time.sleep(0.25)
    raise Failure(case, code)


class Owned:
    def __init__(self, port):
        self.port, self.resources = port, []

    def create(self, path, obj, case="fixtures"):
        code, body = request(self.port, "POST", path, obj)
        metadata = body.get("metadata", {}) if isinstance(body, dict) else {}
        require(code == 201 and isinstance(body, dict) and isinstance(metadata, dict)
                and body.get("kind") == obj["kind"]
                and metadata.get("name") == obj["metadata"]["name"]
                and metadata.get("namespace") == obj["metadata"].get("namespace")
                and metadata.get("uid") and metadata.get("resourceVersion"), case, code)
        self.resources.append((path + "/" + metadata["name"], metadata["uid"]))
        return body

    def cleanup(self):
        failed, deadline = False, time.monotonic() + 45
        for path, uid in reversed(self.resources):
            if time.monotonic() >= deadline:
                failed = True
                break
            try:
                code, current = request(self.port, "GET", path)
                if code == 404:
                    continue
                require(code == 200 and isinstance(current, dict)
                        and current.get("metadata", {}).get("uid") == uid, "cleanup", code)
                code, _ = request(self.port, "DELETE", path, {
                    "apiVersion": "v1", "kind": "DeleteOptions",
                    "preconditions": {"uid": uid}, "propagationPolicy": "Background"})
                require(code in (200, 202), "cleanup", code)
                wait_for(lambda: request(self.port, "GET", path),
                         lambda status, _body: status == 404, "cleanup",
                         seconds=max(0, deadline - time.monotonic()))
            except (Failure, OSError):
                failed = True
        require(not failed, "cleanup")


def grant_fixture(namespace, uid, actor):
    username, actor_uid = actor
    return {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
        "metadata": {"name": "workspace", "namespace": namespace},
        "spec": {"workspaceUid": uid, "enabled": True,
                 "writers": [{"namespace": namespace, "name": username.rsplit(":", 1)[1],
                              "uid": actor_uid}]},
    }


def singleton_rule(crd):
    versions = [version for version in crd["spec"]["versions"]
                if version["name"] == "v1alpha1" and version.get("served")]
    require(len(versions) == 1, "grant-schema")
    rules = versions[0]["schema"]["openAPIV3Schema"].get("x-kubernetes-validations", [])
    matches = [rule for rule in rules if rule.get("rule") == "self.metadata.name == 'workspace'"]
    require(len(matches) == 1 and isinstance(matches[0].get("message"), str), "grant-schema")
    return matches[0]


def singleton_denied(code, body, rule):
    if (code != 422 or not isinstance(body, dict) or body.get("kind") != "Status"
            or body.get("reason") != "Invalid"):
        return False
    details = body.get("details", {})
    if not isinstance(details, dict) or details.get("name") != "not-workspace":
        return False
    causes = details.get("causes") if isinstance(details, dict) else None
    return isinstance(causes, list) and any(
        isinstance(cause, dict) and cause.get("reason") == "FieldValueInvalid"
        and isinstance(cause.get("message"), str) and rule["message"] in cause["message"]
        for cause in causes)


def prepare(port, owned, crd, namespace, other, token):
    ns_objects = [owned.create("/api/v1/namespaces", {
        "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name, "labels": {LABEL: token}},
    }) for name in (namespace, other)]
    require(ns_objects[0]["metadata"]["uid"] != ns_objects[1]["metadata"]["uid"], "fixtures")
    owned.create(CRDS, crd, "grant-schema")
    wait_for(lambda: request(port, "GET", CRDS + "/" + CRD),
             lambda code, body: code == 200 and isinstance(body, dict) and any(
                 c.get("type") == "Established" and c.get("status") == "True"
                 for c in body.get("status", {}).get("conditions", [])), "grant-schema")
    actors = {}
    for name, verb in (("credential-writer", "use-agent-credentials"),
                       ("kars-controller", "project-credentials")):
        account = owned.create(f"/api/v1/namespaces/{namespace}/serviceaccounts", {
            "apiVersion": "v1", "kind": "ServiceAccount",
            "metadata": {"name": name, "namespace": namespace}})
        actor = (f"system:serviceaccount:{namespace}:{name}", account["metadata"]["uid"])
        code, identity = as_actor(port, "/apis/authentication.k8s.io/v1/selfsubjectreviews", {
            "apiVersion": "authentication.k8s.io/v1", "kind": "SelfSubjectReview"}, actor)
        info = identity.get("status", {}).get("userInfo", {}) if isinstance(identity, dict) else {}
        require(code == 201 and info.get("uid") == actor[1] and info.get("username") == actor[0],
                "identity", code)
        actors[name] = actor
        targets = (namespace, other) if name == "credential-writer" else (namespace,)
        resources = ["secrets"] if name == "credential-writer" else ["configmaps", "pods"]
        for target in targets:
            path = f"{RBAC}/namespaces/{target}"
            owned.create(path + "/roles", {
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
                "metadata": {"name": name, "namespace": target},
                "rules": [{"apiGroups": ["kars.azure.com"], "resources": ["karscredentialgrants"],
                           "verbs": [verb, "get"], "resourceNames": ["workspace"]},
                          {"apiGroups": [""], "resources": resources,
                           "verbs": ["create"]}]})
            owned.create(path + "/rolebindings", {
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
                "metadata": {"name": name, "namespace": target},
                "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": name},
                "subjects": [{"kind": "ServiceAccount", "name": name, "namespace": namespace}]})
            for check_verb in ("use-agent-credentials", "project-credentials", "manage"):
                review = {"apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
                          "spec": {"resourceAttributes": {"group": "kars.azure.com",
                              "resource": "karscredentialgrants", "namespace": target,
                              "name": "workspace", "verb": check_verb}}}
                wait_for(lambda: as_actor(port, "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews",
                                          review, actor),
                         lambda code, body: code == 201 and isinstance(body, dict)
                         and not body.get("status", {}).get("evaluationError")
                         and body.get("status", {}).get("allowed") is (check_verb == verb), "rbac")
    grant = grant_fixture(namespace, ns_objects[0]["metadata"]["uid"], actors["credential-writer"])
    path = f"/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/karscredentialgrants"
    code, response = request(port, "POST", path + "?dryRun=All", grant)
    require(allowed(code, response, grant), "grant-canonical", code)
    bad = copy.deepcopy(grant)
    bad["metadata"]["name"] = "not-workspace"
    code, response = request(port, "POST", path + "?dryRun=All", bad)
    require(singleton_denied(code, response, singleton_rule(crd)), "grant-noncanonical", code)
    stored = owned.create(path, grant)
    stored["status"] = {"conditions": [{"type": "WriterReady", "status": "True", "reason": "Fixture",
        "message": "Admission-expression fixture only", "lastTransitionTime": "2026-01-01T00:00:00Z",
        "observedGeneration": stored["metadata"]["generation"]}]}
    code, stored = request(port, "PUT", path + "/workspace/status", stored)
    require(code == 200 and isinstance(stored, dict)
            and stored.get("status", {}).get("conditions", [{}])[0].get("observedGeneration")
            == stored.get("metadata", {}).get("generation"), "fixtures", code)
    return ns_objects, actors, stored


def prove_pair(port, key, policy, validation, actor, good, bad, good_path, bad_path):
    name = policy["metadata"]["name"]
    negative = lambda code, body: intended_denial(
        code, body, name, name, validation, bad["metadata"]["name"])
    wrong = lambda: as_actor(port, bad_path + "?dryRun=All", bad, actor)
    correct = lambda: as_actor(port, good_path + "?dryRun=All", good, actor)
    # Negative warm-up proves admission is active; positives on both sides of
    # the final negative exclude RBAC/Ready/cache failures masquerading as UID denial.
    wait_for(wrong, negative, key + "-negative")
    code, _ = wait_for(correct, lambda c, b: allowed(c, b, good), key + "-positive")
    results = [evidence(key + "-positive", code, "accepted")]
    code, body = wrong()
    require(negative(code, body), key + "-negative", code)
    results.append(evidence(key + "-negative", code, "intended-denial"))
    code, body = correct()
    require(allowed(code, body, good), key + "-positive-after", code)
    results.append(evidence(key + "-positive-after", code, "accepted"))
    return results


def exercise(root, port, version, token, results=None):
    namespace, other = "kars-cel-" + token, "kars-cel-" + token + "-other"
    crd, shipped = render(root, namespace, version)
    owned = Owned(port)
    results = [] if results is None else results
    try:
        namespaces, actors, grant = prepare(port, owned, crd, namespace, other, token)
        results += [evidence("grant-canonical", 201, "accepted"),
                    evidence("grant-noncanonical", 422, "intended-denial")]
        actual_uid, wrong_uid = [obj["metadata"]["uid"] for obj in namespaces]
        for key, (source, original_binding, validation) in shipped.items():
            policy, binding = scoped(source, original_binding, token, namespace, key)
            installed = owned.create(ADMISSION + "/validatingadmissionpolicies", policy, key + "-policy")
            name = installed["metadata"]["name"]
            wait_for(lambda: request(port, "GET", ADMISSION + "/validatingadmissionpolicies/" + name),
                     lambda code, obj: code == 200 and isinstance(obj, dict)
                     and obj.get("status", {}).get("observedGeneration") == obj["metadata"].get("generation")
                     and isinstance(obj["status"].get("typeChecking"), dict)
                     and not obj["status"]["typeChecking"].get("expressionWarnings"), key + "-policy")
            owned.create(ADMISSION + "/validatingadmissionpolicybindings", binding, key + "-policy")
            results.append(evidence(key + "-policy", 201, "accepted"))
            if key == "source-writes":
                good = {"apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                        "metadata": {"name": "kars-credential-input-uid-proof", "namespace": namespace,
                                     "annotations": {"kars.azure.com/credential-grant-uid": grant["metadata"]["uid"]}}}
                bad = copy.deepcopy(good)
                bad["metadata"]["namespace"] = other
                actor, resource = actors["credential-writer"], "secrets"
            else:
                actor = actors["kars-controller"]
                good = {"apiVersion": "v1", "kind": "ConfigMap" if key == "material" else "Pod",
                        "metadata": {"name": "kars-observation-privacy", "namespace": namespace,
                                     "annotations": {"kars.azure.com/privacy-namespace-uid": actual_uid,
                                                     "kars.azure.com/privacy-controller-uid": actor[1]}}}
                if key == "pods":
                    good["metadata"]["labels"] = {"kars.azure.com/observation-privacy-revision": "fixture"}
                    good["spec"] = {"serviceAccountName": "kars-controller", "automountServiceAccountToken": False,
                        "schedulerName": "kars-e2e-admission-never-schedule",
                        "containers": [{"name": "never-executed", "image": "registry.invalid/uid-proof:never",
                                        "imagePullPolicy": "Never"}]}
                bad = copy.deepcopy(good)
                bad["metadata"]["annotations"]["kars.azure.com/privacy-namespace-uid"] = wrong_uid
                resource = "configmaps" if key == "material" else "pods"
            results += prove_pair(port, key, policy, validation, actor, good, bad,
                f"/api/v1/namespaces/{namespace}/{resource}",
                f"/api/v1/namespaces/{bad['metadata']['namespace']}/{resource}")
    finally:
        owned.cleanup()
        results.append(evidence("cleanup", 0, "cleaned"))
    return results


def main(root):
    results, exit_code = [], 1
    try:
        with kind_proxy(root) as (port, version):
            exercise(root, port, version["gitVersion"], uuid.uuid4().hex[:12], results)
        results.append(evidence("complete", 0, "passed"))
        exit_code = 0
    except Failure as error:
        results.append(evidence(error.case, error.code, "failed"))
    except (OSError, RuntimeError, ValueError):
        results.append(evidence("complete", 0, "failed"))
    try:
        path = root / REPORT
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_text(json.dumps({"cases": results}, indent=2) + "\n")
    except OSError:
        evidence("complete", 0, "failed")
        exit_code = 1
    return exit_code


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main(Path(__file__).resolve().parents[2]))
