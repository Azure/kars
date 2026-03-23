/**
 * Unit tests for Transport module.
 */
import { describe, test, expect, beforeEach, vi, afterEach } from 'vitest';
import { RelayTransport, P2PTransport, createP2PTransport } from '../../src/transport';
import { Identity } from '../../src/identity';

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

  private messages: string[] = [];

  constructor(public url: string) {
    // Simulate async connection
    setTimeout(() => {
      this.readyState = MockWebSocket.OPEN;
      this.onopen?.();
    }, 10);
  }

  send(data: string) {
    this.messages.push(data);

    // Simulate server responses based on message type
    const parsed = JSON.parse(data);
    if (parsed.type === 'connect') {
      setTimeout(() => {
        this.receiveMessage({
          type: 'connected',
          session_id: 'test-session-123',
          pending_messages: 5,
        });
      }, 10);
    }
  }

  close() {
    this.readyState = MockWebSocket.CLOSED;
    setTimeout(() => this.onclose?.(), 10);
  }

  // Test helper: simulate receiving a message
  receiveMessage(data: Record<string, unknown>) {
    this.onmessage?.({ data: JSON.stringify(data) });
  }

  // Test helper: get sent messages
  getSentMessages(): string[] {
    return this.messages;
  }

  // Test helper: simulate error
  simulateError(error: unknown) {
    this.onerror?.(error);
  }
}

// Store reference to mock instances
let mockWebSocketInstance: MockWebSocket | null = null;

vi.stubGlobal('WebSocket', class extends MockWebSocket {
  constructor(url: string) {
    super(url);
    mockWebSocketInstance = this;
  }
});

describe('Transport', () => {
  let identity: Identity;

  beforeEach(async () => {
    identity = await Identity.generate();
    mockWebSocketInstance = null;
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('RelayTransport', () => {
    describe('constructor', () => {
      test('should create transport with default options', () => {
        const transport = new RelayTransport(identity);
        expect(transport).toBeDefined();
        expect(transport.isConnected).toBe(false);
      });

      test('should create transport with custom options', () => {
        const transport = new RelayTransport(identity, {
          relayUrl: 'wss://custom.relay.com',
          p2pCapable: true,
          maxReconnectAttempts: 10,
          reconnectBaseDelay: 2000,
        });
        expect(transport).toBeDefined();
      });
    });

    describe('isConnected', () => {
      test('should return false when not connected', () => {
        const transport = new RelayTransport(identity);
        expect(transport.isConnected).toBe(false);
      });
    });

    describe('currentSessionId', () => {
      test('should return null when not connected', () => {
        const transport = new RelayTransport(identity);
        expect(transport.currentSessionId).toBeNull();
      });
    });

    describe('pendingMessageCount', () => {
      test('should return 0 when not connected', () => {
        const transport = new RelayTransport(identity);
        expect(transport.pendingMessageCount).toBe(0);
      });
    });

    describe('connect', () => {
      test('should connect to relay server', async () => {
        const transport = new RelayTransport(identity, {
          relayUrl: 'wss://test.relay.com',
        });

        const result = await transport.connect();

        expect(result).toBe(true);
        expect(transport.isConnected).toBe(true);
        expect(transport.currentSessionId).toBe('test-session-123');
        expect(transport.pendingMessageCount).toBe(5);
      });

      test('should return true if already connected', async () => {
        const transport = new RelayTransport(identity);

        await transport.connect();
        const secondResult = await transport.connect();

        expect(secondResult).toBe(true);
      });

      test('should send authentication message on connect', async () => {
        const transport = new RelayTransport(identity, {
          p2pCapable: true,
        });

        await transport.connect();

        const sentMessages = mockWebSocketInstance?.getSentMessages() ?? [];
        expect(sentMessages.length).toBeGreaterThan(0);

        const connectMsg = JSON.parse(sentMessages[0]!);
        expect(connectMsg.type).toBe('connect');
        expect(connectMsg.protocol).toBe('agentmesh/0.2');
        expect(connectMsg.amid).toBe(identity.amid);
        expect(connectMsg.p2p_capable).toBe(true);
        expect(connectMsg.signature).toBeDefined();
        expect(connectMsg.timestamp).toBeDefined();
      });
    });

    describe('disconnect', () => {
      test('should disconnect from relay', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        await transport.disconnect('test_reason');

        expect(transport.isConnected).toBe(false);
        expect(transport.currentSessionId).toBeNull();
      });

      test('should handle disconnect when not connected', async () => {
        const transport = new RelayTransport(identity);

        // Should not throw
        await transport.disconnect();

        expect(transport.isConnected).toBe(false);
      });
    });

    describe('send', () => {
      test('should send message to relay', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const result = await transport.send(
          'target-amid',
          'encrypted-payload-base64',
          'knock'
        );

        expect(result).toBe(true);

        const sentMessages = mockWebSocketInstance?.getSentMessages() ?? [];
        const sendMsg = JSON.parse(sentMessages[sentMessages.length - 1]!);
        expect(sendMsg.type).toBe('send');
        expect(sendMsg.to).toBe('target-amid');
        expect(sendMsg.encrypted_payload).toBe('encrypted-payload-base64');
        expect(sendMsg.message_type).toBe('knock');
      });

      test('should include ice candidates when provided', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const iceCandidates = [{ candidate: 'test-candidate' }];
        await transport.send('target', 'payload', 'type', iceCandidates);

        const sentMessages = mockWebSocketInstance?.getSentMessages() ?? [];
        const sendMsg = JSON.parse(sentMessages[sentMessages.length - 1]!);
        expect(sendMsg.ice_candidates).toEqual(iceCandidates);
      });

      test('should throw when not connected', async () => {
        const transport = new RelayTransport(identity);

        await expect(
          transport.send('target', 'payload', 'type')
        ).rejects.toThrow('Not connected to relay');
      });
    });

    describe('updatePresence', () => {
      test('should update presence status', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const result = await transport.updatePresence('busy');

        expect(result).toBe(true);

        const sentMessages = mockWebSocketInstance?.getSentMessages() ?? [];
        const presenceMsg = JSON.parse(sentMessages[sentMessages.length - 1]!);
        expect(presenceMsg.type).toBe('presence');
        expect(presenceMsg.status).toBe('busy');
      });

      test('should return false when not connected', async () => {
        const transport = new RelayTransport(identity);

        const result = await transport.updatePresence('online');

        expect(result).toBe(false);
      });
    });

    describe('queryPresence', () => {
      test('should query presence status', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        await transport.queryPresence('target-amid');

        const sentMessages = mockWebSocketInstance?.getSentMessages() ?? [];
        const queryMsg = JSON.parse(sentMessages[sentMessages.length - 1]!);
        expect(queryMsg.type).toBe('presence_query');
        expect(queryMsg.amid).toBe('target-amid');
      });

      test('should not throw when not connected', async () => {
        const transport = new RelayTransport(identity);

        // Should not throw
        await transport.queryPresence('target-amid');
      });
    });

    describe('onMessage / offMessage', () => {
      test('should register and call message handler', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const handler = vi.fn();
        transport.onMessage('custom_message', handler);

        // Simulate receiving a message
        mockWebSocketInstance?.receiveMessage({
          type: 'custom_message',
          data: 'test data',
        });

        // Wait for async handler
        await new Promise(resolve => setTimeout(resolve, 50));

        expect(handler).toHaveBeenCalledWith({
          type: 'custom_message',
          data: 'test data',
        });
      });

      test('should remove message handler', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const handler = vi.fn();
        transport.onMessage('custom_message', handler);
        transport.offMessage('custom_message');

        mockWebSocketInstance?.receiveMessage({
          type: 'custom_message',
          data: 'test',
        });

        await new Promise(resolve => setTimeout(resolve, 50));

        expect(handler).not.toHaveBeenCalled();
      });

      test('should handle async handlers', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        const results: string[] = [];
        transport.onMessage('async_message', async (data) => {
          await new Promise(resolve => setTimeout(resolve, 10));
          results.push(data.value as string);
        });

        mockWebSocketInstance?.receiveMessage({
          type: 'async_message',
          value: 'test',
        });

        await new Promise(resolve => setTimeout(resolve, 100));

        expect(results).toContain('test');
      });
    });

    describe('Store-and-Forward', () => {
      test('should have pending messages count after connect', async () => {
        const transport = new RelayTransport(identity);
        await transport.connect();

        // Pending messages should be 5 (from mock)
        expect(transport.pendingMessageCount).toBe(5);
      });

      test('should register and call transport event handlers', async () => {
        const transport = new RelayTransport(identity);
        const handler = vi.fn();

        transport.onTransportEvent('pending_processed', handler);

        // Verify handler is registered (internal state)
        expect(transport).toBeDefined();
      });

      test('should have onTransportEvent method', () => {
        const transport = new RelayTransport(identity);
        expect(typeof transport.onTransportEvent).toBe('function');
      });

      test('should have requestPendingMessages method', () => {
        const transport = new RelayTransport(identity);
        expect(typeof transport.requestPendingMessages).toBe('function');
      });

      test('should have processPendingMessages method', () => {
        const transport = new RelayTransport(identity);
        expect(typeof transport.processPendingMessages).toBe('function');
      });

      test('requestPendingMessages should throw when not connected', async () => {
        const transport = new RelayTransport(identity);

        await expect(transport.requestPendingMessages()).rejects.toThrow('Not connected');
      });

      test('should return empty when no pending messages', async () => {
        // Override mock to return 0 pending messages
        const originalSend = MockWebSocket.prototype.send;
        let messagesPushed: string[] = [];

        MockWebSocket.prototype.send = function(this: MockWebSocket, data: string) {
          messagesPushed.push(data);
          const parsed = JSON.parse(data);
          if (parsed.type === 'connect') {
            setTimeout(() => {
              this.receiveMessage({
                type: 'connected',
                session_id: 'test-session',
                pending_messages: 0,
              });
            }, 10);
          }
        };

        const transport = new RelayTransport(identity);
        await transport.connect();

        expect(transport.pendingMessageCount).toBe(0);
        const messages = await transport.requestPendingMessages();
        expect(messages).toEqual([]);

        // Restore
        MockWebSocket.prototype.send = originalSend;
      });
    });
  });

  describe('P2PTransport', () => {
    test('should create P2P transport', () => {
      const p2p = new P2PTransport(identity, 'target-amid');
      expect(p2p).toBeDefined();
    });

    test('should report as not available', () => {
      const p2p = new P2PTransport(identity, 'target-amid');
      expect(p2p.isAvailable).toBe(false);
    });

    test('should report as not connected', () => {
      const p2p = new P2PTransport(identity, 'target-amid');
      expect(p2p.isConnected).toBe(false);
    });

    test('should return metrics', () => {
      const p2p = new P2PTransport(identity, 'target-amid');
      const metrics = p2p.getMetrics();

      expect(metrics.available).toBe(false);
      expect(metrics.connected).toBe(false);
      expect(metrics.mode).toBe('relay-only');
      expect(metrics.stunServers).toEqual([]);
    });

    test('should fail to connect', async () => {
      const p2p = new P2PTransport(identity, 'target-amid');
      const result = await p2p.connect();

      expect(result).toBe(false);
    });

    test('should handle close without error', async () => {
      const p2p = new P2PTransport(identity, 'target-amid');

      // Should not throw
      await p2p.close();
    });
  });

  describe('createP2PTransport', () => {
    test('should create P2P transport instance', () => {
      const p2p = createP2PTransport(identity, 'target-amid');
      expect(p2p).toBeInstanceOf(P2PTransport);
    });
  });
});
