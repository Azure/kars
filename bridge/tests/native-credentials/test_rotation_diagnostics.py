# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import contextlib
import copy
from datetime import datetime, timedelta, timezone
import io
import json
import os
from pathlib import Path
import ssl
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch
from urllib.parse import urlsplit

import native_api
import rotation_diagnostics as rotation
import test_observer_network_diagnostics as fixtures
from native_api import CORE, Failure, core, resource

CONTROLLER = core(CORE, "pods", "kars-controller-pod")
GRANT = resource(CORE, "karscredentialgrants", "workspace")
SECRET = core(fixtures.RUNTIME, "secrets", "router-services-observer")
CNP = resource(fixtures.RUNTIME, "ciliumnetworkpolicies", "owned-api", "/apis/cilium.io/v2")
FAILED = {"result": "failed", "failure": rotation.FAILURE}


class Fixture(fixtures.NetworkFixture):
    def __init__(self):
        super().__init__()
        self.calls, self.full_metadata_response, self.change_metadata = [], False, False
        self.partial_inventory = False
        self.meta_reads = {}
        self.objects[CONTROLLER]["status"] = {"containerStatuses": [{
            "name": "controller", "ready": True, "restartCount": 2,
            "containerID": "private-container-canary",
            "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137, "signal": 9,
                                        "message": "private-termination-canary"}}}]}
        self.objects[GRANT] = {
            "metadata": {"name": "workspace", "namespace": CORE, "uid": "grant-uid",
                         "resourceVersion": "2", "generation": 4},
            "spec": {"private": "private-grant-canary"}, "status": {"observedGeneration": 3}}
        self.objects[f"/api/v1/namespaces/{fixtures.RUNTIME}"]["metadata"]["annotations"].update({
            "kars.azure.com/sandbox-namespace": CORE, "kars.azure.com/sandbox-name": "agent"})
        owners = [{"apiVersion": "v1", "kind": "Namespace", "name": fixtures.RUNTIME,
                   "uid": fixtures.RUNTIME + "-uid", "controller": True, "blockOwnerDeletion": False}]
        self.objects[SECRET] = {"kind": "Secret", "metadata": {
            "name": "router-services-observer", "namespace": fixtures.RUNTIME, "uid": "observer-uid",
            "resourceVersion": "7", "ownerReferences": copy.deepcopy(owners),
            "annotations": {"private": "private-secret-annotation-canary"}},
            "data": {"observation-token": "private-token-canary"}}
        self.objects[fixtures.SANDBOX]["status"]["serviceObservation"]["version"] = "observer-uid:6"
        self.objects[CNP] = {"kind": "CiliumNetworkPolicy", "metadata": {
            "name": "owned-api", "namespace": fixtures.RUNTIME, "uid": "policy-uid",
            "resourceVersion": "9", "ownerReferences": copy.deepcopy(owners),
            "labels": {rotation.LABEL: "grant-uid"},
            "annotations": {"kars.azure.com/credential-grant-owner": "grant-uid",
                            "kars.azure.com/observer-grant-generation": "3",
                            "kars.azure.com/observer-namespace-uid": fixtures.RUNTIME + "-uid",
                            "kars.azure.com/sandbox-uid": "sandbox-uid",
                            "private": "private-policy-annotation-canary"}},
            "spec": {"private": "private-policy-body-canary"}}

    def request(self, method, path, expected=(200,), *, accept="application/json", timeout=15):
        assert method == "GET"
        self.calls.append((path, accept, timeout))
        path = urlsplit(path).path
        if path == SECRET or path.endswith("/ciliumnetworkpolicies"):
            listed = path != SECRET
            kind = "PartialObjectMetadataList" if listed else "PartialObjectMetadata"
            assert accept == f"application/json;as={kind};g=meta.k8s.io;v=v1"
            if path == SECRET and path not in self.objects:
                return 404, {"message": "private-404-canary"}
            if self.full_metadata_response:
                return 200, copy.deepcopy(self.objects[SECRET])
            self.meta_reads[path] = self.meta_reads.get(path, 0) + 1
            def partial(value):
                result = {"kind": "PartialObjectMetadata", "apiVersion": "meta.k8s.io/v1",
                          "metadata": copy.deepcopy(value["metadata"])}
                if self.change_metadata and self.meta_reads[path] > 1:
                    result["metadata"]["resourceVersion"] = "changed"
                return result
            if listed:
                return 200, {"kind": kind, "apiVersion": "meta.k8s.io/v1",
                             "metadata": {"continue": "private-continuation-canary"} if self.partial_inventory else {},
                             "items": [partial(self.objects[CNP])]}
            return 200, partial(self.objects[path])
        return 200, self.get(path)


class RotationDiagnosticsTests(unittest.TestCase):
    def setUp(self):
        self.api = Fixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})
        self.since = datetime.now(timezone.utc) - timedelta(seconds=180)
        self.event = {"timestamp": (self.since + timedelta(seconds=100)).isoformat(),
                      "target": "kars_controller::credential_grants", "fields": {
                          "message": "Credential grant is not ready", "namespace": f'Some("{CORE}")',
                          "error": "Retire owned observer API policy: Kubernetes status 409",
                          "private": "private-log-canary"}}

    def collect(self, failed=FAILED, output=None, log_effect=None):
        output = json.dumps(self.event).encode() if output is None else output
        with patch.object(rotation, "selected_origin"), patch.object(rotation.subprocess, "run",
                side_effect=log_effect, return_value=SimpleNamespace(returncode=0, stdout=output)):
            return rotation.collect(self.setup, fixtures.TARGET, failed, self.since)

    def test_scoped_snapshot_retains_lag_restart_and_metadata_without_bodies(self):
        failed = copy.deepcopy(FAILED)
        result = self.collect(failed)
        self.assertEqual(failed, FAILED)
        self.assertTrue(result["controller"]["available"])
        pod = result["controller"]["pods"][0]
        self.assertEqual(pod["identity"]["uid"], "kars-controller-pod-uid")
        self.assertEqual(pod["restartCount"], 2)
        self.assertEqual(pod["lastTermination"], {
            "status": "recorded", "reason": "OOMKilled", "exitCode": 137, "signal": 9})
        self.assertEqual(result["grant"]["currentGeneration"], 4)
        self.assertEqual(result["grant"]["observedGeneration"], 3)
        self.assertFalse(result["grant"]["generationCurrent"])
        self.assertEqual(result["observerMetadata"]["policies"][0]["grantGeneration"], 3)
        self.assertFalse(result["observerMetadata"]["secret"]["matchesPublishedVersion"])
        self.assertEqual(result["controller"]["warnings"]["records"][0]["category"], "cilium-retirement-delete")
        self.assertEqual(result["reconcileStage"], "unavailable")
        self.assertNotIn("canary", json.dumps(result))
        self.assertTrue(all(0 < timeout <= 10 for _, _, timeout in self.api.calls))

    def test_metadata_transport_does_not_fall_back_to_secret_or_policy_bodies(self):
        self.api.full_metadata_response = True
        result = self.collect()
        self.assertFalse(result["observerMetadata"]["available"])
        self.assertTrue(result["grant"]["available"])
        self.assertNotIn("canary", json.dumps(result))
        response = MagicMock(status=200)
        response.read.return_value = b'{"kind":"PartialObjectMetadata","apiVersion":"meta.k8s.io/v1","metadata":{}}'
        connection = MagicMock()
        connection.getresponse.return_value = response
        with patch.object(native_api.http.client, "HTTPSConnection", return_value=connection) as transport:
            api = native_api.Api("https://127.0.0.1:36443", ssl.create_default_context())
            api.request("GET", SECRET, accept="application/json;as=PartialObjectMetadata;g=meta.k8s.io;v=v1", timeout=4)
        self.assertEqual(transport.call_args.kwargs["timeout"], 4)
        self.assertEqual(connection.request.call_args.kwargs["headers"]["Accept"],
                         "application/json;as=PartialObjectMetadata;g=meta.k8s.io;v=v1")

    def test_malformed_restart_termination_and_grant_fields_are_unavailable(self):
        for field in ("restartCount", "ready"):
            self.setUp()
            self.api.objects[CONTROLLER]["status"]["containerStatuses"][0][field] = "private-canary"
            result = self.collect()
            self.assertFalse(result["controller"]["available"])
            self.assertNotIn("canary", json.dumps(result))
        self.setUp()
        self.api.objects[CONTROLLER]["status"]["containerStatuses"][0]["lastState"]["terminated"]["exitCode"] = True
        self.assertFalse(self.collect()["controller"]["available"])
        self.setUp()
        self.api.objects[GRANT]["status"]["observedGeneration"] = True
        self.assertFalse(self.collect()["grant"]["available"])

    def test_unrecorded_absent_and_unavailable_remain_distinct(self):
        self.api.objects[CONTROLLER]["status"]["containerStatuses"][0]["lastState"] = {}
        del self.api.objects[SECRET]
        result = self.collect(output=b"")
        self.assertEqual(result["controller"]["pods"][0]["lastTermination"]["status"], "unrecorded")
        self.assertEqual(result["controller"]["warnings"]["status"], "unobserved")
        self.assertEqual(result["observerMetadata"]["secret"]["status"], "absent")
        result = self.collect(log_effect=OSError("private-canary"))
        self.assertEqual(result["controller"]["warnings"]["status"], "unavailable")
        self.assertNotIn("canary", json.dumps(result))

    def test_metadata_drift_or_foreign_namespace_owner_invalidates_only_that_section(self):
        self.api.change_metadata = True
        result = self.collect()
        self.assertFalse(result["observerMetadata"]["available"])
        self.assertTrue(result["controller"]["available"])
        self.setUp()
        self.api.objects[CNP]["metadata"]["ownerReferences"][0]["uid"] = "foreign"
        self.assertFalse(self.collect()["observerMetadata"]["available"])
        self.setUp()
        self.api.partial_inventory = True
        result = self.collect()
        self.assertFalse(result["observerMetadata"]["available"])
        self.assertNotIn("canary", json.dumps(result))

    def test_warning_redaction_staleness_and_size_bounds(self):
        self.event["fields"]["error"] = "private-error-canary"
        self.assertEqual(self.collect()["controller"]["warnings"]["records"][0]["category"], "unclassified")
        self.event["timestamp"] = (self.since - timedelta(seconds=1)).isoformat()
        self.assertEqual(self.collect()["controller"]["warnings"]["status"], "unobserved")
        self.assertEqual(self.collect(output=b"x" * 65537)["controller"]["warnings"]["status"], "unavailable")
        self.assertEqual(self.collect(output=b'{"fields":null}\nprivate-canary')["controller"]["warnings"]["status"], "unobserved")
        reader = rotation._Reader(self.api)
        reader.deadline = 0
        with self.assertRaises(Failure):
            reader.get(GRANT)
        reader = rotation._Reader(self.api)
        reader.calls = 48
        with self.assertRaises(Failure):
            reader.get(GRANT)

    def test_ineligible_cases_do_not_read_any_resources(self):
        for failed in (None, {"result": "passed"}, {"result": "blocked"},
                       {"result": "failed", "failure": "other"}):
            self.assertEqual(self.collect(failed)["category"], "not-eligible")
        self.assertEqual(self.api.calls, [])

    def test_failure_snapshot_is_saved_before_subsequent_controller_restart(self):
        import run as native_run
        observations, lifecycle = MagicMock(), MagicMock()
        observations.rotation.side_effect = Failure(rotation.FAILURE)
        observations.observer_target = fixtures.TARGET
        order = []
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent) as directory:
            state = Path(directory)
            def snapshot(_setup, _target, failed, _since):
                saved = json.loads((state / "evidence/native.json").read_text())
                self.assertEqual(saved["cases"]["observer-rotation-current-bearer-and-revocation"], FAILED)
                self.assertEqual(failed, FAILED)
                order.append("snapshot")
                return self.collect(failed)
            def restart():
                self.api.objects[CONTROLLER]["metadata"]["uid"] = "replacement-controller"
                saved = json.loads((state / "evidence/native.json").read_text())
                snapshot = saved["cases"]["observer-rotation-current-bearer-and-revocation"]["rotationFailureSnapshot"]
                self.assertEqual(snapshot["controller"]["pods"][0]["identity"]["uid"], "kars-controller-pod-uid")
                order.append("restart")
            lifecycle.writer_uninstall.side_effect = restart
            with patch.dict(os.environ, fixtures.ENV), patch.object(native_run, "STATE", state), \
                    patch.object(native_run, "command", return_value=native_run.CORE_REVISION), \
                    patch.object(native_run, "Setup"), patch.object(native_run, "install_core"), \
                    patch.object(native_run, "install_bridge"), patch.object(native_run, "bridge_connection"), \
                    patch.object(native_run, "CredentialCases"), \
                    patch.object(native_run, "LifecycleCases", return_value=lifecycle), \
                    patch.object(native_run, "ObservationCases", return_value=observations), \
                    patch.object(native_run, "diagnostics", return_value={}), \
                    patch.object(native_run, "rotation_diagnostics", side_effect=snapshot), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(native_run.main(), 1)
            result = json.loads((state / "evidence/native.json").read_text())
            self.assertEqual(order, ["snapshot", "restart"])
            self.assertEqual(result["cases"]["observer-rotation-current-bearer-and-revocation"]["failure"], rotation.FAILURE)
            self.assertFalse(result["runtimeQualified"])
