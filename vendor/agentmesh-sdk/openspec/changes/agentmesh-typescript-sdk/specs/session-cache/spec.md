## ADDED Requirements

### Requirement: Cache sessions by composite key
The SDK SHALL cache established sessions using the composite key: initiatorAmid + receiverAmid + intentHash.

#### Scenario: Session cached after establishment
- **WHEN** session is established between agent A and agent B with intent "weather/query"
- **THEN** SDK SHALL cache session with key "A:B:hash(weather/query)"
- **AND** session SHALL remain cached until TTL expires or manually cleared

#### Scenario: Different intents create different sessions
- **WHEN** agent A sends to agent B with intent "weather/query"
- **AND** agent A sends to agent B with intent "calendar/book"
- **THEN** SDK SHALL maintain two separate cached sessions

### Requirement: Automatic cache hit detection
The SDK SHALL automatically reuse cached sessions when the same initiator/receiver/intent combination is requested.

#### Scenario: Reuse cached session
- **WHEN** agent A sends to agent B with intent "weather/query"
- **AND** a cached session exists for A:B:hash(weather/query)
- **AND** session has not expired
- **THEN** SDK SHALL skip KNOCK protocol
- **AND** send message using cached session directly

#### Scenario: Cache miss triggers KNOCK
- **WHEN** agent A sends to agent B with intent "weather/query"
- **AND** no cached session exists for this combination
- **THEN** SDK SHALL initiate full KNOCK protocol
- **AND** cache the resulting session

### Requirement: Session cache TTL
Cached sessions SHALL expire based on their configured TTL.

#### Scenario: Expired session removed from cache
- **WHEN** cached session TTL expires
- **THEN** SDK SHALL remove session from cache
- **AND** next send SHALL trigger new KNOCK

#### Scenario: Session usage resets TTL
- **WHEN** cached session is used for message send
- **THEN** session TTL SHALL be extended by configured slidingWindow duration

### Requirement: Session cache limits
The SDK SHALL enforce cache size limits to prevent memory exhaustion.

#### Scenario: Cache eviction on limit
- **WHEN** session cache reaches maxCachedSessions limit
- **AND** new session needs to be cached
- **THEN** SDK SHALL evict least recently used session
- **AND** cache new session

#### Scenario: Cache statistics available
- **WHEN** application requests cache statistics
- **THEN** SDK SHALL return hit count, miss count, eviction count, current size

### Requirement: Manual cache control
The SDK SHALL provide methods to manually manage the session cache.

#### Scenario: Clear specific cached session
- **WHEN** application calls clearCachedSession(amid, intent)
- **THEN** SDK SHALL remove matching session from cache

#### Scenario: Clear all cached sessions
- **WHEN** application calls clearAllCachedSessions()
- **THEN** SDK SHALL remove all sessions from cache
- **AND** emit 'cache_cleared' event
