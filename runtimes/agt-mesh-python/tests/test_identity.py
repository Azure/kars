# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Tests for the IdentityStore — persistence + DID derivation."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from kars_agt_mesh.identity import IdentityStore


def test_create_then_load_round_trip(tmp_path: Path) -> None:
    p = tmp_path / "identity.json"
    fresh = IdentityStore.load_or_create(p)
    assert p.exists()
    # File is owner-only (0600)
    assert p.stat().st_mode & 0o777 == 0o600

    reloaded = IdentityStore.load_or_create(p)
    assert reloaded.ed25519_seed == fresh.ed25519_seed
    assert reloaded.x25519_secret == fresh.x25519_secret
    assert reloaded.did == fresh.did


def test_did_format(tmp_path: Path) -> None:
    p = tmp_path / "identity.json"
    identity = IdentityStore.load_or_create(p)
    # AGT registry derives DIDs as `did:mesh:<sha256(pubkey)[:32]>`
    # (see agent-governance-python/agent-mesh/src/agentmesh/registry/app.py).
    assert identity.did.startswith("did:mesh:")
    payload = identity.did.removeprefix("did:mesh:")
    assert len(payload) == 32  # first 32 hex chars of sha256


def test_did_is_deterministic(tmp_path: Path) -> None:
    p = tmp_path / "identity.json"
    fresh = IdentityStore.load_or_create(p)
    # Re-deriving from the same key MUST produce the same DID — the
    # registry uses this to look up agents, so any drift would orphan
    # every prior session.
    import hashlib

    expected = "did:mesh:" + hashlib.sha256(fresh.verify_key_bytes).hexdigest()[:32]
    assert fresh.did == expected


def test_persisted_file_shape(tmp_path: Path) -> None:
    p = tmp_path / "identity.json"
    identity = IdentityStore.load_or_create(p)
    raw = json.loads(p.read_text())
    assert raw["version"] == 1
    assert raw["did"] == identity.did
    assert "ed25519_seed" in raw and "x25519_secret" in raw


def test_corrupt_file_rejected(tmp_path: Path) -> None:
    p = tmp_path / "identity.json"
    p.write_text('{"version": 99, "x": 1}')
    with pytest.raises(RuntimeError, match="version 99"):
        IdentityStore.load_or_create(p)
