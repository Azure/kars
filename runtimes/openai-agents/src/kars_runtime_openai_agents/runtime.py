# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Bootstrap entrypoint for the OpenAI Agents adapter.

Called from `sandbox-images/openai-agents/entrypoint.sh` immediately
before the user's agent code runs. Idempotent — guarded by the
`__KARS_RUNTIME_INITIALIZED__` env var so re-imports during user
code (or test runs that import the package multiple times) do nothing.
"""

from __future__ import annotations

import logging
import os
import signal
import sys
from typing import Optional

from kars_runtime_openai_agents.otel import init_telemetry

logger = logging.getLogger(__name__)

ENV_INITIALIZED = "__KARS_RUNTIME_INITIALIZED__"
ENV_OPENAI_BASE_URL = "OPENAI_BASE_URL"
ENV_OPENAI_API_KEY = "OPENAI_API_KEY"
DEFAULT_OPENAI_BASE_URL = "http://127.0.0.1:8443/v1"
# The router authenticates upstream Foundry calls via Workload Identity
# (IMDS), so the SDK-side API key is never used for auth — but the
# OpenAI Python SDK refuses to construct a client when the env var is
# missing or empty. We pin a sentinel so `OpenAI()` succeeds; the
# router ignores client-supplied keys.
ROUTER_MANAGED_KEY_SENTINEL = "router-managed"
SERVICE_NAME = "kars-runtime-openai-agents"


def _install_signal_handlers() -> None:
    """Translate SIGTERM/SIGINT into a clean SystemExit so atexit hooks run.

    The OTel BatchSpanProcessor flushes via atexit; without these handlers
    a `kubectl delete pod` truncates the trace stream.
    """
    def _handler(signum: int, _frame) -> None:
        logger.info("received signal %s — exiting", signum)
        sys.exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            signal.signal(sig, _handler)
        except (ValueError, OSError):
            # Not on the main thread; nothing to do.
            pass


def bootstrap(
    *,
    service_name: str = SERVICE_NAME,
    service_version: Optional[str] = None,
) -> None:
    """Idempotently initialize the in-pod adapter.

    1. Skip if `__KARS_RUNTIME_INITIALIZED__` is set.
    2. Pin `OPENAI_BASE_URL` to the router sidecar so the OpenAI SDK
       cannot reach a public endpoint by accident (egress-guard would
       drop the packet anyway, but this fails fast with a clear error).
    3. Initialize OTel.
    4. Install signal handlers for graceful shutdown.
    5. Mark the env so subsequent imports are no-ops.
    """
    if os.environ.get(ENV_INITIALIZED) == "1":
        logger.debug("bootstrap: already initialized")
        return

    # Make sure the SDK never accidentally points at api.openai.com.
    os.environ.setdefault(ENV_OPENAI_BASE_URL, DEFAULT_OPENAI_BASE_URL)
    os.environ.setdefault(ENV_OPENAI_API_KEY, ROUTER_MANAGED_KEY_SENTINEL)

    version = service_version or _read_version()
    try:
        init_telemetry(service_name=service_name, service_version=version)
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("init_telemetry failed: %s", exc)

    _install_signal_handlers()

    os.environ[ENV_INITIALIZED] = "1"
    logger.info(
        "kars-runtime-openai-agents bootstrapped: base_url=%s",
        os.environ[ENV_OPENAI_BASE_URL],
    )


def _read_version() -> str:
    try:
        from kars_runtime_openai_agents import __version__

        return __version__
    except Exception:  # pragma: no cover
        return "0.0.0"


if __name__ == "__main__":  # pragma: no cover - exercised by entrypoint.sh
    bootstrap()
