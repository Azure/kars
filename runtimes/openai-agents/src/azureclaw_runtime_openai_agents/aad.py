# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
AAD token broker for the in-pod adapter.

Acquires bearer tokens via Azure Workload Identity (AKS) using
`azure.identity.WorkloadIdentityCredential`. Tokens are cached by scope
and refreshed when within `_SKEW_SECONDS` of expiry so no caller pays
the IMDS round-trip on the hot path.

The router sidecar handles the LLM-side credential exchange — this
broker is for *in-process* needs (e.g. signed mesh envelopes, attestation
of a sub-agent spawn). The two paths must remain independent.
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass
from typing import Optional, Protocol

DEFAULT_SCOPE = "https://cognitiveservices.azure.com/.default"
_SKEW_SECONDS = 300  # refresh 5 minutes before the token actually expires


class _CredentialLike(Protocol):
    def get_token(self, *scopes: str) -> "_AccessTokenLike": ...  # pragma: no cover


class _AccessTokenLike(Protocol):
    token: str
    expires_on: int


@dataclass
class _CachedToken:
    token: str
    expires_on: int  # unix seconds


class TokenBroker:
    """Thread-safe per-scope cached AAD token broker."""

    def __init__(
        self,
        credential: Optional[_CredentialLike] = None,
        skew_seconds: int = _SKEW_SECONDS,
    ) -> None:
        self._credential = credential
        self._skew = skew_seconds
        self._cache: dict[str, _CachedToken] = {}
        self._lock = threading.Lock()

    def _build_default_credential(self) -> _CredentialLike:
        # Imported lazily so unit tests that inject a fake credential never
        # need azure-identity to be installed.
        from azure.identity import WorkloadIdentityCredential

        return WorkloadIdentityCredential()

    def _ensure_credential(self) -> _CredentialLike:
        if self._credential is None:
            self._credential = self._build_default_credential()
        return self._credential

    def get_token(self, scope: str = DEFAULT_SCOPE) -> str:
        now = int(time.time())
        with self._lock:
            cached = self._cache.get(scope)
            if cached is not None and cached.expires_on - self._skew > now:
                return cached.token
            credential = self._ensure_credential()
            access = credential.get_token(scope)
            self._cache[scope] = _CachedToken(
                token=access.token, expires_on=int(access.expires_on)
            )
            return access.token

    def invalidate(self, scope: Optional[str] = None) -> None:
        with self._lock:
            if scope is None:
                self._cache.clear()
            else:
                self._cache.pop(scope, None)


_default_broker: Optional[TokenBroker] = None
_default_broker_lock = threading.Lock()


def _broker() -> TokenBroker:
    global _default_broker
    if _default_broker is None:
        with _default_broker_lock:
            if _default_broker is None:
                _default_broker = TokenBroker()
    return _default_broker


def get_token(scope: str = DEFAULT_SCOPE) -> str:
    """Return a (cached) AAD bearer token for *scope*.

    Refreshes automatically when the token is within 5 minutes of expiry.
    """
    return _broker().get_token(scope)


def reset_default_broker() -> None:
    """Test hook — drop the process-wide singleton."""
    global _default_broker
    with _default_broker_lock:
        _default_broker = None
