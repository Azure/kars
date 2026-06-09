"""Unit tests for kars_mesh_transfer_file (sender side) and
mesh_worker._maybe_save_file_transfer (receiver side).

The two halves must agree byte-for-byte with OpenClaw's plugin so a
file shipped from an OpenClaw agent lands cleanly on a Hermes peer
(and vice versa). Pins the wire shape, the path-traversal guard, the
size cap, the receiver's auto-save path and LLM-bound summary.
"""

from __future__ import annotations

import asyncio
import base64
import json
from pathlib import Path

import pytest

from kars_runtime_hermes.plugin import mesh, mesh_worker


@pytest.fixture()
def sandbox_root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Override SANDBOX_HOME + /sandbox/incoming to land inside tmp_path
    so tests don't write to the host filesystem."""
    sb = tmp_path / "sandbox"
    sb.mkdir()
    monkeypatch.setenv("SANDBOX_HOME", str(sb))
    monkeypatch.setenv("SANDBOX_NAME", "test-sender")
    return sb


# ── Sender: kars_mesh_transfer_file ────────────────────────────────


class _StubClient:
    """Minimal fake MeshClient — captures send_by_name calls so we can
    assert on what was put on the wire."""

    def __init__(self) -> None:
        self.sent: list[tuple[str, bytes]] = []

    async def send_by_name(self, *, to: str, payload: bytes) -> None:
        self.sent.append((to, payload))


def _patch_mesh_client(monkeypatch: pytest.MonkeyPatch, client: _StubClient) -> None:
    """Stub _get_or_init_client() + the asyncio loop so the sender's
    run_coroutine_threadsafe + future.result() round-trip works
    without spinning a real client."""
    monkeypatch.setattr(mesh, "_get_or_init_client", lambda: client)
    loop = asyncio.new_event_loop()
    import threading

    thread = threading.Thread(target=loop.run_forever, daemon=True)
    thread.start()
    monkeypatch.setattr(mesh, "_get_or_init_loop", lambda: loop)


def test_sender_missing_to_agent_returns_error(sandbox_root: Path) -> None:
    result = mesh._kars_mesh_transfer_file({"file_path": "x"})
    assert "to_agent" in json.loads(result)["error"]


def test_sender_missing_file_path_returns_error(sandbox_root: Path) -> None:
    result = mesh._kars_mesh_transfer_file({"to_agent": "peer"})
    assert "file_path" in json.loads(result)["error"]


def test_sender_resolves_relative_path_inside_sandbox(
    sandbox_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Relative file_path lands under $SANDBOX_HOME (the harness
    contract for sub-agents). Mirrors OpenClaw's workspaceRoot join."""
    (sandbox_root / "report.md").write_text("# hello\n")
    client = _StubClient()
    _patch_mesh_client(monkeypatch, client)

    result = mesh._kars_mesh_transfer_file(
        {"to_agent": "peer", "file_path": "report.md"}
    )
    parsed = json.loads(result)
    assert parsed["ok"] is True
    assert parsed["file_name"] == "report.md"
    assert parsed["bytes"] == 8
    assert len(client.sent) == 1
    assert client.sent[0][0] == "peer"


def test_sender_rejects_path_traversal_outside_sandbox(
    sandbox_root: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Absolute path outside the sandbox root is refused — sender
    cannot exfiltrate /etc/passwd or read another sandbox's secrets."""
    client = _StubClient()
    _patch_mesh_client(monkeypatch, client)

    # Drop a canary file OUTSIDE the test's sandbox_root but at a real
    # path on disk so we exercise the guard's "resolved candidate is
    # not under sandbox_root" branch rather than failing on `cannot
    # open`. tmp_path is the pytest temp dir; sandbox_root sits inside
    # tmp_path/sandbox/, so tmp_path/escape.txt is a sibling — outside
    # sandbox_root, but exists on disk for the test.
    canary = tmp_path / "escape.txt"
    canary.write_text("secret")
    result = mesh._kars_mesh_transfer_file(
        {"to_agent": "peer", "file_path": str(canary)}
    )
    parsed = json.loads(result)
    assert "error" in parsed
    assert "outside" in parsed["error"]
    assert "path traversal" in parsed["error"]
    assert client.sent == []


def test_sender_rejects_oversized_file(
    sandbox_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Files larger than 30 MiB are refused at the fstat step so we
    don't ever read them into memory. Sparse-allocate a 31 MiB file."""
    big = sandbox_root / "huge.bin"
    with open(big, "wb") as f:
        f.seek(31 * 1024 * 1024)
        f.write(b"\0")
    client = _StubClient()
    _patch_mesh_client(monkeypatch, client)

    result = mesh._kars_mesh_transfer_file(
        {"to_agent": "peer", "file_path": "huge.bin"}
    )
    parsed = json.loads(result)
    assert "too large" in parsed["error"]
    assert client.sent == []


def test_sender_wire_envelope_shape(
    sandbox_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The wire shape MUST match the OpenClaw envelope so a Hermes →
    OpenClaw transfer works without per-runtime decoders. Keys:
    type, file_name, file_path, file_data (base64), size_bytes,
    description, from_agent, timestamp."""
    (sandbox_root / "data.bin").write_bytes(b"\x00\x01\x02\x03")
    client = _StubClient()
    _patch_mesh_client(monkeypatch, client)

    result = mesh._kars_mesh_transfer_file(
        {
            "to_agent": "peer",
            "file_path": "data.bin",
            "description": "test payload",
        }
    )
    assert json.loads(result)["ok"] is True
    _, payload = client.sent[0]
    envelope = json.loads(payload.decode("utf-8"))
    assert envelope["type"] == "file_transfer"
    assert envelope["file_name"] == "data.bin"
    assert envelope["file_path"] == "data.bin"
    assert envelope["size_bytes"] == 4
    assert envelope["description"] == "test payload"
    assert envelope["from_agent"] == "test-sender"
    assert envelope["timestamp"].endswith("Z")
    # Base64 round-trip integrity.
    assert base64.b64decode(envelope["file_data"]) == b"\x00\x01\x02\x03"


# ── Receiver: mesh_worker._maybe_save_file_transfer ────────────────


class _FakeMsg:
    def __init__(self, payload: bytes, from_did: str = "did:mesh:test") -> None:
        self.payload = payload
        self.from_did = from_did


def test_receiver_non_json_payload_passes_through(monkeypatch: pytest.MonkeyPatch) -> None:
    """A plain text mesh message (not a file_transfer envelope) is
    forwarded to the LLM unchanged."""
    result = mesh_worker._maybe_save_file_transfer(
        "hello there", _FakeMsg(b"hello there"), client=None
    )
    assert result == "hello there"


def test_receiver_other_type_passes_through(monkeypatch: pytest.MonkeyPatch) -> None:
    """JSON payload that isn't `type=file_transfer` is preserved
    verbatim (might be a structured task another agent expects the
    LLM to interpret)."""
    payload = json.dumps({"type": "task", "task": "summarize"})
    result = mesh_worker._maybe_save_file_transfer(
        payload, _FakeMsg(payload.encode()), client=None
    )
    assert result == payload


def test_receiver_saves_file_and_returns_summary(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A well-formed file_transfer envelope lands at
    $KARS_INCOMING_DIR/<file_name> (defaults to /sandbox/incoming in
    production) and the LLM sees a short summary instead of the
    base64 blob."""
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))

    envelope = {
        "type": "file_transfer",
        "file_name": "report.md",
        "file_path": "report.md",
        "file_data": base64.b64encode(b"# report body").decode(),
        "size_bytes": 13,
        "description": "test report",
        "from_agent": "peer-agent",
        "timestamp": "2026-06-06T18:00:00Z",
    }
    payload_text = json.dumps(envelope)

    # Stub client so the file_transfer_ack scheduling doesn't crash
    class _StubAckClient:
        def __init__(self) -> None:
            self.sent: list[tuple[str, bytes]] = []

        async def send_by_name(self, *, to: str, payload: bytes) -> None:
            self.sent.append((to, payload))

    # The function uses asyncio.create_task — needs a running loop.
    async def _run() -> str:
        return mesh_worker._maybe_save_file_transfer(
            payload_text, _FakeMsg(payload_text.encode()), client=_StubAckClient()
        )

    result = asyncio.run(_run())
    # File is saved at the configured path
    saved = incoming / "report.md"
    assert saved.exists()
    assert saved.read_bytes() == b"# report body"
    # LLM gets a summary, not the base64
    assert "report.md" in result
    assert "13 bytes" in result
    assert "peer-agent" in result
    assert "test report" in result
    assert "file_data" not in result  # no base64 leaked into LLM context


def test_receiver_rejects_unsafe_filename(monkeypatch: pytest.MonkeyPatch) -> None:
    """A file_name that resolves to .. or absolute is refused — the
    sender does NOT control where we drop files on disk."""
    envelope = {
        "type": "file_transfer",
        "file_name": "../escape.sh",
        "file_data": base64.b64encode(b"evil").decode(),
    }
    payload_text = json.dumps(envelope)
    # The Path("..").name === "..", which the guard rejects.
    result = mesh_worker._maybe_save_file_transfer(
        payload_text, _FakeMsg(payload_text.encode()), client=None
    )
    # Function returns the ORIGINAL payload unchanged on guard reject —
    # the LLM at least sees the raw envelope and can decide to discard.
    assert result == payload_text


def test_receiver_malformed_envelope_returns_original(monkeypatch: pytest.MonkeyPatch) -> None:
    """Missing file_name / file_data fields trigger a warning + the
    original payload is forwarded; we don't crash the worker."""
    envelope = {"type": "file_transfer"}  # no file_name or file_data
    payload_text = json.dumps(envelope)
    result = mesh_worker._maybe_save_file_transfer(
        payload_text, _FakeMsg(payload_text.encode()), client=None
    )
    assert result == payload_text
