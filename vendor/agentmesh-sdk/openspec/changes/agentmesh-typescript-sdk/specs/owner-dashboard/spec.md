## ADDED Requirements

### Requirement: Dashboard HTTP server
The SDK SHALL provide an optional HTTP dashboard server on localhost.

#### Scenario: Start dashboard server
- **WHEN** application calls dashboard.start(port)
- **THEN** HTTP server SHALL start on specified port (default 7777)
- **AND** server SHALL only bind to localhost (127.0.0.1)
- **AND** emit 'dashboard_started' event with port

#### Scenario: Stop dashboard server
- **WHEN** application calls dashboard.stop()
- **THEN** HTTP server SHALL stop accepting connections
- **AND** existing connections SHALL be closed gracefully
- **AND** emit 'dashboard_stopped' event

#### Scenario: Port already in use
- **WHEN** dashboard.start() is called
- **AND** specified port is already in use
- **THEN** SDK SHALL throw DashboardError with code 'port_in_use'

### Requirement: Status endpoint
The dashboard SHALL provide a status endpoint.

#### Scenario: GET /status returns client info
- **WHEN** GET request is made to /status
- **THEN** response SHALL be JSON with status 200
- **AND** include: amid, connected, capabilities, activeSessions, circuitState, uptime

### Requirement: Sessions endpoint
The dashboard SHALL provide session management endpoints.

#### Scenario: GET /sessions lists active sessions
- **WHEN** GET request is made to /sessions
- **THEN** response SHALL be JSON array of sessions
- **AND** each session SHALL include: id, peerAmid, state, createdAt, messageCount

#### Scenario: POST /sessions/:amid/kill terminates session
- **WHEN** POST request is made to /sessions/:amid/kill
- **THEN** SDK SHALL call killSession(amid)
- **AND** response SHALL be 200 with { success: true }

#### Scenario: Kill non-existent session
- **WHEN** POST request is made to /sessions/:amid/kill
- **AND** no session exists for that AMID
- **THEN** response SHALL be 404 with { error: 'session_not_found' }

### Requirement: Policy endpoints
The dashboard SHALL provide policy management endpoints.

#### Scenario: GET /policy returns current policy
- **WHEN** GET request is made to /policy
- **THEN** response SHALL be JSON representation of current policy
- **AND** include: minTier, acceptedIntents, blocklist, allowlist, maxConcurrent

#### Scenario: POST /policy updates policy
- **WHEN** POST request is made to /policy with valid policy JSON
- **THEN** SDK SHALL update current policy
- **AND** response SHALL be 200 with { success: true }
- **AND** emit 'policy_updated' event

#### Scenario: Invalid policy rejected
- **WHEN** POST request is made to /policy with invalid JSON
- **THEN** response SHALL be 400 with { error: 'invalid_policy', details: [...] }

### Requirement: Circuit breaker endpoints
The dashboard SHALL provide circuit breaker control endpoints.

#### Scenario: POST /circuit/pause pauses new sessions
- **WHEN** POST request is made to /circuit/pause
- **THEN** SDK SHALL call pauseNew()
- **AND** response SHALL be 200 with { state: 'PAUSED' }

#### Scenario: POST /circuit/resume resumes sessions
- **WHEN** POST request is made to /circuit/resume
- **THEN** SDK SHALL call resumeNew()
- **AND** response SHALL be 200 with { state: 'RUNNING' }

#### Scenario: POST /circuit/emergency-stop stops everything
- **WHEN** POST request is made to /circuit/emergency-stop
- **THEN** SDK SHALL call emergencyStop()
- **AND** response SHALL be 200 with { state: 'STOPPED' }

### Requirement: Dashboard security
The dashboard SHALL implement basic security measures.

#### Scenario: Localhost only binding
- **WHEN** dashboard is started
- **THEN** server SHALL bind to 127.0.0.1 only
- **AND** NOT accept connections from other hosts

#### Scenario: Optional API key authentication
- **WHEN** dashboard is configured with apiKey option
- **THEN** all requests SHALL require X-API-Key header
- **AND** requests without valid key SHALL receive 401 response

### Requirement: Dashboard CORS configuration
The dashboard SHALL support CORS for browser-based UIs.

#### Scenario: CORS headers for localhost origins
- **WHEN** request is made with Origin header from localhost
- **THEN** response SHALL include Access-Control-Allow-Origin
- **AND** support preflight OPTIONS requests
