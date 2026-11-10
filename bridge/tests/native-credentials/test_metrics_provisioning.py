# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Native metrics image wiring contracts, not hosted API readiness evidence."""

import io
import json
from contextlib import redirect_stdout
from pathlib import Path
import shlex
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import boot


ROOT = Path(__file__).resolve().parents[3]
IMAGE = "kars-native-metrics-server:latest"
TRUST = {
    "hostAliases": [{"ip": "172.18.0.2", "hostnames": ["bridge-native-control-plane"]},
                    {"ip": "172.18.0.3", "hostnames": ["bridge-native-worker"]}],
    "kubelet": {"insecureTLS": False, "preferredAddressTypes": ["Hostname"],
                "caBundle": "verified-public-ca"},
}


class MetricsProvisioningTests(unittest.TestCase):
    def test_native_metrics_trust_collector(self):
        result = subprocess.run(
            ["node", "--test", "bridge/tests/native-credentials/metrics-trust.test.mjs"],
            cwd=ROOT, capture_output=True, text=True, timeout=30, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_native_builds_existing_al3_recipe_and_loads_before_acceptance(self):
        workflow = (ROOT / ".github/workflows/bridge-native.yml").read_text()
        runtime = workflow.split("  native-runtime:\n", 1)[1].split("  native-required-gates:\n", 1)[0]
        commands = [shlex.split(line.strip()) for line in runtime.replace("\\\n", " ").splitlines()
                    if line.strip().startswith(("docker build ", "kind load docker-image "))]
        builds = [args for args in commands if args[:2] == ["docker", "build"] and IMAGE in args]
        self.assertEqual(builds, [[
            "docker", "build", "--file", ".native/core/sandbox-images/metrics-server/Dockerfile",
            "--tag", IMAGE, ".native/core",
        ]])
        loads = [args for args in commands if args[:3] == ["kind", "load", "docker-image"]]
        self.assertEqual(len(loads), 1)
        self.assertEqual(loads[0][3:5], ["--name", "bridge-native"])
        self.assertEqual(loads[0].count(IMAGE), 1)
        self.assertLess(runtime.index("--tag " + IMAGE), runtime.index("kind load docker-image"))
        self.assertLess(runtime.index("kind load docker-image"),
                        runtime.index("python3 tests/native-credentials/run.py"))
        recipe = (ROOT / "sandbox-images/metrics-server/Dockerfile").read_text()
        self.assertIn("mcr.microsoft.com/azurelinux/distroless/base:3.0", recipe)
        self.assertIn("FROM ${AZURELINUX_DISTROLESS}", recipe)
        self.assertIn("FROM registry.k8s.io/metrics-server/metrics-server@sha256:", recipe)

    def test_boot_uses_only_loaded_metrics_without_changing_tls_or_registration(self):
        setup = SimpleNamespace(
            namespace=lambda name: {"metadata": {"name": name}},
            admin=SimpleNamespace(patch=lambda *_: None, get=lambda *_: {"items": []}),
        )
        with patch.object(boot, "prepare_schemas", return_value={"published": True}), \
             patch.object(boot, "command", return_value=json.dumps(TRUST)) as command, \
             patch.object(boot, "private_file") as write, \
             patch.object(boot, "loaded_image", return_value={}), redirect_stdout(io.StringIO()):
            boot.install_core(setup)
        self.assertEqual(command.call_args_list[0].args,
                         ("node", "tests/native-credentials/metrics-trust.mjs"))
        values = json.loads(write.call_args.args[1])
        self.assertEqual(values["metricsServer"], {
            **TRUST,
            "enabled": True,
            "image": {"repository": "docker.io/library/kars-native-metrics-server",
                      "tag": "latest", "pullPolicy": "Never"},
        })
        self.assertFalse(any(env["name"] == "KARS_DEV_PROFILE"
                             for env in values["controller"]["extraEnv"]))

    def test_unverified_trust_cannot_install_core(self):
        setup = SimpleNamespace(namespace=Mock())
        with patch.object(boot, "command", side_effect=RuntimeError("trust rejected")) as command, \
             patch.object(boot, "private_file") as write:
            with self.assertRaisesRegex(RuntimeError, "trust rejected"):
                boot.install_core(setup)
        setup.namespace.assert_not_called()
        write.assert_not_called()
        self.assertEqual(command.call_count, 1)

    def test_chart_retains_serving_and_kubelet_tls_and_production_pull_policy(self):
        template = (ROOT / "deploy/helm/kars/templates/metrics-server.yaml").read_text()
        api = template.split("kind: APIService\n", 1)[1].split("---", 1)[0]
        self.assertIn("name: v1beta1.metrics.k8s.io", api)
        self.assertIn('caBundle: {{ get $tls "ca.crt" | quote }}', api)
        self.assertNotIn("insecureSkipTLSVerify", api)
        self.assertIn("--tls-cert-file=/var/run/metrics-server/tls.crt", template)
        self.assertIn("--kubelet-certificate-authority=/var/run/secrets/kubernetes.io/serviceaccount/ca.crt",
                      template)
        defaults = (ROOT / "deploy/helm/kars/values.yaml").read_text().split("\nmetricsServer:\n", 1)[1]
        defaults = defaults.split("\nevals:\n", 1)[0]
        self.assertIn("enabled: true", defaults)
        self.assertIn("pullPolicy: Always", defaults)
        self.assertIn("insecureTLS: false", defaults)


if __name__ == "__main__":
    unittest.main()
