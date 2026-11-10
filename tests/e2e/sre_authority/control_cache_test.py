# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Cache-observation ordering checks, not hosted Kubernetes acceptance."""

import base64
import copy
import json
import unittest
from unittest.mock import Mock, patch

from sre_authority.common import EPOCH
from sre_authority.fixtures import CONTROL, CONTROL_NS, seed_control_consumer
from sre_authority.migration import assert_control_cache, rotated_control_pods


class ControlCacheTests(unittest.TestCase):
    def setUp(self):
        self.pod = {"metadata": {"name": "control-pod", "uid": "control-uid",
                                "annotations": {EPOCH: "new-epoch"}},
                    "status": {"containerStatuses": [{"name": "credential-consumer",
                               "ready": True, "state": {"running": {}}}]}}
        self.digest = "a" * 64

    def harness(self, states):
        h = Mock()
        h.k.return_value = self.digest + "  /credential/control-token\n"
        h.get.side_effect = states

        def poll(_label, predicate, *, seconds):
            self.assertEqual(seconds, 120)
            for _ in states:
                h.k.assert_not_called()
                if predicate():
                    return True
            raise AssertionError("cache readiness deadline")

        h.poll.side_effect = poll
        return h

    def verify(self, h):
        assert_control_cache(h, self.pod, "new-epoch", self.digest)

    def test_waits_for_named_cache_container_before_exec(self):
        for status in ({}, {"containerStatuses": [{"name": "inference-router", "ready": True,
                                                    "state": {"running": {}}}]},
                       {"containerStatuses": [{"name": "credential-consumer", "ready": False,
                                                "state": {"running": {}}}]},
                       {"containerStatuses": [{"name": "credential-consumer", "ready": True,
                                                "state": {"waiting": {}}}]}):
            with self.subTest(status=status):
                pending = {**self.pod, "status": status}
                h = self.harness([pending, self.pod])
                self.verify(h)
                self.assertEqual(h.get.call_count, 2)
                h.get.assert_called_with("pod", "control-pod", CONTROL_NS)
                h.k.assert_called_once_with("exec", "-n", CONTROL_NS, "control-pod", "-c",
                    "credential-consumer", "--", "cat", "/proof/cached.sha", timeout=25)

    def test_missing_replaced_deleting_or_changed_epoch_pod_fails_before_exec(self):
        for metadata in (None, {"uid": "replacement"}, {"deletionTimestamp": "2026-11-08T20:28:11Z"},
                         {"annotations": {EPOCH: "old-epoch"}}):
            with self.subTest(metadata=metadata):
                current = copy.deepcopy(self.pod)
                if metadata is None:
                    current = None
                else:
                    current["metadata"].update(metadata)
                h = self.harness([current])
                with self.assertRaises(AssertionError):
                    self.verify(h)
                h.k.assert_not_called()

    def test_permanently_unready_cache_does_not_exec_or_pass(self):
        h = self.harness([{**self.pod, "status": {}}])
        with self.assertRaisesRegex(AssertionError, "deadline"):
            self.verify(h)
        h.k.assert_not_called()

    def test_ready_cache_still_requires_exact_rotated_digest(self):
        for output in ("", "b" * 64 + "  /credential/control-token\n"):
            with self.subTest(output=output):
                h = self.harness([self.pod])
                h.k.return_value = output
                with self.assertRaisesRegex(AssertionError, "did not cache the rotated token"):
                    self.verify(h)

    def test_exec_failure_is_not_retried_or_hidden(self):
        h = self.harness([self.pod])
        h.k.side_effect = AssertionError("command failed")
        with self.assertRaisesRegex(AssertionError, "command failed"):
            self.verify(h)
        self.assertEqual(h.k.call_count, 1)

    def rollout_harness(self, inventories, rollout_error=None):
        h = Mock()
        inventory = iter(inventories)
        events = []

        def kubectl(*args, **kwargs):
            if args[0] == "get":
                self.assertEqual(args, ("get", "pods", "-n", CONTROL_NS, "-o", "json"))
                events.append("inventory")
                return json.dumps({"items": next(inventory)})
            self.assertEqual(args, ("rollout", "status", f"deployment/{CONTROL}", "-n",
                                    CONTROL_NS, "--timeout=120s"))
            self.assertEqual(kwargs, {"timeout": 130})
            events.append("rollout")
            if rollout_error:
                raise rollout_error
            return "successfully rolled out"

        def poll(_label, predicate, *, seconds):
            self.assertEqual(seconds, 120)
            for _ in inventories[1:]:
                result = predicate()
                if result:
                    return result
            raise AssertionError("control rollout deadline")

        h.k.side_effect = kubectl
        h.poll.side_effect = poll
        return h, events

    def test_selects_post_rollout_pods_not_transient_post_rotation_pods(self):
        transient = copy.deepcopy(self.pod)
        transient["metadata"].update(name="transient", uid="transient-uid")
        terminating = copy.deepcopy(transient)
        terminating["metadata"]["deletionTimestamp"] = "2026-11-09T07:40:00Z"
        h, events = self.rollout_harness([[transient], [terminating, self.pod], [self.pod]])
        self.assertEqual(rotated_control_pods(h, {"old-uid"}, "new-epoch"), [self.pod])
        self.assertEqual(events, ["inventory", "rollout", "inventory", "inventory"])

    def test_old_cache_is_rejected_before_waiting_for_rollout(self):
        h, events = self.rollout_harness([[self.pod]])
        with self.assertRaisesRegex(AssertionError, "survived Ready"):
            rotated_control_pods(h, {"control-uid"}, "new-epoch")
        self.assertEqual(events, ["inventory"])
        h.poll.assert_not_called()

    def test_old_cache_or_wrong_epoch_after_rollout_is_not_skipped(self):
        old = copy.deepcopy(self.pod)
        old["metadata"]["uid"] = "old-uid"
        wrong_epoch = copy.deepcopy(self.pod)
        wrong_epoch["metadata"]["annotations"][EPOCH] = "old-epoch"
        for bad in (old, wrong_epoch):
            with self.subTest(pod=bad):
                h, _ = self.rollout_harness([[self.pod], [bad, self.pod]])
                with self.assertRaises(AssertionError):
                    rotated_control_pods(h, {"old-uid"}, "new-epoch")

    def test_empty_or_terminating_post_rollout_inventory_cannot_pass(self):
        terminating = copy.deepcopy(self.pod)
        terminating["metadata"]["deletionTimestamp"] = "2026-11-09T07:40:00Z"
        for inventory in ([], [terminating]):
            with self.subTest(inventory=inventory):
                h, _ = self.rollout_harness([[self.pod], inventory])
                with self.assertRaisesRegex(AssertionError, "deadline"):
                    rotated_control_pods(h, {"old-uid"}, "new-epoch")

    def test_rollout_failure_is_not_hidden(self):
        h, events = self.rollout_harness([[self.pod]], AssertionError("rollout failed"))
        with self.assertRaisesRegex(AssertionError, "rollout failed"):
            rotated_control_pods(h, {"old-uid"}, "new-epoch")
        self.assertEqual(events, ["inventory", "rollout"])
        h.poll.assert_not_called()

    def test_fixture_readiness_requires_successfully_written_cache(self):
        objects = []

        def create(obj, **_kwargs):
            obj = copy.deepcopy(obj)
            obj["metadata"]["uid"] = f"fixture-{len(objects)}"
            if obj["kind"] == "Secret":
                obj["data"] = {key: base64.b64encode(value.encode()).decode()
                               for key, value in obj["stringData"].items()}
            objects.append(obj)
            return obj

        h = Mock(state={})
        h.create.side_effect = create
        with patch("sre_authority.fixtures.namespace_claim", return_value="namespace-uid"):
            seed_control_consumer(h)
        deployment = next(obj for obj in objects if obj["kind"] == "Deployment")
        container = deployment["spec"]["template"]["spec"]["containers"][0]
        self.assertEqual(container["command"][:2], ["/bin/sh", "-ec"])
        self.assertIn("sha256sum /credential/control-token > /proof/cached.sha", container["command"][2])
        self.assertEqual(container["readinessProbe"]["exec"]["command"],
                         ["/bin/sh", "-c", "test -s /proof/cached.sha"])
        self.assertEqual(container["readinessProbe"]["periodSeconds"], 1)


if __name__ == "__main__":
    unittest.main()
