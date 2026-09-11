"""Tagged status-field controls; no live Cilium or Kubernetes qualification."""

import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import Failure
import observer_cilium_diagnostics as cilium
import test_observer_network_diagnostics as fixtures

# v1.18.5 StatusResponse.kube-proxy-replacement is an object, and Mode is a
# case-sensitive string enum, not a DaemonConfigurationMap boolean.
STATUS_FALSE = {"kube-proxy-replacement": {"mode": "False", "deviceList": [], "devices": []},
                "cilium": {"state": "Ok", "msg": "private-status-canary"}}


class CiliumStatusSchemaTests(unittest.TestCase):
    def setUp(self):
        self.api = fixtures.NetworkFixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})

    def collect(self):
        return fixtures.ObserverNetworkTests.collect(self)

    def test_typed_false_completes_existing_fences_and_true_is_a_config_contradiction(self):
        result = self.collect()
        self.assertTrue(result["baselineSnapshot"]["complete"])
        self.assertFalse(result["baselineSnapshot"]["observedEffectiveConfig"]["kubeProxyReplacement"])
        self.assertEqual(result["baselineSnapshot"]["checks"]["kube_proxy_fields"]["kubeProxySyntax"], "status-false")
        self.assertTrue(result["policyCreated"])
        self.assertEqual(result["cleanup"], "uid-rv-deletion-verified")
        self.setUp()
        self.api.kube_proxy_status = ["True"]
        result = self.collect()
        self.assertEqual(result["category"], "unexpected-cilium-configuration-no-intervention")
        self.assertTrue(result["baselineSnapshot"]["observedEffectiveConfig"]["kubeProxyReplacement"])
        self.assertFalse(result["policyCreated"])
        self.assertEqual(self.api.created, [])

    def test_missing_unknown_lowercase_and_wrong_shape_status_never_default_false(self):
        for fields in ([], [""], ["false"], ["true"], ["FALSE"], ["Disabled"], ["<nil>"], ["null"],
                       ["0"], ['"False"'], ["False", "extra"], [False], [None], ["private-mode-canary"]):
            with self.subTest(fields=type(fields[0]).__name__ if fields else "missing"):
                self.setUp()
                self.api.kube_proxy_status = fields
                result = self.collect()
                self.assertFalse(result["available"])
                self.assertFalse(result["policyCreated"])
                self.assertEqual(result["baselineStoppingStage"], "kube_proxy_fields")
                witness = result["baselineSnapshot"]
                self.assertFalse(witness["complete"])
                self.assertTrue(witness["networkValidated"])
                self.assertNotIn("kubeProxyReplacement", witness["observedEffectiveConfig"])
                self.assertNotIn("canary", json.dumps(result))
                self.assertEqual(self.api.created, [])

    def test_status_cli_uses_the_typed_object_path_and_preserves_failure_bounds(self):
        agent = self.api.objects[fixtures.CILIUM_POD]
        with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(
                returncode=0, stdout=b"False\n\n")) as run:
            fields = cilium.read_kube_proxy_projection(agent)
        self.assertEqual(fields, ["False"])
        self.assertFalse(cilium.kube_proxy_mode(fields))
        args = run.call_args.args[0]
        self.assertEqual(args[11:], ["cilium-dbg", "status", "--timeout=10s", "--output",
                                     'jsonpath={.kube-proxy-replacement.mode}{"\\n"}'])
        self.assertNotIn("--require-k8s-connectivity=false", args)
        self.assertEqual(run.call_args.kwargs["stderr"], subprocess.DEVNULL)
        self.assertEqual(run.call_args.kwargs["timeout"], 12)
        self.assertNotIn("KubeProxyReplacement", cilium.CONFIG_OUTPUT)

    def test_status_cli_errors_timeouts_and_empty_output_do_not_authorize_intervention(self):
        original = cilium.read_kube_proxy_projection
        for code, raw, stage in ((1, b"False\n", "kube_proxy_exec"),
                                 (0, b"", "kube_proxy_framing"),
                                 (0, b"False\nprivate-extra-canary\n", "kube_proxy_framing"),
                                 (0, b"private-mode-canary\n", "kube_proxy_fields")):
            self.setUp()
            self.api.read_kube_proxy_projection = original
            with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(returncode=code, stdout=raw)):
                result = self.collect()
            self.assertFalse(result["policyCreated"])
            self.assertEqual(result["baselineStoppingStage"], stage)
            self.assertNotIn("kubeProxyReplacement", result["baselineSnapshot"]["observedEffectiveConfig"])
            self.assertNotIn("canary", json.dumps(result))
            self.assertEqual(self.api.created, [])
        self.setUp()
        self.api.read_kube_proxy_projection = original
        with patch.object(cilium.subprocess, "run", side_effect=subprocess.TimeoutExpired("private-canary", 12)):
            result = self.collect()
        self.assertEqual(result["baselineStoppingStage"], "kube_proxy_exec")
        self.assertTrue(result["baselineSnapshot"]["checks"]["kube_proxy_exec"]["timedOut"])
        self.assertFalse(result["policyCreated"])

    def test_agent_identity_change_during_status_read_remains_fatal(self):
        def status(_agent, witness=None):
            self.api.objects[fixtures.CILIUM_POD]["metadata"]["uid"] = "replacement"
            return ["False"]
        self.api.read_kube_proxy_projection = status
        result = self.collect()
        self.assertFalse(result["policyCreated"])
        self.assertEqual(result["baselineStoppingStage"], "anchor_identity")
        self.assertFalse(result["baselineSnapshot"]["checks"]["anchor_identity"]["uidMatches"])

    @unittest.skipUnless(shutil.which("kubectl"), "Existing offline JSONPath engine is unavailable")
    def test_old_missing_field_and_correct_tagged_status_shape_with_offline_jsonpath(self):
        old_projection = (r'jsonpath={.PolicyCIDRMatchMode}{"\t"}{.EnableCiliumNetworkPolicy}{"\t"}'
                          r'{.EnableK8sNetworkPolicy}{"\t"}{.KubeProxyReplacement}{"\n"}')
        config = {"PolicyCIDRMatchMode": [], "EnableCiliumNetworkPolicy": True, "EnableK8sNetworkPolicy": True}
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent) as directory:
            path = Path(directory) / "offline-context.json"
            context = {"apiVersion": "v1", "kind": "Config",
                       "clusters": [{"name": "offline", "cluster": {"server": "https://127.0.0.1:9"}}],
                       "users": [{"name": "offline", "user": {}}],
                       "contexts": [{"name": "offline", "context": {"cluster": "offline", "user": "offline"}}],
                       "current-context": "offline",
                       "extensions": [{"name": "tagged-shapes", "extension": {"config": config, "status": STATUS_FALSE}}]}
            def project(expression, member):
                path.write_text(json.dumps(context))
                expression = expression.replace("{.", f"{{.extensions[0].extension.{member}.")
                output = subprocess.run(
                    ["kubectl", "--kubeconfig", str(path), "config", "view", "--minify",
                     "--allow-missing-template-keys=true", "-o", expression],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5, check=False)
                self.assertEqual(output.returncode, 0, "Offline tagged-shape projection failed")
                return output.stdout.decode().strip("\r\n").split("\t")
            legacy = project(old_projection, "config")
            self.assertEqual(legacy, ["[]", "true", "true", ""])
            with self.assertRaises(Failure):
                cilium.effective_configuration(legacy)
            current = project(cilium.CONFIG_OUTPUT, "config")
            self.assertEqual(current, ["[]", "true", "true"])
            self.assertNotIn("kubeProxyReplacement", cilium.effective_configuration(current))
            for mode, expected in (("False", False), ("True", True)):
                value = copy.deepcopy(STATUS_FALSE)
                value["kube-proxy-replacement"]["mode"] = mode
                context["extensions"][0]["extension"]["status"] = value
                fields = project(cilium.KUBE_PROXY_OUTPUT, "status")
                self.assertEqual(fields, [mode])
                self.assertIs(cilium.kube_proxy_mode(fields), expected)
                self.assertNotIn("canary", "\t".join(fields))
            for value in ({}, {"kube-proxy-replacement": None}, {"kube-proxy-replacement": {}},
                          {"kube-proxy-replacement": {"mode": None}},
                          {"kube-proxy-replacement": {"mode": False}},
                          {"kube-proxy-replacement": {"mode": "false"}}):
                context["extensions"][0]["extension"]["status"] = value
                with self.assertRaises(Failure):
                    cilium.kube_proxy_mode(project(cilium.KUBE_PROXY_OUTPUT, "status"))


if __name__ == "__main__":
    unittest.main()
