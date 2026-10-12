# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Forward only the shared router recorder's returned-response selection."""

import re

import httpx

from . import mission_protocol as wire

ARTIFACT = "kars-router-observations.json"
LIMIT = 32 * 1024


class ReturnedRouterResponses:
    def __init__(self, transport):
        self.transport = transport
        self.responses = 0
        self.scope = None
        self.rounds = set()
        self.advertised = self.invalid = False

    def capture(self, headers):
        self.responses += 1
        scope, number = headers.get("x-kars-telemetry-scope"), headers.get("x-kars-telemetry-round")
        if scope is None and number is None:
            return
        self.advertised = True
        if (not isinstance(scope, str) or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", scope)
                or not isinstance(number, str) or not re.fullmatch(r"[1-9][0-9]{0,15}", number)):
            self.invalid = True
            return
        number = int(number)
        if (not wire.integer(number, 1) or number in self.rounds or len(self.rounds) >= 32
                or (self.scope is not None and scope != self.scope)):
            self.invalid = True
            return
        self.scope = scope
        self.rounds.add(number)

    def artifact(self):
        if not self.advertised:
            return None
        rounds = sorted(self.rounds)
        base = dict(version=1, source="runtime-forwarded-router-observations",
                    coverage="returned-responses-only", responses=self.responses,
                    scope_id=self.scope, rounds=rounds)

        def unavailable(reason):
            return wire.encoded({**base, "state": "unavailable", "reason": reason}).decode()

        if self.invalid or self.scope is None:
            return unavailable("Response correlation changed or was invalid")
        try:
            request = httpx.Request(
                "GET", "http://127.0.0.1:8443/telemetry/trace?rounds=" + ",".join(map(str, rounds)),
                headers={"x-kars-service-scope": self.scope},
                extensions={"timeout": dict(connect=2, read=2, write=2, pool=2)})
            response = self.transport.handle_request(request)
            try:
                wire.require(response.status_code == 200, "Observation read failed")
                body = bytearray()
                for chunk in response.iter_bytes():
                    body.extend(chunk)
                    wire.require(len(body) <= 48 * 1024, "Observation response too large")
                trace = wire.loads(bytes(body), 48 * 1024)
            finally:
                response.close()
            wire.require(
                isinstance(trace, dict) and trace.get("scope_id") == self.scope
                and type(trace.get("rounds")) is list and all(wire.integer(n, 1) for n in trace["rounds"])
                and trace["rounds"] == rounds and trace.get("coverage") == base["coverage"]
                and trace.get("durable") is False and type(trace.get("events")) is list
                and len(trace["events"]) <= 256 and type(trace.get("missing_rounds")) is list
                and all(wire.integer(n, 1) and n in rounds for n in trace["missing_rounds"])
                and wire.integer(trace.get("dropped_events")) and type(trace.get("truncated")) is bool,
                "Router selection did not match returned response IDs")
            events = []
            for event in trace["events"]:
                wire.require(
                    isinstance(event, dict) and event.get("scope_id") == self.scope
                    and wire.integer(event.get("round"), 1) and event["round"] in rounds
                    and (event.get("kind") in ("round", "tool_proposed")
                         or (event.get("kind") == "tool_result"
                             and wire.integer(event.get("reported_in_round"), 1)
                             and event["reported_in_round"] in rounds)),
                    "Router event did not match returned response IDs")
                fields = (("provider", "model", "http_status", "accepted", "outcome", "usage_state",
                           "finish_reason", "partial_observation", "ms", "tool_calls_observed")
                          if event["kind"] == "round" else ("call_id", "name", "ok", "reported_in_round", "ms"))
                selected = {k: event[k] for k in (
                    "kind", "round", "scope_id", "seq", "source", "observed_at", *fields) if k in event}
                if event["kind"] == "round" and isinstance(event.get("usage"), dict):
                    selected["usage"] = {k: event["usage"].get(k)
                                         for k in ("prompt_tokens", "completion_tokens", "total_tokens")}
                if event["kind"] == "tool_result" and isinstance(event.get("result_summary"), dict):
                    summary = event["result_summary"]
                    fields = {"file_read": ("kind", "bytes", "returned_bytes", "truncated"),
                              "file_write": ("kind", "bytes", "artifact_name"),
                              "failure": ("kind", "reason")}.get(summary.get("kind"))
                    wire.require(fields is not None, "Unknown tool summary")
                    selected["result_summary"] = {k: summary[k] for k in fields if k in summary}
                events.append(selected)
            selection = {k: trace[k] for k in (
                "scope_id", "rounds", "coverage", "durable", "missing_rounds", "dropped_events", "truncated")}
            content = wire.encoded({**base, "state": "observed", "trace": {
                **selection, "events": events}}).decode()
            if len(wire.encoded({ARTIFACT: content})) > LIMIT:
                return unavailable("Router selection exceeded the observation artifact limit")
            return content
        except (httpx.HTTPError, ValueError, TypeError, KeyError, UnicodeError):
            return unavailable("Router observations unavailable after execution (reset, expiry or read failure)")
