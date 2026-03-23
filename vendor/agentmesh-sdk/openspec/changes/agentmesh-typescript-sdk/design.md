## Context

The AgentMesh TypeScript SDK (`@agentmesh/sdk`) is a client library at 70% completion with 279 passing tests. The cryptographic core (Ed25519, X25519, X3DH, Double Ratchet) and KNOCK protocol are solid. However, the audit identified critical gaps:

**Current State:**
- SDK is client-only (registry/relay servers are separate infrastructure)
- Store-and-forward: receives `pending_messages` count but doesn't process queued messages
- P2P transport: stub that always returns `isAvailable: false`
- No circuit breakers, owner dashboard, or transcript encryption
- Policy and keys use memory/storage abstraction, not standard file paths

**Constraints:**
- Must work in Node.js and Cloudflare Workers (MoltWorker)
- Web Crypto API only (no native crypto dependencies)
- Existing 279 tests must continue passing
- Backwards-compatible with current client API

## Goals / Non-Goals

**Goals:**
- Complete all MISSING items from audit (11 items)
- Upgrade all PARTIAL items to PASS (24 items)
- Maintain 100% test pass rate
- Production-ready SDK for agent-to-agent communication

**Non-Goals:**
- Registry server implementation (separate repo)
- Relay server implementation (separate repo)
- Full P2P ICE/STUN/TURN (defer to future, keep stub)
- PostgreSQL schema (server-side concern)
- OpenClaw skill manifest (separate packaging concern)

## Decisions

### 1. Store-and-Forward Processing

**Decision:** Process pending messages in `RelayTransport.connect()` after receiving CONNECTED response.

**Rationale:** The relay already sends `pending_messages` count on connect. We add:
1. Request pending messages after CONNECTED
2. Process each with existing message handlers
3. Emit `pending_processed` event when done

**Alternatives Considered:**
- Background polling: Rejected (wastes bandwidth, relay already pushes)
- Lazy processing: Rejected (messages should be delivered immediately)

### 2. Session Caching Strategy

**Decision:** Cache sessions by composite key `${initiatorAmid}:${receiverAmid}:${intentHash}`.

**Rationale:** Same initiator sending same intent to same receiver should reuse the encrypted session. Cache includes:
- Session ID
- Ratchet state
- TTL expiration
- Usage count for rate limiting

**Alternatives Considered:**
- Cache by AMID only: Rejected (different intents may need different sessions)
- No caching: Rejected (audit requires it, reduces KNOCK overhead)

### 3. Optimistic Send Implementation

**Decision:** For allowlist contacts, send KNOCK + first message in single WebSocket frame.

**Rationale:** Allowlist contacts are pre-approved, so KNOCK will be accepted. Sending both saves one round-trip.

**Implementation:**
1. Check if recipient is in allowlist
2. If yes: send `{ type: 'knock_with_message', knock: {...}, message: {...} }`
3. Relay delivers KNOCK, waits for ACCEPT, then delivers message
4. If rejected, message is dropped (acceptable for allowlist)

**Alternatives Considered:**
- Client-side wait: Rejected (defeats purpose of optimization)
- Always optimistic: Rejected (non-allowlist may reject, wasting message)

### 4. Circuit Breaker Architecture

**Decision:** Add circuit breaker methods to `AgentMeshClient` that emit events and update state.

**Circuit Breakers:**
| Method | Effect |
|--------|--------|
| `killSession(amid)` | Immediately terminate session with peer |
| `pauseNew()` | Reject all new KNOCK requests |
| `resumeNew()` | Resume accepting KNOCK requests |
| `block(amid)` | Add to blocklist, kill session |
| `emergencyStop()` | Disconnect, reject all, clear sessions |

**State Machine:**
```
RUNNING → pauseNew() → PAUSED → resumeNew() → RUNNING
RUNNING → emergencyStop() → STOPPED (terminal)
```

### 5. Owner Dashboard Design

**Decision:** Minimal HTTP server on localhost:7777 with JSON API and optional static UI.

**API Endpoints:**
- `GET /status` - Client info, session count, connection state
- `GET /sessions` - List active sessions
- `POST /sessions/:amid/kill` - Kill specific session
- `GET /policy` - Current policy
- `POST /policy` - Update policy
- `POST /circuit/:action` - Trigger circuit breaker

**Rationale:** JSON API enables CLI tools and custom UIs. Static UI is optional.

**Alternatives Considered:**
- WebSocket only: Rejected (harder to integrate with curl/scripts)
- Electron app: Rejected (too heavy, SDK should be lightweight)

### 6. Transcript Encryption

**Decision:** Encrypt audit log entries with owner's X25519 key using XChaCha20-Poly1305.

**Implementation:**
1. Generate encryption key from owner's X25519 private key via HKDF
2. Each log entry encrypted with random nonce
3. Store as `{ nonce: base64, ciphertext: base64 }` in JSONL
4. Decrypt on demand for dashboard/export

**Key Derivation:**
```
audit_key = HKDF(owner_x25519_private, salt="agentmesh-audit", info="v1")
```

**Alternatives Considered:**
- Ed25519 for encryption: Rejected (signing keys shouldn't encrypt)
- Separate encryption key: Rejected (adds key management complexity)

### 7. File Configuration Paths

**Decision:** Support standard paths with fallback to storage abstraction.

**Standard Paths:**
- `~/.agentmesh/keys/` - Identity keys
- `~/.agentmesh/sessions/` - Session state
- `~/.agentmesh/policy.json` - Policy configuration
- `~/.agentmesh/audit/` - Encrypted audit logs

**Implementation:**
1. New `FileConfigLoader` class
2. `loadPolicy()` reads from standard path, falls back to storage
3. `FileStorage` uses standard paths by default
4. Environment variable `AGENTMESH_HOME` overrides `~/.agentmesh`

### 8. Client-Side Rate Limiting

**Decision:** Token bucket rate limiter in `AgentMeshClient`.

**Configuration:**
```typescript
{
  rateLimit: {
    maxPerSecond: 10,
    maxBurst: 50,
    perPeer: { maxPerSecond: 5 }
  }
}
```

**Behavior:**
- Check bucket before `send()`
- Throw `RateLimitError` if exceeded
- Separate buckets per peer for fairness

## Risks / Trade-offs

**[Risk] Dashboard port conflict** → Make port configurable, default 7777, fail gracefully if in use

**[Risk] Transcript encryption key lost** → Document backup procedures, consider key escrow option

**[Risk] Session cache memory growth** → Implement LRU eviction, max 1000 cached sessions

**[Risk] Optimistic send message loss** → Only for allowlist, document behavior, add delivery confirmation option

**[Risk] Circuit breaker state persistence** → Store in memory only (deliberate), document restart clears state

**[Trade-off] Dashboard adds dependency** → Keep minimal (native http module), optional feature

**[Trade-off] File paths non-portable** → Support environment override, document for containers
