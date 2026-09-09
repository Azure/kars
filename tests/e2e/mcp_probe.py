# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded MCP fixture client; logs protocol facts, never results or sessions."""

import json
import re
from http.client import HTTPException
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener

MAX_BYTES = 2 * 1024 * 1024
PROTOCOL = "2025-06-18"
PROTOCOLS = {"2024-11-05", "2025-03-26", PROTOCOL, "2025-11-25"}
HTTP_FAILURES = {
    (406, b"Accept must include both application/json and text/event-stream"): "accept-negotiation",
    (403, b"Same-router socket peer required for local MCP"): "socket-peer",
    (401, b"Verified MCP caller required"): "oauth-required",
    (404, b"Unknown or unqualified MCP server scope"): "scope-unavailable",
    (400, b"Invalid MCP server scope"): "scope-invalid",
}


def decode(body, content_type, request_id):
    if not body:
        return None
    if content_type == "application/json":
        value = json.loads(body)
    elif content_type == "text/event-stream":
        messages, data = [], []
        for line in body.decode("utf8").replace("\r\n", "\n").splitlines():
            if line.startswith("data:"):
                data.append(line[5:].lstrip(" "))
            elif not line and data:
                message = json.loads("\n".join(data))
                if isinstance(message, dict) and "id" in message:
                    messages.append(message)
                data = []
        if data or len(messages) != 1:
            raise ValueError("MCP response did not complete one result")
        value = messages[0]
    else:
        raise ValueError("MCP response content type is unsupported")
    if (not isinstance(value, dict) or value.get("jsonrpc") != "2.0"
            or type(value.get("id")) is not int or value["id"] != request_id
            or ("result" in value) == ("error" in value)
            or ("result" in value and not isinstance(value["result"], dict))):
        raise ValueError("MCP response correlation failed")
    return value


class Client:
    def __init__(self, port, server):
        if not isinstance(port, int) or not 0 < port < 65536:
            raise ValueError("MCP fixture requires a loopback port")
        if not isinstance(server, str) or not re.fullmatch(r"[a-z0-9.-]{1,253}", server):
            raise ValueError("MCP fixture server scope is invalid")
        self.port, self.server = port, server
        self.session, self.protocol, self.next_id = None, PROTOCOL, 1
        self.initialized, self.last_fact = False, None

    def fact(self, phase, status, content, category, rpc_code=None):
        fact = {"phase": phase, "httpStatus": status, "contentType": content,
                "category": category, "sessionPresent": self.session is not None}
        if rpc_code in (-32700, -32600, -32601, -32602, -32603, -32000, -32001):
            fact["rpcCode"] = rpc_code
        if fact != self.last_fact:
            print("MCP-RPC " + json.dumps(fact, sort_keys=True), flush=True)
            self.last_fact = fact

    def request(self, method, params, server=None, notification=False):
        request_id = None if notification else self.next_id
        self.next_id += 1
        frame = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notification:
            frame["id"] = request_id
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": self.protocol, "X-Kars-Mcp-Server": server or self.server}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = Request(f"http://127.0.0.1:{self.port}/mcp", data=json.dumps(frame).encode(),
                      headers=headers, method="POST")
        phase = method if method in ("initialize", "notifications/initialized", "tools/list", "tools/call") else "other"
        status, content = 0, "absent"
        try:
            try:
                response = build_opener(ProxyHandler({})).open(req, timeout=15)
            except HTTPError as error:
                response = error
            with response:
                status = response.code
                kind = response.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
                content = {"": "absent", "application/json": "json", "text/event-stream": "sse",
                           "text/plain": "text"}.get(kind, "other")
                body = response.read(MAX_BYTES + 1)
                session = response.headers.get("Mcp-Session-Id")
            if len(body) > MAX_BYTES:
                self.fact(phase, status, content, "oversize")
                raise ValueError("MCP fixture response exceeded byte limit")
            if not 200 <= status < 300:
                self.fact(phase, status, content, HTTP_FAILURES.get((status, body.strip()), "http-rejected"))
                return status, None, None
            if notification:
                if status != 202 or body:
                    raise ValueError("MCP fixture notification was not accepted")
                self.fact(phase, status, content, "accepted")
                return status, None, None
            value = decode(body, kind, request_id)
            if value is None:
                raise ValueError("MCP fixture omitted its result")
            if "error" in value:
                error = value["error"]
                self.fact(phase, status, content, "rpc-error",
                          error.get("code") if isinstance(error, dict) else None)
                return status, value, None
            self.fact(phase, status, content, "accepted")
            return status, value, session
        except (OSError, ValueError, HTTPException) as error:
            self.fact(phase, status, content, "invalid-response" if isinstance(error, ValueError) else "transport-error")
            raise AssertionError("Bounded MCP fixture protocol/transport failed") from None

    def initialize(self):
        status, body, session = self.request("initialize", {
            "protocolVersion": PROTOCOL, "capabilities": {},
            "clientInfo": {"name": "kars-kind-fixture", "version": "1"},
        })
        result = (body or {}).get("result", {})
        if status != 200 or not isinstance(result, dict) or result.get("protocolVersion") not in PROTOCOLS:
            return False
        if session is not None and not re.fullmatch(r"[\x21-\x7e]{1,1024}", session):
            raise AssertionError("MCP fixture session format is invalid")
        self.session, self.protocol = session, result["protocolVersion"]
        status, _, _ = self.request("notifications/initialized", {}, notification=True)
        self.initialized = status == 202
        return self.initialized

    def call(self, method, params, server=None):
        if not self.initialized and not self.initialize():
            return 0, None
        status, body, _ = self.request(method, params, server)
        # Discovery may explicitly retry; a tools/call is never replayed here.
        if status in (400, 404):
            self.initialized, self.session = False, None
        return status, body
