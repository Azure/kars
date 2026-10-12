# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reserved installed-mission traffic must never reach the legacy responder."""

import json
from unittest.mock import AsyncMock, Mock

import pytest

from kars_runtime_hermes.plugin import mesh_worker


@pytest.mark.parametrize("auto_responder", ["0", "1"])
@pytest.mark.parametrize("dispatch_enabled", ["false", "true"])
@pytest.mark.parametrize("payload", [
    {"type": "mission:probe", "challenge": "unbound"},
    {"type": "mission:assign", "content": "do not run"},
    {"type": "mission:reply", "status": "succeeded"},
    {"type": "mission:future", "version": 999},
    {"type": "task_request", "content": json.dumps({"type": "mission:assign"})},
    {"type": "mission:assign", "content": json.dumps({
        "type": "file_transfer", "file_name": "must-not-save.txt", "file_data": "aGk=",
    })},
])
@pytest.mark.asyncio
async def test_reserved_envelopes_have_no_legacy_side_effects(
    payload, auto_responder, dispatch_enabled, monkeypatch, caplog,
):
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", auto_responder)
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", dispatch_enabled)
    transfer = Mock(side_effect=AssertionError("mission must not enter file transfer"))
    spawn = AsyncMock(side_effect=AssertionError("mission must not enter hermes -z"))
    trust = Mock(side_effect=AssertionError("mission must not publish generic peer trust"))
    resolve = AsyncMock(side_effect=AssertionError("mission must not resolve a generic peer"))
    monkeypatch.setattr(mesh_worker, "_maybe_save_file_transfer", transfer)
    monkeypatch.setattr(mesh_worker.asyncio, "create_subprocess_exec", spawn)
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", resolve)
    monkeypatch.setattr("kars_runtime_hermes.plugin.telemetry.submit_trust", trust)
    client = Mock(send_by_did=AsyncMock(), send_by_name=AsyncMock(), _kars_mission_receiver=None)
    msg = Mock(from_did="did:mesh:" + "a" * 32, payload=json.dumps(payload).encode())

    await mesh_worker._handle_message(client, msg)

    transfer.assert_not_called()
    spawn.assert_not_called()
    trust.assert_not_called()
    resolve.assert_not_called()
    client.send_by_did.assert_not_called()
    client.send_by_name.assert_not_called()
    assert "receiver disabled" in caplog.text
    assert "do not run" not in caplog.text
    assert "must-not-save.txt" not in caplog.text


@pytest.mark.parametrize("payload", [
    '{"type":"mission:assign","type":"file_transfer","file_data":"aGk="}',
    '{"type":"file_transfer","type":"mission:assign"}',
    '{"type":"task_request","content":"{\\"type\\":\\"mission:assign\\"}","content":"plain"}',
    '{"type":"task_request","type":"file_transfer","content":"{\\"type\\":\\"mission:assign\\"}"}',
    '{"type":"mission\\u003aassign"}',
    json.dumps({"type": "task_request", "content": json.dumps({
        "type": "task_request", "content": json.dumps({"type": "mission:assign"}),
    })}),
])
def test_reserved_discriminator_cannot_be_hidden_by_legacy_wrapping(payload):
    assert mesh_worker._is_reserved_mission(payload)


@pytest.mark.parametrize("payload", [
    "ordinary peer request",
    "Please explain mission:assign without executing a mission.",
    '{"type":"task_request","content":"ordinary request"}',
    '{"type":"file_transfer","file_name":"normal.txt","file_data":"aGk="}',
    json.dumps({"type": "task_request", "content": json.dumps({
        "type": "file_transfer", "file_name": "normal.txt", "file_data": "aGk=",
    })}),
    '{"type":"file_transfer_ack","file_name":"normal.txt"}',
    '"mission:assign"',
    '{"type":null}',
    '{"type":{}}',
    '["mission:assign"]',
])
def test_ordinary_peer_and_file_transfer_payloads_are_preserved(payload):
    assert not mesh_worker._is_reserved_mission(payload)


def test_deeply_nested_mission_does_not_reach_generic_responder():
    assert mesh_worker._is_reserved_mission("[" * 2000 + '{"type":"mission:assign"}' + "]" * 2000)


def test_decoder_recursion_limit_fails_closed(monkeypatch):
    monkeypatch.setattr(mesh_worker.json, "loads", Mock(side_effect=RecursionError))
    assert mesh_worker._is_reserved_mission('{"type":"mission:assign"}')


@pytest.mark.asyncio
async def test_ambiguous_mission_cannot_write_a_transferred_file(tmp_path, monkeypatch):
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "0")
    monkeypatch.setenv("KARS_INCOMING_DIR", str(tmp_path))
    client = Mock(send_by_did=AsyncMock(), send_by_name=AsyncMock(), _kars_mission_receiver=None)
    payload = (
        b'{"type":"mission:assign","type":"file_transfer",'
        b'"file_name":"must-not-save.txt","file_data":"aGk=","from_agent":"dispatcher"}'
    )
    msg = Mock(from_did="did:mesh:" + "a" * 32, payload=payload)

    await mesh_worker._handle_message(client, msg)

    assert list(tmp_path.iterdir()) == []
    client.send_by_did.assert_not_called()
    client.send_by_name.assert_not_called()
