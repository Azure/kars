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

## Build

```sh
npm ci && npm run build
```

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

## License

MIT
