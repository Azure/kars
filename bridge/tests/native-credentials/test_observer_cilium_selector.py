# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Effective identity-label selection; the original Pod rollout fences remain."""

import copy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import Failure, core, resource
import observer_cilium_diagnostics as cilium
import observer_network_diagnostics as network
import test_observer_network_diagnostics as fixtures

# Cilium 1.18.5 explicitly excludes !pod-template-hash from identity labels.
# https://github.com/cilium/cilium/blob/v1.18.5/pkg/labelsfilter/filter.go
# CEP EndpointIdentity.Labels and agent models.Identity.Labels use source:key=value.
# https://github.com/cilium/cilium/blob/v1.18.5/pkg/k8s/apis/cilium.io/v2/types.go
# https://github.com/cilium/cilium/blob/v1.18.5/api/v1/models/identity.go
SELECTOR = {"matchLabels": {
    "k8s:kars.azure.com/sandbox": "agent",
    "k8s:io.kubernetes.pod.namespace": fixtures.RUNTIME,
}}


class CiliumSelectorTests(unittest.TestCase):
    def setUp(self):
        self.api = fixtures.NetworkFixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})

    def collect(self):
        return fixtures.ObserverNetworkTests.collect(self)

    def snapshot(self):
        with patch.object(network, "command", return_value=json.dumps(fixtures.redacted_context())), \
                patch.object(cilium, "read_projection", side_effect=self.api.read_projection), \
                patch.object(cilium, "read_kube_proxy_projection", side_effect=self.api.read_kube_proxy_projection):
            return cilium.snapshot(self.setup, fixtures.TARGET)

    def test_hash_exclusion_does_not_prevent_proven_identity_selector(self):
        before = self.snapshot()
        plan = network.policy_plan(before, "diagnostic")
        self.assertEqual(plan["spec"]["endpointSelector"], SELECTOR)
        self.assertEqual(before["selector"]["matchLabels"]["pod-template-hash"], "abc123")
        self.assertEqual(before["facts"]["pods"][0]["selector"], before["selector"])
        facts = before["facts"]["cilium"]
        self.assertTrue(facts["selectorIdentityLabelsVerified"])
        self.assertTrue(facts["matchingEndpointInventoryVerified"])
        self.assertTrue(facts["podTemplateHashUsedOnlyForPodProvenance"])
        labels = cilium.identity_labels(self.api.identity_labels)
        self.assertNotIn("k8s:pod-template-hash", labels)
        self.assertTrue(network.matches(SELECTOR, labels))
        self.assertFalse(network.matches({"matchLabels": {
            **SELECTOR["matchLabels"], "k8s:pod-template-hash": "abc123"}}, labels))
        self.assertNotIn("canary", json.dumps(before["facts"]))

    def test_missing_or_excluded_required_identity_labels_never_authorize_policy(self):
        for index in (0, 1):
            for source in ("cep", "agent"):
                self.setUp()
                labels = self.api.objects[fixtures.CEP]["status"]["identity"]["labels"] if source == "cep" else self.api.identity_labels
                labels.pop(index)
                result = self.collect()
                self.assertFalse(result["policyCreated"])
                self.assertEqual(self.api.created, [])
                self.assertNotIn("canary", json.dumps(result))
        self.setUp()
        self.api.identity_labels = ["k8s:pod-template-hash=abc123"]
        self.assertFalse(self.collect()["policyCreated"])

    def test_selector_cannot_be_replaced_with_excluded_or_unproven_labels(self):
        before = self.snapshot()
        for replacement in (
            {"matchLabels": {}},
            {"matchLabels": {**SELECTOR["matchLabels"], "pod-template-hash": "abc123"}},
            {"matchLabels": {**SELECTOR["matchLabels"], "k8s:pod-template-hash": "abc123"}},
            {"matchLabels": {"kars.azure.com/sandbox": "agent"}},
            {"matchLabels": {"container:kars.azure.com/sandbox": "agent"}},
        ):
            value = copy.deepcopy(before)
            value["facts"]["cilium"]["endpointSelector"] = replacement
            value["stable"]["cilium"]["endpointSelector"] = replacement
            with self.assertRaises(Failure):
                network.policy_plan(value, "diagnostic")

    def test_identity_source_value_duplicates_and_partial_shapes_fail_closed(self):
        for replacement in (
            None, [], [True], {}, ["k8s:kars.azure.com/sandbox=agent"] * 2,
            ["k8s:kars.azure.com/sandbox=agent", "k8s:kars.azure.com/sandbox=foreign"],
            ["container:kars.azure.com/sandbox=agent", "k8s:io.kubernetes.pod.namespace=" + fixtures.RUNTIME],
            ["k8s:kars.azure.com/sandbox=foreign", "k8s:io.kubernetes.pod.namespace=" + fixtures.RUNTIME],
            ["private-label-canary"], ["k8s:private=" + "x" * 513], ["k8s:private=secret\ncanary"],
        ):
            self.setUp()
            self.api.objects[fixtures.CEP]["status"]["identity"]["labels"] = replacement
            result = self.collect()
            self.assertFalse(result["policyCreated"])
            self.assertEqual(self.api.created, [])
            self.assertNotIn("canary", json.dumps(result))

    def test_agent_projection_must_match_cep_identity_not_only_numeric_id(self):
        self.api.identity_labels.append("k8s:private=another-value")
        result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])
        self.setUp()
        self.api.identity_labels.append("k8s:extra=private-mismatch-canary")
        result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(result["baselineStoppingStage"], "endpoint_label_projection")
        self.assertNotIn("canary", json.dumps(result))

    def extra_pod(self, change):
        pod = copy.deepcopy(self.api.objects[fixtures.POD])
        pod["metadata"].update(name="old-pod", uid="old-pod-uid")
        pod["metadata"]["labels"]["pod-template-hash"] = "old123"
        change(pod)
        self.api.objects[core(fixtures.RUNTIME, "pods", "old-pod")] = pod

    def test_old_rollout_foreign_and_unrepresented_pods_fail_before_create(self):
        for change in (
            lambda pod: pod["metadata"]["annotations"].update({fixtures.VERSION: "old"}),
            lambda pod: pod["metadata"].update(ownerReferences=[]),
            lambda pod: pod["spec"].update(serviceAccountName="foreign"),
            lambda pod: pod["metadata"].update(deletionTimestamp="terminating"),
            lambda pod: pod["metadata"]["labels"].pop("pod-template-hash"),
        ):
            self.setUp()
            self.extra_pod(change)
            result = self.collect()
            self.assertFalse(result["policyCreated"])
            self.assertEqual(self.api.created, [])

    def test_extra_matching_cep_is_rejected_even_without_a_live_pod(self):
        endpoint = copy.deepcopy(self.api.objects[fixtures.CEP])
        endpoint["metadata"].update(name="orphan", uid="orphan-cep")
        endpoint["metadata"]["ownerReferences"][0].update(name="orphan", uid="orphan-pod")
        endpoint["status"]["id"] = 456
        self.api.objects[resource(fixtures.RUNTIME, "ciliumendpoints", "orphan", cilium.CILIUM)] = endpoint
        result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_missing_duplicate_unknown_and_replaced_cep_inventory_fail_closed(self):
        original_get = self.api.get
        path = resource(fixtures.RUNTIME, "ciliumendpoints", group=cilium.CILIUM)
        endpoint = copy.deepcopy(self.api.objects[fixtures.CEP])
        for values in ([], [endpoint, endpoint], [dict(endpoint, status={})]):
            with patch.object(self.api, "get", side_effect=lambda requested:
                    {"items": values} if requested == path else original_get(requested)):
                self.assertFalse(self.collect()["policyCreated"])
        endpoint["metadata"]["uid"] = "replacement"
        with patch.object(self.api, "get", side_effect=lambda requested:
                {"items": [endpoint]} if requested == path else original_get(requested)):
            self.assertFalse(self.collect()["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_inventory_race_during_snapshot_is_rejected(self):
        original_get = self.api.get
        path = resource(fixtures.RUNTIME, "ciliumendpoints", group=cilium.CILIUM)
        reads = 0
        def racing_get(requested):
            nonlocal reads
            if requested == path:
                reads += 1
                if reads == 2:
                    return {"items": []}
            return original_get(requested)
        with patch.object(self.api, "get", side_effect=racing_get):
            result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_paginated_pod_or_cep_inventory_never_authorizes_policy(self):
        original_get = self.api.get
        for path in (core(fixtures.RUNTIME, "pods"),
                     resource(fixtures.RUNTIME, "ciliumendpoints", group=cilium.CILIUM)):
            for metadata in ({"continue": "private-continuation-canary"}, {"remainingItemCount": 1},
                             {"remainingItemCount": False}, []):
                def partial_get(requested):
                    value = original_get(requested)
                    if requested == path:
                        value["metadata"] = metadata
                    return value
                with patch.object(self.api, "get", side_effect=partial_get):
                    result = self.collect()
                self.assertFalse(result["policyCreated"])
                self.assertNotIn("canary", json.dumps(result))
        self.assertEqual(self.api.created, [])

    def test_new_old_rollout_during_intervention_invalidates_and_cleans_policy(self):
        self.api.after_create = lambda: self.extra_pod(
            lambda pod: pod["metadata"].update(ownerReferences=[]))
        result = self.collect()
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.assertEqual(result["originalResult"], "failed")
        self.assertFalse(result["cniAcceptanceQualified"])
        self.assertFalse(result["policyWindowPackets"]["provenanceUnchanged"])


if __name__ == "__main__":
    unittest.main()
