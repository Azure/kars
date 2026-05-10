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
> The **AGT path is NOT yet functionally complete on the wire.** A deep
> audit of every vendored patch against AGT's `MeshClient` found
> **5 protocol-level gaps** that prevent AGT from interoperating with our
> vendored relay/registry without further changes (see
> [Patch-by-patch audit](#patch-by-patch-audit-vendored-vs-agt) below).
> Phase 3 (default flip + cross-provider interop) cannot proceed until
> those gaps are closed in AGT.
>
> The 3 event hooks we added on the local AGT branch are necessary but
> **not sufficient** — diagnostic hooks alone don't fix the wire protocol.

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

Relay is also a **service** (`agentmesh-relay`). Both SDKs are intended
to speak the same WebSocket wire protocol — but per [Gap G5](#gap-g5-connect-frame-incompatibility),
**AGT's connect frame is currently incompatible with our vendored relay's
auth requirements**. Wire-protocol parity for `connect` is a Phase 3
prerequisite.

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

## Patch-by-patch audit (vendored vs AGT)

Every patch in `vendor/agentmesh-sdk/`, `vendor/agentmesh-relay/`, and
`vendor/agentmesh-registry/` was checked against AGT's TypeScript and Rust
trees. Citations point to the file/line where AGT either has the fix
already, or where the equivalent code is missing/wrong.

Legend:

- ✅ **Already correct in AGT** — no port needed
- ✋ **Adapter-side compensation** — gap is on AGT but our `agt-transport.ts` works around it
- ❌ **Gap (blocks Phase 3)** — protocol-level mismatch that must be fixed in AGT before the swap can carry production traffic
- ➖ **N/A** — server-side (relay/registry) patch; both providers share the same patched server

### Vendored SDK patches (`vendor/agentmesh-sdk/README.md`)

| # | Patch | Vendored fix | AGT status | Verdict |
|---|---|---|---|---|
| **#1** | `PrekeyManager.buildBundle()` emits empty signature + drops public keys | Re-sign signed prekey, persist X25519 public keys | `agent-governance-typescript/src/encryption/x3dh.ts:109-126` always signs and `getPublicBundle()` always populated | ✅ |
| **#2** | `base64Decode` crashes on `x25519:` / `ed25519:` key prefixes from registry | Strip prefix before decode | `agent-governance-typescript/src/identity.ts:464+` ports the prefix-strip helper | ✅ |
| **#3** | X3DH→Double-Ratchet handoff: peer's `signedPreKey` not passed as initial DH | `respondToSession` takes `signedPrekey` param; `Session.initializeResponder()` uses it | `agent-governance-typescript/src/encryption/channel.ts:46-49` (sender) and `:85-88` (receiver) pass the correct keypair | ✅ |
| **#4a** | Sender side of KNOCK protocol: `establishSession()` did X3DH locally but never sent the KNOCK frame to relay | Send KNOCK + embed X3DH params in first message | AGT `mesh-client.ts:247-256` sends KNOCK frame, but **does NOT embed X3DH `establishment` in the first message** — receiver never gets the responder material. See [Gap G1](#gap-g1-receiver-side-x3dh-bootstrap). | ❌ |
| **#4b** | Receiver side of KNOCK: first encrypted message must auto-create the responder session from embedded X3DH params | Vendored `handleMessage` extracts establishment data and bootstraps `Session.initializeResponder` on the fly | AGT `mesh-client.ts:393-470` requires the caller to manually invoke `acceptSession(peerId, establishment)`; there is no path from a `message` frame to session creation | ❌ |
| **#5** | KNOCK race: encrypted message arrives between `knock` send and `knock_accept` receipt → dropped | `knockPending` Map; `handleMessage` waits for resolution before decrypting | AGT `mesh-client.ts:57` (`knockPending` Map) + `:397-410` (handleMessage await) — same fix already in place | ✅ |
| **#6** | Connect-then-prekey-upload race: registry rejects prekeys before `register` resolves | Sequence `register()` → `uploadPrekeys()` strictly | AGT MeshClient does **no** registry HTTP at all (see [Gap G3](#gap-g3-registry-rpcs-not-in-meshclient)) — sequence enforced in our adapter. | ✋ adapter-side |
| **#7** | `submitReputation()` swallowed registry errors (no logging on 4xx/5xx) | Log status + body on non-200 | AGT MeshClient has no `submitReputation`. Our adapter (`mesh-plugin/src/agt-transport.ts:640+`) does the POST; **logging on non-200 was missing — fixed in this commit (see below).** | ✋ adapter-side |
| **#8** | After `transport.connect()` fails, `client.connected` was left at `true` causing reconnect deadlock | Reset on transport-fail | AGT collapses transport+client (single `MeshClient`); `connected = true` is set inside `ws.onopen` only, and `onclose` resets it. The exact deadlock from vendored doesn't apply, but a related issue exists: a fast-fail mid-handshake (open → immediate close) leaves no observer signalling reject(). [Verified low-impact](#gap-g4-fast-fail-handshake-edge); not blocking. | ⚠️ minor |
| **#9** | Auto-reconnect: vendored `RelayTransport` defaulted to 5 attempts → agents went mesh-deaf forever | `maxReconnectAttempts = Infinity`, exponential backoff capped at 60s | AGT `mesh-client.ts:156-167` exposes `reconnect()` as a manual method only. **No auto-reconnect loop on `ws.onclose`.** See [Gap G2](#gap-g2-no-auto-reconnect-loop). | ❌ |
| **#12** | Registry `fetch` had no retry on transient network failure | Bounded retry with exponential backoff | AGT MeshClient has no fetch. Our adapter (`agt-transport.ts:334`, `:607`, `:640`) does single-shot fetches **without retry — fixed in this commit (see below).** | ✋ adapter-side |

### Vendored relay patches (`vendor/agentmesh-relay/README.md`)

The relay is a **server** (Rust). Both A and B speak to the same
`agentmesh-relay` instance, so server patches benefit both.

| # | Patch | Server-side or client-coupled? |
|---|---|---|
| Raw timestamp signature verification (`chrono::DateTime::to_rfc3339()` `Z` vs `+00:00` mismatch) | Server stores raw timestamp string and verifies signature over those exact bytes | ➖ Server-side — both providers benefit. **Coupled requirement on the client:** the SDK must send the `timestamp` field as a *string* (not a re-serialized `DateTime`). AGT `mesh-client.ts:102-106` **does not send a `timestamp` at all** in the connect frame — see [Gap G5](#gap-g5-connect-frame-incompatibility). | ❌ (client-side coupling) |
| Session-aware connection (ghost connection cleanup) | When a new session for the same AMID arrives, the old socket is closed with code `4001 SessionReplaced` | ➖ Server-side. **Coupled requirement on the client:** to avoid reconnect storms after supersede, client should distinguish `4001` from generic disconnect. AGT `mesh-client.ts:131-132` only distinguishes `1000` (normal) from non-`1000` (server). | ⚠️ partial |
| HTTP `/health` endpoint | Pure server-side health check | ➖ Server-side. No SDK coupling. | ✅ |
| Explicit close codes (`SessionReplaced=4001`, `PingTimeout=4002`) | Both providers receive these via `ws.onclose.code` | ➖ Server-side. AGT does not currently special-case `4002`. | ⚠️ partial |

### Vendored registry patches (`vendor/agentmesh-registry/README.md`)

| # | Patch | Server-side or client-coupled? |
|---|---|---|
| Raw timestamp signature verification (mirror of relay fix) | Server-side | ➖ Server-side. **Coupled requirement:** clients must send `timestamp` as a string in registry POSTs (registration, prekey upload, feedback). AGT MeshClient does not register at all → adapter handles. | ✋ adapter-side |
| Ghost cleanup + heartbeat + 5-minute freshness window | Server-side | ➖ Server-side | ✅ |
| `feedback_count` SQL referenced wrong table name | Server-side bug fix | ➖ Server-side | ✅ |
| Op-hardening (graceful shutdown, stale cleanup, validation caps, TOCTOU) | Server-side | ➖ Server-side | ✅ |

### Critical gaps (block Phase 3)

#### Gap G1: receiver-side X3DH bootstrap

**Vendored A:** when the first encrypted message arrives from a peer with
no session, `handleMessage` extracts the X3DH `establishment` data
embedded in the frame and calls `Session.initializeResponder()` to
auto-create the responder side of the channel. From the consumer's POV,
encryption "just works" the first time a message arrives.

**AGT B:** `mesh-client.ts:393-470` (`handleMessage`) finds no session,
fires `onError("no_session", ...)`, and drops the message. The only way
to bootstrap the responder side is to call `acceptSession(peerId, establishment)`
manually — but the establishment data is never extracted from the wire
because AGT's `message` frame schema doesn't carry it.

**Required fix in AGT:** extend the `message` frame to optionally carry
`establishment: ChannelEstablishment` (sent only on the first encrypted
send to a peer), and have `handleMessage` auto-call `acceptSession` when
present and no prior session exists.

#### Gap G2: no auto-reconnect loop

**Vendored A:** `RelayTransport` schedules a reconnect on every `ws.onclose`
that wasn't a clean client-initiated `1000`. Patch #9 sets the default
to `maxReconnectAttempts = Infinity` with exponential backoff capped at
60s. Result: mesh-deafness from transient network glitches is impossible.

**AGT B:** `MeshClient.reconnect()` exists but is never called automatically.
Consumers must observe `onDisconnect` and decide to call `reconnect()`
themselves — and AGT 3.5.0 doesn't even export `onDisconnect` (we added
that hook on `azureclaw-meshclient-event-hooks`).

**Required fix in AGT:** add an opt-in (or default-on) auto-reconnect
loop with the same parameters as patch #9. Implementing this in the
adapter is fragile because the adapter has no insight into the WebSocket
lifecycle from outside.

#### Gap G3: registry RPCs not in MeshClient

**Vendored A:** `AgentMeshClient` has built-in registry RPCs:
`register()`, `uploadPrekeys()`, `searchByDisplayName()`,
`fetchPrekeyBundle()`, `lookup()`, `submitReputation()`. They run
during `connect()` and on demand.

**AGT B:** `MeshClient` declares `options.registryUrl` but never reads it.
The class is pure transport. `IdentityRegistry` (`identity.ts:407`) is an
in-memory map for delegation chain validation, not a registry HTTP client.

**Compensation:** our `agt-transport.ts` adapter does the registry HTTP
calls directly (`/agents/search`, `/registry/lookup`, `/registry/feedback`).
This is sustainable for the swap, but it means **AGT's MeshClient is
non-trivial to use without wrapping it** — every consumer has to
reimplement registration, prekey upload, peer-bundle fetch.

**Open design question for AGT team:** should this surface live on
`MeshClient` (matching vendored A and our adapter), or on a separate
`RegistryClient` published alongside? Either is acceptable — but it
needs to exist somewhere standard.

#### Gap G4: fast-fail handshake edge

**Vendored A:** `RelayTransport.connect()` distinguishes "ws never opened"
(rejects connect()) from "ws opened then closed" (sets state correctly).

**AGT B:** `mesh-client.ts:99-138` only rejects on `onerror` if `!connected`.
If `onopen` fires then `onclose` fires before the connect-frame is
delivered, `connected` is briefly `true` and gets reset by `onclose` —
but the connect-promise still resolves successfully because `resolve()`
is called inside `onopen`. Consumer thinks connect succeeded; subsequent
sends will throw "Not connected to relay".

**Severity:** observed only under specific timing pathologies (relay
crashing during handshake). Not a blocker but worth fixing alongside G2.

#### Gap G5: connect frame incompatibility

**Most severe gap.** AGT `mesh-client.ts:102-106` sends:

```json
{ "v": 1, "type": "connect", "from": "did:agentmesh:abc..." }
```

Our vendored relay (`vendor/agentmesh-relay/src/types.rs:13-24`) requires:

```rust
Connect {
    protocol: String,
    amid: Amid,
    public_key: String,         // base64 Ed25519 pubkey
    signature: String,          // Ed25519 sig over timestamp
    timestamp: String,          // raw ISO string
    p2p_capable: bool,
}
```

Serde's tagged-enum deserialization rejects unknown shapes — AGT's
connect frame fails parsing on our relay and the WebSocket closes
immediately. **This means the AGT path cannot establish a connection
to our vendored relay at all.**

The corollary: AGT either (a) was designed against a different relay
implementation, or (b) expects the relay to accept unauthenticated
connect frames (which `vendor/agentmesh-relay/` doesn't).

**Required fix in AGT:** extend the connect frame to include
`protocol`, `public_key`, `signature`, `timestamp`. The signature must
be Ed25519 over the raw `timestamp` string. This is also the prerequisite
for relay patch #1 (raw-timestamp signature verification) to work for
provider B.

### Adapter-side fixes landed in this commit

The two ✋ items above (`#7` reputation logging, `#12` registry fetch
retry) are adapter-only — they don't require AGT changes. They are
applied in `mesh-plugin/src/agt-transport.ts` as part of this audit:

- `submitReputation` now logs status code + body on non-2xx.
- `lookup`, `submitReputation`, and the discovery search RPC now retry
  on transient network failure: 3 attempts, exponential backoff
  (250ms / 750ms / 2000ms).

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
- **Patch-by-patch audit committed (this section)** — finds 5 protocol-level gaps blocking Phase 3.
- **Adapter fixes for ✋ items**: reputation logging (#7) + registry fetch retry (#12) ported into `agt-transport.ts`.

### Phase 3 (BLOCKED on AGT-upstream work)

Cannot proceed until the AGT team accepts patches for:

- **G1** — receiver-side X3DH bootstrap (auto-create responder session from embedded establishment data)
- **G2** — auto-reconnect loop with exponential backoff (`Infinity` attempts, 60s cap)
- **G5** — connect-frame compatibility with our vendored relay (signed timestamp, public key)

Optional but recommended:
- **G3** — registry RPC surface (or a separate `RegistryClient`)
- **G4** — fast-fail handshake edge (defensive)
- Relay close-code handling (4001 SessionReplaced, 4002 PingTimeout)

Once these land in `@microsoft/agent-governance-sdk`:

1. Bump the `optionalDependencies` pin to the new minor.
2. Cross-test in dev: A-parent ↔ B-child and B-parent ↔ A-child.
3. Flip default `AZURECLAW_MESH_PROVIDER=agt` in the sandbox image.
4. Soak ≥ 1 week with both providers running side by side.

### Phase 4 (cleanup — only after Phase 3)
- Drop `vendor/agentmesh-sdk/` (5 protocol patches no longer needed).
- Drop `connection.ts` (vendored adapter).
- Keep `agt-transport.ts` only.
- Remove `AZURECLAW_MESH_PROVIDER` env var (now single-provider).
- Vendored relay + registry stay (server-side patches), unless those PRs
  also merge upstream.

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
   `/registry/lookup/{amid}` and `/registry/feedback`. AGT may eventually
   propose its own registry shape; if so, the adapter is the only file
   that needs to change.

2. **`enableKnockEnforcement` semantics.** AGT B is always-on. If a
   future use case needs to disable enforcement (e.g., trusted-network
   testing), AGT would need a runtime toggle. Out of scope for now.

3. **Cross-provider interop testing.** Phase 3 was originally scoped to
   verify A↔B interop. The audit (see [Patch-by-patch audit](#patch-by-patch-audit-vendored-vs-agt))
   shows this is **currently impossible** — gap G5 means AGT cannot even
   connect to the vendored relay. Once AGT lands the connect-frame fix,
   wire compatibility should follow naturally because both sides target
   the same Rust services.

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
