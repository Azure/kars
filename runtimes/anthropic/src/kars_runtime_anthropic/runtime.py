# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Bootstrap entrypoint for the Anthropic Claude Agent adapter.

Called from `sandbox-images/anthropic/entrypoint.sh` immediately
before the user's agent code runs. Idempotent — guarded by
`__KARS_RUNTIME_INITIALIZED__` so re-imports during user code
or tests are no-ops.

Anthropic-specific concerns (vs. the OpenAI-Agents adapter):
  - Pins `ANTHROPIC_BASE_URL` at the router sidecar's Anthropic
    proxy endpoint so the Claude SDK's HTTP client cannot reach
    `api.anthropic.com` directly. The router enforces governance,
    content safety, and AAD attestation (no API keys in the pod).
  - Sets `ANTHROPIC_API_KEY` to a sentinel value (`router-managed`)
    because the Claude SDK refuses to start without one set; the
    router strips it on egress and substitutes its own credential.
"""

from __future__ import annotations

import logging
import os
import signal
import sys
from typing import Optional

from kars_runtime_anthropic.otel import init_telemetry

logger = logging.getLogger(__name__)

ENV_INITIALIZED = "__KARS_RUNTIME_INITIALIZED__"
ENV_ANTHROPIC_BASE_URL = "ANTHROPIC_BASE_URL"
ENV_ANTHROPIC_API_KEY = "ANTHROPIC_API_KEY"
DEFAULT_ANTHROPIC_BASE_URL = "http://127.0.0.1:8443/anthropic"
ROUTER_MANAGED_KEY_SENTINEL = "router-managed"
SERVICE_NAME = "kars-runtime-anthropic"


def _install_signal_handlers() -> None:
    """Translate SIGTERM/SIGINT into a clean SystemExit so atexit hooks run.

    OTel BatchSpanProcessor flushes via atexit; without these handlers
    a `kubectl delete pod` truncates the trace stream.
    """

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

    1. Skip if `__KARS_RUNTIME_INITIALIZED__` is already set.
    2. Pin `ANTHROPIC_BASE_URL` to the router sidecar so the SDK
       cannot reach `api.anthropic.com` directly (egress-guard would
       drop the packet anyway, but this fails fast with a clearer error).
    3. Set `ANTHROPIC_API_KEY` to a sentinel — the router substitutes
       the real credential on egress.
    4. Initialize OTel.
    5. Install signal handlers for graceful shutdown.
    6. Mark the env so subsequent imports are no-ops.
    """
    if os.environ.get(ENV_INITIALIZED) == "1":
        logger.debug("bootstrap: already initialized")
        return

    os.environ.setdefault(ENV_ANTHROPIC_BASE_URL, DEFAULT_ANTHROPIC_BASE_URL)
    # The Claude SDK refuses to construct a client without an API key
    # set. The router strips this header on egress and substitutes
    # its own AAD-attested credential, so the sentinel never reaches
    # Anthropic. Documented in the security audit (LLM07).
    os.environ.setdefault(ENV_ANTHROPIC_API_KEY, ROUTER_MANAGED_KEY_SENTINEL)

    version = service_version or _read_version()
    try:
        init_telemetry(service_name=service_name, service_version=version)
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("init_telemetry failed: %s", exc)

    _install_signal_handlers()

    os.environ[ENV_INITIALIZED] = "1"
    logger.info(
        "kars-runtime-anthropic bootstrapped: base_url=%s",
        os.environ[ENV_ANTHROPIC_BASE_URL],
    )


def _read_version() -> str:
    try:
        from kars_runtime_anthropic import __version__

        return __version__
    except Exception:  # pragma: no cover
        return "0.0.0"


if __name__ == "__main__":  # pragma: no cover - exercised by entrypoint.sh
    bootstrap()
