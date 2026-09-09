# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit-only harness/transport regression checks, never native CEL evidence."""

import base64
import contextlib
import copy
import io
import json
from pathlib import Path
import signal
import unittest
from unittest.mock import patch

import credential_policy_schema as schema
import credential_schema as shared
from sre_authority import registration_schema as transport

TOKEN = "abc123abc123"
NAMESPACE = "kars-policy-cel-" + TOKEN
PRIVATE = "DO-NOT-LOG-SECRET-OR-PRIVATE-API-BODY"


def documents():
    values = []
    for name, kind, plural in ((schema.GRANT, "KarsCredentialGrant", "karscredentialgrants"),
                               (schema.TASK, "KarsTask", "karstasks")):
        values.append({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": name}, "spec": {"group": "kars.azure.com", "scope": "Namespaced",
                "names": {"kind": kind, "plural": plural}, "versions": [{"name": "v1alpha1",
                    "served": True, "storage": True, "subresources": {"status": {}},
                    "schema": {"openAPIV3Schema": {"type": "object"}}}]}})
    for key, name in schema.POLICIES.items():
        spec = {
            "failurePolicy": "Fail", "matchConstraints": {
                "resourceRules": [{"apiGroups": [""], "apiVersions": ["v1"],
                                  "operations": ["CREATE", "UPDATE"], "resources": ["secrets"]}]},
            "matchConditions": [{"name": "unchanged", "expression": "true"}],
            "variables": [{"name": "unchanged", "expression": "true"}],
            "validations": [{"expression": "true", "message": f"Unit invariant {key} {index}"}
                            for index in range(1 if key == "store" else 3)]}
        binding = {"policyName": name, "validationActions": ["Deny", "Audit"]}
        if key == "store":
            spec["paramKind"] = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant"}
            binding["paramRef"] = {"name": "workspace", "parameterNotFoundAction": "Allow"}
        if key == "rebind":
            spec["validations"][0]["reason"] = "Forbidden"
        if key == "exposure":
            spec["matchConstraints"]["namespaceSelector"] = {
                "matchLabels": {"kars.azure.com/isolated": "strict"}}
            spec["matchConstraints"]["resourceRules"] = [
                {"apiGroups": [""], "apiVersions": ["v1"], "operations": ["CREATE", "UPDATE"],
                 "resources": ["services"]},
                {"apiGroups": ["networking.k8s.io"], "apiVersions": ["v1"],
                 "operations": ["CREATE", "UPDATE"], "resources": ["ingresses", "networkpolicies"]},
                {"apiGroups": ["gateway.networking.k8s.io"], "apiVersions": ["v1", "v1beta1"],
                 "operations": ["CREATE", "UPDATE"], "resources": ["httproutes", "tlsroutes", "tcproutes"]}]
            for validation in spec["validations"]:
                validation["reason"] = "Forbidden"
        values.extend([
            {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicy",
             "metadata": {"name": name, "labels": {"source": "unchanged"}}, "spec": spec},
            {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicyBinding",
             "metadata": {"name": name + "-binding"}, "spec": binding}])
    return values


def selected():
    return schema.select_shipped(json.dumps({"kind": "List", "items": documents()}))


def denied(policy, validation, name):
    return {"kind": "Status", "status": "Failure", "reason": validation.get("reason", "Invalid"),
            "details": {"name": name, "causes": [{"message":
                f"ValidatingAdmissionPolicy '{policy}' with binding '{policy}' denied request: "
                + validation["message"]}]}}


class FixtureAPI:
    """In-memory orchestration fixture only; does not compile/evaluate CEL."""
    def __init__(self):
        self.calls, self.objects = [], {}
        self.revision = 0

    def request(self, _port, method, path, obj=None):
        self.calls.append((method, path, copy.deepcopy(obj)))
        if path == "/api/v1/pods?limit=100":
            return 200, {"kind": "PodList", "metadata": {}, "items": []}
        if method == "GET":
            return (200, copy.deepcopy(self.objects[path])) if path in self.objects else (404, {})
        if method == "DELETE":
            if obj["preconditions"]["uid"] != self.objects[path]["metadata"]["uid"]:
                return 409, {}
            del self.objects[path]
            return 200, {"kind": "Status"}
        if "?dryRun=All" in path:
            key, index = None, None
            if obj["kind"] == "Secret":
                grant = self.objects[f"/apis/kars.azure.com/v1alpha1/namespaces/{NAMESPACE}"
                                     "/karscredentialgrants/workspace"]
                keys = set(obj.get("data", {})) | set(obj.get("stringData", {}))
                if (keys - {"FOUNDRY_API_KEY"} or obj["metadata"]["annotations"][schema.STORE_ANNOTATION]
                        != grant["metadata"]["uid"]):
                    key, index = "store", 0
            elif obj["kind"] == "KarsTask":
                old = self.objects[path.split("?")[0]]
                status = old["status"]
                if (status["executionPhase"] != "CredentialsPaused"
                        or status["observedGeneration"] != old["metadata"]["generation"]
                        or status.get("envelopeDigest") is not None
                        or status["conditions"][0]["status"] != "False"):
                    key, index = "rebind", 1
            elif obj["metadata"]["namespace"] == NAMESPACE:
                if obj["kind"] == "Service" and obj["spec"]["type"] != "ClusterIP":
                    key, index = "exposure", 0
                elif obj["kind"] == "Ingress":
                    key, index = "exposure", 1
                elif (obj["kind"] == "NetworkPolicy"
                      and obj["spec"]["ingress"][0]["from"][0]["ipBlock"]["cidr"] in ("0.0.0.0/0", "::/0")):
                    key, index = "exposure", 2
            if key:
                name = f"credential-cel-{TOKEN}-{key}"
                policy = self.objects[schema.ADMISSION + "/validatingadmissionpolicies/" + name]
                validation = policy["spec"]["validations"][index]
                return (403 if validation.get("reason") == "Forbidden" else 422), denied(
                    name, validation, obj["metadata"]["name"])
            result = copy.deepcopy(obj)
            if result["kind"] == "Secret" and "stringData" in result:
                data = result.setdefault("data", {})
                data.update({key: base64.b64encode(value.encode()).decode()
                             for key, value in result.pop("stringData").items()})
            return (200 if method == "PUT" else 201), result
        target = path.removesuffix("/status") if method == "PUT" else path + "/" + obj["metadata"]["name"]
        if method == "POST" and target in self.objects:
            return 409, {"message": PRIVATE}
        result = copy.deepcopy(obj)
        self.revision += 1
        result["metadata"].setdefault("uid", f"native-uid-{self.revision}")
        result["metadata"].setdefault("generation", 1)
        result["metadata"]["resourceVersion"] = str(self.revision)
        if result["kind"] == "CustomResourceDefinition":
            result["status"] = {"conditions": [{"type": "Established", "status": "True"}]}
        if result["kind"] == "ValidatingAdmissionPolicy":
            result["status"] = {"observedGeneration": 1, "typeChecking": {"expressionWarnings": []}}
        self.objects[target] = result
        return (200 if method == "PUT" else 201), copy.deepcopy(result)


@contextlib.contextmanager
def fixture_transport(api):
    with patch.object(schema, "request", side_effect=api.request), \
         patch.object(shared, "request", side_effect=api.request):
        yield


class CredentialPolicySchemaTests(unittest.TestCase):
    def test_decoder_handles_adjacent_documents_lists_and_rejects_duplicates(self):
        objects = documents()
        expected = selected()
        adjacent = "\n".join(json.dumps(obj) for obj in objects)
        self.assertEqual(schema.select_shipped(adjacent), expected)
        nested = json.dumps({"kind": "List", "items": objects[:2]}) + json.dumps(
            {"kind": "List", "items": objects[2:]})
        self.assertEqual(schema.select_shipped(nested), expected)
        with self.assertRaises(shared.Failure):
            schema.select_shipped(adjacent + json.dumps(objects[0]))

    def test_only_fixture_names_and_namespace_selectors_change(self):
        crds, sources = selected()
        original = copy.deepcopy((crds, sources))
        for key, (source, binding) in sources.items():
            policy, scoped_binding = shared.scoped(source, binding, TOKEN, NAMESPACE, key)
            expected = copy.deepcopy(source["spec"])
            expected["matchConstraints"].setdefault("namespaceSelector", {}).setdefault(
                "matchExpressions", []).append({"key": schema.LABEL, "operator": "In", "values": [TOKEN]})
            self.assertEqual(policy["spec"], expected)
            expected_binding = copy.deepcopy(binding["spec"])
            expected_binding["policyName"] = policy["metadata"]["name"]
            self.assertEqual(scoped_binding["spec"], expected_binding)
            self.assertEqual(policy["metadata"]["name"], scoped_binding["metadata"]["name"])
        self.assertEqual((crds, sources), original)
        exposure = sources["exposure"][0]["spec"]["matchConstraints"]
        self.assertEqual(exposure["namespaceSelector"]["matchLabels"], {"kars.azure.com/isolated": "strict"})
        self.assertEqual(exposure["resourceRules"][-1]["resources"], ["httproutes", "tlsroutes", "tcproutes"])

    def test_missing_ambiguous_or_weakened_sources_fail_closed(self):
        for mutate in (
            lambda values: values.pop(),
            lambda values: values[0]["spec"].update(scope="Cluster"),
            lambda values: values[2]["spec"].update(failurePolicy="Ignore"),
            lambda values: values[3]["spec"].update(validationActions=["Audit"]),
            lambda values: values[4]["spec"]["validations"].pop(),
            lambda values: values.append({**copy.deepcopy(values[3]), "metadata": {"name": "other-binding"}}),
        ):
            objects = documents()
            mutate(objects)
            with self.subTest(mutate=mutate), self.assertRaises(schema.Failure):
                schema.select_shipped(json.dumps({"kind": "List", "items": objects}))

    def test_render_uses_only_shipped_templates_strict_conversion_and_exact_context(self):
        def run(stage, _args, **_kwargs):
            return "rendered public chart" if stage.endswith("-render") else json.dumps(
                {"kind": "List", "items": documents()})
        with patch.object(schema, "command", side_effect=run) as command:
            schema.render(Path("."), NAMESPACE, "v1.31.0")
        helm, kubectl = command.call_args_list
        self.assertEqual(helm.args[1].count("--show-only"), 5)
        self.assertEqual(helm.args[1][0], "helm")
        self.assertIn("--kube-version", helm.args[1])
        for template in schema.TEMPLATES:
            self.assertIn("templates/" + template, helm.args[1])
        self.assertIn("--validate=strict", kubectl.args[1])
        self.assertIn("kind-kars-e2e", kubectl.args[1])
        self.assertEqual(kubectl.kwargs["data"], "rendered public chart")
        self.assertIs(schema.kind_proxy, transport.kind_proxy)
        self.assertIs(schema.request, transport.request)

    def test_guard_refuses_non_kind_and_non_loopback_without_starting_proxy(self):
        for context, server in (("h100", "https://127.0.0.1:6443"),
                                ("kind-kars-e2e", "https://private.example:6443")):
            config = {"contexts": [{"name": context}], "clusters": [{"cluster": {"server": server}}]}
            with patch.object(transport, "command", return_value=json.dumps(config)), \
                 patch.object(transport.subprocess, "Popen") as popen:
                with self.assertRaises(RuntimeError), schema.kind_proxy(Path(".")):
                    self.fail("Unsafe context was entered")
            popen.assert_not_called()

    def test_native_update_acceptance_requires_200_and_original_uid(self):
        obj = {"apiVersion": "v1", "kind": "Secret", "metadata": {
            "name": "proof", "namespace": NAMESPACE, "uid": "actual", "annotations": {"enrolled": "actual"}}}
        self.assertTrue(schema.accepted(200, obj, obj, "PUT"))
        for code, mutation in ((201, {}), (200, {"uid": "replacement"}),
                               (200, {"annotations": {}}), (200, {"namespace": "other"})):
            bad = copy.deepcopy(obj)
            bad["metadata"].update(mutation)
            self.assertFalse(schema.accepted(code, bad, obj, "PUT"))
        self.assertFalse(schema.accepted(422, None, obj, "PUT"))

    def test_exact_policy_denial_never_accepts_native_validation_or_rbac_errors(self):
        obj = {"apiVersion": "v1", "kind": "Secret",
               "metadata": {"name": "proof", "namespace": NAMESPACE, "uid": "actual"}}
        policy, validation = {"metadata": {"name": "policy"}}, {"message": "Exact invariant"}
        good = denied("policy", validation, "proof")
        with patch.object(schema, "request", return_value=(422, good)):
            schema.dry_run(1, "PUT", "/fixture", obj, "store-data-path", None, policy, validation)
        bad_bodies = [None, {"kind": "Status", "reason": "Invalid", "message": PRIVATE},
                      denied("other", validation, "proof"),
                      denied("policy", {"message": PRIVATE}, "proof"),
                      denied("policy", validation, "other")]
        wrong_binding = copy.deepcopy(good)
        wrong_binding["details"]["causes"][0]["message"] = wrong_binding["details"]["causes"][0][
            "message"].replace("binding 'policy'", "binding 'other'")
        bad_bodies.append(wrong_binding)
        for body in bad_bodies:
            with self.subTest(body=body), patch.object(schema, "request", return_value=(422, body)), \
                 self.assertRaises(schema.Failure) as failure:
                schema.dry_run(1, "PUT", "/fixture", obj, "store-data-path", None,
                               policy, validation, warm=True)
            self.assertEqual(failure.exception.category, "native-error")
        with patch.object(schema, "request", return_value=(403, good)), self.assertRaises(schema.Failure):
            schema.dry_run(1, "PUT", "/fixture", obj, "store-data-path", None, policy, validation)

    def test_warm_up_retries_only_acceptance_and_is_bounded(self):
        obj = {"apiVersion": "v1", "kind": "Secret",
               "metadata": {"name": "proof", "namespace": NAMESPACE, "uid": "actual"}}
        policy, validation = {"metadata": {"name": "policy"}}, {"message": "Exact invariant"}
        with patch.object(schema, "request", side_effect=[
                (200, obj), (422, denied("policy", validation, "proof"))]) as request, \
             patch.object(schema.time, "sleep"):
            schema.dry_run(1, "PUT", "/fixture", obj, "store-data-path", None,
                           policy, validation, warm=True)
        self.assertEqual(request.call_count, 2)
        with patch.object(schema.time, "monotonic", side_effect=[0, 0, 31]), \
             patch.object(schema.time, "sleep"), patch.object(schema, "request", return_value=(200, obj)), \
             self.assertRaises(schema.Failure):
            schema.dry_run(1, "PUT", "/fixture", obj, "store-data-path", None,
                           policy, validation, warm=True)

    def test_type_warnings_fail_closed_without_emitting_warning_bodies(self):
        api = FixtureAPI()
        base = api.request
        def warning(port, method, path, obj=None):
            code, body = base(port, method, path, obj)
            if method == "GET" and "/validatingadmissionpolicies/" in path:
                body["status"]["typeChecking"]["expressionWarnings"] = [{"warning": PRIVATE}]
            return code, body
        api.request = warning
        output = io.StringIO()
        with fixture_transport(api), contextlib.redirect_stdout(output), \
             self.assertRaises(schema.Failure) as failure:
            schema.install(1, schema.Fixtures(1), *selected(), NAMESPACE, TOKEN, [])
        self.assertEqual(failure.exception.case, "store-policy")
        self.assertEqual(failure.exception.category, "type-warning")
        self.assertNotIn(PRIVATE, output.getvalue() + str(failure.exception))
        self.assertFalse(any("/validatingadmissionpolicybindings" in path for _, path, _ in api.calls))

    def test_typechecking_requires_current_observed_generation_and_status(self):
        for status in ({}, {"observedGeneration": 0, "typeChecking": {}}, {"observedGeneration": 1}):
            api = FixtureAPI()
            base = api.request
            def incomplete(port, method, path, obj=None):
                code, body = base(port, method, path, obj)
                if method == "GET" and "/validatingadmissionpolicies/" in path:
                    body["status"] = status
                return code, body
            api.request = incomplete
            def once(probe, predicate, case, **_kwargs):
                code, body = probe()
                if not predicate(code, body):
                    raise schema.Failure(case)
                return code, body
            with self.subTest(status=status), fixture_transport(api), \
                 patch.object(schema, "wait_for", side_effect=once), \
                 contextlib.redirect_stdout(io.StringIO()), self.assertRaises(schema.Failure):
                schema.install(1, schema.Fixtures(1), *selected(), NAMESPACE, TOKEN, [])

    def test_store_wire_maps_keep_real_identity_and_use_only_fixture_values(self):
        stored = {"metadata": {"uid": "actual", "annotations": {schema.STORE_ANNOTATION: "grant"}},
                  "type": "Opaque"}
        for representation in ("data", "string", "mixed-data", "mixed-string"):
            obj = schema.store_payload(stored, representation, "PATH")
            self.assertEqual(obj["metadata"], stored["metadata"])
            self.assertEqual(obj["type"], "Opaque")
            self.assertEqual("data" in obj and "stringData" in obj, representation.startswith("mixed-"))
            field = "data" if representation.endswith("data") else "stringData"
            self.assertIn("PATH", obj[field])
        self.assertNotIn("data", stored)

    def test_secret_positive_requires_normalized_requested_data_not_an_empty_response(self):
        original = {"apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                    "metadata": {"name": "proof", "namespace": NAMESPACE, "uid": "actual"}}
        fixture = schema.store_payload(original, "mixed-string", "FOUNDRY_API_KEY")
        result = copy.deepcopy(original)
        self.assertFalse(schema.accepted(200, result, fixture, "PUT"))
        result["data"] = {"FOUNDRY_API_KEY": base64.b64encode(b"public-admission-fixture-only").decode()}
        self.assertTrue(schema.accepted(200, result, fixture, "PUT"))
        result["data"]["PATH"] = result["data"]["FOUNDRY_API_KEY"]
        self.assertFalse(schema.accepted(200, result, fixture, "PUT"))

    def test_orchestration_has_exact_cases_actual_uid_refs_status_fixture_and_no_execution(self):
        api = FixtureAPI()
        results = []
        with fixture_transport(api), patch.object(schema, "render", return_value=selected()), \
             contextlib.redirect_stdout(io.StringIO()):
            schema.exercise(Path("."), 1, "v1.31.0", TOKEN, results)
        self.assertEqual(len(results), 41)
        self.assertEqual(len([r for r in results if r["category"] == "intended-denial"]), 18)
        self.assertEqual(api.objects, {})
        self.assertEqual(results[-1], {"case": "cleanup", "httpStatus": 0, "category": "cleaned"})
        self.assertEqual(len({r["case"] for r in results}), len(results))
        creates = [(path, body) for method, path, body in api.calls
                   if method == "POST" and "?dryRun" not in path]
        self.assertTrue(all(obj["kind"] in {"Namespace", "CustomResourceDefinition", "Secret",
            "KarsCredentialGrant", "KarsTask", "ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"}
            for _, obj in creates))
        secret_index = next(i for i, (_, obj) in enumerate(creates) if obj["kind"] == "Secret")
        grant_index = next(i for i, (_, obj) in enumerate(creates) if obj["kind"] == "KarsCredentialGrant")
        self.assertLess(secret_index, grant_index)
        store = creates[grant_index][1]["spec"]["integrationStores"][0]
        self.assertEqual(store["purpose"], "foundry")
        self.assertTrue(store["secret"]["uid"].startswith("native-uid-"))
        self.assertEqual(set(store["secret"]), {"name", "uid"})
        self.assertNotIn(schema.STORE_ANNOTATION, creates[secret_index][1]["metadata"].get("annotations", {}))
        statuses = []
        for method, path, obj in api.calls:
            self.assertNotIn("karssreregistrations", path)
            self.assertNotIn("namespaces/kars-system", path)
            self.assertNotIn("/token", path)
            if method == "DELETE":
                self.assertTrue(obj["preconditions"]["uid"].startswith("native-uid-"))
            if obj and obj.get("kind") == "KarsTask":
                self.assertTrue(obj["spec"]["execution"]["launch"])
                if path.endswith("/status"):
                    self.assertEqual(obj["metadata"]["annotations"][schema.PENDING], "true")
                    statuses.append(obj["status"])
                elif method == "PUT":
                    self.assertTrue(path.endswith("?dryRun=All"))
                    self.assertNotIn(schema.PENDING, obj["metadata"]["annotations"])
            if obj and obj.get("kind") in ("Service", "Ingress", "NetworkPolicy"):
                self.assertTrue(path.endswith("?dryRun=All"))
        self.assertNotIn("envelopeDigest", statuses[1])
        self.assertIn("envelopeDigest", statuses[2])
        self.assertIsNone(statuses[2]["envelopeDigest"])
        self.assertEqual(statuses[4]["executionPhase"], "Running")
        self.assertEqual(statuses[5]["observedGeneration"], 0)
        self.assertEqual(statuses[6]["conditions"][0]["status"], "True")

    def test_unchanged_detects_dry_run_mutation_and_positive_cannot_mask_denials(self):
        original = {"metadata": {"uid": "actual", "resourceVersion": "1"}}
        with patch.object(schema, "request", return_value=(200, {"metadata": {
                "uid": "actual", "resourceVersion": "2"}})), self.assertRaises(schema.Failure):
            schema.unchanged(1, "/owned", original, "store-unchanged")
        api = FixtureAPI()
        base = api.request
        def always_allow(port, method, path, obj=None):
            if "?dryRun" in path:
                return (200 if method == "PUT" else 201), copy.deepcopy(obj)
            return base(port, method, path, obj)
        api.request = always_allow
        def once(probe, predicate, case, **_kwargs):
            code, body = probe()
            if not predicate(code, body):
                raise schema.Failure(case)
            return code, body
        with fixture_transport(api), patch.object(schema, "render", return_value=selected()), \
             patch.object(schema, "wait_for", side_effect=once), \
             contextlib.redirect_stdout(io.StringIO()), self.assertRaises(schema.Failure):
            schema.exercise(Path("."), 1, "v1.31.0", TOKEN, [])
        self.assertEqual(api.objects, {})

    def test_controller_presence_and_paginated_inventory_refuse_execution_fixture(self):
        cases = [
            {"kind": "PodList", "metadata": {"continue": "more"}, "items": []},
            {"kind": "PodList", "items": [{"metadata": {"namespace": "kars-system", "name": "controller"}}]},
            {"kind": "PodList", "items": [{"metadata": {"namespace": "kube-system", "name": "kars-controller"}}]},
        ]
        for body in cases:
            with patch.object(schema, "request", return_value=(200, body)), self.assertRaises(schema.Failure):
                schema.no_custom_controllers(1)
        allowed = {"kind": "PodList", "items": [
            {"metadata": {"namespace": "kube-system", "name": "kube-controller-manager-kars-e2e-control-plane"}},
            {"metadata": {"namespace": "local-path-storage", "name": "local-path-provisioner-abc12-def34"}}]}
        with patch.object(schema, "request", return_value=(200, allowed)):
            schema.no_custom_controllers(1)

    def test_cleanup_preserves_recreated_children_and_their_parent_namespace_and_crd(self):
        owned = schema.Fixtures(1)
        owned.resources = [
            ("/api/v1/namespaces/owned", "namespace-uid"),
            (schema.CRDS + "/" + schema.TASK, "crd-uid"),
            ("/apis/kars.azure.com/v1alpha1/namespaces/owned/karstasks/fixture", "original-uid")]
        with patch.object(shared, "request", return_value=(200, {
                "metadata": {"uid": "replacement-uid"}})) as request, self.assertRaises(schema.Failure):
            owned.cleanup()
        self.assertEqual([call.args[1] for call in request.call_args_list], ["GET"])

    def test_cleanup_uses_delete_uid_preconditions_and_does_not_adopt_collisions(self):
        api = FixtureAPI()
        owned = schema.Fixtures(1)
        obj = {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "owned"}}
        with fixture_transport(api):
            first = owned.create("/api/v1/namespaces", obj)
            collision = schema.Fixtures(1)
            with self.assertRaises(schema.Failure):
                collision.create("/api/v1/namespaces", obj)
            collision.cleanup()
            self.assertEqual(len(api.objects), 1)
            owned.cleanup()
        deletes = [body for method, _, body in api.calls if method == "DELETE"]
        self.assertEqual(deletes[0]["preconditions"], {"uid": first["metadata"]["uid"]})
        self.assertEqual(api.objects, {})

    def test_unexpected_native_errors_propagate_and_cleanup_still_runs(self):
        api = FixtureAPI()
        with fixture_transport(api), patch.object(schema, "render", return_value=selected()), \
             patch.object(schema, "prove_store", side_effect=schema.Failure("store-data-path", 422,
                                                                          "native-error")), \
             contextlib.redirect_stdout(io.StringIO()), self.assertRaises(schema.Failure):
            schema.exercise(Path("."), 1, "v1.31.0", TOKEN, [])
        self.assertEqual(api.objects, {})

    def test_time_limit_interrupts_and_restores_prior_signal_state(self):
        handlers = []
        def install(_signal, handler):
            handlers.append(handler)
        with patch.object(schema.signal, "getsignal", return_value=signal.SIG_DFL), \
             patch.object(schema.signal, "getitimer", return_value=(0, 0)), \
             patch.object(schema.signal, "signal", side_effect=install), \
             patch.object(schema.signal, "setitimer") as timer:
            with self.assertRaises(schema.Failure) as failure, schema.time_limit(150, "deadline"):
                handlers[0](signal.SIGALRM, None)
        self.assertEqual(failure.exception.case, "deadline")
        self.assertEqual(timer.call_args_list[0].args, (signal.ITIMER_REAL, 150))
        self.assertEqual(timer.call_args_list[-1].args, (signal.ITIMER_REAL, 0))
        self.assertEqual(handlers[-1], signal.SIG_DFL)

    def test_partial_report_is_bounded_and_never_logs_arbitrary_bodies(self):
        def exercise(_root, _port, _version, _token, results):
            results.append(schema.evidence("store-data-allowed", 200, "accepted"))
            raise RuntimeError(PRIVATE)
        output = io.StringIO()
        with patch.object(schema, "kind_proxy", return_value=contextlib.nullcontext(
                (1, {"gitVersion": "v1.31.0"}))), patch.object(schema, "exercise", side_effect=exercise), \
             patch.object(Path, "mkdir"), patch.object(Path, "write_text") as write, \
             contextlib.redirect_stdout(output):
            self.assertEqual(schema.main(Path(".")), 1)
        raw = write.call_args.args[0]
        self.assertNotIn(PRIVATE, raw + output.getvalue())
        self.assertLessEqual(len(raw), 16384)
        for case in json.loads(raw)["cases"]:
            self.assertEqual(set(case), {"case", "httpStatus", "category"})
        for case, code, category in ((PRIVATE, 200, "failed"), ("complete", PRIVATE, "failed"),
                                     ("complete", 200, PRIVATE)):
            with self.assertRaises(schema.Failure):
                schema.evidence(case, code, category)

    def test_ci_adds_gate_after_cleanup_before_unchanged_failure_independent_bootstrap(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  sre-crd-schema:\n", 1)[1].split("  helm-lint:\n", 1)[0]
        self.assertIn("credential_policy_schema_test", job)
        self.assertIn(schema.REPORT, job)
        self.assertIn("timeout-minutes: 10", job)
        self.assertLess(job.index("python3 -m credential_schema\n"),
                        job.index("python3 -m credential_policy_schema\n"))
        self.assertLess(job.index("python3 -m credential_policy_schema\n"),
                        job.index("bootstrap_probe --retirement-bind-proof"))
        self.assertIn("if: ${{ !cancelled() && steps.sre_schema.outcome == 'success' }}\n"
                      "        id: sre_bootstrap\n"
                      "        run: PYTHONPATH=tests/e2e python3 -m sre_authority.bootstrap_probe --retirement-bind-proof", job)
        for forbidden in ("continue-on-error", "--validate=false", "cargo ", "needs:"):
            self.assertNotIn(forbidden, job)


if __name__ == "__main__":
    unittest.main()
