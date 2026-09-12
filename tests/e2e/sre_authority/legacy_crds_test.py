# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure historical fixture checks, not a substitute for hosted Kind acceptance."""

import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
import types
import unittest
from unittest.mock import Mock, patch

from sre_authority.legacy_crds import (
    CRDS, IDENTITIES, preflight_legacy_crds, render_legacy_crds, validate_rendered_crds,
)
from sre_authority.registration_schema import CRD_NAME
from sre_authority.registration_schema import request
from sre_authority.canonical_migration import seed_data
from sre_authority.canonical_migration_test import FakeHarness
from sre_authority.canonical_seed import (
    SEEDS, SeedRejected, collection_path, dry_run_seed_data, nested_action_definition,
    prove_nested_params_support, request_seed, seed_definitions, seed_status,
)


def historical_objects():
    def obj(plural, kind, scope):
        return {
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": f"{plural}.kars.azure.com", "labels": {"app.kubernetes.io/name": "kars"}},
            "spec": {"group": "kars.azure.com", "scope": scope, "names": {"plural": plural, "kind": kind},
                     "versions": [{"name": "v1alpha1", "served": True, "storage": True,
                                   "schema": {"openAPIV3Schema": {"type": "object"}}}]},
        }
    result = {filename: [obj(*identity)] for filename, identity in CRDS.items()}
    result["crd.yaml"].append(obj(*IDENTITIES[-1]))
    return result


def flattened(historical):
    return [obj for objects in historical.values() for obj in objects]


class LegacyCRDTests(unittest.TestCase):
    def test_render_requires_exact_historical_content_not_just_a_matching_name(self):
        historical = historical_objects()
        rendered = flattened(copy.deepcopy(historical))
        self.assertEqual(validate_rendered_crds(rendered, historical), rendered)
        rendered[0]["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "changed"
        with self.assertRaises(AssertionError):
            validate_rendered_crds(rendered, historical)

    def test_current_registration_extra_missing_duplicate_and_foreign_resources_are_rejected(self):
        historical = historical_objects()
        valid = flattened(copy.deepcopy(historical))
        registration = copy.deepcopy(valid[0])
        registration["metadata"]["name"] = CRD_NAME
        for rendered in (valid[:-1], valid + [registration], [registration] + valid[1:],
                         [valid[0]] + valid[:-1], [{"kind": "Secret"}] + valid[1:]):
            with self.subTest(count=len(rendered)), self.assertRaises(AssertionError):
                validate_rendered_crds(rendered, historical)
        with self.assertRaises(AssertionError):
            validate_rendered_crds(valid, {**historical, "crd-karssreregistration.yaml": registration})

    def test_historical_identity_schema_and_preexisting_ownership_are_strict(self):
        def changed(section, key, value):
            obj = historical_objects()
            obj["crd.yaml"][0][section][key] = value
            return obj
        invalid = [
            changed("metadata", "name", "other.kars.azure.com"),
            changed("metadata", "annotations", {"meta.helm.sh/release-name": "other"}),
            changed("metadata", "ownerReferences", [{"uid": "foreign"}]),
            changed("spec", "group", "foreign.example"),
            changed("spec", "scope", "Cluster"),
            changed("spec", "names", {"plural": "karssandboxes", "kind": "Secret"}),
            changed("spec", "versions", []),
        ]
        for historical in invalid:
            with self.subTest(historical=historical["crd.yaml"]), self.assertRaises(AssertionError):
                validate_rendered_crds(flattened(historical), historical)

    def test_changed_extracted_source_and_unexpected_inventory_fail_before_render(self):
        h = Mock()
        class Chart:
            def __truediv__(self, _name):
                return self

            def glob(self, _pattern):
                return [types.SimpleNamespace(name=filename) for filename in CRDS]

            def read_text(self):
                return "changed"
        for sources in ({filename: "original" for filename in CRDS},
                        {filename: "{{ templated }}" for filename in CRDS},
                        {"crd-karssreregistration.yaml": "unexpected"}):
            with self.assertRaises(AssertionError):
                render_legacy_crds(h, Chart(), sources)
        h.run.assert_not_called()
        h.k.assert_not_called()

    def test_every_absence_is_read_only_before_native_helm_creates_any_crd(self):
        h = Mock()
        h.api.return_value = types.SimpleNamespace(json=lambda: {"kind": "Status", "reason": "NotFound"})
        objects = flattened(historical_objects())
        with patch("sre_authority.legacy_crds.render_legacy_crds", return_value=objects):
            preflight_legacy_crds(h, Path("unused-test-chart"), {})
        names = [CRD_NAME] + [obj["metadata"]["name"] for obj in objects]
        self.assertEqual(len(names), 19)
        self.assertEqual([call.args[1].rsplit("/", 1)[-1] for call in h.api.call_args_list], names)
        self.assertTrue(all(call.args[0] == "GET" and call.kwargs == {"status": 404}
                            for call in h.api.call_args_list))
        h.create.assert_not_called()
        h.k.assert_not_called()
        h.passed.assert_called_once()

    def test_existing_or_inaccessible_crd_aborts_without_adoption_or_retry(self):
        for code in (200, 401, 403, 409, 500):
            h = Mock()
            h.api.side_effect = AssertionError(f"HTTP {code}")
            with patch("sre_authority.legacy_crds.render_legacy_crds",
                       return_value=flattened(historical_objects())):
                with self.subTest(code=code), self.assertRaisesRegex(AssertionError, f"HTTP {code}"):
                    preflight_legacy_crds(h, Path("unused-test-chart"), {})
            h.api.assert_called_once()
            h.create.assert_not_called()
            h.passed.assert_not_called()

    def test_false_not_found_is_not_absence_proof(self):
        for body in ({"kind": "Secret", "reason": "NotFound"}, {"kind": "Status", "reason": "Forbidden"}):
            h = Mock()
            h.api.return_value = types.SimpleNamespace(json=lambda: body)
            with patch("sre_authority.legacy_crds.render_legacy_crds",
                       return_value=flattened(historical_objects())), self.assertRaises(AssertionError):
                preflight_legacy_crds(h, Path("unused-test-chart"), {})
            h.create.assert_not_called()
            h.passed.assert_not_called()

    def test_initial_helm_uses_existing_versioned_waiter_without_custom_creation(self):
        fixture = Path(__file__).with_name("fixtures.py").read_text()
        install = fixture.split("def install_historical_chart(h):", 1)[1].split("def prepare_legacy(h):", 1)[0]
        self.assertLess(install.index("preflight_legacy_crds(h,"), install.index('h.run(["helm", "install"'))
        self.assertIn('sre_migration_helm_wait_arg "$2"', install)
        self.assertIn('wait_arg, "--timeout", "120s"', install)
        self.assertLess(install.index('h.run(["helm", "install"'), install.index('h.get("toolpolicy"'))
        self.assertNotIn("h.create(", install)
        for bypass in ("--take-ownership", "--force", "--validate=false", "--no-hooks", "governance.enabled=false"):
            self.assertNotIn(bypass, fixture)


class CanonicalSeedProbeTests(unittest.TestCase):
    """Contract/privacy/transport tests; only the hosted API can accept a body."""

    def setUp(self):
        reporter = patch("sre_authority.canonical_seed.write_report")
        self.reporter = reporter.start()
        self.addCleanup(reporter.stop)

    def test_historical_typed_fields_and_every_original_case_are_preserved(self):
        definitions = dict(seed_definitions())
        self.assertEqual(list(definitions), [row[0] for row in SEEDS])
        for resource, _plural, kind in SEEDS:
            obj = definitions[resource]
            self.assertEqual(obj["kind"], kind)
            self.assertEqual(obj["apiVersion"], "kars.azure.com/v1alpha1")
            self.assertEqual(obj["metadata"], {"name": f"e2e-migration-{resource}", "namespace": "kars-system"})
            self.assertEqual(set(obj), {"apiVersion", "kind", "metadata", "spec"})
        for resource in ("karstask", "karsteam"):
            envelope = definitions[resource]["spec"]["envelope"]
            self.assertEqual(envelope, {"tier": 1, "authorityCeiling": 1, "budget": {"tokens": 20, "usdMicros": 0}})
            self.assertTrue(all(type(value) is int for value in
                                [envelope["tier"], envelope["authorityCeiling"], *envelope["budget"].values()]))
        self.assertEqual(definitions["karstask"]["spec"]["execution"], {"launch": False})
        self.assertIs(definitions["karstask"]["spec"]["execution"]["launch"], False)
        self.assertEqual(definitions["karsteam"]["spec"]["roster"], [])
        self.assertEqual(definitions["mcpserver"]["spec"],
                         {"url": "https://migration-fixture.invalid/", "productionMode": False})
        self.assertIs(definitions["mcpserver"]["spec"]["productionMode"], False)
        self.assertEqual(definitions["karseval"]["spec"],
                         {"corpus": {"builtin": "sre"}, "targetSandboxRef": {"name": "sre"}})
        self.assertEqual(definitions["karssreaction"]["spec"], {
            "action": {"type": "ScaleDeployment", "params": {
                "namespace": "kars-system", "name": "kars-controller", "replicas": 0,
                "opaque": "retained"}},
            "approval": {"state": "Rejected"}})
        definitions["karstask"]["spec"]["envelope"]["tier"] = 9
        self.assertEqual(dict(seed_definitions())["karstask"]["spec"]["envelope"]["tier"], 1)
        self.assertEqual(definitions["karsteam"]["spec"]["envelope"]["tier"], 1)

    def test_original_nested_shape_is_preserved_on_both_correct_schema_sides_with_distinct_names(self):
        baseline = dict(seed_definitions())["karssreaction"]
        before = nested_action_definition(after_migration=False)
        after = nested_action_definition(after_migration=True)
        self.assertEqual(before["spec"], after["spec"])
        for obj in (before, after):
            self.assertEqual(obj["spec"]["action"]["params"]["opaque"], {"nested": [1, "retained", True]})
            scalar = copy.deepcopy(obj)
            scalar["metadata"]["name"] = baseline["metadata"]["name"]
            scalar["spec"]["action"]["params"]["opaque"] = "retained"
            self.assertEqual(scalar, baseline)
        self.assertEqual(len({obj["metadata"]["name"] for obj in (baseline, before, after)}), 3)

    def test_bad_request_diagnostics_only_expose_fixed_kind_categories_and_paths(self):
        message = ('Secret-value cannot unmarshal; strict decoding error: '
                   'unknown field "spec.action.params.opaque.nested", unknown field "secret-value"')
        report = seed_status("karssreaction", 400, {"kind": "Status", "reason": "BadRequest",
            "message": message, "details": {"name": "secret-value", "causes": [
                {"field": "spec.approval.state", "reason": "FieldValueInvalid", "message": "secret-value"},
                {"field": "spec.secret-value", "reason": "secret-value"},
                {"field": ["secret-value"], "reason": ["secret-value"]},
            ]}})
        self.assertEqual(report, {"kind": "KarsSREAction", "httpStatus": 400, "category": "BadRequest",
            "fields": ["spec.action.params.opaque.nested", "spec.approval.state"],
            "validation": ["invalid-field", "strict-decoding", "type-mismatch", "unknown-field"]})
        self.assertNotIn("secret-value", json.dumps(report).lower())
        for body in (None, [], {"kind": "Secret"}, {"kind": "Status", "reason": [], "details": []}):
            self.assertEqual(seed_status("karstask", 400, body)["category"], "unexpected-response")

    def test_real_http_transport_uses_exact_resource_json_media_and_strict_server_dry_run(self):
        seen = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                seen.append((self.path, self.headers["Content-Type"],
                             json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                body = json.dumps({"kind": "Status", "reason": "BadRequest",
                                   "message": 'unknown field "spec.execution.launch"'}).encode()
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            def api(method, path, *, body):
                code, result = request(server.server_port, method, path, body)
                return types.SimpleNamespace(status_code=code, json=lambda: result)
            h = types.SimpleNamespace(root=Path("unused"), api=api)
            obj = dict(seed_definitions())["karstask"]
            with self.assertRaisesRegex(SeedRejected, "KarsTask server-dry-run.*HTTP 400"):
                request_seed(h, "karstask", obj, dry_run=True)
            self.assertEqual(seen, [(collection_path("karstask") + "?fieldManager=kubectl-create&fieldValidation=Strict&dryRun=All",
                                     "application/json", obj)])
            self.assertEqual(self.reporter.call_args.args[2]["fields"], ["spec.execution.launch"])
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive())

    def test_all_five_dry_runs_leave_no_ephemeral_uid_data_or_workload_persisted(self):
        h = FakeHarness()
        before = copy.deepcopy(h.objects)
        dry_run_seed_data(h)
        self.assertEqual(h.objects, before)
        posts = [(path, body) for method, path, body in h.calls if method == "POST"]
        expected = seed_definitions() + [("karssreaction", nested_action_definition(after_migration=False))]
        self.assertEqual(posts, [(collection_path(resource) + "?fieldManager=kubectl-create&fieldValidation=Strict&dryRun=All", obj)
                                for resource, obj in expected])
        self.assertTrue(all(method in ("GET", "POST") for method, _path, _body in h.calls))
        self.assertNotIn("ephemeral-dry-run", json.dumps(list(h.objects.values())))
        self.assertEqual(self.reporter.call_args.args[2], {
            "kind": "KarsSREAction", "httpStatus": 400, "category": "BadRequest",
            "fields": ["spec.action.params.opaque.nested"], "validation": ["strict-decoding", "unknown-field"],
            "expectedHttpStatus": 400, "matched": True, "mode": "nested-before-server-dry-run"})

    def test_historical_negative_requires_the_exact_native_rejection_not_any_failure(self):
        faults = (
            (403, {"kind": "Status", "reason": "Forbidden"}),
            (400, {"kind": "Status", "reason": "BadRequest",
                   "message": 'strict decoding error: unknown field "spec.action.params.name"'}),
            (400, {"kind": "Status", "reason": "BadRequest",
                   "message": 'strict decoding error: unknown field "spec.action.params.opaque.nested", unknown field "unreviewed"'}),
            (400, {"kind": "Status", "reason": "BadRequest",
                   "message": 'unknown field "spec.action.params.opaque.nested"'}),
            (422, {"kind": "Status", "reason": "Invalid"}),
            (201, nested_action_definition(after_migration=False)),
        )
        for code, body in faults:
            h = FakeHarness()
            original = h.api
            def api(method, path, **kwargs):
                result = original(method, path, **kwargs)
                if method == "POST" and kwargs["body"] == nested_action_definition(after_migration=False):
                    return types.SimpleNamespace(status_code=code, json=lambda: body)
                return result
            h.api = api
            before = copy.deepcopy(h.objects)
            with self.subTest(code=code, body=body), self.assertRaisesRegex(AssertionError, "exact expected API result"):
                dry_run_seed_data(h)
            self.assertEqual(h.objects, before)

    def test_post_migration_nested_acceptance_retains_scalar_data_and_all_identities_without_persistence(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        h.migrate_action_schema()
        before = copy.deepcopy(h.objects)
        h.calls.clear()
        prove_nested_params_support(h)
        self.assertEqual(h.objects, before)
        for fixture in fixtures:
            self.assertEqual(h.get(fixture["resource"], fixture["name"])["metadata"]["uid"], fixture["before"]["uid"])
        self.assertEqual([body for method, _path, body in h.calls if method == "POST"],
                         [nested_action_definition(after_migration=True)])
        self.assertTrue(all(method == "GET" or method == "POST" and "dryRun=All" in path
                            for method, path, _body in h.calls))
        self.assertTrue(self.reporter.call_args.args[2]["matched"])
        self.assertEqual(self.reporter.call_args.args[2]["expectedHttpStatus"], 201)

    def test_post_migration_acceptance_cannot_prune_change_or_add_nested_values_or_forge_ready(self):
        changes = (
            lambda body: body["spec"]["action"]["params"]["opaque"].pop("nested"),
            lambda body: body["spec"]["action"]["params"]["opaque"].update(nested=[1, "changed", True]),
            lambda body: body["spec"]["action"]["params"]["opaque"].update(nested=[1, "retained", 1]),
            lambda body: body["spec"]["action"]["params"]["opaque"].update(extra="unreviewed"),
            lambda body: body.update(status={"phase": "Ready"}),
        )
        for change in changes:
            h = FakeHarness()
            h.migrate_action_schema()
            original = h.api
            def api(method, path, **kwargs):
                result = original(method, path, **kwargs)
                if method == "POST":
                    body = result.json()
                    change(body)
                    return types.SimpleNamespace(status_code=201, json=lambda: body)
                return result
            h.api = api
            before = copy.deepcopy(h.objects)
            with self.subTest(change=change), self.assertRaisesRegex(AssertionError, "exact expected API result"):
                prove_nested_params_support(h)
            self.assertEqual(h.objects, before)

    def test_post_migration_snapshot_rejects_schema_cas_data_and_workload_drift(self):
        for fault in ("uid", "resourceVersion", "schema", "data", "workload", "persisted-probe"):
            h = FakeHarness()
            seed_data(h)
            h.migrate_action_schema()
            original = h.api
            def api(method, path, **kwargs):
                result = original(method, path, **kwargs)
                if method == "POST":
                    crd = h.objects[("crd", "karssreactions.kars.azure.com")]
                    if fault in ("uid", "resourceVersion"):
                        crd["metadata"][fault] = "changed"
                    elif fault == "schema":
                        crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "changed"
                    elif fault == "data":
                        h.objects[("karssreaction", "e2e-migration-karssreaction")]["spec"]["approval"]["state"] = "Pending"
                    elif fault == "workload":
                        h.objects[("deployment", "kars-controller")]["spec"]["template"] = {"changed": True}
                    else:
                        h.create(kwargs["body"])
                return result
            h.api = api
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "changed|already exists"):
                prove_nested_params_support(h)

    def test_post_migration_probe_requires_paused_controller_and_no_same_name_object(self):
        for fault in ("controller", "collision"):
            h = FakeHarness()
            h.migrate_action_schema()
            if fault == "controller":
                h.objects[("deployment", "kars-controller")]["spec"]["replicas"] = 1
            else:
                h.create(nested_action_definition(after_migration=True))
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "paused|already exists"):
                prove_nested_params_support(h)
            self.assertTrue(all(method == "GET" for method, _path, _body in h.calls))

    def test_documented_params_schema_is_accepted_but_extra_validation_is_not_ignored(self):
        for phase in (False, True):
            for constraint in ({"maxProperties": 1}, {"properties": {"opaque": {"type": "string"}}},
                               {"additionalProperties": False}):
                h = FakeHarness()
                if phase:
                    h.migrate_action_schema()
                crd = h.objects[("crd", "karssreactions.kars.azure.com")]
                params = crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["action"]["properties"]["params"]
                self.assertIn("description", params)
                params.update(constraint)
                probe = prove_nested_params_support if phase else dry_run_seed_data
                with self.subTest(phase=phase, constraint=constraint), self.assertRaisesRegex(AssertionError, "wrong side"):
                    probe(h)
                self.assertFalse(any(method == "POST" and body == nested_action_definition(after_migration=phase)
                                     for method, _path, body in h.calls))

    def test_all_failed_bodies_are_identified_before_any_real_seed_creation(self):
        h = FakeHarness()
        original = h.api
        def api(method, path, **kwargs):
            if method == "POST":
                original(method, path, **kwargs)
                return types.SimpleNamespace(status_code=400, json=lambda: {
                    "kind": "Status", "reason": "BadRequest", "message": "private-unretained-message"})
            return original(method, path, **kwargs)
        h.api = api
        before = copy.deepcopy(h.objects)
        with self.assertRaisesRegex(AssertionError, "5 historical seed bodies"):
            seed_data(h)
        self.assertEqual(h.objects, before)
        reports = [call.args[2] for call in self.reporter.call_args_list
                   if call.args[2]["category"] != "requesting"]
        self.assertEqual([report["kind"] for report in reports], [row[2] for row in SEEDS])
        self.assertTrue(all(report["httpStatus"] == 400 for report in reports))
        self.assertNotIn("private-unretained-message", json.dumps(reports))
        self.assertTrue(all("dryRun=All" in path for method, path, _body in h.calls if method == "POST"))

    def test_successful_http_status_cannot_hide_pruning_type_change_ready_or_wrong_identity(self):
        obj = dict(seed_definitions())["karstask"]
        changes = (
            lambda result: result["spec"].pop("execution"),
            lambda result: result["spec"]["execution"].update(launch=0),
            lambda result: result.update(status={"phase": "Ready"}),
            lambda result: result["metadata"].update(name="another"),
            lambda result: result["metadata"].pop("resourceVersion"),
        )
        for change in changes:
            result = copy.deepcopy(obj)
            result["metadata"].update(uid="real-fixture", resourceVersion="1")
            change(result)
            h = types.SimpleNamespace(root=Path("unused"), api=lambda *_args, **_kwargs:
                                      types.SimpleNamespace(status_code=201, json=lambda: result))
            with self.subTest(result=result), self.assertRaisesRegex(SeedRejected, "identity-or-data-round-trip"):
                request_seed(h, "karstask", obj, dry_run=False)

    def test_live_data_uid_rv_and_workload_mutations_during_dry_run_fail(self):
        for fault in ("data", "uid", "resourceVersion", "workload", "new-seed"):
            h = FakeHarness()
            prior = dict(seed_definitions())["karstask"]
            prior["metadata"]["name"] = "existing-task"
            h.create(prior)
            original = h.api
            def api(method, path, **kwargs):
                result = original(method, path, **kwargs)
                if method == "POST":
                    item = h.objects[("karstask", "existing-task")]
                    if fault == "data":
                        item["spec"]["objective"] = "changed"
                    elif fault in ("uid", "resourceVersion"):
                        item["metadata"][fault] = "changed"
                    elif fault == "workload":
                        h.objects[("deployment", "kars-controller")]["spec"]["template"] = {"changed": True}
                    elif fault == "new-seed":
                        h.create(dict(seed_definitions())["karseval"])
                return result
            h.api = api
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "changed|already exists"):
                dry_run_seed_data(h)

    def test_paged_missing_identity_and_oversized_inventory_stop_before_any_post(self):
        for items, metadata in (([], {"continue": "opaque"}), ([{}], {}), ([{}] * 513, {})):
            h = FakeHarness()
            h.api = Mock(return_value=types.SimpleNamespace(status_code=200, json=lambda: {
                "kind": "List", "metadata": metadata, "items": items}))
            with self.subTest(metadata=metadata), self.assertRaisesRegex(AssertionError, "inventory"):
                dry_run_seed_data(h)
            self.assertTrue(all(call.args[0] == "GET" for call in h.api.call_args_list))

    def test_early_existing_legacy_probe_uses_shared_bodies_before_current_schema_or_helm_stage(self):
        source = Path(__file__).with_name("legacy_crd_probe.py").read_text()
        self.assertIn("from sre_authority.canonical_seed import dry_run_seed_data", source)
        self.assertIn(r'r"v1\.31\.\d+(?:[-+].*)?"', source)
        self.assertLess(source.index("install_historical_chart(h)"), source.index("dry_run_seed_data(h)"))
        self.assertLess(source.index("dry_run_seed_data(h)"), source.index("create_registration_crd(h, obj)"))
        self.assertLess(source.index("dry_run_seed_data(h)"), source.index('"--dry-run=server"'))
        self.assertIn('"historicalSeedStrictServerDryRuns": 5', source)
        self.assertIn('"historicalNestedParamsRejection": "passed"', source)
        self.assertNotIn("--validate=false", source)


if __name__ == "__main__":
    unittest.main()
