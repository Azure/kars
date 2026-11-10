# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from unittest.mock import MagicMock

import httpx
import pytest

from kars_runtime_hermes.plugin import sre_reports as reports
from kars_runtime_hermes.plugin import sre_watcher as watcher


def record(**values):
    return {"sweep_id": "a" * 32, "started_at": 1000.0, "finished_at": 1001.0,
            "turn_status": "blocked", "error": "Budget blocked; coverage unknown", **values}


def receipt():
    return {"persisted": True, "sweep_id": "a" * 32, "digest": "b" * 64,
            "accepted_at_ms": 1001000, "enforcement_changed": False}


@pytest.fixture
def client(monkeypatch):
    monkeypatch.setenv("SRE_ENABLED", "true")
    monkeypatch.setenv("KUBERNETES_SERVICE_HOST", "127.0.0.1")
    monkeypatch.setenv("KUBERNETES_SERVICE_PORT", "9446")
    client = MagicMock()
    client.post.return_value = receipt()
    monkeypatch.setattr(reports.sre_kube, "client", lambda: client)
    return client


def test_payload_redacts_before_utf8_bounding_and_keeps_failure_and_final_diagnosis():
    secret = "ghp_" + "x" * 36
    result = reports.payload(record(output="\x1b[31m" + secret + "\x1b[0m\x00\n" + "🚀" * 3000
                                    + "\nProposed solution: review node capacity"), 900, 180)
    assert len(result["text"].encode()) <= 8192
    assert secret not in result["text"]
    assert "<REDACTED_SECRET>" in result["text"]
    assert "\x1b" not in result["text"] and "\x00" not in result["text"]
    assert result["text"].startswith("Budget blocked; coverage unknown")
    assert result["text"].endswith("Proposed solution: review node capacity")
    assert result["turn_status"] == "blocked" and result["truncated"] is True
    assert set(result) == {"sweep_id", "started_at_ms", "finished_at_ms", "turn_status",
                           "text", "truncated", "interval_seconds", "timeout_seconds"}


@pytest.mark.parametrize("status", ["Some(429)", "Some(503)", "None"])
def test_admission_failure_replaces_native_provider_advice_without_changing_evidence(status):
    error = f"Governed inference budget /v1/reserve unavailable/denied (HTTP {status}); no provider fallback"
    original = record(turn_status="failed", error="", output=(
        "Azure Foundry rate-limited every one of 3 attempts. Send /retry or use hermes fallback add.\n"
        + error))
    before = dict(original)
    result = reports.payload(original, 900, 180)
    assert "Kars governed inference admission failed" in result["text"]
    assert error in result["text"]
    assert "does not establish Azure Foundry throttling" in result["text"]
    assert "coverage is incomplete" in result["text"]
    assert "No automatic funding, refund, fallback" in result["text"]
    assert "/retry" not in result["text"] and "hermes fallback add" not in result["text"]
    assert "every one of 3" not in result["text"]
    assert result["turn_status"] == "failed"
    assert original == before


@pytest.mark.parametrize("period,reset", [
    ("Daily", "at the next UTC midnight"),
    ("Monthly", "on the 1st of next month UTC"),
])
@pytest.mark.parametrize("status", ["failed", "blocked", "timeout"])
def test_calendar_denial_replaces_fallback_advice_without_claiming_success(period, reset, status):
    denial = f"{period} token budget exceeded (5346600/5000000 tokens). Resets {reset}."
    original = record(turn_status=status, error="", output=(
        "Azure Foundry rate-limited every one of 3 attempts. Send /retry or use hermes fallback add.\n"
        "Provider said: HTTP 429: " + denial))
    before = dict(original)
    result = reports.payload(original, 900, 180)
    assert result["turn_status"] == status
    assert denial in result["text"]
    assert "Kars calendar token limit blocked inference" in result["text"]
    assert "coverage is incomplete" in result["text"]
    assert "does not establish Azure Foundry throttling" in result["text"]
    assert "separately from lifetime funding" in result["text"]
    assert "operator-authorized finite daily-limit increase" in result["text"]
    assert "No automatic funding, refund, fallback" in result["text"]
    assert "/retry" not in result["text"] and "hermes fallback add" not in result["text"]
    assert "every one of 3" not in result["text"]
    assert original == before


@pytest.mark.parametrize("status,text", [
    ("finished", "Observed log: Daily token budget exceeded (5346600/5000000 tokens). "
                 "Resets at the next UTC midnight."),
    ("failed", "Upstream daily quota exhausted; HTTP 429"),
    ("failed", "Daily token budget exceeded (unknown tokens)."),
])
def test_calendar_report_does_not_reclassify_diagnoses_or_unattributed_errors(status, text):
    result = reports.payload(record(turn_status=status, error="", output=text), 900, 180)
    assert result["text"] == text
    assert result["turn_status"] == status


@pytest.mark.parametrize("status,text", [
    ("failed", "Azure Foundry returned HTTP 429"),
    ("finished", "Observed log: Governed inference budget /v1/reserve unavailable/denied "
                 "(HTTP Some(429)); no provider fallback"),
    ("failed", "Governed inference budget provider usage missing; maximum charge retained "
               "unavailable/denied (HTTP None); no provider fallback"),
])
def test_report_does_not_reclassify_upstream_or_successful_diagnostics_or_settlement(status, text):
    assert reports.payload(record(turn_status=status, error="", output=text), 900, 180)["text"] == text


@pytest.mark.parametrize("raw,attribution", [
    ("Send /retry. Governed inference budget /v1/reserve unavailable/denied "
     "(HTTP Some(429)); no provider fallback", "Kars governed inference admission failed"),
    ("Send /retry. Provider said: HTTP 429: Daily token budget exceeded (5346600/5000000 tokens). "
     "Resets at the next UTC midnight.", "Kars calendar token limit blocked inference"),
])
def test_notifications_share_corrected_admission_report_and_keep_raw_local_output(
        client, monkeypatch, tmp_path, raw, attribution):
    monkeypatch.setattr(watcher, "_preflight", lambda _: [])
    monkeypatch.setattr(watcher, "_run_hermes", lambda *args: {"turn_status": "failed", "output": raw})
    monkeypatch.setattr(watcher, "_notify_target", lambda: "telegram:test")
    notify = MagicMock()
    monkeypatch.setattr(watcher, "_notify", notify)
    result = watcher._sweep(tmp_path, 180)
    assert result["output"] == raw
    assert attribution in notify.call_args.args[1]
    assert "/retry" not in notify.call_args.args[1]
    assert attribution in client.post.call_args.kwargs["json"]["text"]
    assert client.post.call_args.kwargs["json"]["turn_status"] == "failed"
    assert "/retry" in (tmp_path / "sre-latest-report.json").read_text()


def test_empty_output_is_explicitly_unverified():
    result = reports.payload(record(error="", output="", turn_status="finished"), 900, 180)
    assert "unverified" in result["text"]
    assert result["turn_status"] == "finished"


def test_budget_blocked_report_needs_no_inference_readiness_or_proposal(client, monkeypatch):
    inference = MagicMock(side_effect=AssertionError("reporting cannot call inference"))
    monkeypatch.setattr(watcher.router_client, "call", inference)
    assert reports.publish(record(), 900, 180) == receipt()
    client.post.assert_called_once_with("/reports/latest", json=reports.payload(record(), 900, 180))
    client.get.assert_not_called()
    inference.assert_not_called()


@pytest.mark.parametrize("status", [None, 500, 503, 409, 401, 403, 413])
def test_retry_reuses_exact_report_and_only_retries_transport_or_server_errors(client, status):
    request = httpx.Request("POST", "https://127.0.0.1:9446/reports/latest")
    failure = (httpx.ConnectError("unreachable", request=request) if status is None else
               httpx.HTTPStatusError("failed", request=request, response=httpx.Response(status, request=request)))
    client.post.side_effect = [failure, receipt()]
    if status is None or status >= 500:
        assert reports.publish(record(), 900, 180) == receipt()
        assert client.post.call_count == 2
        assert client.post.call_args_list[0] == client.post.call_args_list[1]
    else:
        with pytest.raises(httpx.HTTPStatusError):
            reports.publish(record(), 900, 180)
        client.post.assert_called_once()


@pytest.mark.parametrize("field,value", [("persisted", False), ("enforcement_changed", True),
    ("sweep_id", "other"), ("accepted_at_ms", True), ("accepted_at_ms", -1), ("digest", "bad")])
def test_invalid_receipt_never_claims_persistence(client, field, value):
    client.post.return_value = {**receipt(), field: value}
    with pytest.raises(RuntimeError, match="acknowledgement"):
        reports.publish(record(), 900, 180)


@pytest.mark.parametrize("name,value", [("SRE_ENABLED", "false"),
    ("KUBERNETES_SERVICE_HOST", "10.0.0.1"), ("KUBERNETES_SERVICE_PORT", "443")])
def test_projection_must_be_registered_private_facade(client, monkeypatch, name, value):
    monkeypatch.setenv(name, value)
    with pytest.raises(RuntimeError, match="projection"):
        reports.publish(record(), 900, 180)
    client.post.assert_not_called()


def test_failed_publication_keeps_local_failed_diagnostic_evidence(client, monkeypatch, tmp_path):
    monkeypatch.setattr(watcher, "_preflight", lambda _: [])
    monkeypatch.setattr(watcher, "_run_hermes", lambda *args: {
        "turn_status": "failed", "error": "Read denied", "output": "Node state unknown"})
    monkeypatch.setattr(watcher, "_notify_target", lambda: None)
    client.post.side_effect = RuntimeError("private transport details")
    result = watcher._sweep(tmp_path, 180)
    assert result["turn_status"] == "failed"
    assert result["output"] == "Node state unknown"
    assert "publication" not in result
    assert "not confirmed persisted" in result["publication_error"]
    assert "private transport details" not in (tmp_path / "sre-latest-report.json").read_text()
    assert "Node state unknown" in (tmp_path / "sre-latest-report.json").read_text()
