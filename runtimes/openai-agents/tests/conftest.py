# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Shared pytest fixtures for the openai-agents adapter tests."""

from __future__ import annotations

import os
from typing import Iterator

import pytest

from kars_runtime_openai_agents import aad, mesh, otel, tools


@pytest.fixture(autouse=True)
def _reset_module_state() -> Iterator[None]:
    """Reset module-level singletons before every test."""
    aad.reset_default_broker()
    mesh.reset_default_client()
    tools.reset_default_mcp_client()
    otel._reset_for_tests()
    # Wipe env vars that bleed between tests.
    for key in (
        "KARS_AGENT_DID",
        "KARS_AGENT_NAME",
        "KARS_AGT_RELAY_URL",
        "KARS_AGT_REGISTRY_URL",
        "KARS_PLATFORM_MCP_URL",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OPENAI_BASE_URL",
        "__KARS_RUNTIME_INITIALIZED__",
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
