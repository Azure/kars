# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Bootstrap entrypoint for the MAF Python adapter.

Called from `sandbox-images/maf-python/entrypoint.sh` immediately
before the user's agent code runs. Idempotent — guarded by the
`__AZURECLAW_RUNTIME_INITIALIZED__` env var.
"""

from __future__ import annotations

import logging
import os
import signal
import sys
from typing import Optional

from azureclaw_runtime_maf_python.otel import init_telemetry

logger = logging.getLogger(__name__)

ENV_INITIALIZED = "__AZURECLAW_RUNTIME_INITIALIZED__"
ENV_OPENAI_BASE_URL = "OPENAI_BASE_URL"
ENV_AZURE_OPENAI_ENDPOINT = "AZURE_OPENAI_ENDPOINT"
DEFAULT_OPENAI_BASE_URL = "http://127.0.0.1:8443/openai/v1"
DEFAULT_AZURE_OPENAI_ENDPOINT = "http://127.0.0.1:8443/openai"
SERVICE_NAME = "azureclaw-runtime-maf-python"


def _install_signal_handlers() -> None:
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
    """Idempotently initialize the MAF in-pod adapter.

    1. Skip if `__AZURECLAW_RUNTIME_INITIALIZED__` is set.
    2. Pin `OPENAI_BASE_URL` and `AZURE_OPENAI_ENDPOINT` to the router
       sidecar so MAF's OpenAI/AzureOpenAI clients can never reach a
       public endpoint by accident.
    3. Initialize OTel.
    4. Install signal handlers for graceful shutdown.
    5. Mark the env so subsequent imports are no-ops.
    """
    if os.environ.get(ENV_INITIALIZED) == "1":
        logger.debug("bootstrap: already initialized")
        return

    os.environ.setdefault(ENV_OPENAI_BASE_URL, DEFAULT_OPENAI_BASE_URL)
    os.environ.setdefault(ENV_AZURE_OPENAI_ENDPOINT, DEFAULT_AZURE_OPENAI_ENDPOINT)

    version = service_version or _read_version()
    try:
        init_telemetry(service_name=service_name, service_version=version)
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("init_telemetry failed: %s", exc)

    _install_signal_handlers()

    os.environ[ENV_INITIALIZED] = "1"
    logger.info(
        "azureclaw-runtime-maf-python bootstrapped: openai_base_url=%s azure_openai_endpoint=%s",
        os.environ[ENV_OPENAI_BASE_URL],
        os.environ[ENV_AZURE_OPENAI_ENDPOINT],
    )


def _read_version() -> str:
    try:
        from azureclaw_runtime_maf_python import __version__

        return __version__
    except Exception:  # pragma: no cover
        return "0.0.0"


if __name__ == "__main__":  # pragma: no cover - exercised by entrypoint.sh
    bootstrap()
