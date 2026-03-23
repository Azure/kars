## ADDED Requirements

### Requirement: Process pending messages on connect
The SDK SHALL process all pending messages queued by the relay server when the client connects after being offline.

#### Scenario: Client reconnects and receives queued messages
- **WHEN** client connects to relay after being offline
- **AND** relay reports pending_messages count > 0
- **THEN** SDK SHALL request all pending messages from relay
- **AND** process each message through registered handlers
- **AND** emit 'pending_processed' event with count when complete

#### Scenario: No pending messages on connect
- **WHEN** client connects to relay
- **AND** relay reports pending_messages count = 0
- **THEN** SDK SHALL NOT request pending messages
- **AND** proceed with normal operation

### Requirement: Pending message ordering
The SDK SHALL process pending messages in the order they were originally sent (FIFO).

#### Scenario: Multiple pending messages delivered in order
- **WHEN** client receives 3 pending messages sent at times T1, T2, T3
- **THEN** message handlers SHALL be called in order: T1 first, then T2, then T3

### Requirement: Pending message decryption
The SDK SHALL decrypt pending messages using existing session state if available.

#### Scenario: Decrypt pending message with existing session
- **WHEN** pending message arrives from peer with active session
- **THEN** SDK SHALL decrypt using stored ratchet state
- **AND** deliver plaintext to message handler

#### Scenario: Pending message from unknown peer
- **WHEN** pending message arrives from peer with no session
- **THEN** SDK SHALL attempt session establishment from message
- **AND** if establishment fails, emit 'pending_failed' event with error

### Requirement: Pending message acknowledgment
The SDK SHALL acknowledge processed pending messages to the relay.

#### Scenario: Acknowledge successfully processed message
- **WHEN** pending message is successfully processed
- **THEN** SDK SHALL send ACK to relay
- **AND** relay SHALL remove message from queue

#### Scenario: Failed message processing
- **WHEN** pending message processing fails (decryption error, handler error)
- **THEN** SDK SHALL send NACK with reason to relay
- **AND** emit 'pending_failed' event with details
