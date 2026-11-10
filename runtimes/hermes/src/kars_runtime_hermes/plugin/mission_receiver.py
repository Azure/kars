# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Installed receiver on the existing decrypted MeshClient inbox (not a scheduler)."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import hmac
import logging
import os
from pathlib import Path
import re
import socket
import sys
import uuid

import httpx

from . import mission_protocol as wire
from .mission_native import require_supported_runtime

logger = logging.getLogger("kars.hermes.missions")
_CUSTODY = None
_BOOTSTRAP = None


def bootstrap(home):
    """Acquire process custody before creating any deterministic mission identity."""
    global _CUSTODY, _BOOTSTRAP
    if _BOOTSTRAP is not None:
        return _BOOTSTRAP
    if _CUSTODY is None:
        custody = socket.socket()
        try:
            custody.bind(("127.0.0.1", 19791))
            custody.listen(1)
        except BaseException:
            custody.close()
            raise
        _CUSTODY = custody
    require_supported_runtime()
    target = {
        key: os.environ.get("KARS_MISSION_" + env, "")
        for key, env in (
            ("taskName", "TASK_NAME"),
            ("taskUid", "TASK_UID"),
            ("sandboxUid", "SANDBOX_UID"),
            ("podUid", "POD_UID"),
            ("dispatcherDid", "DISPATCHER_DID"),
        )
    }
    wire.require(all(wire.matches(wire.ID, target[k]) for k in ("taskName", "taskUid", "sandboxUid", "podUid")))
    wire.require(all(":" not in target[k] for k in ("sandboxUid", "podUid")))
    wire.require(wire.matches(wire.DID, target["dispatcherDid"]))
    root = os.environ.get("KARS_MISSION_IDENTITY_ROOT", "")
    wire.require(re.fullmatch(r"[a-fA-F0-9]{64}", root) is not None, "Missing mission identity root")
    seed = hmac.digest(
        bytes.fromhex(root),
        wire.encoded(["kars-mission-identity-v1", "runtime", target["sandboxUid"], target["podUid"]]),
        "sha256",
    )
    from kars_agt_mesh.identity import Identity, IdentityStore

    identity = Identity(seed, hmac.digest(seed, b"kars-hermes-mission-exchange-v1", "sha256"))
    target["agentDid"] = identity.did
    contract = wire.configured_contract(os.environ.get("KARS_MISSION_CONTRACT", ""))
    admitted = wire.admission(wire.loads(os.environ.get("KARS_MISSION_ADMISSION", ""), 8192))
    model = os.environ.get("KARS_MODEL") or os.environ.get("AZURE_OPENAI_DEPLOYMENT")
    wire.require(
        isinstance(model, str)
        and 0 < len(model) <= 200
        and model == model.strip()
        and not any(c.isspace() for c in model),
        "Installed missions require KARS_MODEL",
    )
    identity_path = Path(home) / ".agt" / "missions" / target["podUid"] / "identity.json"
    wire.require(
        not any(p.is_symlink() for p in (identity_path, *identity_path.parents)),
        "Mission identity path cannot contain symlinks",
    )
    identity_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if identity_path.exists():
        wire.require(IdentityStore.load_or_create(identity_path) == identity, "Mission identity mismatch")
    else:
        data = {
            "version": 1,
            "did": identity.did,
            "ed25519_seed": base64.urlsafe_b64encode(seed).rstrip(b"=").decode(),
            "x25519_secret": base64.urlsafe_b64encode(identity.x25519_secret).rstrip(b"=").decode(),
        }
        fd = os.open(identity_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as file:
            file.write(wire.encoded(data))
    _BOOTSTRAP = (identity_path, target, contract, admitted, model)
    return _BOOTSTRAP


async def authorize(assignment, *, peer_name=None, peer_args=None):
    context = {
        "task_uid": assignment["taskUid"],
        "run_nonce": assignment["runNonce"],
        "dispatcher_did": assignment["dispatcherDid"],
        "assignment_id": assignment["assignmentId"],
    }
    for snake, camel in (
        ("phase_digest", "phaseDigest"),
        ("reviewed_phase", "reviewedPhase"),
        ("input_digest", "inputDigest"),
    ):
        if camel in assignment:
            context[snake] = assignment[camel]
    action = "task:execute"
    if peer_name is not None:
        from .governance import _action_verb

        action = _action_verb(peer_name, peer_args)
        context.update(tool=peer_name, arguments=peer_args)
    try:
        async with httpx.AsyncClient(timeout=2, trust_env=False) as client:
            async with client.stream(
                "POST", "http://127.0.0.1:8443/agt/evaluate", json={"action": action, "context": context}
            ) as response:
                if response.status_code != 200:
                    return False
                body = bytearray()
                async for chunk in response.aiter_bytes():
                    body.extend(chunk)
                    if len(body) > 65536:
                        return False
                return wire.loads(bytes(body), 65536).get("allowed") is True
    except Exception:
        return False


async def model_limits(model):
    async with asyncio.timeout(15):
        async with httpx.AsyncClient(timeout=15, trust_env=False, follow_redirects=False) as client:
            async with client.stream("GET", "http://127.0.0.1:8443/v1/models") as response:
                wire.require(response.status_code == 200, "Native model metadata is unavailable")
                body = bytearray()
                async for chunk in response.aiter_bytes():
                    wire.require(len(body) + len(chunk) <= 65536, "Native model metadata exceeds its response bound")
                    body.extend(chunk)
                data = wire.loads(bytes(body), 65536).get("data")
                wire.require(
                    isinstance(data, list)
                    and len(data) == 1
                    and isinstance(data[0], dict)
                    and data[0].get("id") == model,
                    "Native model metadata does not match the selected model",
                )
                context = data[0].get("context_window")
                output = data[0].get("max_output_tokens")
                wire.require(
                    wire.integer(context, 1, 2**53 - 1) and wire.integer(output, 1, 2**53 - 1),
                    "Native model metadata has no valid context/output limits",
                )
                return {"contextWindow": context, "maxTokens": output}


async def native_execute(assignment, contract, model, progress, *, peer_call=None):
    from kars_runtime_hermes.plugin import mission_native, mission_peer
    import kars_runtime_hermes
    import shutil

    home = Path(os.environ.get("HERMES_HOME", "/sandbox/.hermes"))
    work = home / "missions" / str(uuid.uuid4())
    wire.require(not any(p.is_symlink() for p in (work, *work.parents)), "Mission work path contains symlinks")
    work.mkdir(parents=True, mode=0o700)
    process = reader = pending_peer = None
    interruption = None
    phase = contract.get("reviewedPhase", {})
    peer_enabled = bool(peer_call and phase.get("executionMode") == "native" and "network" in phase["capabilities"])
    try:
        limits = await model_limits(model)
        for directory in ("inputs", "exports", "home", "scratch"):
            (work / directory).mkdir(mode=0o700)
        for item in assignment.get("inputArtifacts", []):
            wire.require(wire.input_name(item["name"]))
            path = work / "inputs" / item["name"]
            path.write_text(item["content"], encoding="utf-8")
            path.chmod(0o400)
        # A clean HOME prevents native plugin discovery and a second prekey writer.
        (work / "home" / "config.yaml").write_text(
            f"model:\n  context_length: {limits['contextWindow']}\ncompression:\n  enabled: false\n"
            "memory:\n  memory_enabled: false\n  user_profile_enabled: false\n"
            "security:\n  allow_lazy_installs: false\n"
            "toolsets: []\nmcp_servers: {}\nplugins: {}\n",
            encoding="utf-8",
        )
        source = Path(kars_runtime_hermes.__file__).resolve().parent.parent
        env = {
            "HOME": str(work / "home"),
            "HERMES_HOME": str(work / "home"),
            "TMPDIR": str(work / "scratch"),
            "PYTHONPATH": str(source),
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONNOUSERSITE": "1",
            "PATH": str(Path(sys.executable).parent) + ":/usr/bin:/bin",
            "HERMES_TELEMETRY_ENABLED": "false",
            "HERMES_DISABLE_LAZY_INSTALLS": "1",
            "HERMES_SKIP_NODE_BOOTSTRAP": "1",
            "TIRITH_ENABLED": "false",
        }
        for key in ("KARS_SKILL_CATALOG_DIGEST", "KARS_AGENT_INSTRUCTIONS"):
            if key in os.environ:
                env[key] = os.environ[key]
        request = {**assignment, "contract": contract, "model": model, "modelLimits": limits, "peerTools": peer_enabled}
        async with asyncio.timeout(wire.native_timeout_seconds(phase)):
            process = await asyncio.create_subprocess_exec(
                sys.executable,
                "-m",
                mission_native.__name__,
                cwd=work,
                env=env,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
                limit=wire.MAX_WIRE + 1,
            )
            initial = wire.encoded(request) + b"\n"
            wire.require(
                len(initial) <= wire.MAX_WIRE and wire.message_fits(request), "Native assignment exceeds frame limit"
            )
            process.stdin.write(initial)
            await process.stdin.drain()
            result = None
            events = sequence = attempted = 0
            event_limit = wire.native_event_limit(phase)
            if not phase or phase.get("executionMode") != "native":
                event_limit += phase["maxToolCalls"] if peer_enabled else 0
            while True:
                if reader is None:
                    reader = asyncio.create_task(process.stdout.readline())
                line = await reader
                reader = None
                if not line:
                    break
                events += 1
                wire.require(events <= event_limit, "Too many native events")
                wire.require(line.endswith(b"\n"), "Incomplete native frame")
                event = wire.loads(line, wire.MAX_WIRE)
                wire.require(wire.message_fits(event), "Native event exceeds frame limit")
                wire.require(result is None, "Event after native terminal")
                if event.get("kind") == "progress":
                    wire.keys(event, ("kind", "evidence", "routerObservations"), ("kind", "evidence"))
                    if "routerObservations" in event:
                        await progress(event["evidence"], event["routerObservations"])
                    else:
                        await progress(event["evidence"])
                    attempted = event["evidence"].get("phase", {}).get("attemptedToolCalls", 0)
                elif event.get("kind") == "peer_request":
                    wire.keys(event, ("kind", "id", "name", "arguments"), ("kind", "id", "name", "arguments"))
                    wire.require(
                        peer_enabled
                        and wire.integer(event["id"], sequence + 1, sequence + 1)
                        and sequence < phase["maxToolCalls"]
                        and wire.integer(attempted, sequence + 1, phase["maxToolCalls"]),
                        "Native peer request has no reserved assignment invocation",
                    )
                    mission_peer.validate_call(event["name"], event["arguments"])
                    sequence += 1
                    # One stdout reader also observes EOF/pipelining during a blocked peer wait.
                    reader = asyncio.create_task(process.stdout.readline())
                    pending_peer = asyncio.create_task(peer_call(event["name"], event["arguments"]))
                    wait = event["arguments"].get("timeout_seconds", 300 if event["name"] == "kars_mesh_await" else 30)
                    async with asyncio.timeout(wait + 5):
                        done, _ = await asyncio.wait((reader, pending_peer), return_when=asyncio.FIRST_COMPLETED)
                        wire.require(reader not in done, "Native pipe closed or pipelined during peer request")
                        peer_result = await pending_peer
                        pending_peer = None
                        wire.require(type(peer_result) is dict, "Invalid native peer result")
                        response = wire.encoded({"kind": "peer_result", "id": sequence, "result": peer_result}) + b"\n"
                        wire.require(len(response) <= wire.MAX_BASE, "Native peer result exceeds frame limit")
                        process.stdin.write(response)
                        await process.stdin.drain()
                elif event.get("kind") in ("result", "error"):
                    result = event
                    process.stdin.close()
                else:
                    raise ValueError("Invalid native event")
            code = await process.wait()
            wire.require(code == 0 and result is not None, "Native worker exited without measured result")
        for item in assignment.get("inputArtifacts", []):
            path = work / "inputs" / item["name"]
            wire.require(
                not path.parent.is_symlink()
                and not path.is_symlink()
                and mission_native.read_file_bytes(path) == item["content"].encode(),
                "Immutable input changed",
            )
        return result
    except BaseException as error:
        interruption = error
        raise
    finally:
        pending = [task for task in (reader, pending_peer) if task is not None]
        for task in pending:
            task.cancel()
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)
        if process is not None:
            process.stdin.close()
            if process.returncode is None:
                process.terminate()
                try:
                    # Let a running Python supervisor clean descendants and return its measured receipt.
                    async with asyncio.timeout(8):
                        while line := await process.stdout.readline():
                            wire.require(len(line) <= wire.MAX_WIRE, "Native cancellation frame exceeds bound")
                            event = wire.loads(line)
                            if interruption is not None and event.get("kind") == "error":
                                interruption.native_result = event
                        await process.wait()
                except (Exception, asyncio.CancelledError):
                    if process.returncode is None:
                        process.kill()
                    await process.wait()
        # Only this mission's bounded scratch tree is removed, never shared caches.
        shutil.rmtree(work)


def interrupted_python_evidence(error, phase, previous, model, assignment):
    result = getattr(error, "native_result", None)
    if not isinstance(result, dict):
        return {}
    try:
        wire.require(
            result.get("kind") == "error"
            and assignment.get("artifactFormat") == "text-v1"
            and phase.get("executionMode") == "native"
            and "python-execute" in phase["capabilities"]
        )
        measured = wire.evidence(result["evidence"], phase, previous, success=False)
        wire.require(measured["model"] == model)
        exports = wire.artifacts(result["artifacts"])
        wire.require(
            set(exports) == {"kars-python-executions.json"}
            and len(exports["kars-python-executions.json"].encode()) <= 65536
        )
        return {"evidence": measured, "artifacts": exports}
    except (KeyError, ValueError, TypeError):
        return {}


class MissionReceiver:
    def __init__(self, client, target, contract, admitted, model, *, execute=native_execute, permit=authorize):
        self.client = client
        self.target = wire.loads(wire.encoded(target))
        self.contract = wire.contract(contract)
        self.admitted = wire.admission(admitted)
        self.model, self.execute, self.permit = model, execute, permit
        self.boot_id = str(uuid.uuid4())
        self.ledger = {}
        self.active = None
        self.send_lock = asyncio.Lock()

    def reply(self, assignment, status, **fields):
        result = {
            **self.target,
            **self.contract,
            "type": "mission:reply",
            "runNonce": assignment["runNonce"],
            "bootId": self.boot_id,
            "status": status,
        }
        for key in ("challenge", "assignmentId", "artifactFormat", "activityFormat"):
            if key in assignment:
                result[key] = assignment[key]
        return {**result, **fields}

    def auth_epoch(self):
        from .mesh_auth import RequiredEntraLease

        auth = getattr(self.client, "connection_auth", None)
        if isinstance(auth, RequiredEntraLease):
            auth.check()
            return auth, auth.generation
        return None

    def check_epoch(self, epoch):
        if epoch is not None:
            auth, generation = epoch
            wire.require(
                auth is self.client.connection_auth and auth.generation == generation, "Mesh authorization changed"
            )
            auth.check()

    async def native_peer(self, assignment, entry, epoch, owner, name, args):
        from types import SimpleNamespace
        from . import mesh, mission_peer
        from .discover import discover_peer
        from .mesh_worker import _is_reserved_message

        def current():
            self.check_epoch(epoch)
            wire.require(
                self.active is owner
                and not owner.done()
                and self.ledger.get(assignment["runNonce"]) is entry
                and entry.get("reply", {}).get("status") in ("accepted", "running")
                and mesh._MESH_SINGLETON is self.client
                and getattr(self.client, "_kars_mission_receiver", None) is self
                and self.client._inbox_partition is not None
                and asyncio.get_running_loop() is mesh._BACKGROUND_LOOP,
                "Native peer assignment is no longer current",
            )

        current()
        phase = self.contract.get("reviewedPhase", {})
        wire.require(
            phase.get("executionMode") == "native" and "network" in phase["capabilities"],
            "Native peer tools require assigned network authority",
        )
        mission_peer.validate_call(name, args)
        if name == "kars_mesh_send":
            wire.require(
                not _is_reserved_message(SimpleNamespace(payload=args["content"].encode())),
                "Native peer tools cannot send reserved Mission envelopes",
            )
        allowed = await authorize(assignment, peer_name=name, peer_args=args)
        current()
        if not allowed:
            return {"error": "Native peer operation denied by router policy"}
        try:
            if name == "kars_mesh_send":
                result = wire.loads(await mesh._send_peer(self.client, args))
            elif name == "kars_mesh_inbox":
                result = await mesh._drain_peer(self.client, max_bytes=mission_peer.RESULT_BYTES)
            elif name == "kars_mesh_await":
                result = await mesh._await_peer(
                    self.client,
                    args.get("senders", []),
                    args.get("timeout_seconds", 300),
                    max_bytes=mission_peer.RESULT_BYTES,
                )
            else:
                result = await discover_peer(self.client, args)
            wire.require(
                len(wire.encoded(result)) <= mission_peer.RESULT_BYTES, "Native peer result exceeds byte limit"
            )
        except Exception as exc:
            result = {"error": str(exc)[:512], "partial": []}
        current()
        return result

    async def send(self, reply, epoch=None):
        async with self.send_lock:
            try:
                self.check_epoch(epoch)
                payload = wire.encoded(reply)
                wire.require(wire.message_fits(reply))
                async with asyncio.timeout(10):
                    await self.client.send_by_did(to=self.target["dispatcherDid"], payload=payload)
            except Exception:
                logger.warning("Mission reply retained for correlated retry")

    async def receive(self, msg):
        self.auth_epoch()
        if msg.from_did != self.target["dispatcherDid"]:
            return
        try:
            assignment = wire.message(msg.payload, self.contract, self.target, self.admitted)
        except (ValueError, TypeError, UnicodeError, RecursionError):
            return
        if assignment["type"] == "mission:probe":
            await self.send(self.reply(assignment, "ready"))
            return
        if assignment["bootId"] != self.boot_id:
            return
        nonce = assignment["runNonce"]
        fingerprint = hashlib.sha256(wire.encoded(assignment)).digest()
        if nonce in self.ledger:
            entry = self.ledger[nonce]
            if entry["digest"] == fingerprint and entry["reply"] is not None:
                await self.send(entry["reply"])
            return
        if self.active is not None or len(self.ledger) >= 128:
            await self.send(self.reply(assignment, "rejected", error="Receiver busy or ledger full"))
            return
        entry = {"digest": fingerprint, "reply": None}
        self.ledger[nonce] = entry
        self.active = asyncio.create_task(self.run(assignment, entry))
        from .mesh_auth import RequiredEntraLease

        auth = getattr(self.client, "connection_auth", None)
        if isinstance(auth, RequiredEntraLease):
            auth.track(self.active)

    async def run(self, assignment, entry):
        previous = None
        latest_observations = None
        epoch = None
        phase = self.contract.get("reviewedPhase")

        async def progress(measured, observations=None):
            nonlocal previous, latest_observations
            self.check_epoch(epoch)
            validated = wire.evidence(measured, phase, previous)
            wire.require(validated["model"] == self.model)
            previous = validated
            fields = {"evidence": previous}
            if observations is not None:
                wire.require(assignment.get("activityFormat") == wire.ACTIVITY, "Unnegotiated router observations")
                observed = wire.loads(observations, wire.MAX_ACTIVITY, max_nodes=wire.MAX_OBSERVATION_EVENTS * 64)
                wire.require(
                    isinstance(observations, str)
                    and isinstance(observed, dict)
                    and wire.integer(observed.get("version"), 1, 1)
                    and observed.get("source") == "runtime-forwarded-router-observations"
                    and observed.get("coverage") == "returned-responses-only"
                    and wire.integer(observed.get("responses"), 1)
                    and type(observed.get("rounds")) is list
                    and observed.get("state") in ("observed", "unavailable"),
                    "Invalid router observations",
                )
                latest_observations = observations
            if latest_observations is not None:
                fields["routerObservations"] = latest_observations
            entry["reply"] = self.reply(assignment, "running", **fields)
            await self.send(entry["reply"], epoch)

        try:
            epoch = self.auth_epoch()
            wire.require(await self.permit(assignment), "Installed mission admission denied before execution")
            self.check_epoch(epoch)
            entry["reply"] = self.reply(assignment, "accepted")
            await self.send(entry["reply"], epoch)
            self.check_epoch(epoch)
            if self.execute is native_execute:
                owner = asyncio.current_task()

                async def peer_call(name, args):
                    return await self.native_peer(assignment, entry, epoch, owner, name, args)

                result = await self.execute(assignment, self.contract, self.model, progress, peer_call=peer_call)
            else:
                result = await self.execute(assignment, self.contract, self.model, progress)
            self.check_epoch(epoch)
            wire.require(result.get("kind") in ("result", "error"), "Invalid native terminal")
            fields = (
                ("kind", "evidence", "error")
                if result["kind"] == "error"
                else ("kind", "evidence", "output", "artifacts")
            )
            wire.keys(result, (*fields, "output", "artifacts") if result["kind"] == "error" else fields, fields)
            measured = wire.evidence(result["evidence"], phase, previous, success=result["kind"] == "result")
            wire.require(measured["model"] == self.model)
            if result["kind"] == "error":
                wire.require(isinstance(result["error"], str) and result["error"].strip())
                partial = {}
                if "artifacts" in result:
                    exports = wire.artifacts(result["artifacts"])
                    wire.require(
                        set(exports) == {"kars-python-executions.json"}
                        and assignment.get("artifactFormat") == "text-v1"
                        and phase.get("executionMode") == "native"
                        and "python-execute" in phase["capabilities"],
                        "Only native Python receipts survive failed execution",
                    )
                    partial["artifacts"] = exports
                if "output" in result:
                    output = result["output"]
                    wire.require(
                        isinstance(output, str) and output.strip() and len(output.encode("utf-8")) <= 16384,
                        "Invalid failed Mission explanation",
                    )
                    partial["output"] = output
                entry["reply"] = self.reply(
                    assignment, "failed", error=result["error"][:512], evidence=measured, **partial
                )
            else:
                output = result["output"]
                wire.require(isinstance(output, str) and output.strip())
                exports = wire.artifacts(result["artifacts"])
                wire.require(set(self.contract.get("requiredOutputs", [])) <= set(exports))
                fields = {"output": output, "evidence": measured}
                if assignment.get("artifactFormat") == "text-v1":
                    fields["artifacts"] = exports
                reply = self.reply(assignment, "succeeded", **fields)
                wire.require(wire.message_fits(reply), "Mission result exceeds wire bound")
                entry["reply"] = reply
        except asyncio.CancelledError as exc:
            fields = {"evidence": {**previous, "usage": None}} if previous else {}
            fields.update(interrupted_python_evidence(exc, phase, previous, self.model, assignment))
            entry["reply"] = self.reply(assignment, "failed", error="Mesh authorization stopped", **fields)
            raise
        except Exception as exc:
            fields = {}
            if previous:
                fields["evidence"] = {**previous, "usage": None}
            fields.update(interrupted_python_evidence(exc, phase, previous, self.model, assignment))
            entry["reply"] = self.reply(assignment, "failed", error=str(exc)[:512], **fields)
        finally:
            if entry["reply"].get("status") == "failed" and latest_observations is not None:
                entry["reply"]["routerObservations"] = latest_observations
            self.active = None
        await self.send(entry["reply"], epoch)
