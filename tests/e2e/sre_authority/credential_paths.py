# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
from concurrent.futures import ThreadPoolExecutor
import copy
import json
import threading
import time

from .common import AGENT, PRIVATE, REGISTRATION, RUNTIME, SYSTEM, assert_denial, require

WATCH_BINDING = "e2e-sre-legacy-watch-only"
TOKEN_ALIAS = "e2e-prestaged-router-token"
TOKEN_TYPE = "kubernetes.io/service-account-token"
SA_NAME = "kubernetes.io/service-account.name"
WATCH_MARKER = "kind-dummy-secret-watch-proof"


def seed_privacy_gaps(h):
    require(h.get("serviceaccount", "sre-api-router", RUNTIME) is None,
            "Legacy token alias must precede private ServiceAccount creation")
    alias = h.create({"apiVersion": "v1", "kind": "Secret", "type": TOKEN_TYPE,
        "metadata": {"name": TOKEN_ALIAS, "namespace": RUNTIME, "annotations": {SA_NAME: "sre-api-router"}}})
    require(not alias.get("data"), "Prestaged token alias unexpectedly contains credentials")
    h.state["prestaged_alias"] = alias
    h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": {"name": WATCH_BINDING},
        "rules": [{"apiGroups": [""], "resources": ["secrets"], "verbs": ["watch"]}]})
    binding = h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
        "metadata": {"name": WATCH_BINDING},
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": WATCH_BINDING},
        "subjects": [{"kind": "Group", "name": "system:serviceaccounts:kars-sre",
                      "apiGroup": "rbac.authorization.k8s.io"}]})
    h.state["watch_binding"] = binding
    h.create({"apiVersion": "v1", "kind": "ServiceAccount",
              "metadata": {"name": "e2e-watch-only", "namespace": RUNTIME}})
    h.token_identity("watch-only", RUNTIME, "e2e-watch-only")
    h.passed("Watch-only group grant and arbitrary token alias predate admission and private ServiceAccount creation")


def delete_owned(h, path, obj):
    h.api("DELETE", path, body={"apiVersion": "v1", "kind": "DeleteOptions",
        "preconditions": {"uid": obj["metadata"]["uid"],
                          "resourceVersion": obj["metadata"]["resourceVersion"]}}, status=(200, 202))


def token_secret_denials(h, before_enrollment=False):
    policy = "kars-sre-no-legacy-tokens"
    path = f"/api/v1/namespaces/{RUNTIME}/secrets"
    prefix = f"e2e-token-{h.phase}-{'pre' if before_enrollment else 'live'}"
    unsafe = {"apiVersion": "v1", "kind": "Secret", "type": TOKEN_TYPE,
        "metadata": {"name": f"{prefix}-create", "namespace": RUNTIME,
                     "annotations": {SA_NAME: "sre-api-router"}}}
    # Real CREATE/PATCH, not a fixed private name or TokenRequest. A broken
    # policy fails the disposable cluster immediately, without reading tokens.
    for user in ("tenant", "registrar"):
        assert_denial(h.api("POST", path, body=unsafe, user=user),
                      f"{user} arbitrary token Secret CREATE", policy)
    fixtures = []
    try:
        variants = [
            ("type-and-annotation", "Opaque", {}, {"type": TOKEN_TYPE,
                "metadata": {"annotations": {SA_NAME: "sre-api-router"}}}),
            ("annotation", TOKEN_TYPE, {SA_NAME: "e2e-never-created-account"},
                {"metadata": {"annotations": {SA_NAME: "sre-api-router"}}}),
        ]
        if before_enrollment:
            # Test a type-only update before enrollment: an Opaque alias is
            # itself quarantined by the controller inventory, so it must not
            # be introduced into an already-Ready runtime even for this test.
            variants.append(("type", "Opaque", {SA_NAME: "sre-api-router"}, {"type": TOKEN_TYPE}))
        for suffix, kind, annotations, patch in variants:
            obj = h.api("POST", path, body={"apiVersion": "v1", "kind": "Secret", "type": kind,
                "metadata": {"name": f"{prefix}-{suffix}", "namespace": RUNTIME,
                             "annotations": annotations}}, user="tenant", status=201).json()
            fixtures.append(obj)
            assert_denial(h.api("PATCH", path + "/" + obj["metadata"]["name"], body={
                **patch, "metadata": {**patch.get("metadata", {}),
                    "uid": obj["metadata"]["uid"], "resourceVersion": obj["metadata"]["resourceVersion"]}},
                user="tenant"), f"token Secret {suffix} PATCH", policy)
            current = h.get("secret", obj["metadata"]["name"], RUNTIME)
            require(current["metadata"]["resourceVersion"] == obj["metadata"]["resourceVersion"]
                    and not current.get("data"), "Denied token Secret update mutated or populated its fixture")
        if before_enrollment:
            for patch in ({"type": "Opaque"}, {"metadata": {"annotations": {SA_NAME: "e2e-other"}}}):
                assert_denial(h.api("PATCH", path + "/" + TOKEN_ALIAS, body=patch, user="tenant"),
                              "prestaged oldObject type/annotation escape", policy)
    finally:
        for obj in fixtures:
            delete_owned(h, path + "/" + obj["metadata"]["name"], obj)
    h.passed("Tenant and registrar arbitrary token Secret CREATE, plus type/annotation PATCH escapes, receive exact admission denials")


def fixture_review(h):
    # Only the known pre-guard fixture is reviewed here. CLI preview must
    # refuse while the broad watch grant exists; that refusal is tested too.
    spec = copy.deepcopy(h.state["legacy_review_before_guards"])
    for kind, name, namespace, uid in [
        ("namespace", SYSTEM, None, spec["controller"]["namespace"]["uid"]),
        ("deployment", "kars-controller", SYSTEM, spec["controller"]["deployment"]["uid"]),
        ("karssandbox", "sre", SYSTEM, spec["sandbox"]["uid"]),
        ("namespace", RUNTIME, None, spec["runtimeNamespace"]["uid"]),
    ]:
        obj = h.get(kind, name, namespace)
        require(obj and obj["metadata"]["uid"] == uid, "Pre-guard fixture identity changed during staging")
    for binding in spec["legacyBindings"]:
        obj = h.get(binding["kind"].lower(), binding["name"], binding.get("namespace"))
        require(obj and obj["metadata"]["uid"] == binding["uid"]
                and obj["roleRef"] == binding["roleRef"] and obj["subjects"] == binding["subjects"],
                "Pre-guard fixture binding was replaced or its reviewed authority changed")
        binding["resourceVersion"] = obj["metadata"]["resourceVersion"]
    consumer = h.get("deployment", "sre", RUNTIME)
    require(consumer["metadata"]["uid"] == spec["legacyConsumer"]["uid"], "Pre-guard consumer was replaced")
    spec["legacyConsumer"]["resourceVersion"] = consumer["metadata"]["resourceVersion"]
    return spec


def wait_blocked(h, detail):
    def blocked():
        reg = h.get("karssreregistrations.kars.azure.com", "canonical")
        status = reg.get("status", {}) if reg else {}
        return reg if (status.get("phase") == "Blocked"
            and status.get("observedGeneration") == reg["metadata"]["generation"]
            and detail in status.get("detail", "")) else False
    return h.poll(f"controller rejection for {detail}", blocked, seconds=75)


def assert_unissued(h, spec, before):
    for binding in spec["legacyBindings"]:
        require(h.get(binding["kind"].lower(), binding["name"], binding.get("namespace")) == before[binding["name"]],
                "Blocked migration changed a legacy grant")
    require(h.get("deployment", "sre", RUNTIME)["spec"]["replicas"] == 1,
            "Blocked migration stopped the legacy consumer")
    require(h.get("serviceaccount", "sre-api-router", RUNTIME) is None,
            "Private ServiceAccount appeared while legacy privacy was unsafe")
    require(all(h.get("secret", name, RUNTIME) is None for name in (PRIVATE, AGENT)),
            "Private credential appeared while legacy privacy was unsafe")
    for name in ("kars-sre-private-reader", "kars-sre-private-author", "kars-sre-private-renew"):
        require(h.get("clusterrolebinding", name) is None, "Private grant appeared while legacy privacy was unsafe")
    require(h.get("rolebinding", "sre-api-self-renew", RUNTIME) is None,
            "Private token-renewal grant appeared while legacy privacy was unsafe")


def block_prestaged_paths(h, spec, before):
    h.api("POST", REGISTRATION.rsplit("/", 1)[0], body={
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
        "metadata": {"name": "canonical"}, "spec": spec}, user="registrar", status=201)
    wait_blocked(h, "Unsafe legacy SRE token Secret alias")
    assert_unissued(h, spec, before)
    alias = h.get("secret", TOKEN_ALIAS, RUNTIME)
    require(alias == h.state["prestaged_alias"] and not alias.get("data"),
            "Prestaged token Secret was deleted, adopted, populated or modified")
    delete_owned(h, f"/api/v1/namespaces/{RUNTIME}/secrets/{TOKEN_ALIAS}", alias)
    h.passed("Real controller quarantines the untouched prestaged token alias before private identity/grants/issuance; operator CAS cleanup only")

    wait_blocked(h, "broad group grant")
    assert_unissued(h, spec, before)
    binding = h.get("clusterrolebinding", WATCH_BINDING)
    require(binding == h.state["watch_binding"], "Blocked watch-only group grant was changed or adopted")
    h.passed("Real migration rejects a Secrets-watch-only group grant before retiring grants or issuing private material")
    # Keep the registration deliberately incomplete before removing the last
    # unsafe fixture, so cleanup cannot auto-start a successful migration.
    reg = h.get("karssreregistrations.kars.azure.com", "canonical")
    partial = {**spec, "legacyBindings": spec["legacyBindings"][:-1]}
    h.api("PATCH", REGISTRATION, body={"metadata": {"uid": reg["metadata"]["uid"],
        "resourceVersion": reg["metadata"]["resourceVersion"]}, "spec": partial}, user="registrar", status=200)
    delete_owned(h, f"/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/{WATCH_BINDING}", binding)
    blocked = wait_blocked(h, "Unreviewed legacy SRE binding")
    assert_unissued(h, spec, before)
    return blocked


def assert_watch_result(response, observed, allowed):
    if allowed:
        require(response.status_code == 200 and observed,
                "Watch-only baseline did not observe the newly created dummy Secret")
    else:
        # HTTP 200 with no events is still an authorized watch, never a denial.
        assert_denial(response, "held old principal Secret WATCH")
        require(not observed, "Denied Secret watch nevertheless exposed the dummy Secret")


def secret_watch(h, namespace, name, *, user="old-agent", allowed=False, cluster=False):
    collection = "/api/v1/secrets" if cluster else f"/api/v1/namespaces/{namespace}/secrets"
    version = h.api("GET", collection, status=200).json()["metadata"]["resourceVersion"]
    client = h.client(user)
    started = threading.Event()
    encoded = base64.b64encode(WATCH_MARKER.encode()).decode()

    def observe():
        started.set()
        end = min(h.deadline, time.monotonic() + 10)
        with client.stream("GET", collection, params={"watch": "true", "resourceVersion": version,
            "fieldSelector": f"metadata.name={name}", "timeoutSeconds": 5}, timeout=8) as response:
            if response.status_code != 200:
                response.read()
                return response, False
            for index, line in enumerate(response.iter_lines()):
                if index > 16 or time.monotonic() > end:
                    break
                if not line:
                    continue
                event = json.loads(line)
                obj = event.get("object", {})
                if (event.get("type") == "ADDED" and obj.get("metadata", {}).get("name") == name
                        and obj["metadata"].get("namespace") == namespace
                        and obj.get("data", {}).get("dummy") == encoded):
                    return response, True
            return response, False

    obj = None
    try:
        with ThreadPoolExecutor(max_workers=1) as pool:
            result = pool.submit(observe)
            require(started.wait(timeout=3), "Secret watch probe did not start")
            obj = h.create({"apiVersion": "v1", "kind": "Secret",
                "metadata": {"name": name, "namespace": namespace}, "stringData": {"dummy": WATCH_MARKER}})
            response, observed = result.result(timeout=12)
        assert_watch_result(response, observed, allowed)
    finally:
        if obj:
            delete_owned(h, f"/api/v1/namespaces/{namespace}/secrets/{name}", obj)
