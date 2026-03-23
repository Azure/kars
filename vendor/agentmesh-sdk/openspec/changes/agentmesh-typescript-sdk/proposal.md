## Why

The AgentMesh TypeScript SDK audit revealed 11 missing features and 24 partial implementations that prevent production readiness. The SDK is currently a well-implemented client library at 70% completion, but critical gaps in store-and-forward handling, session management, circuit breakers, and observability must be addressed before launch.

## What Changes

### Critical Fixes
- Implement store-and-forward message processing (receive `pending_messages` on connect but don't process them)
- Add session caching with auto-cache-hit for same initiator/receiver/intent patterns
- Implement optimistic send for allowlist contacts (KNOCK + message in one packet)
- Expose circuit breaker controls (kill_session, pause_new, shutdown, block, emergency_stop)

### High Priority
- Add owner dashboard foundation (localhost:7777 interface for session/policy management)
- Implement transcript encryption at rest using owner's key
- Add policy file loading from `~/.agentmesh/policy.json`
- Implement proper session key storage at `~/.agentmesh/sessions/`
- Implement proper key storage at `~/.agentmesh/keys/`

### Medium Priority
- Add client-side rate limiting enforcement (not just server-side)
- Implement session caching with automatic KNOCK skip for cached sessions
- Complete DID resolution with proper registry integration

### Testing
- Complete cross-SDK test vectors (Python <-> TypeScript compatibility)
- Add integration tests against live registry/relay servers

### Documentation
- Create OpenClaw skill manifest (`skill.json`)
- Document background task patterns (relay_connection_keepalive, knock_listener)

## Capabilities

### New Capabilities
- `store-and-forward`: Handle pending messages received on relay connect, process queued messages from offline periods
- `session-cache`: Automatic session caching with cache-hit detection, skip KNOCK for cached sessions
- `optimistic-send`: Send KNOCK + initial message in single packet for allowlist contacts
- `circuit-breakers`: Emergency controls including kill_session, pause_new, shutdown, block, emergency_stop
- `owner-dashboard`: Local web interface at localhost:7777 for session monitoring and policy management
- `transcript-encryption`: Encrypt audit logs and transcripts at rest using owner's encryption key
- `file-config`: Load policy and keys from standard paths (~/.agentmesh/policy.json, keys/, sessions/)
- `client-rate-limiting`: Client-side rate limiting enforcement before server round-trip
- `cross-sdk-testing`: Test vectors and compatibility tests for Python <-> TypeScript interop

### Modified Capabilities
- None (all existing specs remain unchanged, these are additions)

## Impact

### Code Changes
- `src/transport.ts` - Store-and-forward message handling
- `src/session/index.ts` - Session caching, optimistic send
- `src/client.ts` - Circuit breakers, rate limiting, file config loading
- `src/audit/index.ts` - Transcript encryption at rest
- `src/config.ts` - Policy file loading
- `src/storage/index.ts` - Standard path utilities
- New `src/dashboard/` module - Owner dashboard server
- `tests/cross-sdk/` - Complete test vector implementation

### APIs
- New `client.killSession()`, `client.pauseNew()`, `client.shutdown()`, `client.block()`, `client.emergencyStop()` methods
- New `client.loadPolicy(path)` method
- New `Dashboard` class with `start()`, `stop()` methods

### Dependencies
- May add `express` or similar for dashboard HTTP server
- May add file-watching for policy hot-reload

### Systems
- Local filesystem access for ~/.agentmesh/ directory structure
- Localhost:7777 port for dashboard (configurable)
