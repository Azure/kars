# Vendored @agentmesh/sdk (patched)

Upstream: https://github.com/amitayks/agentmesh/tree/main/agentmesh-js (v0.1.2)
npm: `@agentmesh/sdk` — published by `amitaykeisar`, canonical repo `github.com/agentmesh/agentmesh` (private)

## Patches Applied

### 1. PrekeyManager.buildBundle() — empty signature + missing prekeys
**File:** `src/encryption/prekey.ts`

- `signedPrekeySignature: new Uint8Array(0)` → re-signs via `identity.sign()`
- `oneTimePrekeys: []` → returns stored public keys
- Added `oneTimePrekeyPublicKeys` map to `PrekeyState`
- Updated serialization/deserialization

### 2. base64Decode — key type prefix crash
**Files:** `src/encryption/prekey.ts`, `src/client.ts`, `src/session/index.ts`

Registry returns keys with `x25519:` or `ed25519:` prefix. All `base64Decode` /
`fromBase64` callsites now strip the prefix before `atob()`.

### 3. X3DH → Double Ratchet handoff — missing peer ratchet key
**File:** `src/encryption/session.ts`

`initiateSession()` was passing `undefined` for the peer's initial ratchet key.
Per Signal Protocol, the peer's **signed prekey** IS the initial ratchet public
key. Fixed to pass `peerBundle.signedPrekey`.

Added `activateSessionDirect()` for sessions where the peer DH key is already
set during initialization.

### 4. KNOCK protocol — not wired to relay transport
**File:** `src/client.ts`

`establishSession()` did X3DH locally but never sent a KNOCK via the relay.
Fixed to create and send a KNOCK message through `transport.send()` before
activating the session.

### 5. KNOCK race condition — message before KNOCK accepted
**Files:** `dist/index.js`, `dist/index.cjs`

When KNOCK and encrypted message arrive near-simultaneously (1ms apart),
the async KNOCK handler (trust lookup ~33ms) hasn't finished when the
message is processed. The message hits the `!knockAcceptedPeers.has()`
check and is rejected.

Fix: Added `knockPendingPeers` Map to track in-flight KNOCK processing.
When a message arrives for a peer with a pending KNOCK, it awaits the
KNOCK resolution instead of immediately rejecting. If KNOCK is accepted,
the message is processed normally. If rejected, it's blocked as before.

### 6. connect() prekey/register order — registry requires registration first
**Files:** `src/client.ts`, `dist/index.js`, `dist/index.cjs`

Upstream `connect()` registers the agent *before* uploading prekeys. A prior
patch attempted to swap the order (uploadPrekeys → register), but the registry
returns 404 for prekey uploads when the agent isn't registered yet — making
prekeys permanently unavailable.

Fix: Reverted to `register() → uploadPrekeys()` (as registry requires).
The brief race window (~100ms) where an agent is discoverable but prekeys
aren't ready is handled by **sender-side retry** in plugin.ts: both parent
and sub-agent `mesh_send` retry `agtMeshClient.send()` on prekey errors
(parent: 8×2s, sub-agent: 5×1s).

### 7. submitReputation — silent error swallowing
**Files:** `dist/chunk-NMOWWZKF.js`, `dist/chunk-UBUGIENK.cjs`

Upstream `submitReputation()` has `catch { return false }` — errors are
completely swallowed, making reputation failures invisible. The registry
may reject reviews (400 "Rater agent not found" for deregistered sub-agents)
and the caller never knows.

Fix: Log the HTTP status + response body on non-200, and log the error
message on exception. Returns `false` as before (no behavior change for
callers) but now surfaces the actual failure reason in container logs.

### 8. connect() — stale connected state blocks reconnect
**Files:** `dist/index.js`, `dist/index.cjs`

`AgentMeshClient.connect()` sets `this.connected = true` unconditionally after
`transport.connect()` returns — even when the transport returned `false` (relay
unreachable). This creates a deadlock:

- `client.connected = true` (set unconditionally)
- `transport.connected = false` (upstream failed)
- `isConnected` → `false` (correct — checks both)
- `connect()` → throws "Already connected" (checks only `client.connected`)
- Result: client thinks it's connected, can't send, can't reconnect

Fix: Check `transport.connect()` return value. If `false`, skip setting
`this.connected = true` so subsequent `connect()` calls can retry. Also
patched `plugin.ts` reconnect paths to call `disconnect()` first, resetting
stale state before reattempting connection.

## Known Remaining Gap

The SDK's relay transport `receive` events are not wired to
`AgentMeshClient.onMessage()` handlers. This means:
- **Sending** (parent → sub-agent): fully E2E encrypted via relay ✅
- **Receiving** (sub-agent → parent): transport delivers but client doesn't
  surface to `onMessage` — messages arrive but aren't processed by the LLM

This is an upstream SDK issue (transport→client message routing not implemented).

## Patch #12 — Registry fetch retry (April 2026)

**Files:** `dist/chunk-NMOWWZKF.js`, `dist/chunk-UBUGIENK.cjs` (dist-only overlay)

`RegistryClient.fetch()` used to do a single `fetch()` call with timeout
and no retry. Brief 502/503 blips during kubectl port-forward restarts
(`host.docker.internal:18080`) and AKS ingress rolls caused
`submitReputation` (and other registry calls) to silently return `false`
— indistinguishable from "registry rejected the score".

The fix wraps `fetch()` with an endpoint-aware retry policy:

- **GET / HEAD**: always retried (idempotent).
- **POST on idempotent registry endpoints** — `/registry/register`,
  `/registry/prekeys`, `/registry/reputation`, `/registry/status`,
  `/registry/capabilities`, `/registry/revocations/bulk`,
  `/auth/oauth/authorize`: retried.
- **All other POSTs**: not retried (caller decides).
- **Triggers**: status 408/429/502/503/504 or any thrown network error.
- **Cap**: up to 3 attempts, total elapsed budget 2s, exponential backoff
  starting at 100 ms; honours `Retry-After` header up to 1.5s.

`submitReputation` (patch #7) already logs final-failure with target
AMID prefix so operators can correlate "feedback_count stays 0" with
concrete registry responses.

The router (`inference-router/src/routes/mesh.rs::agt_registry_proxy`)
mirrors the same retry policy as a defense-in-depth layer because it is
the single network egress for sandboxes.

> **Maintainer note:** This patch is a **dist-only overlay**, applied
> directly to the published bundles like patches #5, #7, and #8. **Do
> not run `npm run build` on this package** — the build regenerates
> `dist/` from `src/` and silently drops every dist-only patch. To
> modify any dist-only patch, hand-edit the chunk files and update this
> README.

## Build

> ⚠️ **Do not run `npm run build`.** Several patches (#5, #7, #8, #12)
> live exclusively in `dist/`. Running the build regenerates `dist/`
> from `src/` and erases them. The Dockerfile copies `dist/` directly,
> so the committed bundles are the source of truth for those patches.

TypeScript SDK for AgentMesh - a decentralized, end-to-end encrypted messaging protocol for AI agents.

## Features

- **End-to-End Encryption**: X3DH key exchange + Double Ratchet algorithm (Signal Protocol)
- **Decentralized Identity**: Ed25519-based identities with AgentMesh IDs (AMIDs)
- **Session Management**: KNOCK protocol for secure session establishment
- **Policy-Based Access Control**: Flexible policies for managing agent permissions
- **Transport Agnostic**: Pluggable transport layer (WebSocket, WebRTC, etc.)
- **Storage Abstraction**: Memory and persistent storage backends
- **DHT Integration**: Kademlia-based distributed hash table for peer discovery
- **DID Support**: Decentralized Identifier document generation and verification

## Installation

```bash
npm install @agentmesh/sdk
```

## Quick Start

```typescript
import { AgentMeshClient, MemoryStorage, Policy } from '@agentmesh/sdk';

// Create a new agent
const agent = new AgentMeshClient({
  storage: new MemoryStorage(),
  policy: Policy.permissive(),
});

// Initialize the agent
await agent.initialize();

// Get your AgentMesh ID
console.log('My AMID:', agent.amid);

// Connect to another agent
const session = await agent.connect(targetAmid, {
  intent: {
    capability: 'weather/forecast',
    action: 'query',
  },
});

// Send encrypted messages
await agent.send(session.sessionId, {
  type: 'query',
  location: 'New York',
});

// Receive messages
agent.onMessage((msg) => {
  console.log('Received:', msg.payload);
});
```

## Core Concepts

### Identity

Every agent has a cryptographic identity based on Ed25519 key pairs:

```typescript
import { Identity } from '@agentmesh/sdk';

// Generate a new identity
const identity = await Identity.generate();
console.log('AMID:', identity.amid);

// Save and load identity
await identity.save(storage, 'my-agent');
const loaded = await Identity.load(storage, 'my-agent');
```

### Session Establishment (KNOCK Protocol)

Sessions are established using the KNOCK protocol:

1. **KNOCK**: Initiator sends request with intent
2. **Evaluate**: Receiver evaluates against policy
3. **ACCEPT/REJECT**: Receiver responds
4. **Session**: Encrypted channel established

```typescript
import { KnockProtocol, Policy } from '@agentmesh/sdk';

const protocol = new KnockProtocol(identity);
protocol.setPolicy(Policy.verified());

// Create KNOCK request
const knock = await protocol.createKnock(targetAmid, {
  type: 'one-shot',
  ttl: 300,
  intent: { capability: 'chat/messages', action: 'send' },
});

// Validate and evaluate received KNOCK
const validation = await protocol.validateKnock(knock);
if (validation.valid) {
  const evaluation = await protocol.evaluateKnock(knock, senderInfo);
  if (evaluation.allowed) {
    const response = await protocol.createAcceptResponse(knock);
  }
}
```

### Encryption

End-to-end encryption using X3DH key exchange and Double Ratchet:

```typescript
import { PrekeyManager, SessionManager } from '@agentmesh/sdk';

// Initialize prekeys
const prekeyManager = new PrekeyManager(identity, storage);
const bundle = await prekeyManager.loadOrInitialize();

// Establish encrypted session
const sessionManager = new SessionManager(identity, storage, prekeyManager);
const { sessionId, x3dhMessage } = await sessionManager.initiateSession(
  peerAmid,
  peerBundle,
  peerSigningKey
);

// Exchange encrypted messages
const envelope = await sessionManager.encryptMessage(sessionId, { text: 'Hello!' });
const decrypted = await sessionManager.decryptMessage(sessionId, envelope);
```

### Policy

Control which agents can connect and what actions are allowed:

```typescript
import { Policy, Tier } from '@agentmesh/sdk';

// Predefined policies
const permissive = Policy.permissive(); // Accept all
const verified = Policy.verified();     // Require verified tier
const org = Policy.organization();      // Require organization tier

// Custom policy
const custom = new Policy({
  minimumTier: 'verified',
  minReputation: 0.7,
  allowedIntents: ['public/data', 'weather/*'],
  blockedIntents: ['admin/*'],
  blockedAmids: ['malicious-amid'],
  maxSessionTtl: 3600,
});
```

### Storage

Pluggable storage backends:

```typescript
import { MemoryStorage, FileStorage } from '@agentmesh/sdk';

// In-memory storage
const memory = new MemoryStorage();

// File-based storage
const file = new FileStorage('/path/to/data');
```

### Audit Logging

Track all agent activity:

```typescript
import { createAuditLogger } from '@agentmesh/sdk';

const logger = createAuditLogger(amid);

await logger.log('SESSION_INITIATED', 'INFO', 'Session started', {
  peerAmid: 'target-amid',
  sessionId: 'session-123',
});

// Query events
const errors = logger.getErrors();
const byPeer = logger.getByPeer('peer-amid');
const stats = logger.getStats();
```

## API Reference

### AgentMeshClient

Main client class for interacting with the AgentMesh network.

| Method | Description |
|--------|-------------|
| `initialize()` | Initialize the agent with identity and prekeys |
| `connect(amid, options)` | Establish session with another agent |
| `send(sessionId, message)` | Send encrypted message |
| `onMessage(handler)` | Register message handler |
| `getSession(sessionId)` | Get session info |
| `closeSession(sessionId)` | Close a session |
| `shutdown()` | Graceful shutdown |

### Identity

| Method | Description |
|--------|-------------|
| `Identity.generate()` | Generate new identity |
| `Identity.load(storage, key)` | Load from storage |
| `save(storage, key)` | Save to storage |
| `sign(data)` | Sign data with Ed25519 |
| `verify(data, signature, publicKey)` | Verify signature |

### SessionManager

| Method | Description |
|--------|-------------|
| `initiateSession(peerAmid, bundle, pubKey)` | Start new session |
| `acceptSession(peerAmid, x3dhMessage)` | Accept incoming session |
| `activateSession(sessionId, peerRatchetKey)` | Activate pending session |
| `encryptMessage(sessionId, plaintext)` | Encrypt message |
| `decryptMessage(sessionId, envelope)` | Decrypt message |
| `closeSession(sessionId)` | Close session |

### Policy

| Method | Description |
|--------|-------------|
| `Policy.permissive()` | Accept all connections |
| `Policy.verified()` | Require verified tier |
| `Policy.organization()` | Require organization tier |
| `evaluate(context)` | Evaluate KNOCK against policy |

## Project Structure

```
src/
├── identity.ts       # Identity management
├── storage/          # Storage backends
├── config.ts         # Configuration and Policy
├── encryption/       # X3DH and Double Ratchet
│   ├── x3dh.ts       # Extended Triple DH
│   ├── ratchet.ts    # Double Ratchet
│   ├── prekey.ts     # Prekey management
│   ├── session.ts    # Session management
│   └── hkdf.ts       # Key derivation
├── session/          # KNOCK protocol
├── schemas/          # Message validation
├── did/              # DID documents
├── dht/              # Distributed hash table
├── audit/            # Audit logging
├── transport/        # Transport layer
├── discovery/        # Agent discovery
├── certificates/     # Certificate handling
├── client.ts         # High-level client
└── index.ts          # Exports
```

## Security

- **Forward Secrecy**: Each message uses unique keys via Double Ratchet
- **Post-Compromise Security**: Key compromise doesn't affect past messages
- **Signature Verification**: All KNOCK messages are Ed25519 signed
- **Replay Protection**: Nonce-based replay attack prevention

## Testing

```bash
# Run all tests
npm test

# Run specific tests
npm test -- tests/unit/identity.test.ts
npm test -- tests/integration/encryption.test.ts
```

## Building

```bash
# Build for production
npm run build

# Output: dist/ with ESM, CJS, and type definitions
```

### 9. bytesToBase64 — stack overflow on large payloads
**File:** `dist/index.js` (DoubleRatchet and SessionManager classes)

`bytesToBase64()` used `String.fromCharCode(...bytes)` — the spread operator
passes every byte as a separate function argument. For payloads >100 KB (e.g.
handoff state transfer ~108 KB ciphertext after Signal Protocol encryption),
this exceeds V8's maximum call stack size.

**Fix:** Use `Buffer.from(bytes).toString('base64')` in Node.js environments,
fall back to a loop-based approach in browsers. Applied to both instances
(DoubleRatchet line 958 and SessionManager line 1365).

### 10. initiateSession — "Active session already exists" crash on reuse
**File:** `dist/index.js` (SessionManager.initiateSession, AgentMeshClient.establishSession)

When agent A sends a message to agent B, B's KNOCK response creates a session
in A's SessionManager. But `client.activeSessions` (the high-level Map) isn't
updated. A's next `send()` to B misses in `activeSessions` → calls
`establishSession` → calls `initiateSession` → finds the active session in the
crypto layer → throws `"Active session already exists with <amid>"`.

This breaks `mesh_transfer_file` and any second message to the same peer when
the session was established via incoming KNOCK rather than outgoing send.

**Fix:** `initiateSession` returns `{ sessionId, x3dhMessage: null, reused: true }`
for existing active sessions instead of throwing. `establishSession` detects
`reused: true`, syncs `activeSessions`, and returns early (skips redundant
KNOCK + activate).

### 11. `wsFactory` + `plaintextPeers` — extensibility hooks for heterogeneous peers
**Files:** `dist/chunk-WM5AX4U5.js` (`RelayTransport`), `dist/index.js`
(`AgentMeshClient`), `dist/index.d.ts`

Two additions needed to run the SDK inside the NemoClaw mesh-plugin:

1. **`TransportOptions.wsFactory?: (url) => WebSocket`** — `RelayTransport`
   previously did `new WebSocket(this.relayUrl)` directly, which in Node 22
   ignores `HTTPS_PROXY` (global fetch/undici quirk). `wsFactory` lets the
   caller inject a WebSocket built on top of a CONNECT-tunneled socket.
   Threaded through `AgentMeshClient` via
   `ClientOptions.transportOptions.wsFactory`.

2. **`ClientOptions.plaintextPeers?: string[]`** plus runtime methods
   `addPlaintextPeer` / `removePlaintextPeer` / `getPlaintextPeers`.
   AMIDs in this set bypass Signal E2E and use the legacy wire format
   (`encrypted_payload = base64(JSON.stringify(message))`). Needed for peers
   that have not yet adopted the SDK (e.g. the Rust controller). Both the
   send path and the `receive` handler check the set; plaintext peers skip
   KNOCK, X3DH, and SessionManager entirely.

Both hooks are purely additive — defaults match upstream behavior.

### 9. Relay reconnect: never give up + capped backoff

**File:** `src/transport.ts` (rebuilt into `dist/chunk-*.js` / `chunk-*.cjs`)

Upstream `RelayTransport` defaulted to `maxReconnectAttempts = 5` and uncapped
exponential delay. After the 5th failure the SDK silently logged
`Max reconnection attempts reached` and stayed mesh-deaf forever. In the
AzureClaw cluster this manifested as: registry searches kept working (HTTPS,
unaffected) but the relay had zero active WebSocket clients within minutes of
any transient blip (e.g. registry-not-yet-ready during pod boot, or a kubelet
network hiccup) — and stayed at zero until the pod restarted.

Patch:
- `maxReconnectAttempts` default → `Number.POSITIVE_INFINITY` (never quit).
- Reconnect delay capped at 60 s: `min(base * 2^n, 60_000)`.

Callers that want the old behavior can still override via `TransportOptions`.

## Patch #13 — Decrypt-fail clears local session + emits `session_desync` (May 2026)

**File:** `dist/index.js` (`AgentMeshClient.receive` handler, ~line 2986)

Upstream behavior: when `decryptMessage` throws inside an existing session
(ratchet desync — peer rotated keys, transport interleaved a stale message,
etc.), the SDK only emits `decrypt_failed` and **leaves the broken session
in `activeSessions`**. Every subsequent inbound message from that peer
fails decrypt the same way — and is silently dropped. The sender keeps
encrypting against a stale receiver state, so no replies ever arrive.

In the AzureClaw production demo (May 2026) this took down sibling-to-sibling
binary file transfers between sub-agents: chunked PNGs (4–5 chunks each) shipped
fine until the first decrypt failure, then every subsequent chunk in the
chunked-transfer protocol was rejected, leaving partial transfers in
`pendingTransfers` until TTL'd. The receiver never reassembled, never sent
`file_transfer_ack`, the sender retried the whole file 3× — same fate. Hero
image transfers failed; chart transfers (single-chunk → no decrypt session
overlap) succeeded.

Patch:

- On decrypt failure for an existing session, **delete** the entry from
  `activeSessions`, `pendingX3DH`, and `knockAcceptedPeers` (best-effort).
  Next `establishSession` to that peer will run a fresh X3DH + KNOCK round.
- Emit `session_desync` (was `decrypt_failed`) so callers can distinguish
  recoverable ratchet drift from genuine tampering or no-session drops.

The companion mesh-transport recovery layer in
`runtimes/openclaw/src/core/mesh-transport.ts` (gap-detection chunk
re-request + outbound retransmit cache) handles in-flight chunked transfers
that crossed the desync boundary.

## Patch #15 — KNOCK/encrypt race: keep X3DH bundle, idempotent responder (May 2026)

**Files:** `dist/index.js`
- `AgentMeshClient.send` encrypted branch (~line 3122)
- `AgentMeshClient.receive` `parsed.type === "encrypted" && parsed.x3dh` branch (~line 2952)

### Symptom
Cross-sandbox encrypted messages were silently rejected with `no_session — message REJECTED` after a fresh KNOCK had been accepted. Logs on the receiver showed a clean trust verification:

```
21:20:57.817 AGT KNOCK from azureclawtest (322PXauB2Nzs...)
21:20:57.820 ⛔ Message blocked from 322PXauB2Nzs: no accepted KNOCK session
21:20:57.874 AGT KNOCK trust OK: name-verified via registry, score=1000
21:22:11.603 AGT E2E no_session from azureclawtest: No encryption session established
```

### Root cause
A two-step race between KNOCK and the first encrypted message:

1. Parent calls `establishSession(toAmid)`:
   - Stores X3DH bundle in `pendingX3DH` (line 3169)
   - Sends KNOCK on the relay (line 3182)
   - Returns; caller's `send()` immediately encrypts and ships the first
     application message with the bundle attached (line 3126),
   - **Sender deletes `pendingX3DH` after that one send (line 3127).**

2. Receiver:
   - KNOCK and the first encrypted message arrive within a few ms.
   - The encrypted message can land **before** the KNOCK is processed — or
     while `verifyTrustedByName`'s registry lookup is still in flight.
   - At that moment `knockAcceptedPeers.has(fromAmid)` is `false`, so the
     pre-decrypt KNOCK gate at line ~2944 blocks the message and drops it.
   - Receiver therefore never calls `acceptSession()`, never builds the
     responder ratchet, and has no encryption session.

3. Subsequent sends:
   - Sender's `pendingX3DH` was burned in step 1 → no `x3dh` field on the
     wire → receiver falls through to the "session-required" branch
     (~line 2974) → emits `no_session` for every message forever.

This was masked for a long time by `AGT_TRUST_THRESHOLD=0` (the SDK
fail-open path skipped the KNOCK gate). Once threshold was restored to
500 (production default) the race became always-fatal in practice.

### Patch
- **Sender:** stop deleting `pendingX3DH` after the first attach. Every
  encrypted send now carries the bundle until the session is explicitly
  torn down (e.g. by patch #13's `session_desync` recovery). Cost is one
  ~150-byte serialized X3DH message per outbound message — negligible
  next to the AEAD ciphertext + ratchet header.
- **Receiver:** make the `encrypted+x3dh` branch idempotent. It now calls
  `acceptSession()` only when no session exists for `fromAmid`; otherwise
  it decrypts with the established session. This is required because the
  bundle is now re-attached on every message — without idempotence the
  receiver would reset the ratchet on every inbound message and corrupt
  any in-flight chunked transfer.

## Patch #16 — Pre-KNOCK message buffer (May 2026)

**File:** `dist/index.js` (`AgentMeshClient.connect` receive handler, ~line 2926)

### Symptom
Even with patch #15 in place, the very first encrypted message after a fresh
KNOCK was still being lost in the demo (request/response) pattern. Receiver
log:
```
21:31:19.551 ⛔ Message blocked from <parent>: no accepted KNOCK session
21:31:19.563 AGT KNOCK from <parent>
21:31:19.642 AGT KNOCK accepted
```
The encrypted message arrived ~12 ms before the KNOCK; the gate dropped it;
KNOCK was then accepted but no further messages arrived (the parent had
sent only one and was waiting for a reply). Patch #15's "re-attach X3DH on
every send" only helps when the sender produces a SECOND message — it
cannot recover a request/response flow where the first message is lost.

### Root cause
The relay does not guarantee inter-message ordering between distinct
`message_type`s. A sender that calls `establishSession()` (which fires KNOCK)
followed immediately by `send()` (which fires the encrypted payload) can
have the two messages arrive at the receiver in **either** order. The pre-
decrypt gate at line ~2944 drops anything that arrives before
`knockAcceptedPeers.has(fromAmid)`, with no recovery.

### Patch
- Refactor the `transport.onMessage("receive", ...)` callback into a named
  closure `processInbound(data)` so it can be re-entered.
- Add `this.preKnockBuffer = new Map()` keyed by `fromAmid`. When a peer's
  encrypted message arrives before we've accepted their KNOCK, the
  message's raw transport `data` is buffered (cap: 5 messages per peer) and
  scheduled for eviction after **3 s**.
- When KNOCK from the same peer is later accepted, the buffer is drained:
  each buffered `data` is fed back through `processInbound`, hitting the
  normal `encrypted+x3dh` branch (now idempotent per patch #15) and
  decrypting against the freshly built session.
- The drop event still fires (`knock_rejected` to `errorHandlers`) so any
  caller monitoring failures sees that the race occurred — operators can
  alert on it if it spikes — but the message itself is no longer lost.

### Why this is safe
- The buffer is per-peer, capped at 5 entries × 3 s, bounded memory.
- We only buffer messages that already passed transport-level validation
  (the relay envelope is signed; data structure was JSON-parsed).
- We do not bypass the KNOCK gate — buffered messages are only replayed
  AFTER `handleIncomingKnock` returns `accept: true`.
- AEAD authentication on the encrypted body still applies; tampered
  payloads still fail decrypt.
- No replay vulnerability: the buffer is in-memory only and cleared after
  drain or 3s; an attacker who wanted to replay a message would need to
  send it AFTER KNOCK acceptance, in which case the existing AEAD
  sequence/nonce checks on `decryptMessage` reject duplicates.

## License

MIT

## Patch #14 — Restore `messageBytes` assignment in KNOCK signing (May 2026)

**File:** `dist/index.js` (`KnockProtocol.createKnock`, `createAcceptResponse`,
`createRejectResponse`, ~lines 1658, 1735, 1755)

Three sites in upstream's published `dist/index.js` had the form

```js
new TextEncoder().encode(JSON.stringify(messageData));
const signature = await this.identity.sign(messageBytes);
```

— the encoded bytes are computed but **discarded**, and `messageBytes` is never
declared. At runtime every `createKnock` / `createAcceptResponse` /
`createRejectResponse` throws `ReferenceError: messageBytes is not defined`,
visible in caller logs as `KNOCK send to <amid> failed: messageBytes is not defined`.

This was masked for a long time by `AGT_TRUST_THRESHOLD=0`, which makes the
SDK fail-open on inbound peer messages without an accepted KNOCK session.
Once threshold was restored to 500 (production default) every KNOCK send
broke and **every cross-sandbox message was silently rejected** at the
receiver with `"no accepted KNOCK session"` — the parent agent could not
discover or talk to any sub-agent.

Patch: assign `const messageBytes = new TextEncoder().encode(...)` at all
three sites so the buffer is actually passed to `this.identity.sign(...)`.


## Patch #17 — `String.fromCharCode(...bytes)` stack overflow on large payloads (May 2026)

### Symptom
`mesh_transfer_file <name>.png (2.2MB) → <peer> FAILED — Maximum call stack size exceeded`

Reproduces on every transfer ≥ ~125 KB. Demos that ship a hero image, a
chart, or any non-trivial binary collapse: the receiver never gets the
artifact, the sender retries 3× with the same crash, the dependent agent
fabricates around the missing input.

### Root cause
SDK helpers in 5 sites use `String.fromCharCode(...bytes)` to build the
binary string fed to `btoa()`. The `...` spread expands every byte to a
function argument; V8 caps the argument count per call at ~125–250K
(implementation-defined). Our chunked transfer uses 512 KB chunks; even a
single ratchet-encrypted ciphertext easily exceeds the spread limit.

Affected code:

| File | Site |
|---|---|
| `src/encryption/ratchet.ts` | `DoubleRatchetSession.bytesToBase64` (private) + `serializeRatchetHeader` (`header.dhPublicKey`) |
| `src/encryption/session.ts` | `EncryptedSession.bytesToBase64` (private) — hot path for KNOCK + chunk ciphertexts |
| `src/encryption/prekey.ts` | `serializePrekeyBundle` + `serializeState` local `toBase64` |
| `src/encryption/x3dh.ts` | `serializeX3DHMessage` local `toBase64` |
| `src/dht/index.ts` | DHT capability entry signature inline |

### Patch
Replace each helper with a Node-Buffer fast path + browser fallback that
chunks the byte array by 32 KB before spreading:

```ts
private bytesToBase64(bytes: Uint8Array): string {
  if (typeof Buffer !== "undefined") return Buffer.from(bytes).toString("base64");
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK) as unknown as number[]);
  }
  return btoa(binary);
}
```

For inline `String.fromCharCode(...x)` sites (ratchet header serialization,
DHT signature, prekey/X3DH local helpers), the same pattern is inlined.

### Dist patch
`dist/index.{js,cjs}` are hand-edited (per repo convention: never run
`npm run build` on the vendored SDK — it regenerates dist and obliterates
patches). A small helper `__bmsdkB64Bin(bytes)` is injected near the top
of each file; all 9 `String.fromCharCode(...X)` call sites are replaced
with `__bmsdkB64Bin(X)`. The helper returns a binary string (the same
shape `String.fromCharCode(...bytes)` produced) so the surrounding
`btoa(binary)` call still works unchanged.

### Why this is safe
- Node.js `Buffer.from(bytes).toString("base64")` is the same encoding
  produced by `btoa(String.fromCharCode(...bytes))` for any Uint8Array.
- The browser fallback chunks at 32 KB — well under any documented JS
  engine arg-count limit — and produces the identical binary string the
  spread version produced before crashing.
- Only path-of-execution change is internal; wire format is unchanged.

## Patch #18 — Ratchet desync self-heal + per-peer inbound mutex (May 2026)

### Symptom
After any local Double Ratchet desync (chunked-transfer interleave, peer
restart, or a single dropped frame), the writer side enters a permanent
"KNOCK-deaf" loop: every subsequent inbound message from the desynced
peer is rejected with `decrypt_failed` or `no_session`, and the runtime
trust-handler bleeds -0.5 per event until the peer drops below threshold
and gets blocked. The analyst→viz→writer chunked file transfer never
completes.

### Root cause chain
1. Peer A's outbound `establishSession()` re-attaches the SAME X3DH bundle
   on every send (patch #15 keeps it for the KNOCK race).
2. Peer B's patch #15 idempotency: on `encrypted+x3dh` with an existing
   `sessionId`, skip `acceptSession` and decrypt with the stale session.
3. After a desync, B's stale session can no longer decrypt the new
   payload. With the old patch #15 code, B emits `decrypt_failed` and
   leaves state untouched — every following message fails the same way.
4. Meanwhile patch #11 (peer-side desync teardown) clears
   `knockAcceptedPeers`, putting B's KNOCK gate in front of any future
   recovery message. Death-spiral.

### Patch (Patch #18)

Replaces the encrypted+x3dh branch and the patch #11 teardown.

**A. X3DH fingerprint-aware self-heal** (`dist/index.js` ~3005-3120,
`dist/index.cjs` mirror).

- Track `acceptedX3dhFingerprints: Map<string, string>` per client. The
  fingerprint is `parsed.x3dh` itself (deterministic per session).
- First contact (no `sessionId`): `acceptSession` → store fingerprint →
  decrypt. Unchanged.
- Existing session, decrypt OK: store fingerprint, deliver. Unchanged.
- Existing session, decrypt FAILS:
  - If `parsed.x3dh` fingerprint == last-accepted fingerprint → this is
    patch #15's re-attached bundle, NOT a fresh offer. Genuine ratchet
    desync. Tear down: `activeSessions.delete`, `sessionCache.clearByAmid`,
    `pendingX3DH.delete`, close all `protocolSessions` for the peer,
    `sessionManager.closeSession(sessionId)`. Emit `session_desync` so
    the runtime suppresses trust deltas (Patch #18 trust-handler change).
  - If fingerprint NEW → transactional rebuild:
    1. `candidateSid = await acceptSession(parsed.x3dh)`
    2. `decrypted = await decryptMessage(candidateSid, parsed)`
    3. On candidate **failure** (e.g. one-time prekey already consumed
       at the registry): `closeSession(candidateSid)`, leave old session
       intact, emit `decrypt_failed`.
    4. On candidate **success**: swap `activeSessions.set(candidateSid)`,
       store new fingerprint, `sessionCache.clearByAmid` (so `send()`
       can't resurrect the old session), `closeSession(oldSid)`. Log
       "Patch #18: rebuilt session for {peer} via fresh X3DH (desync
       recovered)".

**B. Patch #11 hardening** (`dist/index.js` ~3160).

The same teardown path now also clears `sessionCache`,
`acceptedX3dhFingerprints`, and closes all `protocolSessions` for the
peer + the local `sessionId` (was previously leaving them dangling).
Still does NOT clear `knockAcceptedPeers` — KNOCK trust is identity-
level and persists across session churn (clearing it was the original
bug that turned a one-message hiccup into a permanent block).

**C. Per-peer inbound mutex** (`dist/index.js` ~2950, ~3231).

Wraps `processInbound` in a `Map<peerAmid, Promise<void>>` chain. Two
concurrent inbound messages from the same peer (common during chunked
transfer) used to race the rebuild path: both read stale
`activeSessions`, both `acceptSession()` against the same X3DH bundle,
the registry consumes the one-time prekey for the first, the second
fails. The mutex serializes per-peer state mutations. Map entries are
self-cleaned via `next.finally`.

**D. Policy-aware KNOCK re-add** (`dist/index.js` ~3105).

Successful x3dh decryption only re-adds to `knockAcceptedPeers` if the
peer was previously accepted (has a stored fingerprint history) AND is
not currently `isBlocked()`. Successful crypto must not bypass an
explicit operator block.

**E. Trust handler classification** (runtime side,
`runtimes/openclaw/src/index.ts:643-700`).

Reclassifies `session_desync`, `no_session`, and `decrypt_failed` as
PROTOCOL events: surface a single advisory inbox entry, do NOT call
`pushTrustToRouter(name, -0.5)`. Genuine security events
(signature/tamper failures via `knock_rejected` and other types) keep
the -0.5 trust penalty.

### Why this is safe
- **Replay attacks:** rebuild only triggers on a NEW fingerprint, never
  on a repeated patch #15 bundle. Same-fingerprint desync tears down
  state but never accepts the message — no ratchet poisoning.
- **State consistency:** transactional rebuild guarantees we never
  delete a working session before proving the candidate works.
- **Forward secrecy:** `closeSession(oldSid)` + `sessionCache.clearByAmid`
  ensure no stale ratchet keys remain after a successful rebuild.
- **Concurrency:** per-peer mutex makes acceptSession+decrypt atomic
  per peer; OTP can't be consumed twice.
- **Policy:** `isBlocked()` check on KNOCK re-add prevents crypto
  success from bypassing an operator block.

### Files modified
- `vendor/agentmesh-sdk/dist/index.js` — full implementation (encrypted+x3dh
  branch, patch #11 block, per-peer mutex wrapper).
- `vendor/agentmesh-sdk/dist/index.cjs` — parity (no per-peer mutex; cjs
  is not the runtime path).
- `runtimes/openclaw/src/index.ts:643-700` — trust handler reclassification.

### Compatibility
- Wire format unchanged. Patch is purely receiver-side state-machine
  hardening. A patched receiver works against any (patched or unpatched)
  sender. The only requirement for full self-heal is patch #15 on the
  sender side, which we ship in the same vendor bundle.
