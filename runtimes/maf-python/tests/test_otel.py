# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os

from kars_runtime_maf_python import otel


def test_init_telemetry_sets_providers(monkeypatch):
    monkeypatch.setenv(
        "OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:8443/v1/traces"
    )
    otel._reset_for_tests()
    otel.init_telemetry("svc-test", "9.9.9")

    from opentelemetry import metrics, trace
    from opentelemetry.sdk.metrics import MeterProvider
    from opentelemetry.sdk.trace import TracerProvider

    assert isinstance(trace.get_tracer_provider(), TracerProvider)
    assert isinstance(metrics.get_meter_provider(), MeterProvider)


def test_init_telemetry_idempotent():
    otel._reset_for_tests()
    otel.init_telemetry("svc1")
    # Second call must not throw and must not re-set the providers.
    otel.init_telemetry("svc1")


def test_init_telemetry_uses_default_endpoint_when_unset(monkeypatch):
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", raising=False)
    otel._reset_for_tests()
    # Just exercise the default path; absence of an env var must not raise.
    otel.init_telemetry("svc-default")


def test_otlp_endpoint_resolution(monkeypatch):
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://override:4318")
    assert (
        otel._otlp_endpoint("OTEL_EXPORTER_OTLP_ENDPOINT", "http://default")
        == "http://override:4318"
    )
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT")
    assert (
        otel._otlp_endpoint("OTEL_EXPORTER_OTLP_ENDPOINT", "http://default")
        == "http://default"
    )


def test_init_telemetry_explicit_endpoints_override_env(monkeypatch):
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://env:4318")
    otel._reset_for_tests()
    # Should not raise even though we point at an unreachable endpoint —
    # the BatchSpanProcessor exports lazily.
    otel.init_telemetry(
        "svc-explicit",
        traces_endpoint="http://explicit:4318/v1/traces",
        metrics_endpoint="http://explicit:4318/v1/metrics",
    )


def test_default_endpoint_constants():
    assert otel.DEFAULT_OTLP_ENDPOINT.endswith("/v1/traces")
    assert "127.0.0.1:8443" in otel.DEFAULT_OTLP_ENDPOINT
    assert otel.DEFAULT_OTLP_METRICS_ENDPOINT.endswith("/v1/metrics")
