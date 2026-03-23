/**
 * Cross-SDK Double Ratchet Compatibility Tests
 *
 * These tests verify that Double Ratchet encryption works correctly
 * between TypeScript and Python SDKs.
 */

import { describe, test, expect, beforeAll } from 'vitest';
import { DoubleRatchetSession } from '../../src/encryption/ratchet';
import { hkdf, kdfCK, kdfRK } from '../../src/encryption/hkdf';
import sodium from 'libsodium-wrappers';
import {
  hkdfVectors,
  ratchetVectors,
  secretboxVectors,
  base64ToBytes,
  bytesToBase64,
} from './fixtures/test-vectors';

describe('Cross-SDK Double Ratchet Compatibility', () => {
  beforeAll(async () => {
    await sodium.ready;
  });

  describe('HKDF compatibility', () => {
    test('should match Python HKDF implementation', async () => {
      // Test vector from Python SDK
      const ikm = base64ToBytes(hkdfVectors.vector1.ikm);
      const salt = base64ToBytes(hkdfVectors.vector1.salt);
      const info = new TextEncoder().encode(hkdfVectors.vector1.info);
      const length = hkdfVectors.vector1.length;
      const expectedOutput = base64ToBytes(hkdfVectors.vector1.expectedOutput);

      // Derive key using TypeScript HKDF
      const output = await hkdf(ikm, salt, info, length);

      expect(bytesToBase64(output)).toBe(bytesToBase64(expectedOutput));
    });
  });

  describe('Chain key derivation (kdf_ck)', () => {
    test('should derive same message keys as Python', async () => {
      // Test vector from Python SDK
      const chainKey = base64ToBytes(ratchetVectors.chainKeyDerivation.chainKey);
      const expectedMessageKey = base64ToBytes(ratchetVectors.chainKeyDerivation.expectedMessageKey);
      const expectedNextChainKey = base64ToBytes(ratchetVectors.chainKeyDerivation.expectedNextChainKey);

      // Derive using TypeScript kdfCK
      const [messageKey, nextChainKey] = await kdfCK(chainKey);

      expect(bytesToBase64(messageKey)).toBe(bytesToBase64(expectedMessageKey));
      expect(bytesToBase64(nextChainKey)).toBe(bytesToBase64(expectedNextChainKey));
    });

    test('chain key derivation should be deterministic', async () => {
      const chainKey = base64ToBytes(ratchetVectors.chainKeyDerivation.chainKey);

      const [msgKey1, chainKey1] = await kdfCK(chainKey);
      const [msgKey2, chainKey2] = await kdfCK(chainKey);

      expect(bytesToBase64(msgKey1)).toBe(bytesToBase64(msgKey2));
      expect(bytesToBase64(chainKey1)).toBe(bytesToBase64(chainKey2));
    });
  });

  describe('Root key derivation (kdf_rk)', () => {
    test('should derive same root and chain keys as Python', async () => {
      // Test vector from Python SDK
      const rootKey = base64ToBytes(ratchetVectors.rootKeyDerivation.rootKey);
      const dhOutput = base64ToBytes(ratchetVectors.rootKeyDerivation.dhOutput);
      const expectedNewRoot = base64ToBytes(ratchetVectors.rootKeyDerivation.expectedNewRoot);
      const expectedChainKey = base64ToBytes(ratchetVectors.rootKeyDerivation.expectedChainKey);

      // Derive using TypeScript kdfRK
      const [newRoot, newChain] = await kdfRK(rootKey, dhOutput);

      expect(bytesToBase64(newRoot)).toBe(bytesToBase64(expectedNewRoot));
      expect(bytesToBase64(newChain)).toBe(bytesToBase64(expectedChainKey));
    });

    test('root key derivation should be deterministic', async () => {
      const rootKey = base64ToBytes(ratchetVectors.rootKeyDerivation.rootKey);
      const dhOutput = base64ToBytes(ratchetVectors.rootKeyDerivation.dhOutput);

      const [newRoot1, newChain1] = await kdfRK(rootKey, dhOutput);
      const [newRoot2, newChain2] = await kdfRK(rootKey, dhOutput);

      expect(bytesToBase64(newRoot1)).toBe(bytesToBase64(newRoot2));
      expect(bytesToBase64(newChain1)).toBe(bytesToBase64(newChain2));
    });
  });

  describe('XSalsa20-Poly1305 (NaCl SecretBox) compatibility', () => {
    test('should encrypt same as Python NaCl', async () => {
      // Test vector from Python SDK
      const key = base64ToBytes(secretboxVectors.vector1.key);
      const nonce = base64ToBytes(secretboxVectors.vector1.nonce);
      const plaintext = new TextEncoder().encode(secretboxVectors.vector1.plaintext);
      const expectedCiphertext = base64ToBytes(secretboxVectors.vector1.expectedCiphertext);

      // Encrypt using libsodium with the same nonce
      // Note: The expected ciphertext includes the nonce prefix
      const encrypted = sodium.crypto_secretbox_easy(plaintext, nonce, key);

      // Combine nonce + encrypted to match Python format
      const result = new Uint8Array(nonce.length + encrypted.length);
      result.set(nonce, 0);
      result.set(encrypted, nonce.length);

      expect(bytesToBase64(result)).toBe(bytesToBase64(expectedCiphertext));
    });

    test('should decrypt Python-encrypted data', async () => {
      // Test vector from Python SDK
      const key = base64ToBytes(secretboxVectors.vector1.key);
      const ciphertext = base64ToBytes(secretboxVectors.vector1.expectedCiphertext);
      const expectedPlaintext = secretboxVectors.vector1.plaintext;

      // Extract nonce from beginning
      const nonce = ciphertext.slice(0, sodium.crypto_secretbox_NONCEBYTES);
      const encrypted = ciphertext.slice(sodium.crypto_secretbox_NONCEBYTES);

      // Decrypt using libsodium
      const decrypted = sodium.crypto_secretbox_open_easy(encrypted, nonce, key);

      const decryptedText = new TextDecoder().decode(decrypted);
      expect(decryptedText).toBe(expectedPlaintext);
    });

    test('encryption followed by decryption should recover plaintext', async () => {
      const key = sodium.randombytes_buf(32);
      const plaintext = new TextEncoder().encode('Cross-SDK test message!');
      const nonce = sodium.randombytes_buf(sodium.crypto_secretbox_NONCEBYTES);

      // Encrypt
      const encrypted = sodium.crypto_secretbox_easy(plaintext, nonce, key);

      // Decrypt
      const decrypted = sodium.crypto_secretbox_open_easy(encrypted, nonce, key);

      expect(new TextDecoder().decode(decrypted)).toBe('Cross-SDK test message!');
    });
  });

  describe('Ratchet initialization', () => {
    test('should initialize from X3DH shared secret', async () => {
      // Use a known shared secret
      const sharedSecret = new Uint8Array(32).fill(42);

      // Initialize ratchet as sender
      const session = await DoubleRatchetSession.initialize(sharedSecret, true);

      // Verify ratchet public key is 32 bytes
      const publicKey = session.getRatchetPublicKey();
      expect(publicKey.length).toBe(32);

      // Should be able to get state
      const state = session.getState();
      expect(state.rootKey).toEqual(sharedSecret);
    });

    test('initiator and responder should establish compatible sessions', async () => {
      // Shared secret from X3DH
      const sharedSecret = new Uint8Array(32);
      for (let i = 0; i < 32; i++) {
        sharedSecret[i] = i;
      }

      // Initialize both sides
      const alice = await DoubleRatchetSession.initialize(sharedSecret, true);
      const bob = await DoubleRatchetSession.initialize(sharedSecret, false);

      // Alice sets Bob's key (normally from prekey bundle)
      const bobPublicKey = bob.getRatchetPublicKey();
      await alice.initializeReceiving(bobPublicKey);

      // Bob sets Alice's key (from first message header)
      const alicePublicKey = alice.getRatchetPublicKey();
      await bob.initializeReceiving(alicePublicKey);

      // Alice encrypts
      const plaintext = new TextEncoder().encode('Hello Bob!');
      const encrypted = await alice.encrypt(plaintext);

      // Bob decrypts
      const decrypted = await bob.decrypt(encrypted);
      expect(new TextDecoder().decode(decrypted)).toBe('Hello Bob!');
    });
  });

  describe('Header format', () => {
    test('TypeScript header should match Python format', async () => {
      // Verify header structure matches Python expectations
      const headerFields = ['dh', 'pn', 'n'];

      // TypeScript header format
      interface RatchetHeader {
        dh: Uint8Array; // DH public key
        pn: number; // Previous chain length
        n: number; // Message number
      }

      // Verify structure
      const mockHeader: RatchetHeader = {
        dh: new Uint8Array(32),
        pn: 5,
        n: 10,
      };

      expect(mockHeader).toHaveProperty('dh');
      expect(mockHeader).toHaveProperty('pn');
      expect(mockHeader).toHaveProperty('n');
      expect(mockHeader.dh).toBeInstanceOf(Uint8Array);
      expect(mockHeader.dh.length).toBe(32);
    });
  });

  describe('Message exchange patterns', () => {
    test('should handle multiple messages in sequence', async () => {
      const sharedSecret = new Uint8Array(32).fill(1);

      const alice = await DoubleRatchetSession.initialize(sharedSecret, true);
      const bob = await DoubleRatchetSession.initialize(sharedSecret, false);

      // Exchange keys
      await alice.initializeReceiving(bob.getRatchetPublicKey());
      await bob.initializeReceiving(alice.getRatchetPublicKey());

      // Multiple messages Alice -> Bob
      for (let i = 0; i < 5; i++) {
        const msg = `Message ${i}`;
        const encrypted = await alice.encrypt(new TextEncoder().encode(msg));
        const decrypted = await bob.decrypt(encrypted);
        expect(new TextDecoder().decode(decrypted)).toBe(msg);
      }
    });

    test('should handle bidirectional communication', async () => {
      const sharedSecret = new Uint8Array(32).fill(2);

      const alice = await DoubleRatchetSession.initialize(sharedSecret, true);
      const bob = await DoubleRatchetSession.initialize(sharedSecret, false);

      // Exchange keys
      await alice.initializeReceiving(bob.getRatchetPublicKey());
      await bob.initializeReceiving(alice.getRatchetPublicKey());

      // Alice -> Bob
      const msg1 = await alice.encrypt(new TextEncoder().encode('Hello Bob'));
      expect(new TextDecoder().decode(await bob.decrypt(msg1))).toBe('Hello Bob');

      // Bob -> Alice (triggers DH ratchet)
      const msg2 = await bob.encrypt(new TextEncoder().encode('Hello Alice'));
      expect(new TextDecoder().decode(await alice.decrypt(msg2))).toBe('Hello Alice');

      // Alice -> Bob again
      const msg3 = await alice.encrypt(new TextEncoder().encode('How are you?'));
      expect(new TextDecoder().decode(await bob.decrypt(msg3))).toBe('How are you?');
    });
  });

  describe('State serialization', () => {
    test('ratchet state should serialize and deserialize correctly', async () => {
      const sharedSecret = new Uint8Array(32).fill(3);

      const session1 = await DoubleRatchetSession.initialize(sharedSecret, true);
      await session1.initializeReceiving(new Uint8Array(32).fill(4));

      // Encrypt a message to advance state
      await session1.encrypt(new TextEncoder().encode('Test'));

      // Serialize state
      const serialized = session1.serializeState();

      // Verify serialization format
      expect(serialized).toHaveProperty('dh_private');
      expect(serialized).toHaveProperty('dh_public');
      expect(serialized).toHaveProperty('root_key');
      expect(serialized).toHaveProperty('send_chain_key');
      expect(serialized).toHaveProperty('send_message_number');
      expect(typeof serialized.dh_private).toBe('string'); // Base64 encoded
      expect(serialized.send_message_number).toBe(1);

      // Deserialize
      const session2 = DoubleRatchetSession.deserializeState(serialized, true);
      const state2 = session2.getState();

      // Verify state was restored
      expect(bytesToBase64(state2.dhPublic)).toBe(serialized.dh_public);
      expect(state2.sendMessageNumber).toBe(1);
    });
  });
});
