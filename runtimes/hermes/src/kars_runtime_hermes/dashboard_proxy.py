# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""
Wraps the upstream Hermes dashboard FastAPI app with middleware that
injects ``X-Forwarded-Prefix`` on every request from an env var
(``HERMES_DASHBOARD_PREFIX``).

Why this exists
---------------

The Hermes dashboard (FastAPI + Vite SPA) reads the
``X-Forwarded-Prefix`` request header to rewrite absolute asset URLs
(``/assets/index-XYZ.js`` → ``<prefix>/assets/index-XYZ.js``). It
expects an upstream reverse proxy (Caddy / nginx / Traefik) to inject
the header on each request — that's how the SPA can be served at a
sub-path without a Vite rebuild.

The kars-sre dashboard is reached through the K8s apiserver service
proxy:

    /clusters/<cluster>/api/v1/namespaces/kars-sre/services/sre:9119/proxy/

The K8s apiserver proxy does NOT inject any X-Forwarded-* headers,
so absolute asset paths blank-load the iframe in the Headlamp Chat
console.

Fix: this wrapper script imports the upstream FastAPI app and adds a
single middleware that sets the header from ``HERMES_DASHBOARD_PREFIX``
on every request. The Headlamp plugin sets the env var to the
matching apiserver-proxy sub-path before launching.

How it runs
-----------

The entrypoint script chooses between this wrapper and the stock
``hermes dashboard`` based on whether ``HERMES_DASHBOARD_PREFIX`` is
set. When set, we boot uvicorn directly here (bypassing
``hermes dashboard``'s host gate); when unset, the stock CLI runs
unmodified.

Why not patch upstream
----------------------

The upstream feature is "support reverse proxy"; what we need is
"pretend a reverse proxy is in front". Both are valid, and conflating
them upstream would broaden the contract Hermes has to honour. Wrapping
keeps the divergence small and reversible.
"""

from __future__ import annotations

import os
import sys

# Importing this also executes the upstream startup (lifespan handlers,
# session-token mint, route registration). We rely on that having
# completed before we add middleware.
from hermes_cli.web_server import app  # type: ignore[import-not-found]


def _install_prefix_middleware(prefix: str) -> None:
    """Add a Starlette HTTP middleware that injects X-Forwarded-Prefix.

    Idempotent — calling twice replaces the previous middleware.
    """
    # Lazy import: Starlette ships with FastAPI; importing at top would
    # double-load it.
    from starlette.middleware.base import BaseHTTPMiddleware

    class _ForwardedPrefixMiddleware(BaseHTTPMiddleware):
        async def dispatch(self, request, call_next):  # type: ignore[override]
            # Inject the header by mutating the raw scope. Starlette's
            # request.headers is read-only; the scope's raw header
            # list (`scope["headers"]`) is the source of truth.
            headers = list(request.scope.get("headers", []))
            key = b"x-forwarded-prefix"
            # Drop any existing entry so we always win.
            headers = [(k, v) for (k, v) in headers if k != key]
            headers.append((key, prefix.encode("ascii")))
            request.scope["headers"] = headers
            return await call_next(request)

    app.add_middleware(_ForwardedPrefixMiddleware)


def main() -> None:
    prefix = os.environ.get("HERMES_DASHBOARD_PREFIX", "")
    host = os.environ.get("HERMES_DASHBOARD_HOST", "0.0.0.0")
    port = int(os.environ.get("HERMES_DASHBOARD_PORT", "9119"))

    if prefix:
        _install_prefix_middleware(prefix)
        print(
            f"[kars-hermes-dashboard] X-Forwarded-Prefix middleware installed: {prefix!r}",
            file=sys.stderr,
        )
    else:
        print(
            "[kars-hermes-dashboard] HERMES_DASHBOARD_PREFIX unset — running without prefix injection",
            file=sys.stderr,
        )

    import uvicorn

    uvicorn.run(app, host=host, port=port, log_level="info")


if __name__ == "__main__":
    main()
