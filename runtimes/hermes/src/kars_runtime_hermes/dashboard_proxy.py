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


_KARS_PREFIX_QUERY_KEY = "_kars_prefix"


def _patch_hermes_prefix_validator() -> None:
    """Raise Hermes' built-in X-Forwarded-Prefix length cap.

    Hermes' upstream ``normalise_prefix`` caps the header value at
    64 chars (header-injection guard). When the dashboard is served
    via the K8s apiserver service proxy AND Headlamp's
    ``/clusters/<cluster>/...`` hop, the legitimate prefix runs ~90+
    chars and Hermes rejects it as ``""`` — leaving the SPA with
    empty asset URLs.

    We keep every other rule (no ``//``, no ``..``, no quoting / CR /
    LF / etc.) and just raise the length cap to 256, which is enough
    headroom for any apiserver-proxy URL while still capping obvious
    header garbage.

    Monkey-patches the module-level function; the upstream call sites
    re-import on every request so the patched version takes effect
    immediately.
    """
    from hermes_cli.dashboard_auth import prefix as _pref_mod

    # Mirror the upstream _REJECT_CHARS so a future upstream tightening
    # doesn't silently get loosened here.
    _reject = frozenset(('"', "'", "<", ">", " ", "\n", "\r", "\t"))

    def _permissive(raw):
        if not raw:
            return ""
        p = raw.strip()
        if not p:
            return ""
        if not p.startswith("/"):
            p = "/" + p
        p = p.rstrip("/")
        if "//" in p or ".." in p or any(c in p for c in _reject):
            return ""
        # Was 64 upstream; lift to 256 to fit
        # /clusters/<cluster>/api/v1/namespaces/<ns>/services/<svc>:<port>/proxy
        if len(p) > 256:
            return ""
        return p

    _pref_mod.normalise_prefix = _permissive


def _set_bind_state(host: str, port: int) -> None:
    """Populate ``app.state.bound_host`` + ``bound_port`` + ``auth_required``.

    Hermes' own ``start_server`` populates these from the uvicorn host/port
    args. Since we bypass ``start_server`` (we call ``uvicorn.run`` directly
    so we can install our X-Forwarded-Prefix middleware first), those
    attributes never get set — and several downstream code paths silently
    misbehave:

      - ``_build_gateway_ws_url`` returns ``None`` so the PTY-launched
        ``hermes --tui`` child gets NO ``HERMES_TUI_GATEWAY_URL`` env var
        and can't dial back to this process's in-memory ``tui_gateway``.
        The chat then renders the TUI shell, accepts keystrokes, but the
        bytes have nowhere to land — the smoking-gun symptom of "I can
        click but can't type".
      - ``_ws_client_reason`` can't compare ``client_host`` against the
        bind host, so its loopback-only guard goes silent.
      - ``should_require_auth`` doesn't run, so the OAuth gate is
        ambiguous — we set ``auth_required=False`` explicitly when bound
        to loopback to match the upstream truth table.

    Mirrors hermes_cli/web_server.py ``start_server`` exactly so all the
    upstream ``getattr(app.state, "bound_host", "")`` lookups behave as
    if Hermes had bootstrapped the server itself.
    """
    app.state.bound_host = host
    app.state.bound_port = port
    # Loopback bind ⇒ auth NOT required (per Hermes' should_require_auth
    # truth table). Required so the SPA's getAuthMe / buildWsAuthParam
    # helpers take the loopback fast-path instead of trying to mint
    # OAuth tickets that have no provider configured.
    app.state.auth_required = host not in {"127.0.0.1", "localhost", "::1"}


def _install_prefix_middleware(default_prefix: str) -> None:
    """Add a Starlette HTTP middleware that injects X-Forwarded-Prefix.

    The header value is chosen per-request:

    * If the request URL has a ``?_kars_prefix=<value>`` query param,
      that value wins. This is how the Headlamp plugin tells the SPA
      the FULL apiserver-proxy URL it lives behind — which includes
      the dynamic ``/clusters/<cluster>`` segment that the wrapper
      cannot know from its env alone.
    * Otherwise the env-var ``default_prefix`` is used (matches the
      single in-pod prefix and is sufficient when a user opens the
      dashboard directly via ``kubectl port-forward``).

    The middleware is idempotent — calling twice replaces the previous
    instance.

    Why we also strip the prefix from the inbound path: when the
    dashboard is reached via ``kubectl port-forward`` (no apiserver
    proxy in the loop), the SPA itself emits asset URLs prefixed with
    ``X-Forwarded-Prefix`` and the browser then sends them back as
    ``/<prefix>/assets/...``. Without stripping, those would 404
    because Hermes' static mount is rooted at ``/assets/``. When the
    apiserver proxy IS in the loop it has already stripped the prefix
    for us, and the strip step becomes a no-op (path doesn't start
    with prefix → skipped).
    """
    # Lazy import: Starlette ships with FastAPI; importing at top would
    # double-load it.
    from starlette.middleware.base import BaseHTTPMiddleware
    from urllib.parse import parse_qs

    class _ForwardedPrefixMiddleware(BaseHTTPMiddleware):
        async def dispatch(self, request, call_next):  # type: ignore[override]
            scope = request.scope

            # Per-request prefix: query-param override wins so the
            # Headlamp plugin can stamp the cluster-rooted prefix.
            prefix = ""
            query_string = scope.get("query_string", b"") or b""
            if query_string:
                try:
                    qs = parse_qs(query_string.decode("ascii"))
                    override = qs.get(_KARS_PREFIX_QUERY_KEY, [None])[0]
                    if override:
                        prefix = override
                except (UnicodeDecodeError, ValueError):
                    # Malformed query string — fall back to no prefix.
                    pass

            # Fall back to the env-var prefix ONLY when the inbound
            # path actually lives under it (i.e. we're served behind a
            # reverse proxy that didn't strip the prefix). When the
            # dashboard is reached via `kubectl port-forward` the path
            # is rooted at `/` — injecting a phantom prefix would make
            # the SPA's <Router basename> reject every URL and render
            # nothing (the classic blank-iframe symptom).
            raw_path = scope.get("path", "")
            if not prefix and default_prefix and raw_path.startswith(default_prefix):
                prefix = default_prefix

            # Strip the prefix from the path FastAPI matches against
            # so a directly-served `/api/v1/.../proxy/assets/index.js`
            # still resolves to `/assets/index.js`.
            if prefix and raw_path.startswith(prefix):
                stripped = raw_path[len(prefix):] or "/"
                if not stripped.startswith("/"):
                    stripped = "/" + stripped
                scope["path"] = stripped
                scope["raw_path"] = stripped.encode("ascii")

            # Inject the header so the SPA's index.html bootstrap
            # writes asset URLs that include the full prefix. Skipped
            # entirely when no prefix is in play — Hermes' upstream
            # then bakes "" and the SPA mounts at root.
            if prefix:
                headers = list(scope.get("headers", []))
                key = b"x-forwarded-prefix"
                headers = [(k, v) for (k, v) in headers if k != key]
                headers.append((key, prefix.encode("ascii")))
                scope["headers"] = headers

            return await call_next(request)

    app.add_middleware(_ForwardedPrefixMiddleware)


def main() -> None:
    prefix = os.environ.get("HERMES_DASHBOARD_PREFIX", "")
    host = os.environ.get("HERMES_DASHBOARD_HOST", "127.0.0.1")
    port = int(os.environ.get("HERMES_DASHBOARD_PORT", "9119"))

    _patch_hermes_prefix_validator()
    _set_bind_state(host, port)
    _install_prefix_middleware(prefix)
    print(
        f"[kars-hermes-dashboard] bound_host={host} bound_port={port} "
        f"auth_required={app.state.auth_required} "
        f"(default_prefix={prefix!r}; per-request override via ?{_KARS_PREFIX_QUERY_KEY}=)",
        file=sys.stderr,
    )

    import uvicorn

    uvicorn.run(app, host=host, port=port, log_level="info")


if __name__ == "__main__":
    main()
