# Cross-SDK Compatibility Tests

This directory contains tests to verify compatibility between the TypeScript SDK and Python SDK.

## Prerequisites

1. Both SDKs must be installed and configured
2. A running AgentMesh registry and relay (or test instances)
3. Python 3.10+ with agentmesh-sdk installed

## Test Categories

### AMID Compatibility (`amid-compat.test.ts`)
Verifies that AMID generation produces compatible identifiers across SDKs.

### X3DH Compatibility (`x3dh-compat.test.ts`)
Verifies X3DH key exchange works between TypeScript and Python agents.

### Double Ratchet Compatibility (`ratchet-compat.test.ts`)
Verifies encrypted message exchange using Double Ratchet protocol.

### Message Exchange (`message-exchange.test.ts`)
End-to-end message exchange between TypeScript and Python agents.

## Running Tests

```bash
# Run all cross-SDK tests
npm run test:cross-sdk

# Run specific test
npm test -- tests/cross-sdk/amid-compat.test.ts
```

## Test Data

Test vectors are stored in `fixtures/` directory to enable reproducible testing.

## Notes

- These tests require network access to test instances
- Some tests may require manual Python agent setup
- See individual test files for specific requirements
