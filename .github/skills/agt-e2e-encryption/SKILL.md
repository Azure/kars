---
description: "Kars AGT E2E encryption skill — how the Signal Protocol inter-agent messaging works, how to debug it, and what was patched."
---

# AGT E2E Encrypted Inter-Agent Communication

## Overview
Kars agents communicate via the official Microsoft Agent Governance Toolkit (AGT) — a decentralized, E2E encrypted messaging protocol using Signal Protocol (X3DH + Double Ratchet). Transport is provided through `@microsoft/agent-governance-sdk` (upstream) wrapped by the in-repo `@kars/mesh` provider, selected at runtime via `createMeshTransport()`.

OpenClaw runtimes go through the TypeScript SDK; **Hermes** runtimes go through `runtimes/agt-mesh-python/` (kars-agt-mesh), a Python AGT MeshClient at byte-for-byte wire-format parity with the TS SDK. The same relay/registry pair serves both. Cross-runtime bidi (`OpenClaw ↔ Hermes`) is proven on AKS and local kind — see `tests/e2e/interop/`.

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

---

## Cross-runtime mesh (Hermes ↔ OpenClaw)

### Wire format

| Component | TS SDK | Python `kars_agt_mesh` |
|---|---|---|
| MESSAGE frame | `{v, type:"message", from, to, id, ts, header:{dh, pn, n}, ciphertext}` (std base64, NOT urlsafe) | identical |
| KNOCK establishment | `{ik, ek, otk?}` — **short** keys | identical |
| Plaintext payload | `JSON.parse(plaintext)` hardcoded on receive | sender JSON-wraps; receiver unwraps (`_payload_to_wire_bytes` / `_wire_bytes_to_payload`) |
| AAD inside Double Ratchet | `caller_ad || x3dh_ad` where `caller_ad = "${sender_did}|${receiver_did}"` and `x3dh_ad = IK_init || IK_resp` (32 + 32 = 64) | identical |
| Header serialization | `dh(32) || pcl(u32 BE) || mn(u32 BE)` = 40 bytes | identical |
| HKDF (X3DH) | `HKDF(salt=zero[32], info="AgentMesh_X3DH_v1", IKM=F||dhConcat, len=32)`, F = `0xFF × 32` | identical |
| Connect-frame POP | std-base64 (NOT urlsafe) `public_key` + Ed25519 sig over timestamp | identical |

Both sides are pinned to the same upstream commit via `vendor/agt/pin.json::sha` — a single source of truth across TS, Python, and Rust.

### Prekey-clobber guard (DO NOT BYPASS)

`MeshClient.connect()` takes an exclusive `fcntl.flock` on `<identity_dir>/.mesh-prekeys.lock` (`runtimes/agt-mesh-python/src/kars_agt_mesh/client.py::_acquire_prekey_writer_lock`). A second Python process attempting to start a MeshClient for the same identity raises `MeshTransportError` naming the holder PID.

**DO NOT** debug a live Hermes pod by running `kubectl exec ... python3 -c "from kars_runtime_hermes.plugin import mesh; mesh._get_or_init_client(); ..."`. The secondary process would (before the guard) generate fresh X3DH key material and `PUT` it to the registry, clobbering the daemon's bundle and causing silent `Decrypt failed for did:mesh:...` log entries with no traceback. Cost the team several hours of debugging in 2026-06; see [`docs/internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md`](../../docs/internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md).

**Correct ways to inspect a live daemon:**

```bash
# Identity / DID
kubectl exec <pod> -c agent -- cat /sandbox/.hermes/.agt/identity.json | jq

# Operator trust store (peers seen by the daemon's mesh_worker)
ADMIN=$(kubectl get secret -n kars-<sb> admin-token -o jsonpath='{.data.token}' | base64 -d)
kubectl exec <pod> -c agent -- sh -c "curl -sS -H 'Authorization: Bearer $ADMIN' http://127.0.0.1:8443/agt/trust"

# Daemon log
kubectl logs <pod> -c agent
```

### AKS-specific Entra requirements

Hermes pods on AKS require **two** RBAC arms for the mesh to bring up the verified tier:

1. **The pod's workload-identity client (Entra Agent App auto-provisioned per sandbox)** — must have `Azure AI User` on the resource group. Auto-granted by the controller when you set `KarsAuthConfig.spec.foundryRbac` in the sandbox namespace. The CRD field landed in commit `496cc92`.
2. **The Foundry project's MI** — needs `Azure AI User` on the resource group for Memory Store operations. This is a one-time Portal step per project (see the `Foundry Memory Store Auth` section in the repo's CLAUDE.md).

The entrypoint exchanges the projected SA token for an Entra Agent App token (audience `<app-id>/.default`) and POSTs it to `/agt/registry/v1/registry/verify`. Success surfaces as `tier=verified, verified_app_id=<guid>` on the operator panel.

### `kars push --only runtime-hermes` is hermetic

Out of the box (no manual `git clone`, no `bash runtimes/build-agt-wheels.sh`):

```bash
kars push --only runtime-hermes --apply
```

The CLI auto-clones `microsoft/agent-governance-toolkit@kars-sdk-pop-signing@<pin sha>` (via `cli/src/lib/agt-bootstrap.ts::ensureAgtRepo`) and builds `runtimes/wheels/*.whl` (via `ensureAgtWheels`) before docker build. Cached by `runtimes/wheels/.agt-sha` so re-runs are no-ops.

### End-to-end harnesses

| Scope | Harness | Expected proof |
|---|---|---|
| Local kind, Hermes ↔ OpenClaw bidi | `tests/e2e/interop/hermes_openclaw_bidi.sh` | `score ≥ 500, status=delivered_and_replied` |
| AKS production, four scenarios | `tests/e2e/interop/aks_full_suite.sh` | 4/4 PASS (single, inter-agent, multi-fanout, Entra Verified) |

If either regresses, the first thing to suspect is the AGT pin — `vendor/agt/pin.json::sha` must match upstream `microsoft/agent-governance-toolkit:kars-sdk-pop-signing`. Pre-existing image layers may have a stale SDK; `kars push --only runtime-hermes --apply` (or `runtime-*` per affected runtime) refreshes them.
