# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Shared pytest fixtures for the openai-agents adapter tests."""

from __future__ import annotations

import os
from typing import Iterator

import pytest

from azureclaw_runtime_maf_python import aad, mesh, otel, tools


@pytest.fixture(autouse=True)
def _reset_module_state() -> Iterator[None]:
    """Reset module-level singletons before every test."""
    aad.reset_default_broker()
    mesh.reset_default_client()
    tools.reset_default_mcp_client()
    otel._reset_for_tests()
    # Wipe env vars that bleed between tests.
    for key in (
        "AZURECLAW_AGENT_DID",
        "AZURECLAW_AGENT_NAME",
        "AZURECLAW_AGT_RELAY_URL",
        "AZURECLAW_AGT_REGISTRY_URL",
        "AZURECLAW_PLATFORM_MCP_URL",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OPENAI_BASE_URL",
        "AZURE_OPENAI_ENDPOINT",
        "__AZURECLAW_RUNTIME_INITIALIZED__",
    ):
        os.environ.pop(key, None)
    # Suppress otel SDK's retry-loop log noise on tests that exercise
    # init_telemetry without a live collector.
    os.environ["OTEL_SDK_DISABLED"] = os.environ.get("OTEL_SDK_DISABLED", "true")
    yield
    aad.reset_default_broker()
    mesh.reset_default_client()
    tools.reset_default_mcp_client()
    otel._reset_for_tests()
    os.environ.pop("OTEL_SDK_DISABLED", None)
