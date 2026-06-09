# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Tests for the prekey-writer flock guard in
:meth:`MeshClient._acquire_prekey_writer_lock`.

The guard protects the running daemon's mesh state from being silently
clobbered by a secondary Python process that imports the same plugin
and calls ``_get_or_init_client()`` (the most common cause: an operator
running ``kubectl exec ... python3 -c "..."`` against a live pod for
debugging).

Without the guard, the secondary process generates fresh X3DH key
material and PUTs it to the registry, after which the daemon's
in-memory ``signed_pre_key.private`` no longer matches the public bytes
peers fetch — every X3DH-derived shared secret diverges and every
incoming frame fails AEAD with InvalidTag. With the guard, the second
``MeshClient.connect()`` raises a clear ``MeshTransportError``
naming the lock path and holder pid.
"""

from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

from kars_agt_mesh.client import MeshClient
from kars_agt_mesh.config import MeshConfig
from kars_agt_mesh.errors import MeshTransportError

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="prekey-writer flock guard is a Linux-only protection",
)


def _make_config(tmp_path: Path, name: str = "harness-agent") -> MeshConfig:
    return MeshConfig(
        name=name,
        relay_url="ws://127.0.0.1:65535/agt/relay",
        registry_url="http://127.0.0.1:65535/agt/registry",
        identity_path=tmp_path / ".agt" / "identity.json",
        trust_threshold=0,
        user_agent="kars-agt-mesh-test/0.0.0",
    )


def test_lock_file_recorded_with_holder_pid(tmp_path: Path) -> None:
    """First process records its PID inside the lock file so an
    operator who runs ``cat .mesh-prekeys.lock`` can identify the
    holder without needing ``lsof`` or ``fuser``."""
    cfg = _make_config(tmp_path)
    # Bypass the network-touching parts of connect(): only invoke the
    # private guard method directly.
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(cfg)
    try:
        client._acquire_prekey_writer_lock()
        lock_path = cfg.identity_path.parent / ".mesh-prekeys.lock"
        assert lock_path.exists()
        assert lock_path.read_text().strip() == str(os.getpid())
    finally:
        client._release_prekey_writer_lock()


def test_second_process_fails_loud(tmp_path: Path) -> None:
    """A second process acquiring the same lock raises
    :class:`MeshTransportError` naming the holder PID — the exact
    operator-facing error this guard is designed to surface in place
    of the silent ``Decrypt failed`` log line the prior implementation
    produced."""
    import fcntl

    cfg = _make_config(tmp_path)
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(cfg)

    # Simulate the first-process holder by opening the same lock file
    # directly and grabbing an exclusive flock. We write a fake pid so
    # the error message has something to report.
    lock_path = cfg.identity_path.parent / ".mesh-prekeys.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    holder_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(holder_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        os.write(holder_fd, b"12345\n")
        os.fsync(holder_fd)

        with pytest.raises(MeshTransportError) as excinfo:
            client._acquire_prekey_writer_lock()
        msg = str(excinfo.value)
        # The error must name the lock path so operators can find it
        # via `kubectl exec ... ls -la` if they don't trust the
        # debugger.
        assert ".mesh-prekeys.lock" in msg
        # The error must mention the holder PID we wrote so operators
        # can `kill -0 <pid>` to verify.
        assert "12345" in msg
        # The error must give the right hint about the actual cause —
        # this is the only paragraph that closes the loop between a
        # confused operator and the fix.
        assert "kubectl exec" in msg
    finally:
        fcntl.flock(holder_fd, fcntl.LOCK_UN)
        os.close(holder_fd)


def test_lock_released_on_disconnect(tmp_path: Path) -> None:
    """After ``disconnect()`` a fresh client (or new process) can
    re-acquire the lock — the guard must not be a one-shot."""
    cfg = _make_config(tmp_path)
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(cfg)

    async def _exercise() -> None:
        client._acquire_prekey_writer_lock()
        # Simulate the disconnect path WITHOUT going through .connect()
        # — release explicitly then re-acquire.
        client._release_prekey_writer_lock()
        # Second acquire on the same client must succeed (lock is free).
        client._acquire_prekey_writer_lock()

    asyncio.run(_exercise())
    client._release_prekey_writer_lock()


def test_connect_propagates_loud_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """End-to-end check: ``MeshClient.connect()`` raises
    :class:`MeshTransportError` (not a generic ``OSError``) when the
    lock is held. Patches out registry+relay so the test doesn't try
    to reach the network."""
    import fcntl

    cfg = _make_config(tmp_path, name="locked-out-agent")
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()

    # Hold the lock from a sibling fd (mimics another process).
    lock_path = cfg.identity_path.parent / ".mesh-prekeys.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    holder_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    fcntl.flock(holder_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    try:
        # Stub out the registry constructor so we never touch the
        # network even if the guard accidentally lets us through.
        with patch(
            "kars_agt_mesh.client.RegistryClient",
            autospec=True,
        ) as registry_cls:
            registry_cls.return_value.register_self = AsyncMock()
            registry_cls.return_value.upload_prekeys = AsyncMock()
            client = MeshClient(cfg)
            with pytest.raises(MeshTransportError):
                asyncio.run(client.connect())
        # Registry must NOT have been called — the guard ran first.
        registry_cls.assert_not_called()
    finally:
        fcntl.flock(holder_fd, fcntl.LOCK_UN)
        os.close(holder_fd)
