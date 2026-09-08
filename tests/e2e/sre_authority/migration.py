# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json

from .common import (
    AGENT, EPOCH, OPERATORS, PRIVATE, RUNTIME, SYSTEM, TENANT,
    assert_claim, assert_denial, enrollment_json, fingerprint, printed_object, require, review_args,
)
from .fixtures import CONTROL, CONTROL_NS, GROUP_BINDING, delegate_operators
from .admission import admission_cases, policies_ready
from .credential_paths import block_prestaged_paths, fixture_review, secret_watch, token_secret_denials


def cli_failure(result, text, label):
    require(result.returncode != 0 and text in (result.stdout + result.stderr),
            f"{label}: command did not fail for the intended reason")


def pods(h, namespace):
    return json.loads(h.k("get", "pods", "-n", namespace, "-o", "json"))["items"]


def snapshot_grants(h, spec):
    return {binding["name"]: h.get(binding["kind"].lower(), binding["name"], binding.get("namespace"))
            for binding in spec["legacyBindings"]}


def legacy_migration(h):
    policies_ready(h)
    delegate_operators(h)
    # Group grants were deliberately created before admission, never smuggled
    # through the new deny policies.
    original = h.get("clusterrolebinding", "kars-sre-reader")
    cli_failure(h.cli("authority", "preview", user="registrar", expected=None),
                "broad group grant", "broad-group review")
    require(h.get("clusterrolebinding", "kars-sre-reader")["subjects"] == original["subjects"],
            "Group ambiguity mutated direct legacy subjects")
    require(h.get("secret", PRIVATE, RUNTIME) is None, "Private material appeared before explicit enrollment")
    h.k("delete", "clusterrolebinding", GROUP_BINDING, "--wait=true", "--timeout=30s")
    h.passed("Actual preview blocks an unsafe pre-existing group grant before direct grants or credentials change")
    cli_failure(h.cli("authority", "preview", user="registrar", expected=None),
                "broad group grant", "watch-only broad-group review")
    assert_denial(h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin",
                       user="watch-only"), "watch-only principal GET")
    assert_denial(h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets",
                       user="watch-only"), "watch-only principal LIST")
    secret_watch(h, CONTROL_NS, "e2e-watch-only-before", user="watch-only", allowed=True)
    h.passed("Watch-only group member is denied Secret GET/LIST but genuinely observes a newly created dummy Secret; preview blocks the grant")

    h.k("rollout", "status", f"deployment/{CONTROL}", "-n", CONTROL_NS, "--timeout=150s", timeout=160)
    spec = fixture_review(h)
    require(spec["sandbox"]["uid"] == h.state["legacy_source_uid"], "Staging adopted a replacement SRE source")
    require(spec["runtimeNamespace"]["uid"] == h.state["legacy_namespace_uid"], "Staging replaced the runtime namespace")
    assert_claim(h.get("karssandbox", "sre", SYSTEM), h.get("namespace", RUNTIME),
                 h.get("namespace", SYSTEM), h.state["system_uid"])
    require(spec.get("legacyConsumer") and len(spec["legacyBindings"]) >= 2, "Legacy reviews are incomplete")
    h.state["old_agent_sa_uid"] = h.get("serviceaccount", "sandbox", RUNTIME)["metadata"]["uid"]
    old_pods = {pod["metadata"]["uid"] for pod in pods(h, RUNTIME)}
    old_control_pods = {pod["metadata"]["uid"] for pod in pods(h, CONTROL_NS)}
    require(old_pods and old_control_pods, "Legacy/control consumers are not actually running")
    h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin", user="old-agent", status=200)
    h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets", user="old-agent", status=200)
    before = snapshot_grants(h, spec)
    admission_cases(h, spec)
    token_secret_denials(h, before_enrollment=True)
    # The controller rejects the pre-existing token alias first, then the
    # watch-only group grant. Only explicit UID/RV-fenced fixture cleanup
    # removes them; the registration is left blocked on an incomplete review.
    blocked = block_prestaged_paths(h, spec, before)
    cli_failure(h.cli("authority", "enroll", "--sandbox-uid", spec["sandbox"]["uid"],
                     "--namespace-uid", spec["runtimeNamespace"]["uid"],
                     user="registrar", expected=None), "Every legacy binding", "missing binding reviews")
    current = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(current["metadata"]["uid"] == blocked["metadata"]["uid"] and current["spec"] == blocked["spec"]
            and current["metadata"]["generation"] == blocked["metadata"]["generation"],
            "Unreviewed CLI enrollment changed the blocked registration")
    require(snapshot_grants(h, spec) == before, "Unreviewed enrollment mutated legacy bindings")
    require(h.get("deployment", "sre", RUNTIME)["spec"]["replicas"] == 1, "Unreviewed enrollment stopped the consumer")
    h.passed("Unreviewed bindings/consumer cannot enroll or mutate an occupied legacy source")
    require(snapshot_grants(h, spec) == before, "Controller mutated grants before validating the entire review set")
    require(h.get("deployment", "sre", RUNTIME)["spec"]["replicas"] == 1, "Controller stopped an unreviewed consumer")
    require(h.get("secret", PRIVATE, RUNTIME) is None, "Controller issued private material for an incomplete review")
    h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin", user="old-agent", status=200)
    h.passed("Real registration/controller rejects an incomplete review before grant retirement, consumer stop or private issuance")

    # Refresh once after read-only tests; use the exact current RVs, not guessed
    # names or stale values from the fixture bootstrap.
    spec = enrollment_json(h.cli("authority", "preview", user="registrar"))
    reviewed = review_args(spec, current)
    preview = h.cli("authority", "enroll", *reviewed, "--dry-run", user="registrar")
    require(printed_object(preview)["spec"] == spec, "Enrollment dry-run differs from reviewed API identities")
    after = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(after["spec"] == current["spec"] and after["metadata"]["generation"] == current["metadata"]["generation"]
            and after["metadata"]["uid"] == current["metadata"]["uid"], "Enrollment dry-run mutated registration")
    reviewed = review_args(spec, after)
    h.cli("authority", "enroll", *reviewed, user="registrar")
    require(h.get("karssreregistrations.kars.azure.com", "canonical")["spec"] == spec,
            "Persisted enrollment differs from the exact reviewed identities")

    issuance_seen = False
    def migrated():
        nonlocal issuance_seen
        private = h.get("secret", PRIVATE, RUNTIME)
        if private and "kube-token" in private.get("data", {}) and not issuance_seen:
            # Real held bearer token, with no client certificate fallback.
            assert_denial(h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin",
                               user="old-agent"), "old held principal GET before private issuance")
            assert_denial(h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets",
                               user="old-agent"), "old held principal LIST before private issuance")
            assert_denial(h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets?watch=true&timeoutSeconds=1",
                               user="old-agent"), "old held principal WATCH before private issuance")
            require(not old_pods.intersection(pod["metadata"]["uid"] for pod in pods(h, RUNTIME)),
                    "Private credentials issued while a reviewed old consumer still exists")
            issuance_seen = True
        reg = h.get("karssreregistrations.kars.azure.com", "canonical")
        require(reg is not None, "Enrollment disappeared during migration")
        status = reg.get("status", {})
        require(not (status.get("phase") == "Blocked" and status.get("observedGeneration") == reg["metadata"]["generation"]),
                "Reviewed SRE migration was blocked")
        return reg if status.get("phase") == "Ready" and status.get("observedGeneration") == reg["metadata"]["generation"] else False
    reg = h.poll("reviewed migration and credential-order proof", migrated, seconds=240, interval=0.25)
    require(issuance_seen, "No private issuance was observed")
    h.cli("authority", "migrate", user="registrar", timeout=45)
    h.wait_ready()
    h.passed("Real old-principal GET/LIST/WATCH denial and old Pod termination precede private credential issuance")
    for namespace, cluster, name in [
        (CONTROL_NS, False, "e2e-old-watch-control"),
        (RUNTIME, False, "e2e-old-watch-private"),
        (CONTROL_NS, True, "e2e-old-watch-cluster"),
    ]:
        secret_watch(h, namespace, name, cluster=cluster)
    h.passed("Held legacy token receives actual HTTP Forbidden for namespaced and cluster Secret watches, including newly created dummy-Secret probes")
    require(h.get("serviceaccount", "sandbox", RUNTIME)["metadata"]["uid"] == h.state["old_agent_sa_uid"],
            "Migration used ServiceAccount recreation instead of revoking held-principal authorization")
    reader = h.get("clusterrolebinding", "kars-sre-reader")
    require(h.state["unrelated_subject"] in reader["subjects"], "Migration removed an unrelated binding subject")
    require(not any(subject.get("name") == "sandbox" and subject.get("namespace") == RUNTIME for subject in reader["subjects"]),
            "Legacy SRE subject remains bound")
    h.api("GET", f"/api/v1/namespaces/{CONTROL_NS}/secrets/router-services-admin", user="unrelated", status=200)
    h.passed("Unrelated binding subjects remain authorized; the old Sandbox ServiceAccount UID is preserved but denied")

    rotated = h.get("secret", "router-services-admin", CONTROL_NS)
    require(rotated["metadata"]["uid"] == h.state["control_secret_uid"], "Owned control Secret was replaced instead of rotated")
    require(fingerprint(rotated) != h.state["control_digest"], "Owned exposed control token was not rotated")
    current_pods = pods(h, CONTROL_NS)
    require(current_pods and not old_control_pods.intersection(pod["metadata"]["uid"] for pod in current_pods),
            "A startup-cached old control-token consumer survived Ready")
    expected = fingerprint(rotated)
    for pod in current_pods:
        require(pod["metadata"].get("annotations", {}).get(EPOCH) == reg["status"]["privacyEpoch"],
                "Control consumer lacks the new privacy epoch")
        cached = h.k("exec", "-n", CONTROL_NS, pod["metadata"]["name"], "-c", "credential-consumer",
                     "--", "cat", "/proof/cached.sha", timeout=25).split()[0]
        require(cached == expected, "Restarted control consumer did not cache the rotated token")
    h.passed("Owned router-services-admin token rotates and every cached old consumer is gone before Ready")
    h.cli("install", timeout=240)
    rollback_guard(h)
    h.state["legacy_registration_uid"] = reg["metadata"]["uid"]
    h.save()


def rollback_guard(h):
    # Execute the actual CLI guard against this API, without pretending a Kind
    # cluster has an Azure upgrade context or mocking an Azure command.
    script = """
const {pathToFileURL}=require('node:url');
const {execa}=await import(pathToFileURL(process.cwd()+'/cli/node_modules/execa/index.js'));
const {assertRollbackSafe}=await import(pathToFileURL(process.cwd()+'/cli/dist/lib/sre-authority.js'));
const execute=(file,args,options)=>execa(file,['--context','kind-kars-e2e',...args],options);
try { await assertRollbackSafe(execute); process.exitCode=9; }
catch(error) { if(!String(error.message).includes('Rollback across enrolled SRE authority is unsafe')) throw error; }
"""
    # CommonJS eval cannot use top-level await; the async wrapper keeps imports
    # on the actual compiled CLI module, not an alternate acceptance model.
    h.run(["node", "-e", f"(async()=>{{{script}}})().catch(()=>{{process.exitCode=1}})"], timeout=30)
    h.passed("Actual CLI rollback guard rejects regranting a legacy release while enrollment audit history exists")


def retire_and_uninstall(h):
    reg = h.get("karssreregistrations.kars.azure.com", "canonical")
    h.cli("authority", "retire", "--registration-uid", reg["metadata"]["uid"],
          "--resource-version", reg["metadata"]["resourceVersion"], user="registrar", timeout=240)
    retired = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(retired["status"]["phase"] == "Retired" and retired["spec"]["enabled"] is False,
            "Retirement did not complete")
    for name in (PRIVATE, AGENT):
        require(h.get("secret", name, RUNTIME) is None, "Private credential survived retirement")
    for kind in ("clusterrolebindings", "rolebindings"):
        items = json.loads(h.k("get", kind, "-A", "-o", "json"))["items"]
        require(not any(item["metadata"].get("annotations", {}).get("kars.azure.com/sre-registration-uid") == reg["metadata"]["uid"]
                        for item in items), "Owned private grant survived retirement")
    h.cli("uninstall", timeout=180)
    h.poll("SRE source removal", lambda: h.get("karssandbox", "sre", SYSTEM) is None, seconds=120)
    h.poll("SRE namespace removal", lambda: h.get("namespace", RUNTIME) is None, seconds=120)
    require(h.get("namespace", SYSTEM)["metadata"]["uid"] == h.state["system_uid"], "Retirement changed core namespace identity")
    retained = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(retained["metadata"]["uid"] == reg["metadata"]["uid"] and retained["status"]["phase"] == "Retired",
            "Retired registration audit record was removed or replaced")
    h.passed("Retire/uninstall revoke owned grants/credentials and remove the source namespace while retaining core UID and audit registration")


def fresh_reenrollment(h):
    policies_ready(h)
    retained = h.get("karssreregistrations.kars.azure.com", "canonical")
    require(retained and retained["status"]["phase"] == "Retired", "Fresh phase requires retained retired history")
    foreign = h.create({"apiVersion": "v1", "kind": "Namespace",
                        "metadata": {"name": RUNTIME, "labels": {"e2e-foreign-occupant": "true"}}})
    h.create({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "foreign-sentinel", "namespace": RUNTIME},
              "data": {"value": "must-remain"}})
    cli_failure(h.cli("authority", "stage-source", expected=None),
                "Existing kars-sre namespace", "foreign occupant")
    require(h.get("namespace", RUNTIME)["metadata"]["uid"] == foreign["metadata"]["uid"], "Foreign namespace was replaced")
    require(h.get("configmap", "foreign-sentinel", RUNTIME)["data"]["value"] == "must-remain", "Foreign content was mutated")
    require(h.get("karssandbox", "sre", SYSTEM) is None and h.get("secret", PRIVATE, RUNTIME) is None,
            "Foreign occupancy acquired a source or private privilege")
    h.namespace_delete(RUNTIME, foreign["metadata"]["uid"])
    h.passed("Foreign runtime occupancy blocks fresh staging before source creation or private issuance")

    created = h.cli("authority", "stage-source")
    spec = enrollment_json(h.cli("authority", "preview", user="registrar"))
    assert_claim(h.get("karssandbox", "sre", SYSTEM), h.get("namespace", RUNTIME),
                 h.get("namespace", SYSTEM), h.state["system_uid"])
    require(f'--sandbox-uid {spec["sandbox"]["uid"]}' in created
            and f'--namespace-uid {spec["runtimeNamespace"]["uid"]}' in created,
            "Fresh staging did not return the actual CREATE/claim UIDs")
    require(spec["sandbox"]["uid"] != h.state["legacy_source_uid"] and
            spec["runtimeNamespace"]["uid"] != h.state["legacy_namespace_uid"], "Fresh source reused old authority identities")
    cli_failure(h.cli("authority", "stage-source", expected=None), "SRE source already exists", "source adoption race")
    cli_failure(h.cli("authority", "enroll", "--sandbox-uid", h.state["legacy_source_uid"],
                     "--namespace-uid", h.state["legacy_namespace_uid"], user="registrar", expected=None),
                "no longer matches", "stale source UID enrollment")
    retained = h.get("karssreregistrations.kars.azure.com", "canonical")
    reviewed = review_args(spec, retained)
    dry = printed_object(h.cli("authority", "enroll", *reviewed, "--dry-run", user="registrar"))
    require(dry["spec"] == spec, "Fresh enrollment differs from its reviewed identities")
    h.cli("authority", "enroll", *reviewed, user="registrar")
    require(h.get("karssreregistrations.kars.azure.com", "canonical")["spec"] == spec,
            "Fresh CAS re-enrollment retained unreviewed identities from the retired registration")
    h.cli("authority", "migrate", user="registrar", timeout=240)
    h.wait_ready()
    h.cli("install", timeout=240)
    # Recreate only our test Role/Bindings in the newly claimed namespace.
    role = h.get("role", "e2e-sre-admission-probe", TENANT)
    h.create({"apiVersion": role["apiVersion"], "kind": "Role",
              "metadata": {"name": role["metadata"]["name"], "namespace": RUNTIME}, "rules": role["rules"]})
    for account, namespace in (("tenant", TENANT), ("registrar", OPERATORS)):
        h.create({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
            "metadata": {"name": f"e2e-sre-{account}-runtime", "namespace": RUNTIME},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": role["metadata"]["name"]},
            "subjects": [{"kind": "ServiceAccount", "name": account, "namespace": namespace}]})
    h.passed("Fresh atomic source/claim and delegated CAS re-enrollment reach Ready without adopting stale UIDs")
