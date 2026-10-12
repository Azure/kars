# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
ENTRYPOINT = ROOT / "sandbox-images/agentmesh/entrypoint.py"


class AgentMeshEntrypointTests(unittest.TestCase):
    def test_build_installer_uses_only_selected_core_server_and_native_owner(self):
        installer = runpy.run_path(str(ROOT / "sandbox-images/agentmesh/install.py"))
        core = Path("/tmp/agt-wheels/agent_governance_toolkit_core-5.0.0-py3-none-any.whl")
        native = Path("/tmp/agt-wheels/arm64/agent_control_specification-0.4.0b0-cp311-abi3-linux_aarch64.whl")
        with patch("runpy.run_path") as load, patch("subprocess.run") as execute, \
                patch("platform.machine", return_value="aarch64"):
            from unittest.mock import Mock
            select = Mock(return_value=[core, native, Path("/tmp/agt-wheels/unused-cli.whl")])
            load.return_value = {"select_wheels": select}
            installer["main"]()
            select.assert_called_once_with(Path("/tmp/agt-wheels"), "aarch64")
            execute.assert_any_call([
                sys.executable, "-m", "pip", "install", "--no-cache-dir",
                "--target", "/opt/agentmesh", "--constraint", "/tmp/constraints.txt",
                str(core) + "[server]", str(native),
            ], check=True)
            self.assertEqual(execute.call_count, 2)
            self.assertEqual(execute.call_args.args[0], [sys.executable, "-m", "pip", "check"])
            self.assertEqual(execute.call_args.kwargs["env"]["PYTHONPATH"], "/opt/agentmesh")

    def test_execs_unchanged_upstream_module_and_preserves_arguments(self):
        for component in ("registry", "relay"):
            with self.subTest(component=component), \
                    patch.dict(os.environ, {"AGENTMESH_COMPONENT": component}), \
                    patch.object(sys, "argv", [str(ENTRYPOINT), "--example", "two words"]), \
                    patch("os.execv") as execute:
                runpy.run_path(str(ENTRYPOINT), run_name="__main__")
                execute.assert_called_once_with(sys.executable, [
                    sys.executable, "-m", f"agentmesh.{component}", "--example", "two words",
                ])

    def test_rejects_missing_or_invalid_component_before_execution(self):
        for component in (None, "", "unknown", "registry; sh", "../relay"):
            environment = {} if component is None else {"AGENTMESH_COMPONENT": component}
            with self.subTest(component=component), patch.dict(os.environ, environment, clear=True), \
                    patch("os.execv") as execute:
                with self.assertRaisesRegex(SystemExit, "must be registry or relay"):
                    runpy.run_path(str(ENTRYPOINT), run_name="__main__")
                execute.assert_not_called()

    def test_packaging_reuses_verified_owner_wheels_and_distroless_python(self):
        recipe = (ROOT / "sandbox-images/agentmesh/Dockerfile").read_text()
        installer = (ROOT / "sandbox-images/agentmesh/install.py").read_text()
        self.assertIn('["select_wheels"]', installer)
        self.assertIn('str(core) + "[server]", str(native)', installer)
        self.assertIn("RUN python3 /tmp/install-agentmesh.py", recipe)
        self.assertNotIn("<<", recipe)
        self.assertNotIn('".[server]"', recipe)
        final = recipe.split("\nFROM ")[-1]
        self.assertTrue(final.startswith("mcr.microsoft.com/azurelinux/distroless/python:3.12@sha256:"))
        self.assertNotIn("\nRUN ", final)
        self.assertIn("USER 1000:1000", final)
        self.assertIn('ENTRYPOINT ["/usr/bin/python3", "/opt/kars/launch-agentmesh.py"]', final)
        self.assertNotIn("/opt/kars/agentmesh.py", final)
        self.assertIn("COPY vendor/agt/pin.json", final)


if __name__ == "__main__":
    unittest.main()
