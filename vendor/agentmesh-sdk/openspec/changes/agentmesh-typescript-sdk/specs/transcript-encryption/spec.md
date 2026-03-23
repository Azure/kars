## ADDED Requirements

### Requirement: Encrypt audit logs at rest
The SDK SHALL encrypt all audit log entries before writing to storage.

#### Scenario: Audit entry encrypted before storage
- **WHEN** audit logger writes a log entry
- **THEN** entry SHALL be encrypted with owner's derived key
- **AND** stored as { nonce: base64, ciphertext: base64 }
- **AND** plaintext SHALL NOT be written to storage

#### Scenario: Encryption uses unique nonce per entry
- **WHEN** multiple audit entries are written
- **THEN** each entry SHALL have a unique random nonce
- **AND** nonces SHALL be cryptographically random (12 bytes)

### Requirement: Derive encryption key from owner identity
The SDK SHALL derive the audit encryption key from the owner's X25519 private key.

#### Scenario: Key derivation on initialization
- **WHEN** audit logger is initialized with identity
- **THEN** SDK SHALL derive encryption key using HKDF
- **AND** use salt "agentmesh-audit-v1"
- **AND** use info "transcript-encryption"
- **AND** output 32-byte key for XChaCha20-Poly1305

#### Scenario: Same identity produces same key
- **WHEN** audit logger is initialized multiple times with same identity
- **THEN** derived encryption key SHALL be identical each time

### Requirement: Decrypt audit logs on demand
The SDK SHALL provide methods to decrypt audit logs for viewing.

#### Scenario: Decrypt single log entry
- **WHEN** application calls decryptEntry(encryptedEntry, identity)
- **THEN** SDK SHALL derive decryption key from identity
- **AND** decrypt ciphertext using stored nonce
- **AND** return plaintext JSON entry

#### Scenario: Decrypt fails with wrong identity
- **WHEN** decryptEntry is called with different identity than encryption
- **THEN** decryption SHALL fail
- **AND** throw DecryptionError with code 'invalid_key'

#### Scenario: Export decrypted audit log
- **WHEN** application calls exportAuditLog(identity, options)
- **THEN** SDK SHALL decrypt all entries
- **AND** return array of plaintext entries
- **AND** support filtering by date range and event type

### Requirement: Transcript encryption configuration
The SDK SHALL allow configuration of transcript encryption behavior.

#### Scenario: Disable encryption (not recommended)
- **WHEN** audit logger is configured with encrypted: false
- **THEN** entries SHALL be written in plaintext
- **AND** warning SHALL be logged about security risk

#### Scenario: Encryption enabled by default
- **WHEN** audit logger is initialized without explicit config
- **THEN** encryption SHALL be enabled by default

### Requirement: Key rotation for transcripts
The SDK SHALL support re-encrypting transcripts when keys rotate.

#### Scenario: Re-encrypt transcripts after key rotation
- **WHEN** application calls reencryptTranscripts(oldIdentity, newIdentity)
- **THEN** SDK SHALL decrypt all entries with old key
- **AND** re-encrypt all entries with new key
- **AND** replace stored entries with new ciphertext

#### Scenario: Re-encryption progress tracking
- **WHEN** re-encryption is in progress
- **THEN** SDK SHALL emit 'reencrypt_progress' events
- **AND** include processed count and total count

### Requirement: Transcript integrity verification
The SDK SHALL verify transcript integrity on decryption.

#### Scenario: Tampered ciphertext detected
- **WHEN** stored ciphertext has been modified
- **THEN** decryption SHALL fail authentication check
- **AND** throw IntegrityError with code 'tampered'

#### Scenario: Corrupted nonce detected
- **WHEN** stored nonce is invalid (wrong length)
- **THEN** decryption SHALL fail
- **AND** throw IntegrityError with code 'invalid_nonce'
