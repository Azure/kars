# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit checks for the native probe; these are not Kubernetes API evidence."""

import contextlib
import copy
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import credential_schema as schema

PRIVATE = "DO-NOT-LOG-CREDENTIALS-OR-PRIVATE-API-BODIES"
TOKEN = "abc123abc123"
NAMESPACE = "kars-cel-" + TOKEN


def documents():
    crd = {
        "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
        "metadata": {"name": schema.CRD},
        "spec": {"scope": "Namespaced", "names": {"kind": "KarsCredentialGrant"},
                 "versions": [{"name": "v1alpha1", "served": True, "schema": {
                     "openAPIV3Schema": {"x-kubernetes-validations": [{
                         "rule": "self.metadata.name == 'workspace'", "message": "Canonical fixture invariant",
                     }]}}}]},
    }
    objects = [crd]
    for key, name in schema.POLICIES.items():
        spec = {"failurePolicy": "Fail", "matchConstraints": {"resourceRules": [{"resources": ["secrets"]}]},
                "matchConditions": [{"name": "unchanged", "expression": "true"}],
                "variables": [{"name": "unchanged", "expression": "true"}],
                "validations": [{"expression": "true", "message": "Other invariant"},
                                {"expression": "dyn(namespaceObject.metadata).uid != ''",
                                 "message": "Exact UID fixture invariant"}]}
        binding = {"policyName": name, "validationActions": ["Deny", "Audit"]}
        if key != "material":
            spec["validations"][1]["reason"] = "Forbidden"
        if key == "source-writes":
            spec["paramKind"] = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant"}
            binding["paramRef"] = {"name": "workspace", "parameterNotFoundAction": "Allow"}
        objects += [
            {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicy",
             "metadata": {"name": name}, "spec": spec},
            {"apiVersion": "admissionregistration.k8s.io/v1", "kind": "ValidatingAdmissionPolicyBinding",
             "metadata": {"name": name}, "spec": binding},
        ]
    return objects


def denied(policy="p", binding="p", message="Exact UID fixture invariant", name="probe", reason="Forbidden"):
    return {
        "kind": "Status", "status": "Failure", "reason": reason,
        "details": {"name": name, "causes": [{"message":
            f"ValidatingAdmissionPolicy '{policy}' with binding '{binding}' denied request: {message}"}]},
    }


class FixtureAPI:
    """In-memory transport fixture for verifying harness orchestration only."""
    def __init__(self):
        self.objects, self.calls, self.actor_calls = {}, [], []

    def request(self, _port, method, path, obj=None):
        self.calls.append((method, path, copy.deepcopy(obj)))
        if method == "GET":
            return (200, copy.deepcopy(self.objects[path])) if path in self.objects else (404, {})
        if method == "DELETE":
            if obj["preconditions"]["uid"] != self.objects[path]["metadata"]["uid"]:
                return 409, {}
            del self.objects[path]
            return 200, {"kind": "Status"}
        if "?dryRun=All" in path:
            if obj["metadata"]["name"] == "not-workspace":
                return 422, {"kind": "Status", "reason": "Invalid", "details": {
                    "name": "not-workspace", "causes": [{"reason": "FieldValueInvalid",
                        "message": "Canonical fixture invariant"}]}}
            return 201, copy.deepcopy(obj)
        if method == "PUT":
            self.objects[path.removesuffix("/status")] = copy.deepcopy(obj)
            return 200, copy.deepcopy(obj)
        result = copy.deepcopy(obj)
        result["metadata"].update(uid=f"native-uid-{len(self.objects)}",
                                  resourceVersion="1", generation=1)
        if result["kind"] == "CustomResourceDefinition":
            result["status"] = {"conditions": [{"type": "Established", "status": "True"}]}
        if result["kind"] == "ValidatingAdmissionPolicy":
            result["status"] = {"observedGeneration": 1, "typeChecking": {"expressionWarnings": []}}
        self.objects[path + "/" + result["metadata"]["name"]] = result
        return 201, copy.deepcopy(result)

    def actor(self, _port, path, obj, actor):
        self.actor_calls.append((path, copy.deepcopy(obj), actor))
        if path.endswith("/selfsubjectreviews"):
            return 201, {"status": {"userInfo": {"username": actor[0], "uid": actor[1]}}}
        if path.endswith("/selfsubjectaccessreviews"):
            verb = obj["spec"]["resourceAttributes"]["verb"]
            expected = "project-credentials" if actor[0].endswith(":kars-controller") else "use-agent-credentials"
            return 201, {"status": {"allowed": verb == expected}}
        resource = path.split("?")[0].rsplit("/", 1)[1]
        key = {"secrets": "source-writes", "configmaps": "material", "pods": "pods"}[resource]
        ns_uid = self.objects[f"/api/v1/namespaces/{NAMESPACE}"]["metadata"]["uid"]
        mismatch = obj["metadata"]["namespace"] != NAMESPACE if key == "source-writes" else (
            obj["metadata"]["annotations"]["kars.azure.com/privacy-namespace-uid"] != ns_uid)
        if mismatch:
            name = f"credential-cel-{TOKEN}-{key}"
            reason = "Invalid" if key == "material" else "Forbidden"
            return (422 if reason == "Invalid" else 403), denied(
                name, name, name=obj["metadata"]["name"], reason=reason)
        return 201, copy.deepcopy(obj)


class CredentialSchemaTests(unittest.TestCase):
    def test_source_extraction_preserves_selected_rendered_expressions(self):
        objects = documents()
        adjacent = "\n".join(json.dumps(obj) for obj in objects)
        crd, selected = schema.select_shipped(adjacent)
        self.assertEqual(crd, objects[0])
        for key, (policy, binding, validation) in selected.items():
            original = copy.deepcopy(policy)
            changed, scoped_binding = schema.scoped(policy, binding, TOKEN, NAMESPACE, key)
            for field in ("validations", "variables", "matchConditions", "paramKind"):
                self.assertEqual(changed["spec"].get(field), original["spec"].get(field))
            self.assertEqual(validation, original["spec"]["validations"][1])
            self.assertEqual(policy, original)
            self.assertEqual(changed["spec"]["matchConstraints"]["resourceRules"],
                             original["spec"]["matchConstraints"]["resourceRules"])
            self.assertEqual(scoped_binding["spec"]["validationActions"], ["Deny", "Audit"])
            if key == "source-writes":
                self.assertEqual(scoped_binding["spec"]["paramRef"], {
                    "name": "workspace", "namespace": NAMESPACE, "parameterNotFoundAction": "Allow"})

    def test_missing_duplicate_or_ambiguous_source_fails_closed(self):
        for mutate in (
            lambda values: values.pop(),
            lambda values: values.append(copy.deepcopy(values[0])),
            lambda values: values[1]["spec"]["validations"].append(
                {"expression": "namespaceObject != null", "message": "Unexpected additional UID gate"}),
            lambda values: values[1]["spec"].update(failurePolicy="Ignore"),
            lambda values: values[2]["spec"].update(validationActions=["Audit"]),
        ):
            values = documents()
            mutate(values)
            with self.subTest(mutate=mutate), self.assertRaises(schema.Failure):
                schema.select_shipped(json.dumps({"kind": "List", "items": values}))

    def test_render_converts_actual_templates_with_strict_pinned_context(self):
        seen = []
        def command(stage, args, **kwargs):
            seen.append((stage, args, kwargs))
            return "rendered chart" if stage == "credential-render" else json.dumps(
                {"kind": "List", "items": documents()})
        with patch.object(schema, "command", side_effect=command):
            schema.render(Path("."), NAMESPACE, "v1.31.0")
        self.assertEqual(seen[0][1].count("--show-only"), 3)
        for template in schema.TEMPLATES:
            self.assertIn("templates/" + template, seen[0][1])
        self.assertIn("--validate=strict", seen[1][1])
        self.assertIn(schema.CONTEXT, seen[1][1])
        self.assertEqual(seen[1][2]["data"], "rendered chart")

    def test_intended_denial_requires_exact_binding_message_reason_and_resource(self):
        validation = {"message": "Exact UID fixture invariant", "reason": "Forbidden"}
        self.assertTrue(schema.intended_denial(403, denied(), "p", "p", validation, "probe"))
        for code, body in (
            (403, {"kind": "Status", "reason": "Forbidden", "message": PRIVATE}),
            (403, denied(binding="other")), (403, denied(policy="other")),
            (403, denied(message="expression resulted in error: no such key: UID " + PRIVATE)),
            (403, denied(name="other")), (403, denied(reason="Invalid")),
            (422, denied()), (500, denied()), (403, None),
        ):
            self.assertFalse(schema.intended_denial(code, body, "p", "p", validation, "probe"))
        self.assertTrue(schema.intended_denial(
            422, denied(reason="Invalid"), "p", "p", {"message": validation["message"]}, "probe"))

    def test_singleton_cases_require_actual_root_rule_and_exact_invalid_cause(self):
        crd = documents()[0]
        rule = schema.singleton_rule(crd)
        body = {"kind": "Status", "reason": "Invalid", "details": {"name": "not-workspace", "causes": [
            {"reason": "FieldValueInvalid", "message": rule["message"]}]}}
        self.assertTrue(schema.singleton_denied(422, body, rule))
        for code, candidate in ((403, body), (422, None), (422, {"kind": "Status", "reason": "Invalid",
                "details": {"causes": [{"reason": "FieldValueInvalid", "message": PRIVATE}]}})):
            self.assertFalse(schema.singleton_denied(code, candidate, rule))
        crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["x-kubernetes-validations"] = []
        with self.assertRaises(schema.Failure):
            schema.singleton_rule(crd)

    def test_allowed_requires_native_object_identity_and_uid_annotations(self):
        fixture = {"kind": "ConfigMap", "metadata": {"name": "probe", "namespace": NAMESPACE,
                                                   "annotations": {"uid": "actual"}}}
        self.assertTrue(schema.allowed(201, copy.deepcopy(fixture), fixture))
        for mutation in ({"namespace": "other"}, {"annotations": {"uid": "wrong"}}, {"name": "other"}):
            body = copy.deepcopy(fixture)
            body["metadata"].update(mutation)
            self.assertFalse(schema.allowed(201, body, fixture))
        self.assertFalse(schema.allowed(403, fixture, fixture))

    def test_actor_transport_sends_only_native_uid_impersonation_over_loopback(self):
        actor = (f"system:serviceaccount:{NAMESPACE}:credential-writer", "native-uid")
        response = unittest.mock.MagicMock()
        response.code, response.read.return_value = 201, b'{"kind":"SelfSubjectReview"}'
        response.__enter__.return_value = response
        opener = unittest.mock.Mock()
        opener.open.return_value = response
        with patch.object(schema, "build_opener", return_value=opener):
            self.assertEqual(schema.as_actor(12345, "/review", {}, actor)[0], 201)
        req = opener.open.call_args.args[0]
        headers = {key.lower(): value for key, value in req.header_items()}
        self.assertEqual(req.full_url, "http://127.0.0.1:12345/review")
        self.assertEqual(headers["impersonate-uid"], actor[1])
        self.assertEqual(headers["impersonate-user"], actor[0])
        self.assertNotIn("authorization", headers)
        self.assertEqual(opener.open.call_args.kwargs["timeout"], 15)
        with self.assertRaises(schema.Failure):
            schema.as_actor(12345, "/review", {}, ("system:admin", PRIVATE))

    def test_orchestration_brackets_native_uid_negatives_without_secrets_or_scheduling(self):
        api = FixtureAPI()
        crd, selected = schema.select_shipped(json.dumps({"kind": "List", "items": documents()}))
        with patch.object(schema, "render", return_value=(crd, selected)), \
             patch.object(schema, "request", side_effect=api.request), \
             patch.object(schema, "as_actor", side_effect=api.actor), \
             contextlib.redirect_stdout(io.StringIO()):
            results = schema.exercise(Path("."), 1, "v1.31.0", TOKEN)
        self.assertEqual(len([r for r in results if r["category"] == "intended-denial"]), 4)
        self.assertEqual(api.objects, {})
        for path, body, _actor in api.actor_calls:
            if body.get("kind") in ("Secret", "ConfigMap", "Pod"):
                self.assertTrue(path.endswith("?dryRun=All"))
                self.assertNotIn("data", body)
                self.assertNotIn("stringData", body)
                if body["kind"] == "Pod":
                    self.assertEqual(body["spec"]["schedulerName"], "kars-e2e-admission-never-schedule")
                    self.assertFalse(body["spec"]["automountServiceAccountToken"])
        for method, path, body in api.calls:
            if method == "DELETE":
                self.assertTrue(body["preconditions"]["uid"].startswith("native-uid-"))
                self.assertNotIn("karssreregistrations", path)
                self.assertNotIn("namespaces/kars-system", path)

    def test_cleanup_never_deletes_replaced_or_unowned_resources(self):
        owned = schema.Owned(1)
        owned.resources = [("/api/v1/namespaces/owned", "original")]
        with patch.object(schema, "request", return_value=(200, {"metadata": {"uid": "replacement"}})) as request:
            with self.assertRaises(schema.Failure):
                owned.cleanup()
        self.assertEqual([call.args[1] for call in request.call_args_list], ["GET"])
        with patch.object(schema, "request", return_value=(409, {"message": PRIVATE})):
            with self.assertRaises(schema.Failure):
                schema.Owned(1).create("/fixture", {"kind": "Namespace", "metadata": {"name": "owned"}})

    def test_wait_is_bounded_and_never_treats_arbitrary_denial_as_success(self):
        with patch.object(schema.time, "monotonic", side_effect=[0, 0, 41]), \
             patch.object(schema.time, "sleep"):
            with self.assertRaises(schema.Failure):
                schema.wait_for(lambda: (403, {"message": PRIVATE}), lambda *_: False, "pods-negative")

    def test_ready_or_authorizer_failure_cannot_masquerade_as_namespace_uid_proof(self):
        fixture = {"kind": "Secret", "metadata": {"name": "probe", "namespace": NAMESPACE}}
        validation = {"message": "Exact UID fixture invariant", "reason": "Forbidden"}
        with patch.object(schema, "as_actor", return_value=(403, denied())), \
             patch.object(schema.time, "monotonic", side_effect=[0, 0, 0, 0, 41]), \
             patch.object(schema.time, "sleep"):
            with self.assertRaises(schema.Failure) as failure:
                schema.prove_pair(1, "source-writes", {"metadata": {"name": "p"}}, validation,
                                  ("unused", "unused"), fixture, fixture, "/good", "/bad")
        self.assertEqual(failure.exception.case, "source-writes-positive")

    def test_failed_native_run_retains_only_allowlisted_partial_evidence(self):
        def exercise(_root, _port, _version, _token, results):
            results.append(schema.evidence("source-writes-positive", 201, "accepted"))
            raise schema.Failure("pods-negative", 422)
        output = io.StringIO()
        with patch.object(schema, "kind_proxy", return_value=contextlib.nullcontext((1, {"gitVersion": "v1.31.0"}))), \
             patch.object(schema, "exercise", side_effect=exercise), \
             patch.object(Path, "mkdir"), patch.object(Path, "write_text") as write, \
             contextlib.redirect_stdout(output):
            self.assertEqual(schema.main(Path(".")), 1)
        cases = json.loads(write.call_args.args[0])["cases"]
        self.assertEqual([case["case"] for case in cases], ["source-writes-positive", "pods-negative"])
        self.assertTrue(all(set(case) == {"case", "httpStatus", "category"} for case in cases))

    def test_privacy_boundary_emits_only_fixed_cases_codes_and_categories(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            item = schema.evidence("pods-negative", 403, "intended-denial")
            with self.assertRaises(schema.Failure):
                schema.evidence(PRIVATE, 403, "failed")
        self.assertEqual(set(item), {"case", "httpStatus", "category"})
        self.assertNotIn(PRIVATE, output.getvalue())
        with patch.object(schema, "kind_proxy", side_effect=RuntimeError(PRIVATE)), \
             patch.object(Path, "mkdir"), patch.object(Path, "write_text") as write, \
             contextlib.redirect_stdout(output):
            self.assertEqual(schema.main(Path(".")), 1)
        self.assertNotIn(PRIVATE, output.getvalue() + write.call_args.args[0])

    def test_ci_runs_native_proof_before_existing_bootstrap_without_weakening_gates(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  sre-crd-schema:\n", 1)[1].split("  helm-lint:\n", 1)[0]
        self.assertIn("credential_schema_test", job)
        self.assertLess(job.index("registration_schema.py --exercise"),
                        job.index("python3 -m credential_schema\n"))
        self.assertLess(job.index("python3 -m credential_schema\n"),
                        job.index("bootstrap_probe --retirement-bind-proof"))
        self.assertIn(schema.REPORT, job)
        self.assertIn("if: ${{ !cancelled() && steps.sre_schema.outcome == 'success' }}", job)
        for forbidden in ("continue-on-error", "--validate=false", "cargo ", "needs:"):
            self.assertNotIn(forbidden, job)


if __name__ == "__main__":
    unittest.main()
