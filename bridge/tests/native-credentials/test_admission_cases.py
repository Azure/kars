# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
import types
import unittest
from unittest.mock import Mock, patch

from admission_cases import Cases
from native_api import Failure


class StoreReadinessTests(unittest.TestCase):
    def setUp(self):
        self.cases = Cases.__new__(Cases)
        self.cases.results = {}
        self.request = Mock()
        self.cases.api = types.SimpleNamespace(request=self.request)
        self.path = "/api/v1/namespaces/native-admission-stores/secrets/kars-provider-native"
        self.body = {"metadata": {"uid": "secret-uid", "resourceVersion": "17"},
                     "stringData": {"API_KEY": "private-canary"}}
        self.denial = (422, {"message": "kars-credential-enrolled-store-shape: exact UID and purpose"})

    def await_policy(self):
        with patch("native_api.time.sleep"):
            self.cases.await_store_policy(self.path, self.body, "grant-uid-rebound")

    def test_waits_for_parameter_observation_without_any_persisted_write(self):
        self.request.side_effect = [(200, {"data": {"private": "private-canary"}}), self.denial]
        self.await_policy()
        self.assertEqual(self.request.call_count, 2)
        for call in self.request.call_args_list:
            self.assertEqual(call.args, ("PATCH", self.path + "?dryRun=All", self.body))
            self.assertEqual(call.kwargs, {"expected": (200, 422),
                                          "patch_type": "application/merge-patch+json"})
        self.assertEqual(self.cases.results["enrolledStoreReadiness"], [{
            "phase": "grant-uid-rebound", "allowedDryRuns": 1, "deniedDryRuns": 1, "result": "passed"}])
        self.assertNotIn("private-canary", json.dumps(self.cases.results))

    def test_already_observed_parameters_do_not_sleep_or_retry(self):
        self.request.return_value = self.denial
        with patch("native_api.time.sleep") as sleep:
            self.cases.await_store_policy(self.path, self.body, "grant-created")
        sleep.assert_not_called()
        self.assertEqual(self.request.call_count, 1)

    def test_permanently_missing_enforcement_fails_and_retains_observation(self):
        self.request.return_value = (200, {})
        with patch("native_api.time.monotonic", side_effect=[0, 0, 31]):
            with self.assertRaisesRegex(Failure, "Deadline: current enrolled store"):
                self.await_policy()
        self.assertEqual(self.cases.results["enrolledStoreReadiness"][0], {
            "phase": "grant-uid-rebound", "allowedDryRuns": 1, "deniedDryRuns": 0, "result": "failed"})

    def test_unrelated_denial_is_not_readiness(self):
        for message in ["another-policy: exact UID and purpose", "kars-credential-enrolled-store-shape: unrelated"]:
            with self.subTest(message=message):
                self.request.reset_mock()
                self.request.return_value = (422, {"message": message})
                with self.assertRaisesRegex(Failure, "intended admission predicate"):
                    self.await_policy()
                self.assertEqual(self.request.call_count, 1)
                self.assertEqual(self.cases.results["enrolledStoreReadiness"][-1]["result"], "failed")

    def test_transport_or_unexpected_status_failures_are_not_retried(self):
        self.request.side_effect = Failure("Kubernetes request failed")
        with self.assertRaisesRegex(Failure, "Kubernetes request failed"):
            self.await_policy()
        self.assertEqual(self.request.call_count, 1)
        self.assertEqual(self.cases.results["enrolledStoreReadiness"][0]["result"], "failed")

    def test_real_forbidden_write_is_still_single_shot(self):
        self.request.side_effect = Failure("Kubernetes PATCH returned 200")
        with self.assertRaisesRegex(Failure, "returned 200"):
            self.cases.denied("PATCH", self.path, self.body,
                              "kars-credential-enrolled-store-shape", "exact UID and purpose", 422)
        self.request.assert_called_once_with("PATCH", self.path, self.body, expected=(422,),
                                             patch_type="application/merge-patch+json")


if __name__ == "__main__":
    unittest.main()
