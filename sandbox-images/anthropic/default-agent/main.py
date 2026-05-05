"""
AzureClaw default agent for the Anthropic Claude runtime.

The router exposes `/anthropic/v1/messages` — a translation route that
accepts native Anthropic Messages-API requests and forwards them to
Foundry chat completions on the way through. When Foundry adds a
native Anthropic surface this becomes a passthrough, no client
changes needed.

Replace this file by mounting agent code via the `agentCode` fields
on ClawSandbox.
"""

from __future__ import annotations

import os
import sys
import time

# Bootstrap MUST run in this process so ANTHROPIC_BASE_URL /
# ANTHROPIC_API_KEY (router-managed sentinel) and OTel are set
# before the Anthropic SDK is imported.
from azureclaw_runtime_anthropic.runtime import bootstrap
bootstrap()

BANNER = "🔒 AzureClaw — Anthropic Claude (default agent)"


def _env(name: str, default: str = "(unset)") -> str:
    v = os.environ.get(name)
    return v if v else default


def _print_banner() -> None:
    print("=" * 64, flush=True)
    print(BANNER, flush=True)
    print("=" * 64, flush=True)
    print(f"  sandbox        : {_env('SANDBOX_NAME', _env('HOSTNAME'))}", flush=True)
    print(f"  model          : {_env('ANTHROPIC_MODEL', _env('OPENCLAW_MODEL'))}", flush=True)
    print(f"  anthropic base : {_env('ANTHROPIC_BASE_URL', 'http://127.0.0.1:8443/anthropic')}", flush=True)
    print(f"  foundry project: {_env('FOUNDRY_PROJECT_ENDPOINT')}", flush=True)
    print(f"  agt relay      : {_env('AGT_RELAY_URL')}", flush=True)
    print("=" * 64, flush=True)


def _smoke_test() -> None:
    try:
        from anthropic import Anthropic  # type: ignore
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ anthropic SDK import failed: {exc}", flush=True)
        return

    # ANTHROPIC_MODEL wins; otherwise fall back to whatever Foundry
    # default the runtime adapter pinned (e.g. gpt-4.1 — the translation
    # route doesn't care about the model name shape).
    model = (
        os.environ.get("ANTHROPIC_MODEL")
        or os.environ.get("OPENCLAW_MODEL")
        or "gpt-4.1"
    )
    print(f"[default-agent] running smoke test against model={model} via Anthropic SDK…", flush=True)

    try:
        client = Anthropic(api_key=os.environ.get("ANTHROPIC_API_KEY") or "router-managed")
        msg = client.messages.create(
            model=model,
            max_tokens=128,
            system="You are a default smoke-test agent embedded in an AzureClaw sandbox.",
            messages=[
                {"role": "user", "content": "Reply with exactly one short sentence confirming you are alive."}
            ],
        )
        text = "".join(
            block.text for block in msg.content if getattr(block, "type", None) == "text"
        )
        print(f"[default-agent] ✓ inference reply: {text}", flush=True)
        print(f"[default-agent] ✓ usage: in={msg.usage.input_tokens} out={msg.usage.output_tokens}", flush=True)
        print("[default-agent] ✓ Foundry inference proven via /anthropic/v1/messages router route", flush=True)
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
