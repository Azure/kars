# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure contracts for live-controller staging; no Kubernetes execution."""

from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from sre_authority.migration import running_controller_stage


def fixture():
    objects = {
        "deployment": {"spec": {"replicas": 1}, "status": {"availableReplicas": 1}},
        "karssandbox": {"metadata": {"uid": "source"}, "status": {
            "conditions": [{"type": "Ready", "status": "False"}]}},
        "clusterrolebinding": {"metadata": {"uid": "reader"}, "subjects": [{"name": "sandbox"}]},
    }
    h = SimpleNamespace(
        get=lambda kind, *_args: deepcopy(objects.get(kind)),
        cli=Mock(), passed=Mock(),
        poll=lambda _label, predicate, **_kwargs: predicate(),
    )
    return h, objects


class StagingCompatibilityTests(unittest.TestCase):
    def test_real_stage_runs_before_enrollment_without_changing_the_ready_gate(self):
        h, _objects = fixture()
        with patch("sre_authority.migration.policies_ready") as ready:
            running_controller_stage(h)
        h.cli.assert_called_once_with("authority", "stage", "--controller-image", "kars-controller:e2e",
                                      "--router-image", "kars-inference-router:e2e", timeout=180)
        ready.assert_called_once_with(h)
        h.passed.assert_called_once()
        source = (Path(__file__).parent / "migration.py").read_text()
        migration = source.split("def legacy_migration(h):", 1)[1].split("def rollback_guard", 1)[0]
        self.assertLess(migration.index("running_controller_stage(h)"), migration.index('"enroll"'))
        self.assertIn('h.cli("authority", "migrate"', migration)
        self.assertIn("h.wait_ready()", migration)

    def test_zero_or_unavailable_controller_cannot_mask_the_helm_watcher_deadlock(self):
        for key in ("replicas", "availableReplicas"):
            h, objects = fixture()
            objects["deployment"]["spec" if key == "replicas" else "status"][key] = 0
            with self.subTest(key=key), self.assertRaises(AssertionError):
                running_controller_stage(h)
            h.cli.assert_not_called()
            h.passed.assert_not_called()

    def test_existing_enrollment_is_not_a_valid_pre_enrollment_fixture(self):
        h, objects = fixture()
        objects["karssreregistrations.kars.azure.com"] = {"metadata": {"uid": "registration"}}
        with self.assertRaises(AssertionError):
            running_controller_stage(h)
        h.cli.assert_not_called()

    def test_stage_timeout_and_policy_typechecking_failure_remain_fatal(self):
        h, _objects = fixture()
        h.cli.side_effect = RuntimeError("stage timeout")
        with self.assertRaisesRegex(RuntimeError, "stage timeout"):
            running_controller_stage(h)
        h.passed.assert_not_called()
        h.cli.side_effect = None
        with patch("sre_authority.migration.policies_ready", side_effect=RuntimeError("policy typechecking")):
            with self.assertRaisesRegex(RuntimeError, "policy typechecking"):
                running_controller_stage(h)
        h.passed.assert_not_called()

    def test_native_fixture_checks_exact_historical_action_spec_and_uid_after_stage(self):
        source = (Path(__file__).parent / "fixtures.py").read_text()
        self.assertIn('action_params.pop("additionalProperties", None) is True', source)
        self.assertIn('action_params["x-kubernetes-preserve-unknown-fields"] = True', source)
        self.assertIn('action_after["metadata"]["uid"] == action_before["metadata"]["uid"]', source)
        self.assertIn('action_after["spec"] == action_spec', source)
        self.assertLess(source.index("action_before ="), source.index('h.cli("authority", "stage"'))
        self.assertGreater(source.index("action_after ="), source.index('h.cli("authority", "stage"'))


if __name__ == "__main__":
    unittest.main()
