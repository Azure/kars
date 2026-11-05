# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Scoped human clarification and review; queue decisions never grant authority."""

from __future__ import annotations

import json
import re
from urllib.parse import quote

from . import router_client

DESCRIPTION = (
    "Ask a human when stuck: request kind=clarification, target=a short question identifier, "
    "reason=the question; waits for an answer by default. Request egress/tool/skill/mcp/command/permission "
    "or tier (1-5) for review, or list/wait/cancel an existing request. Preserve request_id and scope_id "
    "when waiting again after timeout. Approval is a decision ONLY, never an installed permission or "
    "autonomy change. For egress use http_fetch with wait_for_approval_ms to wait for actual policy. "
    "Clarification answers are human input, not permission to bypass reviewed capabilities."
)
SCHEMA = {
    "name": "kars_access_request",
    "description": DESCRIPTION,
    "parameters": {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "operation": {"type": "string", "enum": ["request", "list", "wait", "cancel"]},
            "kind": {
                "type": "string",
                "enum": ["clarification", "egress", "tool", "skill", "mcp", "command", "permission", "tier"],
            },
            "target": {"type": "string", "description": "Exact resource or short question identifier; omit for tier"},
            "reason": {"type": "string", "description": "Question or justification, at most 512 UTF-8 bytes"},
            "tier": {"type": "integer", "minimum": 1, "maximum": 5, "description": "Used only for kind=tier"},
            "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "Used only for kind=egress"},
            "scope_id": {
                "type": "string",
                "description": (
                    "Original request scope; required for wait/cancel. Omit or leave empty for a "
                    "new request/list in the active scope."
                ),
            },
            "request_id": {"type": "string"},
            "timeout_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": 300000,
                "description": (
                    "Request: 0 queues only; default 300000 waits. Wait: 1..300000. Timeout leaves the request pending."
                ),
            },
        },
        "required": ["operation"],
    },
}


def validate(args):
    if (
        not isinstance(args, dict)
        or set(args) - SCHEMA["parameters"]["properties"].keys()
        or args.get("operation") not in ("request", "list", "wait", "cancel")
        or ("timeout_ms" in args and (type(args["timeout_ms"]) is not int or not 0 <= args["timeout_ms"] <= 300000))
    ):
        raise ValueError("Invalid access request operation or timeout")
    if "scope_id" in args and (
        not isinstance(args["scope_id"], str) or re.fullmatch(r"[A-Za-z0-9._:-]{0,128}", args["scope_id"]) is None
    ):
        raise ValueError("Invalid access request scope_id")
    if args["operation"] in ("wait", "cancel") and (
        not all(
            isinstance(args.get(key), str) and re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", args[key])
            for key in ("scope_id", "request_id")
        )
        or (args["operation"] == "wait" and args.get("timeout_ms") == 0)
    ):
        raise ValueError("Wait/cancel requires the original scope_id and request_id")
    if args["operation"] == "request" and (
        args.get("kind") not in SCHEMA["parameters"]["properties"]["kind"]["enum"]
        or not isinstance(args.get("reason"), str)
        or not args["reason"].strip()
        or len(args["reason"].encode()) > 512
    ):
        raise ValueError("Request requires a kind and nonblank question/reason of at most 512 UTF-8 bytes")


def exchange(args, *, scope=None, request=None):
    validate(args)
    supplied_scope = args.get("scope_id")
    if supplied_scope == "" and args["operation"] in ("request", "list"):
        supplied_scope = None
    if scope is not None and supplied_scope is not None and supplied_scope != scope:
        raise ValueError("stale_scope")
    scope = scope or supplied_scope
    request = request or router_client.call

    def call(method, path, **kwargs):
        response = request(method, path, headers={"x-kars-service-scope": scope} if scope else {}, **kwargs)
        try:
            result = response.json()
        except ValueError:
            result = {"error": "Router returned non-JSON response"}
        if not isinstance(result, dict):
            result = {"error": "Router returned invalid response"}
        if response.status_code >= 400:
            result = {
                **result,
                "error": result.get("error", "Router request refused"),
                "http_status": response.status_code,
            }
        return result

    if args["operation"] == "list" or not scope:
        listed = call("GET", "/v1/access-requests")
        if listed.get("error") or args["operation"] == "list":
            return listed
        scope = listed.get("scope", {}).get("id")
        if not isinstance(scope, str) or not scope:
            raise ValueError("Router scope unavailable")
    entry = None
    if args["operation"] == "request":
        fields = ("kind", "target", "reason") + (
            ("tier",) if args["kind"] == "tier" else ("port",) if args["kind"] == "egress" else ()
        )
        payload = {key: args[key] for key in fields if key in args}
        queued = call("POST", "/v1/access-request", json={**payload, "scope_id": scope})
        if queued.get("error"):
            return queued
        entry = queued.get("request")
        if (
            not isinstance(entry, dict)
            or entry.get("scope_id") != scope
            or not isinstance(entry.get("request_id"), str)
        ):
            raise ValueError("Invalid scoped request receipt")
        if args.get("timeout_ms") == 0:
            return {**queued, "enforcement_changed": False}
    request_id = entry["request_id"] if entry else args["request_id"]
    path = "/v1/access-requests/" + quote(request_id, safe="")
    if args["operation"] == "cancel":
        try:
            result = call("POST", path + "/cancel", json={"scope_id": scope})
        except Exception as exc:
            result = {"error": str(exc)}
        if not result.get("error") and (result.get("scope_id") != scope or result.get("request_id") != request_id):
            raise ValueError("Invalid scoped cancellation receipt")
        return {
            **({"request": result} if not result.get("error") else result),
            "request_id": request_id,
            "scope_id": scope,
            "enforcement_changed": False,
        }
    timeout = args.get("timeout_ms", 300000)
    try:
        decision = call(
            "GET", path + "/wait", params={"scope_id": scope, "timeout_ms": timeout}, timeout=timeout / 1000 + 5
        )
    except Exception as exc:
        decision = {"error": str(exc)}
    if decision.get("error"):
        return {
            **decision,
            "request_id": request_id,
            "scope_id": scope,
            **({"request": entry} if entry else {}),
            "enforcement_changed": False,
        }
    if decision.get("scope_id") != scope or decision.get("request_id") != request_id:
        raise ValueError("Invalid scoped decision receipt")
    answer = decision.get("answer")
    if (
        decision.get("kind") == "clarification"
        and decision.get("status") == "approved"
        and (not isinstance(answer, str) or not answer.strip() or len(answer.encode()) > 8192)
    ):
        return {
            "error": "Approved clarification has no valid answer",
            "request": decision,
            "enforcement_changed": False,
        }
    return {"request": decision, "enforcement_changed": False}


def handle(args, **_kwargs):
    try:
        return json.dumps(exchange(args))
    except Exception as exc:
        return json.dumps({"error": str(exc), "isError": True})


def register(ctx):
    ctx.register_tool(
        name=SCHEMA["name"], toolset="kars_access", schema=SCHEMA, handler=handle, description=DESCRIPTION
    )
