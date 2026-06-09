"""Unit tests for peer-roster auto-prepend on kars_mesh_send.

Mirrors OpenClaw's `spawnedRoster` logic at
`runtimes/openclaw/src/core/agt-tools/agt.ts:545`. Pins the
suppression rules (parent, <2 entries, idempotent, exclude
recipient) and the byte-shape of the prepended header so a Hermes
analyst→viz→writer pipeline produces the same authoritative roster
context an OpenClaw equivalent would.
"""

from __future__ import annotations

import asyncio
import json
import threading

import pytest

from kars_runtime_hermes.plugin import mesh, spawn


@pytest.fixture(autouse=True)
def _clear_roster() -> None:
    """Each test starts with an empty process-local roster."""
    spawn._SPAWNED_ROSTER.clear()
    yield
    spawn._SPAWNED_ROSTER.clear()


@pytest.fixture()
def sender_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "parent-agent")
    yield


# ── pure helper tests (no MeshClient involved) ──────────────────────


def test_roster_empty_no_prefix(sender_env: None) -> None:
    """No siblings tracked → content passes through unchanged."""
    result = mesh._maybe_prepend_peer_roster("hello", "peer")
    assert result == "hello"


def test_roster_single_entry_no_prefix(sender_env: None) -> None:
    """One sibling = no ambiguity, no roster needed. Same threshold
    OpenClaw uses (≥2 distinct names besides parent + recipient)."""
    spawn._record_in_roster("analyst", "data analyst")
    result = mesh._maybe_prepend_peer_roster("hello", "analyst")
    # Recipient itself is excluded — and the only other entry is the
    # recipient — so the roster row count is 0.
    assert result == "hello"


def test_roster_two_entries_prefixed(sender_env: None) -> None:
    """Two siblings with the recipient being one of them → 1 row
    remains after recipient-exclusion. Mirrors OpenClaw's `rosterLines
    >= 1` gate (lines 546-562 of agt.ts)."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")
    result = mesh._maybe_prepend_peer_roster("draft this", "writer")
    assert result.startswith("Peer roster")
    assert "analyst" in result
    assert "data analyst" in result
    # Recipient itself is excluded — check that there's no roster
    # ROW for the recipient. (The word "writer" might appear in
    # another sibling's role description like "technical writer".)
    assert "\n  - writer" not in result
    assert "draft this" in result


def test_roster_parent_recipient_suppresses_prefix(sender_env: None) -> None:
    """`recipient == "parent"` (sub→parent traffic) never gets a
    roster prefix — siblings would just be noise back up to parent
    who owns the spawn truth anyway."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")
    spawn._record_in_roster("viz", "visualization engineer")
    result = mesh._maybe_prepend_peer_roster("done", "parent")
    assert result == "done"


def test_roster_idempotent_when_already_prefixed(sender_env: None) -> None:
    """If content already starts with `Peer roster:` (retried send,
    relayed message), don't double-stamp. Case-insensitive match
    matches OpenClaw's regex behaviour."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")
    pre = "Peer roster: (already set)\n\nrest of message"
    result = mesh._maybe_prepend_peer_roster(pre, "writer")
    assert result == pre
    # Also case-insensitive
    pre_upper = "PEER ROSTER: foo\n\nbar"
    assert mesh._maybe_prepend_peer_roster(pre_upper, "writer") == pre_upper


def test_roster_excludes_parent_sandbox_name(
    sender_env: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The parent's own SANDBOX_NAME never appears in the roster
    block — would confuse the sub-agent into self-targeting."""
    monkeypatch.setenv("SANDBOX_NAME", "exec-brief")
    spawn._record_in_roster("exec-brief", "root coordinator")  # the parent
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")
    result = mesh._maybe_prepend_peer_roster("task", "writer")
    # Result includes analyst (the non-recipient sibling).
    # No ROW for exec-brief (the parent) and no ROW for writer
    # (the recipient). Substring checks must be row-anchored because
    # role descriptions may contain other agent names.
    header = result.split("---")[0]
    assert "\n  - analyst" in header
    assert "\n  - exec-brief" not in header
    assert "\n  - writer" not in header


def test_roster_no_role_shows_just_name(sender_env: None) -> None:
    """A sibling spawned without a `role` arg shows up with just its
    name (no `— role` suffix)."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("nameless", None)
    result = mesh._maybe_prepend_peer_roster("task", "writer-not-in-roster")
    assert "  - analyst — data analyst" in result
    # `None` was normalized to empty string in _record_in_roster
    assert "  - nameless\n" in result or "  - nameless\n" in (result + "\n")


def test_roster_spawn_destroy_removes_entry(sender_env: None) -> None:
    """kars_spawn_destroy must drop the sibling from the roster so a
    later kars_mesh_send doesn't advertise a dead peer."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")
    spawn._remove_from_roster("analyst")
    result = mesh._maybe_prepend_peer_roster("task", "viz-elsewhere")
    # Only writer should remain in the roster after analyst was torn
    # down. Row-anchored substring checks so role text doesn't match.
    assert "\n  - writer" in result
    assert "\n  - analyst" not in result


# ── integration: _kars_mesh_send applies the roster ────────────────


def test_send_prefixes_payload_when_roster_populated(
    sender_env: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    """End-to-end through _kars_mesh_send: roster is applied to the
    UTF-8 payload before it hits client.send_by_name. Critical
    regression guard — the helper might do the right thing in
    isolation but only matters when wired into the send path."""
    spawn._record_in_roster("analyst", "data analyst")
    spawn._record_in_roster("writer", "technical writer")

    class _Capture:
        def __init__(self) -> None:
            self.sent: list[tuple[str, bytes]] = []

        async def send_by_name(self, *, to: str, payload: bytes) -> None:
            self.sent.append((to, payload))

    client = _Capture()
    monkeypatch.setattr(mesh, "_get_or_init_client", lambda: client)
    loop = asyncio.new_event_loop()
    threading.Thread(target=loop.run_forever, daemon=True).start()
    monkeypatch.setattr(mesh, "_get_or_init_loop", lambda: loop)

    result = mesh._kars_mesh_send(
        {"to_agent": "writer", "content": "write the brief"}
    )
    assert json.loads(result)["ok"] is True
    sent_payload = client.sent[0][1].decode("utf-8")
    assert sent_payload.startswith("Peer roster")
    assert "analyst — data analyst" in sent_payload
    assert "write the brief" in sent_payload
    # bytes count reflects the prefixed payload, not just the original
    assert json.loads(result)["bytes"] == len(sent_payload.encode("utf-8"))
