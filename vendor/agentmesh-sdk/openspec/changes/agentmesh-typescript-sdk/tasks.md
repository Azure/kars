## 1. Store-and-Forward

- [x] 1.1 Add `pending_messages` handling in RelayTransport CONNECTED response
- [x] 1.2 Implement `requestPendingMessages()` method in RelayTransport
- [x] 1.3 Add message processing loop for pending messages (FIFO order)
- [ ] 1.4 Integrate pending message decryption with SessionManager
- [x] 1.5 Add `pending_processed` event emission
- [x] 1.6 Implement ACK/NACK sending for processed messages
- [x] 1.7 Add `pending_failed` event for decryption/handler failures
- [x] 1.8 Write unit tests for store-and-forward handling
- [ ] 1.9 Write integration test for pending message flow

## 2. Session Cache

- [x] 2.1 Create `SessionCache` class with composite key (amid:amid:intentHash)
- [x] 2.2 Implement LRU eviction with configurable maxCachedSessions
- [x] 2.3 Add TTL tracking and sliding window extension on use
- [ ] 2.4 Integrate cache-hit detection in AgentMeshClient.send()
- [ ] 2.5 Skip KNOCK protocol when cache hit detected
- [x] 2.6 Add cache statistics (hits, misses, evictions)
- [x] 2.7 Implement `clearCachedSession(amid, intent)` method
- [x] 2.8 Implement `clearAllCachedSessions()` method
- [x] 2.9 Add `cache_cleared` event emission
- [x] 2.10 Write unit tests for SessionCache
- [ ] 2.11 Write integration test for cache-hit KNOCK skip

## 3. Optimistic Send

- [x] 3.1 Add `knock_with_message` message type to RelayMessage
- [x] 3.2 Implement allowlist check before send in AgentMeshClient
- [x] 3.3 Create combined packet builder for KNOCK + message
- [x] 3.4 Add `optimisticSend` configuration option
- [x] 3.5 Add `forceOptimistic` per-send option
- [x] 3.6 Implement `optimistic_dropped` event for rejections
- [ ] 3.7 Write unit tests for optimistic send logic
- [ ] 3.8 Write integration test with mock relay

## 4. Circuit Breakers

- [x] 4.1 Add `CircuitState` enum (RUNNING, PAUSED, STOPPED)
- [x] 4.2 Implement `killSession(amid)` method in AgentMeshClient
- [x] 4.3 Implement `pauseNew()` method with state transition
- [x] 4.4 Implement `resumeNew()` method with state transition
- [x] 4.5 Implement `block(amid)` method (add to blocklist + kill session)
- [x] 4.6 Implement `unblock(amid)` method
- [x] 4.7 Implement `emergencyStop()` terminal state
- [ ] 4.8 Add KNOCK rejection for PAUSED state with "agent_paused" reason
- [x] 4.9 Add `getCircuitState()` method
- [x] 4.10 Include circuitState in getInfo() response
- [x] 4.11 Emit circuit breaker events (session_killed, peer_blocked, emergency_stop)
- [x] 4.12 Write unit tests for all circuit breakers
- [ ] 4.13 Write integration test for pause/resume flow

## 5. Owner Dashboard

- [x] 5.1 Create `src/dashboard/` module structure
- [x] 5.2 Implement Dashboard class with start/stop methods
- [x] 5.3 Bind HTTP server to 127.0.0.1 only
- [x] 5.4 Implement GET /status endpoint
- [x] 5.5 Implement GET /sessions endpoint
- [x] 5.6 Implement POST /sessions/:amid/kill endpoint
- [x] 5.7 Implement GET /policy endpoint
- [x] 5.8 Implement POST /policy endpoint with validation
- [x] 5.9 Implement POST /circuit/pause endpoint
- [x] 5.10 Implement POST /circuit/resume endpoint
- [x] 5.11 Implement POST /circuit/emergency-stop endpoint
- [x] 5.12 Add optional X-API-Key authentication
- [x] 5.13 Add CORS support for localhost origins
- [x] 5.14 Handle port-in-use error gracefully
- [x] 5.15 Emit dashboard_started and dashboard_stopped events
- [x] 5.16 Write unit tests for all endpoints
- [ ] 5.17 Write integration test for dashboard lifecycle

## 6. Transcript Encryption

- [x] 6.1 Add HKDF key derivation for audit encryption key
- [x] 6.2 Implement XChaCha20-Poly1305 encryption (or fallback to AES-GCM)
- [x] 6.3 Update AuditLogger to encrypt entries before storage
- [x] 6.4 Store entries as { nonce: base64, ciphertext: base64 }
- [x] 6.5 Implement `decryptEntry(encryptedEntry, identity)` method
- [x] 6.6 Implement `exportAuditLog(identity, options)` with filtering
- [x] 6.7 Add `encrypted` configuration option (default: true)
- [x] 6.8 Log warning when encryption disabled
- [x] 6.9 Implement `reencryptTranscripts(oldIdentity, newIdentity)` for key rotation
- [x] 6.10 Add integrity verification on decryption
- [x] 6.11 Emit reencrypt_progress events
- [ ] 6.12 Write unit tests for encryption/decryption
- [ ] 6.13 Write integration test for full audit lifecycle

## 7. File Configuration

- [x] 7.1 Create `FileConfigLoader` class
- [x] 7.2 Implement default path resolution (~/.agentmesh/ or AGENTMESH_HOME)
- [x] 7.3 Create directory structure on first use with proper permissions
- [x] 7.4 Implement `loadPolicy(path?)` from policy.json
- [x] 7.5 Handle missing policy.json gracefully (use default)
- [x] 7.6 Validate policy file content and throw ConfigError if invalid
- [x] 7.7 Implement policy file watching with watchPolicy option
- [x] 7.8 Emit policy_reloaded and policy_reload_failed events
- [ ] 7.9 Update FileStorage to use standard paths by default
- [x] 7.10 Set file permissions (0600 for keys, 0700 for directories)
- [x] 7.11 Implement session state persistence to sessions directory
- [x] 7.12 Implement session restoration on startup
- [x] 7.13 Implement expired session cleanup
- [x] 7.14 Add useFileStorage: false option for containers
- [x] 7.15 Add gracefulFallback for read-only filesystems
- [x] 7.16 Write unit tests for FileConfigLoader
- [ ] 7.17 Write integration test for policy hot reload

## 8. Client-Side Rate Limiting

- [x] 8.1 Create `RateLimiter` class with token bucket algorithm
- [x] 8.2 Implement global rate limit configuration
- [x] 8.3 Implement per-peer rate limit buckets
- [x] 8.4 Add rate limit check in AgentMeshClient.send()
- [x] 8.5 Throw RateLimitError with retryAfter on limit exceeded
- [x] 8.6 Bypass rate limits for KNOCK and CLOSE messages
- [x] 8.7 Implement `getRateLimitStatus()` method
- [x] 8.8 Emit rate_limited events
- [x] 8.9 Implement token refill over time
- [x] 8.10 Add waitForCapacity option with maxWait timeout
- [x] 8.11 Implement fair queuing across peers
- [x] 8.12 Write unit tests for RateLimiter
- [ ] 8.13 Write integration test for rate limiting behavior

## 9. Cross-SDK Testing

- [ ] 9.1 Generate AMID test vectors from Python SDK
- [ ] 9.2 Populate test-vectors.ts with all primitive test data
- [ ] 9.3 Complete AMID compatibility test implementation
- [ ] 9.4 Complete X3DH shared secret compatibility test
- [ ] 9.5 Complete prekey bundle format compatibility test
- [ ] 9.6 Complete Double Ratchet encryption/decryption test
- [ ] 9.7 Complete HKDF output compatibility test
- [ ] 9.8 Complete Ed25519 signature verification test
- [ ] 9.9 Complete AES-GCM compatibility test
- [ ] 9.10 Create Python test vector generation script
- [ ] 9.11 Write KNOCK protocol exchange test
- [ ] 9.12 Write full end-to-end message exchange test
- [ ] 9.13 Document test vector update process

## 10. Integration and Polish

- [x] 10.1 Update AgentMeshClient constructor to integrate all new features
- [x] 10.2 Update ClientOptions interface with all new configuration
- [x] 10.3 Update ClientInfo interface with new status fields
- [x] 10.4 Add comprehensive error types (RateLimitError, DashboardError, ConfigError, etc.)
- [x] 10.5 Update src/index.ts exports for all new modules
- [ ] 10.6 Update README.md with new features and configuration
- [ ] 10.7 Add examples for circuit breakers, dashboard, file config
- [x] 10.8 Run full test suite and ensure 100% pass rate
- [x] 10.9 Update AUDIT_REPORT.md with resolved items
