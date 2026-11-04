# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path

from kars_runtime_hermes.plugin import mission_protocol as wire

import httpx
import pytest

from kars_runtime_hermes.plugin.mission_observations import ReturnedRouterResponses


@pytest.mark.parametrize("name,arguments", [
    ("kars_mesh_send", {"to_agent": "peer", "content": "message"}),
    ("kars_discover", {"name": "peer"}),
    ("kars_mesh_inbox", {}),
    ("kars_mesh_await", {"senders": ["peer"], "timeout_seconds": .1})])
def test_peer_authorization_keeps_router_scope_and_exact_arguments(name, arguments):
    requests = []

    def policy(request):
        requests.append(json.loads(request.content))
        return httpx.Response(200, json={"allowed": True})

    value = ReturnedRouterResponses(httpx.MockTransport(policy))
    value.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1", "x-kars-telemetry-round": "1"}))
    assert value.authorize_tool("call-1", name, arguments)
    assert requests == [{"action": "tool:" + name, "context": {
        "tool_call_id": "call-1", "tool": name, "arguments": arguments, "scope_id": "scope-1"}}]
    assert not value.authorize_tool("call-2", name, {**arguments, "assignmentId": "other"})
    assert len(requests) == 1


def selection():
    return dict(scope_id="router.scope-1", rounds=[2, 3], coverage="returned-responses-only",
                durable=False, missing_rounds=[], dropped_events=0, truncated=False, events=[
                    dict(kind="round", scope_id="router.scope-1", round=2, seq=1, source="router-observed",
                         observed_at="2026-10-12T12:00:00Z", model="model", http_status=200,
                         usage=dict(prompt_tokens=10, completion_tokens=5, total_tokens=15, secret="drop"),
                         prompt="must never be forwarded"),
                    dict(kind="tool_result", scope_id="router.scope-1", round=2, reported_in_round=3, seq=2,
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
        body = copy.deepcopy(selection() if trace is None else trace)
        cursor = body["events"][-1].get("seq", 0) if body.get("events") and isinstance(body["events"][-1], dict) else 0
        return httpx.Response(status, json={"cursor": cursor, "high_water": cursor, "has_more": False, **body})

    value = ReturnedRouterResponses(httpx.MockTransport(handle))
    for number in (2, 3):
        value.capture(httpx.Headers({"x-kars-telemetry-scope": "router.scope-1",
                                     "x-kars-telemetry-round": str(number)}))
    return value, requests


def ordinary_observations(mutate=None, previews=False):
    cycle = json.loads((Path(__file__).resolve().parents[3] / "inference-router/tests/fixtures/router-observation-tool-cycle.json").read_text())
    events = []
    for number in range(1, 129):
        for template in cycle:
            if template["kind"] == "tool_result" and "reported_in_round" not in template:
                continue
            event = {**template, "round": number, "seq": (number - 1) * 7 + template["seq"]}
            if "call_id" in event:
                event["call_id"] = f"call-{number}"
            if "reported_in_round" in event:
                event["reported_in_round"] = number + 1
            if previews and event["kind"] == "tool_proposed":
                event["argument_preview"] = dict(text="Useful details " * 28, truncated=False, redacted=False)
            events.append(event)
    events.append({**cycle[0], "round": 129, "seq": 897, "tool_calls_observed": 0, "finish_reason": "stop"})
    requests = []

    def handle(request):
        requests.append(request)
        assert request.headers["x-kars-service-scope"] == "ordinary-phase-scope"
        assert request.url.params["rounds"] == ",".join(map(str, range(1, 130)))
        cursor = int(request.url.params.get("after", "0"))
        page = [e for e in events if e["seq"] > cursor][:256]
        body = dict(scope_id="ordinary-phase-scope", rounds=list(range(1, 130)), coverage="returned-responses-only",
                    durable=False, missing_rounds=[], dropped_events=0, truncated=False,
                    events=copy.deepcopy(page), cursor=page[-1]["seq"], high_water=897, has_more=page[-1]["seq"] < 897)
        if cursor and mutate:
            mutate(body)
        return httpx.Response(200, json=body)

    value = ReturnedRouterResponses(httpx.MockTransport(handle))
    for number in range(1, 130):
        value.capture(httpx.Headers({"x-kars-telemetry-scope": "ordinary-phase-scope", "x-kars-telemetry-round": str(number)}))
    return value, requests


@pytest.mark.parametrize("corruption", [None, "call_id", "name", "round", "null", "unknown_outcome", "missing_proposal"])
def test_immediate_results_require_a_selected_prior_proposal(corruption):
    trace = selection()
    result = trace["events"][-1]
    result.pop("reported_in_round")
    result["seq"] = 3
    trace["events"].insert(1, dict(kind="tool_proposed", scope_id="router.scope-1", round=2, seq=2,
        call_id="call-1", name="kars_mission_read", ok=None, source="model-proposed"))
    if corruption in ("call_id", "name"):
        result[corruption] = "unrelated"
    elif corruption == "round":
        result["round"] = 3
    elif corruption == "null":
        result["reported_in_round"] = None
    elif corruption == "unknown_outcome":
        result["ok"] = None
    elif corruption == "missing_proposal":
        trace["events"].pop(1)
    value, _ = recorder(trace)
    artifact = json.loads(value.artifact())
    assert artifact["state"] == ("unavailable" if corruption else "observed")
    if not corruption:
        assert artifact["trace"]["events"][-1]["ok"] is True
        assert "reported_in_round" not in artifact["trace"]["events"][-1]


def test_router_argument_previews_are_forwarded_without_raw_payloads():
    trace = selection()
    trace["events"].insert(1, dict(kind="tool_proposed", scope_id="router.scope-1", round=2, seq=2,
        call_id="call-1", name="kars_mission_read", ok=None, source="model-proposed",
        argument_preview=dict(text='{"path":"/sandbox/report.md"}', redacted=False, truncated=False),
        raw_arguments="must never be forwarded"))
    trace["events"][2]["seq"] = 3
    value, _ = recorder(trace)
    content = value.artifact()
    assert json.loads(content)["trace"]["events"][1]["argument_preview"] == trace["events"][1]["argument_preview"]
    assert "must never be forwarded" not in content
    for invalid in (None, {"text": "🦀" * 513, "truncated": False, "redacted": False},
                    {"text": "x", "truncated": False, "redacted": "true"},
                    {"text": "x", "truncated": False, "redacted": True, "raw": "hidden"}):
        trace["events"][1]["argument_preview"] = invalid
        value, _ = recorder(trace)
        assert json.loads(value.artifact())["state"] == "unavailable"


@pytest.mark.parametrize("previews", [False, True])
def test_ordinary_128_tools_export_four_pages_with_reporting_rounds_and_bounded_artifact(previews):
    value, requests = ordinary_observations(previews=previews)
    content = value.artifact()
    output = wire.loads(content, wire.MAX_ACTIVITY, max_nodes=wire.MAX_OBSERVATION_EVENTS * 64)
    assert output["state"] == "observed" and output["responses"] == 129
    assert len(output["trace"]["events"]) == 769 and len(requests) == 4
    assert len([e for e in output["trace"]["events"] if e["kind"] == "tool_result"]) == 128
    assert len([e for e in output["trace"]["events"] if "argument_preview" in e]) == (128 if previews else 0)
    assert all(e["reported_in_round"] == e["round"] + 1 for e in output["trace"]["events"] if e["kind"] == "tool_result")
    assert 128 * 1024 < len(content.encode()) < wire.MAX_ACTIVITY
    assert wire.artifacts({wire.OBSERVATION_ARTIFACT: content, "report.md": "x" * (120 * 1024)})


@pytest.mark.parametrize("mutate", [
    lambda page: page.update(scope_id="reset"),
    lambda page: page.update(high_water=896),
    lambda page: page.update(cursor=0),
    lambda page: page.update(dropped_events=1),
    lambda page: page.update(truncated=True),
    lambda page: page["events"][0].update(seq=1),
    lambda page: page["events"][0].update(scope_id="other"),
    lambda page: page["events"][0].update(round=130),
])
def test_later_selected_page_errors_never_become_observed(mutate):
    value, requests = ordinary_observations(mutate)
    assert json.loads(value.artifact())["state"] == "unavailable"
    assert 2 <= len(requests) <= 4


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


def test_selected_router_operations_keep_identity_flags_and_host_not_raw_payloads():
    trace = selection()
    trace["events"].append(dict(kind="router_operation", scope_id="router.scope-1", round=2,
        seq=4, call_id="call-1", name="http_fetch", source="router", operation="egress_fetch",
        flag="response", allowed=None, http_status=403, complete=True,
        destination_host="source.example", destination_port=443, ms=12,
        url="https://source.example/secret?token=private", headers={"Authorization": "private"}, body="private"))
    value, _ = recorder(trace)
    content = value.artifact()
    output = json.loads(content)
    assert output["state"] == "observed"
    event = output["trace"]["events"][-1]
    assert event["flag"] == "response" and event["http_status"] == 403
    assert event["allowed"] is None and event["call_id"] == "call-1"
    assert event["destination_host"] == "source.example"
    assert not any(v in content for v in ("private", "secret", "Authorization"))


def test_observation_retention_does_not_revoke_authorized_long_running_work():
    requests = []

    def policy(request):
        requests.append(request)
        return httpx.Response(200, json={"allowed": True})

    value = ReturnedRouterResponses(httpx.MockTransport(policy))
    for number in range(1, 132):
        value.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1",
                                    "x-kars-telemetry-round": str(number)}))
    assert not value.invalid
    assert len(value.rounds) == 130
    output = json.loads(value.artifact())
    assert output["state"] == "unavailable" and output["responses"] == 131
    assert output["rounds"] == list(range(1, 131))
    assert not requests
    assert value.authorize_tool("call-129", "write_file", {"path": "exports/report.md", "content": "Report"})
    assert len(requests) == 1
    value.capture(httpx.Headers({"x-kars-telemetry-scope": "changed",
                                "x-kars-telemetry-round": "130"}))
    assert value.invalid
    assert not value.authorize_tool("call-130", "write_file", {})


@pytest.mark.parametrize("number", [1, 32, 33, 130, 131])
def test_round_correlation_stays_monotonic_after_retention_window(number):
    value = ReturnedRouterResponses(httpx.MockTransport(lambda _: pytest.fail("Unexpected request")))
    for current in range(1, 132):
        value.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1",
                                    "x-kars-telemetry-round": str(current)}))
    assert not value.invalid
    value.capture(httpx.Headers({"x-kars-telemetry-scope": "scope-1", "x-kars-telemetry-round": str(number)}))
    assert value.invalid
    assert not value.authorize_tool("call-late", "write_file", {})


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
    trace["extra"] = "x" * (256 * 1024)
    value, _ = recorder(trace)
    assert json.loads(value.artifact())["state"] == "unavailable"
