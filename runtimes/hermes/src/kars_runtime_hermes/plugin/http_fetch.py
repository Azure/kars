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


def _http_fetch(args: dict[str, Any], **_kwargs: Any) -> str:
    url = str(args.get("url", "")).strip()
    if not url:
        return json.dumps({"error": "url is required"})
    method = str(args.get("method", "GET")).upper()
    headers = args.get("headers") or {}
    body = args.get("body") or ""

    payload = {
        "url": url,
        "method": method,
        "headers": headers if isinstance(headers, dict) else {},
        "body": str(body),
    }
    try:
        resp = router_client.call("POST", "/egress/fetch", json=payload)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"egress fetch failed: {exc}"})

    if resp.status_code >= 400:
        return json.dumps(
            {
                "error": f"egress denied (HTTP {resp.status_code})",
                "body": resp.text[:500],
            }
        )
    # Return body verbatim — caller can parse as needed.
    return resp.text[:32 * 1024]  # cap at 32KB for prompt safety


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

