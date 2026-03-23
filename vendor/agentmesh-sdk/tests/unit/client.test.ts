/**
 * Unit tests for AgentMeshClient module.
 */
import { describe, test, expect, beforeEach, vi, afterEach } from 'vitest';
import { AgentMeshClient } from '../../src/client';
import { MemoryStorage } from '../../src/storage';
import { Identity } from '../../src/identity';
import { Policy } from '../../src/config';

// Mock fetch globally
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Mock WebSocket
class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState = MockWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: ((error: unknown) => void) | null = null;

  constructor(public url: string) {
    setTimeout(() => {
      this.readyState = MockWebSocket.OPEN;
      this.onopen?.();
    }, 10);
  }

  send(data: string) {
    const parsed = JSON.parse(data);
    if (parsed.type === 'connect') {
      setTimeout(() => {
        this.onmessage?.({
          data: JSON.stringify({
            type: 'connected',
            session_id: 'test-session-123',
            pending_messages: 0,
          }),
        });
      }, 10);
    }
  }

  close() {
    this.readyState = MockWebSocket.CLOSED;
    setTimeout(() => this.onclose?.(), 10);
  }
}

vi.stubGlobal('WebSocket', MockWebSocket);

describe('AgentMeshClient', () => {
  let storage: MemoryStorage;

  beforeEach(() => {
    storage = new MemoryStorage();
    mockFetch.mockReset();

    // Default mock responses
    mockFetch.mockImplementation(async (url: string) => {
      if (url.includes('/registry/register')) {
        return { status: 201, json: async () => ({ success: true }) };
      }
      if (url.includes('/registry/status')) {
        return { status: 200 };
      }
      if (url.includes('/registry/prekeys') && !url.includes('amid=')) {
        return { status: 200 };
      }
      return { status: 404, json: async () => ({}) };
    });
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('create', () => {
    test('should create client with generated identity', async () => {
      const client = await AgentMeshClient.create({ storage });

      expect(client).toBeDefined();
      expect(client.amid).toBeDefined();
      expect(client.amid.length).toBeGreaterThan(10);
    });

    test('should create client with custom options', async () => {
      const client = await AgentMeshClient.create({
        storage,
        registryUrl: 'https://custom.registry.com',
        relayUrl: 'wss://custom.relay.com',
      });

      const info = client.getInfo();
      expect(info.registryUrl).toBe('https://custom.registry.com');
      expect(info.relayUrl).toBe('wss://custom.relay.com');
    });
  });

  describe('load', () => {
    test('should load client from storage', async () => {
      // First create and save
      const identity = await Identity.generate();
      await identity.save(storage, 'test-agent');

      // Then load
      const client = await AgentMeshClient.load(storage, 'test-agent');

      expect(client.amid).toBe(identity.amid);
    });

    test('should throw if identity not found', async () => {
      await expect(
        AgentMeshClient.load(storage, 'nonexistent')
      ).rejects.toThrow();
    });
  });

  describe('fromIdentity', () => {
    test('should create client from existing identity', async () => {
      const identity = await Identity.generate();
      const client = AgentMeshClient.fromIdentity(identity, { storage });

      expect(client.amid).toBe(identity.amid);
    });
  });

  describe('getIdentity', () => {
    test('should return the underlying identity', async () => {
      const identity = await Identity.generate();
      const client = AgentMeshClient.fromIdentity(identity, { storage });

      expect(client.getIdentity()).toBe(identity);
    });
  });

  describe('amid', () => {
    test('should return the AMID', async () => {
      const client = await AgentMeshClient.create({ storage });

      expect(client.amid).toBeDefined();
      expect(typeof client.amid).toBe('string');
    });
  });

  describe('connect', () => {
    test('should connect to network', async () => {
      const client = await AgentMeshClient.create({ storage });

      await client.connect({
        displayName: 'Test Agent',
        capabilities: ['test/*'],
      });

      expect(client.isConnected).toBe(true);
    });

    test('should throw if already connected', async () => {
      const client = await AgentMeshClient.create({ storage });

      await client.connect();

      await expect(client.connect()).rejects.toThrow('Already connected');
    });

    test('should set policy on connect', async () => {
      const client = await AgentMeshClient.create({ storage });
      const policy = Policy.verified();

      await client.connect({ policy });

      // Policy is set internally, verify via info
      expect(client.isConnected).toBe(true);
    });

    test('should emit connected event', async () => {
      const client = await AgentMeshClient.create({ storage });
      const handler = vi.fn();

      client.on('connected', handler);
      await client.connect();

      expect(handler).toHaveBeenCalledWith({ amid: client.amid });
    });
  });

  describe('disconnect', () => {
    test('should disconnect from network', async () => {
      const client = await AgentMeshClient.create({ storage });

      await client.connect();
      await client.disconnect();

      expect(client.isConnected).toBe(false);
    });

    test('should emit disconnected event', async () => {
      const client = await AgentMeshClient.create({ storage });
      const handler = vi.fn();

      client.on('disconnected', handler);
      await client.connect();
      await client.disconnect();

      expect(handler).toHaveBeenCalledWith({ amid: client.amid });
    });

    test('should handle disconnect when not connected', async () => {
      const client = await AgentMeshClient.create({ storage });

      // Should not throw
      await client.disconnect();

      expect(client.isConnected).toBe(false);
    });
  });

  describe('isConnected', () => {
    test('should return false when not connected', async () => {
      const client = await AgentMeshClient.create({ storage });

      expect(client.isConnected).toBe(false);
    });
  });

  describe('setPolicy', () => {
    test('should set policy', async () => {
      const client = await AgentMeshClient.create({ storage });
      const policy = Policy.organization();

      // Should not throw
      client.setPolicy(policy);
    });
  });

  describe('setCapabilities', () => {
    test('should update capabilities when connected', async () => {
      const client = await AgentMeshClient.create({ storage });

      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/capabilities')) {
          return { status: 200 };
        }
        if (url.includes('/registry/register')) {
          return { status: 201, json: async () => ({}) };
        }
        if (url.includes('/registry/prekeys')) {
          return { status: 200 };
        }
        return { status: 404 };
      });

      await client.connect();
      await client.setCapabilities(['weather/*', 'travel/*']);

      const info = client.getInfo();
      expect(info.capabilities).toEqual(['weather/*', 'travel/*']);
    });
  });

  describe('getSessions', () => {
    test('should return empty array when no sessions', async () => {
      const client = await AgentMeshClient.create({ storage });

      const sessions = client.getSessions();

      expect(sessions).toEqual([]);
    });
  });

  describe('getSession', () => {
    test('should return undefined when session not found', async () => {
      const client = await AgentMeshClient.create({ storage });

      const session = client.getSession('unknown-amid');

      expect(session).toBeUndefined();
    });
  });

  describe('getInfo', () => {
    test('should return client info', async () => {
      const client = await AgentMeshClient.create({ storage });

      const info = client.getInfo();

      expect(info.amid).toBe(client.amid);
      expect(info.connected).toBe(false);
      expect(info.capabilities).toEqual([]);
      expect(info.activeSessions).toBe(0);
    });

    test('should reflect connection state', async () => {
      const client = await AgentMeshClient.create({ storage });

      await client.connect({ capabilities: ['test/*'] });
      const info = client.getInfo();

      expect(info.connected).toBe(true);
      expect(info.capabilities).toEqual(['test/*']);
    });
  });

  describe('onMessage', () => {
    test('should register message handler', async () => {
      const client = await AgentMeshClient.create({ storage });
      const handler = vi.fn();

      client.onMessage(handler);

      // Handler is registered (internal state)
    });
  });

  describe('onKnock', () => {
    test('should register KNOCK handler', async () => {
      const client = await AgentMeshClient.create({ storage });
      const handler = vi.fn().mockResolvedValue({ accept: true });

      client.onKnock(handler);

      // Handler is registered (internal state)
    });
  });

  describe('on / off', () => {
    test('should register and remove event handlers', async () => {
      const client = await AgentMeshClient.create({ storage });
      const connectedHandler = vi.fn();
      const disconnectedHandler = vi.fn();

      // Register handlers
      client.on('connected', connectedHandler);
      client.on('disconnected', disconnectedHandler);

      await client.connect();
      expect(connectedHandler).toHaveBeenCalledWith({ amid: client.amid });

      // Remove the disconnected handler
      client.off('disconnected', disconnectedHandler);

      await client.disconnect();

      // Disconnected handler was removed, so it should not be called
      expect(disconnectedHandler).not.toHaveBeenCalled();
    });
  });

  describe('save', () => {
    test('should save identity to storage', async () => {
      const client = await AgentMeshClient.create({ storage });

      await client.save('saved-agent');

      // Verify by loading
      const loaded = await AgentMeshClient.load(storage, 'saved-agent');
      expect(loaded.amid).toBe(client.amid);
    });
  });

  describe('uploadPrekeys', () => {
    test('should upload prekeys to registry', async () => {
      const client = await AgentMeshClient.create({ storage });

      let prekeyUploadCalled = false;
      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/prekeys') && !url.includes('amid=')) {
          prekeyUploadCalled = true;
          return { status: 200 };
        }
        return { status: 200, json: async () => ({}) };
      });

      await client.uploadPrekeys();

      expect(prekeyUploadCalled).toBe(true);
    });
  });

  describe('rotatePrekeys', () => {
    test('should rotate and upload prekeys', async () => {
      const client = await AgentMeshClient.create({ storage });

      let prekeyUploadCount = 0;
      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/prekeys') && !url.includes('amid=')) {
          prekeyUploadCount++;
          return { status: 200 };
        }
        return { status: 200, json: async () => ({}) };
      });

      await client.rotatePrekeys();

      expect(prekeyUploadCount).toBe(1);
    });
  });

  describe('search', () => {
    test('should search for agents', async () => {
      const client = await AgentMeshClient.create({ storage });

      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/search')) {
          return {
            status: 200,
            json: async () => ({
              results: [
                {
                  amid: 'agent-1',
                  tier: 'verified',
                  signing_public_key: 'key1',
                  exchange_public_key: 'key2',
                  capabilities: ['weather/*'],
                  relay_endpoint: 'wss://relay.test.com',
                  status: 'online',
                  reputation_score: 0.9,
                  last_seen: '2025-01-01T00:00:00Z',
                },
              ],
              total: 1,
            }),
          };
        }
        return { status: 200, json: async () => ({}) };
      });

      const results = await client.search('weather/*');

      expect(results.length).toBe(1);
      expect(results[0]?.amid).toBe('agent-1');
    });
  });

  describe('lookup', () => {
    test('should lookup agent by AMID', async () => {
      const client = await AgentMeshClient.create({ storage });

      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/lookup')) {
          return {
            status: 200,
            json: async () => ({
              amid: 'target-amid',
              tier: 'verified',
              signing_public_key: 'key1',
              exchange_public_key: 'key2',
              capabilities: ['test/*'],
              relay_endpoint: 'wss://relay.test.com',
              status: 'online',
              reputation_score: 0.8,
              last_seen: '2025-01-01T00:00:00Z',
            }),
          };
        }
        return { status: 200, json: async () => ({}) };
      });

      const info = await client.lookup('target-amid');

      expect(info?.amid).toBe('target-amid');
    });

    test('should return null for unknown agent', async () => {
      const client = await AgentMeshClient.create({ storage });

      mockFetch.mockImplementation(async (url: string) => {
        if (url.includes('/registry/lookup')) {
          return { status: 404 };
        }
        return { status: 200, json: async () => ({}) };
      });

      const info = await client.lookup('unknown-amid');

      expect(info).toBeNull();
    });
  });

  describe('closeSession', () => {
    test('should close session for peer', async () => {
      const client = await AgentMeshClient.create({ storage });
      const handler = vi.fn();

      client.on('session_closed', handler);

      await client.closeSession('peer-amid');

      expect(handler).toHaveBeenCalledWith({ amid: 'peer-amid' });
    });
  });

  describe('Circuit Breakers', () => {
    describe('getCircuitState', () => {
      test('should return RUNNING by default', async () => {
        const client = await AgentMeshClient.create({ storage });
        const state = client.getCircuitState();

        expect(state.state).toBe('RUNNING');
        expect(state.changedAt).toBeGreaterThan(0);
      });
    });

    describe('pauseNew/resumeNew', () => {
      test('should pause and emit event', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        client.on('circuit_paused', handler);
        client.pauseNew();

        expect(client.getCircuitState().state).toBe('PAUSED');
        expect(handler).toHaveBeenCalled();
      });

      test('should resume and emit event', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        client.pauseNew();
        client.on('circuit_resumed', handler);
        client.resumeNew();

        expect(client.getCircuitState().state).toBe('RUNNING');
        expect(handler).toHaveBeenCalled();
      });

      test('should be idempotent', async () => {
        const client = await AgentMeshClient.create({ storage });

        client.pauseNew();
        client.pauseNew(); // Should not throw

        expect(client.getCircuitState().state).toBe('PAUSED');
      });
    });

    describe('block/unblock', () => {
      test('should block a peer', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        client.on('peer_blocked', handler);
        await client.block('bad-peer');

        expect(client.isBlocked('bad-peer')).toBe(true);
        expect(handler).toHaveBeenCalledWith({ amid: 'bad-peer' });
      });

      test('should unblock a peer', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        await client.block('bad-peer');
        client.on('peer_unblocked', handler);
        await client.unblock('bad-peer');

        expect(client.isBlocked('bad-peer')).toBe(false);
        expect(handler).toHaveBeenCalledWith({ amid: 'bad-peer' });
      });
    });

    describe('killSession', () => {
      test('should emit session_killed event', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        client.on('session_killed', handler);
        await client.killSession('peer-amid');

        expect(handler).toHaveBeenCalledWith({ amid: 'peer-amid' });
      });
    });

    describe('emergencyStop', () => {
      test('should transition to STOPPED state', async () => {
        const client = await AgentMeshClient.create({ storage });
        const handler = vi.fn();

        client.on('emergency_stop', handler);
        await client.emergencyStop();

        expect(client.getCircuitState().state).toBe('STOPPED');
        expect(handler).toHaveBeenCalled();
      });

      test('should prevent further operations', async () => {
        const client = await AgentMeshClient.create({ storage });

        await client.emergencyStop();

        expect(() => client.pauseNew()).toThrow('Client is stopped');
        expect(() => client.resumeNew()).toThrow('Client is stopped');
      });
    });

    describe('getInfo', () => {
      test('should include circuit state', async () => {
        const client = await AgentMeshClient.create({ storage });

        const info = client.getInfo();

        expect(info.circuitState).toBe('RUNNING');
        expect(info.circuitStateChangedAt).toBeGreaterThan(0);
      });
    });
  });
});
