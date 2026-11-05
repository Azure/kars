# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""http_fetch tool — Phase A1.4 (always-on).

HTTP fetch routed through the inference router's ``/egress/fetch``
endpoint. The router enforces:
  * Blocklist (always — even in Learn mode)
  * Allowlist (Strict mode)
  * Learn mode (records all new domains)
  * SSRF protection (rejects localhost/private/.local/.internal hosts)

Mirror of OpenClaw's ``runtimes/openclaw/src/core/agt-tools/http-fetch.ts``.
"""

from __future__ import annotations

import json
import logging
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.http_fetch")


def fetch_response(args: dict[str, Any], request=None, *, telemetry=None):
    """Shared router dispatch; reviewed execution supplies its bounded mission transport."""
    request = request or router_client.call
    wait = args.get("wait_for_approval_ms", 0)
    if type(wait) is not int or not 0 <= wait <= 300000:
        raise ValueError("wait_for_approval_ms must be 0..300000")
    scope = telemetry.get("scope_id") if telemetry else None
    if wait and scope is None:
        listed = request("GET", "/v1/access-requests")
        listed.raise_for_status()
        scope = listed.json().get("scope", {}).get("id")
        if not isinstance(scope, str) or not scope:
            raise ValueError("Router scope unavailable")
    return request(
        "POST",
        "/egress/fetch",
        **({"timeout": wait / 1000 + 35} if wait else {}),
        json={
            "url": str(args.get("url", "")).strip(),
            "method": str(args.get("method", "GET")).upper(),
            "headers": args.get("headers") if isinstance(args.get("headers"), dict) else {},
            "body": str(args.get("body") or ""),
            **({"telemetry": telemetry} if telemetry is not None else {}),
            **({"scope_id": scope, "wait_for_approval_ms": wait} if wait else {}),
        },
    )


def _http_fetch(args: dict[str, Any], **_kwargs: Any) -> str:
    url = str(args.get("url", "")).strip()
    if not url:
        return json.dumps({"error": "url is required"})
    try:
        resp = fetch_response(args)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"egress fetch failed: {exc}"})

    if resp.status_code >= 400:
        try:
            result = resp.json()
        except ValueError:
            result = {"body": resp.text[:500]}
        if not isinstance(result, dict):
            result = {"body": resp.text[:500]}
        return json.dumps(
            {
                **result,
                "error": result.get("error", f"egress denied (HTTP {resp.status_code})"),
                "http_status": resp.status_code,
            }
        )
    # Return body verbatim — caller can parse as needed.
    return resp.text[: 32 * 1024]  # cap at 32KB for prompt safety


_HTTP_FETCH_SCHEMA = {
    "name": "http_fetch",
    "description": (
        "HTTP fetch routed through the kars egress proxy. Subject to "
        "blocklist (always), Learn-mode logging (default), or allowlist "
        "(Strict mode). SSRF-protected — localhost/private IPs rejected. "
        "Use for arbitrary HTTPS endpoints; for high-level web search "
        "use foundry_web_search if available."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "url": {"type": "string", "description": "Absolute URL (https:// or http://)"},
            "method": {"type": "string", "description": "GET / POST / PUT / PATCH / DELETE / HEAD (default GET)"},
            "headers": {"type": "object", "description": "Optional HTTP headers"},
            "body": {"type": "string", "description": "Optional request body (string)"},
            "wait_for_approval_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": 300000,
                "description": (
                    "Wait for installed egress policy before dispatch; queue approval alone is "
                    "not permission. Default 0 returns denial and request details."
                ),
            },
        },
        "required": ["url"],
    },
}


def register(ctx: Any) -> None:  # noqa: ANN401
    ctx.register_tool(
        name="http_fetch",
        toolset="http_fetch",
        schema=_HTTP_FETCH_SCHEMA,
        handler=_http_fetch,
        description=_HTTP_FETCH_SCHEMA["description"],
    )
    logger.info("http_fetch registered")
