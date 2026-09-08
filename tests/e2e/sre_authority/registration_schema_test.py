# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure checks for the public-CRD diagnostic boundary, not real API evidence."""

import contextlib
import copy
import json
from pathlib import Path
import types
import unittest
from unittest.mock import patch

from sre_authority.common import command_error_category
from sre_authority import registration_schema as schema

PRIVATE = "DO-NOT-LOG-PRIVATE-AUTH-OR-SECRET-BODY"


def crd():
    return {
        "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
        "metadata": {"name": schema.CRD_NAME},
        "spec": {"group": "kars.azure.com", "scope": "Cluster",
                 "names": {"kind": "KarsSRERegistration", "plural": "karssreregistrations"}},
    }


def invalid():
    return {
        "kind": "Status", "status": "Failure", "reason": "Invalid", "code": 422,
        "details": {
            "name": schema.CRD_NAME, "group": "apiextensions.k8s.io", "kind": "CustomResourceDefinition",
            "causes": [{"field": "spec.versions[0].schema.openAPIV3Schema.properties[spec].x-kubernetes-validations[0].rule",
                        "message": "public schema compile diagnostic\n ^"}],
        },
    }


class RegistrationSchemaTests(unittest.TestCase):
    def test_native_kubectl_invalid_classification_does_not_echo_body(self):
        message = f'The CustomResourceDefinition "{schema.CRD_NAME}" is invalid: {PRIVATE}'
        self.assertEqual(command_error_category(message), "Invalid")
        self.assertNotIn(PRIVATE, command_error_category(message))
        self.assertEqual(command_error_category(f"Error from server (Forbidden): {PRIVATE}"), "Forbidden")

    def test_only_exact_public_schema_422_causes_are_exposed(self):
        report = schema.public_status(422, invalid())
        self.assertEqual(report["category"], "Invalid")
        self.assertEqual(len(report["causes"]), 1)
        self.assertIn("public schema compile diagnostic", report["causes"][0]["message"])
        for code, kind, name, group in [
            (403, "CustomResourceDefinition", schema.CRD_NAME, "apiextensions.k8s.io"),
            (500, "CustomResourceDefinition", schema.CRD_NAME, "apiextensions.k8s.io"),
            (422, "Secret", "private-secret", ""),
            (422, "CustomResourceDefinition", "other.example.test", "apiextensions.k8s.io"),
            (422, "CustomResourceDefinition", schema.CRD_NAME, "other.group"),
        ]:
            body = invalid()
            body["message"] = PRIVATE
            body["details"].update({"kind": kind, "name": name, "group": group})
            body["details"]["causes"][0]["message"] = PRIVATE
            with self.subTest(code=code, kind=kind):
                self.assertNotIn(PRIVATE, json.dumps(schema.public_status(code, body)))
        body = invalid()
        body["details"]["causes"] = [{"field": "data.token", "message": PRIVATE}]
        self.assertNotIn(PRIVATE, json.dumps(schema.public_status(422, body)))

    def test_malformed_responses_do_not_become_success_or_echo_raw_content(self):
        for body in (None, [], PRIVATE, {"message": PRIVATE},
                     {**invalid(), "details": None},
                     {**invalid(), "details": {**invalid()["details"], "causes": None}}):
            with self.subTest(body=type(body).__name__):
                report = schema.public_status(422, body)
                self.assertNotEqual(report["category"], "accepted")
                self.assertNotIn(PRIVATE, json.dumps(report))

    def test_general_secret_commands_cannot_use_the_public_crd_diagnostic(self):
        for obj in (
            {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": schema.CRD_NAME}},
            {**crd(), "data": {"token": PRIVATE}},
            {**crd(), "metadata": {"name": "other.example.test"}},
        ):
            with self.assertRaises(AssertionError):
                schema.require_public_crd(obj)

    def test_actual_create_path_surfaces_only_allowlisted_api_evidence(self):
        response = types.SimpleNamespace(status_code=422, json=invalid)
        harness = types.SimpleNamespace(root=Path("."), api=lambda *_args, **_kwargs: response)
        with patch.object(schema, "write_report") as write:
            with self.assertRaisesRegex(AssertionError, "HTTP 422"):
                schema.create_registration_crd(harness, crd())
        self.assertEqual(write.call_args.args[1], "registration-create.json")
        self.assertEqual(write.call_args.args[2]["causes"], schema.public_status(422, invalid())["causes"])

    def test_fast_preflight_uses_real_server_dry_run_and_rejects_invalid_schema(self):
        root = Path(__file__).resolve().parents[3]
        seen = []
        def request(_port, method, path, body=None):
            seen.append((method, path, body))
            return (404, {}) if method == "GET" else (422, invalid())
        def command(stage, _args, **_kwargs):
            return "public yaml" if stage == "render" else json.dumps(crd())
        with patch.object(schema, "kind_proxy", return_value=contextlib.nullcontext((1, {"gitVersion": "v1.31.0"}))), \
             patch.object(schema, "request", side_effect=request), \
             patch.object(schema, "command", side_effect=command), \
             patch.object(schema, "write_report") as write:
            with self.assertRaisesRegex(RuntimeError, "HTTP 422"):
                schema.preflight(root)
        self.assertEqual(seen[1][:2], ("POST", schema.CRD_PATH + "?dryRun=All"))
        self.assertEqual(write.call_args.args[2]["category"], "Invalid")

    def test_reused_kind_cluster_still_validates_update_instead_of_accepting_already_exists(self):
        root = Path(__file__).resolve().parents[3]
        existing = crd()
        existing["metadata"].update({"uid": "crd-uid", "resourceVersion": "42"})
        seen = []
        def request(_port, method, path, body=None):
            seen.append((method, path, copy.deepcopy(body)))
            return (200, existing)
        with patch.object(schema, "kind_proxy", return_value=contextlib.nullcontext((1, {"gitVersion": "v1.31.0"}))), \
             patch.object(schema, "request", side_effect=request), \
             patch.object(schema, "command", side_effect=lambda stage, *_args, **_kwargs:
                          "public yaml" if stage == "render" else json.dumps(crd())), \
             patch.object(schema, "write_report"):
            schema.preflight(root)
        self.assertEqual(seen[1][:2], ("PUT", schema.CRD_PATH + "/" + schema.CRD_NAME + "?dryRun=All"))
        self.assertEqual(seen[1][2]["metadata"]["uid"], "crd-uid")
        self.assertEqual(seen[1][2]["metadata"]["resourceVersion"], "42")

    def test_fast_ci_gate_uses_the_harness_pins_without_waiting_for_rust_images(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  sre-crd-schema:\n", 1)[1].split("  helm-lint:\n", 1)[0]
        for required in ("version: v0.24.0", "version: v1.30.5", "tests/e2e/kind-config.yaml",
                         "registration_schema.py", "e2e-sre-schema-diag/validation.json"):
            self.assertIn(required, job)
        for forbidden in ("needs:", "cargo ", "docker/build-push", "--validate=false", "continue-on-error"):
            self.assertNotIn(forbidden, job)
        runner = (root / "tests/e2e/run.sh").read_text()
        self.assertLess(runner.index('python3 "$SCRIPT_DIR/sre_authority/registration_schema.py"'),
                        runner.index("\n    build_images\n"))


if __name__ == "__main__":
    unittest.main()
