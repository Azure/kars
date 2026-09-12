import copy
import json
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import Failure, core, resource
import observer_cilium_diagnostics as cilium
import observer_network_diagnostics as network
import test_observer_network_diagnostics as network_tests
from test_observer_network_diagnostics import (
    NetworkFixture, TARGET, RUNTIME, POD, POLICY, CEP, CILIUM_POD, redacted_context,
)

BASELINE_CNP = resource(RUNTIME, "ciliumnetworkpolicies", "existing-private-policy", cilium.CILIUM)


class CiliumObserverTests(unittest.TestCase):
    def setUp(self):
        self.api = NetworkFixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})

    def collect(self, **kwargs):
        return network_tests.ObserverNetworkTests.collect(self, **kwargs)

    def snapshot(self):
        with patch.object(network, "command", return_value=json.dumps(redacted_context())), \
                patch.object(cilium, "read_projection", side_effect=self.api.read_projection), \
                patch.object(cilium, "read_kube_proxy_projection", side_effect=self.api.read_kube_proxy_projection):
            return cilium.snapshot(self.setup, TARGET)

    def baseline_policy(self):
        self.api.objects[BASELINE_CNP] = {
            "kind": "CiliumNetworkPolicy",
            "metadata": {"name": "existing-private-policy", "namespace": RUNTIME,
                         "uid": "baseline-cnp-uid", "resourceVersion": "1",
                         "annotations": {"private": "private-annotation-canary"}},
            "spec": {"endpointSelector": {"matchLabels": {"kars.azure.com/sandbox": "agent"}},
                     "description": "private-description-canary", "ingress": []},
            "status": {"nodes": {"private-node-canary": {"error": "private-error-canary"}}},
        }

    def test_actual_configuration_identity_revision_and_policy_digests_are_allowlisted(self):
        self.baseline_policy()
        facts = self.snapshot()["facts"]
        self.assertTrue(facts["cilium"]["configurationMatchesExpected"])
        self.assertEqual(facts["cilium"]["effectiveAgentConfig"], {
            "policyCIDRMatchMode": [], "ciliumNetworkPolicyEnabled": True,
            "kubernetesNetworkPolicyEnabled": True, "kubeProxyReplacement": False})
        endpoint = facts["cilium"]["endpoints"][0]
        self.assertEqual(endpoint["podUid"], "agent-pod-uid")
        self.assertEqual(endpoint["endpointId"], 123)
        self.assertEqual(endpoint["securityIdentity"], 12345)
        self.assertEqual(endpoint["desiredPolicyRevision"], 7)
        self.assertEqual(endpoint["realizedPolicyRevision"], 7)
        self.assertEqual(endpoint["policyEnabled"], "both")
        baseline = facts["cilium"]["baselineCiliumNetworkPolicies"][0]
        self.assertEqual(baseline["identity"]["uid"], "baseline-cnp-uid")
        self.assertRegex(baseline["specDigest"], r"^[0-9a-f]{64}$")
        self.assertRegex(facts["networkPolicies"][0]["specDigest"], r"^[0-9a-f]{64}$")
        self.assertNotIn("canary", json.dumps(facts))
        self.assertTrue(facts["cilium"]["policyRevisionIsNotRuleSpecificProof"])

    def test_installed_config_contradictions_stop_without_any_policy_write(self):
        for key, value in (("policy-cidr-match-mode", "nodes"), ("enable-policy", "never"),
                           ("kube-proxy-replacement", "true"), ("policy-cidr-match-mode", "private-canary")):
            with self.subTest(key=key, value=value):
                self.setUp()
                self.api.objects[cilium.CONFIG]["data"][key] = value
                result = self.collect()
                self.assertTrue(result["available"])
                self.assertEqual(result["category"], "unexpected-cilium-configuration-no-intervention")
                self.assertFalse(result["policyCreated"])
                self.assertEqual(self.api.created, [])
                self.assertNotIn("canary", json.dumps(result))

    def test_effective_agent_contradictions_also_stop_when_configmap_looks_expected(self):
        for fields in (['["nodes"]', "true", "true"], ["[]", "false", "true"],
                       ["[]", "true", "false"]):
            with self.subTest(fields=fields):
                self.api.effective_config = fields
                result = self.collect()
                self.assertEqual(result["category"], "unexpected-cilium-configuration-no-intervention")
                self.assertEqual(self.api.created, [])
        self.assertEqual(cilium.effective_configuration(["null", "true", "true"])["policyCIDRMatchMode"], [])
        for invalid in (["", "true", "true"], ["[]", "private-canary", "true"],
                        ['["unknown-private-canary"]', "true", "true"]):
            with self.assertRaises((Failure, ValueError)):
                cilium.effective_configuration(invalid)

    def test_non_enforcing_endpoint_and_unreviewed_ports_do_not_gain_a_policy(self):
        original = self.api.read_projection
        def read(agent, endpoint_id=None, witness=None):
            fields = original(agent, endpoint_id)
            if endpoint_id is not None:
                fields[4] = "none"
            return fields
        self.api.read_projection = read
        result = self.collect()
        self.assertEqual(result["category"], "unexpected-cilium-configuration-no-intervention")
        self.assertEqual(self.api.created, [])
        before = network.snapshot(self.setup, TARGET)
        before["facts"]["destinations"].append({"address": "172.18.0.2", "port": 22})
        with self.assertRaises(Failure):
            network.policy_plan(before, "invalid-ports")

    def test_foreign_cilium_agent_lineage_account_image_or_selector_is_rejected(self):
        mutations = [
            lambda: self.api.objects[CILIUM_POD]["metadata"]["ownerReferences"][0].update(uid="foreign"),
            lambda: self.api.objects[CILIUM_POD]["metadata"].update(ownerReferences=[]),
            lambda: self.api.objects[CILIUM_POD]["spec"].update(serviceAccountName="foreign"),
            lambda: self.api.objects[CILIUM_POD]["spec"]["containers"][0].update(image="quay.io/cilium/cilium:v1.19.0"),
            lambda: self.api.objects[cilium.DAEMONSET]["spec"].update(selector={"matchLabels": {"foreign": "true"}}),
            lambda: self.api.objects[cilium.CONFIG]["metadata"].update(namespace="foreign"),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                self.setUp()
                mutate()
                self.assertFalse(self.collect()["policyCreated"])
                self.assertEqual(self.api.created, [])
        self.setUp()
        pod = copy.deepcopy(self.api.objects[CILIUM_POD])
        pod["metadata"].update(name="foreign-cilium", uid="foreign-agent", ownerReferences=[])
        self.api.objects[core("kube-system", "pods", "foreign-cilium")] = pod
        self.assertFalse(self.collect()["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_cilium_endpoint_requires_exact_pod_uid_address_and_node_binding(self):
        mutations = [
            lambda value: value["metadata"]["ownerReferences"][0].update(uid="foreign-pod"),
            lambda value: value["metadata"].update(ownerReferences=[]),
            lambda value: value["metadata"].update(name="other"),
            lambda value: value["status"]["networking"].update(node="172.18.0.3"),
            lambda value: value["status"]["networking"].update(addressing=[{"ipv4": "10.244.1.99"}]),
            lambda value: value["status"]["identity"].update(id=0),
            lambda value: value["status"].update(id=True),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                self.setUp()
                mutate(self.api.objects[CEP])
                self.assertFalse(self.collect()["policyCreated"])
                self.assertEqual(self.api.created, [])

    def test_cli_projection_endpoint_or_workload_mismatch_is_not_accepted(self):
        binding = {"endpointId": 123, "securityIdentity": 12345}
        valid = ["123", "12345", "7", "7", "both", RUNTIME, "agent-pod"]
        for index, replacement in ((0, "124"), (1, "99999"), (2, "-1"),
                                   (3, str(2**63)), (4, "unknown"), (5, "foreign"), (6, "foreign")):
            fields = valid.copy()
            fields[index] = replacement
            with self.assertRaises(Failure):
                cilium.endpoint_revision(fields, binding, self.api.objects[POD])
        self.assertEqual(cilium.endpoint_revision(valid, binding, self.api.objects[POD])["realizedPolicyRevision"], 7)

    def test_realized_revision_ahead_of_desired_is_a_bounded_observation(self):
        witness = {}
        fields = ["123", "12345", "7", "8", "both", RUNTIME, "agent-pod"]
        result = cilium.endpoint_revision(fields, {"endpointId": 123, "securityIdentity": 12345},
                                         self.api.objects[POD], witness)
        self.assertEqual(result["desiredPolicyRevision"], 7)
        self.assertEqual(result["realizedPolicyRevision"], 8)
        self.assertTrue(witness["checks"]["endpoint_revision_bounds"]["realizedAheadOfDesired"])

    def test_noop_revision_advance_alone_never_counts_as_api_or_readiness_progress(self):
        self.api.after_create = lambda: setattr(self.api, "policy_revisions", [7, 8])
        with patch.object(network.time, "monotonic", side_effect=[0, 1, 61]):
            result = self.collect(outcomes={"available": False, "category": "no-matching-evidence"})
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["category"], "no-progress-observed")
        self.assertFalse(result["apiResponseObserved"])
        self.assertFalse(result["samePodObserverReady"])
        self.assertFalse(result["cniAcceptanceQualified"])
        self.assertEqual(result["originalResult"], "failed")
        self.assertEqual(result["duringCiliumEndpoints"][0]["desiredPolicyRevision"], 7)
        self.assertEqual(result["duringCiliumEndpoints"][0]["realizedPolicyRevision"], 8)
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")

    def test_fixed_cli_only_prints_requested_fields_and_never_mutates_configuration(self):
        agent = self.api.objects[CILIUM_POD]
        with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(
                returncode=0, stdout=b"[]\ttrue\ttrue\n\n")) as run:
            self.assertEqual(cilium.read_projection(agent), ["[]", "true", "true"])
        args = run.call_args.args[0]
        self.assertEqual(args[:9], ["kubectl", "--context", "kind-bridge-native", "--request-timeout=10s",
                                   "exec", "-n", "kube-system", "cilium-worker", "-c"])
        self.assertEqual(args[9:], ["cilium-agent", "--", "cilium-dbg", "config", "--read-only", "--output",
                                   cilium.CONFIG_OUTPUT])
        self.assertEqual(run.call_args.kwargs["stderr"], subprocess.DEVNULL)
        self.assertEqual(run.call_args.kwargs["timeout"], 12)
        with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(
                returncode=0, stdout=b"123\t12345\t7\t7\tboth\tkars-agent\tagent-pod\n")) as run:
            cilium.read_projection(agent, 123)
        self.assertEqual(run.call_args.args[0][12:], ["endpoint", "get", "123", "--output", cilium.ENDPOINT_OUTPUT])
        for projection in (cilium.CONFIG_OUTPUT, cilium.KUBE_PROXY_OUTPUT, cilium.ENDPOINT_OUTPUT):
            self.assertTrue(projection.startswith("jsonpath="))
            self.assertNotIn(".log", projection)
            self.assertNotIn(".labels", projection)
            self.assertNotIn("token", projection)

    def test_projection_bounds_and_unknown_outputs_fail_closed(self):
        agent = self.api.objects[CILIUM_POD]
        for status, raw in ((1, b"private-canary"), (0, b"x" * 4097), (0, b"one\ntwo"), (0, b"")):
            with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(returncode=status, stdout=raw)):
                with self.assertRaises(Failure):
                    cilium.read_projection(agent)
        for endpoint in ("123; private-command", 0, True, 65536):
            with patch.object(cilium.subprocess, "run") as run:
                with self.assertRaises(Failure):
                    cilium.read_projection(agent, endpoint)
                run.assert_not_called()

    def test_policy_revision_and_cep_status_updates_are_observations_not_identity_changes(self):
        def update():
            self.api.policy_revisions = [8, 8]
            self.api.objects[CEP]["metadata"]["resourceVersion"] = "2"
            self.api.objects[CEP]["status"]["log"] = ["private-after-log-canary"]
        self.api.after_create = update
        result = self.collect()
        self.assertEqual(result["category"], "same-pod-progress-with-temporary-policy")
        self.assertTrue(result["apiResponseObserved"])
        self.assertFalse(result["samePodObserverReady"])
        self.assertEqual(result["duringCiliumEndpoints"][0]["realizedPolicyRevision"], 8)
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertNotIn("canary", json.dumps(result))

    def test_ready_transition_during_cilium_reads_is_not_lost_or_attributed_to_a_policy(self):
        original = self.api.read_projection
        def read(agent, endpoint_id=None, witness=None):
            self.api.source["metadata"]["resourceVersion"] = "2"
            self.api.source["status"]["serviceObservation"].update(phase="Ready", reason="Verified")
            return original(agent, endpoint_id)
        self.api.read_projection = read
        result = self.collect()
        self.assertTrue(result["samePodObserverReady"])
        self.assertEqual(result["category"], "already-ready-without-intervention")
        self.assertEqual(self.api.created, [])

    def test_cilium_identity_or_config_changes_during_probe_abort_and_clean_only_owned_cnp(self):
        mutations = [
            lambda: self.api.objects[CEP]["metadata"].update(uid="replacement-cep"),
            lambda: self.api.objects[CEP]["status"]["identity"].update(id=999),
            lambda: self.api.objects[cilium.CONFIG]["metadata"].update(resourceVersion="2"),
            lambda: self.api.objects[CILIUM_POD]["metadata"].update(uid="replacement-agent"),
            lambda: self.api.objects[CILIUM_POD]["status"]["containerStatuses"][0].update(restartCount=1),
            lambda: self.api.objects[cilium.DAEMONSET]["metadata"].update(uid="replacement-daemonset"),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                self.setUp()
                self.api.after_create = mutate
                result = self.collect()
                self.assertEqual(result["category"], "provenance-or-operation-unavailable")
                self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
                self.assertEqual(len(self.api.deleted), 1)
                self.assertIn("/ciliumnetworkpolicies/", self.api.deleted[0][1])

    def test_post_create_stability_failure_retains_only_fixed_comparison_facts(self):
        self.api.after_create = lambda: self.api.objects[cilium.CONFIG]["metadata"].update(resourceVersion="2")
        result = self.collect()
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["originalResult"], "failed")
        self.assertEqual(result["category"], "provenance-or-operation-unavailable")
        self.assertEqual(result["observationStoppingStage"], "before-api-stability")
        self.assertTrue(result["observationSnapshot"]["complete"])
        self.assertFalse(result["stableChecks"]["cilium"])
        self.assertFalse(result["ciliumStableChecks"]["anchors"])
        self.assertNotIn("duringApiOutcomes", result)
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertFalse(result["cniAcceptanceQualified"])

    def test_post_create_projection_failure_keeps_current_witness_not_stale_stability(self):
        original = self.api.read_projection

        def read(agent, endpoint_id=None, witness=None):
            if self.api.created and endpoint_id is not None:
                cilium.checkpoint(witness, "endpoint_projection_fields", numericFieldsValid=False)
                raise Failure("private-post-create-error-canary")
            return original(agent, endpoint_id, witness)

        self.api.read_projection = read
        result = self.collect()
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["observationStoppingStage"], "before-api-snapshot")
        self.assertFalse(result["observationSnapshot"]["complete"])
        self.assertEqual(result["observationSnapshot"]["lastStage"], "endpoint_projection_fields")
        self.assertEqual(result["observationSnapshot"]["failureKind"], "constraint")
        self.assertNotIn("stableChecks", result)
        self.assertNotIn("ciliumStableChecks", result)
        self.assertNotIn("duringApiOutcomes", result)
        self.assertNotIn("canary", json.dumps(result))
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertEqual(result["originalResult"], "failed")
        self.assertFalse(result["cniAcceptanceQualified"])

    def test_existing_cnp_spec_or_identity_changes_are_fatal_but_status_updates_are_not(self):
        for change in ("spec", "uid", "status"):
            self.setUp()
            self.baseline_policy()
            def update():
                value = self.api.objects[BASELINE_CNP]
                value["metadata"]["resourceVersion"] = "2"
                if change == "spec":
                    value["spec"]["ingress"] = [{}]
                elif change == "uid":
                    value["metadata"]["uid"] = "foreign"
                else:
                    value["status"] = {"private-node": {"private": "private-status-canary"}}
            self.api.after_create = update
            result = self.collect()
            expected = "same-pod-progress-with-temporary-policy" if change == "status" else "provenance-or-operation-unavailable"
            self.assertEqual(result["category"], expected)
            self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
            self.assertIn(BASELINE_CNP, self.api.objects)
            self.assertNotIn(BASELINE_CNP, [entry[1] for entry in self.api.deleted])

    def test_knp_with_same_name_is_never_omitted_or_deleted_and_no_ip_policy_is_created(self):
        name = "native-observer-api-0123456789abcdef"
        value = copy.deepcopy(self.api.objects[POLICY])
        value["metadata"].update(name=name, uid="existing-same-name-knp")
        path = resource(RUNTIME, "networkpolicies", name, network.NETWORK)
        self.api.objects[path] = value
        with patch.object(network.secrets, "token_hex", return_value="0123456789abcdef"):
            result = self.collect()
        self.assertTrue(result["policyCreated"])
        self.assertEqual(len(result["before"]["networkPolicies"]), 2)
        self.assertEqual(self.api.objects[path], value)
        self.assertEqual(self.api.created[0][1]["kind"], "CiliumNetworkPolicy")
        self.assertNotIn("ipBlock", json.dumps(self.api.created[0][1]))
        self.assertNotIn("policyCIDRMatchMode", json.dumps(self.api.created[0][1]))
        self.assertNotIn("toServices", json.dumps(self.api.created[0][1]))


if __name__ == "__main__":
    unittest.main()
