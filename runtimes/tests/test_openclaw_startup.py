# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ENTRYPOINT = Path(__file__).resolve().parents[2] / "sandbox-images/openclaw/entrypoint.sh"


def render_config(path, primary="azure-openai/gpt-5.4-mini"):
    source = ENTRYPOINT.read_text()
    start = source.index('  cat > "$OPENCLAW_CONFIG" << EOF')
    end = source.index("\nEOF", start) + len("\nEOF")
    mcp_start = source.index('  _MCP_BLOCK=""')
    mcp_end = source.index("  # mcporter", mcp_start)
    result = subprocess.run(
        ["bash", "-e", "-s"], input=source[mcp_start:mcp_end] + source[start:end] + "\n",
        capture_output=True, text=True, timeout=5,
        env={
            **os.environ, "OPENCLAW_CONFIG": str(path),
            "MODELS_JSON": '[{"id":"gpt-5.4-mini","name":"Fixture model"}]',
            "_PRIMARY_MODEL_REF": primary, "_ANTHROPIC_PROVIDER_BLOCK": "",
            "KARS_MCP_SERVERS": "", "HOSTNAME": "fixture-agent",
            "KARS_DISPLAY_NAME": "Feedback pilot",
        },
    )
    if result.returncode:
        raise RuntimeError(result.stderr)
    content = path.read_text().replace("PLUGINS_ALLOW_PLACEHOLDER", '"kars"')
    content = content.replace("PLUGINS_ENTRIES_PLACEHOLDER", '"kars":{"enabled":true}')
    return json.loads(content.replace("CHANNELS_PLACEHOLDER", ""))


class OpenClawConfigTests(unittest.TestCase):
    def test_current_schema_paths_preserve_routed_models_and_memory(self):
        with tempfile.TemporaryDirectory() as temporary:
            for primary in ("azure-openai/gpt-5.4-mini", "anthropic/claude-sonnet-4"):
                with self.subTest(primary=primary):
                    config = render_config(Path(temporary) / "openclaw.json", primary)
                    defaults = config["agents"]["defaults"]
                    self.assertEqual(defaults["model"]["primary"], primary)
                    self.assertEqual(defaults["mediaModels"]["image"], "azure-openai/gpt-image-1")
                    self.assertEqual(defaults["timeoutSeconds"], 1500)
                    self.assertNotIn("imageGenerationModel", defaults)
                    self.assertNotIn("memorySearch", defaults)
                    memory = config["memory"]["search"]
                    self.assertTrue(memory["enabled"])
                    self.assertEqual(memory["provider"], "azure-openai")
                    self.assertEqual(memory["model"], "text-embedding-3-small")
                    self.assertEqual(memory["remote"]["baseUrl"], "http://127.0.0.1:8443/v1/")
                    self.assertEqual(config["agents"]["entries"]["main"]["identity"]["name"], "Feedback pilot")
                    self.assertNotIn("ui", config)

    def test_auth_and_governance_are_not_relaxed_by_schema_migration(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = render_config(Path(temporary) / "openclaw.json")
        self.assertEqual(config["gateway"]["auth"], {"mode": "token"})
        self.assertEqual(config["gateway"]["controlUi"], {"enabled": True})
        self.assertEqual(config["tools"]["deny"], ["sessions_spawn", "sessions_send"])
        self.assertEqual(config["tools"]["exec"], {"mode": "full"})
        self.assertEqual(config["plugins"], {"allow": ["kars"], "entries": {"kars": {"enabled": True}}})
        self.assertEqual(config["mcp"]["servers"]["kars-router"]["url"], "http://127.0.0.1:8443/mcp")


class OpenClawStartupTests(unittest.TestCase):
    def run_gateway(self, exit_code=None, healthy=False):
        source = ENTRYPOINT.read_text()
        block = source.split("# Start OpenClaw gateway in the background", 1)[1]
        block = block.split("# Start the node host", 1)[0]
        block = "# Start OpenClaw gateway in the background" + block
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "gateway.log"
            block = block.replace("/tmp/gateway.log", shlex.quote(str(log)))
            gateway = (
                f"return {exit_code}"
                if exit_code is not None
                else "while :; do command sleep 0.02; done"
            )
            health = f"test -s {shlex.quote(str(log))}" if healthy else "return 1"
            script = f"""
set -e
AS_SANDBOX=''
GATEWAY_TOKEN='test-only'
openclaw() {{
  echo 'gateway output'
  echo 'gateway diagnostic' >&2
  {gateway}
}}
curl() {{ {health}; }}
sleep() {{ command sleep 0.02; }}
{block}
echo 'node host may start'
kill -TERM "$GATEWAY_PID"
wait "$GATEWAY_PID" || true
"""
            result = subprocess.run(
                ["bash", "-c", script], capture_output=True, text=True, timeout=5
            )
            return result, log.read_text()

    def test_failed_gateway_keeps_original_exit_and_surfaces_both_streams(self):
        result, log = self.run_gateway(exit_code=23)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertIn("status: 23", result.stderr)
        self.assertNotIn("node host may start", result.stdout)
        for line in ("gateway output", "gateway diagnostic"):
            self.assertIn(line, result.stdout)
            self.assertIn(line, log)

    def test_early_success_exit_is_not_a_running_gateway(self):
        result, _ = self.run_gateway(exit_code=0)
        self.assertEqual(result.returncode, 1)
        self.assertIn("status: 0", result.stderr)
        self.assertNotIn("node host may start", result.stdout)

    def test_healthy_gateway_still_starts_node_host(self):
        result, _ = self.run_gateway(healthy=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Gateway running", result.stdout)
        self.assertIn("node host may start", result.stdout)

    def test_slow_live_gateway_preserves_existing_startup_window(self):
        result, _ = self.run_gateway()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("node host may start", result.stdout)
        self.assertNotIn("Gateway running", result.stdout)

    def test_node_host_also_preserves_file_and_container_logging(self):
        source = ENTRYPOINT.read_text()
        self.assertIn('> >(tee /tmp/node-host.log) 2>&1 &', source)


if __name__ == "__main__":
    unittest.main()
