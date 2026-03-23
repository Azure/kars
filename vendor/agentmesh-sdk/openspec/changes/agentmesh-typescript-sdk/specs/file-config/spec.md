## ADDED Requirements

### Requirement: Standard configuration directory
The SDK SHALL use ~/.agentmesh/ as the default configuration directory.

#### Scenario: Default directory structure
- **WHEN** SDK is initialized without custom paths
- **THEN** SDK SHALL use the following structure:
  - ~/.agentmesh/keys/ for identity keys
  - ~/.agentmesh/sessions/ for session state
  - ~/.agentmesh/policy.json for policy configuration
  - ~/.agentmesh/audit/ for encrypted audit logs

#### Scenario: Environment variable override
- **WHEN** AGENTMESH_HOME environment variable is set
- **THEN** SDK SHALL use that path instead of ~/.agentmesh/
- **AND** maintain same subdirectory structure

#### Scenario: Create directories if missing
- **WHEN** SDK initializes and ~/.agentmesh/ does not exist
- **THEN** SDK SHALL create the directory structure
- **AND** set appropriate file permissions (0700 for directories)

### Requirement: Policy file loading
The SDK SHALL load policy configuration from policy.json file.

#### Scenario: Load policy on initialization
- **WHEN** SDK client is created
- **AND** ~/.agentmesh/policy.json exists
- **THEN** SDK SHALL read and parse the policy file
- **AND** apply policy to KNOCK evaluation

#### Scenario: Policy file missing
- **WHEN** SDK client is created
- **AND** ~/.agentmesh/policy.json does not exist
- **THEN** SDK SHALL use default open policy
- **AND** NOT throw an error

#### Scenario: Invalid policy file
- **WHEN** SDK reads policy.json
- **AND** file contains invalid JSON or invalid policy
- **THEN** SDK SHALL throw ConfigError with details
- **AND** NOT start with invalid policy

### Requirement: Policy file hot reload
The SDK SHALL support watching policy file for changes.

#### Scenario: Enable policy hot reload
- **WHEN** SDK is configured with watchPolicy: true
- **THEN** SDK SHALL watch ~/.agentmesh/policy.json for changes
- **AND** reload policy when file changes

#### Scenario: Policy reloaded on file change
- **WHEN** policy.json is modified
- **AND** watchPolicy is enabled
- **THEN** SDK SHALL validate new policy
- **AND** if valid, apply new policy
- **AND** emit 'policy_reloaded' event

#### Scenario: Invalid policy change ignored
- **WHEN** policy.json is modified with invalid content
- **AND** watchPolicy is enabled
- **THEN** SDK SHALL log warning
- **AND** keep existing valid policy
- **AND** emit 'policy_reload_failed' event

### Requirement: Identity key file storage
The SDK SHALL store identity keys in the keys directory.

#### Scenario: Save identity to standard path
- **WHEN** identity.save() is called without path
- **THEN** SDK SHALL save to ~/.agentmesh/keys/identity.json

#### Scenario: Load identity from standard path
- **WHEN** Identity.load() is called without path
- **THEN** SDK SHALL load from ~/.agentmesh/keys/identity.json

#### Scenario: Key file permissions
- **WHEN** identity keys are saved
- **THEN** file SHALL have permissions 0600 (owner read/write only)

### Requirement: Session state persistence
The SDK SHALL persist session state to the sessions directory.

#### Scenario: Save session state
- **WHEN** session is established
- **THEN** SDK SHALL save session state to ~/.agentmesh/sessions/<session_id>.json

#### Scenario: Restore sessions on startup
- **WHEN** SDK client starts
- **THEN** SDK SHALL scan sessions directory
- **AND** restore valid unexpired sessions to cache

#### Scenario: Clean expired sessions
- **WHEN** SDK client starts
- **AND** expired session files exist
- **THEN** SDK SHALL delete expired session files

### Requirement: Explicit path override
The SDK SHALL allow explicit path configuration.

#### Scenario: Custom paths in options
- **WHEN** client is created with explicit path options
- **THEN** SDK SHALL use provided paths instead of defaults
- **AND** keysPath, sessionsPath, policyPath, auditPath configurable individually

#### Scenario: Mixed custom and default paths
- **WHEN** only some paths are specified
- **THEN** SDK SHALL use custom paths for specified
- **AND** use default paths for unspecified

### Requirement: Container-friendly configuration
The SDK SHALL support containerized deployments.

#### Scenario: Disable file-based storage
- **WHEN** SDK is configured with useFileStorage: false
- **THEN** SDK SHALL NOT access filesystem
- **AND** require explicit Storage implementation

#### Scenario: Read-only filesystem support
- **WHEN** SDK cannot write to ~/.agentmesh/
- **AND** gracefulFallback: true is configured
- **THEN** SDK SHALL log warning
- **AND** use in-memory storage for that component
