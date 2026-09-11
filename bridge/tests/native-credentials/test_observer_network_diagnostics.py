import contextlib
import copy
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

from native_api import CORE, Failure, core, resource
from observation_diagnostics import VERSION
import observer_network_diagnostics as network
import observer_cilium_diagnostics as cilium
from test_observation_diagnostics import Fixture, SANDBOX, RUNTIME, POD, DEPLOYMENT, REPLICA_SET

TARGET = {"workspace": CORE, "sandbox": "agent", "uid": "sandbox-uid", "task": "native-observation-task"}
FAILED = {"result": "failed", "failure": network.FAILURE}
POLICY = resource(RUNTIME, "networkpolicies", "sandbox-policy", network.NETWORK)
CEP = resource(RUNTIME, "ciliumendpoints", "agent-pod", cilium.CILIUM)
CILIUM_POD = core("kube-system", "pods", "cilium-worker")
ENV = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "Azure/kars",
       "CORE_REVISION": network.CORE_REVISION}
OUTCOMES = {"available": True, "category": "outcomes-retained", "coverage": "bounded-metadata-tail",
            "outcomes": [{"verb": "get", "apiGroup": "kars.azure.com", "apiVersion": "v1alpha1",
                          "resource": "karssandboxes", "http_status": 403,
                          "category": "denied-response", "count": 1}]}


def redacted_context(server="https://127.0.0.1:36443"):
    name = "kind-bridge-native"
    return {"current-context": name,
            "contexts": [{"name": name, "context": {"cluster": name, "user": name}}],
            "clusters": [{"name": name, "cluster": {
                "server": server, "certificate-authority-data": "DATA+OMITTED"}}],
            "users": [{"name": name, "user": {
                "client-certificate-data": "DATA+OMITTED", "client-key-data": "DATA+OMITTED"}}],
            "extensions": [{"name": "private-context-canary", "extension": "private-context-canary"}]}


class NetworkFixture(Fixture):
    def __init__(self):
        super().__init__()
        self.server, self.host, self.port = "https://127.0.0.1:36443", "127.0.0.1", 36443
        self.created, self.deleted = [], []
        self.after_create = None
        self.delete_conflict = False
        self.source = self.objects[SANDBOX]
        self.source["metadata"]["generation"] = 1
        self.source["spec"] = {"runtime": {"kind": "OpenClaw"}, "governance": {"enabled": True}}
        namespace = self.objects[f"/api/v1/namespaces/{RUNTIME}"]
        namespace["metadata"]["annotations"] = {"kars.azure.com/sandbox-uid": "sandbox-uid"}
        deployment = self.objects[DEPLOYMENT]
        deployment["metadata"].update(generation=3, annotations={
            "kars.azure.com/credential-sandbox-uid": "sandbox-uid",
            "kars.azure.com/credential-namespace-uid": RUNTIME + "-uid"})
        pod = self.objects[POD]
        pod["metadata"]["labels"]["pod-template-hash"] = "abc123"
        pod["spec"].update(nodeName="bridge-native-worker", containers=[
            {"name": "inference-router", "securityContext": {
                "runAsUser": 1001, "allowPrivilegeEscalation": False}},
            {"name": "agent", "env": [{"name": "PRIVATE", "value": "private-env-canary"}]},
        ])
        pod["status"] = {"phase": "Running", "hostIP": "172.18.0.2", "podIP": "10.244.1.10", "containerStatuses": [
            {"name": "inference-router", "containerID": "containerd://private-id-canary",
             "ready": True, "restartCount": 0}]}
        self.objects["/api/v1/namespaces/default"] = {
            "metadata": {"name": "default", "uid": "default-uid", "resourceVersion": "1"}}
        self.objects[network.API_SERVICE] = {
            "metadata": {"name": "kubernetes", "namespace": "default", "uid": "service-uid",
                         "resourceVersion": "1", "annotations": {"private": "private-service-canary"}},
            "spec": {"type": "ClusterIP", "clusterIP": "10.96.0.1", "clusterIPs": ["10.96.0.1"],
                     "ports": [{"name": "https", "protocol": "TCP", "port": 443, "targetPort": 6443}]}}
        self.objects[network.API_ENDPOINTS] = {
            "metadata": {"name": "kubernetes", "namespace": "default", "uid": "endpoints-uid",
                         "resourceVersion": "1"},
            "subsets": [{"addresses": [{"ip": "172.18.0.2"}],
                         "ports": [{"name": "https", "protocol": "TCP", "port": 6443}]}]}
        self.objects[POLICY] = {
            "kind": "NetworkPolicy",
            "metadata": {"name": "sandbox-policy", "namespace": RUNTIME, "uid": "baseline-policy",
                         "resourceVersion": "1", "annotations": {"private": "private-policy-canary"}},
            "spec": {"podSelector": {"matchLabels": {"kars.azure.com/sandbox": "agent"}},
                     "policyTypes": ["Ingress", "Egress"], "egress": [
                         {"to": [{"ipBlock": {"cidr": "0.0.0.0/0",
                                             "except": ["10.0.0.0/8", "172.16.0.0/12"]}}],
                          "ports": [{"protocol": "TCP", "port": 443}]}]}}
        self.objects["/api/v1/namespaces/kube-system"] = {
            "metadata": {"name": "kube-system", "uid": "kube-system-uid", "resourceVersion": "1"}}
        self.objects[cilium.CONFIG] = {
            "metadata": {"name": "cilium-config", "namespace": "kube-system", "uid": "config-uid", "resourceVersion": "1"},
            "data": {"policy-cidr-match-mode": "", "enable-policy": "default", "kube-proxy-replacement": "false",
                     "private-unrelated-key": "private-config-canary"}}
        container = {"name": "cilium-agent", "image": "quay.io/cilium/cilium:v1.18.5"}
        self.objects[cilium.DAEMONSET] = {
            "metadata": {"name": "cilium", "namespace": "kube-system", "uid": "daemonset-uid", "resourceVersion": "1"},
            "spec": {"selector": {"matchLabels": {"k8s-app": "cilium"}},
                     "template": {"spec": {"serviceAccountName": "cilium", "containers": [container]}}}}
        self.objects[core("kube-system", "serviceaccounts", "cilium")] = {
            "metadata": {"name": "cilium", "namespace": "kube-system", "uid": "cilium-account-uid", "resourceVersion": "1"}}
        self.objects[CILIUM_POD] = {
            "kind": "Pod", "metadata": {"name": "cilium-worker", "namespace": "kube-system",
                "uid": "cilium-pod-uid", "resourceVersion": "1", "labels": {"k8s-app": "cilium"},
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "DaemonSet", "name": "cilium",
                                     "uid": "daemonset-uid", "controller": True}]},
            "spec": {"nodeName": "bridge-native-worker", "serviceAccountName": "cilium", "containers": [container]},
            "status": {"hostIP": "172.18.0.2", "containerStatuses": [
                {"name": "cilium-agent", "ready": True, "containerID": "private-cilium-container-canary",
                 "restartCount": 0}]}}
        self.objects[CEP] = {
            "metadata": {"name": "agent-pod", "namespace": RUNTIME, "uid": "cep-uid", "resourceVersion": "1",
                         "ownerReferences": [{"apiVersion": "v1", "kind": "Pod", "name": "agent-pod",
                                              "uid": "agent-pod-uid"}]},
            "status": {"id": 123, "identity": {"id": 12345, "labels": ["private-label-canary"]},
                       "networking": {"node": "172.18.0.2", "addressing": [{"ipv4": "10.244.1.10"}]},
                       "log": ["private-endpoint-log-canary"]}}
        self.effective_config = ["[]", "true", "true"]
        self.kube_proxy_status = ["False"]
        self.policy_revisions = [7, 7]

    def get(self, path):
        if path.endswith("/networkpolicies"):
            return {"items": copy.deepcopy([item for item in self.objects.values()
                                           if item.get("kind") == "NetworkPolicy"])}
        if path.endswith("/ciliumnetworkpolicies"):
            return {"items": copy.deepcopy([item for item in self.objects.values()
                                           if item.get("kind") == "CiliumNetworkPolicy"])}
        return super().get(path)

    def read_projection(self, _agent, endpoint_id=None, witness=None):
        if endpoint_id is None:
            return self.effective_config
        return [str(endpoint_id), "12345", *(str(value) for value in self.policy_revisions),
                "both", RUNTIME, "agent-pod"]

    def read_kube_proxy_projection(self, _agent, witness=None):
        return self.kube_proxy_status

    def optional(self, path):
        return self.get(path) if path in self.objects else None

    def create(self, path, value):
        value = copy.deepcopy(value)
        value["metadata"].update(uid="diagnostic-policy-uid", resourceVersion="7")
        self.created.append((path, copy.deepcopy(value)))
        self.objects[path + "/" + value["metadata"]["name"]] = copy.deepcopy(value)
        if self.after_create:
            self.after_create()
        return value

    def request(self, method, path, body=None, expected=(200,)):
        if method == "GET":
            try:
                return 200, self.get(path)
            except KeyError:
                return 404, {"message": "private-api-response-canary"}
        if self.delete_conflict:
            raise Failure("DELETE returned 409")
        current = self.objects[path]
        assert method == "DELETE"
        assert expected == (200, 202)
        assert body["preconditions"] == {
            "uid": current["metadata"]["uid"], "resourceVersion": current["metadata"]["resourceVersion"]}
        self.deleted.append((method, path, copy.deepcopy(body)))
        del self.objects[path]
        return 200, {}


class ObserverNetworkTests(unittest.TestCase):
    def setUp(self):
        self.api = NetworkFixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})

    def collect(self, outcomes=OUTCOMES, failed=FAILED, config=None, commands=None):
        config = redacted_context() if config is None else config
        if commands is None:
            commands = lambda *args, **_kwargs: json.dumps(config) if args[0] == "kubectl" else "bridge-native"
        with patch.dict(os.environ, ENV), \
                patch.object(network, "command", side_effect=commands), \
                patch.object(cilium, "read_projection", side_effect=self.api.read_projection), \
                patch.object(cilium, "read_kube_proxy_projection", side_effect=self.api.read_kube_proxy_projection), \
                patch.object(network, "api_outcomes", return_value=outcomes), \
                patch.object(network.time, "sleep"):
            return network.collect(self.setup, TARGET, failed)

    def test_snapshot_projects_exact_api_targets_without_private_fields(self):
        before = network.snapshot(self.setup, TARGET)
        self.assertEqual(before["facts"]["destinations"], [
            {"address": "10.96.0.1", "port": 443}, {"address": "172.18.0.2", "port": 6443}])
        self.assertEqual(before["facts"]["networkPolicies"][0]["apiDeclaredMatches"], [
            {"address": "10.96.0.1", "port": 443, "ruleMatches": [False]},
            {"address": "172.18.0.2", "port": 6443, "ruleMatches": [False]}])
        self.assertNotIn("canary", json.dumps(before["facts"]))
        self.assertFalse(before["facts"]["cniBehaviorProven"])

    def test_status_only_sandbox_ready_update_during_snapshot_reads_is_retained(self):
        self.api.source["metadata"]["managedFields"] = [
            {"manager": "native", "operation": "Update", "apiVersion": "kars.azure.com/v1alpha1",
             "fieldsType": "FieldsV1", "fieldsV1": {"f:spec": {}}, "time": "2026-09-10T20:00:00Z"},
            {"manager": "controller", "operation": "Update", "subresource": "status",
             "fieldsType": "FieldsV1", "fieldsV1": {"f:status": {}}, "time": "2026-09-10T20:00:00Z"}]
        before = network.snapshot(self.setup, TARGET)
        original_get = self.api.get
        def advancing_get(path):
            if path == network.API_SERVICE:
                self.api.source["metadata"]["resourceVersion"] = "2"
                self.api.source["status"]["serviceObservation"].update(phase="Ready", reason="Verified")
                self.api.source["status"]["conditions"] = [{"type": "Ready", "status": "True"}]
                self.api.source["metadata"]["managedFields"][1]["time"] = "2026-09-10T20:00:01Z"
            return original_get(path)
        with patch.object(self.api, "get", side_effect=advancing_get):
            after = network.snapshot(self.setup, TARGET)
        self.assertTrue(after["ready"])
        self.assertEqual(after["actor"]["anchors"][SANDBOX]["metadata"]["resourceVersion"], "2")
        self.assertEqual(before["stable"], after["stable"])

    def test_status_update_with_any_authority_mutation_during_snapshot_is_rejected(self):
        mutations = [
            lambda value: value["spec"]["governance"].update(enabled=False),
            lambda value: value["spec"]["governance"].update(enabled=1),
            lambda value: value["metadata"].update(generation=2),
            lambda value: value["metadata"].update(uid="replacement"),
            lambda value: value["metadata"].update(namespace="foreign"),
            lambda value: value["metadata"].update(name="foreign"),
            lambda value: value["metadata"].update(labels={"authority": "changed"}),
            lambda value: value["metadata"].update(annotations={"authority": "changed"}),
            lambda value: value["metadata"].update(ownerReferences=[{"uid": "foreign"}]),
            lambda value: value["metadata"].update(finalizers=["foreign"]),
            lambda value: value["metadata"].update(deletionTimestamp="terminating"),
            lambda value: value["metadata"].update(managedFields=[{"manager": "new-spec-manager"}]),
            lambda value: value["status"]["serviceObservation"].update(version="rotated"),
            lambda value: value["status"]["serviceObservation"].update(namespaceUid="foreign"),
            lambda value: value["status"]["serviceObservation"].update(deploymentUid="foreign"),
            lambda value: value["status"]["serviceObservation"].update(grant={"uid": "foreign"}),
            lambda value: value["status"]["serviceObservation"].update(secret={"uid": "foreign"}),
            lambda value: value["status"]["serviceObservation"].update(capability="different"),
        ]
        for index, mutation in enumerate(mutations):
            with self.subTest(index=index):
                self.setUp()
                original_get = self.api.get
                changed = False
                def racing_get(path):
                    nonlocal changed
                    if path == network.API_SERVICE and not changed:
                        changed = True
                        self.api.source["metadata"]["resourceVersion"] = "2"
                        self.api.source["status"]["serviceObservation"]["phase"] = "Ready"
                        mutation(self.api.source)
                    return original_get(path)
                with patch.object(self.api, "get", side_effect=racing_get):
                    with self.assertRaises(Failure):
                        network.snapshot(self.setup, TARGET)

    def test_authority_mutations_between_snapshots_remain_unstable_even_without_generation_change(self):
        before = network.snapshot(self.setup, TARGET)
        self.api.source["spec"]["governance"]["enabled"] = False
        after = network.snapshot(self.setup, TARGET)
        self.assertNotEqual(before["stable"], after["stable"])

    def test_inflight_status_progress_after_policy_creation_is_retained_and_cleaned_up(self):
        armed = False
        def created():
            nonlocal armed
            armed = True
        self.api.after_create = created
        original_get = self.api.get
        def advancing_get(path):
            nonlocal armed
            if armed and path == network.API_SERVICE:
                armed = False
                self.api.source["metadata"]["resourceVersion"] = "2"
                self.api.source["status"]["serviceObservation"].update(phase="Ready", reason="Verified")
            return original_get(path)
        absent = {"available": False, "category": "no-matching-evidence",
                  "coverage": "bounded-metadata-tail", "outcomes": []}
        failed = copy.deepcopy(FAILED)
        with patch.object(self.api, "get", side_effect=advancing_get):
            result = self.collect(outcomes=absent, failed=failed)
        self.assertEqual(result["category"], "same-pod-progress-with-temporary-policy")
        self.assertTrue(result["samePodObserverReady"])
        self.assertEqual(result["duringApiOutcomes"], absent)
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertEqual(failed, FAILED)

    def test_other_anchor_resource_versions_stay_strict_within_snapshot(self):
        for changed_path in (DEPLOYMENT, POD, POLICY, network.API_SERVICE, network.API_ENDPOINTS):
            with self.subTest(path=changed_path):
                self.setUp()
                original_get = self.api.get
                reads = {}
                def racing_get(path):
                    reads[path] = reads.get(path, 0) + 1
                    if path == changed_path and reads[path] == (1 if path == POLICY else 2):
                        self.api.objects[path]["metadata"]["resourceVersion"] = "changed"
                    return original_get(path)
                with patch.object(self.api, "get", side_effect=racing_get):
                    with self.assertRaises(Failure):
                        network.snapshot(self.setup, TARGET)

    def test_nonloopback_setup_origin_never_creates_a_policy(self):
        self.api.server = self.setup.cluster["server"] = "https://203.0.113.1:36443"
        self.api.host = "203.0.113.1"
        result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_only_exact_host_ports_are_added_and_owned_cleanup_is_fenced(self):
        original = copy.deepcopy(self.api.objects)
        failed = copy.deepcopy(FAILED)
        result = self.collect(failed=failed)
        self.assertEqual(failed, FAILED)
        self.assertEqual(result["category"], "same-pod-progress-with-temporary-policy")
        self.assertTrue(result["diagnosticOnly"])
        self.assertEqual(result["originalResult"], "failed")
        self.assertFalse(result["cniAcceptanceQualified"])
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertEqual(len(self.api.created), 1)
        self.assertEqual(len(self.api.deleted), 1)
        policy = self.api.created[0][1]
        self.assertEqual(policy["kind"], "CiliumNetworkPolicy")
        self.assertEqual(policy["apiVersion"], "cilium.io/v2")
        self.assertTrue(self.api.created[0][0].endswith("/ciliumnetworkpolicies"))
        self.assertEqual(policy["spec"], {
            "endpointSelector": {"matchLabels": {"kars.azure.com/sandbox": "agent", "pod-template-hash": "abc123"}},
            "egress": [{"toEntities": ["kube-apiserver"], "toPorts": [{"ports": [
                {"protocol": "TCP", "port": "443"}, {"protocol": "TCP", "port": "6443"}]}]}]})
        self.assertTrue(result["apiResponseObserved"])
        self.assertFalse(result["samePodObserverReady"])
        self.assertEqual(result["duringApiOutcomes"]["outcomes"][0]["http_status"], 403)
        self.assertEqual(policy["metadata"]["ownerReferences"][0]["uid"], "agent-deployment")
        self.assertEqual(self.api.deleted[0][2]["preconditions"], {
            "uid": "diagnostic-policy-uid", "resourceVersion": "7"})
        self.assertEqual(self.api.objects, original)
        self.assertNotIn("canary", json.dumps(result))

    def test_success_blocked_unrelated_failure_and_non_disposable_hosts_never_write(self):
        for failed in (None, {"result": "passed"}, {"result": "blocked"},
                       {"result": "failed", "failure": "different failure"}):
            self.assertEqual(self.collect(failed=failed)["category"], "not-eligible")
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}):
            self.assertFalse(network.collect(self.setup, TARGET, FAILED)["available"])
        for commands in (["foreign-context"], [json.dumps(redacted_context()), "foreign-cluster"]):
            with patch.dict(os.environ, ENV), patch.object(network, "command", side_effect=commands):
                self.assertFalse(network.collect(self.setup, TARGET, FAILED)["available"])
        self.assertEqual(self.api.created, [])

    def test_selected_origin_uses_only_redacted_minified_context_and_bound_client(self):
        config = redacted_context()
        with patch.object(network, "command", return_value=json.dumps(config)) as command:
            self.assertEqual(network.selected_origin(self.setup), ("127.0.0.1", 36443))
        command.assert_called_once_with("kubectl", "config", "view", "--minify", "-o", "json", timeout=10)
        self.assertNotIn("--raw", command.call_args.args)
        for server in ("http://127.0.0.1:36443", "https://203.0.113.1:36443",
                       "https://localhost:36443", "https://127.0.0.1:36444",
                       "https://user:private@127.0.0.1:36443",
                       "https://127.0.0.1:36443/private", "https://127.0.0.1:36443?token=private",
                       "https://127.0.0.1:36443#private", "https://127.0.0.1:0",
                       " https://127.0.0.1:36443"):
            with self.subTest(server=server):
                result = self.collect(config=redacted_context(server))
                self.assertFalse(result["policyCreated"])
                self.assertNotIn("private", json.dumps(result))
        self.assertEqual(self.api.created, [])

    def test_mismatched_context_bindings_and_unexpected_transport_never_write(self):
        mutations = [
            lambda config: config.update(**{"current-context": "foreign"}),
            lambda config: config["contexts"][0].update(name="foreign"),
            lambda config: config["contexts"][0]["context"].update(cluster="foreign"),
            lambda config: config["contexts"][0]["context"].update(user="foreign"),
            lambda config: config["clusters"][0].update(name="foreign"),
            lambda config: config["users"][0].update(name="foreign"),
            lambda config: config["clusters"].append(copy.deepcopy(config["clusters"][0])),
            lambda config: config["clusters"][0]["cluster"].update(**{"proxy-url": "https://private"}),
            lambda config: config["clusters"][0]["cluster"].update(**{"insecure-skip-tls-verify": True}),
            lambda config: config["clusters"][0]["cluster"].update(**{"tls-server-name": "foreign"}),
            lambda config: config["contexts"][0].update(context="malformed"),
            lambda config: config["clusters"][0].update(cluster=None),
        ]
        for mutation in mutations:
            config = redacted_context()
            mutation(config)
            result = self.collect(config=config)
            self.assertFalse(result["policyCreated"])
            self.assertEqual(result["stage"], "disposable-host")
        for change in ("admin-host", "admin-port", "captured-cluster"):
            self.setUp()
            if change == "admin-host":
                self.api.host = "203.0.113.1"
            elif change == "admin-port":
                self.api.port = 36444
            else:
                self.setup.cluster["server"] = "https://127.0.0.1:36444"
            self.assertFalse(self.collect()["policyCreated"])
            self.assertEqual(self.api.created, [])

    def test_context_switch_during_snapshot_is_rejected_before_policy_creation(self):
        result = self.collect(commands=[
            json.dumps(redacted_context()), "bridge-native",
            json.dumps(redacted_context("https://127.0.0.1:36444")),
        ])
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])
        self.assertEqual(self.api.deleted, [])

    def test_literal_ipv6_loopback_origin_is_accepted_without_changing_api_targets(self):
        self.api.server = self.setup.cluster["server"] = "https://[::1]:36443"
        self.api.host = "::1"
        result = self.collect(config=redacted_context(self.api.server))
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")

    def test_ready_or_unisolated_observer_gets_no_new_egress_isolation(self):
        self.api.source["status"]["serviceObservation"]["phase"] = "Ready"
        self.assertEqual(self.collect()["category"], "already-ready-without-intervention")
        self.api.source["status"]["serviceObservation"]["phase"] = "Prepared"
        self.api.objects[POLICY]["spec"]["policyTypes"] = ["Ingress"]
        self.assertEqual(self.collect()["category"], "no-observed-egress-isolation-no-intervention")
        self.assertEqual(self.api.created, [])

    def test_foreign_matching_consumers_and_forged_actor_lineage_never_get_a_policy(self):
        for foreign in ("missing-owner", "wrong-account", "stale-version", "host-network", "terminating"):
            with self.subTest(foreign=foreign):
                self.setUp()
                pod = copy.deepcopy(self.api.objects[POD])
                pod["metadata"].update(name="foreign", uid="foreign-uid")
                if foreign == "missing-owner":
                    pod["metadata"]["ownerReferences"] = []
                elif foreign == "wrong-account":
                    pod["spec"]["serviceAccountName"] = "foreign"
                elif foreign == "stale-version":
                    pod["metadata"]["annotations"][VERSION] = "old"
                elif foreign == "host-network":
                    pod["spec"]["hostNetwork"] = True
                else:
                    pod["metadata"]["deletionTimestamp"] = "terminating"
                self.api.objects[core(RUNTIME, "pods", "foreign")] = pod
                self.assertFalse(self.collect()["available"])
                self.assertEqual(self.api.created, [])

    def test_invalid_api_addresses_ports_and_inventory_are_rejected(self):
        for address in (True, 1, "0.0.0.1", "8.8.8.8", "127.0.0.1", "169.254.1.2", "0.0.0.0", "224.1.2.3",
                        "10.96.0.0/12", "https://10.96.0.1?token=canary", "::1", "fe80::1"):
            with self.subTest(address=address):
                self.setUp()
                self.api.objects[network.API_ENDPOINTS]["subsets"][0]["addresses"][0]["ip"] = address
                result = self.collect()
                self.assertFalse(result["available"])
                self.assertEqual(self.api.created, [])
                self.assertNotIn("canary", json.dumps(result))
        for mutate in (
            lambda service, endpoint: service["spec"].update(type="ExternalName"),
            lambda service, endpoint: service["spec"].update(externalIPs=["10.1.1.1"]),
            lambda service, endpoint: service["spec"]["ports"][0].update(port=22),
            lambda service, endpoint: service["spec"]["ports"][0].update(targetPort=4443),
            lambda service, endpoint: endpoint["subsets"][0]["ports"][0].update(port=443),
            lambda service, endpoint: endpoint.update(subsets=[]),
            lambda service, endpoint: endpoint["subsets"][0].update(addresses=[{"ip": "172.18.0.2"}] * 9),
        ):
            self.setUp()
            mutate(self.api.objects[network.API_SERVICE], self.api.objects[network.API_ENDPOINTS])
            self.assertFalse(self.collect()["available"])
            self.assertEqual(self.api.created, [])

    def test_ipv6_targets_do_not_widen_entity_or_ports(self):
        service = self.api.objects[network.API_SERVICE]
        service["spec"].update(clusterIP="fd00::1", clusterIPs=["fd00::1"])
        self.api.objects[network.API_ENDPOINTS]["subsets"][0]["addresses"] = [{"ip": "fd01::2"}]
        plan = network.policy_plan(network.snapshot(self.setup, TARGET), "diagnostic")
        self.assertEqual(plan["spec"]["egress"], [{"toEntities": ["kube-apiserver"], "toPorts": [{"ports": [
            {"protocol": "TCP", "port": "443"}, {"protocol": "TCP", "port": "6443"}]}]}])

    def test_changed_actor_api_or_baseline_policy_aborts_and_removes_only_owned_policy(self):
        mutations = [
            lambda api: api.objects[POD]["metadata"].update(uid="replacement-pod"),
            lambda api: api.objects[POD]["status"]["containerStatuses"][0].update(restartCount=1),
            lambda api: api.objects[POD]["status"]["containerStatuses"][0].update(containerID="new"),
            lambda api: api.objects[DEPLOYMENT]["metadata"].update(generation=4),
            lambda api: api.objects[REPLICA_SET]["metadata"]["ownerReferences"][0].update(uid="foreign"),
            lambda api: api.source["metadata"].update(generation=2),
            lambda api: api.objects[network.API_ENDPOINTS]["metadata"].update(resourceVersion="2"),
            lambda api: api.objects[POLICY]["metadata"].update(resourceVersion="2"),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutations.index(mutate)):
                self.setUp()
                self.api.after_create = lambda: mutate(self.api)
                result = self.collect()
                self.assertEqual(result["category"], "provenance-or-operation-unavailable")
                self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
                self.assertEqual(len(self.api.deleted), 1)

    def test_foreign_policy_replacement_and_namespace_recreation_are_never_deleted(self):
        for replace in ("policy", "namespace"):
            self.setUp()
            def changed():
                if replace == "policy":
                    name = self.api.created[0][1]["metadata"]["name"]
                    self.api.objects[resource(RUNTIME, "ciliumnetworkpolicies", name, network.CILIUM)]["metadata"]["uid"] = "foreign"
                else:
                    self.api.objects[f"/api/v1/namespaces/{RUNTIME}"]["metadata"]["uid"] = "foreign"
            self.api.after_create = changed
            result = self.collect()
            self.assertEqual(result["cleanup"], "unverified-or-refused")
            self.assertEqual(result["category"], "cleanup-not-verified")
            self.assertEqual(self.api.deleted, [])

    def test_conflicting_cleanup_is_not_reported_as_verified(self):
        self.api.delete_conflict = True
        result = self.collect()
        self.assertEqual(result["cleanup"], "unverified-or-refused")
        self.assertEqual(result["category"], "cleanup-not-verified")
        self.assertEqual(self.api.deleted, [])

    def test_cleanup_uses_the_current_owned_resource_version(self):
        def update():
            name = self.api.created[0][1]["metadata"]["name"]
            self.api.objects[resource(RUNTIME, "ciliumnetworkpolicies", name, network.CILIUM)]["metadata"]["resourceVersion"] = "9"
        self.api.after_create = update
        result = self.collect()
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertEqual(self.api.deleted[0][2]["preconditions"], {
            "uid": "diagnostic-policy-uid", "resourceVersion": "9"})

    def test_new_foreign_selector_consumer_aborts_after_creation_and_cleans_up(self):
        def update():
            pod = copy.deepcopy(self.api.objects[POD])
            pod["metadata"].update(name="new-foreign", uid="new-foreign-uid", ownerReferences=[])
            self.api.objects[core(RUNTIME, "pods", "new-foreign")] = pod
        self.api.after_create = update
        result = self.collect()
        self.assertEqual(result["category"], "provenance-or-operation-unavailable")
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")

    def test_mutated_policy_is_removed_but_lost_create_response_is_not_adopted(self):
        original_create = self.api.create
        def mutated(path, value):
            value = copy.deepcopy(value)
            value["spec"]["egress"] = []
            return original_create(path, value)
        with patch.object(self.api, "create", side_effect=mutated):
            result = self.collect()
        self.assertEqual(result["category"], "provenance-or-operation-unavailable")
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.setUp()
        with patch.object(self.api, "create", side_effect=OSError("private-create-canary")):
            result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(result["cleanup"], "creation-unconfirmed")
        self.assertEqual(self.api.deleted, [])
        self.assertNotIn("canary", json.dumps(result))

    def test_name_collision_never_adopts_or_deletes_existing_policy(self):
        name = "native-observer-api-0123456789abcdef"
        policy = copy.deepcopy(self.api.objects[POLICY])
        policy["metadata"].update(name=name, uid="foreign-policy")
        policy["kind"] = "CiliumNetworkPolicy"
        self.api.objects[resource(RUNTIME, "ciliumnetworkpolicies", name, network.CILIUM)] = policy
        with patch.object(network.secrets, "token_hex", return_value="0123456789abcdef"):
            result = self.collect()
        self.assertEqual(result["stage"], "pre-create-recheck")
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])
        self.assertEqual(self.api.deleted, [])

    def test_absent_audit_evidence_remains_unknown_after_bounded_probe(self):
        absent = {"available": False, "category": "no-matching-evidence",
                  "coverage": "bounded-metadata-tail", "outcomes": []}
        with patch.object(network.time, "monotonic", side_effect=[0, 0, 61]):
            result = self.collect(outcomes=absent)
        self.assertEqual(result["category"], "no-progress-observed")
        self.assertEqual(result["duringApiOutcomes"], absent)
        self.assertFalse(result["samePodObserverReady"])
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")

    def test_selector_expressions_and_unresolved_policy_peers_are_not_assumed_allow(self):
        self.assertTrue(network.matches({"matchExpressions": [
            {"key": "absent", "operator": "NotIn", "values": ["x"]},
            {"key": "present", "operator": "Exists"},
        ]}, {"present": "yes"}))
        self.assertFalse(network.matches({"matchExpressions": [
            {"key": "present", "operator": "DoesNotExist"}]}, {"present": "yes"}))
        with self.assertRaises(Failure):
            network.matches({"unknown": {}}, {})
        target = {"address": "10.96.0.1", "port": 443}
        self.assertIsNone(network.rule_match(
            {"to": [{"namespaceSelector": {}}], "ports": [{"port": 443}]}, target))
        self.assertIsNone(network.rule_match(
            {"to": [{"ipBlock": {"cidr": "10.96.0.1/32"}}], "ports": [{"port": "https"}]}, target))

    def test_probe_does_not_relabel_native_failure_or_unblock_dependent_cases(self):
        import run as native_run

        observations = MagicMock()
        observations.enable.side_effect = Failure(network.FAILURE)
        observations.observer_target = TARGET
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent) as directory:
            state = Path(directory)
            def probe(_setup, _target, failed):
                saved = json.loads((state / "evidence/native.json").read_text())
                self.assertEqual(saved["cases"]["private-bff-observer-and-fresh-privacy-rpc"]["result"], "failed")
                self.assertEqual(failed["result"], "failed")
                return {"diagnosticOnly": True, "category": "same-pod-progress-with-temporary-policy",
                        "samePodObserverReady": True, "cleanup": "uid-rv-deletion-verified"}
            with patch.dict(os.environ, ENV), patch.object(native_run, "STATE", state), \
                    patch.object(native_run, "command", return_value=network.CORE_REVISION), \
                    patch.object(native_run, "Setup"), patch.object(native_run, "install_core"), \
                    patch.object(native_run, "install_bridge"), patch.object(native_run, "bridge_connection"), \
                    patch.object(native_run, "CredentialCases"), patch.object(native_run, "LifecycleCases"), \
                    patch.object(native_run, "ObservationCases", return_value=observations), \
                    patch.object(native_run, "diagnostics", return_value={}), \
                    patch.object(native_run, "observation_diagnostics", return_value={}), \
                    patch.object(native_run, "api_outcome_diagnostics", return_value={}), \
                    patch.object(native_run, "observer_network_diagnostics", side_effect=probe), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(native_run.main(), 1)
            report = json.loads((state / "evidence/native.json").read_text())
            self.assertEqual(report["result"], "failed")
            self.assertFalse(report["runtimeQualified"])
            self.assertFalse(report["networkPolicyEnforcementQualified"])
            self.assertEqual(report["cases"]["private-bff-observer-and-fresh-privacy-rpc"]["failure"], network.FAILURE)
            for case in ("purpose-only-pinned-tls-api-negative-matrix",
                         "cni-9447-9448-positive-and-unauthorized-peer-denial",
                         "observer-rotation-current-bearer-and-revocation"):
                self.assertEqual(report["cases"][case]["result"], "blocked")


if __name__ == "__main__":
    unittest.main()
