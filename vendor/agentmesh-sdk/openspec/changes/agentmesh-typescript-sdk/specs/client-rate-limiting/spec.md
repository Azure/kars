## ADDED Requirements

### Requirement: Token bucket rate limiter
The SDK SHALL implement client-side rate limiting using a token bucket algorithm.

#### Scenario: Allow requests within rate limit
- **WHEN** client sends messages within configured rate
- **THEN** messages SHALL be sent immediately
- **AND** no rate limit error

#### Scenario: Reject requests exceeding rate limit
- **WHEN** client attempts to send message
- **AND** token bucket is empty
- **THEN** SDK SHALL throw RateLimitError
- **AND** error SHALL include retryAfter in milliseconds

### Requirement: Global rate limit configuration
The SDK SHALL support global rate limit configuration.

#### Scenario: Configure global rate limit
- **WHEN** client is configured with rateLimit: { maxPerSecond: 10, maxBurst: 50 }
- **THEN** SDK SHALL allow max 10 messages per second average
- **AND** allow burst of up to 50 messages

#### Scenario: Default rate limits
- **WHEN** client is created without rate limit config
- **THEN** SDK SHALL use defaults: maxPerSecond: 100, maxBurst: 500

### Requirement: Per-peer rate limiting
The SDK SHALL support separate rate limits per peer.

#### Scenario: Configure per-peer rate limit
- **WHEN** client is configured with perPeer: { maxPerSecond: 5 }
- **THEN** SDK SHALL maintain separate token bucket per peer AMID
- **AND** each peer limited to 5 messages per second

#### Scenario: Per-peer and global limits combined
- **WHEN** both global and per-peer limits are configured
- **THEN** message must satisfy BOTH limits to be sent
- **AND** either limit can block the message

### Requirement: Rate limit bypass for system messages
The SDK SHALL allow system messages to bypass rate limits.

#### Scenario: KNOCK not rate limited
- **WHEN** SDK sends KNOCK message
- **THEN** KNOCK SHALL NOT consume rate limit tokens
- **AND** KNOCK messages are always allowed

#### Scenario: CLOSE not rate limited
- **WHEN** SDK sends CLOSE message
- **THEN** CLOSE SHALL NOT consume rate limit tokens

### Requirement: Rate limit statistics
The SDK SHALL expose rate limit statistics.

#### Scenario: Query rate limit status
- **WHEN** application calls getRateLimitStatus()
- **THEN** SDK SHALL return current token count, max tokens, refill rate
- **AND** include per-peer status if configured

#### Scenario: Rate limit events
- **WHEN** message is rate limited
- **THEN** SDK SHALL emit 'rate_limited' event
- **AND** event SHALL include peer AMID and retry time

### Requirement: Rate limit recovery
The SDK SHALL handle rate limit recovery gracefully.

#### Scenario: Tokens refill over time
- **WHEN** token bucket is depleted
- **AND** time passes
- **THEN** tokens SHALL refill at configured rate
- **AND** subsequent requests may succeed

#### Scenario: Wait for capacity option
- **WHEN** send() is called with waitForCapacity: true
- **AND** token bucket is empty
- **THEN** SDK SHALL wait until token available
- **AND** then send the message
- **AND** respect maxWait timeout

### Requirement: Rate limit fairness
The SDK SHALL ensure fair distribution of rate limit capacity.

#### Scenario: Fair queuing across peers
- **WHEN** multiple peers are being messaged
- **AND** global rate limit is approached
- **THEN** SDK SHALL distribute capacity fairly
- **AND** no single peer SHALL starve others
