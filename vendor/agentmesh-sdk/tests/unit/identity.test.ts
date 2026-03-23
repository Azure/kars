import { describe, it, expect, beforeEach } from 'vitest';
import { Identity } from '../../src/identity';
import { MemoryStorage } from '../../src/storage/memory';

describe('Identity', () => {
  describe('generate', () => {
    it('should generate a new identity with unique AMID', async () => {
      const identity1 = await Identity.generate();
      const identity2 = await Identity.generate();

      expect(identity1.amid).toBeDefined();
      expect(identity2.amid).toBeDefined();
      expect(identity1.amid).not.toBe(identity2.amid);
    });

    it('should generate AMID of expected length (~27-28 chars)', async () => {
      const identity = await Identity.generate();

      // Base58 encoding of 20 bytes typically produces 27-28 chars
      expect(identity.amid.length).toBeGreaterThanOrEqual(25);
      expect(identity.amid.length).toBeLessThanOrEqual(30);
    });

    it('should generate valid Base58 AMID', async () => {
      const identity = await Identity.generate();
      const base58Regex = /^[123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]+$/;

      expect(identity.amid).toMatch(base58Regex);
    });

    it('should set framework metadata', async () => {
      const identity = await Identity.generate();

      expect(identity.framework).toBe('agentmesh-js');
      expect(identity.frameworkVersion).toBe('0.1.0');
      expect(identity.createdAt).toBeInstanceOf(Date);
    });

    it('should generate valid public keys', async () => {
      const identity = await Identity.generate();

      // Ed25519 public key is 32 bytes = 44 chars base64 (with prefix)
      expect(identity.signingPublicKeyB64).toMatch(/^ed25519:/);
      expect(identity.exchangePublicKeyB64).toMatch(/^x25519:/);

      // Raw bytes should be 32 bytes for both key types
      expect(identity.getSigningPublicKeyRaw().length).toBe(32);
      expect(identity.getExchangePublicKeyRaw().length).toBe(32);
    });
  });

  describe('sign and verify', () => {
    it('should sign messages and verify signatures', async () => {
      const identity = await Identity.generate();
      const message = new TextEncoder().encode('Hello, AgentMesh!');

      const signature = await identity.signB64(message);

      const isValid = await Identity.verifySignature(
        identity.signingPublicKeyB64,
        message,
        signature
      );

      expect(isValid).toBe(true);
    });

    it('should reject invalid signatures', async () => {
      const identity = await Identity.generate();
      const message = new TextEncoder().encode('Hello, AgentMesh!');
      const tamperedMessage = new TextEncoder().encode('Hello, Tampered!');

      const signature = await identity.signB64(message);

      const isValid = await Identity.verifySignature(
        identity.signingPublicKeyB64,
        tamperedMessage,
        signature
      );

      expect(isValid).toBe(false);
    });

    it('should reject signatures from wrong key', async () => {
      const identity1 = await Identity.generate();
      const identity2 = await Identity.generate();
      const message = new TextEncoder().encode('Hello, AgentMesh!');

      const signature = await identity1.signB64(message);

      const isValid = await Identity.verifySignature(
        identity2.signingPublicKeyB64, // Wrong key
        message,
        signature
      );

      expect(isValid).toBe(false);
    });

    it('should handle keys without prefix', async () => {
      const identity = await Identity.generate();
      const message = new TextEncoder().encode('Test message');

      const signature = await identity.signB64(message);

      // Verify with raw key (no prefix)
      const isValid = await Identity.verifySignature(
        identity.signingPublicKeyB64Raw,
        message,
        signature
      );

      expect(isValid).toBe(true);
    });
  });

  describe('signTimestamp', () => {
    it('should sign current timestamp', async () => {
      const identity = await Identity.generate();
      const before = new Date();

      const [timestamp, signature] = await identity.signTimestamp();

      const after = new Date();
      const timestampDate = new Date(timestamp);

      expect(timestampDate.getTime()).toBeGreaterThanOrEqual(before.getTime());
      expect(timestampDate.getTime()).toBeLessThanOrEqual(after.getTime());
      expect(signature).toBeDefined();

      // Verify the signature
      const isValid = await Identity.verifySignature(
        identity.signingPublicKeyB64,
        new TextEncoder().encode(timestamp),
        signature
      );
      expect(isValid).toBe(true);
    });
  });

  describe('save and load', () => {
    let storage: MemoryStorage;

    beforeEach(() => {
      storage = new MemoryStorage();
    });

    it('should save and load identity', async () => {
      const original = await Identity.generate();
      await original.save(storage, 'test-identity.json');

      const loaded = await Identity.load(storage, 'test-identity.json');

      expect(loaded.amid).toBe(original.amid);
      expect(loaded.framework).toBe(original.framework);
      expect(loaded.frameworkVersion).toBe(original.frameworkVersion);
    });

    it('should preserve signing capability after load', async () => {
      const original = await Identity.generate();
      const message = new TextEncoder().encode('Test message');

      await original.save(storage, 'test-identity.json');
      const loaded = await Identity.load(storage, 'test-identity.json');

      const signature = await loaded.signB64(message);

      // Verify with original identity's public key
      const isValid = await Identity.verifySignature(
        original.signingPublicKeyB64,
        message,
        signature
      );

      expect(isValid).toBe(true);
    });

    it('should throw when loading non-existent identity', async () => {
      await expect(Identity.load(storage, 'non-existent.json')).rejects.toThrow(
        'Identity not found'
      );
    });

    it('should save with key prefixes', async () => {
      const identity = await Identity.generate();
      await identity.save(storage, 'prefixed.json');

      const data = await storage.get('identity/prefixed.json');
      const json = JSON.parse(new TextDecoder().decode(data!));

      expect(json.signing_private_key).toMatch(/^ed25519:/);
      expect(json.signing_public_key).toMatch(/^ed25519:/);
      expect(json.exchange_private_key).toMatch(/^x25519:/);
      expect(json.exchange_public_key).toMatch(/^x25519:/);
    });
  });

  describe('toPublicInfo', () => {
    it('should export public information', async () => {
      const identity = await Identity.generate();
      const info = identity.toPublicInfo();

      expect(info.amid).toBe(identity.amid);
      expect(info.signing_public_key).toBe(identity.signingPublicKeyB64);
      expect(info.exchange_public_key).toBe(identity.exchangePublicKeyB64);
      expect(info.framework).toBe(identity.framework);
      expect(info.framework_version).toBe(identity.frameworkVersion);
    });

    it('should not include private keys', async () => {
      const identity = await Identity.generate();
      const info = identity.toPublicInfo();

      // Check that no private key properties exist
      expect(info).not.toHaveProperty('signing_private_key');
      expect(info).not.toHaveProperty('exchange_private_key');
    });
  });

  describe('rotateKeys', () => {
    it('should generate new identity with different AMID', async () => {
      const original = await Identity.generate();
      const rotated = await original.rotateKeys();

      expect(rotated.amid).not.toBe(original.amid);
      expect(rotated.signingPublicKeyB64).not.toBe(original.signingPublicKeyB64);
      expect(rotated.exchangePublicKeyB64).not.toBe(original.exchangePublicKeyB64);
    });
  });

  describe('toData / fromData', () => {
    it('should serialize and deserialize identity', async () => {
      const original = await Identity.generate();
      const data = await original.toData();

      const restored = await Identity.fromData(data);

      expect(restored.amid).toBe(original.amid);
      expect(restored.signingPublicKeyB64).toBe(original.signingPublicKeyB64);
      expect(restored.exchangePublicKeyB64).toBe(original.exchangePublicKeyB64);
    });

    it('should preserve created_at date', async () => {
      const original = await Identity.generate();
      const data = await original.toData();

      const restored = await Identity.fromData(data);

      // Dates should be equal (within 1 second for rounding)
      expect(Math.abs(restored.createdAt.getTime() - original.createdAt.getTime())).toBeLessThan(
        1000
      );
    });
  });
});

describe('AMID Derivation', () => {
  it('should produce deterministic AMID for same key', async () => {
    const identity = await Identity.generate();
    const data = await identity.toData();

    // Load same identity twice
    const loaded1 = await Identity.fromData(data);
    const loaded2 = await Identity.fromData(data);

    expect(loaded1.amid).toBe(loaded2.amid);
    expect(loaded1.amid).toBe(identity.amid);
  });

  it('should produce different AMID for different keys', async () => {
    const identity1 = await Identity.generate();
    const identity2 = await Identity.generate();

    expect(identity1.amid).not.toBe(identity2.amid);
  });
});
