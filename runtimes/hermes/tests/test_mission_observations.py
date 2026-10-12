# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json

import httpx
import pytest

from kars_runtime_hermes.plugin.mission_observations import ReturnedRouterResponses


def selection():
    return dict(scope_id="router.scope-1", rounds=[2, 3], coverage="returned-responses-only",
                durable=False, missing_rounds=[], dropped_events=0, truncated=False, events=[
                    dict(kind="round", scope_id="router.scope-1", round=2, seq=1, source="router-observed",
                         observed_at="2026-10-12T12:00:00Z", model="model", http_status=200,
                         usage=dict(prompt_tokens=10, completion_tokens=5, total_tokens=15, secret="drop"),
                         prompt="must never be forwarded"),
                    dict(kind="tool_result", scope_id="router.scope-1", round=2, reported_in_round=3,
                         call_id="call-1", name="kars_mission_read", ok=True, source="harness-reported",
                         result_summary=dict(kind="file_read", bytes=4, returned_bytes=4, truncated=False,
                                             content="must never be forwarded"))])


def recorder(trace=None, status=200):
    requests = []

    def handle(request):
        requests.append(request)
        assert str(request.url) == "http://127.0.0.1:8443/telemetry/trace?rounds=2,3"
        assert request.headers["x-kars-service-scope"] == "router.scope-1"
        assert request.method == "GET" and request.extensions["timeout"]["read"] == 2
        return httpx.Response(status, json=selection() if trace is None else trace)

    value = ReturnedRouterResponses(httpx.MockTransport(handle))
    for number in (2, 3):
        value.capture(httpx.Headers({"x-kars-telemetry-scope": "router.scope-1",
                                     "x-kars-telemetry-round": str(number)}))
    return value, requests


def test_only_returned_correlated_router_fields_are_forwarded():
    value, requests = recorder()
    content = value.artifact()
    output = json.loads(content)
    assert output["state"] == "observed" and output["responses"] == 2
    assert output["rounds"] == [2, 3] and output["trace"]["durable"] is False
    assert output["trace"]["events"][1]["source"] == "harness-reported"
    assert output["trace"]["events"][1]["result_summary"]["bytes"] == 4
    assert not any(v in content for v in ("must never", "secret", "prompt\""))
    assert len(requests) == 1


def test_unadvertised_router_does_not_trigger_ambient_queries():
    value = ReturnedRouterResponses(httpx.MockTransport(lambda _: pytest.fail("Unexpected request")))
    value.capture(httpx.Headers())
    assert value.artifact() is None


@pytest.mark.parametrize("scope,number", [
    ("different", "4"), ("router.scope-1", "2"), ("router.scope-1", "0"),
    ("router.scope-1", "1.5"), ("router.scope-1", "9007199254740992"),
    ("router.scope-1", None), (None, "4"), ("bad scope", "4")])
def test_changed_or_invalid_correlation_is_explicitly_unavailable(scope, number):
    value, requests = recorder()
    value.capture(httpx.Headers({k: v for k, v in (
        ("x-kars-telemetry-scope", scope), ("x-kars-telemetry-round", number)) if v is not None}))
    output = json.loads(value.artifact())
    assert output["state"] == "unavailable" and "correlation" in output["reason"]
    assert not requests


@pytest.mark.parametrize("change", [
    {"scope_id": "different"}, {"rounds": [2]}, {"durable": True}, {"coverage": "complete"},
    {"missing_rounds": [9]}, {"dropped_events": True}, {"truncated": 0},
    {"events": [{"kind": "round", "round": 5, "scope_id": "router.scope-1"}]},
    {"events": [{"kind": "tool_result", "round": 2, "scope_id": "router.scope-1", "reported_in_round": 9}]},
    {"events": [None]}, {"events": [{}] * 257},
])
def test_mismatched_selection_never_becomes_observed(change):
    trace = selection()
    trace.update(copy.deepcopy(change))
    value, _ = recorder(trace)
    assert json.loads(value.artifact())["state"] == "unavailable"


def test_reset_http_failure_and_oversized_body_remain_explicit():
    value, _ = recorder(status=503)
    assert json.loads(value.artifact())["state"] == "unavailable"
    trace = selection()
    trace["extra"] = "x" * (48 * 1024)
    value, _ = recorder(trace)
    assert json.loads(value.artifact())["state"] == "unavailable"
