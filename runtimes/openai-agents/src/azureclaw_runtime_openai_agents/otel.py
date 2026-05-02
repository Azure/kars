# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
OpenTelemetry auto-init for the OpenAI Agents adapter.

The router sidecar exposes an OTLP/HTTP collector at
`/v1/traces` and `/v1/metrics`. We default to the loopback address so
egress-guard can keep the pod sealed; an operator may override via
`OTEL_EXPORTER_OTLP_ENDPOINT` if they front the collector elsewhere.

`init_telemetry()` is idempotent: a second call replaces nothing — once
a tracer/meter provider is set globally we leave it alone. This keeps
re-imports during test runs from blowing up the global state.
"""

from __future__ import annotations

import logging
import os
import threading
from typing import Optional

logger = logging.getLogger(__name__)

DEFAULT_OTLP_ENDPOINT = "http://127.0.0.1:8443/v1/traces"
DEFAULT_OTLP_METRICS_ENDPOINT = "http://127.0.0.1:8443/v1/metrics"

_init_lock = threading.Lock()
_initialized = False


def _is_initialized() -> bool:
    return _initialized


def _otlp_endpoint(env_var: str, default: str) -> str:
    raw = os.environ.get(env_var)
    return raw if raw else default


def init_telemetry(
    service_name: str,
    service_version: str = "0.1.0",
    *,
    traces_endpoint: Optional[str] = None,
    metrics_endpoint: Optional[str] = None,
) -> None:
    """Configure global OTel tracer + meter providers.

    Calling this more than once is a no-op (subsequent calls log and
    return). `traces_endpoint`/`metrics_endpoint` override the
    `OTEL_EXPORTER_OTLP_*` env vars when given (used by tests).
    """
    global _initialized
    with _init_lock:
        if _initialized:
            logger.debug("init_telemetry: already initialized, skipping")
            return

        # Imports deferred so the module imports cheaply when telemetry
        # is disabled (e.g. unit tests that just probe constants).
        from opentelemetry import metrics, trace
        from opentelemetry.exporter.otlp.proto.http.metric_exporter import (
            OTLPMetricExporter,
        )
        from opentelemetry.exporter.otlp.proto.http.trace_exporter import (
            OTLPSpanExporter,
        )
        from opentelemetry.sdk.metrics import MeterProvider
        from opentelemetry.sdk.metrics.export import PeriodicExportingMetricReader
        from opentelemetry.sdk.resources import Resource
        from opentelemetry.sdk.trace import TracerProvider
        from opentelemetry.sdk.trace.export import BatchSpanProcessor

        traces_url = traces_endpoint or _otlp_endpoint(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            _otlp_endpoint("OTEL_EXPORTER_OTLP_ENDPOINT", DEFAULT_OTLP_ENDPOINT),
        )
        metrics_url = metrics_endpoint or _otlp_endpoint(
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            DEFAULT_OTLP_METRICS_ENDPOINT,
        )

        resource = Resource.create(
            {
                "service.name": service_name,
                "service.version": service_version,
                "service.namespace": "azureclaw",
                "azureclaw.runtime.kind": "OpenAIAgents",
            }
        )

        tracer_provider = TracerProvider(resource=resource)
        tracer_provider.add_span_processor(
            BatchSpanProcessor(OTLPSpanExporter(endpoint=traces_url))
        )
        trace.set_tracer_provider(tracer_provider)

        metric_reader = PeriodicExportingMetricReader(
            OTLPMetricExporter(endpoint=metrics_url),
        )
        meter_provider = MeterProvider(
            resource=resource, metric_readers=[metric_reader]
        )
        metrics.set_meter_provider(meter_provider)

        _instrument_httpx()

        _initialized = True
        logger.info(
            "azureclaw OTel initialized: service=%s traces=%s metrics=%s",
            service_name,
            traces_url,
            metrics_url,
        )


def _instrument_httpx() -> None:
    """Auto-instrument httpx so every LLM/tool HTTP call is a span.

    The OpenAI Python SDK uses httpx under the hood, so this transitively
    captures every model call. Failures are downgraded to warnings —
    telemetry must never crash the agent.
    """
    try:
        from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor

        HTTPXClientInstrumentor().instrument()
    except Exception as exc:  # pragma: no cover - defensive
        logger.warning("httpx instrumentation failed: %s", exc)


def _reset_for_tests() -> None:
    """Test-only hook to reset the module-level init flag."""
    global _initialized
    with _init_lock:
        _initialized = False
