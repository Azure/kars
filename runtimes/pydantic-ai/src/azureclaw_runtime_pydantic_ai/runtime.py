# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Bootstrap entrypoint for the Pydantic-AI adapter.

Called from `sandbox-images/pydantic-ai/entrypoint.sh` immediately
before the user's agent code runs. Idempotent — guarded by
`__AZURECLAW_RUNTIME_INITIALIZED__` so re-imports during user code
or tests are no-ops.

Pydantic-AI-specific concerns (vs. the OpenAI-Agents adapter):
  - Pydantic-AI is provider-agnostic — a single `Agent` definition
    can target OpenAI, Azure OpenAI, Anthropic, Gemini, etc. Each
    provider's SDK reads its base URL + API key from the process env
    at construction time. The adapter pins each known provider base
    URL to the router sidecar's matching proxy endpoint so model
    calls cannot egress directly. The router enforces governance,
    content safety, and AAD attestation (no API keys in the pod).
  - For each provider we set the API-key env to a sentinel value
    (`router-managed`); the router strips and substitutes its own
    AAD-attested credential on egress. The egress-guard iptables init
    container drops UID-1000 packets to non-loopback / non-DNS
    targets, so even a leaked base-url cannot reach the public
    provider endpoint.
"""

from __future__ import annotations

import logging
import os
import signal
import sys
from typing import Optional

from azureclaw_runtime_pydantic_ai.otel import init_telemetry

logger = logging.getLogger(__name__)

ENV_INITIALIZED = "__AZURECLAW_RUNTIME_INITIALIZED__"
ROUTER_MANAGED_KEY_SENTINEL = "router-managed"
SERVICE_NAME = "azureclaw-runtime-pydantic-ai"

# (env_var_for_base_url, default_router_path, env_var_for_api_key)
PROVIDER_BASE_URLS: list[tuple[str, str, Optional[str]]] = [
    ("OPENAI_BASE_URL", "http://127.0.0.1:8443/v1", "OPENAI_API_KEY"),
    (
        "AZURE_OPENAI_ENDPOINT",
        "http://127.0.0.1:8443/azure-openai",
        "AZURE_OPENAI_API_KEY",
    ),
    ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8443/anthropic/v1", "ANTHROPIC_API_KEY"),
]


def _install_signal_handlers() -> None:
    """Translate SIGTERM/SIGINT into a clean SystemExit so atexit hooks run."""

    def _handler(signum: int, _frame) -> None:
        logger.info("received signal %s — exiting", signum)
        sys.exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            signal.signal(sig, _handler)
        except (ValueError, OSError):
            pass


def bootstrap(
    *,
    service_name: str = SERVICE_NAME,
    service_version: Optional[str] = None,
) -> None:
    """Idempotently initialize the in-pod adapter.

    1. Skip if `__AZURECLAW_RUNTIME_INITIALIZED__` is already set.
    2. Pin each known provider base URL to the router sidecar so
       LangChain factories cannot reach the public model endpoints
       directly (egress-guard would drop the packet anyway).
    3. Set each provider API-key env to a sentinel; the router
       substitutes the real credential on egress.
    4. Initialize OTel.
    5. Install signal handlers for graceful shutdown.
    6. Mark the env so subsequent imports are no-ops.
    """
    if os.environ.get(ENV_INITIALIZED) == "1":
        logger.debug("bootstrap: already initialized")
        return

    for base_url_env, default, api_key_env in PROVIDER_BASE_URLS:
        os.environ.setdefault(base_url_env, default)
        if api_key_env is not None:
            os.environ.setdefault(api_key_env, ROUTER_MANAGED_KEY_SENTINEL)

    version = service_version or _read_version()
    try:
        init_telemetry(service_name=service_name, service_version=version)
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("init_telemetry failed: %s", exc)

    _install_signal_handlers()

    os.environ[ENV_INITIALIZED] = "1"
    logger.info(
        "azureclaw-runtime-pydantic-ai bootstrapped: providers pinned to router",
    )


def _read_version() -> str:
    try:
        from azureclaw_runtime_pydantic_ai import __version__

        return __version__
    except Exception:  # pragma: no cover
        return "0.0.0"


if __name__ == "__main__":  # pragma: no cover - exercised by entrypoint.sh
    bootstrap()
