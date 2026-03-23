/**
 * Unit tests for Discovery/Registry Client module.
 */
import { describe, test, expect, beforeEach, vi, afterEach } from 'vitest';
import { RegistryClient } from '../../src/discovery';
import { Identity } from '../../src/identity';

// Mock fetch globally
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

describe('RegistryClient', () => {
  let client: RegistryClient;
  let identity: Identity;

  beforeEach(async () => {
    client = new RegistryClient('https://api.test.com/v1', 5000);
    identity = await Identity.generate();
    mockFetch.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('constructor', () => {
    test('should create client with default URL', () => {
      const defaultClient = new RegistryClient();
      expect(defaultClient).toBeDefined();
    });

    test('should strip trailing slash from base URL', () => {
      const clientWithSlash = new RegistryClient('https://api.test.com/v1/');
      expect(clientWithSlash).toBeDefined();
    });
  });

  describe('register', () => {
    test('should register agent successfully', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 201,
        json: async () => ({ agent_id: identity.amid }),
      });

      const result = await client.register(identity, {
        displayName: 'Test Agent',
        capabilities: ['weather/forecast'],
      });

      expect(result.success).toBe(true);
      expect(mockFetch).toHaveBeenCalledWith(
        'https://api.test.com/v1/registry/register',
        expect.objectContaining({
          method: 'POST',
          headers: expect.objectContaining({
            'Content-Type': 'application/json',
          }),
        })
      );
    });

    test('should handle already registered response', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 409,
        json: async () => ({ message: 'Already registered' }),
      });

      const result = await client.register(identity);

      expect(result.success).toBe(true);
      expect(result.alreadyRegistered).toBe(true);
    });

    test('should handle registration error', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 400,
        json: async () => ({ error: 'Invalid request' }),
      });

      const result = await client.register(identity);

      expect(result.success).toBe(false);
      expect(result.error).toBe('Invalid request');
    });

    test('should throw NetworkError on connection failure', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Connection refused'));

      await expect(client.register(identity)).rejects.toThrow('Registry connection error');
    });
  });

  describe('lookup', () => {
    test('should lookup agent by AMID', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          amid: 'test-amid',
          tier: 'verified',
          signing_public_key: 'abc123',
          exchange_public_key: 'def456',
          capabilities: ['weather/forecast'],
          relay_endpoint: 'wss://relay.test.com',
          status: 'online',
          reputation_score: 0.8,
          last_seen: '2025-01-01T00:00:00Z',
        }),
      });

      const info = await client.lookup('test-amid');

      expect(info).toBeDefined();
      expect(info?.amid).toBe('test-amid');
      expect(info?.tier).toBe('verified');
      expect(info?.capabilities).toContain('weather/forecast');
    });

    test('should return null for not found', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 404,
        json: async () => ({}),
      });

      const info = await client.lookup('unknown-amid');

      expect(info).toBeNull();
    });

    test('should throw NetworkError on server error', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 500,
        json: async () => ({}),
      });

      await expect(client.lookup('test-amid')).rejects.toThrow('Lookup failed');
    });
  });

  describe('search', () => {
    test('should search agents by capability', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          results: [
            {
              amid: 'agent-1',
              tier: 'verified',
              signing_public_key: 'key1',
              exchange_public_key: 'key2',
              capabilities: ['weather/forecast'],
              relay_endpoint: 'wss://relay.test.com',
              status: 'online',
              reputation_score: 0.9,
              last_seen: '2025-01-01T00:00:00Z',
            },
          ],
          total: 1,
        }),
      });

      const result = await client.search({ capability: 'weather/*' });

      expect(result.results.length).toBe(1);
      expect(result.total).toBe(1);
      expect(result.results[0]?.amid).toBe('agent-1');
    });

    test('should include search filters in query params', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({ results: [], total: 0 }),
      });

      await client.search({
        capability: 'weather/*',
        tierMin: 2,
        reputationMin: 0.5,
        status: 'online',
        limit: 10,
        offset: 5,
      });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('capability=weather'),
        expect.any(Object)
      );
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('tier_min=2'),
        expect.any(Object)
      );
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('reputation_min=0.5'),
        expect.any(Object)
      );
    });
  });

  describe('updateStatus', () => {
    test('should update agent status', async () => {
      mockFetch.mockResolvedValueOnce({ status: 200 });

      const result = await client.updateStatus(identity, 'online');

      expect(result).toBe(true);
    });

    test('should return false on failure', async () => {
      mockFetch.mockResolvedValueOnce({ status: 400 });

      const result = await client.updateStatus(identity, 'online');

      expect(result).toBe(false);
    });

    test('should return false on network error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const result = await client.updateStatus(identity, 'online');

      expect(result).toBe(false);
    });
  });

  describe('updateCapabilities', () => {
    test('should update agent capabilities', async () => {
      mockFetch.mockResolvedValueOnce({ status: 200 });

      const result = await client.updateCapabilities(identity, ['weather/forecast', 'travel/flights']);

      expect(result).toBe(true);
    });

    test('should return false on failure', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const result = await client.updateCapabilities(identity, ['test/*']);

      expect(result).toBe(false);
    });
  });

  describe('uploadPrekeys', () => {
    test('should upload prekeys successfully', async () => {
      mockFetch.mockResolvedValueOnce({ status: 200 });

      const result = await client.uploadPrekeys(
        identity,
        'signedPrekeyBase64',
        'signatureBase64',
        1,
        [{ id: 1, key: 'otpKey1' }, { id: 2, key: 'otpKey2' }]
      );

      expect(result).toBe(true);
    });

    test('should return false on failure', async () => {
      mockFetch.mockResolvedValueOnce({ status: 400 });

      const result = await client.uploadPrekeys(identity, 'key', 'sig', 1, []);

      expect(result).toBe(false);
    });
  });

  describe('getPrekeys', () => {
    test('should fetch prekeys for agent', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          identity_key: 'identityKeyBase64',
          signed_prekey: 'signedPrekeyBase64',
          signed_prekey_signature: 'signatureBase64',
          signed_prekey_id: 1,
          one_time_prekeys: [{ id: 1, key: 'otpKey1' }],
        }),
      });

      const bundle = await client.getPrekeys('target-amid');

      expect(bundle).toBeDefined();
      expect(bundle?.identityKey).toBe('identityKeyBase64');
      expect(bundle?.signedPrekey).toBe('signedPrekeyBase64');
      expect(bundle?.oneTimePrekeys.length).toBe(1);
    });

    test('should return null for not found', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 404,
        json: async () => ({}),
      });

      const bundle = await client.getPrekeys('unknown-amid');

      expect(bundle).toBeNull();
    });

    test('should return null on network error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const bundle = await client.getPrekeys('target-amid');

      expect(bundle).toBeNull();
    });
  });

  describe('getOAuthProviders', () => {
    test('should fetch OAuth providers', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          providers: [
            { name: 'github', displayName: 'GitHub' },
            { name: 'google', displayName: 'Google' },
          ],
        }),
      });

      const providers = await client.getOAuthProviders();

      expect(providers.length).toBe(2);
      expect(providers[0]?.name).toBe('github');
    });

    test('should return empty array on error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const providers = await client.getOAuthProviders();

      expect(providers).toEqual([]);
    });
  });

  describe('startOAuthVerification', () => {
    test('should start OAuth flow', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          authorization_url: 'https://github.com/oauth/authorize?...',
          state: 'random-state-123',
        }),
      });

      const result = await client.startOAuthVerification(identity, 'github');

      expect(result).toBeDefined();
      expect(result?.authorizationUrl).toContain('github.com');
      expect(result?.state).toBe('random-state-123');
    });

    test('should return null on failure', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 400,
        json: async () => ({ error: 'Invalid provider' }),
      });

      const result = await client.startOAuthVerification(identity, 'invalid');

      expect(result).toBeNull();
    });
  });

  describe('getVerificationStatus', () => {
    test('should return verification status', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          amid: 'test-amid',
          tier: 'verified',
          signing_public_key: 'key1',
          exchange_public_key: 'key2',
          capabilities: [],
          relay_endpoint: 'wss://relay.test.com',
          status: 'online',
          reputation_score: 0.8,
          last_seen: '2025-01-01T00:00:00Z',
        }),
      });

      const status = await client.getVerificationStatus('test-amid');

      expect(status?.tier).toBe('verified');
      expect(status?.isVerified).toBe(true);
    });

    test('should return null for unknown agent', async () => {
      mockFetch.mockResolvedValueOnce({ status: 404 });

      const status = await client.getVerificationStatus('unknown-amid');

      expect(status).toBeNull();
    });
  });

  describe('checkRevocation', () => {
    test('should check revocation status', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({ revoked: false }),
      });

      const result = await client.checkRevocation('test-amid');

      expect(result.revoked).toBe(false);
    });

    test('should return revoked status with reason', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({ revoked: true, reason: 'Key compromise' }),
      });

      const result = await client.checkRevocation('compromised-amid');

      expect(result.revoked).toBe(true);
      expect(result.reason).toBe('Key compromise');
    });

    test('should return not revoked on error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const result = await client.checkRevocation('test-amid');

      expect(result.revoked).toBe(false);
    });
  });

  describe('bulkCheckRevocation', () => {
    test('should check multiple agents', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({
          revocations: {
            'amid-1': { revoked: false },
            'amid-2': { revoked: true, reason: 'Expired' },
          },
        }),
      });

      const result = await client.bulkCheckRevocation(['amid-1', 'amid-2']);

      expect(result['amid-1']?.revoked).toBe(false);
      expect(result['amid-2']?.revoked).toBe(true);
    });

    test('should return empty object on error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const result = await client.bulkCheckRevocation(['amid-1']);

      expect(result).toEqual({});
    });
  });

  describe('healthCheck', () => {
    test('should return healthy status', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 200,
        json: async () => ({ status: 'healthy', agentCount: 100 }),
      });

      const result = await client.healthCheck();

      expect(result.status).toBe('healthy');
      expect(result.agentCount).toBe(100);
    });

    test('should return unhealthy on server error', async () => {
      mockFetch.mockResolvedValueOnce({
        status: 500,
        json: async () => ({}),
      });

      const result = await client.healthCheck();

      expect(result.status).toBe('unhealthy');
    });

    test('should return unreachable on network error', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network error'));

      const result = await client.healthCheck();

      expect(result.status).toBe('unreachable');
    });
  });

  describe('submitReputation', () => {
    test('should submit reputation score', async () => {
      mockFetch.mockResolvedValueOnce({ status: 200 });

      const result = await client.submitReputation(
        identity,
        'target-amid',
        'session-123',
        0.9,
        ['helpful', 'fast']
      );

      expect(result).toBe(true);
    });

    test('should reject invalid score', async () => {
      await expect(
        client.submitReputation(identity, 'target', 'session', 1.5)
      ).rejects.toThrow('Score must be between 0.0 and 1.0');

      await expect(
        client.submitReputation(identity, 'target', 'session', -0.1)
      ).rejects.toThrow('Score must be between 0.0 and 1.0');
    });

    test('should return false on failure', async () => {
      mockFetch.mockResolvedValueOnce({ status: 400 });

      const result = await client.submitReputation(identity, 'target', 'session', 0.5);

      expect(result).toBe(false);
    });
  });
});
