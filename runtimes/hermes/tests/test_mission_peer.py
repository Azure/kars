# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual clean native subprocess, encrypted parent client, and pipe failure boundaries."""

import asyncio
import json
import os
import time
from unittest.mock import AsyncMock

import httpx
import pytest

from kars_runtime_hermes.plugin import mesh, mission_peer as peer, mission_protocol as wire
from kars_runtime_hermes.plugin import mission_receiver as receiver
from kars_agt_mesh import client as sdk
from test_mission_protocol import binding
import test_mission_mesh as fixtures
from test_mesh_auth import assert_lock

installed = fixtures.installed
runtime = fixtures.runtime
bootstrap_env = fixtures.bootstrap_env


def native_binding():
    target, contract, admitted, assignment = binding()
    contract["reviewedPhase"] = wire.phase(
        {
            **contract["reviewedPhase"],
            "executionMode": "native",
            "capabilities": ["filesystem-read", "filesystem-write", "network"],
            "maxToolCalls": 8,
        }
    )
    contract["phaseDigest"] = wire.digest(contract["reviewedPhase"])
    assignment.update(contract)
    return target, contract, admitted, assignment


BOOTSTRAP = """
import base64, json, runpy, sys, httpx
from pathlib import Path
class NoMeshImport:
    def find_spec(self, fullname, *args):
        if fullname.startswith('kars_agt_mesh') or fullname in (
                'kars_runtime_hermes.plugin.mesh', 'kars_runtime_hermes.plugin.mesh_worker'):
            raise AssertionError('child must never import a mesh client or responder')
sys.meta_path.insert(0, NoMeshImport())
count = 0
operations = [
    [('kars_discover', {'name':'peer', 'did':None}), ('kars_mesh_send', {'to_agent':'peer', 'content':'native peer answer'})],
    [('kars_mesh_await', {'senders':['peer'], 'timeout_seconds':0.2})],
    [('kars_mesh_inbox', {})],
    [('kars_mesh_await', {'timeout_seconds':0.03})],
    [('write_file', {'path':'exports/report.md','content':'Received encrypted peer findings'})],
]
seen = []
def provider(req):
    global count
    if req.url.path == '/agt/evaluate':
        body = json.loads(req.content)
        assert body['action'].startswith('tool:')
        assert set(body['context']) == {'tool_call_id','tool','arguments'}
        return httpx.Response(200, json={'allowed':True})
    assert req.url.path == '/v1/chat/completions'
    body = json.loads(req.content)
    tools = {t['function']['name'] for t in body.get('tools',[])}
    if count == 0:
        expected = {'kars_mesh_send','kars_discover','kars_mesh_inbox','kars_mesh_await','write_file','kars_mcp_call'}
        assert expected <= tools
    results = [json.loads(m['content']) for m in body['messages'] if m['role'] == 'tool']
    if count == 1:
        assert results[-2]['count'] == 1 and results[-1]['ok'] is True, results
    if count == 2:
        result = results[-1]
        assert result['completed'] and result['count'] == 1, result
        payload = base64.b64decode(result['messages'][0]['payload_b64'])
        assert payload == b'{"type":"task_request","content":"peer findings"}'
        assert result['messages'][0]['content'] == '{"type":"task_request","content":"peer findings"}'
    if count == 3:
        result = results[-1]
        assert result['count'] == 1, result
        assert base64.b64decode(result['messages'][0]['payload_b64']) == b'other sender retained'
        assert result['messages'][0]['content'] == 'other sender retained'
    if count == 4:
        assert results[-1] == {'messages':[], 'count':0, 'completed':False}, results
    calls = [{'id':f'call-{count}-{i}', 'type':'function',
              'function':{'name':name,'arguments':json.dumps(args)}}
             for i,(name,args) in enumerate(operations[count])] if count < len(operations) else None
    count += 1
    return httpx.Response(200,json={'id':'local','object':'chat.completion','created':1,'model':'mission-model',
        'choices':[{'index':0,'finish_reason':'tool_calls' if calls else 'stop',
                    'message':{'role':'assistant','content':None if calls else 'Native peer work complete',
                               'tool_calls':calls}}],
        'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}})
httpx.HTTPTransport = lambda **kw: httpx.MockTransport(provider)
runpy.run_module(sys.argv[1], run_name='__main__')
assert not list(Path.cwd().rglob('.mesh-prekeys.lock'))
from tools.registry import registry
peer_names = ('kars_mesh_send','kars_discover','kars_mesh_inbox','kars_mesh_await')
assert all(registry.get_entry(name) is None for name in peer_names)
"""


@pytest.mark.parametrize("installed", [True], indirect=True)
@pytest.mark.parametrize("large_assignment", [False, True])
def test_installed_receiver_real_native_peer_bridge(installed, runtime, monkeypatch, tmp_path, model_metadata, large_assignment):
    real_spawn = asyncio.create_subprocess_exec
    processes = []

    async def spawn(executable, dash_m, module, **kwargs):
        assert dash_m == "-m" and module == "kars_runtime_hermes.plugin.mission_native"
        assert "AGT_OAUTH_TOKEN" not in kwargs["env"]
        process = await real_spawn(executable, "-c", BOOTSTRAP, module, **kwargs)
        processes.append(process)
        return process

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    policy = AsyncMock(return_value=True)
    monkeypatch.setattr(receiver, "authorize", policy)
    client = installed.client
    r = client._kars_mission_receiver
    _, contract, _, assignment = native_binding()
    if large_assignment:
        original = assignment["inputArtifacts"][0]
        documents = wire.inputs([dict(original, name=f"memory-{i}.md", content="x" * 63000,
                                      sha256=wire.text_hash("x" * 63000)) for i in range(6)])
        contract["inputDigest"] = wire.digest(dict(schema="kars.mission-inputs/v1", inputs=documents))
        assignment.update(contract, inputArtifacts=documents, content="x" * 8192)
        r.admitted = wire.admission(dict(r.admitted, objectiveDigest=wire.text_hash(assignment["content"])))
        assert len(wire.encoded(assignment)) > wire.MAX_BASE
    r.contract = contract
    r.permit = AsyncMock(return_value=True)
    assignment.update(r.target, bootId=r.boot_id)
    assert wire.message(wire.encoded(assignment), r.contract, r.target, r.admitted) == assignment
    payload = b'{"type":"task_request","content":"peer findings"}'
    installed.receive(b'{"type":"mission:assign"}')
    installed.receive(b"other sender retained", "other")
    installed.receive(payload)
    original_send = client.send_by_did
    replies = []

    async def send(*, to, payload):
        if to == r.target["dispatcherDid"]:
            replies.append(wire.loads(payload))
        else:
            await original_send(to=to, payload=payload)

    client.send_by_did = send
    client._relay.send_frame = AsyncMock()

    def registry(req):
        from datetime import datetime, timezone

        assert req.method == "GET"
        agent = {"did": "did:mesh:" + "d" * 32, "capabilities": ["peer"],
                 "metadata": {"display_name": "peer"}, "last_seen": datetime.now(timezone.utc).isoformat()}
        if req.url.path == "/agt/registry/v1/discover":
            assert dict(req.url.params) == {"capability": "peer", "limit": "16"}
            return httpx.Response(200, json={"results": [agent], "total": 1})
        assert req.url.path == "/agt/registry/v1/agents/" + agent["did"]
        return httpx.Response(200, json=agent)

    async def run():
        client._registry._client = httpx.AsyncClient(
            base_url="http://127.0.0.1:8443/agt/registry",
            transport=httpx.MockTransport(registry),
        )
        try:
            from types import SimpleNamespace

            await r.receive(SimpleNamespace(from_did=r.target["dispatcherDid"], payload=wire.encoded(assignment)))
            await r.active
        finally:
            await client._registry._client.aclose()

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=45)
    assert replies[-1]["status"] == "succeeded", json.dumps(replies[-1])
    assert len(model_metadata["requests"]) == 1
    assert replies[-1]["artifacts"]["report.md"] == "Received encrypted peer findings"
    assert replies[-1]["evidence"]["phase"]["successfulToolCalls"] == 6
    assert [c.kwargs["peer_name"] for c in policy.call_args_list] == [
        "kars_discover",
        "kars_mesh_send",
        "kars_mesh_await",
        "kars_mesh_inbox",
        "kars_mesh_await",
    ]
    frames = client._relay.send_frame.call_args_list
    assert len(frames) == 1
    plaintext = installed.channels["peer"].receive(sdk._message_frame_to_encrypted(frames[0].args[0]))
    assert sdk._wire_bytes_to_payload(plaintext) == b"native peer answer"
    assert runtime.registry_factory.call_count == runtime.relay_factory.call_count == 1
    assert len(sdk._SINGLETONS) == 1 and mesh._MESH_SINGLETON is client
    assert_lock(client._config.identity_path.parent / ".mesh-prekeys.lock", held=True)
    assert processes[0].returncode == 0
    assert not list((tmp_path / "missions").iterdir())


@pytest.mark.parametrize(
    "boundary", ["denied", "owner", "ledger", "terminal", "singleton", "auth", "network", "reserved"]
)
def test_parent_assignment_authority_is_rechecked(installed, monkeypatch, boundary):
    client = installed.client
    r = client._kars_mission_receiver
    _, r.contract, _, assignment = native_binding()
    permit = AsyncMock(return_value=boundary != "denied")
    monkeypatch.setattr(receiver, "authorize", permit)
    installed.receive(b"not consumed without authority")

    async def run():
        owner = asyncio.current_task()
        entry = {"reply": {"status": "running"}}
        r.active = owner
        r.ledger[assignment["runNonce"]] = entry
        epoch = r.auth_epoch()
        if boundary == "owner":
            r.active = None
        elif boundary == "ledger":
            r.ledger.clear()
        elif boundary == "terminal":
            entry["reply"]["status"] = "succeeded"
        elif boundary == "singleton":
            monkeypatch.setattr(mesh, "_MESH_SINGLETON", None)
        elif boundary == "auth":
            client.connection_auth.generation += 1
        elif boundary == "network":
            r.contract["reviewedPhase"]["capabilities"].remove("network")
        name, args = (
            ("kars_mesh_send", {"to_agent": "peer", "content": '{"type":"mission:assign"}'})
            if boundary == "reserved"
            else ("kars_mesh_inbox", {})
        )
        if boundary == "denied":
            result = await r.native_peer(assignment, entry, epoch, owner, name, args)
            assert "denied" in result["error"]
        else:
            with pytest.raises(ValueError):
                await r.native_peer(assignment, entry, epoch, owner, name, args)
        assert client._inbox.qsize() == 1
        if boundary != "denied":
            permit.assert_not_awaited()
        r.active = None

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


def test_assignment_rechecked_after_router_await(installed, monkeypatch):
    r = installed.client._kars_mission_receiver
    _, r.contract, _, assignment = native_binding()
    installed.receive(b"policy race must not drain")

    async def run():
        owner = asyncio.current_task()
        r.active = owner
        entry = {"reply": {"status": "running"}}
        r.ledger[assignment["runNonce"]] = entry

        async def retire(*args, **kwargs):
            r.active = None
            return True

        monkeypatch.setattr(receiver, "authorize", retire)
        with pytest.raises(ValueError, match="no longer current"):
            await r.native_peer(assignment, entry, r.auth_epoch(), owner, "kars_mesh_inbox", {})
        assert installed.client._inbox.qsize() == 1

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


def test_cancelled_parent_wait_leaves_no_consumer(installed, monkeypatch):
    r = installed.client._kars_mission_receiver
    _, r.contract, _, assignment = native_binding()
    monkeypatch.setattr(receiver, "authorize", AsyncMock(return_value=True))

    async def run():
        r.active = asyncio.current_task()
        entry = {"reply": {"status": "running"}}
        r.ledger[assignment["runNonce"]] = entry
        waiting = asyncio.create_task(
            r.native_peer(assignment, entry, r.auth_epoch(), r.active, "kars_mesh_await", {"timeout_seconds": 300})
        )
        await asyncio.sleep(0.03)
        waiting.cancel()
        with pytest.raises(asyncio.CancelledError):
            await waiting
        await installed.deliver(b"after cancellation")
        assert (await mesh._drain_peer(installed.client))["count"] == 1
        r.active = None

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.parametrize("payload", [
    "Peer evidence: café → 再試行\nSecond line".encode(),
    b'{"type":"text","content":"untrusted peer findings"}',
    b"", b"\xff\xfe\x00",
])
@pytest.mark.parametrize("operation", ["inbox", "await"])
def test_peer_result_exposes_lossless_text_without_a_decoder(installed, payload, operation):
    import base64

    frame = installed.receive(payload)

    async def run():
        if operation == "inbox":
            result = await mesh._drain_peer(installed.client, max_bytes=peer.RESULT_BYTES)
        else:
            result = await mesh._await_peer(installed.client, [], 0.1, max_bytes=peer.RESULT_BYTES)
        assert result["count"] == 1
        message = result["messages"][0]
        assert base64.b64decode(message["payload_b64"]) == payload
        if payload.startswith(b"\xff"):
            assert "content" not in message
        else:
            assert message["content"] == payload.decode("utf-8")
        assert message["from_did"] == frame["from"]
        assert message["message_id"] == frame["id"] and message["received_at"]
        assert len(wire.encoded(result)) <= peer.RESULT_BYTES

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.asyncio
@pytest.mark.parametrize("target", ["peer", "did:mesh:" + "a" * 32, "did:mesh:bad", "did:other:bad"])
async def test_peer_send_selects_exact_address_without_fallback(monkeypatch, target):
    from types import SimpleNamespace

    client = SimpleNamespace(send_by_name=AsyncMock(), send_by_did=AsyncMock())
    monkeypatch.setattr(mesh, "_maybe_prepend_peer_roster", lambda content, recipient: content)
    result = json.loads(await mesh._send_peer(client, {"to_agent": target, "content": "readable peer reply"}))
    if target == "peer":
        client.send_by_name.assert_awaited_once_with(to=target, payload=b"readable peer reply")
        client.send_by_did.assert_not_awaited()
        assert result["ok"]
    elif target == "did:mesh:" + "a" * 32:
        client.send_by_did.assert_awaited_once_with(to=target, payload=b"readable peer reply")
        client.send_by_name.assert_not_awaited()
        assert result["ok"]
    else:
        assert result == {"error": "Invalid peer DID"}
        client.send_by_name.assert_not_awaited()
        client.send_by_did.assert_not_awaited()


@pytest.mark.parametrize("payload", [b"x" * 65536, ("再" * 12000).encode()])
def test_native_result_bound_preserves_omitted_messages(installed, payload):
    installed.receive(payload)
    installed.receive(b"y" * 65536)
    installed.receive(b"z" * (peer.RESULT_BYTES + 1))

    async def run():
        first = await mesh._drain_peer(installed.client, max_bytes=peer.RESULT_BYTES)
        assert first["count"] == 1 and first["truncated"]
        assert len(wire.encoded(first)) < peer.RESULT_BYTES
        second = await mesh._drain_peer(installed.client, max_bytes=peer.RESULT_BYTES)
        assert second["count"] == 1 and second["truncated"]
        third = await mesh._await_peer(installed.client, [], 0.03, max_bytes=peer.RESULT_BYTES)
        assert third == {"messages": [], "count": 0, "completed": False, "truncated": True}
        assert installed.client._inbox.qsize() == 1

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.parametrize("payload", [b"\x00" * 30000, ("再" * 20000).encode()])
def test_decoded_content_that_exceeds_budget_stays_queued(installed, payload):
    import base64

    installed.receive(payload)
    installed.receive(b"small readable reply")

    async def run():
        result = await mesh._drain_peer(installed.client, max_bytes=peer.RESULT_BYTES)
        assert result["count"] == 1 and result["truncated"]
        assert result["messages"][0]["content"] == "small readable reply"
        assert len(wire.encoded(result)) <= peer.RESULT_BYTES
        awaited = await mesh._await_peer(installed.client, [], 0.03, max_bytes=peer.RESULT_BYTES)
        assert awaited == {"messages": [], "count": 0, "completed": False, "truncated": True}
        assert installed.client._inbox.qsize() == 1
        retained = await mesh._drain_peer(installed.client)
        assert base64.b64decode(retained["messages"][0]["payload_b64"]) == payload
        assert retained["messages"][0]["content"] == payload.decode()

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.parametrize("use_did", [False, True])
def test_peer_reply_reuses_existing_encrypted_session(installed, use_did):
    frame = installed.receive(b"initial peer evidence")
    client = installed.client
    client._relay.send_frame = AsyncMock()
    target = frame["from"] if use_did else "peer"
    result = asyncio.run_coroutine_threadsafe(
        mesh._send_peer(client, {"to_agent": target, "content": "clarified peer conclusion"}),
        mesh._BACKGROUND_LOOP,
    ).result(timeout=5)
    assert json.loads(result)["ok"]
    client._relay.send_frame.assert_awaited_once()
    sent = client._relay.send_frame.call_args.args[0]
    assert sent["to"] == frame["from"]
    plaintext = installed.channels["peer"].receive(sdk._message_frame_to_encrypted(sent))
    assert sdk._wire_bytes_to_payload(plaintext) == b"clarified peer conclusion"
    assert client._registry.find_by_display_name.await_count == (0 if use_did else 1)


@pytest.mark.asyncio
async def test_parent_policy_context_comes_from_assignment(monkeypatch):
    _, _, _, assignment = native_binding()
    real_client = httpx.AsyncClient
    observed = []

    def policy(request):
        observed.append(json.loads(request.content))
        return httpx.Response(200, json={"allowed": True})

    monkeypatch.setattr(
        httpx, "AsyncClient", lambda **kwargs: real_client(**kwargs, transport=httpx.MockTransport(policy))
    )
    assert await receiver.authorize(
        assignment, peer_name="kars_mesh_send", peer_args={"to_agent": "peer", "content": "hello"}
    )
    body = observed[0]
    assert body["action"] == "mesh:send:peer"
    assert body["context"]["assignment_id"] == assignment["assignmentId"]
    assert body["context"]["run_nonce"] == assignment["runNonce"]
    assert body["context"]["task_uid"] == assignment["taskUid"]
    assert body["context"]["dispatcher_did"] == assignment["dispatcherDid"]
    assert body["context"]["phase_digest"] == assignment["phaseDigest"]


@pytest.mark.parametrize(
    "name,args",
    [
        ("kars_spawn", {}),
        ([], {}),
        ("kars_mesh_inbox", {"assignmentId": "other"}),
        ("kars_mesh_send", {"to_agent": "peer", "content": "hi", "identity": "other"}),
        ("kars_mesh_send", {"to_agent": "peer", "content": "é" * 65536}),
        ("kars_mesh_await", {"timeout_seconds": False}),
        ("kars_mesh_await", {"timeout_seconds": 301}),
        ("kars_mesh_await", {"senders": ["peer", "peer"]}),
        ("kars_discover", {"name": "peer", "did": "did:mesh:a"}),
        ("kars_discover", {"name": None, "did": None}),
        ("kars_discover", {"name": "peer", "did": ""}),
        ("kars_discover", {"name": "peer", "did": "/"}),
        ("kars_discover", {"name": None, "did": "did:mesh:bad"}),
        ("kars_discover", {"name": "peer", "did": "did:mesh:" + "a" * 32}),
        ("kars_mesh_send", {"to_agent": None, "content": "hello"}),
    ],
)
def test_peer_schema_rejects_authority_and_unbounded_arguments(name, args):
    with pytest.raises((ValueError, TypeError)):
        peer.validate_call(name, args)


@pytest.mark.parametrize("args", [
    {"name": "peer"}, {"name": "peer", "did": None},
    {"did": "did:mesh:" + "a" * 32}, {"name": None, "did": "did:mesh:" + "a" * 32},
])
def test_discovery_accepts_omitted_or_nullable_alternative(args):
    from kars_runtime_hermes.plugin.discover import _lookup

    peer.validate_call("kars_discover", args)
    query, direct, _, _ = _lookup(args)
    assert query == (args.get("name") or args["did"])
    assert direct == (args.get("name") is None)
    schema = peer.SCHEMAS["kars_discover"]["parameters"]
    assert "oneOf" not in schema
    assert all(schema["properties"][key]["type"] == ["string", "null"] for key in ("name", "did"))
    assert schema["additionalProperties"] is False


@pytest.mark.parametrize("raw", [b"{}\n{}\n", b'{"id":1,"id":2}\n', b"not-json\n", b'{"a":NaN}\n'])
def test_child_rejects_malformed_frames(raw):
    read, write = os.pipe()
    with os.fdopen(read, "rb", buffering=0) as stream, os.fdopen(write, "wb", buffering=0) as output:
        output.write(raw)
        with pytest.raises(ValueError):
            peer.read_frame(stream, 0.1)


def test_child_channel_exhaustion_cannot_emit_more_requests():
    read, write = os.pipe()
    with os.fdopen(read, "rb", buffering=0) as stream, os.fdopen(write, "wb", buffering=0) as output:
        sent = []
        channel = peer.ChildPeerChannel(stream, sent.append, 1)
        output.write(b'{"kind":"peer_result","id":1,"result":{"count":0}}\n')
        assert channel.call("kars_mesh_inbox", {}) == {"count": 0}
        with pytest.raises(ValueError, match="exhausted"):
            channel.call("kars_mesh_inbox", {})
        assert len(sent) == 1


def test_child_whole_frame_deadline_and_response_correlation():
    read, write = os.pipe()
    with os.fdopen(read, "rb", buffering=0) as stream, os.fdopen(write, "wb", buffering=0) as output:
        output.write(b"{")
        start = time.monotonic()
        with pytest.raises(ValueError, match="deadline"):
            peer.read_frame(stream, 0.03)
        assert 0.02 <= time.monotonic() - start < 0.5
        output.write(b'{"kind":"peer_result","id":2,"result":{}}\n')
        channel = peer.ChildPeerChannel(stream, lambda event: None, 2)
        with pytest.raises(ValueError, match="Mismatched"):
            channel.call("kars_mesh_inbox", {})
        assert channel.closed
        with pytest.raises(ValueError, match="closed"):
            channel.call("kars_mesh_inbox", {})


@pytest.mark.asyncio
@pytest.mark.parametrize("boundary", ["duplicate-id", "budget", "no-network", "large-result"])
async def test_parent_enforces_channel_authority_and_budget(boundary, monkeypatch, tmp_path, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    _, contract, _, assignment = native_binding()
    contract["reviewedPhase"]["maxToolCalls"] = 1
    if boundary == "no-network":
        contract["reviewedPhase"]["capabilities"].remove("network")
    real_spawn = asyncio.create_subprocess_exec
    processes = []
    script = '''
import json,sys
sys.stdin.buffer.readline()
print(json.dumps({'kind':'progress','evidence':{'phase':{'attemptedToolCalls':1}}}),flush=True)
event = {'kind':'peer_request','id':1,'name':'kars_mesh_inbox','arguments':{}}
print(json.dumps(event),flush=True)
sys.stdin.buffer.readline()
event['id'] = SECOND
print(json.dumps(event),flush=True)
sys.stdin.buffer.readline()
'''.replace("SECOND", "1" if boundary == "duplicate-id" else "2")

    async def spawn(executable, *args, **kwargs):
        process = await real_spawn(executable, "-c", script, **kwargs)
        processes.append(process)
        return process

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    callback = AsyncMock(return_value={"value": "x" * wire.MAX_WIRE} if boundary == "large-result" else {})
    with pytest.raises(ValueError):
        await asyncio.wait_for(receiver.native_execute(
            assignment, contract, "mission-model", AsyncMock(), peer_call=callback), 5)
    assert callback.await_count == (0 if boundary == "no-network" else 1)
    assert all(p.returncode is not None for p in processes)
    assert not list((tmp_path / "missions").iterdir())


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "violation", ["identity", "operation", "id", "no-attempt", "oversized", "eof", "pipeline", "cancel", "deadline"]
)
async def test_parent_pipe_rejects_bad_frames_and_cancels_waiters(violation, monkeypatch, tmp_path, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    _, contract, _, assignment = native_binding()
    event = dict(kind="peer_request", id=1, name="kars_mesh_await", arguments={"timeout_seconds": 300})
    if violation == "identity":
        event["arguments"]["agentDid"] = "other"
    if violation == "operation":
        event["name"] = "kars_spawn"
    if violation == "id":
        event["id"] = 2
    real_spawn = asyncio.create_subprocess_exec
    processes = []
    entered, cancelled = asyncio.Event(), asyncio.Event()
    script = """
import json,sys,time
sys.stdin.buffer.readline()
print(json.dumps({'kind':'progress','evidence':{'phase':{'attemptedToolCalls':ATTEMPT}}}),flush=True)
print(EVENT,flush=True)
AFTER
""".replace("ATTEMPT", "0" if violation == "no-attempt" else "1")
    script = script.replace("EVENT", repr("x" * (wire.MAX_WIRE + 1) if violation == "oversized" else json.dumps(event)))
    script = script.replace(
        "AFTER",
        "print('{}',flush=True)"
        if violation == "pipeline"
        else "pass"
        if violation == "eof"
        else "sys.stdin.buffer.readline()",
    )

    async def spawn(executable, *args, **kwargs):
        process = await real_spawn(executable, "-c", script, **kwargs)
        processes.append(process)
        return process

    async def wait(name, args):
        entered.set()
        try:
            await asyncio.Event().wait()
        finally:
            cancelled.set()

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    if violation == "deadline":
        real_timeout = asyncio.timeout
        monkeypatch.setattr(asyncio, "timeout", lambda seconds: real_timeout(0.2 if seconds == 600 else seconds))
    task = asyncio.create_task(
        receiver.native_execute(assignment, contract, "mission-model", AsyncMock(), peer_call=wait)
    )
    if violation == "cancel":
        await asyncio.wait_for(entered.wait(), 5)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    else:
        with pytest.raises((ValueError, OSError, TimeoutError)):
            await asyncio.wait_for(task, 5)
    if entered.is_set():
        assert cancelled.is_set()
    assert all(p.returncode is not None for p in processes)
    assert not list((tmp_path / "missions").iterdir())
