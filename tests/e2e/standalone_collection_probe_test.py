# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import unittest
from unittest.mock import Mock, patch

import standalone_collection_probe as probe


class StandaloneCollectionProbeTests(unittest.TestCase):
    def test_wrong_cluster_cannot_create_or_remove_any_fixture(self):
        with patch.object(probe, "request", return_value=(200, {"metadata": {"uid": "other"}})), \
             patch.object(probe, "Owned") as owned:
            with self.assertRaises(RuntimeError):
                probe.exercise(1, "expected")
        owned.assert_not_called()

    def test_existing_namespace_is_not_adopted_and_cases_do_not_run(self):
        owned = Mock()
        owned.create.side_effect = RuntimeError("No adoption")
        with patch.object(probe, "request", return_value=(200, {"metadata": {"uid": "expected"}})), \
             patch.object(probe, "Owned", return_value=owned), patch.object(probe, "cases") as cases:
            with self.assertRaises(RuntimeError):
                probe.exercise(1, "expected")
        cases.assert_not_called()
        owned.cleanup.assert_called_once()

    def test_only_actual_installed_policy_observation_precedes_exact_collection_cases(self):
        owned = Mock()
        policies = [{"metadata": {"name": name}} for name in probe.POLICIES]
        results = [{"case": "fixture", "httpStatus": 200, "expectedStatus": 200, "matched": True,
                    "message": "DO-NOT-EXPORT"}]
        with patch.object(probe, "request", return_value=(200, {"metadata": {"uid": "expected"}})), \
             patch.object(probe, "Owned", return_value=owned), \
             patch.object(probe, "wait_for", side_effect=[(200, policy) for policy in policies]), \
             patch.object(probe, "cases", return_value=results) as cases:
            output = probe.exercise(1, "expected")
        self.assertEqual(set(cases.call_args.args[1]), set(probe.POLICIES))
        self.assertEqual(owned.create.call_args.args[1]["metadata"]["name"], "kars-sre")
        self.assertNotIn("message", output[0])
        owned.cleanup.assert_called_once()
