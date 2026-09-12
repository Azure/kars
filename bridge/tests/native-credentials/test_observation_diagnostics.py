import copy
from datetime import datetime, timedelta, timezone
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import BRIDGE, CORE, WRITER, core, resource
import api_outcome_diagnostics as api_outcomes
from observation_diagnostics import CLIENT_FIELDS, CLIENT_TRANSPORT_FIELDS, TARGETS, VERSION, collect, project


def event(component="router", **updates):
    value = {
        "target": TARGETS[component],
        "fields": {"message": "Private observation readiness pending",
                   "stage": "observer_target_read", "http_status": 403,
                   "timeout": False, "connect": False},
        "span": {"private": "private-span-canary"},
    }
    value["fields"].update(updates)
    return json.dumps(value)


TARGET = {"workspace": CORE, "sandbox": "agent"}
SANDBOX = resource(CORE, "karssandboxes", "agent")
RUNTIME = "kars-agent"
POD = core(RUNTIME, "pods", "agent-pod")
DEPLOYMENT = resource(RUNTIME, "deployments", "agent", "/apis/apps/v1")
REPLICA_SET = resource(RUNTIME, "replicasets", "agent-rs", "/apis/apps/v1")


class Fixture:
    def __init__(self):
        self.objects = {}
        for namespace, name, account, labels in [
            (CORE, "kars-controller", "kars-controller",
             {"app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller"}),
            (RUNTIME, "agent", "sandbox", {"kars.azure.com/sandbox": "agent"}),
            (BRIDGE, "kars-bridge-bff", WRITER,
             {"app.kubernetes.io/name": "kars-bridge", "app.kubernetes.io/component": "bff"}),
        ]:
            self.objects[f"/api/v1/namespaces/{namespace}"] = {
                "metadata": {"name": namespace, "uid": namespace + "-uid", "resourceVersion": "1"}}
            self.objects[core(namespace, "serviceaccounts", account)] = {
                "metadata": {"name": account, "namespace": namespace,
                             "uid": account + "-sa-uid", "resourceVersion": "1"}}
            metadata = {"name": name, "namespace": namespace, "uid": name + "-deployment",
                        "resourceVersion": "1"}
            template = {"metadata": {"labels": labels, "annotations": {VERSION: "observer:1"}},
                        "spec": {"serviceAccountName": account}}
            self.objects[resource(namespace, "deployments", name, "/apis/apps/v1")] = {
                "metadata": metadata, "spec": {"template": template}}
            self.objects[resource(namespace, "replicasets", name + "-rs", "/apis/apps/v1")] = {
                "metadata": {"name": name + "-rs", "namespace": namespace,
                             "uid": name + "-rs-uid", "resourceVersion": "1",
                             "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment",
                                                  "name": name, "uid": metadata["uid"], "controller": True}]}}
            self.objects[core(namespace, "pods", name + "-pod")] = {
                "kind": "Pod", "metadata": {
                    "name": name + "-pod", "namespace": namespace, "uid": name + "-pod-uid",
                    "resourceVersion": "1", "labels": labels, "annotations": {VERSION: "observer:1"},
                    "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet",
                                         "name": name + "-rs", "uid": name + "-rs-uid", "controller": True}]},
                "spec": {"serviceAccountName": account}}
        self.objects[SANDBOX] = {
            "metadata": {"name": "agent", "namespace": CORE, "uid": "sandbox-uid", "resourceVersion": "1"},
            "status": {"serviceObservation": {"phase": "Prepared", "namespaceUid": RUNTIME + "-uid",
                                              "deploymentUid": "agent-deployment", "version": "observer:1"}}}

    def get(self, path):
        if path.endswith("/pods"):
            namespace = path.split("/")[-2]
            return {"items": copy.deepcopy([
                item for item in self.objects.values()
                if item.get("kind") == "Pod" and item["metadata"]["namespace"] == namespace])}
        return copy.deepcopy(self.objects[path])


def logs(*args, **kwargs):
    return event(component="controller" if args[3] == CORE else "router")


class ObservationDiagnosticsTests(unittest.TestCase):
    def test_transport_facts_are_optional_atomic_booleans_and_old_absence_stays_unknown(self):
        flags = {key: False for key in CLIENT_FIELDS}
        old = json.loads(event(message="Private observation target client pending",
                               stage="observer_target_client", http_status=0, **flags))
        old_record = project(json.dumps(old), "router")[0]
        self.assertTrue(all(key not in old_record for key in CLIENT_TRANSPORT_FIELDS))
        for started, connected, handshake in [(False, False, False), (True, False, False),
                                               (True, True, False), (True, True, True)]:
            value = copy.deepcopy(old)
            transport = dict.fromkeys(CLIENT_TRANSPORT_FIELDS, True)
            transport.update(tcp_connect_started=started, tcp_connected=connected,
                             http_handshake_complete=handshake)
            value["fields"].update(transport, endpoint="private-endpoint-canary",
                                   error="private-error-canary", body="private-body-canary")
            records = project(json.dumps(value), "router")
            self.assertEqual(records, [{**old_record, **transport}])
            self.assertNotIn("canary", json.dumps(records))
            self.assertEqual(project(json.dumps(value), "controller"), [])
        for key in CLIENT_TRANSPORT_FIELDS:
            for invalid in ("private-value-canary", 0, None, [], {}):
                value = copy.deepcopy(old)
                value["fields"].update(dict.fromkeys(CLIENT_TRANSPORT_FIELDS, False))
                value["fields"][key] = invalid
                self.assertEqual(project(json.dumps(value), "router"), [])
            del value["fields"][key]
            self.assertEqual(project(json.dumps(value), "router"), [])

    def test_client_boundaries_are_fixed_value_free_and_do_not_claim_packet_delivery(self):
        flags = {key: False for key in CLIENT_FIELDS}
        flags.update(client_initialized=True, request_built=True, service_entered=True,
                     dispatch_observable=True, after_auth_dispatch=True,
                     config_observed=True, tls_verification=True)
        raw = event(message="Private observation target client pending", stage="observer_target_client",
                    http_status=0, url="private-url-canary", token="private-token-canary",
                    certificate="private-certificate-canary", **flags)
        self.assertEqual(project(raw, "router"), [
            {"stage": "observer_target_client", "http_status": 0, **flags}])
        self.assertNotIn("canary", json.dumps(project(raw, "router")))
        self.assertEqual(project(raw, "controller"), [])
        self.assertNotIn("packet", json.dumps(project(raw, "router")))

    def test_client_boundary_fields_require_booleans_and_current_router_provenance(self):
        flags = {key: False for key in CLIENT_FIELDS}
        valid = json.loads(event(message="Private observation target client pending",
                                 stage="observer_target_client", http_status=0, **flags))
        for field in CLIENT_FIELDS:
            for invalid in ("false", 0, None, [], {}):
                value = copy.deepcopy(valid)
                value["fields"][field] = invalid
                self.assertEqual(project(json.dumps(value), "router"), [])
            value = copy.deepcopy(valid)
            del value["fields"][field]
            self.assertEqual(project(json.dumps(value), "router"), [])
        fixture = Fixture()
        with patch("observation_diagnostics.command", return_value=json.dumps(valid)):
            result = collect(SimpleNamespace(admin=fixture), TARGET)
        self.assertEqual(result["samples"][1]["records"][0]["stage"], "observer_target_client")
        fixture.objects[POD]["metadata"]["ownerReferences"][0]["uid"] = "replaced"
        with patch("observation_diagnostics.command", return_value=json.dumps(valid)):
            result = collect(SimpleNamespace(admin=fixture), TARGET)
        self.assertFalse(result["samples"][1]["available"])
        self.assertEqual(result["samples"][1]["records"], [])

    def test_only_fixed_fields_survive(self):
        result = project(event(error="private-body-canary", token="private-token-canary"), "router")
        self.assertEqual(result, [{"stage": "observer_target_read", "http_status": 403,
                                   "timeout": False, "connect": False}])
        self.assertNotIn("canary", json.dumps(result))
        self.assertEqual(project(event(), "controller"), [])

    def test_unknown_stages_types_and_non_events_are_dropped(self):
        raw = "\n".join([
            "raw-private-canary", "null", "[]", "42",
            event(stage="private-stage-canary"), event(stage=[]), event(http_status=True),
            event(http_status=700), event(timeout="true"), event(connect=1),
            event(message="private-message-canary"),
        ])
        self.assertEqual(project(raw, "router"), [])

    def test_bounds_and_consecutive_duplicates(self):
        self.assertEqual(len(project("\n".join([event()] * 600), "router")), 1)
        result = project("\n".join(event(http_status=400 + index % 100) for index in range(600)), "router")
        self.assertEqual(len(result), 24)
        self.assertEqual(result[-1]["http_status"], 499)

    def test_zero_status_is_not_misreported_as_http_success(self):
        result = project(event(http_status=0, timeout=True, connect=True), "router")
        self.assertEqual(result[0]["http_status"], 0)
        self.assertTrue(result[0]["timeout"])
        self.assertTrue(result[0]["connect"])

    def test_collection_errors_never_emit_exception_or_raw_output(self):
        with patch("observation_diagnostics.command", side_effect=RuntimeError("private-error-canary")):
            result = collect(SimpleNamespace(admin=Fixture()), TARGET)
        self.assertFalse(result["available"])
        self.assertTrue(all(not item["available"] and not item["records"] for item in result["samples"]))
        self.assertNotIn("canary", json.dumps(result))

    def test_only_current_uid_linked_sources_supply_records(self):
        with patch("observation_diagnostics.command", side_effect=logs) as command:
            result = collect(SimpleNamespace(admin=Fixture()), TARGET)
        self.assertTrue(result["available"])
        self.assertEqual(command.call_count, 2)
        for call in command.call_args_list:
            self.assertIn("--limit-bytes=131072", call.args)
            self.assertIn("--tail=512", call.args)
        self.assertNotIn("uid", json.dumps(result))

    def test_foreign_labeled_same_account_pods_and_lineage_mismatches_are_not_logged(self):
        for path, field, replacement in [
            (POD, "kind", "Deployment"),
            (POD, "uid", "foreign-rs-uid"),
            (POD, "apiVersion", "foreign/v1"),
            (POD, "controller", False),
            (REPLICA_SET, "uid", "foreign-deployment-uid"),
            (REPLICA_SET, "name", "foreign-deployment"),
            (REPLICA_SET, "kind", "Pod"),
        ]:
            with self.subTest(path=path, field=field):
                fixture = Fixture()
                fixture.objects[path]["metadata"]["ownerReferences"][0][field] = replacement
                with patch("observation_diagnostics.command", side_effect=logs) as command:
                    result = collect(SimpleNamespace(admin=fixture), TARGET)
                self.assertFalse(result["available"])
                self.assertEqual(result["samples"][1], {"component": "router", "available": False, "records": []})
                self.assertEqual([call.args[3] for call in command.call_args_list], [CORE])

    def test_foreign_controller_pod_with_correct_labels_and_account_is_not_logged(self):
        fixture = Fixture()
        controller_pod = core(CORE, "pods", "kars-controller-pod")
        fixture.objects[controller_pod]["metadata"]["ownerReferences"] = []
        with patch("observation_diagnostics.command", side_effect=logs) as command:
            result = collect(SimpleNamespace(admin=fixture), TARGET)
        self.assertFalse(result["available"])
        self.assertEqual(result["samples"][0]["records"], [])
        self.assertEqual([call.args[3] for call in command.call_args_list], [RUNTIME])

    def test_recreated_or_incomplete_roots_and_stale_versions_are_unavailable(self):
        for path, mutate in [
            (DEPLOYMENT, lambda value: value["metadata"].update(uid="replaced")),
            (f"/api/v1/namespaces/{RUNTIME}", lambda value: value["metadata"].update(uid="replaced")),
            (SANDBOX, lambda value: value["status"]["serviceObservation"].pop("deploymentUid")),
            (SANDBOX, lambda value: value["metadata"].pop("resourceVersion")),
            (POD, lambda value: value["metadata"]["annotations"].update({VERSION: "old:1"})),
            (POD, lambda value: value["metadata"].update(deletionTimestamp="terminating")),
        ]:
            with self.subTest(path=path):
                fixture = Fixture()
                mutate(fixture.objects[path])
                with patch("observation_diagnostics.command", side_effect=logs) as command:
                    result = collect(SimpleNamespace(admin=fixture), TARGET)
                self.assertFalse(result["available"])
                self.assertEqual(result["samples"][1]["records"], [])
                self.assertEqual([call.args[3] for call in command.call_args_list], [CORE])

    def test_uid_or_revision_change_during_logs_discards_projected_records(self):
        for path in [SANDBOX, f"/api/v1/namespaces/{RUNTIME}", DEPLOYMENT, REPLICA_SET, POD]:
            for field in ["uid", "resourceVersion"]:
                with self.subTest(path=path, field=field):
                    fixture = Fixture()

                    def racing_logs(*args, **kwargs):
                        if args[3] == RUNTIME:
                            fixture.objects[path]["metadata"][field] = "changed"
                        return logs(*args, **kwargs)

                    with patch("observation_diagnostics.command", side_effect=racing_logs):
                        result = collect(SimpleNamespace(admin=fixture), TARGET)
                    self.assertFalse(result["available"])
                    self.assertEqual(result["samples"][1]["records"], [])


    def test_missing_target_and_empty_or_unrecognized_records_are_unavailable(self):
        for raw in ["", "raw-private-canary", event(stage="unknown-private-canary")]:
            with self.subTest(raw=bool(raw)):
                with patch("observation_diagnostics.command", return_value=raw):
                    result = collect(SimpleNamespace(admin=Fixture()), TARGET)
                self.assertFalse(result["available"])
                self.assertTrue(all(not item["available"] and not item["records"] for item in result["samples"]))
                self.assertNotIn("canary", json.dumps(result))
        with patch("observation_diagnostics.command", side_effect=logs):
            result = collect(SimpleNamespace(admin=Fixture()), None)
        self.assertFalse(result["available"])
        self.assertEqual(result["samples"][1]["records"], [])


class ApiOutcomeDiagnosticsTests(unittest.TestCase):
    def setUp(self):
                    self.fixture = Fixture()
                    self.setup = SimpleNamespace(admin=self.fixture)
                    self.since = datetime.now(timezone.utc) - timedelta(seconds=30)
                    self.observer = dict(TARGET, uid="sandbox-uid")
                    self.delivery = {"workspace": "native-delivery", "sandbox": "native-delivery-a", "uid": "delivery-uid"}
                    self.fixture.objects["/api/v1/namespaces/native-delivery"] = {
                        "metadata": {"name": "native-delivery", "uid": "delivery-namespace", "resourceVersion": "1"}}
                    self.delivery_path = resource("native-delivery", "karssandboxes", "native-delivery-a")
                    self.fixture.objects[self.delivery_path] = {
                        "metadata": {"name": "native-delivery-a", "namespace": "native-delivery",
                                     "uid": "delivery-uid", "resourceVersion": "1"}}

    def audit(self, actor="observer_router", code=200):
                    router = actor == "observer_router"
                    ns, account, name = (RUNTIME, "sandbox", "agent") if router else (BRIDGE, WRITER, "kars-bridge-bff")
                    target = self.observer if router else self.delivery
                    return {
                        "level": "Metadata", "stage": "ResponseComplete", "verb": "get",
                        "requestReceivedTimestamp": self.since.isoformat(),
                        "stageTimestamp": (self.since + timedelta(seconds=1)).isoformat(),
                        "user": {"username": f"system:serviceaccount:{ns}:{account}", "uid": account + "-sa-uid",
                                 "extra": {api_outcomes.POD_UID: [name + "-pod-uid"],
                                           api_outcomes.POD_NAME: [name + "-pod"]}},
                        "objectRef": {"apiGroup": "kars.azure.com", "apiVersion": "v1alpha1",
                                      "resource": "karssandboxes", "namespace": target["workspace"], "name": target["sandbox"]},
                        "responseStatus": {"code": code},
                    }

    def collect(self, events, actor="observer_router", effect=None):
                    raw = b"\n".join(json.dumps(event).encode() for event in events)

                    def read():
                        if effect:
                            effect()
                        return raw

                    with patch("api_outcome_diagnostics.read_audit_tail", side_effect=read):
                        return api_outcomes.collect(self.setup, actor,
                                                    self.observer if actor == "observer_router" else self.delivery, self.since)

    def test_real_response_statuses_are_distinct_from_absence(self):
                    for code, category in [
                        (200, "successful-response"), (201, "successful-response"),
                        (401, "unauthenticated-response"), (403, "denied-response"),
                        (409, "conflict-response"), (422, "invalid-response"),
                        (429, "rate-limited-response"), (500, "server-error-response"), (504, "timeout-response"),
                    ]:
                        with self.subTest(code=code):
                            result = self.collect([self.audit(code=code)])
                            self.assertTrue(result["available"])
                            self.assertEqual(result["category"], "outcomes-retained")
                            self.assertEqual(result["outcomes"][0]["http_status"], code)
                            self.assertEqual(result["outcomes"][0]["category"], category)
                    for code in [0, None, True, "403", 700]:
                        with self.subTest(code=code):
                            result = self.collect([self.audit(code=code)])
                            self.assertFalse(result["available"])
                            self.assertEqual(result["category"], "no-matching-evidence")
                            self.assertEqual(result["outcomes"], [])

    def test_only_allowlisted_primitive_outcomes_survive(self):
                    event = self.audit(code=403)
                    event.update(requestURI="/private-url-canary?token=private-token-canary",
                                 requestObject={"secret": "private-request-canary"},
                                 responseObject={"secret": "private-response-canary"},
                                 headers={"authorization": "private-header-canary"})
                    event["responseStatus"]["message"] = "private-error-canary"
                    event["user"]["extra"]["credential"] = ["private-credential-canary"]
                    result = self.collect([event, event])
                    self.assertEqual(result["outcomes"], [{
                        "verb": "get", "apiGroup": "kars.azure.com", "apiVersion": "v1alpha1",
                        "resource": "karssandboxes", "http_status": 403, "category": "denied-response", "count": 2,
                    }])
                    self.assertNotIn("canary", json.dumps(result))
                    self.assertNotIn("uid", json.dumps(result).lower())

    def test_other_actor_or_pod_cannot_supply_an_outcome(self):
                    mutations = [
                        lambda e: e["user"].update(uid="foreign-account"),
                        lambda e: e["user"].update(username="system:serviceaccount:foreign:sandbox"),
                        lambda e: e["user"]["extra"].update({api_outcomes.POD_UID: ["foreign-pod"]}),
                        lambda e: e["user"]["extra"].update({api_outcomes.POD_UID: ["agent-pod-uid", "foreign-pod"]}),
                        lambda e: e["user"]["extra"].pop(api_outcomes.POD_UID),
                        lambda e: e["user"]["extra"].update({api_outcomes.POD_NAME: ["foreign-pod"]}),
                        lambda e: e.update(impersonatedUser={"username": "other"}),
                    ]
                    for mutate in mutations:
                        event = self.audit(code=403)
                        mutate(event)
                        result = self.collect([event])
                        self.assertFalse(result["available"])
                        self.assertEqual(result["category"], "no-matching-evidence")

    def test_bff_write_scope_is_limited_to_captured_target_workspace_and_source(self):
                    for verb, group, version, resource_name, namespace, name in [
                        ("get", "kars.azure.com", "v1alpha1", "karscredentialgrants", "native-delivery", "workspace"),
                        ("patch", "kars.azure.com", "v1alpha1", "karssandboxes", "native-delivery", "native-delivery-a"),
                        ("create", "", "v1", "secrets", "native-delivery", "kars-credential-input-sandbox-native-delivery-a"),
                        ("create", "", "v1", "secrets", "native-delivery", None),
                        ("get", "", "v1", "secrets", "native-delivery", "kars-credential-input-sandbox-native-delivery-a"),
                        ("get", "", "v1", "namespaces", None, "native-delivery"),
                    ]:
                        event = self.audit("bff_writer", 403)
                        event["verb"] = verb
                        event["objectRef"] = {"apiGroup": group, "apiVersion": version, "resource": resource_name,
                                              "namespace": namespace, "name": name}
                        self.assertTrue(self.collect([event], "bff_writer")["available"])
                    for replacement in [
                        {"namespace": "foreign"}, {"name": "foreign"},
                        {"resource": "pods"}, {"subresource": "exec"}, {"apiVersion": "v2"},
                        {"uid": "foreign-target"},
                    ]:
                        event = self.audit("bff_writer", 403)
                        event["objectRef"].update(replacement)
                        self.assertFalse(self.collect([event], "bff_writer")["available"])
                    event = self.audit("bff_writer")
                    event["verb"] = "list"
                    event["objectRef"].update(apiGroup="", apiVersion="v1", resource="secrets", name=None)
                    self.assertFalse(self.collect([event], "bff_writer")["available"])

    def test_only_completed_responses_in_the_current_case_window_match(self):
                    for mutate in [
                        lambda e: e.update(stage="RequestReceived"),
                        lambda e: e.update(stage="ResponseStarted"),
                        lambda e: e.update(level="RequestResponse"),
                        lambda e: e.pop("requestReceivedTimestamp"),
                        lambda e: e.update(requestReceivedTimestamp=(self.since - timedelta(seconds=1)).isoformat()),
                        lambda e: e.update(stageTimestamp=(datetime.now(timezone.utc) + timedelta(minutes=1)).isoformat()),
                        lambda e: e.update(stageTimestamp="unparseable"),
                    ]:
                        event = self.audit()
                        mutate(event)
                        result = self.collect([event])
                        self.assertFalse(result["available"])
                        self.assertEqual(result["outcomes"], [])

    def test_source_replacement_during_audit_capture_discards_outcomes(self):
                    for path in [SANDBOX, f"/api/v1/namespaces/{RUNTIME}", DEPLOYMENT, REPLICA_SET, POD,
                                 core(RUNTIME, "serviceaccounts", "sandbox")]:
                        for field in ("uid", "resourceVersion"):
                            with self.subTest(path=path, field=field):
                                original = self.fixture.objects[path]["metadata"][field]
                                result = self.collect([self.audit()], effect=lambda: self.fixture.objects[path]["metadata"].update(
                                    {field: "changed"}))
                                self.assertFalse(result["available"])
                                self.assertEqual(result["category"], "source-unavailable")
                                self.assertEqual(result["outcomes"], [])
                                self.fixture.objects[path]["metadata"][field] = original

    def test_bff_foreign_workload_or_changed_target_never_reads_audit(self):
                    pod_path = core(BRIDGE, "pods", "kars-bridge-bff-pod")
                    self.fixture.objects[pod_path]["metadata"]["ownerReferences"] = []
                    with patch("api_outcome_diagnostics.read_audit_tail") as read:
                        result = api_outcomes.collect(self.setup, "bff_writer", self.delivery, self.since)
                    self.assertEqual(result["category"], "source-unavailable")
                    read.assert_not_called()
                    self.fixture = Fixture()
                    self.setup.admin = self.fixture
                    self.fixture.objects[self.delivery_path] = {
                        "metadata": {"name": "native-delivery-a", "namespace": "native-delivery",
                                     "uid": "replacement", "resourceVersion": "1"}}
                    with patch("api_outcome_diagnostics.read_audit_tail") as read:
                        result = api_outcomes.collect(self.setup, "bff_writer", self.delivery, self.since)
                    self.assertFalse(result["available"])
                    read.assert_not_called()

    def test_audit_unavailable_and_no_matching_evidence_are_not_success_or_denial(self):
                    with patch("api_outcome_diagnostics.read_audit_tail", side_effect=RuntimeError("private-error-canary")):
                        result = api_outcomes.collect(self.setup, "observer_router", self.observer, self.since)
                    self.assertEqual(result["category"], "audit-unavailable")
                    self.assertFalse(result["available"])
                    self.assertNotIn("canary", json.dumps(result))
                    for raw in [b"", b"invalid-private-canary\nnull\n[]", json.dumps(self.audit("bff_writer")).encode()]:
                        with patch("api_outcome_diagnostics.read_audit_tail", return_value=raw):
                            result = api_outcomes.collect(self.setup, "observer_router", self.observer, self.since)
                        self.assertEqual(result["category"], "no-matching-evidence")
                        self.assertFalse(result["available"])
                        self.assertEqual(result["outcomes"], [])

    def test_reader_uses_only_the_existing_bounded_metadata_audit_tail(self):
                    completed = SimpleNamespace(returncode=0, stdout=b"{}")
                    with patch("api_outcome_diagnostics.subprocess.run", return_value=completed) as run:
                        self.assertEqual(api_outcomes.read_audit_tail(), b"{}")
                    self.assertEqual(run.call_args.args[0][-4:], [
                        "tail", "-c", str(api_outcomes.MAX_BYTES), "/var/log/kars-native-audit/audit.log"])
                    self.assertEqual(run.call_args.kwargs["timeout"], 15)
                    self.assertEqual(run.call_args.kwargs["stderr"], api_outcomes.subprocess.DEVNULL)
                    completed.stdout = b"x" * (api_outcomes.MAX_BYTES + 1)
                    with patch("api_outcome_diagnostics.subprocess.run", return_value=completed):
                        with self.assertRaises(Exception):
                            api_outcomes.read_audit_tail()


class UnexpectedDiagnosticErrorsTests(unittest.TestCase):
    def test_stage_collector_does_not_hide_programming_errors(self):
        with patch("observation_diagnostics.resolve_actor", side_effect=NameError("fixture bug")):
            with self.assertRaises(NameError):
                collect(SimpleNamespace(), TARGET)

    def test_api_collector_does_not_hide_programming_errors(self):
        with patch("api_outcome_diagnostics.resolve_actor", side_effect=NameError("fixture bug")):
            with self.assertRaises(NameError):
                api_outcomes.collect(SimpleNamespace(), "observer_router", TARGET,
                                     datetime.now(timezone.utc))


if __name__ == "__main__":
    unittest.main()
