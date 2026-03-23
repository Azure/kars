/**
 * Unit tests for Config module.
 */
import { describe, test, expect } from 'vitest';
import { Policy, Config, Tier, TierLevel, getTierLevel } from '../../src/config';

describe('Config', () => {
  describe('Tier', () => {
    test('should have correct tier levels', () => {
      expect(TierLevel[Tier.ANONYMOUS]).toBe(0);
      expect(TierLevel[Tier.VERIFIED]).toBe(1);
      expect(TierLevel[Tier.ORGANIZATION]).toBe(2);
    });

    test('getTierLevel should return correct level', () => {
      expect(getTierLevel(Tier.ANONYMOUS)).toBe(0);
      expect(getTierLevel(Tier.VERIFIED)).toBe(1);
      expect(getTierLevel(Tier.ORGANIZATION)).toBe(2);
      expect(getTierLevel('unknown')).toBe(0);
    });
  });

  describe('Policy', () => {
    test('should create policy with defaults', () => {
      const policy = new Policy();
      expect(policy.minTier).toBe(Tier.ANONYMOUS);
      expect(policy.minReputation).toBe(0.0);
      expect(policy.autoAccept).toBe(false);
      expect(policy.maxSessionTtl).toBe(300);
      expect(policy.maxConcurrentSessions).toBe(100);
    });

    test('should create policy with custom options', () => {
      const policy = new Policy({
        minTier: Tier.VERIFIED,
        minReputation: 0.5,
        autoAccept: true,
        maxSessionTtl: 600,
        allowedIntents: ['travel/*', 'weather/*'],
        blockedAmids: ['blocked-amid'],
      });

      expect(policy.minTier).toBe(Tier.VERIFIED);
      expect(policy.minReputation).toBe(0.5);
      expect(policy.autoAccept).toBe(true);
      expect(policy.maxSessionTtl).toBe(600);
      expect(policy.allowedIntents.has('travel/*')).toBe(true);
      expect(policy.blockedAmids.has('blocked-amid')).toBe(true);
    });

    test('evaluate should accept valid knock', () => {
      const policy = new Policy();
      const result = policy.evaluate({
        fromAmid: 'test-amid',
        fromTier: Tier.VERIFIED,
        fromReputation: 0.8,
        intentCategory: 'travel',
        requestedTtl: 100,
      });

      expect(result.allowed).toBe(true);
    });

    test('evaluate should reject blocked AMID', () => {
      const policy = new Policy({ blockedAmids: ['blocked-amid'] });
      const result = policy.evaluate({
        fromAmid: 'blocked-amid',
        fromTier: Tier.VERIFIED,
        fromReputation: 0.8,
        intentCategory: 'travel',
        requestedTtl: 100,
      });

      expect(result.allowed).toBe(false);
      expect(result.reason).toContain('blocked');
    });

    test('evaluate should reject low tier', () => {
      const policy = new Policy({ minTier: Tier.ORGANIZATION });
      const result = policy.evaluate({
        fromAmid: 'test-amid',
        fromTier: Tier.ANONYMOUS,
        fromReputation: 0.8,
        requestedTtl: 100,
      });

      expect(result.allowed).toBe(false);
      expect(result.reason).toContain('Tier');
    });

    test('evaluate should reject low reputation', () => {
      const policy = new Policy({ minReputation: 0.7 });
      const result = policy.evaluate({
        fromAmid: 'test-amid',
        fromTier: Tier.VERIFIED,
        fromReputation: 0.3,
        requestedTtl: 100,
      });

      expect(result.allowed).toBe(false);
      expect(result.reason).toContain('Reputation');
    });

    test('evaluate should reject excessive TTL', () => {
      const policy = new Policy({ maxSessionTtl: 60 });
      const result = policy.evaluate({
        fromAmid: 'test-amid',
        fromTier: Tier.VERIFIED,
        fromReputation: 0.8,
        requestedTtl: 300,
      });

      expect(result.allowed).toBe(false);
      expect(result.reason).toContain('TTL');
    });

    test('permissive policy should auto-accept', () => {
      const policy = Policy.permissive();
      expect(policy.autoAccept).toBe(true);
    });

    test('verified policy should require verified tier', () => {
      const policy = Policy.verified();
      expect(policy.minTier).toBe(Tier.VERIFIED);
      expect(policy.minReputation).toBe(0.5);
    });

    test('organization policy should require organization tier', () => {
      const policy = Policy.organization();
      expect(policy.minTier).toBe(Tier.ORGANIZATION);
      expect(policy.minReputation).toBe(0.7);
    });
  });

  describe('Config', () => {
    test('should create config with defaults', () => {
      const config = Config.default();
      expect(config.registryUrl).toContain('agentmesh');
      expect(config.relayUrl).toContain('agentmesh');
      expect(config.debug).toBe(false);
    });

    test('should create development config', () => {
      const config = Config.development();
      expect(config.debug).toBe(true);
      expect(config.policy.autoAccept).toBe(true);
    });

    test('should accept custom options', () => {
      const config = new Config({
        registryUrl: 'https://custom-registry.com',
        relayUrl: 'wss://custom-relay.com',
        displayName: 'Test Agent',
        capabilities: ['travel/*'],
        debug: true,
      });

      expect(config.registryUrl).toBe('https://custom-registry.com');
      expect(config.relayUrl).toBe('wss://custom-relay.com');
      expect(config.displayName).toBe('Test Agent');
      expect(config.capabilities).toContain('travel/*');
      expect(config.debug).toBe(true);
    });
  });
});
