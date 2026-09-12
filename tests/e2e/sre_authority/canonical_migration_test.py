# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure checks of the native fixture; no cluster or controller execution."""

import copy
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from sre_authority.canonical_migration import (
    CRDS, STAGE, assert_data_unchanged, deny_late_conflicts, finish_data_proof, seed_data,
)
from sre_authority.canonical_seed import SEEDS, WORKLOADS, collection_path, nested_action_definition, seed_definitions


class FakeHarness:
    """Transport orchestration only; this is not Kubernetes schema validation."""

    def __init__(self):
        self.root = Path("unused-fixture-report-root")
        self.objects = {
            ("deployment", "kars-controller"): {
                "apiVersion": "apps/v1", "kind": "Deployment",
                "metadata": {"name": "kars-controller", "namespace": "kars-system",
                             "uid": "controller", "resourceVersion": "1"},
                "spec": {"replicas": 0}},
            ("clusterrolebinding", "kars-sre-reader"): {
                "metadata": {"uid": "binding"}, "subjects": [{"name": "legacy"}, {"name": "unrelated"}]},
        }
        for name in ("karstasks.kars.azure.com", "karssreactions.kars.azure.com"):
            self.objects[("crd", name)] = {"metadata": {"name": name, "uid": name, "resourceVersion": "1",
                "annotations": {"meta.helm.sh/release-name": "kars"}},
                "spec": {"versions": [{"schema": {"openAPIV3Schema": {"description": "canonical"}}}]}}
        self.calls = []
        self.rejections = []
        self.serial = 1
        action = self.objects[("crd", "karssreactions.kars.azure.com")]
        action["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"] = {
            "spec": {"properties": {"action": {"properties": {
                "params": {"type": "object", "additionalProperties": True,
                           "description": "Public action params documentation"}}}}}}

    def migrate_action_schema(self):
        action = self.objects[("crd", "karssreactions.kars.azure.com")]
        params = action["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["action"]["properties"]["params"]
        assert params.pop("additionalProperties") is True
        params["x-kubernetes-preserve-unknown-fields"] = True
        action["metadata"]["resourceVersion"] = str(int(action["metadata"]["resourceVersion"]) + 1)

    def get(self, kind, name, *_args):
        return copy.deepcopy(self.objects.get((kind, name)))

    def create(self, obj):
        result = copy.deepcopy(obj)
        result["metadata"].update(uid=f"fixture-{self.serial}", resourceVersion="1")
        self.serial += 1
        self.objects[(result["kind"].lower(), result["metadata"]["name"])] = result
        return copy.deepcopy(result)

    def api(self, method, path, *, body=None, status=None):
        self.calls.append((method, path, copy.deepcopy(body)))
        parsed = urlsplit(path)
        if method == "GET":
            assert status == 200 and parse_qs(parsed.query) == {"limit": ["513"]}
            if parsed.path in WORKLOADS:
                items = [self.get("deployment", "kars-controller")] if parsed.path.endswith("/deployments") else []
            else:
                resource = next(resource for resource, _plural, _kind in SEEDS
                                if collection_path(resource) == parsed.path)
                items = [copy.deepcopy(obj) for (kind, _name), obj in self.objects.items() if kind == resource]
            result = {"kind": "List", "metadata": {}, "items": items}
            return SimpleNamespace(status_code=200, json=lambda: result)
        if method == "POST":
            resource = next(resource for resource, _plural, _kind in SEEDS
                            if collection_path(resource) == parsed.path)
            query = parse_qs(parsed.query)
            assert query in ({"fieldManager": ["kubectl-create"], "fieldValidation": ["Strict"]},
                             {"fieldManager": ["kubectl-create"], "fieldValidation": ["Strict"], "dryRun": ["All"]})
            nested = resource == "karssreaction" and body in (
                nested_action_definition(after_migration=False), nested_action_definition(after_migration=True))
            if nested:
                assert query["dryRun"] == ["All"]
                crd = self.objects[("crd", "karssreactions.kars.azure.com")]
                params = crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["action"]["properties"]["params"]
                params = {key: value for key, value in params.items() if key != "description"}
                if params == {"type": "object", "additionalProperties": True}:
                    # The precise native BASE365 rejection, not a generic validator.
                    result = {"kind": "Status", "reason": "BadRequest", "message":
                              'strict decoding error: unknown field "spec.action.params.opaque.nested"'}
                    return SimpleNamespace(status_code=400, json=lambda: result)
                assert params == {"type": "object", "x-kubernetes-preserve-unknown-fields": True}
            else:
                assert body == dict(seed_definitions())[resource]
            if "dryRun" in query:
                result = copy.deepcopy(body)
                result["metadata"]["uid"] = "ephemeral-dry-run"
            else:
                result = self.create(body)
            return SimpleNamespace(status_code=201, json=lambda: result)
        if method == "PATCH":
            current = self.objects[("crd", path.rsplit("/", 1)[1])]
            assert body["metadata"]["uid"] == current["metadata"]["uid"]
            assert body["metadata"]["resourceVersion"] == current["metadata"]["resourceVersion"]
            if "spec" in body:
                current["spec"] = copy.deepcopy(body["spec"])
            current["metadata"]["annotations"].update(body["metadata"].get("annotations", {}))
            current["metadata"]["resourceVersion"] = str(int(current["metadata"]["resourceVersion"]) + 1)
        elif method == "DELETE":
            name = path.rsplit("/", 1)[1]
            key = next(key for key in self.objects if key[1] == name)
            current = self.objects[key]
            assert body["preconditions"] == {
                "uid": current["metadata"]["uid"], "resourceVersion": current["metadata"]["resourceVersion"]}
            del self.objects[key]
        else:
            raise AssertionError("Unexpected fixture mutation")

    def cli(self, *args, **kwargs):
        self.rejections.append((args, kwargs))
        assert args in (STAGE, (*STAGE, "--dry-run"))
        return SimpleNamespace(returncode=1)

    def passed(self, _message):
        pass


class CanonicalMigrationFixtureTests(unittest.TestCase):
    def setUp(self):
        reporter = patch("sre_authority.canonical_seed.write_report")
        self.reporter = reporter.start()
        self.addCleanup(reporter.stop)

    def test_seed_uses_typed_inert_action_and_real_data_preservation_assertions(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        h.calls.clear()
        action = h.get("karssreaction", "e2e-migration-karssreaction")
        self.assertEqual(action["spec"]["approval"]["state"], "Rejected")
        self.assertEqual(action["spec"]["action"]["type"], "ScaleDeployment")
        self.assertEqual(action["spec"]["action"]["params"]["opaque"], "retained")
        self.assertFalse(h.get("karstask", "e2e-migration-karstask")["spec"]["execution"]["launch"])
        assert_data_unchanged(h, fixtures)
        h.objects[("mcpserver", "e2e-migration-mcpserver")]["spec"]["url"] = "changed"
        with self.assertRaisesRegex(AssertionError, "data or its UID"):
            assert_data_unchanged(h, fixtures)

    def test_negative_fixtures_use_the_public_cli_and_restore_only_exact_uid_rv_owned_schema(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        h.calls.clear()
        before = copy.deepcopy(h.objects[("crd", "karstasks.kars.azure.com")]["spec"])
        action = copy.deepcopy(h.objects[("crd", "karssreactions.kars.azure.com")])
        subjects = copy.deepcopy(h.objects[("clusterrolebinding", "kars-sre-reader")]["subjects"])
        deny_late_conflicts(h, fixtures)
        self.assertEqual([args for args, _kwargs in h.rejections],
                         [(*STAGE, "--dry-run"), STAGE] * 2)
        self.assertEqual(h.objects[("crd", "karstasks.kars.azure.com")]["spec"], before)
        self.assertEqual(h.objects[("crd", "karssreactions.kars.azure.com")], action)
        self.assertEqual(h.objects[("clusterrolebinding", "kars-sre-reader")]["subjects"], subjects)
        self.assertTrue(all(method == "PATCH" and path == f"{CRDS}/karstasks.kars.azure.com"
                            for method, path, _body in h.calls))

    def test_cleanup_is_limited_to_measured_disposable_crs_with_exact_uid_rv(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        h.migrate_action_schema()
        h.calls.clear()
        finish_data_proof(h, fixtures)
        deletes = [(method, path, body) for method, path, body in h.calls if method == "DELETE"]
        self.assertEqual(len(deletes), 5)
        self.assertTrue(all(method == "DELETE" and "/customresourcedefinitions/" not in path
                            for method, path, _body in deletes))
        self.assertTrue(all(method in ("GET", "DELETE") or method == "POST" and "dryRun=All" in path
                            for method, path, _body in h.calls))
        nested = [(index, body) for index, (method, _path, body) in enumerate(h.calls) if method == "POST"]
        self.assertEqual([body for _index, body in nested], [nested_action_definition(after_migration=True)])
        self.assertLess(nested[0][0], next(index for index, call in enumerate(h.calls) if call[0] == "DELETE"))
        self.assertIsNotNone(h.get("crd", "karstasks.kars.azure.com"))

    def test_post_migration_proof_refuses_an_unmigrated_schema_without_deleting_preserved_data(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        h.calls.clear()
        before = copy.deepcopy(h.objects)
        with self.assertRaisesRegex(AssertionError, "wrong side"):
            finish_data_proof(h, fixtures)
        self.assertEqual(h.objects, before)
        self.assertTrue(all(method == "GET" for method, _path, _body in h.calls))

    def test_controller_must_remain_paused_for_native_data_measurement(self):
        h = FakeHarness()
        h.objects[("deployment", "kars-controller")]["spec"]["replicas"] = 1
        with self.assertRaisesRegex(AssertionError, "paused"):
            seed_data(h)


if __name__ == "__main__":
    unittest.main()
