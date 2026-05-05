"""
AzureClaw default agent for the Microsoft Agent Framework Python runtime.

See the openai-agents default agent for the rationale; replace this
file by mounting agent code via the `agentCode` fields on ClawSandbox.
"""

from __future__ import annotations

import asyncio
import os
import sys
import time

# Bootstrap MUST run in this process so OPENAI_BASE_URL /
# OPENAI_API_KEY (router-managed sentinel) and OTel are set
# before any framework SDK is imported.
from azureclaw_runtime_maf_python.runtime import bootstrap
bootstrap()

BANNER = "🔒 AzureClaw — Microsoft Agent Framework (default agent)"


def _env(name: str, default: str = "(unset)") -> str:
    v = os.environ.get(name)
    return v if v else default


def _print_banner() -> None:
    print("=" * 64, flush=True)
    print(BANNER, flush=True)
    print("=" * 64, flush=True)
    print(f"  sandbox        : {_env('SANDBOX_NAME', _env('HOSTNAME'))}", flush=True)
    print(f"  model          : {_env('AZURE_OPENAI_DEPLOYMENT', _env('OPENCLAW_MODEL'))}", flush=True)
    print(f"  router         : {_env('OPENAI_BASE_URL', 'http://127.0.0.1:8443/v1')}", flush=True)
    print(f"  foundry project: {_env('FOUNDRY_PROJECT_ENDPOINT')}", flush=True)
    print(f"  agt relay      : {_env('AGT_RELAY_URL')}", flush=True)
    print(f"  platform mcp   : {_env('AZURECLAW_PLATFORM_MCP_URL')}", flush=True)
    print("=" * 64, flush=True)


async def _smoke_test() -> None:
    try:
        from agent_framework.openai import OpenAIChatClient  # type: ignore
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ agent-framework import failed: {exc}", flush=True)
        return

    model = os.environ.get("AZURE_OPENAI_DEPLOYMENT") or os.environ.get("OPENCLAW_MODEL") or "gpt-4.1"
    print(f"[default-agent] running smoke test against model={model}…", flush=True)

    try:
        client = OpenAIChatClient(model_id=model)
        agent = client.as_agent(
            instructions=(
                "You are a default smoke-test agent embedded in an AzureClaw sandbox. "
                "Reply with exactly one short sentence confirming you are alive."
            ),
            name="azureclaw-default",
        )
        result = await agent.run("Say hello and confirm you are running inside AzureClaw.")
        print(f"[default-agent] ✓ inference reply: {result}", flush=True)
        print("[default-agent] ✓ Foundry inference proven via inference-router", flush=True)
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ smoke test failed: {exc}", flush=True)


def _idle_forever() -> None:
    print("[default-agent] idling — supply your own /sandbox/agent/main.py to take over.", flush=True)
    while True:
        time.sleep(3600)


def main() -> int:
    _print_banner()
    try:
        asyncio.run(_smoke_test())
    except KeyboardInterrupt:
        return 0
    _idle_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
