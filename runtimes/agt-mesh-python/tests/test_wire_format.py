# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Wire-format regression tests — guards the TS-SDK compatibility.

The TypeScript SDK (`@microsoft/agent-governance-sdk`) and this
Python library must produce byte-equivalent relay frames. An earlier
Python build packed `header + ciphertext` into a single urlsafe-b64
blob in the `ciphertext` field, which TS receivers silently dropped.
These tests pin the exact wire-shape so a future refactor can't
re-introduce that bug.
"""

from __future__ import annotations

import base64

import pytest
from agentmesh.encryption.channel import ChannelEstablishment
from agentmesh.encryption.ratchet import EncryptedMessage, MessageHeader

from kars_agt_mesh.client import (
    _b64std,
    _b64std_decode,
    _encrypted_to_message_frame,
    _establishment_to_wire,
    _message_frame_to_encrypted,
    _wire_to_establishment,
)


def test_establishment_wire_uses_ts_short_keys() -> None:
    est = ChannelEstablishment(
        initiator_identity_key=b"\x01" * 32,
        ephemeral_public_key=b"\x02" * 32,
        used_one_time_key_id=7,
    )
    out = _establishment_to_wire(est)
    # Must use TS-style short keys (ik/ek/otk), NOT the legacy
    # snake_case long-keys (initiator_identity_key / ...).
    assert set(out.keys()) == {"ik", "ek", "otk"}
    assert out["otk"] == 7
    # Must use STANDARD base64 (not urlsafe). Standard b64 of 32 bytes
    # of 0x01 contains '+' / '/' chars when raw bytes hit those
    # positions; urlsafe replaces them. We just verify the round-trip.
    assert base64.b64decode(out["ik"]) == est.initiator_identity_key
    assert base64.b64decode(out["ek"]) == est.ephemeral_public_key


def test_establishment_wire_omits_otk_when_none() -> None:
    est = ChannelEstablishment(
        initiator_identity_key=b"\x01" * 32,
        ephemeral_public_key=b"\x02" * 32,
        used_one_time_key_id=None,
    )
    out = _establishment_to_wire(est)
    assert "otk" not in out


def test_establishment_round_trip_ts_shape() -> None:
    est = ChannelEstablishment(
        initiator_identity_key=b"abc" * 11 + b"d",  # 34 bytes — non-power-of-2
        ephemeral_public_key=b"xyz" * 11 + b"q",
        used_one_time_key_id=42,
    )
    wire = _establishment_to_wire(est)
    parsed = _wire_to_establishment(wire)
    assert parsed.initiator_identity_key == est.initiator_identity_key
    assert parsed.ephemeral_public_key == est.ephemeral_public_key
    assert parsed.used_one_time_key_id == 42


def test_establishment_legacy_shape_still_accepted() -> None:
    """Bridge bias: while the fleet mid-upgrades from the old
    Python-only shape to the TS-compatible one, accept both."""
    legacy = {
        "initiator_identity_key": _b64std(b"\x01" * 32),
        "ephemeral_public_key": _b64std(b"\x02" * 32),
        "used_one_time_key_id": 9,
    }
    parsed = _wire_to_establishment(legacy)
    assert parsed.initiator_identity_key == b"\x01" * 32
    assert parsed.used_one_time_key_id == 9


def test_message_frame_uses_ts_structured_header() -> None:
    em = EncryptedMessage(
        header=MessageHeader(
            dh_public_key=b"\x03" * 32,
            previous_chain_length=11,
            message_number=7,
        ),
        ciphertext=b"opaque ciphertext bytes here",
    )
    frame = _encrypted_to_message_frame(em, "did:mesh:from", "did:mesh:to")
    assert frame["v"] == 1
    assert frame["type"] == "message"
    assert frame["from"] == "did:mesh:from"
    assert frame["to"] == "did:mesh:to"
    # Header MUST be a structured object with TS-style short keys
    # (dh/pn/n), NOT bundled into the ciphertext blob.
    assert isinstance(frame["header"], dict)
    assert set(frame["header"].keys()) == {"dh", "pn", "n"}
    assert frame["header"]["pn"] == 11
    assert frame["header"]["n"] == 7
    # std-base64 (not urlsafe)
    assert base64.b64decode(frame["header"]["dh"]) == em.header.dh_public_key
    assert base64.b64decode(frame["ciphertext"]) == em.ciphertext


def test_message_frame_round_trip_ts_shape() -> None:
    em = EncryptedMessage(
        header=MessageHeader(
            dh_public_key=b"\x04" * 32,
            previous_chain_length=0,
            message_number=0,
        ),
        ciphertext=b"x" * 100,
    )
    frame = _encrypted_to_message_frame(em, "did:from", "did:to")
    parsed = _message_frame_to_encrypted(frame)
    assert parsed.header.dh_public_key == em.header.dh_public_key
    assert parsed.header.previous_chain_length == 0
    assert parsed.header.message_number == 0
    assert parsed.ciphertext == em.ciphertext


def test_message_frame_legacy_packed_ciphertext_still_decodes() -> None:
    """During the upgrade window, accept the old packed shape so
    in-flight messages aren't lost. The legacy shape had no `header`
    field — the entire `EncryptedMessage.serialize()` blob was
    base64-encoded into `ciphertext`."""
    em = EncryptedMessage(
        header=MessageHeader(
            dh_public_key=b"\x05" * 32,
            previous_chain_length=3,
            message_number=2,
        ),
        ciphertext=b"y" * 50,
    )
    legacy_frame = {
        "v": 1,
        "type": "message",
        "ciphertext": _b64std(em.serialize()),
    }
    parsed = _message_frame_to_encrypted(legacy_frame)
    assert parsed.header.previous_chain_length == 3
    assert parsed.header.message_number == 2
    assert parsed.ciphertext == em.ciphertext


def test_b64std_tolerates_urlsafe_alphabet_on_decode() -> None:
    """A sender on the OLD Python build emits urlsafe-b64. The
    receiver should accept either alphabet to bridge the upgrade
    window without losing frames."""
    raw = b"\xff\xfe\xfd" * 10  # bytes that produce '-'/'_' in urlsafe alphabet
    urlsafe = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
    decoded = _b64std_decode(urlsafe)
    assert decoded == raw
