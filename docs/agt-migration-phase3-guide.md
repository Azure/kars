# AGT Migration — Phase 3 Implementation Guide

## What's done (Phases 1-2)
- `transport-interface.ts` — IMeshTransport abstraction
- `agt-transport.ts` — AGT-backed implementation
- `agt-identity.ts` — AGT-compatible identity (did:agentmesh)

## Phase 3: Swap connection.ts

### Step 1: Update package.json
```diff
- "@agentmesh/sdk": "file:../vendor/agentmesh-sdk",
+ "@microsoft/agentmesh-sdk": "^3.2.0",
```

### Step 2: Update identity.ts imports
Replace all `import { Identity, type IdentityData } from "@agentmesh/sdk"` with:
```typescript
import { loadOrCreateIdentity, type AgtMeshIdentity } from "./agt-identity.js";
```

### Step 3: Refactor MeshConnection to use IMeshTransport
The key change: MeshConnection's constructor takes an `IMeshTransport` instead of creating an `AgentMeshClient` internally.

```typescript
// Before
constructor(config: ConnectionConfig) {
  // creates AgentMeshClient internally
}

// After
constructor(config: ConnectionConfig, transport?: IMeshTransport) {
  this.transport = transport ?? new AgtTransport({
    relayUrl: config.relayUrl,
    registryUrl: config.registryUrl,
    identity: config.identity,
    wsFactory: config.wsFactory,
    plaintextPeers: config.plaintextPeers,
  });
}
```

### Step 4: Replace SDK calls in connection.ts

| Current (vendored @agentmesh/sdk) | New (AGT via IMeshTransport) |
|----|----|
| `client.connect()` | `transport.connect()` |
| `client.send(amid, payload)` | `transport.send(amid, payload)` |
| `client.onMessage(handler)` | `transport.onMessage(handler)` |
| `client.addPlaintextPeer(amid)` | `transport.addPlaintextPeer(amid)` |
| `client.disconnect()` | `transport.disconnect()` |
| `Identity.generate()` | `generateIdentity()` from agt-identity |
| `Identity.fromData(data)` | `loadIdentity()` from agt-identity |

### Step 5: Keep app-layer code unchanged
These stay in MeshConnection (they don't touch the SDK):
- Chunking logic (CHUNK_THRESHOLD, reassembly)
- File transfer protocol (file_transfer + ack dance)
- Inbox/waiter management (getInbox, drainInbox, consumeInbox)
- mesh:ping / mesh:pong
- Discovery fan-out

### Step 6: Update cli/src/plugin.ts
Replace `import("@agentmesh/sdk")` with `import("@microsoft/agentmesh-sdk")`.
The policy evaluation path already uses AGT governance — just the import path changes.

### Step 7: Update deploy/agentmesh.yaml
Replace vendored relay/registry containers with AGT services:
```yaml
# Before: custom relay + registry images
# After:
- image: agentmesh/governance-sidecar:3.1.1  # includes registry
- python -m agentmesh.relay  # relay service
- python -m agentmesh.registry  # registry service
```

### Step 8: Remove vendor/
```bash
git rm -r vendor/agentmesh-sdk vendor/agentmesh-relay vendor/agentmesh-registry
```

## Testing checklist
- [ ] Identity generation (did:agentmesh format)
- [ ] Connect to relay
- [ ] Send/receive encrypted messages (parent <-> sub-agent)
- [ ] Plaintext peer communication (Rust controller)
- [ ] KNOCK handshake + policy evaluation
- [ ] File transfer
- [ ] Offline message delivery (store-and-forward)
- [ ] Heartbeat/presence
- [ ] Discovery
- [ ] Reconnect after disconnect
