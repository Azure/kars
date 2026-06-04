# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Message envelopes surfaced to callers."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone


@dataclass(frozen=True)
class InboundMessage:
    """A decrypted message received from a peer over the mesh.

    Yielded by :meth:`MeshClient.inbox`. The library has already
    verified the X3DH session + Double Ratchet auth tag — by the time
    a caller sees this object, the bytes are guaranteed to come from
    the claimed peer DID and have not been tampered with in transit.
    """

    from_did: str
    """DID of the sending agent (e.g.
    ``did:agentmesh:base64url(ed25519_public_key)``)."""

    from_display_name: str | None
    """Best-effort display name resolution from the registry. May be
    ``None`` if the peer isn't currently registered or the registry
    lookup failed at message-receive time. Callers should treat the
    DID as the canonical identity and the display name as UI hint."""

    payload: bytes
    """The decrypted plaintext bytes the peer sent. The library does
    not interpret these — callers parse JSON, file chunks, custom
    framing, etc."""

    message_id: str
    """Per-message unique ID (UUIDv7). Survives across reconnects so
    callers can build idempotent ack/retry on top."""

    received_at: datetime
    """UTC timestamp when this message arrived at the local
    ``MeshClient``. Useful for stale-message detection and SLO
    metrics."""

    @classmethod
    def new(
        cls,
        *,
        from_did: str,
        from_display_name: str | None,
        payload: bytes,
        message_id: str,
    ) -> InboundMessage:
        return cls(
            from_did=from_did,
            from_display_name=from_display_name,
            payload=payload,
            message_id=message_id,
            received_at=datetime.now(timezone.utc),
        )
