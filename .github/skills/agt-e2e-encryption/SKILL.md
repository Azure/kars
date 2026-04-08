---
description: "AzureClaw AGT E2E encryption skill — how the Signal Protocol inter-agent messaging works, how to debug it, and what was patched."
---

# AGT E2E Encrypted Inter-Agent Communication

## Overview
AzureClaw agents communicate via AgentMesh (amitayks/agentmesh) — a decentralized, E2E encrypted messaging protocol using Signal Protocol (X3DH + Double Ratchet).

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

## Vendor Patches (8 bugs fixed)
All in `vendor/` directory with READMEs explaining each patch:

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
