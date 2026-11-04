# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import asyncio
import base64
import copy
import hmac
import json
from pathlib import Path
import socket
import subprocess
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import httpx
import pytest

from kars_runtime_hermes.plugin import mesh_worker
from kars_runtime_hermes.plugin import mission_protocol as wire
from kars_runtime_hermes.plugin import mission_receiver as runtime
from test_mission_protocol import binding


def measurement(contract, rounds=1, successful=1):
    phase = contract["reviewedPhase"]
    return dict(model="mission-model", rounds=rounds, usage=dict(promptTokens=10 * rounds,
                completionTokens=5 * rounds, totalTokens=15 * rounds), phase=dict(name=phase["name"],
                minToolCalls=phase["minToolCalls"], maxToolCalls=phase["maxToolCalls"],
                attemptedToolCalls=successful, successfulToolCalls=successful))


@pytest.fixture
def receiver():
    target, contract, admitted, assignment = binding()
    client = SimpleNamespace(send_by_did=AsyncMock())
    execute = AsyncMock(return_value=dict(kind="result", evidence=measurement(contract), output="Report ready",
                                         artifacts={"report.md": "Verified café evidence"}))
    receiver = runtime.MissionReceiver(client, target, contract, admitted, "mission-model",
                                       execute=execute, permit=AsyncMock(return_value=True))
    assignment["bootId"] = receiver.boot_id
    client._kars_mission_receiver = receiver
    return receiver, assignment


def message(receiver, assignment):
    return SimpleNamespace(from_did=receiver.target["dispatcherDid"], payload=wire.encoded(assignment))


def replies(receiver):
    calls = receiver.client.send_by_did.call_args_list
    assert all(call.kwargs["to"] == receiver.target["dispatcherDid"] for call in calls)
    return [wire.loads(call.kwargs["payload"], wire.MAX_WIRE) for call in calls]


async def finish(receiver):
    if receiver.active is not None:
        await receiver.active


@pytest.mark.asyncio
async def test_v5_context_survives_parent_execution_replies_and_replay(receiver):
    r, assignment = receiver
    peers = dict(rootUid="reviewed-root", stateName="kars-reviewed-" + "a" * 40, slot="r0-p0",
                 peers=[dict(role="writer", phase="draft", slot="r0-p0", agentName="rp-" + "a" * 20 + "-r0-p0")])
    r.contract = wire.contract({**r.contract, "version": 5, "peerContext": peers})
    assignment.update(r.contract)
    changed = copy.deepcopy(assignment)
    changed["peerContext"]["rootUid"] = "other-root"
    await r.receive(message(r, changed))
    missing = copy.deepcopy(assignment)
    del missing["peerContext"]
    await r.receive(message(r, missing))
    r.execute.assert_not_called()
    probe = {**r.target, **r.contract, "type": "mission:probe", "challenge": "probe-1", "runNonce": assignment["runNonce"]}
    await r.receive(message(r, probe))
    assert replies(r)[0]["peerContext"] == peers
    await r.receive(message(r, assignment))
    await finish(r)
    assert replies(r)[-1]["status"] == "succeeded"
    assert all(reply["peerContext"] == peers and reply["version"] == 5 for reply in replies(r))
    assert r.execute.await_args.args[:2] == (assignment, r.contract)
    await r.receive(message(r, assignment))
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("boundary", ["admission", "native", "progress"])
async def test_late_mission_callback_cannot_succeed_across_auth_epochs(receiver, boundary):
    import time
    from kars_runtime_hermes.plugin.mesh_auth import RequiredEntraLease

    from kars_agt_mesh import MeshClient

    r, assignment = receiver
    r.client._inbox = asyncio.Queue()
    r.client._reserved_inbox = asyncio.Queue()
    r.client._inbox_changed = asyncio.Event()
    r.client.clear_inboxes = lambda: MeshClient.clear_inboxes(r.client)
    auth = RequiredEntraLease(r.client, Mock())
    r.client.connection_auth = auth
    auth.ready = True
    auth.deadline, auth.wall_deadline = time.monotonic() + 60, time.time() + 60
    entered, release = asyncio.Event(), asyncio.Event()
    result = r.execute.return_value

    async def cross_epoch():
        entered.set()
        try:
            await release.wait()
        except asyncio.CancelledError:
            await release.wait()

    async def permit(*_):
        if boundary == "admission":
            await cross_epoch()
        return True

    async def execute(*args):
        await cross_epoch()
        if boundary == "progress":
            await args[-1](measurement(r.contract))
        return result

    r.permit = AsyncMock(side_effect=permit)
    r.execute = AsyncMock(side_effect=execute)
    await r.receive(message(r, assignment))
    task = r.active
    try:
        await entered.wait()
        auth.suspend()
        auth.ready = True
        release.set()
        await task
        entry = next(iter(r.ledger.values()))
        assert entry["reply"]["status"] == "failed"
        assert not any(reply["status"] in ("running", "succeeded", "failed") for reply in replies(r))
        if boundary == "admission":
            r.execute.assert_not_called()
        await r.receive(message(r, assignment))
        assert replies(r)[-1]["status"] == "failed", "Retry must replay failure, not re-execute"
    finally:
        release.set()
        await asyncio.gather(task, return_exceptions=True)
        auth.stop()


@pytest.mark.asyncio
async def test_sole_listener_probe_execution_and_terminal_replay(receiver):
    r, assignment = receiver
    probe = {**r.target, **r.contract, "type": "mission:probe", "challenge": "challenge", "runNonce": "nonce-1",
             "activityFormat": wire.ACTIVITY}
    await mesh_worker._handle_message(r.client, message(r, probe))
    ready = replies(r)[-1]
    assert ready["status"] == "ready" and ready["bootId"] == r.boot_id
    assert ready["activityFormat"] == wire.ACTIVITY
    await mesh_worker._handle_message(r.client, message(r, assignment))
    await finish(r)
    assert [x["status"] for x in replies(r)] == ["ready", "accepted", "succeeded"]
    terminal = replies(r)[-1]
    assert terminal["artifacts"] == {"report.md": "Verified café evidence"}
    await r.receive(message(r, assignment))
    assert replies(r)[-1] == terminal
    r.execute.assert_awaited_once()
    assert r.active is None
    assert_shared_parser_accepts([probe, assignment, *replies(r)])


def assert_shared_parser_accepts(messages):
    repo = Path(__file__).resolve().parents[3]
    protocol = (repo / "mesh-plugin/src/mission-protocol.ts").as_uri()
    script = f"""
        import {{registerHooks}} from 'node:module';
        import {{readFileSync}} from 'node:fs';
        registerHooks({{resolve(specifier, context, nextResolve) {{
            if (specifier.startsWith('./mission-') && specifier.endsWith('.js'))
                specifier = new URL(specifier.slice(0, -3) + '.ts', context.parentURL).href;
            return nextResolve(specifier, context);
        }}}});
        const {{parseMissionMessage}} = await import({json.dumps(protocol)});
        const messages = JSON.parse(readFileSync(0, 'utf8'));
        console.log(JSON.stringify(messages.map(value => parseMissionMessage(value))));
    """
    parsed = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", script],
                            input=wire.encoded(messages).decode("utf-8"),
                            capture_output=True, text=True, check=True)
    assert all(json.loads(parsed.stdout)), "Shared dispatcher must parse every real receiver reply"


@pytest.mark.asyncio
async def test_rejected_reserved_mission_never_enters_generic_responder(receiver, monkeypatch):
    r, assignment = receiver
    assignment["bootId"] = "stale-boot"
    monkeypatch.setattr(mesh_worker, "_maybe_save_file_transfer", Mock(side_effect=AssertionError("generic transfer")))
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", AsyncMock(side_effect=AssertionError("generic responder")))
    await mesh_worker._handle_message(r.client, message(r, assignment))
    assert replies(r) == [] and not r.ledger
    r.execute.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("negotiated", [True, False])
async def test_running_observations_require_negotiation_and_preserve_replay(receiver, negotiated):
    r, assignment = receiver
    if negotiated:
        assignment["activityFormat"] = wire.ACTIVITY
    content = wire.encoded(dict(version=1, source="runtime-forwarded-router-observations",
                                coverage="returned-responses-only", responses=1, rounds=[1],
                                state="unavailable", reason="Recorder expired")).decode()
    result = r.execute.return_value

    async def execute(*args):
        await args[-1](measurement(r.contract), content)
        return result

    r.execute.side_effect = execute
    await r.receive(message(r, assignment))
    await finish(r)
    delivered = replies(r)
    if negotiated:
        assert delivered[-2]["status"] == "running"
        assert delivered[-2]["routerObservations"] == content
        assert delivered[-2]["activityFormat"] == wire.ACTIVITY
        assert delivered[-1]["status"] == "succeeded"
    else:
        assert delivered[-1]["status"] == "failed"
        assert delivered[-1]["error"] == "Unnegotiated router observations"
        assert not any("routerObservations" in event for event in delivered)
    await r.receive(message(r, assignment))
    assert replies(r)[-1] == delivered[-1]
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["terminal", "exception"])
async def test_failed_run_retains_validated_activity_without_deliverables(receiver, failure):
    r, assignment = receiver
    assignment["activityFormat"] = wire.ACTIVITY
    content = wire.encoded(dict(version=1, source="runtime-forwarded-router-observations",
                                coverage="returned-responses-only", responses=1, rounds=[1],
                                state="unavailable", reason="Recorder expired")).decode()

    async def execute(*args):
        await args[-1](measurement(r.contract), content)
        await args[-1](measurement(r.contract))
        if failure == "exception":
            raise RuntimeError("Write failed")
        return dict(kind="error", evidence=measurement(r.contract), error="Write failed")

    r.execute.side_effect = execute
    await r.receive(message(r, assignment))
    await finish(r)
    delivered = replies(r)
    assert all(event["routerObservations"] == content for event in delivered if event["status"] == "running")
    terminal = delivered[-1]
    assert terminal["status"] == "failed" and terminal["error"] == "Write failed"
    assert terminal["routerObservations"] == content
    assert "output" not in terminal and "artifacts" not in terminal
    assert "routerObservations" not in terminal["evidence"]
    assert_shared_parser_accepts([assignment, *delivered])
    await r.receive(message(r, assignment))
    assert replies(r)[-1] == terminal
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("output", ["Exact-host approval is needed.", "🔥" * 4096, None, "", " ", {}, "🔥" * 4097])
async def test_failed_explanation_is_bounded_and_never_a_deliverable(receiver, output):
    r, assignment = receiver
    r.execute.return_value = dict(kind="error", error="Required exports missing",
                                  evidence=measurement(r.contract), output=output)
    await r.receive(message(r, assignment))
    await finish(r)
    terminal = replies(r)[-1]
    assert terminal["status"] == "failed"
    assert "artifacts" not in terminal
    if isinstance(output, str) and output.strip() and len(output.encode()) <= 16384:
        assert terminal["output"] == output
        assert terminal["error"] == "Required exports missing"
    else:
        assert "output" not in terminal
        assert terminal["error"] == "Invalid failed Mission explanation"
    assert_shared_parser_accepts([assignment, terminal])
    await r.receive(message(r, assignment))
    assert replies(r)[-1] == terminal
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("status", ["succeeded", "failed"])
async def test_128_tool_activity_survives_progress_terminal_and_shared_wire(receiver, status):
    from test_mission_observations import ordinary_observations

    r, assignment = receiver
    phase = wire.phase({**r.contract["reviewedPhase"], "executionMode": "native", "maxToolCalls": 128})
    r.contract = wire.contract({**r.contract, "reviewedPhase": phase, "phaseDigest": wire.digest(phase)})
    assignment.update(r.contract)
    assignment["activityFormat"] = wire.ACTIVITY
    collector, _ = ordinary_observations()
    content = collector.artifact()
    evidence = measurement(r.contract, rounds=129, successful=128)

    async def execute(*args):
        await args[-1](evidence, content)
        if status == "failed":
            return dict(kind="error", evidence=evidence, error="Terminal tool failed")
        return dict(kind="result", evidence=evidence, output="Report attached", artifacts={
            "report.md": "x" * (120 * 1024), wire.OBSERVATION_ARTIFACT: content})

    r.execute = AsyncMock(side_effect=execute)
    await r.receive(message(r, assignment))
    await finish(r)
    delivered = replies(r)
    assert delivered[-2]["status"] == "running" and delivered[-2]["routerObservations"] == content
    assert delivered[-1]["status"] == status
    assert all(wire.message_fits(reply) for reply in delivered)
    assert_shared_parser_accepts([assignment, *delivered])
    if status == "succeeded":
        assert delivered[-1]["artifacts"][wire.OBSERVATION_ARTIFACT] == content
    else:
        assert delivered[-1]["routerObservations"] == content and "artifacts" not in delivered[-1]


@pytest.mark.asyncio
async def test_sender_and_boot_custody(receiver):
    r, assignment = receiver
    msg = message(r, assignment)
    msg.from_did = "did:mesh:" + "f" * 32
    await r.receive(msg)
    assignment["bootId"] = "stale-boot"
    await r.receive(message(r, assignment))
    assert not r.ledger and replies(r) == []


@pytest.mark.asyncio
@pytest.mark.parametrize("permit", [False, TimeoutError("outage")])
async def test_admission_denial_or_outage_never_accepts_or_executes(receiver, permit):
    r, assignment = receiver
    r.permit = AsyncMock(side_effect=permit) if isinstance(permit, Exception) else AsyncMock(return_value=permit)
    await r.receive(message(r, assignment))
    await finish(r)
    assert [x["status"] for x in replies(r)] == ["failed"]
    r.execute.assert_not_called()


@pytest.mark.asyncio
async def test_pending_duplicate_does_not_acknowledge_before_authorization(receiver):
    r, assignment = receiver
    gate = asyncio.Event()

    async def permit(_):
        await gate.wait()
        return True

    r.permit = permit
    await r.receive(message(r, assignment))
    await asyncio.sleep(0)
    active = r.active
    await r.receive(message(r, assignment))
    assert r.active is active and replies(r) == []
    changed = {**assignment, "assignmentId": "replacement"}
    await r.receive(message(r, changed))
    assert replies(r) == []
    gate.set()
    await finish(r)
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
async def test_busy_and_full_ledger_never_evict_or_execute(receiver):
    r, assignment = receiver
    r.active = object()
    await r.receive(message(r, assignment))
    assert replies(r)[-1]["status"] == "rejected" and not r.ledger
    r.active = None
    r.ledger = {str(n): {"digest": b"old", "reply": None} for n in range(128)}
    await r.receive(message(r, assignment))
    assert len(r.ledger) == 128 and "nonce-1" not in r.ledger
    assert replies(r)[-1]["status"] == "rejected"
    r.execute.assert_not_called()


@pytest.mark.asyncio
async def test_failed_send_keeps_measured_terminal_for_retry(receiver):
    r, assignment = receiver
    r.client.send_by_did.side_effect = OSError("transport unavailable")
    await r.receive(message(r, assignment))
    await finish(r)
    terminal = r.ledger["nonce-1"]["reply"]
    assert terminal["status"] == "succeeded"
    r.client.send_by_did.side_effect = None
    await r.receive(message(r, assignment))
    assert replies(r)[-1] == terminal
    r.execute.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("mutation", ["model", "decrease", "unknown-usage", "required-output", "kind", "extra"])
async def test_malformed_or_regressing_native_terminal_fails(receiver, mutation):
    r, assignment = receiver
    result = copy.deepcopy(r.execute.return_value)
    if mutation == "model":
        result["evidence"]["model"] = "other-model"
    elif mutation == "decrease":
        result["evidence"] = measurement(r.contract, rounds=0)
    elif mutation == "unknown-usage":
        result["evidence"]["usage"] = None
    elif mutation == "required-output":
        result["artifacts"] = {}
    elif mutation == "kind":
        result["kind"] = "success"
    else:
        result["unreviewed"] = True

    async def execute(a, c, model, progress):
        await progress(measurement(c))
        return result

    r.execute = execute
    await r.receive(message(r, assignment))
    await finish(r)
    assert [x["status"] for x in replies(r)] == ["accepted", "running", "failed"]
    assert replies(r)[-1]["evidence"]["usage"] is None


@pytest.mark.asyncio
async def test_bad_progress_does_not_replace_last_valid_evidence(receiver):
    r, assignment = receiver

    async def execute(a, c, model, progress):
        await progress(measurement(c))
        await progress({**measurement(c, rounds=2), "model": "wrong"})

    r.execute = execute
    await r.receive(message(r, assignment))
    await finish(r)
    assert replies(r)[-1]["status"] == "failed"
    assert replies(r)[-1]["evidence"]["model"] == "mission-model"
    assert replies(r)[-1]["evidence"]["rounds"] == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("status,body", [(200, {"allowed": True}), (200, {"allowed": False}),
                                       (200, {"allowed": "true"}), (403, {"allowed": True}),
                                       (503, {}), (200, [True])])
async def test_real_http_admission_boundary(monkeypatch, status, body):
    _, _, _, assignment = binding()
    factory = httpx.AsyncClient
    seen = []

    def handler(request):
        seen.append(json.loads(request.content))
        assert str(request.url) == "http://127.0.0.1:8443/agt/evaluate"
        return httpx.Response(status, json=body)

    monkeypatch.setattr(runtime.httpx, "AsyncClient", lambda **kwargs: factory(
        **kwargs, transport=httpx.MockTransport(handler)))
    assert await runtime.authorize(assignment) is (status == 200 and body == {"allowed": True})
    assert seen[0]["action"] == "task:execute"
    context = seen[0]["context"]
    assert context["phase_digest"] == assignment["phaseDigest"]
    assert context["input_digest"] == assignment["inputDigest"]
    assert context["assignment_id"] == assignment["assignmentId"]


@pytest.fixture
def bootstrap_env(monkeypatch, tmp_path):
    target, contract, admitted, _ = binding()
    for key, value in zip(("TASK_NAME", "TASK_UID", "SANDBOX_UID", "POD_UID", "DISPATCHER_DID"),
                          (target[k] for k in ("taskName", "taskUid", "sandboxUid", "podUid", "dispatcherDid"))):
        monkeypatch.setenv("KARS_MISSION_" + key, value)
    monkeypatch.setenv("KARS_MISSION_IDENTITY_ROOT", "12" * 32)
    monkeypatch.setenv("KARS_MISSION_CONTRACT", wire.encoded(contract).decode())
    monkeypatch.setenv("KARS_MISSION_ADMISSION", wire.encoded(admitted).decode())
    monkeypatch.setenv("KARS_MODEL", "mission-model")
    monkeypatch.setattr(runtime, "_CUSTODY", None)
    monkeypatch.setattr(runtime, "_BOOTSTRAP", None)
    yield tmp_path
    if runtime._CUSTODY:
        runtime._CUSTODY.close()


def test_bootstrap_installs_full_bounded_v5_peer_map(bootstrap_env, monkeypatch):
    _, contract, _, _ = binding()
    peers = dict(rootUid="root", stateName="kars-reviewed-" + "a" * 40, slot="r0-p0", peers=[
        dict(role=f"role-{r}-" + "x" * 40, phase=f"phase-{p}-" + "x" * 39,
             slot=f"r{r}-p{p}", agentName="rp-" + "a" * 20 + f"-r{r}-p{p}")
        for r in range(8) for p in range(8)])
    expected = wire.contract({**contract, "version": 5, "peerContext": peers})
    raw = wire.encoded(expected).decode()
    assert len(raw.encode()) > 8192
    monkeypatch.setenv("KARS_MISSION_CONTRACT", raw)
    assert runtime.bootstrap(bootstrap_env)[2] == expected


def test_real_bootstrap_derivation_matches_node_ed25519(bootstrap_env):
    path, target, contract, admitted, model = runtime.bootstrap(bootstrap_env)
    seed = hmac.digest(bytes.fromhex("12" * 32), wire.encoded([
        "kars-mission-identity-v1", "runtime", target["sandboxUid"], target["podUid"]]), "sha256")
    script = f"""
        const c = require('node:crypto');
        const prefix = Buffer.from('302e020100300506032b657004220420','hex');
        const privateKey = c.createPrivateKey({{key:Buffer.concat([prefix,
          Buffer.from('{seed.hex()}','hex')]),format:'der',type:'pkcs8'}});
        const pub = c.createPublicKey(privateKey).export({{format:'der',type:'spki'}}).subarray(-32);
        console.log('did:mesh:' + c.createHash('sha256').update(pub).digest('hex').slice(0,32));
    """
    result = subprocess.run(["node", "-e", script], capture_output=True, text=True, check=True)
    assert target["agentDid"] == result.stdout.strip()
    assert path.stat().st_mode & 0o777 == 0o600
    assert base64.urlsafe_b64decode(json.loads(path.read_text())["ed25519_seed"] + "==") == seed
    assert runtime.bootstrap(bootstrap_env)[0] == path
    with socket.socket() as competitor:
        with pytest.raises(OSError):
            competitor.bind(("127.0.0.1", 19791))


@pytest.mark.parametrize("variable,value", [("KARS_MISSION_CONTRACT", ""), ("KARS_MISSION_IDENTITY_ROOT", "bad"),
                                          ("KARS_MODEL", "model with space"), ("KARS_MISSION_ADMISSION", "{}")])
def test_invalid_bootstrap_never_writes_identity(bootstrap_env, monkeypatch, variable, value):
    monkeypatch.setenv(variable, value)
    with pytest.raises(ValueError):
        runtime.bootstrap(bootstrap_env)
    assert not (bootstrap_env / ".agt").exists()


def test_bootstrap_refuses_symlink_identity_parent(bootstrap_env):
    actual = bootstrap_env / "actual"
    actual.mkdir()
    (bootstrap_env / ".agt").symlink_to(actual, target_is_directory=True)
    with pytest.raises(ValueError, match="symlinks"):
        runtime.bootstrap(bootstrap_env)
    assert list(actual.iterdir()) == []


def test_bootstrap_retry_detects_corrupted_persisted_identity(bootstrap_env, monkeypatch):
    path, *_ = runtime.bootstrap(bootstrap_env)
    data = json.loads(path.read_text())
    data["ed25519_seed"] = base64.urlsafe_b64encode(b"x" * 32).decode().rstrip("=")
    path.write_text(json.dumps(data))
    monkeypatch.setattr(runtime, "_BOOTSTRAP", None)
    with pytest.raises(ValueError, match="identity mismatch"):
        runtime.bootstrap(bootstrap_env)


@pytest.mark.asyncio
async def test_selected_model_capacity_uses_router_metadata(model_metadata):
    assert await runtime.model_limits("mission-model") == {"contextWindow": 1_050_000, "maxTokens": 128_000}
    assert len(model_metadata["requests"]) == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("field", ["context_window", "max_output_tokens"])
@pytest.mark.parametrize("value", [None, True, 0, -1, 1.5, 2**53])
async def test_model_capacity_rejects_invalid_limits(model_metadata, field, value):
    model_metadata["body"]["data"][0][field] = value
    with pytest.raises(ValueError, match="valid context/output limits"):
        await runtime.model_limits("mission-model")


@pytest.mark.asyncio
@pytest.mark.parametrize("data", [[], [{"id": "other"}], [{"id": "mission-model"}, {"id": "mission-model"}]])
async def test_model_capacity_requires_exact_selected_model(model_metadata, data):
    model_metadata["body"] = {"data": data}
    with pytest.raises(ValueError, match="match the selected model"):
        await runtime.model_limits("mission-model")


@pytest.mark.asyncio
async def test_model_capacity_bounds_response_and_refuses_unavailable_catalog(model_metadata):
    model_metadata["status"] = 503
    with pytest.raises(ValueError, match="metadata is unavailable"):
        await runtime.model_limits("mission-model")
    model_metadata["status"] = 200
    model_metadata["body"] = {"padding": "x" * 65537}
    with pytest.raises(ValueError, match="response bound"):
        await runtime.model_limits("mission-model")


@pytest.mark.asyncio
async def test_model_capacity_failure_never_starts_native_process(tmp_path, monkeypatch, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    model_metadata["status"] = 503
    _, contract, _, assignment = binding()
    spawn = AsyncMock()
    monkeypatch.setattr(runtime.asyncio, "create_subprocess_exec", spawn)
    with pytest.raises(ValueError, match="metadata is unavailable"):
        await runtime.native_execute(assignment, contract, "mission-model", AsyncMock())
    spawn.assert_not_called()
    assert list((tmp_path / "missions").iterdir()) == []


@pytest.mark.asyncio
async def test_subprocess_start_failure_cleans_only_owned_workspace(tmp_path, monkeypatch, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    sentinel = tmp_path / "keep.txt"
    sentinel.write_text("unrelated")
    _, contract, _, assignment = binding()
    monkeypatch.setattr(runtime.asyncio, "create_subprocess_exec", AsyncMock(side_effect=OSError("cannot spawn")))
    with pytest.raises(OSError, match="cannot spawn"):
        await runtime.native_execute(assignment, contract, "mission-model", AsyncMock())
    assert list((tmp_path / "missions").iterdir()) == []
    assert sentinel.read_text() == "unrelated"


@pytest.mark.asyncio
async def test_genuine_native_worker_128_tools_retains_progress_and_cleanup(tmp_path, monkeypatch, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    _, contract, _, assignment = binding()
    phase = wire.phase({**contract["reviewedPhase"], "executionMode": "native", "maxToolCalls": 128})
    contract = wire.contract({**contract, "reviewedPhase": phase, "phaseDigest": wire.digest(phase)})
    assignment.update(contract)
    real_spawn = asyncio.create_subprocess_exec
    bootstrap = '''
import json, runpy, sys, httpx
count = 0
def provider(req):
    global count
    if req.url.path == '/agt/evaluate':
        return httpx.Response(200, json={'allowed': True})
    assert req.url.path == '/v1/chat/completions'
    count += 1
    arguments = {'path':'inputs/facts.txt'} if count == 1 else {'path':'exports/report.md','content':f'Verified report revision {count}'}
    name = 'read_file' if count == 1 else 'write_file'
    calls = [{'id':f'call-{count}','type':'function','function':{'name':name,'arguments':json.dumps(arguments)}}]
    return httpx.Response(200,json={'id':'local','object':'chat.completion','created':1,'model':'mission-model',
        'choices':[{'index':0,'finish_reason':'tool_calls' if count <= 128 else 'stop',
         'message':{'role':'assistant','content':None if count <= 128 else 'Report ready',
                    'tool_calls':calls if count <= 128 else None}}],
        'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}})
httpx.HTTPTransport = lambda **kwargs: httpx.MockTransport(provider)
runpy.run_module(sys.argv[1], run_name='__main__')
'''

    async def spawn(executable, dash_m, module, **kwargs):
        assert dash_m == "-m" and module == "kars_runtime_hermes.plugin.mission_native"
        return await real_spawn(executable, "-c", bootstrap, module, **kwargs)

    monkeypatch.setattr(runtime.asyncio, "create_subprocess_exec", spawn)
    progress = AsyncMock()
    result = await runtime.native_execute(assignment, contract, "mission-model", progress)
    assert result["kind"] == "result", result
    assert result["evidence"]["rounds"] == 129
    assert result["evidence"]["usage"]["totalTokens"] == 1935
    assert result["evidence"]["phase"]["successfulToolCalls"] == 128
    assert result["artifacts"] == {"report.md": "Verified report revision 128"}
    assert progress.await_count > 128
    assert list((tmp_path / "missions").iterdir()) == []


@pytest.mark.asyncio
async def test_genuine_native_worker_clean_process_progress_and_exports(tmp_path, monkeypatch, model_metadata):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    monkeypatch.setenv("AGT_OAUTH_TOKEN", "must-not-reach-native")
    monkeypatch.setenv("HERMES_PLUGINS_PATH", "must-not-load")
    monkeypatch.setenv("HERMES_DISABLE_LAZY_INSTALLS", "0")
    monkeypatch.setenv("HERMES_SKIP_NODE_BOOTSTRAP", "0")
    monkeypatch.setenv("TIRITH_ENABLED", "true")
    monkeypatch.setenv("HERMES_LAZY_INSTALL_TARGET", str(tmp_path / "must-not-install"))
    monkeypatch.setenv("KARS_AGENT_INSTRUCTIONS", "Reviewed charter: preserve café facts.")
    _, contract, _, assignment = binding()
    events = []
    real_spawn = asyncio.create_subprocess_exec
    bootstrap = '''
import json, runpy, sys, httpx
count = 0
authorized = []
def provider(req):
    global count
    if str(req.url) == 'http://127.0.0.1:8443/agt/evaluate':
        assert req.method == 'POST' and count in (1, 2)
        name = 'kars_mission_read' if count == 1 else 'kars_mission_write'
        assert json.loads(req.content) == {
            'action': 'tool:' + name, 'context': {'tool_call_id': f'call-{count}'}}
        authorized.append(count)
        return httpx.Response(200, json={'allowed': True})
    count += 1
    assert str(req.url) == 'http://127.0.0.1:8443/v1/chat/completions'
    assert authorized == list(range(1, count))
    assert any('Reviewed charter: preserve café facts.' in m.get('content', '')
               for m in json.loads(req.content)['messages'])
    args = ({'path':'inputs/facts.txt'} if count == 1 else
            {'path':'exports/report.md','content':'Verified café evidence'})
    name = 'kars_mission_read' if count == 1 else 'kars_mission_write'
    calls = [{'id':f'call-{count}','type':'function','function':{'name':name,'arguments':json.dumps(args)}}]
    if count == 2:
        assert any('Evidence: café' in json.loads(m['content']).get('content','')
                   for m in json.loads(req.content)['messages'] if m.get('role') == 'tool')
    return httpx.Response(200,json={'id':'local','object':'chat.completion','created':1,'model':'mission-model',
        'choices':[{'index':0,'finish_reason':'tool_calls' if count < 3 else 'stop',
         'message':{'role':'assistant','content':None if count < 3 else 'Report ready',
                    'tool_calls':calls if count < 3 else None}}],
        'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}})
httpx.HTTPTransport = lambda **kwargs: httpx.MockTransport(provider)
runpy.run_module(sys.argv[1], run_name='__main__')
'''

    async def spawn(executable, dash_m, module, **kwargs):
        assert dash_m == "-m" and module == "kars_runtime_hermes.plugin.mission_native"
        env = kwargs["env"]
        assert "AGT_OAUTH_TOKEN" not in env and "HERMES_PLUGINS_PATH" not in env
        assert "KARS_MISSION_IDENTITY_ROOT" not in env
        assert "HERMES_LAZY_INSTALL_TARGET" not in env
        assert env["HERMES_DISABLE_LAZY_INSTALLS"] == "1"
        assert env["HERMES_SKIP_NODE_BOOTSTRAP"] == "1"
        assert env["TIRITH_ENABLED"] == "false"
        assert env["HOME"] == env["HERMES_HOME"]
        assert env["KARS_AGENT_INSTRUCTIONS"] == "Reviewed charter: preserve café facts."
        import yaml
        config = yaml.safe_load((Path(env["HOME"]) / "config.yaml").read_text())
        assert config["model"]["context_length"] == 1_050_000
        assert config["compression"]["enabled"] is False
        assert config["security"]["allow_lazy_installs"] is False
        assert Path(env["HOME"]).is_relative_to(tmp_path)
        assert not list(Path(env["HOME"]).glob("plugins/*"))
        return await real_spawn(executable, "-c", bootstrap, module, **kwargs)

    monkeypatch.setattr(runtime.asyncio, "create_subprocess_exec", spawn)

    async def progress(value):
        events.append(value)

    result = await runtime.native_execute(assignment, contract, "mission-model", progress)
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": "Verified café evidence"}
    assert result["evidence"]["usage"]["totalTokens"] == 45
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert len(events) >= 5
    assert list((tmp_path / "missions").iterdir()) == []
    assert not list(tmp_path.rglob(".mesh-prekeys.lock"))
