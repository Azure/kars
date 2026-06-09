# kars-agt-mesh — Python AGT MeshClient for any Python agent framework

**Status:** Act 2.1 — core MeshClient + Hermes adapter.

The official Microsoft Agent Governance Toolkit (AGT) ships a full
end-to-end encrypted mesh client in **TypeScript**
(`@microsoft/agent-governance-sdk`). The Python distribution ships the
crypto primitives (X3DH, Double Ratchet, SecureChannel) and the relay
+ registry **servers**, but no equivalent client — every Python agent
framework that wants to participate in the AGT mesh has had to roll
its own.

`kars-agt-mesh` fills that gap.

## What this package gives every Python framework

```python
from kars_agt_mesh import MeshClient, MeshConfig

async with MeshClient(MeshConfig(
    name="execbrief-analyst",
    relay_url="ws://agentmesh-relay.agentmesh.svc.cluster.local:8765",
    registry_url="http://agentmesh-registry.agentmesh.svc.cluster.local:8080",
    identity_path="/sandbox/.agt/identity.json",
    trust_threshold=500,
)) as mesh:
    # Outbound: send an encrypted message to a peer by display name
    await mesh.send_by_name(to="execbrief-viz", payload=b"data.json")

    # Inbound: drain the encrypted inbox
    async for msg in mesh.inbox():
        handle(msg.from_did, msg.payload)
```

The library is intentionally **runtime-neutral** — no Hermes-specific
imports, no kars-specific paths baked in, every knob is injectable.
This lets pydantic-ai, Anthropic Claude Agents SDK, LangGraph, MAF
Python, OpenAI Agents Python, and any future Python framework reuse
the exact same wire protocol that the TS SDK speaks.

## Architecture

| Layer | Implementation | Source |
|---|---|---|
| X3DH key exchange | `X3DHKeyManager` | upstream `agentmesh.encryption.x3dh` |
| Double Ratchet | `DoubleRatchet` | upstream `agentmesh.encryption.ratchet` |
| Secure channel framing | `SecureChannel` | upstream `agentmesh.encryption.channel` |
| Identity + persistent keys | `IdentityStore` | this package |
| Registry HTTP client | `RegistryClient` | this package |
| Relay WebSocket transport | `RelayTransport` | this package |
| KNOCK handshake | `MeshClient.knock` | this package |
| Trust-score gating | `MeshClient._accept_peer` | this package, queries registry |
| Public façade | `MeshClient`, `MeshConfig`, `InboundMessage` | this package |

The crypto comes from the upstream AGT Python `agentmesh-platform`
package (built locally via `runtimes/build-agt-wheels.sh`), so kars
never re-implements primitives — we wire them into a client and add
the transport / lifecycle.

## Wire-protocol compatibility with the TypeScript SDK

Every byte of the X3DH bundle, Double Ratchet header, and relay frame
shape must match `@microsoft/agent-governance-sdk`'s output exactly so
a Python sub-agent can mesh with a TS parent (and vice versa). This is
verified by:

1. **Golden vectors** at `tests/fixtures/ts-frames/` — known plaintext +
   keys + ciphertext bytes captured from the TS SDK, replayed against
   the Python implementation. (Phase A: 2 vectors; Phase B: full
   matrix.)
2. **Live cross-runtime e2e** — kind cluster with TS OpenClaw + Python
   Hermes both registered against the same relay; bidirectional
   KNOCK + 100 encrypted messages; transcript hash must match across
   both ends. (Deferred to Act 2.2.)

## Runtime adoption

| Runtime | Adapter | Status |
|---|---|---|
| **Hermes** | `runtimes/hermes/.../plugin/mesh.py` | Act 2.1 (shipped with this package) |
| **pydantic-ai** | `runtimes/pydantic-ai/.../mesh.py` | follow-up |
| **Anthropic Claude Agents** | `runtimes/anthropic/.../mesh.py` | follow-up |
| **LangGraph Python** | `runtimes/langgraph/.../mesh.py` | follow-up |
| **BYO** | `from kars_agt_mesh import MeshClient` | available now |

Each adapter is ~50 LOC of framework-specific glue (tool schema +
dispatch to `MeshClient` methods).

## Scope of v0.1 (this commit)

In:
- `MeshClient` async API: `connect`, `disconnect`, `send_by_name`,
  `send_by_did`, `inbox` (async iterator), `discover`
- `RegistryClient`: register self, fetch prekey bundle, search by
  display name, post heartbeat with Ed25519-Timestamp auth
- `RelayTransport`: WebSocket connect with 30 s heartbeat,
  exponential-backoff reconnect, KNOCK frame send/recv
- `IdentityStore`: durable Ed25519 + X25519 keys at
  `<identity_path>` (JSON), generated on first run
- KNOCK protocol with trust-score gating
- Singleton guard so multiple imports inside one process share one
  client (cache key: `(name, relay_url, registry_url)`)

Out (deferred to v0.2):
- Multi-process broker (Hermes' cron / lazy_install subprocesses still
  each instantiate a per-process client → Phase B will introduce an
  in-container broker daemon with UDS IPC).
- Chunked encrypted file transfer (`kars_mesh_transfer_file`) — small
  messages only for now.
- Cross-runtime TS↔Python e2e harness scenario.
- Upstream PR to `microsoft/agent-governance-toolkit`.

## Why a library, not a sidecar

Previously the team weighed sidecar vs in-process. User direction was
explicit: **no sidecar**. This package keeps mesh in-process for v0.1
(one MeshClient per Python process) and plans the multi-process story
as an in-container broker daemon — same pod, same container, talks to
the per-runtime adapter over Unix-domain socket. That gives most of
the sidecar's singleton guarantees without changing pod topology.
