# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import Failure
from observation_diagnostics import VERSION
from rollout_diagnostics import collect, container_state, event_records, startup_records
from test_observation_diagnostics import DEPLOYMENT, POD, REPLICA_SET, RUNTIME, SANDBOX, TARGET, Fixture


def event(**changes):
    value = {
        "involvedObject": {"kind": "Pod", "apiVersion": "v1", "namespace": RUNTIME,
                           "name": "agent-pod", "uid": "agent-pod-uid",
                           "fieldPath": "spec.containers{inference-router}"},
        "source": {"component": "kubelet"}, "reason": "Unhealthy",
        "message": 'Readiness probe failed: Get "http://private-canary": connection refused',
    }
    value.update(changes)
    return value


class RolloutFixture(Fixture):
    def __init__(self):
        super().__init__()
        self.events = [event()]
        self.objects[POD]["status"] = {"containerStatuses": [{
            "name": "inference-router", "ready": False, "restartCount": 3,
            "containerID": "containerd://private-canary", "state": {"running": {"startedAt": "private-canary"}},
            "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137, "signal": 9,
                                         "message": "private-canary"}},
        }]}

    def get(self, path):
        if "/events?" in path:
            return {"items": copy.deepcopy(self.events)}
        return super().get(path)


def log(message="kars Inference Router starting", **changes):
    return json.dumps({"target": "kars_inference_router", "level": "INFO",
                       "fields": {"message": message, "token": "private-canary"}, **changes})


class RolloutDiagnosticsTests(unittest.TestCase):
    def capture(self, fixture=None, side_effect=None):
        fixture = fixture or RolloutFixture()
        with patch("rollout_diagnostics.command", return_value=log(), side_effect=side_effect) as call:
            value = collect(SimpleNamespace(admin=fixture), {**TARGET, "uid": "sandbox-uid"})
        return value, call

    def test_unready_router_has_bounded_process_and_probe_evidence(self):
        value, call = self.capture()
        self.assertTrue(value["available"])
        self.assertTrue(value["diagnosticOnly"])
        pod = value["pods"][0]
        self.assertFalse(pod["routerReady"])
        self.assertEqual(pod["restartCount"], 3)
        self.assertEqual(pod["lastState"], {"phase": "terminated", "reason": "OOMKilled",
                                          "exitCode": 137, "signal": 9})
        self.assertEqual(pod["logs"]["stages"], ["starting"])
        self.assertFalse(pod["logs"]["listenerReachabilityProved"])
        self.assertEqual(pod["probeEvents"]["records"],
                         [{"probe": "readiness", "outcome": "failed",
                           "category": "connection-refused", "httpStatus": 0}])
        self.assertNotIn("private-canary", json.dumps(value))
        self.assertEqual(call.call_count, 2)
        self.assertIn("--limit-bytes=131072", call.call_args.args)
        self.assertIn("--previous", call.call_args.args)

    def test_stale_observer_version_is_reported_not_treated_as_current_authority(self):
        fixture = RolloutFixture()
        fixture.objects[POD]["metadata"]["annotations"][VERSION] = "old:1"
        value, _ = self.capture(fixture)
        self.assertTrue(value["available"])
        self.assertFalse(value["pods"][0]["observerVersionMatchesStatus"])
        self.assertFalse(value["pods"][0]["observerVersionMatchesTemplate"])
        self.assertTrue(value["diagnosticOnly"])

    def test_waiting_container_reads_only_its_previous_process_logs(self):
        fixture = RolloutFixture()
        fixture.objects[POD]["status"]["containerStatuses"][0]["state"] = {
            "waiting": {"reason": "CrashLoopBackOff", "message": "private-canary"}}
        value, call = self.capture(fixture)
        self.assertTrue(value["available"])
        self.assertEqual(value["pods"][0]["state"]["phase"], "waiting")
        self.assertFalse(value["pods"][0]["logs"]["available"])
        self.assertTrue(value["pods"][0]["previousLogs"]["available"])
        self.assertEqual(call.call_count, 1)
        self.assertIn("--previous", call.call_args.args)

    def test_fixed_startup_markers_do_not_claim_bound_listener(self):
        raw = "\n".join([log(), log("Registry topology"), log("Listening on 0.0.0.0:8443"),
                         log("unknown-private-canary", level="ERROR"), "Error: private-canary",
                         log([], level="ERROR"), "[]"])
        value = startup_records(raw)
        self.assertEqual(value["stages"], ["starting", "configuration-loaded", "listener-planned"])
        self.assertEqual(value["errorRecords"], 1)
        self.assertFalse(value["listenerReachabilityProved"])
        self.assertNotIn("canary", json.dumps(value))
        with self.assertRaises(Failure):
            startup_records("x" * 131073)

    def test_probe_events_remain_pod_lifetime_and_foreign_events_are_ignored(self):
        records = [event(), event(message="Liveness probe failed: HTTP probe failed with statuscode: 503"),
                   event(message="Startup probe failed: private-canary context deadline exceeded"),
                   event(message="Readiness probe failed: private-canary"),
                   event(message="Readiness probe errored: private-canary"),
                   event(involvedObject={"uid": "foreign"}),
                   event(source={"component": "private-canary"}),
                   event(involvedObject={**event()["involvedObject"],
                                         "fieldPath": "spec.containers{openclaw}"})]
        value = event_records({"items": records}, RUNTIME, "agent-pod", "agent-pod-uid")
        self.assertEqual(value["coverage"], "pod-lifetime-events-not-current-process")
        self.assertEqual([item["category"] for item in value["records"]],
                         ["connection-refused", "http-response", "timeout", "other", "other"])
        self.assertEqual(value["records"][1]["httpStatus"], 503)
        self.assertEqual(value["records"][-1]["outcome"], "errored")
        self.assertNotIn("canary", json.dumps(value))
        for metadata in ({"continue": "more"}, {"remainingItemCount": 1}):
            with self.assertRaises(Failure):
                event_records({"metadata": metadata, "items": records}, RUNTIME, "agent-pod", "agent-pod-uid")

    def test_changed_identity_revision_or_process_discards_projection(self):
        for path in (SANDBOX, DEPLOYMENT, REPLICA_SET, POD):
            for field in ("uid", "resourceVersion"):
                with self.subTest(path=path, field=field):
                    fixture = RolloutFixture()

                    def changed(*args, **kwargs):
                        fixture.objects[path]["metadata"][field] = "changed"
                        return log()

                    value, _ = self.capture(fixture, changed)
                    self.assertFalse(value["available"])
                    self.assertNotIn("pods", value)
        fixture = RolloutFixture()

        def restarted(*args, **kwargs):
            fixture.objects[POD]["status"]["containerStatuses"][0]["restartCount"] += 1
            return log()

        value, _ = self.capture(fixture, restarted)
        self.assertFalse(value["available"])
        self.assertNotIn("pods", value)

    def test_foreign_or_incomplete_authority_never_reads_logs(self):
        for path, change in (
            (SANDBOX, lambda item: item["metadata"].update(uid="foreign")),
            (DEPLOYMENT, lambda item: item["metadata"].update(uid="foreign")),
            (REPLICA_SET, lambda item: item["metadata"]["ownerReferences"][0].update(uid="foreign")),
            (POD, lambda item: item["metadata"].update(deletionTimestamp="terminating")),
        ):
            with self.subTest(path=path):
                fixture = RolloutFixture()
                change(fixture.objects[path])
                value, call = self.capture(fixture)
                self.assertFalse(value["available"])
                call.assert_not_called()

    def test_shared_replicaset_retains_its_first_snapshot_across_pods(self):
        for changed_field in (None, "uid", "resourceVersion"):
            with self.subTest(changed_field=changed_field):
                fixture = RolloutFixture()
                second = copy.deepcopy(fixture.objects[POD])
                second["metadata"].update(name="agent-pod-2", uid="agent-pod-2-uid")
                fixture.objects[POD + "-2"] = second
                original = fixture.get
                replica_reads = 0

                def changing(path):
                    nonlocal replica_reads
                    if path == REPLICA_SET:
                        replica_reads += 1
                        if replica_reads == 2 and changed_field:
                            fixture.objects[path]["metadata"][changed_field] = "changed-private-canary"
                    return original(path)

                fixture.get = changing
                value, _ = self.capture(fixture)
                self.assertEqual(value["available"], changed_field is None)
                if changed_field is None:
                    self.assertEqual(len(value["pods"]), 2)
                    self.assertEqual(replica_reads, 3)
                else:
                    self.assertNotIn("pods", value)
                    self.assertEqual(replica_reads, 2)
                self.assertNotIn("canary", json.dumps(value))

    def test_errors_are_explicit_and_value_free(self):
        value, _ = self.capture(side_effect=OSError("private-canary"))
        self.assertTrue(value["available"])
        self.assertFalse(value["pods"][0]["logs"]["available"])
        self.assertFalse(value["pods"][0]["previousLogs"]["available"])
        self.assertEqual(value["pods"][0]["logs"]["category"], "log-read-unavailable")
        self.assertNotIn("canary", json.dumps(value))

    def test_event_read_error_preserves_stable_process_evidence(self):
        fixture = RolloutFixture()
        original = fixture.get

        def failing(path):
            if "/events?" in path:
                raise OSError("private-canary")
            return original(path)

        fixture.get = failing
        value, _ = self.capture(fixture)
        self.assertTrue(value["available"])
        self.assertTrue(value["pods"][0]["logs"]["available"])
        self.assertEqual(value["pods"][0]["probeEvents"],
                         {"available": False, "category": "event-read-unavailable"})
        self.assertNotIn("canary", json.dumps(value))

    def test_state_types_and_values_are_bounded(self):
        for value in ({"terminated": {"exitCode": True}}, {"terminated": {"exitCode": 256}},
                      {"running": {}, "waiting": {}}, {"private-canary": {}}, []):
            with self.subTest(value=value):
                with self.assertRaises(Failure):
                    container_state(value)
        self.assertEqual(container_state({"waiting": {"reason": "private-canary"}}),
                         {"phase": "waiting", "reason": "Other"})


if __name__ == "__main__":
    unittest.main()
