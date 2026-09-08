# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Image-free, real-controller-principal RBAC retirement dry-run experiment."""

import base64
import json
import re
import ssl
import time
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import HTTPSHandler, ProxyHandler, Request, build_opener

from .common import POLICIES, REGISTRATION, RUNTIME, SYSTEM, require
from .fixtures import LEGACY_COMMIT
from .registration_schema import CONTEXT, command, request

RBAC = "/apis/rbac.authorization.k8s.io/v1"
READER = "kars-sre-reader"
CUSTOM = "e2e-sre-custom-review"
EXTRA = "e2e-sre-reader-bind-proof"
CONTROLLER = f"system:serviceaccount:{SYSTEM}:kars-controller"
RETIRED = "kars.azure.com/sre-legacy-retired"
SURVIVOR = {"kind": "ServiceAccount", "name": "e2e-retained-subject", "namespace": SYSTEM}
LEGACY = {"kind": "ServiceAccount", "name": "sandbox", "namespace": RUNTIME}


def create(port, path, obj):
    code, result = request(port, "POST", path, obj)
    require(code == 201 and result.get("metadata", {}).get("uid")
            and result["metadata"].get("resourceVersion"), "RBAC proof fixture CREATE failed")
    return result


def get(port, path):
    code, obj = request(port, "GET", path)
    require(code == 200 and isinstance(obj, dict), "RBAC proof fixture GET failed")
    return obj


def historical_reader(root):
    from .bootstrap_probe import converted_objects
    command("reader-history", ["git", "fetch", "--no-tags", "--depth=1",
            "https://github.com/Azure/kars.git", LEGACY_COMMIT], root=root)
    chart = root / ".e2e-sre-reader-chart"
    require(not chart.exists(), "Refusing to overwrite an existing historical reader fixture directory")
    files = ("Chart.yaml", "values.yaml", "templates/sre.yaml")
    try:
        for name in files:
            contents = command("reader-source", ["git", "show",
                f"{LEGACY_COMMIT}:deploy/helm/kars/{name}"], root=root)
            path = chart / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents)
        rendered = command("reader-render", ["helm", "template", "kars", str(chart),
            "--namespace", SYSTEM, "--set", "sre.enabled=true", "--show-only", "templates/sre.yaml"], root=root)
        documents = [document for document in re.split(r"(?m)^---\s*\n", rendered)
                     if re.search(r"(?m)^kind: ClusterRole\s*$", document)
                     and re.search(r"(?m)^  name: kars-sre-reader\s*$", document)]
        require(len(documents) == 1, "Pinned legacy chart did not emit exactly one reader role")
        objects = converted_objects(command("reader-convert", [
            "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
            "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
        ], root=root, data=documents[0]))
        require(len(objects) == 1 and objects[0]["metadata"]["name"] == READER
                and objects[0]["kind"] == "ClusterRole", "Unexpected historical RBAC fixture")
        return objects[0]
    finally:
        for name in files:
            (chart / name).unlink(missing_ok=True)
        for path in (chart / "templates", chart):
            if path.exists():
                path.rmdir()


def seed(port, root):
    code, _ = request(port, "GET", "/apis/admissionregistration.k8s.io/v1/"
                      "validatingadmissionpolicies/kars-sre-binding-authority")
    require(code == 404, "Legacy binding proof fixtures must precede admission policies")
    reader = create(port, RBAC + "/clusterroles", historical_reader(root))
    custom = create(port, RBAC + "/clusterroles", {
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": {"name": CUSTOM},
        "rules": [{"apiGroups": [""], "resources": ["limitranges"], "verbs": ["get"]}]})
    bindings = []
    for role in (reader, custom):
        bindings.append(create(port, RBAC + "/clusterrolebindings", {
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
            "metadata": {"name": role["metadata"]["name"]},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole",
                        "name": role["metadata"]["name"]},
            "subjects": [LEGACY, SURVIVOR]}))
    consumer = create(port, f"/apis/apps/v1/namespaces/{RUNTIME}/deployments", {
        "apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "sre", "namespace": RUNTIME},
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "e2e-reviewed-consumer"}},
                 "template": {"metadata": {"labels": {"app": "e2e-reviewed-consumer"}},
                     "spec": {"automountServiceAccountToken": False,
                              "schedulerName": "kars-e2e-admission-never-schedule",
                              "containers": [{"name": "never-executed",
                                  "image": "registry.invalid/kars-admission-proof:never", "imagePullPolicy": "Never"}]}}}})
    return {"bindings": bindings, "roles": [reader, custom], "consumer": consumer}


def retirement_patch(binding, registration_uid):
    require(binding["subjects"] == [LEGACY, SURVIVOR], "Unexpected reviewed binding subjects")
    return {"metadata": {"uid": binding["metadata"]["uid"],
                        "resourceVersion": binding["metadata"]["resourceVersion"],
                        "annotations": {RETIRED: registration_uid}}, "subjects": [SURVIVOR]}


def authorization_category(code, body):
    if not isinstance(body, dict):
        return "unexpected-response"
    if code == 403 and body.get("kind") == "Status" and body.get("reason") == "Forbidden":
        message = body.get("message", "")
        if isinstance(message, str) and "is attempting to grant RBAC permissions not currently held" in message:
            return "rbac-permissions-not-held"
        return "other-forbidden"
    if code == 409 and body.get("kind") == "Status" and body.get("reason") == "Conflict":
        return "cas-conflict"
    if code == 200 and body.get("kind") == "ClusterRoleBinding":
        return "accepted"
    return "unexpected-response"


class ControllerAPI:
    def __init__(self, root, port):
        config_args = ["kubectl", "--context", CONTEXT, "config", "view", "--raw", "--minify", "-o"]
        self.server = command("public-api-server", config_args + [
            "jsonpath={.clusters[0].cluster.server}"], root=root)
        require(urlsplit(self.server).hostname in ("127.0.0.1", "localhost", "::1"),
                "RBAC proof refuses a non-loopback API")
        ca = command("public-api-ca", config_args + [
            "jsonpath={.clusters[0].cluster.certificate-authority-data}"], root=root)
        context = ssl.create_default_context(
            cadata=base64.b64decode(ca).decode())
        self.opener = build_opener(ProxyHandler({}), HTTPSHandler(context=context))
        account = get(port, f"/api/v1/namespaces/{SYSTEM}/serviceaccounts/kars-controller")
        code, response = request(port, "POST", f"/api/v1/namespaces/{SYSTEM}/serviceaccounts/kars-controller/token", {
            "apiVersion": "authentication.k8s.io/v1", "kind": "TokenRequest",
            "spec": {"audiences": [], "expirationSeconds": 600}})
        require(code == 201 and response.get("status", {}).get("token"),
                "Ephemeral controller-principal TokenRequest failed")
        self.token = response["status"]["token"]
        code, response = self.request("POST", "/apis/authentication.k8s.io/v1/selfsubjectreviews", {
            "apiVersion": "authentication.k8s.io/v1", "kind": "SelfSubjectReview"})
        identity = response.get("status", {}).get("userInfo", {})
        require(code == 201 and identity.get("username") == CONTROLLER
                and identity.get("uid") == account["metadata"]["uid"],
                "Bearer-only proof principal does not match the actual controller ServiceAccount UID")
        self.account = account

    def request(self, method, path, obj):
        req = Request(self.server + path, data=json.dumps(obj).encode(), method=method,
            headers={"Content-Type": "application/merge-patch+json" if method == "PATCH" else "application/json",
                     "Accept": "application/json", "Authorization": f"Bearer {self.token}"})
        try:
            response = self.opener.open(req, timeout=15)
        except HTTPError as error:
            response = error
        with response:
            return response.code, json.loads(response.read(1024 * 1024))

    def allowed(self, group, resource, verb, name=None):
        attributes = {"group": group, "resource": resource, "verb": verb}
        if name:
            attributes["name"] = name
        code, result = self.request("POST", "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews", {
            "apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
            "spec": {"resourceAttributes": attributes}})
        require(code == 201 and isinstance(result.get("status", {}).get("allowed"), bool)
                and not result["status"].get("evaluationError"), "Live controller authorization review failed")
        return result["status"]["allowed"]


def observe_bind(api, expected):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if api.allowed("rbac.authorization.k8s.io", "clusterroles", "bind", READER) is expected:
            return
        time.sleep(0.25)
    raise AssertionError("Exact named bind permission did not converge")


def prove(root, port, state, objects, report):
    from .bootstrap_probe import PATHS
    source = create(port, f"/apis/kars.azure.com/v1alpha1/namespaces/{SYSTEM}/karssandboxes", {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": "sre", "namespace": SYSTEM},
        "spec": {"runtime": {"kind": "BYO", "byo": {"contractVersion": "v1",
            "image": "registry.invalid/kars-admission-proof:never", "command": ["/bin/true"]}},
            "inferenceRef": {"name": "sre-inference"}, "sandbox": {"isolation": "standard"}}})
    consumer_path = f"/apis/apps/v1/namespaces/{RUNTIME}/deployments/sre"
    consumer = get(port, consumer_path)
    spec = {"controller": {"namespace": {"name": SYSTEM,
                "uid": get(port, f"/api/v1/namespaces/{SYSTEM}")["metadata"]["uid"]},
            "deployment": {"name": "kars-controller", "uid": get(port,
                f"/apis/apps/v1/namespaces/{SYSTEM}/deployments/kars-controller")["metadata"]["uid"]}, "release": "kars"},
        "sandbox": {"namespace": SYSTEM, "name": "sre", "uid": source["metadata"]["uid"]},
        "runtimeNamespace": {"name": RUNTIME, "uid": get(port, f"/api/v1/namespaces/{RUNTIME}")["metadata"]["uid"]},
        "legacyBindings": [{"kind": "ClusterRoleBinding", **{k: binding["metadata"][k]
             for k in ("name", "uid", "resourceVersion")}, "roleRef": binding["roleRef"], "subjects": binding["subjects"]}
             for binding in state["bindings"]],
        "legacyConsumer": {"namespace": RUNTIME, "name": "sre", **{k: consumer["metadata"][k]
                                                                                for k in ("uid", "resourceVersion")}}}
    registration = create(port, REGISTRATION.rsplit("/", 1)[0], {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
        "metadata": {"name": "canonical"}, "spec": spec})
    snapshots = {}
    for obj in objects + state["roles"] + state["bindings"]:
        if obj["kind"] not in ("ClusterRole", "ClusterRoleBinding", "Role", "RoleBinding",
                               "ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"):
            continue
        path = PATHS[obj["kind"]].format(namespace=obj["metadata"].get("namespace", SYSTEM))
        path += "/" + obj["metadata"]["name"]
        current = get(port, path)
        if obj["kind"] == "ValidatingAdmissionPolicy" and obj["metadata"]["name"].startswith("kars-sre-"):
            require(current.get("status", {}).get("observedGeneration") == current["metadata"]["generation"]
                    and "typeChecking" in current["status"]
                    and not current["status"]["typeChecking"].get("expressionWarnings"),
                    "SRE policy is not observed and warning-free during RBAC proof")
        if obj["kind"] == "ValidatingAdmissionPolicyBinding" and obj["metadata"]["name"].startswith("kars-sre-"):
            require("Deny" in current["spec"]["validationActions"]
                    and current["spec"]["policyName"] == current["metadata"]["name"],
                    "SRE policy lacks its intended Deny binding during RBAC proof")
        # Native CEL type-check status may evolve; specification/identity must not.
        current.pop("status", None)
        if obj["kind"] == "ValidatingAdmissionPolicy":
            current["metadata"].pop("resourceVersion", None)
        snapshots[path] = current
    require({obj["metadata"]["name"] for obj in objects if obj["kind"] == "ValidatingAdmissionPolicy"
             and obj["metadata"]["name"].startswith("kars-sre-")} == {"kars-sre-" + name for name in POLICIES},
            "RBAC experiment lacks the complete SRE policy set")
    api = ControllerAPI(root, port)
    facts = {"controllerBearerUidVerified": True, "workloadExecution": "not-attempted",
             "legacyCommit": LEGACY_COMMIT, "observedWarningFreeDenyBoundSrePolicies": len(POLICIES), "cases": []}
    report(facts)
    require(api.allowed("rbac.authorization.k8s.io", "clusterrolebindings", "patch", READER),
            "Controller lacks ordinary CRB PATCH; this is not the bind hypothesis")
    require(api.allowed("kars.azure.com", "karssreregistrations", "use", "canonical"),
            "Controller lacks canonical registrar use")
    baseline_bind = api.allowed("rbac.authorization.k8s.io", "clusterroles", "bind", READER)
    facts["authorization"] = {"patchReaderBinding": True, "useCanonical": True, "bindReader": baseline_bind,
        "bindCustom": api.allowed("rbac.authorization.k8s.io", "clusterroles", "bind", CUSTOM),
        "getCustomLimitRanges": api.allowed("", "limitranges", "get"),
        "getReaderNodeMetrics": api.allowed("metrics.k8s.io", "nodes", "get")}
    require(not facts["authorization"]["bindCustom"] and not facts["authorization"]["getCustomLimitRanges"],
            "Custom fixture is not outside the controller's current authority")

    def unchanged():
        for path, before in snapshots.items():
            current = get(port, path)
            current.pop("status", None)
            if current["kind"] == "ValidatingAdmissionPolicy":
                current["metadata"].pop("resourceVersion", None)
            require(current == before, "RBAC experiment changed a policy, role or reviewed binding")
        live = get(port, consumer_path)
        require(all(live["metadata"][key] == consumer["metadata"][key] for key in ("uid", "resourceVersion"))
                and live["spec"] == consumer["spec"],
                "RBAC experiment changed or stopped the legacy consumer")
        require(get(port, REGISTRATION) == registration, "Image-free experiment unexpectedly reconciled registration")
        require(get(port, f"/api/v1/namespaces/{SYSTEM}/serviceaccounts/kars-controller") == api.account,
                "Controller identity was replaced during the RBAC experiment")
        for secret in ("sre-api-router-identity", "sre-api-agent"):
            code, _ = request(port, "GET", f"/api/v1/namespaces/{RUNTIME}/secrets/{secret}")
            require(code == 404, "RBAC dry-run experiment issued private material")

    def probe(binding, phase, expected, stale=None):
        patch = retirement_patch(binding, registration["metadata"]["uid"])
        if stale:
            patch["metadata"][stale] = "00000000-0000-0000-0000-000000000000" if stale == "uid" else "1"
        code, response = api.request("PATCH",
            RBAC + "/clusterrolebindings/" + binding["metadata"]["name"] + "?dryRun=All", patch)
        category = authorization_category(code, response)
        result = {"phase": phase, "role": binding["roleRef"]["name"], "httpStatus": code, "category": category}
        facts["cases"].append(result)
        report(facts)
        require(code == expected and category == {200: "accepted", 403: "rbac-permissions-not-held",
                409: "cas-conflict"}[expected], "Retirement dry-run did not prove the intended API outcome")
        if expected == 200:
            require(response["metadata"]["uid"] == binding["metadata"]["uid"]
                    and response["metadata"].get("annotations", {}).get(RETIRED) == registration["metadata"]["uid"]
                    and response["roleRef"] == binding["roleRef"] and response["subjects"] == [SURVIVOR],
                    "Retirement dry-run did not preserve the exact unrelated subject and roleRef")
        unchanged()

    reader, custom = state["bindings"]
    probe(reader, "baseline", 200 if baseline_bind else 403)
    probe(custom, "baseline-custom", 403)
    extra = []
    try:
        role = create(port, RBAC + "/clusterroles", {
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole", "metadata": {"name": EXTRA},
            "rules": [{"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["clusterroles"],
                       "resourceNames": [READER], "verbs": ["bind"]}]})
        extra.append((RBAC + "/clusterroles/" + EXTRA, role))
        binding = create(port, RBAC + "/clusterrolebindings", {
            "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding", "metadata": {"name": EXTRA},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": EXTRA},
            "subjects": [{"kind": "ServiceAccount", "name": "kars-controller", "namespace": SYSTEM}]})
        extra.append((RBAC + "/clusterrolebindings/" + EXTRA, binding))
        observe_bind(api, True)
        require(not api.allowed("rbac.authorization.k8s.io", "clusterroles", "bind", CUSTOM)
                and not api.allowed("", "limitranges", "get"), "Named reader bind widened custom-role authority")
        probe(reader, "exact-reader-bind-only", 200)
        probe(custom, "exact-reader-bind-only-custom", 403)
        probe(reader, "wrong-uid", 409, "uid")
        probe(reader, "stale-resource-version", 409, "resourceVersion")
        facts["allReviewedDryRunsAuthorized"] = False
        facts["noRetirementsApplied"] = True
    finally:
        for path, obj in reversed(extra):
            code, _ = request(port, "DELETE", path, {
                "apiVersion": "v1", "kind": "DeleteOptions",
                "preconditions": {k: obj["metadata"][k] for k in ("uid", "resourceVersion")}})
            require(code in (200, 202), "Exact named bind fixture cleanup failed")
    observe_bind(api, baseline_bind)
    probe(reader, "named-bind-removed", 200 if baseline_bind else 403)
    facts["unchangedPoliciesRolesBindingsConsumer"] = True
    facts["testBindGrantRemoved"] = True
    report(facts)
