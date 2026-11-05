# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Forward only the shared router recorder's returned-response selection."""

import re

import httpx

from . import mission_protocol as wire
from .mission_peer import SCHEMAS as PEER_SCHEMAS, validate_call

ARTIFACT = "kars-router-observations.json"
LIMIT = wire.MAX_ACTIVITY


class ReturnedRouterResponses:
    def __init__(self, transport):
        self.transport = transport
        self.responses = 0
        self.scope = None
        self.rounds = set()
        self.last_round = 0
        self.advertised = self.invalid = self.truncated = False

    def capture(self, headers):
        self.responses += 1
        scope, number = headers.get("x-kars-telemetry-scope"), headers.get("x-kars-telemetry-round")
        if scope is None and number is None:
            return
        self.advertised = True
        if (
            not isinstance(scope, str)
            or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", scope)
            or not isinstance(number, str)
            or not re.fullmatch(r"[1-9][0-9]{0,15}", number)
        ):
            self.invalid = True
            return
        number = int(number)
        if not wire.integer(number, 1) or number <= self.last_round or (self.scope is not None and scope != self.scope):
            self.invalid = True
            return
        self.scope = scope
        self.last_round = number
        if len(self.rounds) >= wire.MAX_OBSERVATION_ROUNDS:
            self.truncated = True
        else:
            self.rounds.add(number)

    def authorize_tool(self, call_id, name, arguments=None):
        if (
            self.invalid
            or name
            not in (
                "kars_mission_read",
                "kars_mission_write",
                "http_fetch",
                "read_file",
                "write_file",
                "search_files",
                "foundry_memory",
                "kars_access_request",
                "github_pr",
                "python_execute",
                "kars_mcp_list",
                "kars_mcp_call",
                *PEER_SCHEMAS,
            )
            or not isinstance(call_id, str)
            or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", call_id)
        ):
            return False
        if name in PEER_SCHEMAS:
            try:
                validate_call(name, arguments)
            except (TypeError, ValueError):
                return False
        context = {"tool_call_id": call_id}
        if name in (
            "http_fetch",
            "read_file",
            "write_file",
            "search_files",
            "foundry_memory",
            "kars_access_request",
            "github_pr",
            "python_execute",
            "kars_mcp_list",
            "kars_mcp_call",
            *PEER_SCHEMAS,
        ):
            if not isinstance(arguments, dict):
                return False
            context.update(tool=name, arguments=arguments)
        if self.scope is not None:
            context["scope_id"] = self.scope
        request = httpx.Request(
            "POST",
            "http://127.0.0.1:8443/agt/evaluate",
            json={"action": "tool:" + name, "context": context},
            extensions={"timeout": dict(connect=2, read=2, write=2, pool=2)},
        )
        try:
            response = self.transport.handle_request(request)
            try:
                wire.require(response.status_code == 200, "Tool authorization refused")
                body = bytearray()
                for chunk in response.iter_bytes():
                    body.extend(chunk)
                    wire.require(len(body) <= 65536, "Tool authorization too large")
                decision = wire.loads(bytes(body), 65536)
                return isinstance(decision, dict) and decision.get("allowed") is True
            finally:
                response.close()
        except (httpx.HTTPError, ValueError, TypeError, UnicodeError):
            return False

    def report_tool(self, call_id, ok, latency_ms, summary):
        if (
            self.invalid
            or self.scope is None
            or not isinstance(call_id, str)
            or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", call_id)
        ):
            return False
        request = httpx.Request(
            "POST",
            "http://127.0.0.1:8443/telemetry/tool",
            json=dict(
                scope_id=self.scope,
                call_id=call_id,
                ok=ok,
                latency_ms=min(3_600_000, max(0, round(latency_ms))),
                result_summary=summary,
            ),
            extensions={"timeout": dict(connect=2, read=2, write=2, pool=2)},
        )
        try:
            response = self.transport.handle_request(request)
            try:
                wire.require(response.status_code == 200, "Tool report refused")
                body = bytearray()
                for chunk in response.iter_bytes():
                    body.extend(chunk)
                    wire.require(len(body) <= 4096, "Tool receipt too large")
                receipt = wire.loads(bytes(body), 4096)
                return (
                    isinstance(receipt, dict)
                    and receipt.get("recorded") is True
                    and receipt.get("source") == "harness-reported"
                    and receipt.get("enforcement_changed") is False
                )
            finally:
                response.close()
        except (httpx.HTTPError, ValueError, TypeError, UnicodeError):
            return False

    def selection(self, rounds):
        first, cursor, high_water, events = None, 0, 0, []
        for _ in range(wire.MAX_OBSERVATION_EVENTS // 256):
            suffix = "&after=" + str(cursor) if cursor else ""
            request = httpx.Request(
                "GET",
                "http://127.0.0.1:8443/telemetry/trace?rounds=" + ",".join(map(str, rounds)) + suffix,
                headers={"x-kars-service-scope": self.scope},
                extensions={"timeout": dict(connect=2, read=2, write=2, pool=2)},
            )
            response = self.transport.handle_request(request)
            try:
                wire.require(response.status_code == 200, "Observation read failed")
                body = bytearray()
                for chunk in response.iter_bytes():
                    body.extend(chunk)
                    wire.require(len(body) <= wire.MAX_OBSERVATION_PAGE, "Observation response too large")
                trace = wire.loads(bytes(body), wire.MAX_OBSERVATION_PAGE)
            finally:
                response.close()
            wire.require(
                isinstance(trace, dict)
                and trace.get("scope_id") == self.scope
                and trace.get("coverage") == "returned-responses-only"
                and trace.get("durable") is False
                and trace.get("rounds") == rounds
                and wire.integer(trace.get("cursor"), cursor)
                and wire.integer(trace.get("high_water"), trace["cursor"])
                and trace.get("has_more") is (trace["cursor"] < trace["high_water"])
                and type(trace.get("events")) is list
                and len(trace["events"]) <= 256,
                "Invalid selection page",
            )
            if first is None:
                first, high_water = trace, trace["high_water"]
            wire.require(
                trace["high_water"] >= high_water
                and all(trace.get(key) == first.get(key) for key in ("dropped_events", "missing_rounds", "truncated")),
                "Selection changed during pagination",
            )
            for event in trace["events"]:
                wire.require(
                    isinstance(event, dict) and wire.integer(event.get("seq"), cursor + 1, trace["cursor"]),
                    "Invalid selection sequence",
                )
                cursor = event["seq"]
                if cursor <= high_water:
                    events.append(event)
            wire.require(
                cursor == trace["cursor"] and (cursor >= high_water or trace["events"]), "Selection did not advance"
            )
            if cursor >= high_water:
                return {**first, "events": events}
        raise ValueError("Selection exceeds recorder retention")

    def artifact(self):
        if not self.advertised:
            return None
        rounds = sorted(self.rounds)
        base = dict(
            version=1,
            source="runtime-forwarded-router-observations",
            coverage="returned-responses-only",
            responses=self.responses,
            scope_id=self.scope,
            rounds=rounds,
        )

        def unavailable(reason):
            return wire.encoded({**base, "state": "unavailable", "reason": reason}).decode()

        if self.invalid or self.scope is None:
            return unavailable("Response correlation changed or was invalid")
        if self.truncated:
            return unavailable("Router selection exceeded the observation artifact limit")
        try:
            trace = self.selection(rounds)
            wire.require(
                isinstance(trace, dict)
                and trace.get("scope_id") == self.scope
                and type(trace.get("rounds")) is list
                and all(wire.integer(n, 1) for n in trace["rounds"])
                and trace["rounds"] == rounds
                and trace.get("coverage") == base["coverage"]
                and trace.get("durable") is False
                and type(trace.get("events")) is list
                and len(trace["events"]) <= wire.MAX_OBSERVATION_EVENTS
                and type(trace.get("missing_rounds")) is list
                and all(wire.integer(n, 1) and n in rounds for n in trace["missing_rounds"])
                and wire.integer(trace.get("dropped_events"))
                and type(trace.get("truncated")) is bool,
                "Router selection did not match returned response IDs",
            )
            events = []
            for event in trace["events"]:
                wire.require(
                    isinstance(event, dict)
                    and event.get("scope_id") == self.scope
                    and wire.integer(event.get("round"), 1)
                    and event["round"] in rounds
                    and (
                        event.get("kind") in ("round", "tool_proposed", "router_operation")
                        or (
                            event.get("kind") == "tool_result"
                            and (
                                (
                                    wire.integer(event.get("reported_in_round"), 1)
                                    and event["reported_in_round"] > event["round"]
                                    and event["reported_in_round"] in rounds
                                )
                                or (
                                    "reported_in_round" not in event
                                    and event.get("source") == "harness-reported"
                                    and type(event.get("ok")) is bool
                                    and any(
                                        proposal.get("kind") == "tool_proposed"
                                        and proposal.get("round") == event["round"]
                                        and proposal.get("call_id") == event.get("call_id")
                                        and proposal.get("name") == event.get("name")
                                        for proposal in events
                                    )
                                )
                            )
                        )
                    ),
                    "Router event did not match returned response IDs",
                )
                fields = (
                    (
                        "provider",
                        "model",
                        "http_status",
                        "accepted",
                        "outcome",
                        "usage_state",
                        "finish_reason",
                        "partial_observation",
                        "ms",
                        "tool_calls_observed",
                    )
                    if event["kind"] == "round"
                    else ("call_id", "name", "ok", "reported_in_round", "ms")
                )
                if event["kind"] == "router_operation":
                    fields = (
                        "call_id",
                        "name",
                        "operation",
                        "flag",
                        "allowed",
                        "http_status",
                        "complete",
                        "destination_host",
                        "destination_port",
                        "ms",
                    )
                selected = {
                    k: event[k]
                    for k in ("kind", "round", "scope_id", "seq", "source", "observed_at", *fields)
                    if k in event
                }
                if event["kind"] == "round" and isinstance(event.get("usage"), dict):
                    selected["usage"] = {
                        k: event["usage"].get(k) for k in ("prompt_tokens", "completion_tokens", "total_tokens")
                    }
                if event["kind"] == "tool_proposed" and "argument_preview" in event:
                    preview = event["argument_preview"]
                    wire.require(
                        isinstance(preview, dict)
                        and set(preview) == {"text", "truncated", "redacted"}
                        and isinstance(preview["text"], str)
                        and 0 < len(preview["text"].encode()) <= 2048
                        and not re.search(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]", preview["text"])
                        and type(preview["truncated"]) is bool
                        and type(preview["redacted"]) is bool,
                        "Invalid tool argument preview",
                    )
                    selected["argument_preview"] = dict(preview)
                if event["kind"] == "tool_result" and isinstance(event.get("result_summary"), dict):
                    summary = event["result_summary"]
                    fields = {
                        "file_read": ("kind", "bytes", "returned_bytes", "truncated"),
                        "file_write": ("kind", "bytes", "artifact_name"),
                        "python_execution": ("kind", "bytes", "receipt_sha256", "exit_code", "reason"),
                        "failure": ("kind", "reason"),
                    }.get(summary.get("kind"))
                    wire.require(fields is not None, "Unknown tool summary")
                    selected["result_summary"] = {k: summary[k] for k in fields if k in summary}
                events.append(selected)
            selection = {
                k: trace[k]
                for k in ("scope_id", "rounds", "coverage", "durable", "missing_rounds", "dropped_events", "truncated")
            }
            content = wire.encoded({**base, "state": "observed", "trace": {**selection, "events": events}}).decode()
            if len(wire.encoded({ARTIFACT: content})) > LIMIT:
                return unavailable("Router selection exceeded the observation artifact limit")
            return content
        except (httpx.HTTPError, ValueError, TypeError, KeyError, UnicodeError):
            return unavailable("Router observations unavailable after execution (reset, expiry or read failure)")
