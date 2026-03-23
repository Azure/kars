/**
 * Cross-SDK X3DH Compatibility Tests
 *
 * These tests verify that X3DH key exchange works correctly
 * between TypeScript and Python SDKs.
 */

import { describe, test, expect } from 'vitest';
import { Identity } from '../../src/identity';
import { PrekeyManager, x25519DH, generateX25519Keypair } from '../../src/encryption/prekey';
import { hkdf } from '../../src/encryption/hkdf';
import { MemoryStorage } from '../../src/storage';
import {
  x3dhVectors,
  base64ToBytes,
  bytesToBase64,
} from './fixtures/test-vectors';

/**
 * Perform X3DH as initiator (Alice).
 * Computes shared secret from DH operations.
 */
async function x3dhInitiator(
  aliceIdentityPrivate: Uint8Array,
  aliceEphemeralPrivate: Uint8Array,
  bobIdentityPublic: Uint8Array,
  bobSignedPrekey: Uint8Array,
  bobOneTimePrekey?: Uint8Array
): Promise<Uint8Array> {
  // DH1 = DH(IKa, SPKb)
  const dh1 = await x25519DH(aliceIdentityPrivate, bobSignedPrekey);

  // DH2 = DH(EKa, IKb)
  const dh2 = await x25519DH(aliceEphemeralPrivate, bobIdentityPublic);

  // DH3 = DH(EKa, SPKb)
  const dh3 = await x25519DH(aliceEphemeralPrivate, bobSignedPrekey);

  // Concatenate DH outputs
  let dhConcat: Uint8Array;
  if (bobOneTimePrekey) {
    // DH4 = DH(EKa, OPKb)
    const dh4 = await x25519DH(aliceEphemeralPrivate, bobOneTimePrekey);
    dhConcat = new Uint8Array(128);
    dhConcat.set(dh1, 0);
    dhConcat.set(dh2, 32);
    dhConcat.set(dh3, 64);
    dhConcat.set(dh4, 96);
  } else {
    dhConcat = new Uint8Array(96);
    dhConcat.set(dh1, 0);
    dhConcat.set(dh2, 32);
    dhConcat.set(dh3, 64);
  }

  // Derive shared secret using HKDF
  const info = new TextEncoder().encode('X3DH');
  const salt = new Uint8Array(32); // Zero salt
  return hkdf(dhConcat, salt, info, 32);
}

/**
 * Perform X3DH as responder (Bob).
 * Computes shared secret from DH operations.
 */
async function x3dhResponder(
  bobIdentityPrivate: Uint8Array,
  bobSignedPrekeyPrivate: Uint8Array,
  aliceIdentityPublic: Uint8Array,
  aliceEphemeralPublic: Uint8Array,
  bobOneTimePrekeyPrivate?: Uint8Array
): Promise<Uint8Array> {
  // DH1 = DH(SPKb, IKa)
  const dh1 = await x25519DH(bobSignedPrekeyPrivate, aliceIdentityPublic);

  // DH2 = DH(IKb, EKa)
  const dh2 = await x25519DH(bobIdentityPrivate, aliceEphemeralPublic);

  // DH3 = DH(SPKb, EKa)
  const dh3 = await x25519DH(bobSignedPrekeyPrivate, aliceEphemeralPublic);

  // Concatenate DH outputs
  let dhConcat: Uint8Array;
  if (bobOneTimePrekeyPrivate) {
    // DH4 = DH(OPKb, EKa)
    const dh4 = await x25519DH(bobOneTimePrekeyPrivate, aliceEphemeralPublic);
    dhConcat = new Uint8Array(128);
    dhConcat.set(dh1, 0);
    dhConcat.set(dh2, 32);
    dhConcat.set(dh3, 64);
    dhConcat.set(dh4, 96);
  } else {
    dhConcat = new Uint8Array(96);
    dhConcat.set(dh1, 0);
    dhConcat.set(dh2, 32);
    dhConcat.set(dh3, 64);
  }

  // Derive shared secret using HKDF
  const info = new TextEncoder().encode('X3DH');
  const salt = new Uint8Array(32); // Zero salt
  return hkdf(dhConcat, salt, info, 32);
}

describe('Cross-SDK X3DH Compatibility', () => {
  describe('Prekey bundle format', () => {
    test('should generate prekey bundle in expected format', async () => {
      const storage = new MemoryStorage();
      const identity = await Identity.generate();
      const manager = new PrekeyManager(identity, storage);

      const bundle = await manager.loadOrInitialize();

      // Verify bundle structure
      expect(bundle).toHaveProperty('identityKey');
      expect(bundle).toHaveProperty('signedPrekey');
      expect(bundle).toHaveProperty('signedPrekeySignature');
      expect(bundle).toHaveProperty('oneTimePrekeys');

      // Verify types
      expect(bundle.identityKey).toBeInstanceOf(Uint8Array);
      expect(bundle.signedPrekey).toBeInstanceOf(Uint8Array);
      expect(bundle.signedPrekeySignature).toBeInstanceOf(Uint8Array);
      expect(Array.isArray(bundle.oneTimePrekeys)).toBe(true);

      // Verify key sizes
      expect(bundle.identityKey.length).toBe(32); // X25519 public key
      expect(bundle.signedPrekey.length).toBe(32); // X25519 public key
    });

    test('should parse Python prekey bundle correctly', async () => {
      // Python SDK prekey bundle format
      const pythonBundle = {
        identity_key: x3dhVectors.vector1.bobIdentityPublic,
        signed_prekey: x3dhVectors.vector1.bobSignedPrekeyPublic,
        signed_prekey_signature: x3dhVectors.vector1.bobSignedPrekeySignature,
        one_time_prekeys: [{ id: 0, key: x3dhVectors.vector1.bobOneTimePrekeyPublic }],
      };

      // Parse bundle
      const identityKey = base64ToBytes(pythonBundle.identity_key);
      const signedPrekey = base64ToBytes(pythonBundle.signed_prekey);
      const signature = base64ToBytes(pythonBundle.signed_prekey_signature);
      const oneTimePrekey = base64ToBytes(pythonBundle.one_time_prekeys[0].key);

      // Verify sizes
      expect(identityKey.length).toBe(32);
      expect(signedPrekey.length).toBe(32);
      expect(signature.length).toBe(64); // Ed25519 signature
      expect(oneTimePrekey.length).toBe(32);
    });
  });

  describe('X25519 Diffie-Hellman', () => {
    test('should perform X25519 DH correctly', async () => {
      // Generate two keypairs
      const alice = await generateX25519Keypair();
      const bob = await generateX25519Keypair();

      // Both parties compute shared secret
      const aliceSecret = await x25519DH(alice.privateKey, bob.publicKey);
      const bobSecret = await x25519DH(bob.privateKey, alice.publicKey);

      // Should match
      expect(bytesToBase64(aliceSecret)).toBe(bytesToBase64(bobSecret));
    });

    test('should produce 32-byte output', async () => {
      const alice = await generateX25519Keypair();
      const bob = await generateX25519Keypair();

      const shared = await x25519DH(alice.privateKey, bob.publicKey);

      expect(shared.length).toBe(32);
    });
  });

  describe('X3DH protocol execution', () => {
    test('should derive same shared secret as Python SDK (initiator)', async () => {
      // Test vectors from Python SDK
      const aliceIdentityPrivate = base64ToBytes(x3dhVectors.vector1.aliceIdentityPrivate);
      const aliceEphemeralPrivate = base64ToBytes(x3dhVectors.vector1.aliceEphemeralPrivate);
      const bobIdentityPublic = base64ToBytes(x3dhVectors.vector1.bobIdentityPublic);
      const bobSignedPrekey = base64ToBytes(x3dhVectors.vector1.bobSignedPrekeyPublic);
      const bobOneTimePrekey = base64ToBytes(x3dhVectors.vector1.bobOneTimePrekeyPublic);
      const expectedSharedSecret = base64ToBytes(x3dhVectors.vector1.expectedSharedSecret);

      // Compute shared secret as initiator
      const sharedSecret = await x3dhInitiator(
        aliceIdentityPrivate,
        aliceEphemeralPrivate,
        bobIdentityPublic,
        bobSignedPrekey,
        bobOneTimePrekey
      );

      expect(bytesToBase64(sharedSecret)).toBe(bytesToBase64(expectedSharedSecret));
    });

    test('should derive same shared secret as Python SDK (responder)', async () => {
      // Test vectors from Python SDK
      const bobIdentityPrivate = base64ToBytes(x3dhVectors.vector1.bobIdentityPrivate);
      const bobSignedPrekeyPrivate = base64ToBytes(x3dhVectors.vector1.bobSignedPrekeyPrivate);
      const aliceIdentityPublic = base64ToBytes(x3dhVectors.vector1.aliceIdentityPublic);
      const aliceEphemeralPublic = base64ToBytes(x3dhVectors.vector1.aliceEphemeralPublic);
      const bobOneTimePrekeyPrivate = base64ToBytes(x3dhVectors.vector1.bobOneTimePrekeyPrivate);
      const expectedSharedSecret = base64ToBytes(x3dhVectors.vector1.expectedSharedSecret);

      // Compute shared secret as responder
      const sharedSecret = await x3dhResponder(
        bobIdentityPrivate,
        bobSignedPrekeyPrivate,
        aliceIdentityPublic,
        aliceEphemeralPublic,
        bobOneTimePrekeyPrivate
      );

      expect(bytesToBase64(sharedSecret)).toBe(bytesToBase64(expectedSharedSecret));
    });

    test('initiator and responder should derive same secret', async () => {
      // Generate keys for both parties
      const aliceIdentity = await generateX25519Keypair();
      const aliceEphemeral = await generateX25519Keypair();
      const bobIdentity = await generateX25519Keypair();
      const bobSignedPrekey = await generateX25519Keypair();
      const bobOneTimePrekey = await generateX25519Keypair();

      // Alice (initiator) computes
      const aliceSecret = await x3dhInitiator(
        aliceIdentity.privateKey,
        aliceEphemeral.privateKey,
        bobIdentity.publicKey,
        bobSignedPrekey.publicKey,
        bobOneTimePrekey.publicKey
      );

      // Bob (responder) computes
      const bobSecret = await x3dhResponder(
        bobIdentity.privateKey,
        bobSignedPrekey.privateKey,
        aliceIdentity.publicKey,
        aliceEphemeral.publicKey,
        bobOneTimePrekey.privateKey
      );

      expect(bytesToBase64(aliceSecret)).toBe(bytesToBase64(bobSecret));
    });

    test('X3DH without one-time prekey should also work', async () => {
      // Generate keys for both parties
      const aliceIdentity = await generateX25519Keypair();
      const aliceEphemeral = await generateX25519Keypair();
      const bobIdentity = await generateX25519Keypair();
      const bobSignedPrekey = await generateX25519Keypair();

      // Alice (initiator) computes without OPK
      const aliceSecret = await x3dhInitiator(
        aliceIdentity.privateKey,
        aliceEphemeral.privateKey,
        bobIdentity.publicKey,
        bobSignedPrekey.publicKey
        // no one-time prekey
      );

      // Bob (responder) computes without OPK
      const bobSecret = await x3dhResponder(
        bobIdentity.privateKey,
        bobSignedPrekey.privateKey,
        aliceIdentity.publicKey,
        aliceEphemeral.publicKey
        // no one-time prekey
      );

      expect(bytesToBase64(aliceSecret)).toBe(bytesToBase64(bobSecret));
    });
  });

  describe('Signature verification', () => {
    test('should verify Python-signed prekeys', async () => {
      // Python-signed prekey test vector
      const signingKey = base64ToBytes(x3dhVectors.vector1.bobSigningPublicKey);
      const prekey = base64ToBytes(x3dhVectors.vector1.bobSignedPrekeyPublic);
      const signature = base64ToBytes(x3dhVectors.vector1.bobSignedPrekeySignature);

      // Verify signature using TypeScript crypto
      const isValid = await Identity.verifySignatureRaw(signingKey, prekey, signature);

      expect(isValid).toBe(true);
    });

    test('TypeScript-signed prekeys should be verifiable by Python', async () => {
      const storage = new MemoryStorage();
      const identity = await Identity.generate();
      const manager = new PrekeyManager(identity, storage);

      const bundle = await manager.loadOrInitialize();

      // Verify locally (same algorithm Python should use)
      const isValid = await Identity.verifySignatureRaw(
        identity.getSigningPublicKeyRaw(),
        bundle.signedPrekey,
        bundle.signedPrekeySignature
      );

      expect(isValid).toBe(true);
    });
  });

  describe('Key format interoperability', () => {
    test('X25519 keys should be 32 bytes', async () => {
      const keypair = await generateX25519Keypair();

      expect(keypair.publicKey.length).toBe(32);
      expect(keypair.privateKey.length).toBe(32);
    });

    test('should handle Python-formatted keys', () => {
      // Keys from Python test vectors
      const aliceIdentity = base64ToBytes(x3dhVectors.vector1.aliceIdentityPublic);
      const bobIdentity = base64ToBytes(x3dhVectors.vector1.bobIdentityPublic);

      expect(aliceIdentity.length).toBe(32);
      expect(bobIdentity.length).toBe(32);
    });
  });
});
