# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import copy
import json
import threading
import time

from .common import AGENT, OWNER, PRIVATE, REGISTRATION, RUNTIME, SYSTEM, assert_denial, require

WATCH_BINDING = "e2e-sre-legacy-watch-only"
TOKEN_ALIAS = "e2e-prestaged-router-token"
TOKEN_TYPE = "kubernetes.io/service-account-token"
SA_NAME = "kubernetes.io/service-account.name"
SA_UID = "kubernetes.io/service-account.uid"
SYNTHETIC_TOKEN = "kind-synthetic-not-a-jwt-never-authenticates"
WATCH_MARKER = "kind-dummy-secret-watch-proof"


def synthetic_token_secret(h, name, account):
    metadata = account["metadata"]
    require(metadata.get("uid") and metadata.get("namespace") == RUNTIME
            and not metadata.get("deletionTimestamp"), "Synthetic token fixture requires a live ServiceAccount UID")
    ca = h.poll("public namespace CA for synthetic token fixture",
                lambda: h.get("configmap", "kube-root-ca.crt", RUNTIME), seconds=30)
    ca = ca.get("data", {}).get("ca.crt", "")
    require(ca.startswith("-----BEGIN CERTIFICATE-----"), "Synthetic token fixture lacks the public cluster CA")
    # A matching live SA UID and complete data prevent native TokenController
    # garbage collection and token generation. Never authenticate with this data.
    return {"apiVersion": "v1", "kind": "Secret", "type": TOKEN_TYPE,
        "metadata": {"name": name, "namespace": RUNTIME,
                     "annotations": {SA_NAME: metadata["name"], SA_UID: metadata["uid"]}},
        "data": {key: base64.b64encode(value.encode()).decode() for key, value in {
            "token": SYNTHETIC_TOKEN, "ca.crt": ca, "namespace": RUNTIME}.items()}}


def assert_secret_unchanged(current, before):
    require(current is not None, "Token Secret fixture disappeared; absence is not denial proof")
    require(all(current["metadata"].get(key) == before["metadata"].get(key)
                for key in ("name", "namespace", "uid", "resourceVersion", "annotations", "labels",
                            "ownerReferences", "finalizers", "deletionTimestamp"))
            and current.get("type") == before.get("type")
            and current.get("data", {}) == before.get("data", {}),
            "Token Secret fixture was replaced, mutated or populated")
    if current.get("type") == TOKEN_TYPE:
        require(current.get("data", {}).get("token") == base64.b64encode(SYNTHETIC_TOKEN.encode()).decode(),
                "Token Secret fixture no longer contains the exact synthetic non-JWT")


def assert_token_type_immutable(response, label):
    """Secret type validation precedes VAP; this is not admission-policy proof."""
    require(response.status_code == 422,
            f"{label}: expected immutable Secret type rejection, got HTTP {response.status_code}")
    body = response.json()
    require(body.get("kind") == "Status" and body.get("reason") == "Invalid"
            and any(cause.get("field") == "type"
                    and cause.get("reason") == "FieldValueInvalid"
                    and "field is immutable" in cause.get("message", "")
                    for cause in body.get("details", {}).get("causes", [])),
            f"{label}: missing the specific immutable type validation cause")


def seed_privacy_gaps(h):
    require(h.get("serviceaccount", "sre-api-router", RUNTIME) is None,
            "Legacy fixtures require an unoccupied reserved ServiceAccount name")
    account = h.create({"apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": {"name": "sre-api-router", "namespace": RUNTIME},
        "automountServiceAccountToken": False})
    h.state["prestaged_account"] = account
    h.state["prestaged_account_cleaned"] = False
    alias = h.create(synthetic_token_secret(h, TOKEN_ALIAS, account))
    assert_secret_unchanged(h.get("secret", TOKEN_ALIAS, RUNTIME), alias)
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
    h.passed("Pre-guard same-name ServiceAccount has no controller-issued authority; its stable token alias contains only synthetic non-JWT data")
    h.passed("Unsafe legacy group grants intentionally cover all runtime ServiceAccounts; no token is requested for the same-name fixture")


def delete_owned(h, path, obj):
    response = h.api("DELETE", path, body={"apiVersion": "v1", "kind": "DeleteOptions",
        "preconditions": {"uid": obj["metadata"]["uid"],
                          "resourceVersion": obj["metadata"]["resourceVersion"]}}, status=(200, 202, 404))
    if response.status_code == 404:
        require(response.json().get("kind") == "Status" and response.json().get("reason") == "NotFound",
                "Fixture cleanup did not return a Kubernetes NotFound status")


@contextmanager
def owned_fixtures(h):
    fixtures = []
    primary_failure = False
    try:
        yield fixtures
    except BaseException:
        primary_failure = True
        raise
    finally:
        failed = False
        for path, obj in reversed(fixtures):
            try:
                delete_owned(h, path, obj)
            except Exception:
                failed = True
        if failed:
            if primary_failure:
                print("SRE-DIAG Fenced fixture cleanup failed; retaining the primary acceptance failure", flush=True)
            else:
                raise AssertionError("Fixture cleanup failed; UID/resourceVersion fences were not retried or weakened")


def token_secret_denials(h, before_enrollment=False):
    policy = "kars-sre-no-legacy-tokens"
    path = f"/api/v1/namespaces/{RUNTIME}/secrets"
    prefix = f"e2e-token-{h.phase}-{'pre' if before_enrollment else 'live'}"
    account = h.get("serviceaccount", "sre-api-router", RUNTIME)
    require(account is not None, "Token denial probes require their actual same-name ServiceAccount fixture")
    unsafe = synthetic_token_secret(h, f"{prefix}-create", account)
    # Real CREATE/PATCH, not a fixed private name or TokenRequest. A broken
    # policy fails immediately; even an accepted fixture cannot mint a JWT.
    with owned_fixtures(h) as fixtures:
        for user in ("tenant", "registrar"):
            response = h.api("POST", path, body=unsafe, user=user)
            if response.status_code == 201:
                fixtures.append((path + "/" + unsafe["metadata"]["name"], response.json()))
            assert_denial(response, f"{user} arbitrary token Secret CREATE", policy)
        benign = h.create({"apiVersion": "v1", "kind": "ServiceAccount",
            "metadata": {"name": f"{prefix}-account", "namespace": RUNTIME},
            "automountServiceAccountToken": False})
        fixtures.append((f"/api/v1/namespaces/{RUNTIME}/serviceaccounts/{prefix}-account", benign))
        variants = [
            ("type-and-annotation", "Opaque", {}, {"type": TOKEN_TYPE,
                "metadata": {"annotations": {SA_NAME: "sre-api-router"}}}),
            ("annotation", TOKEN_TYPE, {},
                {"metadata": {"annotations": {SA_NAME: "sre-api-router"}}}),
        ]
        if before_enrollment:
            # Test a type-only update before enrollment: an Opaque alias is
            # itself quarantined by the controller inventory, so it must not
            # be introduced into an already-Ready runtime even for this test.
            variants.append(("type", "Opaque", {SA_NAME: "sre-api-router"}, {"type": TOKEN_TYPE}))
        for suffix, kind, annotations, patch in variants:
            body = {"apiVersion": "v1", "kind": "Secret", "type": kind,
                "metadata": {"name": f"{prefix}-{suffix}", "namespace": RUNTIME,
                             "annotations": annotations}}
            if kind == TOKEN_TYPE:
                body = synthetic_token_secret(h, f"{prefix}-{suffix}", benign)
            obj = h.api("POST", path, body=body, user="tenant", status=201).json()
            fixtures.append((path + "/" + obj["metadata"]["name"], obj))
            assert_secret_unchanged(h.get("secret", obj["metadata"]["name"], RUNTIME), obj)
            response = h.api("PATCH", path + "/" + obj["metadata"]["name"], body={
                **patch, "metadata": {**patch.get("metadata", {}),
                    "uid": obj["metadata"]["uid"], "resourceVersion": obj["metadata"]["resourceVersion"]}},
                user="tenant")
            if "type" in patch:
                assert_token_type_immutable(response, f"token Secret {suffix} PATCH")
            else:
                assert_denial(response, f"token Secret {suffix} PATCH", policy)
            assert_secret_unchanged(h.get("secret", obj["metadata"]["name"], RUNTIME), obj)
        if before_enrollment:
            for patch in ({"type": "Opaque"}, {"metadata": {"annotations": {SA_NAME: "e2e-other"}}}):
                before = h.get("secret", TOKEN_ALIAS, RUNTIME)
                assert_secret_unchanged(before, h.state["prestaged_alias"])
                response = h.api("PATCH", path + "/" + TOKEN_ALIAS, body={
                    **patch, "metadata": {**patch.get("metadata", {}), "uid": before["metadata"]["uid"],
                                         "resourceVersion": before["metadata"]["resourceVersion"]}}, user="tenant")
                if "type" in patch:
                    assert_token_type_immutable(response, "prestaged oldObject type escape")
                else:
                    assert_denial(response, "prestaged oldObject annotation escape", policy)
                assert_secret_unchanged(h.get("secret", TOKEN_ALIAS, RUNTIME), before)
    h.passed("Tenant and registrar token Secret CREATE and schema-valid annotation updates receive exact admission denials")
    h.passed("Token-type mutations receive specific Kubernetes immutable-field errors without changing fixtures")


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
    expected = None if h.state["prestaged_account_cleaned"] else h.state["prestaged_account"]
    require(h.get("serviceaccount", "sre-api-router", RUNTIME) == expected,
            "Controller adopted, mutated or replaced the unowned same-name ServiceAccount before safe enrollment")
    require(all(h.get("secret", name, RUNTIME) is None for name in (PRIVATE, AGENT)),
            "Private credential appeared while legacy privacy was unsafe")
    for name in ("kars-sre-private-reader", "kars-sre-private-author", "kars-sre-private-renew"):
        require(h.get("clusterrolebinding", name) is None, "Private grant appeared while legacy privacy was unsafe")
    require(h.get("rolebinding", "sre-api-self-renew", RUNTIME) is None,
            "Private token-renewal grant appeared while legacy privacy was unsafe")
    reg = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(not reg.get("status", {}).get("routerServiceAccountUid"),
            "Controller registered the unowned same-name ServiceAccount as private authority")


def assert_fresh_private_identity(h, reg):
    account = h.get("serviceaccount", "sre-api-router", RUNTIME)
    require(h.state["prestaged_account_cleaned"] and account
            and account["metadata"]["uid"] != h.state["prestaged_account"]["metadata"]["uid"]
            and account["metadata"]["uid"] == reg["status"].get("routerServiceAccountUid")
            and account["metadata"].get("annotations", {}).get(OWNER) == reg["metadata"]["uid"]
            and account.get("automountServiceAccountToken") is False,
            "Trusted private identity was not freshly controller-created after unowned fixture cleanup")
    require(h.get("secret", TOKEN_ALIAS, RUNTIME) is None, "Prestaged token alias survived private issuance")


def block_prestaged_paths(h, spec, before):
    h.api("POST", REGISTRATION.rsplit("/", 1)[0], body={
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
        "metadata": {"name": "canonical"}, "spec": spec}, user="registrar", status=201)
    wait_blocked(h, "Unsafe legacy SRE token Secret alias")
    assert_unissued(h, spec, before)
    alias = h.get("secret", TOKEN_ALIAS, RUNTIME)
    assert_secret_unchanged(alias, h.state["prestaged_alias"])
    delete_owned(h, f"/api/v1/namespaces/{RUNTIME}/secrets/{TOKEN_ALIAS}", alias)
    h.poll("prestaged synthetic alias cleanup", lambda: h.get("secret", TOKEN_ALIAS, RUNTIME) is None, seconds=30)
    # The broad watch grant still blocks enrollment while the operator removes
    # the unowned same-name SA. Never adopt it by name or attach private grants.
    account = h.get("serviceaccount", "sre-api-router", RUNTIME)
    require(account == h.state["prestaged_account"], "Unowned ServiceAccount changed before operator cleanup")
    delete_owned(h, f"/api/v1/namespaces/{RUNTIME}/serviceaccounts/sre-api-router", account)
    h.poll("unowned same-name ServiceAccount cleanup",
           lambda: h.get("serviceaccount", "sre-api-router", RUNTIME) is None, seconds=30)
    h.state["prestaged_account_cleaned"] = True
    h.passed("Real controller blocks the unchanged synthetic alias before adopting the same-name SA or issuing private authority; operator UID/RV-fenced cleanup removes both")

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

    with owned_fixtures(h) as fixtures:
        with ThreadPoolExecutor(max_workers=1) as pool:
            result = pool.submit(observe)
            require(started.wait(timeout=3), "Secret watch probe did not start")
            obj = h.create({"apiVersion": "v1", "kind": "Secret",
                "metadata": {"name": name, "namespace": namespace}, "stringData": {"dummy": WATCH_MARKER}})
            fixtures.append((f"/api/v1/namespaces/{namespace}/secrets/{name}", obj))
            response, observed = result.result(timeout=12)
        assert_watch_result(response, observed, allowed)
