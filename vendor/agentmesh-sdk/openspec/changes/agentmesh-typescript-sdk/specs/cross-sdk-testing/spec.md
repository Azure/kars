## ADDED Requirements

### Requirement: AMID derivation compatibility
The SDK SHALL derive identical AMIDs as the Python SDK for the same keys.

#### Scenario: AMID matches Python for test vector
- **WHEN** TypeScript SDK derives AMID from test vector signing key
- **THEN** AMID SHALL match value produced by Python SDK
- **AND** match base58(sha256(signing_public_key)[:20]) exactly

#### Scenario: AMID test vectors documented
- **WHEN** cross-SDK tests run
- **THEN** at least 3 AMID test vectors SHALL be validated
- **AND** test vectors SHALL include edge cases (all zeros, all ones, random)

### Requirement: X3DH compatibility
The SDK SHALL produce compatible X3DH output with the Python SDK.

#### Scenario: Shared secret matches Python
- **WHEN** TypeScript initiates X3DH with Python responder's bundle
- **THEN** derived shared secret SHALL match Python's derivation
- **AND** using identical test vector keys

#### Scenario: Prekey bundle format compatible
- **WHEN** TypeScript exports prekey bundle
- **THEN** Python SDK SHALL successfully parse the bundle
- **AND** validate signed prekey signature

#### Scenario: X3DH message format compatible
- **WHEN** TypeScript sends X3DH initial message
- **THEN** Python SDK SHALL successfully parse the message
- **AND** extract ephemeral key and used prekey index

### Requirement: Double Ratchet compatibility
The SDK SHALL produce compatible Double Ratchet ciphertext with Python SDK.

#### Scenario: Python decrypts TypeScript message
- **WHEN** TypeScript encrypts message with Double Ratchet
- **AND** Python has matching ratchet state
- **THEN** Python SHALL successfully decrypt the message
- **AND** plaintext SHALL match original

#### Scenario: TypeScript decrypts Python message
- **WHEN** Python encrypts message with Double Ratchet
- **AND** TypeScript has matching ratchet state
- **THEN** TypeScript SHALL successfully decrypt the message

#### Scenario: Bidirectional ratchet progression
- **WHEN** TypeScript and Python exchange 10 messages alternating
- **THEN** all messages SHALL decrypt successfully
- **AND** ratchet state SHALL remain synchronized

### Requirement: HKDF compatibility
The SDK SHALL produce identical HKDF output as Python SDK.

#### Scenario: HKDF test vectors match
- **WHEN** TypeScript runs HKDF with test vector inputs
- **THEN** output SHALL match Python HKDF output
- **AND** for salt, ikm, info, length combinations

### Requirement: Signature compatibility
The SDK SHALL produce Ed25519 signatures verifiable by Python.

#### Scenario: Python verifies TypeScript signature
- **WHEN** TypeScript signs message with Ed25519
- **THEN** Python SHALL successfully verify the signature

#### Scenario: TypeScript verifies Python signature
- **WHEN** Python signs message with Ed25519
- **THEN** TypeScript SHALL successfully verify the signature

### Requirement: Test vector documentation
The SDK SHALL maintain documented test vectors for cross-SDK validation.

#### Scenario: Test vectors file exists
- **WHEN** cross-SDK tests run
- **THEN** tests/cross-sdk/fixtures/test-vectors.ts SHALL exist
- **AND** contain all required test vector data

#### Scenario: Test vectors include all primitives
- **WHEN** test vectors are reviewed
- **THEN** SHALL include: AMID, X3DH, Double Ratchet, HKDF, AES-GCM, Ed25519

#### Scenario: Test vector generation script
- **WHEN** developer needs new test vectors
- **THEN** Python script SHALL generate compatible vectors
- **AND** output format matches TypeScript test-vectors.ts structure

### Requirement: KNOCK protocol compatibility
The SDK SHALL exchange KNOCK messages with Python agents.

#### Scenario: TypeScript KNOCK to Python agent
- **WHEN** TypeScript sends KNOCK to Python agent
- **THEN** Python SHALL parse and validate KNOCK
- **AND** respond with ACCEPT or REJECT

#### Scenario: Python KNOCK to TypeScript agent
- **WHEN** Python sends KNOCK to TypeScript agent
- **THEN** TypeScript SHALL parse and validate KNOCK
- **AND** respond with ACCEPT or REJECT

### Requirement: End-to-end message exchange
The SDK SHALL exchange encrypted messages with Python agents.

#### Scenario: Full conversation test
- **WHEN** TypeScript and Python agents are both running
- **THEN** full message exchange SHALL succeed
- **AND** from KNOCK through multiple encrypted messages

#### Scenario: Session persistence across reconnect
- **WHEN** TypeScript agent disconnects and reconnects
- **THEN** session with Python agent SHALL resume
- **AND** without new KNOCK required
