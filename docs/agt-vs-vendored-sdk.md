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
| **A** (vendored) | `@agentmesh/sdk` v0.1.2 | `vendor/agentmesh-sdk/` (8 patches over upstream amitayks) | ✅ Yes |
| **B** (AGT) | `@microsoft/agent-governance-sdk` v3.5.0+ | npm (Microsoft AGT) | Opt-in |

Set `AZURECLAW_MESH_PROVIDER=agt` in the sandbox environment to swap to B.
The default (`vendored`) uses A. Anything else falls back to A.

After Phase 2 the runtime never imports a transport class directly — it
calls `createMeshTransport({...})` from `@azureclaw/mesh`, which decides
which adapter to instantiate. Both adapters expose the **same**
`IMeshTransport` interface so the rest of the runtime is provider-agnostic.

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

Relay is also a **service** (`agentmesh-relay`). Both SDKs speak the same
WebSocket wire protocol:

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

### Phase 2 (this PR)
- IMeshTransport extended with the 6 missing methods.
- Both adapters fully implement the surface.
- Runtime swapped to use the factory.
- Side-by-side compat test pins the contract.
- AGT local branch with the 3 missing event hooks.

### Phase 3 (next)
- Default flip: `AZURECLAW_MESH_PROVIDER=agt` in sandbox image.
- Soak in dev for ≥ 1 week with both providers cross-tested
  (parent on A, child on B, and vice versa).
- Confirm wire compatibility (they should — same protocol, same registry,
  same relay).

### Phase 4 (cleanup)
- Once AGT upstream PR merges + publishes (with the 3 event hooks):
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

3. **Cross-provider interop testing.** Phase 3 should explicitly verify
   that an A-parent can E2E-message a B-child and vice versa. The wire
   protocol is identical, so this should "just work", but we don't have
   a CI test for it yet.

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
