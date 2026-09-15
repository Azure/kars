# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Read-only producer parity for the BFF's shared host-matching fixture.

Called by the Rust contract regression, so existing BFF test registration runs
both consumers without editing the separately owned producer or CI files.
"""
import importlib.util
import json
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
CONTRACT = json.loads((HERE / "datapath_matching_contract.json").read_text())
SOURCE = HERE.parents[4] / "deploy/ebpf-witness/aggregator/witness.py"
SPEC = importlib.util.spec_from_file_location("witness_matching_producer", SOURCE)
PRODUCER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRODUCER)


class DeclarationApi:
    def __init__(self, mode, hosts):
        self.mode, self.hosts = mode, hosts

    def request(self, path):
        if path == "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/demo":
            return {
                "metadata": {"name": "demo", "namespace": "kars-system"},
                "spec": {"networkPolicy": {"egressMode": self.mode}},
            }
        if path == "/api/v1/namespaces/kars-demo/configmaps/karssandbox-demo-egress-allowlist":
            return {
                "metadata": {"name": "karssandbox-demo-egress-allowlist", "namespace": "kars-demo"},
                "data": {"allowlist.json": json.dumps({
                    "schemaVersion": 1, "endpoints": [{"host": host} for host in self.hosts],
                })},
            }
        raise AssertionError("unexpected producer read")


class MatchingContractTests(unittest.TestCase):
    def test_matcher_language_is_exactly_the_producers(self):
        for case in CONTRACT["matching"]:
            with self.subTest(case=case["name"]):
                self.assertEqual(
                    PRODUCER.declared_host(case["host"], case["declared_hosts"]),
                    case["matches"],
                )

    def test_report_sets_and_verdicts_come_from_actual_producer(self):
        for case in CONTRACT["reports"]:
            with self.subTest(case=case["name"]):
                expected = case["sandbox"]
                records = PRODUCER.declarations(
                    DeclarationApi(expected["egress_mode"], expected["declared_hosts"]), ["demo"]
                )
                dns = [
                    {"k8s": {"namespace": "kars-demo", "node": "node-1"}, "name": host, "qr": "Q"}
                    for host in expected["observed_dns"]
                ]
                tcp = [
                    {"k8s": {"namespace": "kars-demo", "node": "node-1"},
                     "type": "connect", "dst": {"addr": "8.8.8.8"}}
                    for _ in range(expected["observed_connects"])
                ]
                actual, _, _ = PRODUCER.compute(records, dns, tcp, ["node-1"])
                self.assertEqual(actual, [expected])

    def test_open_and_mixed_case_modes_remain_rejected_by_declarations(self):
        for mode in CONTRACT["rejected_modes"]:
            with self.subTest(mode=mode):
                with self.assertRaises(PRODUCER.WitnessError):
                    PRODUCER.declarations(DeclarationApi(mode, []), ["demo"])


if __name__ == "__main__":
    unittest.main()
