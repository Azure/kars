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
    target = {key: os.environ.get("KARS_MISSION_" + env, "") for key, env in (
        ("taskName", "TASK_NAME"), ("taskUid", "TASK_UID"), ("sandboxUid", "SANDBOX_UID"),
        ("podUid", "POD_UID"), ("dispatcherDid", "DISPATCHER_DID"))}
    wire.require(all(wire.matches(wire.ID, target[k]) for k in ("taskName", "taskUid", "sandboxUid", "podUid")))
    wire.require(all(":" not in target[k] for k in ("sandboxUid", "podUid")))
    wire.require(wire.matches(wire.DID, target["dispatcherDid"]))
    root = os.environ.get("KARS_MISSION_IDENTITY_ROOT", "")
    wire.require(re.fullmatch(r"[a-fA-F0-9]{64}", root) is not None, "Missing mission identity root")
    seed = hmac.digest(bytes.fromhex(root), wire.encoded([
        "kars-mission-identity-v1", "runtime", target["sandboxUid"], target["podUid"]]), "sha256")
    from kars_agt_mesh.identity import Identity, IdentityStore
    identity = Identity(seed, hmac.digest(seed, b"kars-hermes-mission-exchange-v1", "sha256"))
    target["agentDid"] = identity.did
    contract = wire.contract(wire.loads(os.environ.get("KARS_MISSION_CONTRACT", ""), 8192))
    admitted = wire.admission(wire.loads(os.environ.get("KARS_MISSION_ADMISSION", ""), 8192))
    model = os.environ.get("KARS_MODEL") or os.environ.get("AZURE_OPENAI_DEPLOYMENT")
    wire.require(isinstance(model, str) and 0 < len(model) <= 200 and model == model.strip()
                 and not any(c.isspace() for c in model), "Installed missions require KARS_MODEL")
    identity_path = Path(home) / ".agt" / "missions" / target["podUid"] / "identity.json"
    wire.require(not any(p.is_symlink() for p in (identity_path, *identity_path.parents)),
                 "Mission identity path cannot contain symlinks")
    identity_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if identity_path.exists():
        wire.require(IdentityStore.load_or_create(identity_path) == identity, "Mission identity mismatch")
    else:
        data = {"version": 1, "did": identity.did,
                "ed25519_seed": base64.urlsafe_b64encode(seed).rstrip(b"=").decode(),
                "x25519_secret": base64.urlsafe_b64encode(identity.x25519_secret).rstrip(b"=").decode()}
        fd = os.open(identity_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as file:
            file.write(wire.encoded(data))
    _BOOTSTRAP = (identity_path, target, contract, admitted, model)
    return _BOOTSTRAP


async def authorize(assignment):
    context = {"task_uid": assignment["taskUid"], "run_nonce": assignment["runNonce"],
               "dispatcher_did": assignment["dispatcherDid"], "assignment_id": assignment["assignmentId"]}
    for snake, camel in (("phase_digest", "phaseDigest"), ("reviewed_phase", "reviewedPhase"),
                         ("input_digest", "inputDigest")):
        if camel in assignment:
            context[snake] = assignment[camel]
    try:
        async with httpx.AsyncClient(timeout=2, trust_env=False) as client:
            async with client.stream("POST", "http://127.0.0.1:8443/agt/evaluate",
                                     json={"action": "task:execute", "context": context}) as response:
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


async def native_execute(assignment, contract, model, progress):
    from kars_runtime_hermes.plugin import mission_native
    import kars_runtime_hermes
    import shutil

    home = Path(os.environ.get("HERMES_HOME", "/sandbox/.hermes"))
    work = home / "missions" / str(uuid.uuid4())
    wire.require(not any(p.is_symlink() for p in (work, *work.parents)), "Mission work path contains symlinks")
    work.mkdir(parents=True, mode=0o700)
    process = None
    try:
        for directory in ("inputs", "exports", "home", "scratch"):
            (work / directory).mkdir(mode=0o700)
        for item in assignment.get("inputArtifacts", []):
            wire.require(wire.input_name(item["name"]))
            path = work / "inputs" / item["name"]
            path.write_text(item["content"], encoding="utf-8")
            path.chmod(0o400)
        # A clean HOME prevents native plugin discovery and a second prekey writer.
        (work / "home" / "config.yaml").write_text(
            "model:\n  context_length: 131072\ncompression:\n  enabled: false\n"
            "memory:\n  memory_enabled: false\n  user_profile_enabled: false\n"
            "security:\n  allow_lazy_installs: false\n"
            "toolsets: []\nmcp_servers: {}\nplugins: {}\n", encoding="utf-8")
        source = Path(kars_runtime_hermes.__file__).resolve().parent.parent
        env = {"HOME": str(work / "home"), "HERMES_HOME": str(work / "home"), "TMPDIR": str(work / "scratch"),
               "PYTHONPATH": str(source), "PYTHONDONTWRITEBYTECODE": "1", "PYTHONNOUSERSITE": "1",
               "PATH": str(Path(sys.executable).parent) + ":/usr/bin:/bin", "HERMES_TELEMETRY_ENABLED": "false",
               "HERMES_DISABLE_LAZY_INSTALLS": "1", "HERMES_SKIP_NODE_BOOTSTRAP": "1", "TIRITH_ENABLED": "false"}
        if "KARS_SKILL_CATALOG_DIGEST" in os.environ:
            env["KARS_SKILL_CATALOG_DIGEST"] = os.environ["KARS_SKILL_CATALOG_DIGEST"]
        request = {**assignment, "contract": contract, "model": model}
        async with asyncio.timeout(600):
            process = await asyncio.create_subprocess_exec(
                sys.executable, "-m", mission_native.__name__, cwd=work, env=env,
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
                limit=wire.MAX_WIRE + 1)
            process.stdin.write(wire.encoded(request))
            await process.stdin.drain()
            process.stdin.close()
            result = None
            events = 0
            async for line in process.stdout:
                events += 1
                wire.require(events <= 128, "Too many native events")
                event = wire.loads(line)
                wire.require(result is None, "Event after native terminal")
                if event.get("kind") == "progress":
                    wire.keys(event, ("kind", "evidence", "routerObservations"), ("kind", "evidence"))
                    if "routerObservations" in event:
                        await progress(event["evidence"], event["routerObservations"])
                    else:
                        await progress(event["evidence"])
                elif event.get("kind") in ("result", "error"):
                    result = event
                else:
                    raise ValueError("Invalid native event")
            code = await process.wait()
            wire.require(code == 0 and result is not None, "Native worker exited without measured result")
        for item in assignment.get("inputArtifacts", []):
            path = work / "inputs" / item["name"]
            wire.require(not path.parent.is_symlink() and not path.is_symlink()
                         and mission_native.read_file_bytes(path) == item["content"].encode(),
                         "Immutable input changed")
        return result
    finally:
        if process is not None and process.returncode is None:
            process.kill()
            await process.wait()
        # Only this mission's bounded scratch tree is removed, never shared caches.
        shutil.rmtree(work)


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
        result = {**self.target, **self.contract, "type": "mission:reply", "runNonce": assignment["runNonce"],
                  "bootId": self.boot_id, "status": status}
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
            wire.require(auth is self.client.connection_auth and auth.generation == generation,
                         "Mesh authorization changed")
            auth.check()

    async def send(self, reply, epoch=None):
        async with self.send_lock:
            try:
                self.check_epoch(epoch)
                payload = wire.encoded(reply)
                wire.require(len(payload) <= wire.MAX_WIRE)
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
        epoch = None
        phase = self.contract.get("reviewedPhase")

        async def progress(measured, observations=None):
            nonlocal previous
            self.check_epoch(epoch)
            validated = wire.evidence(measured, phase, previous)
            wire.require(validated["model"] == self.model)
            previous = validated
            fields = {"evidence": previous}
            if observations is not None:
                wire.require(assignment.get("activityFormat") == wire.ACTIVITY,
                             "Unnegotiated router observations")
                observed = wire.loads(observations, 32 * 1024)
                wire.require(isinstance(observations, str) and isinstance(observed, dict)
                             and wire.integer(observed.get("version"), 1, 1)
                             and observed.get("source") == "runtime-forwarded-router-observations"
                             and observed.get("coverage") == "returned-responses-only"
                             and wire.integer(observed.get("responses"), 1)
                             and type(observed.get("rounds")) is list
                             and observed.get("state") in ("observed", "unavailable"),
                             "Invalid router observations")
                fields["routerObservations"] = observations
            entry["reply"] = self.reply(assignment, "running", **fields)
            await self.send(entry["reply"], epoch)

        try:
            epoch = self.auth_epoch()
            wire.require(await self.permit(assignment), "Installed mission admission denied before execution")
            self.check_epoch(epoch)
            entry["reply"] = self.reply(assignment, "accepted")
            await self.send(entry["reply"], epoch)
            self.check_epoch(epoch)
            result = await self.execute(assignment, self.contract, self.model, progress)
            self.check_epoch(epoch)
            wire.require(result.get("kind") in ("result", "error"), "Invalid native terminal")
            fields = ("kind", "evidence", "error") if result["kind"] == "error" else (
                "kind", "evidence", "output", "artifacts")
            wire.keys(result, fields, fields)
            measured = wire.evidence(result["evidence"], phase, previous, success=result["kind"] == "result")
            wire.require(measured["model"] == self.model)
            if result["kind"] == "error":
                wire.require(isinstance(result["error"], str) and result["error"].strip())
                entry["reply"] = self.reply(assignment, "failed", error=result["error"][:512], evidence=measured)
            else:
                output = result["output"]
                wire.require(isinstance(output, str) and output.strip())
                exports = wire.artifacts(result["artifacts"])
                wire.require(set(self.contract.get("requiredOutputs", [])) <= set(exports))
                fields = {"output": output, "evidence": measured}
                if assignment.get("artifactFormat") == "text-v1":
                    fields["artifacts"] = exports
                reply = self.reply(assignment, "succeeded", **fields)
                wire.require(len(wire.encoded(reply)) <= wire.MAX_WIRE, "Mission result exceeds wire bound")
                entry["reply"] = reply
        except asyncio.CancelledError:
            fields = {"evidence": {**previous, "usage": None}} if previous else {}
            entry["reply"] = self.reply(assignment, "failed", error="Mesh authorization stopped", **fields)
            raise
        except Exception as exc:
            fields = {}
            if previous:
                fields["evidence"] = {**previous, "usage": None}
            entry["reply"] = self.reply(assignment, "failed", error=str(exc)[:512], **fields)
        finally:
            self.active = None
        await self.send(entry["reply"], epoch)
