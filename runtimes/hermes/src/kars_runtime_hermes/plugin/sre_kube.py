# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre — Kubernetes apiserver client (S1).

A minimal in-cluster apiserver client built on httpx — no `kubernetes`
PyPI dep added to the Hermes runtime image (which is shared with
non-SRE sandboxes; keeping the dep footprint tight is part of the
§7.8.1 design even though Slice 1 ships SRE in the shared image
behind the ``KARS_SRE_ENABLED`` env gate — the §7.8.1 separate
image is a follow-up slice).

Reads the standard projected ServiceAccount artefacts mounted at:

  - ``/var/run/secrets/kubernetes.io/serviceaccount/token``  — auto-rotated
  - ``/var/run/secrets/kubernetes.io/serviceaccount/ca.crt`` — apiserver CA
  - ``/var/run/secrets/kubernetes.io/serviceaccount/namespace`` — pod's ns

and dials ``https://kubernetes.default.svc.cluster.local:443`` (the
in-cluster apiserver Service) with the SA token as the Bearer credential.

There is no fallback for out-of-cluster operation; this module is
designed to run inside a pod with a projected SA token. The Slice 1
RBAC binding (``kars-sre-reader`` ClusterRole on the ``sandbox`` SA
in namespace ``kars-sre``) defines what this client can read.
"""

from __future__ import annotations

import os
import pathlib
from typing import Any

import httpx

_SA_DIR = pathlib.Path("/var/run/secrets/kubernetes.io/serviceaccount")
_DEFAULT_APISERVER = "https://kubernetes.default.svc.cluster.local"

# Read tokens / CA each call. The kubelet rotates the projected token
# on a regular cadence (default 1h) and rewrites the file in place; a
# cached value would expire silently. The cost of re-reading a ~1KB
# file is negligible vs. the apiserver round-trip.


def _read_token() -> str:
    p = _SA_DIR / "token"
    if not p.exists():
        raise RuntimeError(
            "no ServiceAccount token at "
            f"{p} — kars-sre must run inside a pod with a projected SA"
        )
    return p.read_text(encoding="utf-8").strip()


def _ca_bundle() -> str:
    p = _SA_DIR / "ca.crt"
    if not p.exists():
        raise RuntimeError(f"no apiserver CA at {p}")
    return str(p)


def _apiserver_host() -> str:
    # The standard env vars the kubelet injects.
    host = os.environ.get("KUBERNETES_SERVICE_HOST")
    port = os.environ.get("KUBERNETES_SERVICE_PORT", "443")
    if host:
        return f"https://{host}:{port}"
    return _DEFAULT_APISERVER


class KubeClient:
    """Thin wrapper around httpx for read-only apiserver calls.

    Per-instance httpx client is reused across calls; rebuilt when the
    SA token is rotated (detected by content hash on each request).
    """

    def __init__(self, timeout: float = 30.0) -> None:
        self._timeout = timeout
        self._client: httpx.Client | None = None
        self._token: str | None = None

    def _build_client(self) -> httpx.Client:
        token = _read_token()
        ca = _ca_bundle()
        host = _apiserver_host()
        client = httpx.Client(
            base_url=host,
            headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
            verify=ca,
            timeout=self._timeout,
        )
        self._token = token
        return client

    def _ensure_client(self) -> httpx.Client:
        # Detect token rotation by re-reading the file and comparing.
        current_token = _read_token()
        if self._client is None or current_token != self._token:
            if self._client is not None:
                self._client.close()
            self._client = self._build_client()
        return self._client

    def get(self, path: str, *, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """GET ``path`` on the apiserver, return parsed JSON.

        ``path`` is the apiserver URL path (e.g. ``/api/v1/namespaces/kars-sre/pods``).
        Raises httpx.HTTPStatusError on non-2xx so the caller can present a
        clear error to the agent.
        """
        client = self._ensure_client()
        resp = client.get(path, params=params)
        resp.raise_for_status()
        return resp.json()

    def post(self, path: str, *, json: dict[str, Any]) -> dict[str, Any]:
        """POST ``json`` to ``path`` on the apiserver, return parsed JSON.

        Used by the SRE plugin to CREATE KarsSREAction CRs (Slice 3 of
        kars-sre — typed apply-fix proposals). The SRE sandbox SA has
        ``create`` on ``karssreactions.kars.azure.com`` via the chart-
        shipped ``kars-sre-action-author`` ClusterRole.
        """
        client = self._ensure_client()
        resp = client.post(path, json=json)
        resp.raise_for_status()
        return resp.json()

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None
            self._token = None


_singleton: KubeClient | None = None


def client() -> KubeClient:
    """Return a process-wide singleton KubeClient."""
    global _singleton  # noqa: PLW0603 — process-singleton is intentional
    if _singleton is None:
        _singleton = KubeClient()
    return _singleton
