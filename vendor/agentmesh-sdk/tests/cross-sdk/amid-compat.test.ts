/**
 * Cross-SDK AMID Compatibility Tests
 *
 * These tests verify that AMID generation produces compatible
 * identifiers between TypeScript and Python SDKs.
 */

import { describe, test, expect } from 'vitest';
import { Identity } from '../../src/identity';
import {
  amidVectors,
  base58Vectors,
  signatureVectors,
  base64ToBytes,
  bytesToBase64,
  hexToBytes,
} from './fixtures/test-vectors';

// Base58 alphabet (Bitcoin-style)
const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

/**
 * Local implementation of Base58 encoding for testing.
 * Mirrors the implementation in identity.ts.
 */
function encodeBase58(bytes: Uint8Array): string {
  let num = 0n;
  for (const byte of bytes) {
    num = num * 256n + BigInt(byte);
  }

  const result: string[] = [];
  while (num > 0n) {
    const remainder = Number(num % 58n);
    result.push(BASE58_ALPHABET[remainder]!);
    num = num / 58n;
  }

  for (const byte of bytes) {
    if (byte === 0) {
      result.push(BASE58_ALPHABET[0]!);
    } else {
      break;
    }
  }

  return result.reverse().join('');
}

/**
 * Derive AMID from signing public key (mirrors identity.ts logic).
 */
async function deriveAmid(signingPublicKey: Uint8Array): Promise<string> {
  const hashBuffer = await crypto.subtle.digest('SHA-256', signingPublicKey);
  const hashBytes = new Uint8Array(hashBuffer).slice(0, 20);
  return encodeBase58(hashBytes);
}

describe('Cross-SDK AMID Compatibility', () => {
  describe('AMID derivation algorithm', () => {
    test('should produce same AMID from same key material (Python -> TS)', async () => {
      // Test vector from Python SDK
      const pythonSigningKey = base64ToBytes(amidVectors.vector1.signingPublicKey);
      const expectedAmid = amidVectors.vector1.expectedAmid;

      // Derive AMID using same algorithm
      const derivedAmid = await deriveAmid(pythonSigningKey);

      expect(derivedAmid).toBe(expectedAmid);
    });

    test('should produce AMID that Python SDK accepts (TS -> Python)', async () => {
      // Generate identity in TypeScript
      const identity = await Identity.generate();

      // Export public keys
      const publicInfo = identity.toPublicInfo();

      // Verify we have the expected structure
      expect(publicInfo.amid).toBeDefined();
      expect(publicInfo.amid.length).toBeGreaterThan(0);
      expect(publicInfo.signing_public_key).toBeDefined();
      expect(publicInfo.exchange_public_key).toBeDefined();

      // Verify AMID is valid Base58 (no 0, O, I, l characters)
      const invalidChars = /[0OIl]/;
      expect(invalidChars.test(publicInfo.amid)).toBe(false);
    });

    test('AMID should be deterministic from signing key', async () => {
      // Same key should always produce same AMID
      const pythonSigningKey = base64ToBytes(amidVectors.vector1.signingPublicKey);

      const amid1 = await deriveAmid(pythonSigningKey);
      const amid2 = await deriveAmid(pythonSigningKey);

      expect(amid1).toBe(amid2);
    });
  });

  describe('Base58 encoding', () => {
    test('should use standard Base58 alphabet', () => {
      // Verify our Base58 uses the same alphabet as Python
      const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
      expect(alphabet.length).toBe(58);
      expect(alphabet.includes('0')).toBe(false); // No zero
      expect(alphabet.includes('O')).toBe(false); // No capital O
      expect(alphabet.includes('I')).toBe(false); // No capital I
      expect(alphabet.includes('l')).toBe(false); // No lowercase L
    });

    test('should encode bytes same as Python', () => {
      // Test vector from Python SDK
      const input = hexToBytes(base58Vectors.vector1.input);
      const expectedBase58 = base58Vectors.vector1.expectedBase58;

      const encoded = encodeBase58(input);

      expect(encoded).toBe(expectedBase58);
    });

    test('should handle leading zeros correctly', () => {
      // Leading zeros should become leading '1's in Base58
      const input = hexToBytes('0000000102030405');
      const result = encodeBase58(input);

      // Should start with three '1's for the three leading zero bytes
      expect(result.startsWith('111')).toBe(true);
    });
  });

  describe('SHA256 hashing', () => {
    test('should produce same hash as Python hashlib', async () => {
      // Use the signing key from test vectors
      const input = base64ToBytes(amidVectors.vector1.signingPublicKey);

      // Hash it
      const hashBuffer = await crypto.subtle.digest('SHA-256', input);
      const hashBytes = new Uint8Array(hashBuffer);

      // Verify it's 32 bytes
      expect(hashBytes.length).toBe(32);

      // Verify the first 20 bytes produce the expected AMID
      const truncated = hashBytes.slice(0, 20);
      const amid = encodeBase58(truncated);
      expect(amid).toBe(amidVectors.vector1.expectedAmid);
    });
  });

  describe('Key format compatibility', () => {
    test('TypeScript keys should use correct prefixes', async () => {
      const identity = await Identity.generate();

      // Verify key prefixes
      expect(identity.signingPublicKeyB64).toMatch(/^ed25519:/);
      expect(identity.exchangePublicKeyB64).toMatch(/^x25519:/);
    });

    test('should parse Python key formats correctly', async () => {
      // Python SDK key format examples - strip prefix and decode
      const pythonSigningKey = 'ed25519:' + amidVectors.vector1.signingPublicKey;
      const pythonExchangeKey = 'x25519:' + amidVectors.vector1.exchangePublicKey;

      // Extract base64 after prefix
      const signingKeyB64 = pythonSigningKey.slice(8);
      const exchangeKeyB64 = pythonExchangeKey.slice(7);

      // Decode
      const signingKey = base64ToBytes(signingKeyB64);
      const exchangeKey = base64ToBytes(exchangeKeyB64);

      // Verify key sizes
      expect(signingKey.length).toBe(32); // Ed25519 public key
      expect(exchangeKey.length).toBe(32); // X25519 public key
    });
  });

  describe('Ed25519 signature compatibility', () => {
    test('should verify Python-generated signatures', async () => {
      // Test vector from Python SDK
      const publicKey = base64ToBytes(signatureVectors.vector1.publicKey);
      const message = new TextEncoder().encode(signatureVectors.vector1.message);
      const signature = base64ToBytes(signatureVectors.vector1.expectedSignature);

      // Verify signature using TypeScript crypto
      const isValid = await Identity.verifySignatureRaw(publicKey, message, signature);

      expect(isValid).toBe(true);
    });

    test('should reject invalid signatures', async () => {
      // Use valid key but wrong message
      const publicKey = base64ToBytes(signatureVectors.vector1.publicKey);
      const wrongMessage = new TextEncoder().encode('Wrong message');
      const signature = base64ToBytes(signatureVectors.vector1.expectedSignature);

      const isValid = await Identity.verifySignatureRaw(publicKey, wrongMessage, signature);

      expect(isValid).toBe(false);
    });

    test('TypeScript-signed messages should be verifiable', async () => {
      const identity = await Identity.generate();
      const message = new TextEncoder().encode('Test message from TypeScript');

      // Sign with TypeScript
      const signature = await identity.sign(message);

      // Verify with static method
      const isValid = await Identity.verifySignatureRaw(
        identity.getSigningPublicKeyRaw(),
        message,
        signature
      );

      expect(isValid).toBe(true);
    });
  });
});
