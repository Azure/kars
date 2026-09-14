# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit/transport contracts only; field-ownership causation requires native API evidence."""

import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import subprocess
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from sre_authority.canonical_migration_test import FakeHarness
from sre_authority.schema_preparation_diagnostics import schema_preparation_failure
from sre_authority.ssa_diagnostics import MANAGERS, PATHS, ssa_conflict
from sre_authority.task_schema_conflicts import TASK_NAME, TASK_PATH, task_manager_facts, task_schema_conflict
from sre_authority.task_schema_preview import exercise_task_restore_preview


class TaskSchemaPreviewTests(unittest.TestCase):
    def setUp(self):
        self.reports = []
        for module in ("task_schema_preview", "task_schema_conflicts"):
            reporter = patch(f"sre_authority.{module}.write_report", side_effect=lambda _root, file, facts:
                             self.reports.append((file, copy.deepcopy(facts))))
            reporter.start()
            self.addCleanup(reporter.stop)

    def harness(self, failure_at=None, metadata_conflict=None, returned_change=None):
        h = FakeHarness()
        h.root = Path(__file__).resolve().parents[3]
        target = copy.deepcopy(h.objects[("crd", TASK_NAME)])
        for key in ("uid", "resourceVersion", "managedFields"):
            target["metadata"].pop(key)
        target["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "Current public chart fixture"
        target["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["numeric"] = {
            "type": "number", "minimum": 1e-7, "maximum": 1e21}
        h.rendered = json.dumps(target)
        def run(args, data=None, timeout=20):
            if args[0] == "helm":
                return json.dumps(h.manifest) if args[1:3] == ["get", "manifest"] else h.rendered
            self.assertEqual(args[:2], ["node", str(h.root / "tests/e2e/sre_authority/task_schema_payload.mjs")])
            result = subprocess.run(args, cwd=h.root, input=data, capture_output=True, text=True,
                                    timeout=timeout, check=False)
            if result.returncode:
                self.fail(f"Production fixture helper rejected input at {args[2]}")
            return result.stdout
        h.run = run
        h.previews = []
        h.raw_previews = []
        owned_apply = h.k
        def k(*args, data, **kwargs):
            if "--validate=strict" in args:
                return owned_apply(*args, data=data, **kwargs)
            self.assertEqual(args, ("apply", "--server-side", "--field-manager=helm", "-f", "-", "-o", "json",
                                    "--dry-run=server", "--request-timeout=20s"))
            self.assertEqual(kwargs, {"expected": None, "timeout": 25})
            obj = json.loads(data)
            current = h.objects[("crd", TASK_NAME)]
            self.assertEqual(obj["metadata"]["uid"], current["metadata"]["uid"])
            self.assertEqual(obj["metadata"]["resourceVersion"], current["metadata"]["resourceVersion"])
            self.assertEqual(obj["spec"], target["spec"])
            self.assertNotIn("managedFields", obj["metadata"])
            annotations = obj["metadata"]["annotations"]
            self.assertEqual(annotations["kars.azure.com/core-schema-owner"],
                             '{"namespace":"kars-system","ownership":"helm","release":"kars"}')
            self.assertRegex(annotations["kars.azure.com/core-schema-spec"], r"^[0-9a-f]{64}$")
            h.previews.append(copy.deepcopy(obj))
            h.raw_previews.append(data)
            if metadata_conflict is not None:
                self.assertIn(metadata_conflict, annotations)
                return SimpleNamespace(returncode=1, stdout="", stderr=
                    f'error: Apply failed with 1 conflict: conflict with "Python-urllib" using apiextensions.k8s.io/v1: .metadata.annotations.{metadata_conflict}\n')
            if len(h.previews) == failure_at:
                return SimpleNamespace(returncode=1, stdout="", stderr=
                    'error: Apply failed with 1 conflict: conflict with "Python-urllib" using apiextensions.k8s.io/v1: .spec.versions\n')
            if returned_change:
                returned_change(obj)
            return SimpleNamespace(returncode=0, stdout=json.dumps(obj), stderr="")
        h.k = k
        return h

    def test_full_payload_bytes_match_direct_production_builder_including_js_numeric_digest(self):
        h = self.harness()
        current = copy.deepcopy(h.objects[("crd", TASK_NAME)])
        script = """
import {readFileSync} from 'node:fs';
import {schemaDocuments} from './cli/dist/lib/schema-documents.js';
import {buildSchemaWriteRequest} from './cli/dist/lib/schema-write-request.js';
const {rendered,current}=JSON.parse(readFileSync(0,'utf8'));
const request=buildSchemaWriteRequest(schemaDocuments(rendered)[0],current,
    {namespace:'kars-system',release:'kars',ownership:'helm'});
process.stdout.write(JSON.stringify({args:request.args,input:JSON.stringify(request.object)}));
"""
        result = subprocess.run(["node", "--input-type=module", "-e", script], cwd=h.root,
                                input=json.dumps({"rendered": h.rendered, "current": current}),
                                text=True, capture_output=True, timeout=20, check=True)
        expected = json.loads(result.stdout)
        exercise_task_restore_preview(h)
        self.assertEqual(h.raw_previews[0], expected["input"])
        self.assertIn('"minimum":1e-7', h.raw_previews[0])
        self.assertNotIn('"minimum":1e-07', h.raw_previews[0])
        self.assertEqual(expected["args"], ["apply", "--server-side", "--field-manager=helm", "-f", "-", "-o", "json"])

    def test_metadata_only_owner_and_digest_conflicts_cannot_false_pass(self):
        for annotation in ("kars.azure.com/core-schema-owner", "kars.azure.com/core-schema-spec"):
            h = self.harness(metadata_conflict=annotation)
            before = copy.deepcopy(h.objects)
            with self.subTest(annotation=annotation), self.assertRaisesRegex(AssertionError, "SSA server-preview failed"):
                exercise_task_restore_preview(h)
            self.assertEqual(h.objects, before)
            self.assertEqual(self.reports[-1][1]["conflictFields"], [f"metadata/annotations/{annotation}"])
            self.assertFalse(any(method == "PATCH" for method, _path, _body in h.calls))

    def test_exact_production_response_validation_rejects_added_schema_and_foreign_owner(self):
        changes = [
            lambda obj: obj["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"].update(
                unexpected={"type": "string"}),
            lambda obj: obj["metadata"]["annotations"].update({"meta.helm.sh/release-name": "foreign"}),
            lambda obj: obj["metadata"].update(resourceVersion="different"),
        ]
        for change in changes:
            h = self.harness(returned_change=change)
            before = copy.deepcopy(h.objects)
            with self.subTest(change=change), self.assertRaisesRegex(AssertionError, "helper rejected input at validate"):
                exercise_task_restore_preview(h)
            self.assertEqual(h.objects, before)
            self.assertFalse(any(method == "PATCH" for method, _path, _body in h.calls))

    def test_early_probe_uses_four_original_identity_applies_and_exact_nonpersistent_previews(self):
        h = self.harness()
        original = copy.deepcopy(h.objects)
        exercise_task_restore_preview(h)
        self.assertEqual(len(h.previews), 3)
        patches = h.owned_mutations
        self.assertEqual(len(patches), 4)
        self.assertEqual(len(h.owned_previews), 4)
        self.assertEqual(patches[0]["metadata"]["annotations"], {
            "meta.helm.sh/release-name": "foreign-fixture", "meta.helm.sh/release-namespace": "kars-system"})
        self.assertEqual(patches[1]["spec"], original[("crd", TASK_NAME)]["spec"])
        self.assertEqual(patches[2]["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"],
                         "Unreviewed public fixture description")
        self.assertEqual(patches[3]["spec"], original[("crd", TASK_NAME)]["spec"])
        self.assertFalse(any(method == "PATCH" for method, _path, _body in h.calls))
        current = h.objects[("crd", TASK_NAME)]
        self.assertEqual(current["spec"], original[("crd", TASK_NAME)]["spec"])
        self.assertEqual(current["metadata"]["uid"], original[("crd", TASK_NAME)]["metadata"]["uid"])
        for before, after in zip(original[("crd", TASK_NAME)]["metadata"]["managedFields"], current["metadata"]["managedFields"]):
            self.assertEqual({key: value for key, value in before.items() if key != "time"},
                             {key: value for key, value in after.items() if key != "time"})
        self.assertEqual([facts["case"] for _file, facts in self.reports if "nonPersistent" in facts],
                         ["before-negatives", "after-owner-restore", "after-schema-restore"])

    def test_first_preview_failure_does_not_mutate_negative_fixtures_or_hide_conflicts(self):
        h = self.harness(failure_at=1)
        original = copy.deepcopy(h.objects)
        with self.assertRaisesRegex(AssertionError, "Task SSA server-preview failed"):
            exercise_task_restore_preview(h)
        self.assertEqual(h.objects, original)
        self.assertFalse(any(method == "PATCH" for method, _path, _body in h.calls))
        self.assertEqual(self.reports[-1][1]["conflictFields"], ["spec/versions"])

    def test_after_restore_conflict_fails_explicitly_without_force_retries_or_schema_writes(self):
        h = self.harness(failure_at=3)
        original = copy.deepcopy(h.objects[("crd", TASK_NAME)])
        with self.assertRaisesRegex(AssertionError, "Task SSA server-preview failed"):
            exercise_task_restore_preview(h)
        self.assertEqual(len(h.previews), 3)
        self.assertEqual(h.objects[("crd", TASK_NAME)]["spec"], original["spec"])
        facts = self.reports[-1][1]
        self.assertEqual(facts["case"], "after-schema-restore")
        self.assertEqual(facts["conflictManagers"], ["Python-urllib"])
        self.assertEqual(facts["conflictKind"], "field-manager")

    def test_restoration_refuses_external_changes_instead_of_overwriting_them(self):
        h = self.harness()
        with self.assertRaisesRegex(AssertionError, "changed externally"):
            with task_schema_conflict(h, "schema"):
                h.objects[("crd", TASK_NAME)]["spec"]["external"] = "retained"
        self.assertEqual(h.objects[("crd", TASK_NAME)]["spec"]["external"], "retained")
        self.assertEqual(len(h.owned_mutations), 1)

    def test_foreign_initial_owner_is_not_adopted(self):
        h = self.harness()
        h.objects[("crd", TASK_NAME)]["metadata"]["annotations"]["meta.helm.sh/release-name"] = "foreign"
        with self.assertRaisesRegex(AssertionError, "foreign CRD ownership"):
            with task_schema_conflict(h, "owner"):
                self.fail("Foreign owner entered the fixture")
        self.assertFalse(any(method == "PATCH" for method, _path, _body in h.calls))

    def test_original_apply_identity_is_required_not_just_the_helm_manager_name(self):
        for fault in ("update", "version", "coowner", "same-name-update", "ancestor"):
            h = self.harness()
            rows = h.objects[("crd", TASK_NAME)]["metadata"]["managedFields"]
            if fault == "update":
                rows[0]["operation"] = "Update"
            elif fault == "version":
                rows[0]["apiVersion"] = "apiextensions.k8s.io/v1beta1"
            else:
                other = copy.deepcopy(rows[0])
                other["operation"] = "Update"
                other["manager"] = "helm" if fault == "same-name-update" else "external"
                other["fieldsV1"] = {"f:spec": {} if fault == "ancestor" else {"f:versions": {}}}
                rows.append(other)
            before = copy.deepcopy(h.objects)
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "helper rejected input at owned-request"):
                with task_schema_conflict(h, "schema"):
                    self.fail("Unqualified field owner entered the fixture")
            self.assertEqual(h.objects, before)
            self.assertEqual(h.owned_mutations, [])
            self.assertEqual(h.owned_previews, [])

    def test_all_owned_fields_are_sent_while_foreign_fields_and_original_values_are_preserved(self):
        h = self.harness()
        obj = h.objects[("crd", TASK_NAME)]
        obj["metadata"]["labels"]["fixture.example/owned"] = "retained"
        obj["metadata"]["annotations"]["external.example/note"] = "untouched"
        obj["spec"]["names"]["shortNames"] = ["owned-task"]
        h.manifest["spec"]["names"]["shortNames"] = ["owned-task"]
        fields = obj["metadata"]["managedFields"][0]["fieldsV1"]
        fields["f:metadata"]["f:labels"]["f:fixture.example/owned"] = {}
        fields["f:spec"]["f:names"]["f:shortNames"] = {}
        obj["metadata"]["managedFields"].append({
            "manager": "external", "operation": "Update", "apiVersion": "apiextensions.k8s.io/v1",
            "fieldsType": "FieldsV1", "time": "2026-09-12T00:00:00Z",
            "fieldsV1": {"f:metadata": {"f:annotations": {"f:external.example/note": {}}}},
        })
        before = copy.deepcopy(obj)
        for fault in ("owner", "schema"):
            with task_schema_conflict(h, fault):
                self.assertEqual(h.objects[("crd", TASK_NAME)]["metadata"]["annotations"]["external.example/note"], "untouched")
        after = h.objects[("crd", TASK_NAME)]
        self.assertEqual(after["spec"], before["spec"])
        for key in ("name", "uid", "labels", "annotations"):
            self.assertEqual(after["metadata"][key], before["metadata"][key])
        self.assertEqual(after["metadata"]["managedFields"][1], before["metadata"]["managedFields"][1])
        self.assertEqual(len(h.owned_mutations), 4)
        for body in h.owned_mutations:
            self.assertEqual(body["metadata"]["labels"]["fixture.example/owned"], "retained")
            self.assertNotIn("external.example/note", body["metadata"]["annotations"])
            self.assertEqual(body["spec"]["names"]["shortNames"], ["owned-task"])
            self.assertNotIn("managedFields", body["metadata"])
            self.assertNotIn("status", body)
        self.assertTrue(all(facts["originalHelmApplyOwnershipPreserved"] for _file, facts in self.reports))

    def test_omitted_owned_empty_spec_value_comes_only_from_the_matching_release(self):
        h = self.harness()
        obj = h.objects[("crd", TASK_NAME)]
        obj["metadata"]["managedFields"][0]["fieldsV1"]["f:spec"]["f:names"]["f:categories"] = {}
        h.manifest["spec"]["names"]["categories"] = []
        with task_schema_conflict(h, "schema"):
            self.assertNotIn("categories", h.objects[("crd", TASK_NAME)]["spec"]["names"])
        self.assertTrue(all(body["spec"]["names"]["categories"] == [] for body in h.owned_mutations))
        self.assertNotIn("categories", h.objects[("crd", TASK_NAME)]["spec"]["names"])

    def test_missing_owned_metadata_or_mismatched_manifest_is_not_guessed(self):
        for fault in ("metadata", "manifest", "indexed-fields"):
            h = self.harness()
            obj = h.objects[("crd", TASK_NAME)]
            if fault == "metadata":
                obj["metadata"]["managedFields"][0]["fieldsV1"]["f:metadata"]["f:labels"]["f:missing"] = {}
                h.manifest["metadata"]["labels"]["missing"] = "not-live"
            elif fault == "manifest":
                h.manifest["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"] = "different-release"
            else:
                obj["metadata"]["finalizers"] = ["fixture"]
                obj["metadata"]["managedFields"][0]["fieldsV1"]["f:metadata"]["f:finalizers"] = {'v:"fixture"': {}}
            before = copy.deepcopy(h.objects)
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "helper rejected input at owned-request"):
                with task_schema_conflict(h, "schema"):
                    self.fail("Unproven payload entered the fixture")
            self.assertEqual(h.objects, before)
            self.assertEqual(h.owned_mutations, [])

    def test_unplanned_managed_fields_drift_never_triggers_ownership_reset(self):
        for fault in ("external-owner", "apply-to-update", "owned-field-change"):
            h = self.harness()
            with self.subTest(fault=fault), self.assertRaisesRegex(AssertionError, "helper rejected input at owned-request"):
                with task_schema_conflict(h, "owner"):
                    rows = h.objects[("crd", TASK_NAME)]["metadata"]["managedFields"]
                    if fault == "external-owner":
                        rows.append({"manager": "external", "operation": "Update", "apiVersion": "apiextensions.k8s.io/v1",
                                     "fieldsType": "FieldsV1", "fieldsV1": {"f:metadata": {"f:labels": {"f:external": {}}}}})
                    elif fault == "apply-to-update":
                        rows[0]["operation"] = "Update"
                    else:
                        rows[0]["fieldsV1"]["f:metadata"]["f:labels"]["f:unexpected"] = {}
            self.assertEqual(len(h.owned_mutations), 1)
            self.assertEqual(h.objects[("crd", TASK_NAME)]["metadata"]["annotations"]["meta.helm.sh/release-name"], "foreign-fixture")

    def test_owned_preflight_rejects_field_ownership_expansion_before_any_real_mutation(self):
        h = self.harness()
        original_k = h.k
        def k(*args, **kwargs):
            result = original_k(*args, **kwargs)
            if "--dry-run=server" in args and "--validate=strict" in args:
                value = json.loads(result)
                value["metadata"]["managedFields"][0]["fieldsV1"]["f:metadata"]["f:labels"]["f:extra"] = {}
                return json.dumps(value)
            return result
        h.k = k
        before = copy.deepcopy(h.objects)
        with self.assertRaisesRegex(AssertionError, "helper rejected input at owned-check"):
            with task_schema_conflict(h, "schema"):
                self.fail("Ownership-expanding preview was applied")
        self.assertEqual(h.objects, before)
        self.assertEqual(h.owned_mutations, [])

    def test_owned_actual_write_still_cas_fails_when_rv_changes_after_dry_run(self):
        h = self.harness()
        original_k = h.k
        def k(*args, **kwargs):
            if "--dry-run=server" not in args:
                h.objects[("crd", TASK_NAME)]["metadata"]["resourceVersion"] = "99"
            return original_k(*args, **kwargs)
        h.k = k
        with self.assertRaises(AssertionError):
            with task_schema_conflict(h, "schema"):
                self.fail("Stale reviewed RV was applied")
        self.assertEqual(h.owned_mutations, [])
        self.assertEqual(h.objects[("crd", TASK_NAME)]["metadata"]["resourceVersion"], "99")

    def test_actual_kubectl_json_printer_requires_explicit_managed_fields(self):
        obj = FakeHarness().get("crd", TASK_NAME)
        seen = []
        routes = {
            "/api": {"apiVersion": "v1", "kind": "APIVersions", "versions": ["v1"]},
            "/api/v1": {"apiVersion": "v1", "kind": "APIResourceList", "groupVersion": "v1", "resources": []},
            "/apis": {"apiVersion": "v1", "kind": "APIGroupList", "groups": [{
                "name": "apiextensions.k8s.io", "versions": [{"groupVersion": "apiextensions.k8s.io/v1", "version": "v1"}],
                "preferredVersion": {"groupVersion": "apiextensions.k8s.io/v1", "version": "v1"}}]},
            "/apis/apiextensions.k8s.io/v1": {"apiVersion": "v1", "kind": "APIResourceList",
                "groupVersion": "apiextensions.k8s.io/v1", "resources": [{"name": "customresourcedefinitions",
                    "singularName": "customresourcedefinition", "kind": "CustomResourceDefinition",
                    "namespaced": False, "verbs": ["get"]}]},
            TASK_PATH: obj,
        }
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                seen.append((self.command, self.path, self.headers.get("Authorization")))
                value = routes.get(urlsplit(self.path).path)
                body = json.dumps(value if value is not None else {"kind": "Status", "reason": "NotFound", "code": 404}).encode()
                self.send_response(200 if value is not None else 404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            with tempfile.TemporaryDirectory() as cache:
                base = ["kubectl", "--kubeconfig=/dev/null", "--server", f"http://127.0.0.1:{server.server_port}",
                        "--cache-dir", cache, "--request-timeout=5s", "get",
                        "customresourcedefinitions.apiextensions.k8s.io", TASK_NAME, "-o", "json"]
                for flags, expected in (([], False), (["--show-managed-fields=true"], True)):
                    result = subprocess.run(base + flags, capture_output=True, text=True, timeout=15, check=True)
                    value = json.loads(result.stdout)
                    self.assertEqual("managedFields" in value["metadata"], expected)
                    if expected:
                        self.assertEqual(value["metadata"]["managedFields"], obj["metadata"]["managedFields"])
            self.assertTrue(all(method == "GET" and auth is None for method, _path, auth in seen))
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)
            self.assertFalse(worker.is_alive())

    def test_managed_fields_reports_only_known_manager_classes_and_fixed_claim_flags(self):
        h = FakeHarness()
        obj = h.objects[("crd", TASK_NAME)]
        obj["metadata"]["managedFields"].append({
            "manager": "PRIVATE-MANAGER", "operation": "Update", "time": "PRIVATE-TIME",
            "fieldsV1": {"f:spec": {"f:versions": {"k:PRIVATE-KEY": {}}},
                         "f:metadata": {"f:annotations": {"f:meta.helm.sh/release-name": {}}}},
        })
        facts = task_manager_facts(obj)
        self.assertNotIn("PRIVATE", json.dumps(facts))
        self.assertEqual({item["versionsClaim"] for item in facts}, {"whole", "nested"})
        self.assertIn("other", {item["managerClass"] for item in facts})

    def test_conflict_parser_and_native_capture_are_closed_and_match_cli_vocabulary(self):
        stderr = 'error: Apply failed with 1 conflict: conflict with "PRIVATE-MANAGER": .spec.versions\n'
        facts = ssa_conflict(stderr)
        self.assertEqual(facts, {"conflictKind": "field-manager", "conflictCount": 1,
                                "conflictFields": ["spec/versions"], "conflictManagers": ["other"]})
        record = {"step": "schema-server-preview", "source": "cli/src/lib/schema-stage.ts",
                  "kind": "KarsTask", "category": "api-rejection", "reason": "Conflict", **facts}
        self.assertEqual(schema_preparation_failure("SRE-SCHEMA-PREPARATION " + json.dumps(record)), record)
        self.assertIsNone(schema_preparation_failure("SRE-SCHEMA-PREPARATION " + json.dumps({
            **record, "conflictManagers": ["PRIVATE-MANAGER"]})))
        self.assertIsNone(ssa_conflict('Error from server (Conflict): the object has been modified'))
        cas = ssa_conflict('error: Operation cannot be fulfilled on customresourcedefinitions.apiextensions.k8s.io '
                           '"PRIVATE-NAME": the object has been modified; please apply your changes to the latest version and try again')
        self.assertEqual(cas, {"conflictKind": "resource-version"})
        cas_record = {key: value for key, value in record.items() if not key.startswith("conflict")}
        cas_record.update(cas)
        self.assertEqual(schema_preparation_failure("SRE-SCHEMA-PREPARATION " + json.dumps(cas_record)), cas_record)
        self.assertIsNone(ssa_conflict('error: Apply failed with 2 conflicts: conflict with "helm": .spec.versions'))
        root = Path(__file__).resolve().parents[3]
        source = (root / "cli/src/lib/schema-ssa-conflicts.ts").read_text()
        manager_set = re.search(r"const managerClasses = new Set\(\[(.*?)\]\);", source, re.S).group(1)
        self.assertEqual(set(re.findall(r'"([^"]+)"', manager_set)), MANAGERS)
        paths = re.search(r"const conflictPaths:.*?= \{(.*?)\};", source, re.S).group(1)
        self.assertEqual(dict(re.findall(r'"([^"]+)": "([^"]+)"', paths)), PATHS)

    def test_existing_early_gate_runs_task_reproduction_before_the_new_registration_create(self):
        source = Path(__file__).with_name("legacy_crd_probe.py").read_text()
        self.assertLess(source.index("require_task_payload_helper(h)"), source.index("with kind_proxy(root)"))
        self.assertLess(source.index("dry_run_seed_data(h)"), source.index("exercise_task_restore_preview(h)"))
        self.assertLess(source.index("exercise_task_restore_preview(h)"), source.index("create_registration_crd(h, obj)"))


if __name__ == "__main__":
    unittest.main()
