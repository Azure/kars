---
description: "Kars AGT E2E encryption skill — how the Signal Protocol inter-agent messaging works, how to debug it, and what was patched."
---

# AGT E2E Encrypted Inter-Agent Communication

## Overview
Kars agents communicate via the official Microsoft Agent Governance Toolkit (AGT) — a decentralized, E2E encrypted messaging protocol using Signal Protocol (X3DH + Double Ratchet). Transport is provided through `@microsoft/agent-governance-sdk` (upstream) wrapped by the in-repo `@kars/mesh` provider, selected at runtime via `createMeshTransport()`.

> **History note:** Earlier releases used a vendored fork of `@agentmesh/sdk` with 8+ local patches under `vendor/`. All upstream-relevant fixes have landed in AGT (PRs through the toolkit's 3.x line) and the vendored tree has been removed. The bug table below is preserved as a regression checklist — if any of these symptoms reappear, treat as a regression in the official SDK.

## Protocol Flow
```
Parent Agent                    Sub-Agent
     │                              │
     │── Register + upload prekeys ──│── Register + upload prekeys
     │                              │
     │── Registry search by name ───│
     │── Fetch target's prekeys ────│
     │── X3DH key exchange ─────────│
     │── Send KNOCK via relay ──────│── Receive KNOCK, auto-accept
     │── Encrypt with Double Ratchet│
     │── Send encrypted msg ────────│── Decrypt with Double Ratchet
     │                              │── Call local LLM via router
     │── Receive encrypted reply ◄──│── Encrypt reply, send via relay
```

## Infrastructure
- **Relay**: `agentmesh-relay` in `agentmesh` namespace (WebSocket, routes encrypted blobs)
- **Registry**: `agentmesh-registry` in `agentmesh` namespace (agent discovery, prekey storage)
- **Router proxy**: Inference router proxies `/agt/relay` (WS) and `/agt/registry/*` (HTTP)

## Encryption Details
- **Identity**: Ed25519 signing + X25519 key exchange per agent
- **Key Exchange**: X3DH (Extended Triple Diffie-Hellman) with signed prekeys
- **Message Encryption**: Double Ratchet with XSalsa20-Poly1305 (libsodium)
- **Session**: KNOCK protocol for policy-gated session establishment
- **Forward Secrecy**: Per-message key rotation via Double Ratchet

## Historical bug catalogue (now-upstream regression checklist)
All originally fixed via patches in `vendor/` (removed in 2026-05). Listed here as a regression-detection cheatsheet against the current official `@microsoft/agent-governance-sdk`:

| Component | Bug | Fix |
|-----------|-----|-----|
| Relay | chrono `to_rfc3339()` Z vs +00:00 | Raw timestamp string verification |
| Registry | Same for `verify_update_signature` | Same fix |
| SDK prekey | `buildBundle()` empty signature | Re-sign + store public keys |
| SDK base64 | `x25519:` prefix crash | Strip before decode |
| SDK X3DH | Missing peer ratchet key | Pass signedPrekey |
| SDK ratchet | Wrong responder keypair | `initializeResponder()` with signedPrekey |
| SDK transport | Receive not wired to client | Added in `connect()` |
| SDK session | No KNOCK sent via relay | Send KNOCK + X3DH params in first message |

## Debugging
- Check relay logs: `kubectl logs -n agentmesh -l app=agentmesh-relay`
- Check registry logs for prekey uploads: `kubectl logs -n agentmesh -l app=agentmesh-registry`
- Check sub-agent relay listener: `kubectl exec <pod> -c openclaw -- cat /tmp/agt-relay-listener.log`
- Parent plugin logs: look for `[plugins] AGT relay:` lines

## Trust Scoring
- `AGT_TRUST_THRESHOLD` env var (default: 500 in sandbox, 0 = accept all)
- KNOCK handler evaluates peer trust score via registry lookup
- Anonymous agents (no OAuth) have score 0 — fail open when registry lookup fails
- Verified agents (Tier 1, GitHub OAuth) have score 600+

## Mesh-message metrics

The inference router exports two Prometheus counters that the
Headlamp Mesh Topology and operator-CLI topology use:

- `kars_mesh_messages_sent_total`
- `kars_mesh_messages_received_total`

Defined in `inference-router/src/metrics.rs` (`AGT_MESH_MESSAGES_*`) and
incremented in `inference-router/src/routes/mesh.rs` next to the
atomic `MeshMetrics::messages_{sent,received}` counters. The
`sandbox=<name>` label is added at scrape time by
`deploy/monitoring/podmonitor-sandbox-router.yaml`.

**Counted**: KNOCK frames, X3DH bundle exchange, each `mesh_send`
call, explicit `sendHeartbeat()` ticks (every 30 s, scheduled from
`mesh-plugin/src/agt-transport.ts` — vanilla AGT doesn't auto-heartbeat).

**Not counted**: WebSocket Ping/Pong keepalives (short-circuited with
`continue` before the counter increments), and registry HTTP calls
(`/v1/agents/...`, `/v1/agents/{did}/heartbeat`) which go over HTTP,
not the WS relay.

**Why sent ≫ received early on**: a fresh sandbox emits ≥ 1 KNOCK
per known peer plus a 30 s heartbeat tick, but only receives back
the relay's KNOCK-ack until a real bidirectional conversation
starts. Counters live in the router process and **reset on pod restart**.
