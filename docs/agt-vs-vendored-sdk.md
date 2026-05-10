# AGT vs Vendored AgentMesh SDK — Side-by-Side Analysis

> **Status:** Phase 2 complete. Runtime can swap between the vendored
> `@agentmesh/sdk` (default) and Microsoft's `@microsoft/agent-governance-sdk`
> via the `AZURECLAW_MESH_PROVIDER` environment variable.
>
> **Audience:** AzureClaw maintainers + AGT upstream team. This document is
> the source of truth for what the two SDKs offer, where they diverge, and
> what we patched on each side to reach functional parity.

---

## TL;DR

We currently ship two implementations of the AgentMesh protocol:

| Codename | Package | Source | Default? |
|---|---|---|---|
| **A** (vendored) | `@agentmesh/sdk` v0.1.2 | `vendor/agentmesh-sdk/` (9 patches over upstream amitayks) | ✅ Yes |
| **B** (AGT) | `@microsoft/agent-governance-sdk` v3.5.0+ | npm (Microsoft AGT) | Opt-in |

Set `AZURECLAW_MESH_PROVIDER=agt` in the sandbox environment to swap to B.
The default (`vendored`) uses A. Anything else falls back to A.

After Phase 2 the runtime never imports a transport class directly — it
calls `createMeshTransport({...})` from `@azureclaw/mesh`, which decides
which adapter to instantiate. Both adapters expose the **same**
`IMeshTransport` interface so the rest of the runtime is provider-agnostic.

> ### ⚠️ Audit verdict (Phase 2 wrap-up)
>
> The **swap mechanism is correct and tested**: factory + interface + both
> adapters implement the contract; 16 compat tests pin parity at the API
> shape level. Phase 2's goal — letting the runtime import a single
> factory and stay provider-agnostic — is met.
>
> A deep audit of every vendored patch against AGT's full upstream stack
> (TS SDK + Python relay + Python registry) found:
>
> - **8 patches already fixed in AGT** — no porting needed.
> - **3 patches that are adapter-only** (registry RPCs AGT deliberately
>   doesn't expose) — fixes landed in `agt-transport.ts` this commit.
> - **5 patches with different-but-equivalent solutions** in AGT — no
>   functional regression.
> - **2 real gaps in AGT** that blocked moving fully upstream: receiver-side
>   X3DH bootstrap (G1) and auto-reconnect loop (G2).
>
> Both G1 and G2 are now **fixed on the local AGT branch**
> `azureclaw-meshclient-event-hooks` (commit `d75ea37b`, held locally,
> NOT pushed pending coordination with the AGT team for an upstream PR).
> Together with the 3 event hooks already on that branch, this means
> the AGT SDK is feature-complete for the upstream-only scenario from
> AzureClaw's perspective. AGT TS test suite: 398/398 pass.
> See [Patch-by-patch audit](#patch-by-patch-audit-vendored-vs-agt-upstream-stack).

---

## Surface-by-surface comparison

### 1. Identity (Ed25519 + X25519)

| Capability | A (vendored) | B (AGT) | Parity |
|---|---|---|---|
| Generate identity | `Identity.generate()` | `Identity.generate()` | ✅ |
| Persist | `identity.toData()` / `Identity.fromData()` | `identity.toJSON()` / `Identity.fromJSON()` | ✅ (different method names; we adapt at the seam) |
| Derive AMID from signing pubkey | SHA-256 truncated to base32, `did:agentmesh:` prefix | Same algorithm | ✅ |
| Verify Ed25519 signatures | `Identity.verifySignature(pubKey, payload, sig)` | `crypto.verifySignature(...)` (utility module) | ✅ (call site in runtime stays on A for now) |

**Wiring:** `Identity` is generated **once** via the vendored SDK regardless of
provider, then we extract the raw Ed25519 keys (`identity.toData()`) and
hand them to the factory. The factory passes the raw bytes to AGT and the
full `sdkIdentity` to the vendored adapter. This keeps signing keys
identical across providers (so AMIDs don't change when you flip the env var).

### 2. Policy engine

| Capability | A | B |
|---|---|---|
| Tool allow/deny | `new sdk.Policy([{ action, effect }])` | `new PolicyEngine({ rules: [...] })` |
| Per-tool decision | `policy.evaluate(action)` returns `allow|deny` | `engine.evaluate({ tool, args })` returns `{ decision, reason }` |
| Wildcards | Action prefix match (`shell:*`) | Glob patterns + JSON path |

**Decision:** Stay on A's `Policy` for now — it's used for tool-level allow/deny
in the sandbox, and the call surface is small and stable. A rewrite to AGT's
PolicyEngine is a separate project (no mesh dependency).

### 3. Trust store + audit log

| Capability | A | B | Parity |
|---|---|---|---|
| Score peers (0–1000) | `createTrustStore()` → `set/get/incr` | `TrustManager` (composable, persisted) | ⚠️ Different semantics |
| Append-only audit log | `createAuditLogger()` (in-memory hash chain) | `AuditLogger` (pluggable backend, hash chain) | ⚠️ Different semantics |

**Decision:** Stay on A. AGT's TrustManager is structurally richer (handles
reputation aggregation, decay, cross-session memory) but our sandbox lifecycle
is short-lived enough that the simple A model fits. Migration is out of scope
for the swap.

### 4. Mesh transport (the actual swap)

This is the surface that `IMeshTransport` covers — the only thing the factory
actually swaps.

| Method | A (`AgentMeshClient`) | B (`MeshClient`) | Parity post-Phase 2 |
|---|---|---|---|
| `connect()` | ✅ | ✅ | ✅ |
| `disconnect()` | ✅ | ✅ | ✅ |
| `isConnected` | ✅ | ✅ | ✅ |
| `send(toAmid, msg)` | ✅ | ✅ | ✅ |
| `onMessage(cb)` | ✅ | ✅ | ✅ |
| `onKnock(cb)` | ✅ | ✅ | ✅ |
| `addPlaintextPeer / removePlaintextPeer / isPlaintextPeer` | ✅ | ✅ | ✅ |
| `lookup(amid)` | ✅ (built-in registry RPC) | ❌ (missing) | ✅ via REST in adapter |
| `submitReputation(amid, sessionId, score, tags)` | ✅ | ❌ (missing) | ✅ via REST in adapter |
| `enableKnockEnforcement()` | ✅ (per-instance toggle) | ❌ (always on) | ✅ no-op on B |
| `onError(kind, from, detail)` | ✅ | ❌ → ✅ ([added in local AGT branch](#agt-upstream-changes-required)) | ✅ |
| `onE2EVerified(peer, isFirst)` | ✅ | ❌ → ✅ (added in local AGT branch) | ✅ |
| `onDisconnect(reason, code)` | ✅ | ❌ → ✅ (added in local AGT branch) | ✅ |
| `sendHeartbeat()` | ✅ | ✅ | ✅ |
| `sendWithAck(toAmid, msg, timeout)` | ✅ | ✅ | ✅ |

The **3 event hooks** are functionally critical — without them the runtime
loses visibility into decrypt failures, ws disconnects, and peer-handshake
completion. We added them on a local AGT branch (`azureclaw-meshclient-event-hooks`,
sha `e5f4346f`, NOT pushed) so we can test parity locally; the upstream PR
will be opened by the AGT team using that branch as a reference.

The **3 governance methods** (`lookup`, `submitReputation`, `enableKnockEnforcement`)
are intentionally NOT pushed to AGT. AGT's `MeshClient` is pure transport;
registry RPC belongs in a separate `RegistryClient`. We implement them as
REST-to-registry calls inside our `AgtTransport` adapter and document them
as an open design question.

### 5. Registry (agent discovery, prekey storage, reputation)

Registry is a **service** (`agentmesh-registry`), not part of the SDK. Both
A and B speak the same wire protocol against it:

| Endpoint | Method | Used by | Notes |
|---|---|---|---|
| `/agents` | `POST` | Both adapters via `connect()` | Register agent + upload prekey bundle |
| `/agents/{amid}` | `GET` | Both adapters during X3DH | Fetch peer's signed prekey + one-time prekey |
| `/registry/lookup/{amid}` | `GET` | A built-in; B via our adapter | Fetch reputation + display name |
| `/registry/feedback` | `POST` | A built-in; B via our adapter | Submit reputation score |
| `/agents/search?q=name` | `GET` | Both | Discovery by display name |

The registry was patched (`vendor/agentmesh-registry/`) for chrono RFC3339
serialization (`Z` vs `+00:00` mismatch breaking signature verification).
Both A and B benefit from this server-side fix; no SDK-side changes needed.

### 6. Relay (E2E encrypted message routing)

Relay is a **service**, but A and B ship different relay implementations:
- **A** uses our vendored Rust relay (`vendor/agentmesh-relay/`), per-frame
  Ed25519-signed connect, raw-timestamp signature verification.
- **B** uses AGT's Python FastAPI relay
  (`agent-governance-python/agent-mesh/src/agentmesh/relay/app.py`),
  shared-secret token auth (`AGENTMESH_RELAY_TOKEN`), no per-frame
  signature verification.

When we move fully upstream, the AGT relay replaces ours. The two
relays are **wire-incompatible** (different connect frame schemas),
which means A and B cannot share a single relay deployment. This is by
design — each provider speaks to its matching server. See the audit
below for per-feature gap analysis.

| Frame type | Direction | A | B |
|---|---|---|---|
| `connect` (auth) | client→server | ✅ | ✅ |
| `send` | client→server | ✅ | ✅ |
| `receive` | server→client | ✅ | ✅ |
| `ack` | server→client | ✅ | ✅ |
| `ping` / `pong` | bidirectional | ✅ | ✅ |

Frames use serde-tagged JSON with `"type"` field. The vendored relay was
patched for the same chrono RFC3339 issue (`vendor/agentmesh-relay/`) and
for one additional fix: **"never give up" reconnect** — vendored A uses
`maxReconnectAttempts = Infinity` with capped 60s backoff (vs upstream's
5 attempts). We applied the same default on B's adapter side; AGT's
`MeshClient` already has configurable reconnect.

### 7. X3DH key exchange + Double Ratchet

| Stage | A | B | Parity |
|---|---|---|---|
| Generate signed prekey + one-time prekeys | `X3DHKeyManager.generateSignedPreKey()` | `X3DHKeyManager.generateSignedPreKey()` | ✅ |
| Build prekey bundle | `buildBundle()` (patched in vendored — was emitting empty signature) | `buildBundle()` | ✅ |
| Initiator handshake | `X3DH.initiateSession()` | `X3DH.initiate()` | ✅ |
| Responder handshake | `X3DH.respondToSession(signedPrekey)` (patched — was missing the signedPrekey param) | `X3DH.respond({signedPrekey, oneTimePrekey})` | ✅ |
| Double Ratchet step | `Session.encrypt/decrypt` (patched — `initializeResponder` was using wrong keypair) | `Ratchet.encrypt/decrypt` | ✅ |
| AEAD cipher | XSalsa20-Poly1305 (libsodium) | XSalsa20-Poly1305 (libsodium) | ✅ |

The 5 cryptographic patches in `vendor/agentmesh-sdk/` brought A to
correctness. B is upstream-clean — Microsoft's implementation is correct
out of the box. **This is the primary motivation for the swap**: removing
the maintenance burden of carrying 5 protocol-level patches.

### 8. KNOCK protocol (session establishment)

Both SDKs implement KNOCK identically: send a signed handshake frame as
the first message of a new session, the receiver evaluates policy + trust,
and either auto-accepts (returning their X3DH params) or rejects.

| Aspect | A | B |
|---|---|---|
| KNOCK frame in first send | ✅ (patched — was missing) | ✅ |
| `onKnock(handler)` | ✅ | ✅ |
| Auto-accept default | enforce-off (we explicitly call `enableKnockEnforcement()`) | enforce-on (always) |
| Trust threshold integration | Caller provides via `onKnock` handler | Same |

**Behavior difference:** B is always-enforce. The runtime's KNOCK handler
runs in both modes — so calling `enableKnockEnforcement()` on B is a no-op
and on A is required. The adapter handles this transparently.

### 9. Plaintext peers (mesh-trusted, no E2E)

| Capability | A | B |
|---|---|---|
| `addPlaintextPeer(amid)` | ✅ | ✅ |
| `removePlaintextPeer(amid)` | ✅ | ✅ |
| `isPlaintextPeer(amid)` | ✅ | ✅ |

Used for parent↔child sandbox messaging where both endpoints are inside
our trust boundary and the encryption overhead is unnecessary. Identical
on both sides.

### 10. File transfer

| Capability | A | B |
|---|---|---|
| Send blob ≤ 10MB | `sendFile(toAmid, name, mime, bytes)` | `sendFile(toAmid, name, mime, bytes)` |
| Receive | Comes through `onMessage` with `type: "file"` | Same |

Both implementations chunk and re-assemble identically; wire format matches.

---

## Wiring (the swapping mechanism)

### Components

```
┌─────────────────────────────────────────────────────┐
│  runtimes/openclaw/src/index.ts                     │
│  ─────────────────────────────                      │
│  await createMeshTransport({                        │
│    relayUrl, registryUrl, identity, displayName     │
│  })                                                 │
└────────────────┬────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────┐
│  mesh-plugin/src/transport-factory.ts               │
│  resolveMeshProvider(env)                           │
│   ├── "agt"       → AgtTransport      (B)           │
│   └── default     → MeshConnection    (A)           │
└────────────┬───────────────────────┬────────────────┘
             │                       │
             ▼                       ▼
┌──────────────────────┐  ┌──────────────────────────┐
│  connection.ts       │  │  agt-transport.ts        │
│  (vendored adapter)  │  │  (AGT adapter)           │
│  delegates to        │  │  delegates to            │
│  @agentmesh/sdk      │  │  @microsoft/agent-       │
│  AgentMeshClient     │  │  governance-sdk          │
│                      │  │  MeshClient              │
└──────────────────────┘  └──────────────────────────┘
```

### Environment variable

```bash
# Default — vendored SDK (current production)
AZURECLAW_MESH_PROVIDER=vendored   # or unset

# Opt-in — AGT SDK
AZURECLAW_MESH_PROVIDER=agt
```

Anything else (typo, empty, missing) falls back to `vendored` so a
mis-configured pod always boots on the safe path.

### IMeshTransport contract

`mesh-plugin/src/transport-interface.ts` is the canonical contract. Both
adapters MUST expose every method on it. The Phase 2 compatibility test
(`mesh-plugin/src/transport-phase2-compat.test.ts`) pins this — if either
adapter regresses, the build fails.

### Optional dependency

`@microsoft/agent-governance-sdk` is declared as an `optionalDependency`
on `runtimes/openclaw`. The factory's dynamic `import()` is wrapped in
try/catch so that:

- Pods built without AGT installed → factory throws clearly when
  `AZURECLAW_MESH_PROVIDER=agt` is set, but works fine on default.
- Pods with AGT installed → can flip the env var freely.

### Identity sharing

Both providers receive **the same Ed25519 keys** generated by vendored
`Identity.generate()`. We extract via `identity.toData()`, strip the
`ed25519:` / `x25519:` base64 prefixes, and hand raw bytes to the factory.
The factory passes raw bytes to AGT's `MeshClient` constructor and the
full `sdkIdentity` to vendored. **AMIDs do not change when you swap
providers** — same signing key, same SHA-256, same AMID.

---

## Patch-by-patch audit (vendored vs AGT upstream stack)

This audit answers the question: **when we move fully upstream to the
AGT stack (AGT TS SDK + AGT Python relay + AGT Python registry), do we
lose any correctness or robustness fixes that exist in our vendored
patches?** AGT will eventually replace `vendor/agentmesh-{sdk,relay,registry}`
entirely, so each vendored patch needs an equivalent on the AGT side or
a documented decision that the issue doesn't apply.

The AGT components audited:

- **TS SDK**: `agent-governance-typescript/src/encryption/{mesh-client.ts, x3dh.ts, channel.ts}` (and `identity.ts`)
- **Python relay**: `agent-governance-python/agent-mesh/src/agentmesh/relay/app.py`
- **Python registry**: `agent-governance-python/agent-mesh/src/agentmesh/registry/{app.py, store.py}`

Legend:

- ✅ **AGT has the equivalent fix already** — no port needed
- ❌ **Gap in AGT** — must be patched upstream before moving off vendored
- ⚠️ **Different model / partial** — AGT solves the same problem differently; verify it's acceptable
- ✋ **Adapter responsibility** — AGT MeshClient deliberately doesn't expose this surface; consumer wraps it (we ship this in `agt-transport.ts`)
- ➖ **N/A** — patch addresses something specific to vendored impl that doesn't apply to AGT

### Vendored SDK patches → AGT TS SDK

| # | Patch summary | AGT equivalent | Verdict |
|---|---|---|---|
| **#1** | `PrekeyManager.buildBundle()` emitted empty signature, dropped public keys | `x3dh.ts:109-126` always signs the prekey; `getPublicBundle()` always populates X25519 pubkeys | ✅ |
| **#2** | `base64Decode` crashed on `x25519:` / `ed25519:` key prefixes | `identity.ts:464+` strips prefix before decode (helper present) | ✅ |
| **#3** | X3DH→Double-Ratchet handoff: peer's `signedPreKey` not passed as initial DH; `Session.initializeResponder()` used wrong keypair | `channel.ts:46-49` (sender) and `:85-88` (receiver) pass the correct keypair into the ratchet | ✅ |
| **#4a** | KNOCK frame must be sent on the wire when `establishSession()` is called (vendored did X3DH locally only) | `mesh-client.ts:247-256` sends a `knock` frame as part of `establishSession()` | ✅ |
| **#4b** | First encrypted message must auto-bootstrap the **responder** session (extract X3DH `establishment` from frame, call `initializeResponder` on the fly) | **Fixed locally on AGT branch `azureclaw-meshclient-event-hooks`** (commit `d75ea37b`): sender now embeds `establishment` in the `knock` frame; `handleKnock` auto-calls `acceptSession()` when present. Backwards-compatible with legacy peers that omit the field. | ✅ (local AGT branch) |
| **#5** | KNOCK race: encrypted message arrives between `knock` send and `knock_accept` receipt → silently dropped | `mesh-client.ts:57` (`knockPending` Map) + `:397-410` (handleMessage awaits resolution) — same fix already in AGT | ✅ |
| **#6** | Connect-then-prekey-upload race: registry rejects prekeys before `register` resolves | AGT MeshClient does no registry HTTP itself — sequencing is the consumer's responsibility (our adapter handles this) | ✋ adapter |
| **#7** | `submitReputation()` swallowed registry 4xx/5xx errors | AGT MeshClient has no `submitReputation`; reputation lives at `/v1/agents/{did}/reputation` on AGT registry. Our adapter logs status + body on non-2xx (landed in this commit) | ✋ adapter |
| **#8** | After transport-connect failed, `client.connected` was left at `true` causing reconnect deadlock | AGT collapses transport+client; `connected` is set inside `ws.onopen` only and reset by `onclose`. Edge case: open→immediate-close before connect-frame ack leaves the connect promise resolved but `connected = false`. Subsequent `send()` throws "Not connected to relay" — recoverable via manual `reconnect()`. | ⚠️ minor edge |
| **#9** | Auto-reconnect: `RelayTransport` defaulted to 5 attempts → agents went mesh-deaf forever | **Fixed locally on AGT branch `azureclaw-meshclient-event-hooks`** (commit `d75ea37b`): MeshClient now auto-reconnects on non-1000 close with exponential backoff (1s → 60s cap, ±20% jitter), defaults to `maxReconnectAttempts: Infinity`. Opt-out via `autoReconnect: false`. | ✅ (local AGT branch) |
| **#12** | Registry `fetch` had no retry on transient network failure | AGT MeshClient has no fetch. Our adapter (`agt-transport.ts`) now uses `fetchWithRetry` (3 attempts, 250/750/2000ms backoff, transient-5xx aware) for `lookup`, `submitReputation`, and discovery (landed in this commit) | ✋ adapter |

### Vendored relay patches → AGT Python relay

| # | Patch summary | AGT equivalent | Verdict |
|---|---|---|---|
| Relay-1 | Raw timestamp signature verification (chrono `to_rfc3339()` `Z` vs `+00:00` mismatch broke Ed25519 verify) | AGT relay does **not** verify per-frame signatures. Auth model is shared-secret token only (`AGENTMESH_RELAY_TOKEN` at `app.py:117-127`). Different security model — weaker against compromised tokens but immune to the chrono bug. | ➖ different model (flag for security review) |
| Relay-2 | Session-aware connection (ghost cleanup): on duplicate connect for same AMID, close old socket with `4001 SessionReplaced` | AGT relay (`app.py:130`) overwrites `self._connections[agent_did]` without explicitly closing the old socket. Old WS lingers until heartbeat timeout (90s, `OFFLINE_THRESHOLD`). Functionally similar but slower cleanup; client cannot distinguish supersede from drop. | ⚠️ slower / no explicit close code |
| Relay-3 | HTTP `/health` endpoint | AGT relay has `/health` at `app.py:87` returning `{"status":"healthy","connected_agents":n}` | ✅ |
| Relay-4 | Explicit close codes (`4001 SessionReplaced`, `4002 PingTimeout`) so client can suppress reconnect storms | AGT relay uses `4001 first-frame-error`, `4002 missing-from`, `4003 auth-fail` (different semantics — error responses, not lifecycle signals). No SessionReplaced or PingTimeout codes. | ⚠️ different semantics |

### Vendored registry patches → AGT Python registry

| # | Patch summary | AGT equivalent | Verdict |
|---|---|---|---|
| Registry-1 | Raw timestamp Ed25519 signature verification (mirror of relay fix) | AGT registry `app.py:54-98` (`verify_ed25519_timestamp_auth`) verifies Ed25519 over the **raw timestamp string** (`vk.verify(timestamp_str.encode("utf-8"), sig)` at line 94). Identical approach, never had the chrono bug. | ✅ |
| Registry-2 | Ghost cleanup + heartbeat + 5-minute freshness window for online status | AGT registry tracks `last_seen` per agent (`store.py:29`) and computes `online = (now - last_seen) < 90s` (`app.py:236`). Tighter window (90s vs 5min) but same approach. | ✅ (tighter window) |
| Registry-3 | `feedback_count` SQL referenced wrong table name | AGT registry uses an in-memory store (`store.py`) — not affected by the SQL bug. AGT in-memory implementation is correct. | ➖ different impl |
| Registry-4 | Op-hardening (graceful shutdown, stale cleanup, validation caps, TOCTOU) | AGT registry uses Pydantic models with `Field(ge=0.0, le=1.0)` validation (`app.py:47`) and FastAPI handles graceful shutdown. Stale cleanup is implicit via the freshness window. TOCTOU: AGT in-memory store uses dict ops which are atomic in CPython — different concurrency model than our vendored Postgres registry. | ✅ different impl, equivalent guarantees |

### Real gaps blocking the move-upstream scenario (now fixed locally)

Two genuine issues in AGT itself were identified. **Both are now fixed
on the local AGT branch `azureclaw-meshclient-event-hooks`** (commit
`d75ea37b`), held locally pending coordination with the AGT team for an
upstream PR. AGT TS test suite: 398/398 pass.

#### Gap-G1: receiver-side X3DH bootstrap — ✅ fixed locally

**Vendored A:** when the first encrypted message arrives from a peer
with no prior session, `handleMessage` extracts the X3DH `establishment`
data embedded in the frame and calls `Session.initializeResponder()` to
auto-create the responder side of the channel.

**AGT B (before fix):** the `acceptSession(peerId, establishment)` API
existed but `ChannelEstablishment` was never serialized onto the wire.
Any fresh encrypted session failed on first receive.

**Local fix on AGT branch (commit `d75ea37b`):**
- `establishSession()` now creates the channel BEFORE sending KNOCK and
  embeds the establishment as `{ ik, ek, otk? }` (base64) on the frame.
- `handleKnock()` auto-calls `acceptSession()` when an accepted KNOCK
  carries `establishment` and no prior session exists.
- Backwards-compatible: legacy peers that don't embed `establishment`
  still go through the manual `acceptSession()` path.
- Malformed `establishment` rejects the KNOCK and fires
  `onError("knock", ...)`.

Tests: `tests/mesh-client-knock-bootstrap.test.ts` (5/5 pass).

#### Gap-G2: no auto-reconnect loop in MeshClient — ✅ fixed locally

**Vendored A:** `RelayTransport` schedules a reconnect on every
`ws.onclose` that wasn't a clean client-initiated `1000`. Patch #9 sets
the default to `maxReconnectAttempts = Infinity` with exponential
backoff capped at 60s.

**AGT B (before fix):** `MeshClient.reconnect()` existed but was never
called automatically. Network blips left agents mesh-deaf forever.

**Local fix on AGT branch (commit `d75ea37b`):**
- New options: `autoReconnect` (default `true`),
  `maxReconnectAttempts` (default `Infinity`),
  `reconnectBaseDelayMs` (default `1000`),
  `reconnectMaxDelayMs` (default `60000`).
- `ws.onclose` schedules a reconnect with exponential backoff + ±20%
  jitter on any non-1000 / non-client-initiated close.
- `disconnect()` cancels any pending reconnect timer and sends
  explicit close code 1000.
- After `maxReconnectAttempts`, fires
  `onError("ws", ..., "auto-reconnect gave up after N attempts")`.

Tests: `tests/mesh-client-auto-reconnect.test.ts` (6/6 pass).

### Soft / minor

- **Relay-2 (slower ghost cleanup):** AGT will keep stale connections
  for up to 90s before the heartbeat check evicts them. Vendored closes
  immediately with `4001`. Acceptable for now; would be a nice-to-have
  upstream improvement.
- **Relay-4 (close-code semantics):** AGT uses `4001-4003` for protocol
  errors, not lifecycle signals. Clients cannot distinguish supersede
  from network drop. With G2 now fixed, the gap is reduced to a
  storm-suppression hint rather than a functional issue.
- **SDK-#8 fast-fail handshake edge:** AGT resolves the connect promise
  inside `ws.onopen` even if `ws.onclose` fires immediately after. Defensive
  fix: only resolve once the relay's first `connected` ack arrives.

### Summary

| Class | Count | Status |
|---|---|---|
| AGT already has it (✅) | 8 | No action needed |
| Adapter responsibility (✋) | 3 | Landed in this commit (#7, #12); #6 was already correct in adapter |
| Different model, equivalent (➖ / ⚠️) | 5 | Documented; no functional regression expected |
| Real gaps in AGT — fixed on local branch (✅ local) | **2** | **G1** + **G2** — `azureclaw-meshclient-event-hooks` `d75ea37b`, pending upstream PR |

The 2 real gaps (G1, G2) and the 3 diagnostic event hooks are all
implemented on the local AGT branch `azureclaw-meshclient-event-hooks`
(commits `e5f4346f` for hooks, `d75ea37b` for G1+G2). The branch is
held locally — NOT pushed — pending coordination with the AGT team for
an upstream PR. From AzureClaw's perspective, AGT is now
feature-complete for the upstream-only scenario.

### Adapter-side fixes landed in this commit

The ✋ items are adapter-only — they don't require AGT changes. They
are applied in `mesh-plugin/src/agt-transport.ts`:

- `submitReputation` now logs status code + body on non-2xx and logs
  network errors (vendored swallowed both).
- `lookup`, `submitReputation`, and discovery search now use a
  bounded-retry policy: 3 attempts, exponential backoff
  (250ms / 750ms / 2000ms), retries on transient 5xx.

---

## AGT upstream changes required

The local AGT branch `azureclaw-meshclient-event-hooks` (NOT pushed)
adds 3 public methods + their internal wiring to
`agent-governance-typescript/src/encryption/mesh-client.ts`:

```typescript
onError(handler: (kind: string, fromAmid: string, detail: string) => void): void
onE2EVerified(handler: (peerAmid: string, isFirstPeer: boolean) => void): void
onDisconnect(handler: (reason: "client" | "server" | "ws-error", code?: number) => void): void
```

Internal wiring:

- `ws.onerror` fans out to `errorHandlers` with `kind="ws-error"`
- `ws.onclose` fans out to `disconnectHandlers` with reason="client" if
  code === 1000 else "server"
- `handleMessage()` decrypt path:
  - missing-session → `onError("no_session", from, ...)`
  - decrypt-throw → `onError("decrypt_failed", from, ...)`
  - first-successful-decrypt-per-peer → `onE2EVerified(peer, isFirstPeer)`

Total: 8 new tests in `tests/mesh-client-event-hooks.test.ts`, all pass
alongside the existing 379 tests (387/387 green). Build is clean.

The branch is held locally pending the AGT team's review for the upstream
PR. Until merged + published in the next `@microsoft/agent-governance-sdk`
release, our adapter's optional-chain (`client.onError?.(...)`) makes
these hooks no-ops on the published 3.5.0 — provider stays functional,
just without diagnostic hooks.

The 3 governance methods (`lookup`, `submitReputation`,
`enableKnockEnforcement`) are intentionally NOT proposed for AGT upstream.
They belong in a separate `RegistryClient`, not on `MeshClient` (which
is pure transport). Our adapter implements them via REST.

---

## Migration strategy

### Phase 1 (already done in PR #244)
- Factory + interface + AGT adapter scaffold.
- Behavior selectable via `AZURECLAW_MESH_PROVIDER`.
- Optional dependency wired.

### Phase 2 (this PR — #245)
- IMeshTransport extended with the 6 missing methods.
- Both adapters fully implement the surface.
- Runtime swapped to use the factory.
- Side-by-side compat test pins the contract.
- AGT local branch with the 3 missing event hooks.
- **Patch-by-patch audit committed (this section)** — finds 2 protocol-level gaps in AGT (G1, G2) blocking the upstream-only scenario.
- **Adapter fixes for ✋ items**: reputation logging (#7) + registry fetch retry (#12) ported into `agt-transport.ts`.

### Phase 3 (cross-provider deployment)

A and B can run side by side **only with their own server stacks**:
- A: vendored relay + vendored registry
- B: AGT Python relay + AGT Python registry

A↔B cross-provider chat is not supported (different relay wire formats
by design). Each provider talks to its own service. This is acceptable
because the swap unit is the entire sandbox, not individual messages.

To test cross-provider in dev:
1. Deploy AGT relay+registry into the kind cluster alongside the vendored ones (different namespaces).
2. Set `AZURECLAW_MESH_PROVIDER=agt` on a sandbox + point its router at AGT's relay/registry URLs.
3. Run two sandboxes (one A, one B) and verify each works end-to-end with a peer of the same provider.

### Phase 4 (move fully upstream — gated on G1 + G2)

Cannot proceed until the AGT team accepts patches for:

- **G1** — receiver-side X3DH bootstrap (auto-create responder session from embedded establishment data)
- **G2** — auto-reconnect loop with exponential backoff (`Infinity` attempts, 60s cap)

Optional but recommended:
- Relay-2 — eager ghost cleanup with explicit `4001 SessionReplaced` close code
- Relay-4 — distinct close codes for supersede vs ping-timeout vs network drop
- SDK-#8 — defensive fast-fail handshake fix

Once G1 + G2 land in `@microsoft/agent-governance-sdk`:

1. Bump the `optionalDependencies` pin to the new minor.
2. Verify in dev: B-only end-to-end works for spawn → KNOCK → encrypted reply roundtrip with the AGT relay+registry.
3. Soak with `AZURECLAW_MESH_PROVIDER=agt` as default for ≥ 1 week.
4. Drop `vendor/agentmesh-sdk/`, `vendor/agentmesh-relay/`, `vendor/agentmesh-registry/`.
5. Drop `connection.ts` (vendored adapter); keep `agt-transport.ts` only.
6. Remove `AZURECLAW_MESH_PROVIDER` env var (now single-provider).
7. Switch dev infra to deploy AGT relay+registry instead of vendored ones.

---

## Testing matrix

| Test | Coverage | Status |
|---|---|---|
| `mesh-plugin/src/transport-factory.test.ts` | Factory env-var resolution | 5 tests ✅ |
| `mesh-plugin/src/transport-phase2-compat.test.ts` | Both adapters expose all 6 methods | 16 tests ✅ |
| `mesh-plugin/src/connection.test.ts` | Vendored adapter end-to-end | 16 tests ✅ |
| `mesh-plugin/src/agt-transport.test.ts` | AGT adapter unit tests | 8 tests ✅ |
| `mesh-plugin/src/agt-transport.live.test.ts` | AGT against real services | 2 skipped (live-only) |
| AGT `tests/mesh-client-event-hooks.test.ts` | New event hooks | 8 tests ✅ |

Total: **97 mesh-plugin tests pass** (81 pre-Phase-2 + 16 compat).
AGT side: **387/387 pass**.

---

## Open questions / known limitations

1. **Reputation API standardization.** Our adapter assumes registry
   `/registry/lookup/{amid}` and `/registry/feedback`. AGT registry uses
   `/v1/agents/{did}/reputation` with a different shape (EWMA score
   updates). When we move to AGT registry, the adapter's reputation
   methods need re-targeting; the contract via IMeshTransport stays
   the same.

2. **`enableKnockEnforcement` semantics.** AGT B is always-on. If a
   future use case needs to disable enforcement (e.g., trusted-network
   testing), AGT would need a runtime toggle. Out of scope for now.

3. **Cross-provider message interop is not a goal.** A and B speak to
   different relay implementations. The swap unit is the sandbox, not
   the message. There is no scenario where an A sandbox sends a message
   directly to a B sandbox — both endpoints in any conversation must be
   the same provider.

4. **AGT version pinning.** We pin `^3.5.0` in `optionalDependencies`.
   Once the AGT team releases the version with our event hooks, bump to
   that minor.

---

## References

- `mesh-plugin/src/transport-interface.ts` — IMeshTransport contract
- `mesh-plugin/src/transport-factory.ts` — provider selection
- `mesh-plugin/src/connection.ts` — vendored adapter (A)
- `mesh-plugin/src/agt-transport.ts` — AGT adapter (B)
- `runtimes/openclaw/src/index.ts:478-560` — runtime swap point
- `vendor/agentmesh-sdk/README.md` — list of 8 vendored patches
- `vendor/agentmesh-relay/README.md` — relay-side chrono fix
- `vendor/agentmesh-registry/README.md` — registry-side chrono fix
- AGT branch `azureclaw-meshclient-event-hooks` (local, sha `e5f4346f`,
  NOT pushed) — proposed upstream changes
