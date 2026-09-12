# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure checks of the native fixture; no cluster or controller execution."""

import copy
from types import SimpleNamespace
import unittest

from sre_authority.canonical_migration import (
    CRDS, STAGE, assert_data_unchanged, deny_late_conflicts, finish_data_proof, seed_data,
)


class FakeHarness:
    def __init__(self):
        self.objects = {
            ("deployment", "kars-controller"): {"spec": {"replicas": 0}},
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

    def get(self, kind, name, *_args):
        return copy.deepcopy(self.objects.get((kind, name)))

    def create(self, obj):
        result = copy.deepcopy(obj)
        result["metadata"].update(uid=f"fixture-{self.serial}", resourceVersion="1")
        self.serial += 1
        self.objects[(result["kind"].lower(), result["metadata"]["name"])] = result
        return copy.deepcopy(result)

    def api(self, method, path, *, body, status):
        self.calls.append((method, path, copy.deepcopy(body)))
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
    def test_seed_uses_nonexecuting_valid_action_and_real_data_preservation_assertions(self):
        h = FakeHarness()
        fixtures = seed_data(h)
        action = h.get("karssreaction", "e2e-migration-karssreaction")
        self.assertEqual(action["spec"]["approval"]["state"], "Rejected")
        self.assertEqual(action["spec"]["action"]["type"], "ScaleDeployment")
        self.assertFalse(h.get("karstask", "e2e-migration-karstask")["spec"]["execution"]["launch"])
        assert_data_unchanged(h, fixtures)
        h.objects[("mcpserver", "e2e-migration-mcpserver")]["spec"]["url"] = "changed"
        with self.assertRaisesRegex(AssertionError, "data or its UID"):
            assert_data_unchanged(h, fixtures)

    def test_negative_fixtures_use_the_public_cli_and_restore_only_exact_uid_rv_owned_schema(self):
        h = FakeHarness()
        fixtures = seed_data(h)
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
        finish_data_proof(h, fixtures)
        self.assertEqual(len(h.calls), 5)
        self.assertTrue(all(method == "DELETE" and "/customresourcedefinitions/" not in path
                            for method, path, _body in h.calls))
        self.assertIsNotNone(h.get("crd", "karstasks.kars.azure.com"))

    def test_controller_must_remain_paused_for_native_data_measurement(self):
        h = FakeHarness()
        h.objects[("deployment", "kars-controller")]["spec"]["replicas"] = 1
        with self.assertRaisesRegex(AssertionError, "paused"):
            seed_data(h)


if __name__ == "__main__":
    unittest.main()
