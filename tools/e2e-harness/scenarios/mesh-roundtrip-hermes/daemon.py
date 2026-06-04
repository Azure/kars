# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Long-lived mesh echo daemon — Hermes Act 2 e2e harness helper.

Runs inside the mesh-pong-hermes sandbox container as a background
process. Uses :mod:`kars_agt_mesh` (the runtime-neutral Python AGT
MeshClient that ships with kars Act 2) to:

  1. Register with the AGT registry over the inference-router proxy.
  2. Open a long-lived WebSocket to the relay (also over the router proxy).
  3. Loop forever draining the inbox and replying ``echo(<name>): <text>``
     to every inbound mesh message.

The driver waits for ``ECHO_READY`` on stdout before posting the
prompt to the sibling sandbox.
"""

from __future__ import annotations

import asyncio
import logging
import os
import sys
from pathlib import Path

logging.basicConfig(
    level=logging.INFO,
    format="[echo] %(name)s %(message)s",
    stream=sys.stderr,
)

from kars_agt_mesh import MeshClient, MeshConfig


async def main() -> None:
    name = os.environ.get("NAME", "mesh-pong-hermes")
    cfg = MeshConfig(
        name=name,
        # Loopback router proxy — egress-guard drops UID-1000 TCP
        # to anywhere except DNS + localhost + ESTABLISHED, so
        # direct egress to the cluster relay/registry is unreachable.
        # The router has `/agt/relay` (WS) and `/agt/registry/*` (HTTP)
        # proxies for exactly this case.
        relay_url="ws://127.0.0.1:8443/agt/relay",
        registry_url="http://127.0.0.1:8443/agt/registry",
        identity_path=Path("/sandbox/.agt/identity.json"),
    )
    async with MeshClient(cfg) as client:
        # Single-line ready marker the driver greps for.
        print(f"ECHO_READY did={client._identity.did} name={name}", flush=True)
        async for msg in client.inbox():
            text = msg.payload.decode("utf-8", errors="replace")
            print(
                f"ECHO_GOT from={msg.from_did} bytes={len(msg.payload)} "
                f"text={text!r}",
                flush=True,
            )
            reply = f"echo({name}): {text}".encode("utf-8")
            await client.send_by_did(to=msg.from_did, payload=reply)
            print(f"ECHO_REPLIED bytes={len(reply)}", flush=True)


if __name__ == "__main__":
    asyncio.run(main())
