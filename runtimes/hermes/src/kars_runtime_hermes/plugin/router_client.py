"""HTTP client to the inference-router sidecar at ``http://127.0.0.1:8443``.

Single source of truth for: base URL, admin-token discovery, default
headers, request/response shape, error handling. Every other plugin
module talks to the router through this.

Reads the admin token once at first use from
``/etc/kars/secrets/admin-token`` and caches for the process lifetime.
Falls back to the env var ``KARS_ROUTER_ADMIN_TOKEN`` if the file is
absent (docker dev convenience).
"""

from __future__ import annotations

import logging
import os
from functools import lru_cache
from typing import Any

import httpx

logger = logging.getLogger("kars.hermes.router")

# Loopback only — kars egress-guard blocks every other destination from
# UID 1000. The runtime contract pins this; do NOT make it configurable.
ROUTER_BASE_URL = "http://127.0.0.1:8443"

# Path the controller mounts the router-admin-token Secret into.
# See docs/runtimes/CONTRACT.md § File system contract.
ADMIN_TOKEN_PATH = "/etc/kars/secrets/admin-token"


@lru_cache(maxsize=1)
def admin_token() -> str | None:
    """Return the router admin token, or None if not available.

    Loopback connections to the router bypass the bearer check, but
    handoff endpoints reject the bypass and require the token — so
    plugins should always send it when available.
    """
    try:
        with open(ADMIN_TOKEN_PATH, encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return os.environ.get("KARS_ROUTER_ADMIN_TOKEN") or None


def _headers() -> dict[str, str]:
    """Build default request headers."""
    h: dict[str, str] = {
        "content-type": "application/json",
        "x-kars-sandbox": os.environ.get("SANDBOX_NAME", "unknown"),
    }
    token = admin_token()
    if token:
        h["authorization"] = f"Bearer {token}"
    return h


@lru_cache(maxsize=1)
def _client() -> httpx.Client:
    """Per-process httpx client. Reused across all router calls."""
    return httpx.Client(
        base_url=ROUTER_BASE_URL,
        timeout=httpx.Timeout(connect=2.0, read=30.0, write=10.0, pool=2.0),
        headers=_headers(),
    )


def call(method: str, path: str, *, json: Any | None = None, params: dict | None = None) -> httpx.Response:
    """Call the router. Path is a leading-slash absolute path under base URL.

    Returns the raw httpx.Response so callers can branch on .status_code
    (some kars endpoints intentionally return 4xx as part of normal flow,
    e.g. /sandbox/{name}/status returns 404 while a sub-agent is booting).
    """
    return _client().request(method, path, json=json, params=params)


def call_json(method: str, path: str, *, json: Any | None = None, params: dict | None = None) -> dict[str, Any]:
    """Call the router and return parsed JSON, raising on non-2xx.

    Use when you want strict success-only semantics. For endpoints where
    4xx is meaningful (status polling), use ``call`` directly.
    """
    resp = call(method, path, json=json, params=params)
    resp.raise_for_status()
    return resp.json()


def clear_token_cache() -> None:
    """Test helper — forget the cached admin token + client."""
    admin_token.cache_clear()
    _client.cache_clear()
