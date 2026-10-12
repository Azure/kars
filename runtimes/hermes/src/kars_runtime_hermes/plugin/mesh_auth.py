# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Required Entra lease for the existing mesh client, never a second transport."""
from __future__ import annotations

import asyncio
import base64
import json
import logging
import time
import weakref

from kars_agt_mesh import MeshTransportError

from . import router_client

logger = logging.getLogger("kars.hermes.mesh_auth")
REQUEST_TIMEOUT = 10.0
CLEANUP_TIMEOUT = 5.0
EXPIRY_SKEW = 30.0


def token_lifetime(body, now):
    """JWT decoding bounds lifetime only; relay and registry authenticate it."""
    try:
        token = body["access_token"]
        ttl = body["expires_in"]
        if (not isinstance(token, str) or not token or len(token) > 16384
                or any(c.isspace() for c in token) or body["token_type"].lower() != "bearer"
                or type(ttl) is not int or not 0 < ttl <= 86400):
            raise ValueError
        parts = token.split(".")
        if len(parts) != 3 or not all(parts):
            raise ValueError
        claims = json.loads(base64.urlsafe_b64decode(parts[1] + "=" * (-len(parts[1]) % 4)))
        expiry = claims["exp"]
        if type(expiry) is not int:
            raise ValueError
        lifetime = min(ttl, expiry - now) - EXPIRY_SKEW
        if lifetime <= 0:
            raise ValueError
        return token, lifetime
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError):
        raise MeshTransportError("Required Entra mint returned invalid token lifetime") from None


def verified_identity(body, did):
    if (not isinstance(body, dict) or body.get("did") != did or body.get("tier") != "verified"
            or any(not isinstance(body.get(k), str) or not body[k].strip()
                   for k in ("verified_app_id", "verified_tenant_id"))
            or body.get("error") or body.get("success") is False
            or body.get("status") in ("error", "failed", "rejected")):
        raise MeshTransportError("Required Entra registry verification failed; mesh unavailable")


async def bounded_cleanup(client):
    """Retain the writer lock if cancellation-resistant teardown is still running."""
    task = getattr(client, "_kars_cleanup_task", None)
    if task is None or (task.done() and (task.cancelled() or task.exception() is not None)):
        task = asyncio.create_task(client.disconnect())
        client._kars_cleanup_task = task
    done, _ = await asyncio.wait({task}, timeout=CLEANUP_TIMEOUT)
    if not done:
        # Do not cancel and then assume a possibly-live writer has stopped.
        raise MeshTransportError("Mesh teardown pending; client quarantined")
    task.result()
    client._kars_cleanup_task = None


class RequiredEntraLease:
    """Loop-owned authorization, with monotonic expiry and generation fencing.

    Every relay connection consumes a fresh router mint and canonical registry
    verification. Renewal reconnects only the existing WebSocket; prekeys and
    ratchets stay with their sole client. Any failed auth attempt is fail-stop.
    """

    def __init__(self, client, failed):
        self.client = client
        self.failed = failed
        self.token = None
        self.deadline = 0.0
        self.wall_deadline = 0.0
        self.ready = False
        self.closed = False
        self.generation = 0
        self.renewal = None
        self.expiry = None
        self.failure_task = None
        self.handlers = weakref.WeakSet()
        self.listeners = weakref.WeakSet()

    def _current(self, generation):
        if self.closed or self.generation != generation:
            raise MeshTransportError("Required Entra authorization superseded")

    async def _request(self, method, path, **kwargs):
        generation = self.generation
        try:
            async with asyncio.timeout(REQUEST_TIMEOUT):
                response = await asyncio.to_thread(router_client.call, method, path, **kwargs)
            self._current(generation)
            if response.status_code != 200:
                raise ValueError
            return response.json()
        except asyncio.CancelledError:
            raise
        except Exception:
            raise MeshTransportError("Required Entra auth request failed") from None

    async def mint_initial(self):
        self._current(self.generation)
        started, wall = time.monotonic(), time.time()
        body = await self._request("GET", "/v1/mesh-token")
        token, lifetime = token_lifetime(body, wall)
        self.token = token
        self.deadline = started + lifetime
        self.wall_deadline = wall + lifetime
        self._live()
        self._arm_expiry()

    def _live(self):
        if self.closed or time.monotonic() >= self.deadline or time.time() >= self.wall_deadline:
            raise MeshTransportError("Required Entra authorization expired or stopped")

    def check(self):
        try:
            self._live()
            if not self.ready:
                raise MeshTransportError("Required Entra relay authorization unavailable")
        except MeshTransportError:
            if not self.closed and self.ready:
                self.fail()
            raise

    def track(self, task, *, listener=False):
        self.check()
        (self.listeners if listener else self.handlers).add(task)

    def suspend(self):
        self.ready = False
        self.generation += 1
        self.token = None
        # A new connection is a new authorization epoch, not permission for
        # unfinished work from the old one. The sole inbox listener may survive.
        current = asyncio.current_task()
        for task in self.handlers:
            if task is not current:
                task.cancel()
        # Queued frames from an earlier connection cannot authorize later work.
        while not self.client._inbox.empty():
            self.client._inbox.get_nowait()

    async def prepare_relay(self):
        if self.token is None:
            await self.mint_initial()
        self._live()
        return self.token

    async def verify_relay(self):
        generation = self.generation
        try:
            body = await self._request("POST", "/agt/registry/v1/registry/verify", json={
                "amid": self.client._identity.did, "verification_token": self.token,
            })
            verified_identity(body, self.client._identity.did)
            self._current(generation)
            self._live()
        except asyncio.CancelledError:
            raise
        except Exception:
            raise MeshTransportError("Required Entra registry verification failed; mesh unavailable") from None
        self.ready = True
        if self.renewal is not None:
            self.renewal.cancel()
        self.renewal = asyncio.create_task(self._renew(generation))

    def _arm_expiry(self):
        if self.expiry is not None:
            self.expiry.cancel()
        self.expiry = asyncio.create_task(self._expire())

    async def _expire(self):
        await asyncio.sleep(max(0, self.deadline - time.monotonic()))
        if not self.closed and time.monotonic() >= self.deadline:
            self.fail()

    async def _renew(self, generation):
        try:
            await asyncio.sleep(max(0, (self.deadline - time.monotonic()) * 0.8))
            self._current(generation)
            self.check()
            async with asyncio.timeout(REQUEST_TIMEOUT):
                await self.client._relay.reconnect()
        except asyncio.CancelledError:
            raise
        except Exception:
            self.fail()

    def stop(self):
        if self.closed:
            return
        self.closed = True
        self.suspend()
        current = asyncio.current_task()
        for task in (self.renewal, self.expiry, *self.handlers, *self.listeners):
            if task is not None and task is not current:
                task.cancel()

    def fail(self):
        if self.closed:
            return
        self.stop()
        self.failed(self.client)
        self.failure_task = asyncio.create_task(self._cleanup())

    async def _cleanup(self):
        try:
            await bounded_cleanup(self.client)
        except Exception:
            logger.error("Required Entra mesh stopped; teardown quarantined")

    async def shutdown(self):
        self.stop()
        tasks = [t for t in (self.renewal, self.expiry, *self.handlers, *self.listeners)
                 if t is not None and t is not asyncio.current_task()]
        if tasks:
            done, pending = await asyncio.wait(tasks, timeout=CLEANUP_TIMEOUT)
            for task in done:
                if not task.cancelled():
                    task.exception()
            if pending:
                raise MeshTransportError("Mesh handlers still stopping; client quarantined")
