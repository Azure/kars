# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Human review and continuation through the maintained router transport."""

import json

import httpx
import pytest

from kars_runtime_hermes.plugin import access_request as access
from kars_runtime_hermes.plugin import http_fetch

SCOPE = "scope-1"
ENTRY = {"scope_id": SCOPE, "request_id": "request-1", "kind": "clarification", "status": "pending"}
ARGS = {"operation": "request", "kind": "clarification", "target": "source", "reason": "Which source?"}


def exchange_for(decision, status=200):
    calls = []

    def request(method, path, **kwargs):
        calls.append((method, path, kwargs))
        if path == "/v1/access-requests":
            return httpx.Response(200, json={"scope": {"id": SCOPE}, "requests": []})
        if path == "/v1/access-request":
            return httpx.Response(202, json={"status": "queued", "request": ENTRY})
        return httpx.Response(status, json=decision)

    return request, calls


def test_request_waits_for_answer_and_preserves_scope():
    request, calls = exchange_for({**ENTRY, "status": "approved", "answer": "Internal report"})
    result = access.exchange(ARGS, request=request)
    assert result["request"]["answer"] == "Internal report"
    assert result["enforcement_changed"] is False
    assert [call[1] for call in calls] == [
        "/v1/access-requests",
        "/v1/access-request",
        "/v1/access-requests/request-1/wait",
    ]
    assert calls[-1][2]["params"] == {"scope_id": SCOPE, "timeout_ms": 300000}
    assert calls[-1][2]["timeout"] > 300
    assert calls[-1][2]["headers"] == {"x-kars-service-scope": SCOPE}


@pytest.mark.parametrize("status", ["denied", "cancelled", "expired", "dispatch_claimed"])
def test_terminal_decisions_do_not_grant_authority(status):
    request, _ = exchange_for({**ENTRY, "status": status})
    assert access.exchange(ARGS, scope=SCOPE, request=request) == {
        "request": {**ENTRY, "status": status},
        "enforcement_changed": False,
    }


@pytest.mark.parametrize("answer", [None, "", " ", "🔥" * 2049])
def test_invalid_clarification_answer_is_not_success(answer):
    request, _ = exchange_for({**ENTRY, "status": "approved", "answer": answer})
    assert "no valid answer" in access.exchange(ARGS, scope=SCOPE, request=request)["error"]


@pytest.mark.parametrize("error,status", [("wait_timed_out", 408), ("stale_scope", 409), ("request_expired", 410)])
def test_wait_error_preserves_identity_for_explicit_continuation(error, status):
    request, calls = exchange_for({"error": error}, status)
    result = access.exchange(ARGS, scope=SCOPE, request=request)
    assert result == {
        "error": error,
        "http_status": status,
        "scope_id": SCOPE,
        "request_id": ENTRY["request_id"],
        "request": ENTRY,
        "enforcement_changed": False,
    }
    request, calls = exchange_for({**ENTRY, "status": "approved", "answer": "Report"})
    resumed = access.exchange(
        {"operation": "wait", "scope_id": result["scope_id"], "request_id": result["request_id"]}, request=request
    )
    assert resumed["request"]["answer"] == "Report"
    assert len(calls) == 1 and calls[0][0] == "GET"


@pytest.mark.parametrize("bound", [None, SCOPE])
def test_observed_egress_request_uses_active_scope_and_only_egress_fields(bound):
    request, calls = exchange_for({**ENTRY, "kind": "egress", "status": "approved"})
    args = {
        "operation": "request",
        "kind": "egress",
        "target": "example.net",
        "reason": "Review page access",
        "tier": 1,
        "port": 443,
        "scope_id": "",
        "request_id": "",
        "timeout_ms": 300000,
    }
    result = access.exchange(args, scope=bound, request=request)
    posted = next(call[2]["json"] for call in calls if call[0] == "POST")
    assert posted == {
        "kind": "egress",
        "target": "example.net",
        "reason": "Review page access",
        "port": 443,
        "scope_id": SCOPE,
    }
    assert result["enforcement_changed"] is False
    assert args["scope_id"] == "" and args["tier"] == 1


@pytest.mark.parametrize("operation", ["request", "list", "wait", "cancel"])
def test_nonempty_stale_scope_never_dispatches(operation):
    request, calls = exchange_for({})
    with pytest.raises(ValueError, match="stale_scope"):
        access.exchange(
            {**ARGS, "operation": operation, "scope_id": "old", "request_id": "request-1"}, scope=SCOPE, request=request
        )
    assert not calls


@pytest.mark.parametrize("operation", ["wait", "cancel"])
@pytest.mark.parametrize("field", ["scope_id", "request_id"])
def test_empty_resume_identity_never_rebinds(operation, field):
    request, calls = exchange_for({})
    args = {"operation": operation, "scope_id": SCOPE, "request_id": "request-1", field: ""}
    with pytest.raises(ValueError, match="original scope_id and request_id"):
        access.exchange(args, scope=SCOPE, request=request)
    assert not calls


@pytest.mark.parametrize("scope", [None, False, 0, " ", "scope/other"])
def test_invalid_supplied_scope_never_dispatches(scope):
    request, calls = exchange_for({})
    with pytest.raises(ValueError, match="scope_id"):
        access.exchange({**ARGS, "scope_id": scope}, scope=SCOPE, request=request)
    assert not calls


def test_empty_list_scope_uses_active_scope():
    request, calls = exchange_for({})
    access.exchange({"operation": "list", "scope_id": ""}, scope=SCOPE, request=request)
    assert len(calls) == 1 and calls[0][2]["headers"] == {"x-kars-service-scope": SCOPE}


@pytest.mark.parametrize("kind", ["clarification", "tool", "skill", "mcp", "command", "permission", "tier"])
def test_request_kind_selects_applicable_authority_fields(kind):
    request, calls = exchange_for({})
    access.exchange(
        {**ARGS, "kind": kind, "target": "" if kind == "tier" else "resource", "tier": 5, "port": 443, "timeout_ms": 0},
        scope=SCOPE,
        request=request,
    )
    posted = calls[0][2]["json"]
    assert ("tier" in posted) == (kind == "tier")
    assert "port" not in posted


def test_stale_assignment_never_dispatches():
    request, calls = exchange_for({})
    with pytest.raises(ValueError, match="stale_scope"):
        access.exchange(
            {"operation": "wait", "scope_id": "old", "request_id": "request-1"}, scope=SCOPE, request=request
        )
    assert not calls


def test_cancel_preserves_failure_and_original_identity():
    request, calls = exchange_for({"error": "request_terminal"}, 409)
    result = access.exchange({"operation": "cancel", "scope_id": SCOPE, "request_id": "request-1"}, request=request)
    assert result["error"] == "request_terminal"
    assert result["enforcement_changed"] is False
    assert calls[0][2]["json"] == {"scope_id": SCOPE}


def test_cancel_transport_failure_preserves_identity():
    def request(*args, **kwargs):
        raise httpx.ReadTimeout("timeout")

    result = access.exchange({"operation": "cancel", "scope_id": SCOPE, "request_id": "request-1"}, request=request)
    assert result == {"error": "timeout", "scope_id": SCOPE, "request_id": "request-1", "enforcement_changed": False}


def test_tier_approval_does_not_claim_installed_autonomy():
    request, _ = exchange_for({**ENTRY, "kind": "tier", "status": "approved", "tier": 5})
    result = access.exchange(
        {"operation": "request", "kind": "tier", "tier": 5, "reason": "Review more autonomy"},
        scope=SCOPE,
        request=request,
    )
    assert result["request"]["status"] == "approved" and result["enforcement_changed"] is False


def test_queue_only_does_not_wait():
    request, calls = exchange_for({})
    assert access.exchange({**ARGS, "timeout_ms": 0}, scope=SCOPE, request=request)["status"] == "queued"
    assert len(calls) == 1


def test_ordinary_fetch_wait_binds_current_scope_and_preserves_denial(monkeypatch):
    calls = []

    def request(method, path, **kwargs):
        calls.append((method, path, kwargs))
        if path == "/v1/access-requests":
            return httpx.Response(
                200, request=httpx.Request(method, "http://localhost" + path), json={"scope": {"id": SCOPE}}
            )
        return httpx.Response(403, json={"error": "approval_pending", "request": ENTRY, "enforcement_changed": False})

    monkeypatch.setattr(http_fetch.router_client, "call", request)
    result = json.loads(http_fetch._http_fetch({"url": "https://example.com", "wait_for_approval_ms": 300000}))
    assert result == {"error": "approval_pending", "http_status": 403, "request": ENTRY, "enforcement_changed": False}
    assert calls[-1][2]["json"]["scope_id"] == SCOPE
    assert calls[-1][2]["json"]["wait_for_approval_ms"] == 300000
    assert calls[-1][2]["timeout"] > 300
