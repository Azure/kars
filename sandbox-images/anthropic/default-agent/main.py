"""
AzureClaw default agent for the Anthropic Claude runtime.

The router exposes a Claude-compatible surface (or an OpenAI-compat
surface — depends on your `azureclaw.azure.com/v1alpha1/InferencePolicy`
configuration). This default agent uses the openai SDK against the
router for portability; swap to `anthropic.Anthropic()` once you
attach an Anthropic-backed InferencePolicy.

Replace this file by mounting agent code via the `agentCode` fields
on ClawSandbox.
"""

from __future__ import annotations

import os
import sys
import time

BANNER = "🔒 AzureClaw — Anthropic Claude (default agent)"


def _env(name: str, default: str = "(unset)") -> str:
    v = os.environ.get(name)
    return v if v else default


def _print_banner() -> None:
    print("=" * 64, flush=True)
    print(BANNER, flush=True)
    print("=" * 64, flush=True)
    print(f"  sandbox        : {_env('SANDBOX_NAME', _env('HOSTNAME'))}", flush=True)
    print(f"  model          : {_env('AZURE_OPENAI_DEPLOYMENT', _env('OPENCLAW_MODEL'))}", flush=True)
    print(f"  router (openai): {_env('OPENAI_BASE_URL', 'http://127.0.0.1:8443/v1')}", flush=True)
    print(f"  anthropic base : {_env('ANTHROPIC_BASE_URL')}", flush=True)
    print(f"  foundry project: {_env('FOUNDRY_PROJECT_ENDPOINT')}", flush=True)
    print(f"  agt relay      : {_env('AGT_RELAY_URL')}", flush=True)
    print("=" * 64, flush=True)


def _smoke_test() -> None:
    try:
        from openai import OpenAI  # type: ignore
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ openai SDK import failed: {exc}", flush=True)
        return

    model = os.environ.get("AZURE_OPENAI_DEPLOYMENT") or os.environ.get("OPENCLAW_MODEL") or "gpt-4.1"
    print(f"[default-agent] running smoke test against model={model} via router…", flush=True)

    try:
        client = OpenAI()
        resp = client.chat.completions.create(
            model=model,
            messages=[
                {"role": "system", "content": "You are a default smoke-test agent embedded in an AzureClaw sandbox."},
                {"role": "user", "content": "Reply with exactly one short sentence confirming you are alive."},
            ],
        )
        print(f"[default-agent] ✓ inference reply: {resp.choices[0].message.content}", flush=True)
        print("[default-agent] ✓ Foundry inference proven via inference-router", flush=True)
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ smoke test failed: {exc}", flush=True)


def _idle_forever() -> None:
    print("[default-agent] idling — supply your own /sandbox/agent/main.py to take over.", flush=True)
    while True:
        time.sleep(3600)


def main() -> int:
    _print_banner()
    _smoke_test()
    _idle_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
