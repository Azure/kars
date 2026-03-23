## ADDED Requirements

### Requirement: Kill session circuit breaker
The SDK SHALL provide a killSession method to immediately terminate a session with a specific peer.

#### Scenario: Kill active session
- **WHEN** application calls killSession(amid)
- **AND** session exists with that peer
- **THEN** SDK SHALL send CLOSE message to peer
- **AND** remove session from active sessions
- **AND** remove session from cache
- **AND** emit 'session_killed' event

#### Scenario: Kill non-existent session
- **WHEN** application calls killSession(amid)
- **AND** no session exists with that peer
- **THEN** SDK SHALL return silently (no error)

### Requirement: Pause new sessions circuit breaker
The SDK SHALL provide pauseNew and resumeNew methods to control KNOCK acceptance.

#### Scenario: Pause new KNOCK requests
- **WHEN** application calls pauseNew()
- **THEN** SDK state SHALL transition to PAUSED
- **AND** all incoming KNOCK requests SHALL be rejected with reason "agent_paused"
- **AND** existing sessions SHALL continue to function

#### Scenario: Resume KNOCK acceptance
- **WHEN** application calls resumeNew()
- **AND** SDK is in PAUSED state
- **THEN** SDK state SHALL transition to RUNNING
- **AND** incoming KNOCK requests SHALL be processed normally

#### Scenario: Pause is idempotent
- **WHEN** application calls pauseNew() while already paused
- **THEN** SDK SHALL remain in PAUSED state
- **AND** no error SHALL be thrown

### Requirement: Block peer circuit breaker
The SDK SHALL provide a block method to add a peer to the blocklist and kill their session.

#### Scenario: Block peer with active session
- **WHEN** application calls block(amid)
- **AND** session exists with that peer
- **THEN** SDK SHALL add peer to blocklist
- **AND** kill the active session
- **AND** emit 'peer_blocked' event

#### Scenario: Block peer without session
- **WHEN** application calls block(amid)
- **AND** no session exists with that peer
- **THEN** SDK SHALL add peer to blocklist
- **AND** emit 'peer_blocked' event

#### Scenario: Blocked peer KNOCK rejected
- **WHEN** blocked peer sends KNOCK request
- **THEN** SDK SHALL reject with reason "blocked"
- **AND** NOT process the KNOCK

### Requirement: Unblock peer
The SDK SHALL provide an unblock method to remove a peer from the blocklist.

#### Scenario: Unblock previously blocked peer
- **WHEN** application calls unblock(amid)
- **AND** peer is on blocklist
- **THEN** SDK SHALL remove peer from blocklist
- **AND** emit 'peer_unblocked' event

### Requirement: Emergency stop circuit breaker
The SDK SHALL provide an emergencyStop method for complete shutdown.

#### Scenario: Emergency stop execution
- **WHEN** application calls emergencyStop()
- **THEN** SDK SHALL transition to STOPPED state
- **AND** disconnect from relay
- **AND** reject all pending KNOCK requests
- **AND** close all active sessions
- **AND** clear session cache
- **AND** emit 'emergency_stop' event

#### Scenario: Emergency stop is terminal
- **WHEN** SDK is in STOPPED state
- **THEN** SDK SHALL reject all operations with error "client_stopped"
- **AND** reconnect() SHALL throw error
- **AND** new client instance required to resume

### Requirement: Circuit breaker state introspection
The SDK SHALL expose current circuit breaker state.

#### Scenario: Query circuit breaker state
- **WHEN** application calls getCircuitState()
- **THEN** SDK SHALL return current state: RUNNING, PAUSED, or STOPPED
- **AND** include timestamp of last state change

#### Scenario: Circuit breaker state in client info
- **WHEN** application calls getInfo()
- **THEN** response SHALL include circuitState field
