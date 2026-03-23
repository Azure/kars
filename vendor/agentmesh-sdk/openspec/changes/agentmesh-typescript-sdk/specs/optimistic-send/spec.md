## ADDED Requirements

### Requirement: Optimistic send for allowlist contacts
The SDK SHALL send KNOCK and initial message in a single packet for contacts on the allowlist.

#### Scenario: Optimistic send to allowlist contact
- **WHEN** agent sends message to peer on allowlist
- **AND** no cached session exists
- **THEN** SDK SHALL send combined knock_with_message packet
- **AND** packet SHALL contain both KNOCK request and encrypted message

#### Scenario: Non-allowlist contact uses standard flow
- **WHEN** agent sends message to peer NOT on allowlist
- **THEN** SDK SHALL use standard KNOCK-then-message flow
- **AND** wait for ACCEPT before sending message

### Requirement: Optimistic send message delivery
The relay SHALL hold the optimistic message until KNOCK is processed.

#### Scenario: Optimistic message delivered after ACCEPT
- **WHEN** relay receives knock_with_message packet
- **THEN** relay SHALL deliver KNOCK to recipient first
- **AND** wait for ACCEPT response
- **AND** then deliver the held message

#### Scenario: Optimistic message dropped on REJECT
- **WHEN** relay receives knock_with_message packet
- **AND** recipient sends REJECT for the KNOCK
- **THEN** relay SHALL drop the held message
- **AND** notify sender of rejection

### Requirement: Optimistic send configuration
The SDK SHALL allow configuration of optimistic send behavior.

#### Scenario: Disable optimistic send globally
- **WHEN** client is configured with optimisticSend: false
- **THEN** SDK SHALL NOT use optimistic send even for allowlist contacts

#### Scenario: Per-send optimistic override
- **WHEN** send() is called with option forceOptimistic: true
- **AND** target is on allowlist
- **THEN** SDK SHALL use optimistic send

#### Scenario: Optimistic send disabled for non-allowlist
- **WHEN** send() is called with forceOptimistic: true
- **AND** target is NOT on allowlist
- **THEN** SDK SHALL ignore forceOptimistic
- **AND** use standard KNOCK flow

### Requirement: Optimistic send failure handling
The SDK SHALL handle optimistic send failures gracefully.

#### Scenario: Notify sender of dropped message
- **WHEN** optimistic message is dropped due to REJECT
- **THEN** SDK SHALL emit 'optimistic_dropped' event
- **AND** event SHALL include recipient AMID and rejection reason

#### Scenario: Retry available for dropped messages
- **WHEN** optimistic message is dropped
- **THEN** SDK SHALL NOT automatically retry
- **AND** application MAY manually retry with standard flow
