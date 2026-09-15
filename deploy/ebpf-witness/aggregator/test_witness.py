# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
import copy
import io
import json
import os
from pathlib import Path
import struct
import runpy
import tempfile
import unittest
from unittest.mock import patch

import witness


def metadata(name, namespace="kars-system"):
    return {"name": name, "namespace": namespace, "uid": f"{name}-uid", "resourceVersion": "1",
            "generation": 1,
            "labels": {"kars.azure.com/witness-addon": "true", "app.kubernetes.io/managed-by": "Helm"},
            "annotations": {"meta.helm.sh/release-name": witness.RELEASE,
                            "meta.helm.sh/release-namespace": "kars-system"}}


class Api:
    def __init__(self):
        self.calls = []
        self.cm = {"apiVersion": "v1", "kind": "ConfigMap",
                   "metadata": metadata(witness.RELEASE), "data": {}}
        self.ds = {"metadata": metadata("gadget", witness.NAMESPACE),
                   "status": {"desiredNumberScheduled": 1, "numberReady": 1,
                              "updatedNumberScheduled": 1, "observedGeneration": 1}}
        self.pods = {"items": [{
            "metadata": dict(metadata("gadget-pod", witness.NAMESPACE),
                             ownerReferences=[{"uid": "gadget-uid", "controller": True}]),
            "spec": {"nodeName": "node-1"},
            "status": {"conditions": [{"type": "Ready", "status": "True"}]},
        }]}

    def request(self, path, method="GET", body=None):
        self.calls.append((path, method, copy.deepcopy(body)))
        if path == witness.CM_PATH:
            if method == "PUT":
                if body["metadata"]["resourceVersion"] != self.cm["metadata"]["resourceVersion"]:
                    raise witness.WitnessError("api_http_409")
                self.cm = copy.deepcopy(body)
            return copy.deepcopy(self.cm)
        assert method == "GET"
        if path.endswith("/daemonsets/gadget"):
            return copy.deepcopy(self.ds)
        if "/pods?" in path:
            return copy.deepcopy(self.pods)
        if path.endswith("/karssandboxes/demo"):
            return {"metadata": metadata("demo"), "spec": {"networkPolicy": {"egressMode": "Strict"}}}
        if path.endswith("/configmaps/karssandbox-demo-egress-allowlist"):
            return {"metadata": metadata("karssandbox-demo-egress-allowlist", "kars-demo"),
                    "data": {"allowlist.json": json.dumps({"schemaVersion": 1, "endpoints": []})}}
        raise AssertionError(f"unscoped API access: {method} {path}")


def record(mode="Strict", hosts=None):
    return {"namespace": "kars-demo", "sandbox": "demo", "egress_mode": mode,
            "declared_hosts": hosts or []}


def dns(name="example.com", namespace="kars-demo"):
    return {"k8s": {"namespace": namespace, "node": "node-1"}, "name": name, "qr": "Q"}


def tcp(kind="connect", address="8.8.8.8"):
    return {"k8s": {"namespace": "kars-demo", "node": "node-1"},
            "type": kind, "dst": {"addr": address}}


class ComputationTests(unittest.TestCase):
    def test_strict_empty_is_deny_all_not_learn_or_compliant(self):
        rows, count, nodes = witness.compute([record()], [dns()], [tcp()], ["node-1"])
        self.assertEqual(rows[0]["verdict"], "BEYOND-DECLARED")
        self.assertEqual(rows[0]["observed_connects"], 1)
        self.assertEqual(count, 2)
        self.assertEqual(nodes, ["node-1"])
        rows, count, _ = witness.compute([record()], [], [], ["node-1"])
        self.assertEqual(rows[0]["verdict"], "NO-TRAFFIC")
        self.assertEqual(count, 0)

    def test_learn_wildcard_and_tcp_event_types_are_not_overclaimed(self):
        rows, _, _ = witness.compute([record("Learn")], [dns()], [], ["node-1"])
        self.assertEqual(rows[0]["verdict"], "LEARN")
        rows, _, _ = witness.compute([record(hosts=["*.example.com"])],
                                    [dns("api.example.com")],
                                    [tcp("close"), tcp("accept"), tcp(address="10.0.0.1")], ["node-1"])
        self.assertEqual(rows[0]["verdict"], "NO-BEYOND-OBSERVED")
        self.assertEqual(rows[0]["observed_connects"], 0)
        self.assertFalse(witness.declared_host("notexample.com", ["*.example.com"]))
        self.assertFalse(witness.declared_host("example.com", ["*.example.com"]))

    def test_internal_search_expansion_and_out_of_scope_events(self):
        self.assertTrue(witness.internal_host("svc.ns.svc.cluster.local.cloudapp.net"))
        self.assertFalse(witness.internal_host("external.cloudapp.net"))
        rows, count, nodes = witness.compute([record()], [dns(namespace="other")], [], ["node-1"])
        self.assertEqual(count, 0)
        self.assertEqual(nodes, [])
        self.assertEqual(rows[0]["observed_dns"], [])

    def test_malformed_or_unattributed_events_never_become_empty_success(self):
        for raw in [b"not json", b"{}\nnot json", b"[null]", b"\xff"]:
            with self.assertRaises(witness.WitnessError):
                witness.parse_events(raw)
        self.assertEqual(witness.parse_events(b" \n"), [])
        for event in [{}, {"k8s": {}}, dict(dns(), qr="unknown"),
                      dict(dns(), k8s={"namespace": "kars-demo", "node": "other"})]:
            with self.assertRaises(witness.WitnessError):
                witness.compute([record()], [event], [], ["node-1"])
        with self.assertRaises(witness.WitnessError):
            witness.compute([record()], [], [tcp(address=123)], ["node-1"])


class RuntimeTests(unittest.TestCase):
    def test_reader_scope_and_readiness_identity(self):
        api = Api()
        self.assertEqual(witness.declarations(api, ["demo"]), [record()])
        self.assertEqual(witness.ready_nodes(api), {"node-1": "gadget-pod-uid"})
        self.assertTrue(all(method == "GET" for _, method, _ in api.calls))
        self.assertFalse(any("/secrets" in path or "configmaps?" in path for path, _, _ in api.calls))
        for field, value in [("numberReady", 0), ("desiredNumberScheduled", 0), ("observedGeneration", 0)]:
            api = Api()
            api.ds["status"][field] = value
            with self.assertRaises(witness.WitnessError):
                witness.ready_nodes(api)
        api = Api()
        api.pods["items"][0]["metadata"]["ownerReferences"][0]["uid"] = "foreign"
        with self.assertRaises(witness.WitnessError):
            witness.ready_nodes(api)

    def test_publisher_one_object_identity_concurrency_and_no_creation(self):
        api = Api()
        publisher = witness.Publisher(api)
        publisher.publish({"status": "empty"})
        publisher.publish({"status": "empty"})
        self.assertEqual([method for _, method, _ in api.calls], ["GET", "PUT", "GET", "PUT"])
        self.assertEqual(json.loads(api.cm["data"]["witness.json"])["publisher_uid"], f"{witness.RELEASE}-uid")
        api.cm["metadata"]["uid"] = "replaced"
        with self.assertRaisesRegex(witness.WitnessError, "uid_conflict"):
            publisher.publish({"status": "empty"})
        self.assertEqual(api.calls[-1][1], "GET")
        api = Api()
        api.cm["metadata"]["annotations"] = {}
        with self.assertRaises(witness.WitnessError):
            witness.Publisher(api).publish({})
        self.assertEqual(len(api.calls), 1)
        api = Api()
        request = api.request
        def conflict(path, method="GET", body=None):
            if method == "PUT":
                raise witness.WitnessError("api_http_409")
            return request(path, method, body)
        with patch.object(api, "request", side_effect=conflict):
            with self.assertRaisesRegex(witness.WitnessError, "409"):
                witness.Publisher(api).publish({})

    def test_invalid_or_missing_baselines_do_not_default_to_learning(self):
        for value in ["not-json", "{}", '{"schemaVersion":1,"endpoints":[{"host":null}]}',
                      '{"schemaVersion":true,"endpoints":[]}',
                      '{"schemaVersion":1,"endpoints":[{"host":" "}]}']:
            api = Api()
            request = api.request
            def invalid(path, method="GET", body=None):
                result = request(path, method, body)
                if path.endswith("egress-allowlist"):
                    result["data"]["allowlist.json"] = value
                return result
            with patch.object(api, "request", side_effect=invalid):
                with self.assertRaisesRegex(witness.WitnessError, "declaration_invalid"):
                    witness.declarations(api, ["demo"])
        with patch.object(Api, "request", side_effect=witness.WitnessError("api_http_404")):
            with self.assertRaisesRegex(witness.WitnessError, "404"):
                witness.declarations(Api(), ["demo"])

    def test_changing_node_set_invalidates_the_entire_window(self):
        config = {"window_seconds": 15, "sandboxes": ["demo"], "dns_image": "dns", "tcp_image": "tcp"}
        with patch.object(witness, "capture", return_value=[]), patch.object(
            witness, "ready_nodes", side_effect=[{"node-1": "old-pod"}, {"node-1": "new-pod"}]
        ):
            with self.assertRaisesRegex(witness.WitnessError, "nodes_changed"):
                witness.sample(Api(), config)

    def test_failed_capture_or_api_is_explicit_and_not_ready(self):
        config = {"window_seconds": 15}
        with tempfile.TemporaryDirectory() as directory, patch.object(witness, "HEALTH", Path(directory) / "health"):
            api = Api()
            with patch.object(witness, "sample", side_effect=witness.WitnessError("capture_failed")):
                report = witness.run_cycle(api, witness.Publisher(api), config, 1, "a" * 64)
            self.assertEqual(report["status"], "unavailable")
            self.assertEqual(report["sandboxes"], [])
            self.assertFalse(witness.HEALTH.exists())
            with patch.object(api, "request", side_effect=witness.WitnessError("api_http_403")):
                with self.assertRaisesRegex(witness.WitnessError, "403"):
                    witness.run_cycle(api, witness.Publisher(api), config, 1, "a" * 64)
            self.assertFalse(witness.HEALTH.exists())

    def test_complete_empty_commands_publish_empty_not_coverage(self):
        api = Api()
        config = {"window_seconds": 15, "sandboxes": ["demo"], "dns_image": "dns", "tcp_image": "tcp"}
        with tempfile.TemporaryDirectory() as directory, patch.object(witness, "HEALTH", Path(directory) / "health"):
            with patch.object(witness, "capture", return_value=[]):
                result = witness.run_cycle(api, witness.Publisher(api), config, 1, "a" * 64)
            self.assertEqual(result["status"], "empty")
            self.assertEqual(result["coverage"], "partial")
            self.assertTrue(witness.HEALTH.exists())

    def test_host_btf_is_actually_read_and_validated(self):
        with tempfile.TemporaryDirectory() as directory:
            btf = Path(directory) / "vmlinux"
            for raw in [b"", b"not a BTF file", b"\x00" * 24]:
                btf.write_bytes(raw)
                with self.assertRaises(witness.WitnessError):
                    witness.check_btf(btf)
            btf.write_bytes(struct.pack("<HBBIIIII", 0xEB9F, 1, 0, 24, 0, 10, 10, 10))
            witness.check_btf(btf)

    def test_real_subprocess_exit_stderr_json_limits_and_fixed_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "kubectl-gadget"
            executable.write_text(
                "#!/usr/bin/env python3\nimport json,os,sys,time\n"
                "assert '--gadget-namespace' in sys.argv and 'kars-witness-gadget' in sys.argv\n"
                "assert '--node' in sys.argv and '--detach' not in sys.argv\n"
                "mode=os.environ['FAKE_MODE']\n"
                "if mode=='timeout': time.sleep(10)\n"
                "if mode=='error': sys.exit(2)\n"
                "if mode=='warn': print('level=warning messages dropped',file=sys.stderr)\n"
                "print('bad json' if mode=='json' else ('x'*1024 if mode=='limit' else json.dumps({})))\n"
            )
            executable.chmod(0o755)
            with patch.dict(os.environ, {"PATH": directory + os.pathsep + os.environ["PATH"]}):
                for mode in ["error", "warn", "json", "limit"]:
                    with patch.dict(os.environ, {"FAKE_MODE": mode}), patch.object(witness, "MAX_OUTPUT", 128):
                        with self.assertRaises(witness.WitnessError):
                            witness.capture("verified-image", 0, ["node-1"])
                with patch.dict(os.environ, {"FAKE_MODE": "ok"}):
                    self.assertEqual(witness.capture("verified-image", 0, ["node-1"]), [{}])
                    with self.assertRaisesRegex(witness.WitnessError, "ended_early"):
                        witness.capture("verified-image", 5, ["node-1"])
                with patch.dict(os.environ, {"FAKE_MODE": "timeout"}), patch.object(
                    witness.time, "monotonic", side_effect=[0, 46]
                ):
                    with self.assertRaisesRegex(witness.WitnessError, "capture_timeout"):
                        witness.capture("verified-image", 0, ["node-1"])

    def test_image_build_requires_provenance_and_verifies_client_before_extracting(self):
        script = Path(__file__).with_name("fetch-client.py")
        environment = {"TARGETARCH": "amd64", "SOURCE_REVISION": "a" * 40,
                       "PYTHON_BASE": "python@sha256:" + "b" * 64}
        with patch.dict(os.environ, environment), patch("urllib.request.urlopen", return_value=io.BytesIO(b"untrusted archive")):
            with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                runpy.run_path(str(script))
        with patch.dict(os.environ, dict(environment, PYTHON_BASE="python:mutable")), patch(
            "urllib.request.urlopen", side_effect=AssertionError("must fail before network")
        ):
            with self.assertRaisesRegex(SystemExit, "reviewed image digest"):
                runpy.run_path(str(script))


if __name__ == "__main__":
    unittest.main()
