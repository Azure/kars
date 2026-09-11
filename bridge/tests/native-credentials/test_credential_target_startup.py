"""Initial ownership must settle before reviewing a credential write."""

import copy
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from lifecycle_cases import reviewable_target
from native_api import Failure


class TargetStartupTests(unittest.TestCase):
    def setUp(self):
        self.created = {
            "kind": "KarsSandbox",
            "metadata": {"namespace": "workspace", "name": "agent", "uid": "target-uid",
                         "generation": 1, "resourceVersion": "1"},
            "spec": {"suspended": True},
        }
        self.target = copy.deepcopy(self.created)
        self.namespace = {
            "kind": "Namespace",
            "metadata": {"name": "kars-agent", "uid": "namespace-uid",
                         "annotations": {"kars.azure.com/sandbox-uid": "target-uid"}},
        }
        self.admin = SimpleNamespace(
            get=lambda _path: copy.deepcopy(self.target),
            optional=lambda _path: copy.deepcopy(self.namespace),
        )
        self.setup = SimpleNamespace(admin=self.admin)

    def initialize(self):
        self.target["metadata"].update({
            "resourceVersion": "3",
            "finalizers": ["kars.azure.com/namespace-cleanup"],
            "annotations": {"kars.azure.com/namespace-uid": "namespace-uid"},
        })

    def test_waits_for_actual_backlink_and_finalizer_before_initial_review(self):
        def wait(_label, check, seconds):
            self.assertEqual(seconds, 60)
            self.assertIsNone(check())
            self.target["metadata"]["annotations"] = {
                "kars.azure.com/namespace-uid": "namespace-uid"}
            self.assertIsNone(check())
            self.initialize()
            return check()
        with patch("lifecycle_cases.until", side_effect=wait):
            current = reviewable_target(self.setup, self.created)
        self.assertEqual(current, self.target)
        self.assertEqual(current["spec"], self.created["spec"])
        self.assertNotIn("annotations", self.created["metadata"])

    def test_absent_namespace_is_not_adopted_or_created_by_the_fixture(self):
        self.admin.optional = lambda _path: None
        with patch("lifecycle_cases.until", side_effect=lambda _label, check, _seconds: check()):
            self.assertIsNone(reviewable_target(self.setup, self.created))

    def test_identity_intent_generation_and_namespace_changes_fail_instead_of_rebasing(self):
        for mutation in ("uid", "generation", "spec", "owner", "binding", "terminating"):
            with self.subTest(mutation=mutation):
                self.setUp()
                self.initialize()
                if mutation in ("uid", "generation"):
                    self.target["metadata"][mutation] = "changed"
                elif mutation == "spec":
                    self.target["spec"]["suspended"] = False
                elif mutation == "owner":
                    self.namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] = "foreign"
                elif mutation == "binding":
                    self.target["metadata"]["annotations"]["kars.azure.com/namespace-uid"] = "foreign"
                else:
                    self.namespace["metadata"]["deletionTimestamp"] = "2026-09-11T00:00:00Z"
                with patch("lifecycle_cases.until", side_effect=lambda _label, check, _seconds: check()):
                    with self.assertRaises(Failure):
                        reviewable_target(self.setup, self.created)


if __name__ == "__main__":
    unittest.main()
