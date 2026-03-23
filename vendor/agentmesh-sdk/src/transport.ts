/**
 * Transport layer for AgentMesh.
 * Handles WebSocket relay connections and P2P stubs.
 */

import { Identity } from './identity';
import { NetworkError } from './errors';

/**
 * Message envelope for relay communication.
 */
export interface RelayMessage {
  type: string;
  [key: string]: unknown;
}

/**
 * Options for transport configuration.
 */
export interface TransportOptions {
  /** Relay WebSocket URL */
  relayUrl?: string;
  /** Enable P2P capability flag */
  p2pCapable?: boolean;
  /** Reconnection attempts */
  maxReconnectAttempts?: number;
  /** Reconnection base delay in ms */
  reconnectBaseDelay?: number;
}

type MessageHandler = (data: Record<string, unknown>) => void | Promise<void>;
type TransportEventHandler<T = unknown> = (event: T) => void;

/**
 * Pending message from store-and-forward queue.
 */
export interface PendingMessage {
  id: string;
  from: string;
  encrypted_payload: string;
  message_type: string;
  timestamp: number;
}

/**
 * Event types for transport.
 */
export type TransportEventType =
  | 'pending_processed'
  | 'pending_failed'
  | 'optimistic_dropped';

/**
 * WebSocket transport via the AgentMesh relay server.
 */
export class RelayTransport {
  private readonly identity: Identity;
  private readonly relayUrl: string;
  private readonly p2pCapable: boolean;
  private readonly maxReconnectAttempts: number;
  private readonly reconnectBaseDelay: number;

  private ws: WebSocket | null = null;
  private sessionId: string | null = null;
  private connected = false;
  private messageHandlers: Map<string, MessageHandler> = new Map();
  private eventHandlers: Map<TransportEventType, TransportEventHandler[]> = new Map();
  private pendingMessages = 0;
  private reconnectAttempts = 0;
  private reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
  private pendingMessageQueue: PendingMessage[] = [];

  constructor(identity: Identity, options: TransportOptions = {}) {
    this.identity = identity;
    this.relayUrl = options.relayUrl ?? 'wss://relay.agentmesh.online/v1/connect';
    this.p2pCapable = options.p2pCapable ?? false;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 5;
    this.reconnectBaseDelay = options.reconnectBaseDelay ?? 1000;
  }

  /**
   * Check if connected to relay.
   */
  get isConnected(): boolean {
    return this.connected && this.ws !== null && this.ws.readyState === WebSocket.OPEN;
  }

  /**
   * Get current session ID.
   */
  get currentSessionId(): string | null {
    return this.sessionId;
  }

  /**
   * Get count of pending messages on connect.
   */
  get pendingMessageCount(): number {
    return this.pendingMessages;
  }

  /**
   * Connect to the relay server.
   */
  async connect(): Promise<boolean> {
    if (this.isConnected) {
      return true;
    }

    return new Promise((resolve) => {
      try {
        this.ws = new WebSocket(this.relayUrl);

        this.ws.onopen = async () => {
          // Send authentication
          const [timestamp, signature] = await this.identity.signTimestamp();

          const connectMsg: RelayMessage = {
            type: 'connect',
            protocol: 'agentmesh/0.2',
            amid: this.identity.amid,
            public_key: this.identity.signingPublicKeyB64Raw,
            signature,
            timestamp,
            p2p_capable: this.p2pCapable,
          };

          this.ws!.send(JSON.stringify(connectMsg));
        };

        this.ws.onmessage = (event) => {
          try {
            const data = JSON.parse(event.data as string) as Record<string, unknown>;
            const msgType = data.type as string;

            if (msgType === 'connected') {
              this.sessionId = data.session_id as string;
              this.pendingMessages = (data.pending_messages as number) ?? 0;
              this.connected = true;
              this.reconnectAttempts = 0;

              // Request pending messages if any exist
              if (this.pendingMessages > 0) {
                this.requestPendingMessages().catch((err) => {
                  console.error('Failed to request pending messages:', err);
                });
              }

              resolve(true);
            } else if (msgType === 'pending_messages') {
              // Handle pending messages response from relay
              const messages = data.messages as PendingMessage[] ?? [];
              this.pendingMessageQueue = messages;
            } else if (msgType === 'error') {
              console.error('Relay error:', data.error);
              this.connected = false;
              resolve(false);
            } else {
              // Dispatch to handler
              const handler = this.messageHandlers.get(msgType);
              if (handler) {
                Promise.resolve(handler(data)).catch((err) => {
                  console.error(`Handler error for ${msgType}:`, err);
                });
              }
            }
          } catch (err) {
            console.error('Failed to parse relay message:', err);
          }
        };

        this.ws.onclose = () => {
          const wasConnected = this.connected;
          this.connected = false;
          this.ws = null;

          if (wasConnected) {
            this.attemptReconnect();
          }
        };

        this.ws.onerror = (error) => {
          console.error('WebSocket error:', error);
          resolve(false);
        };

        // Connection timeout
        setTimeout(() => {
          if (!this.connected) {
            this.ws?.close();
            resolve(false);
          }
        }, 10000);
      } catch (error) {
        console.error('Failed to connect to relay:', error);
        resolve(false);
      }
    });
  }

  /**
   * Disconnect from the relay server.
   */
  async disconnect(reason = 'client_disconnect'): Promise<void> {
    if (this.reconnectTimeout) {
      clearTimeout(this.reconnectTimeout);
      this.reconnectTimeout = null;
    }

    if (this.ws && this.connected) {
      try {
        this.ws.send(JSON.stringify({ type: 'disconnect', reason }));
        this.ws.close();
      } catch {
        // Ignore close errors
      }
    }

    this.connected = false;
    this.ws = null;
    this.sessionId = null;
  }

  /**
   * Send a message to another agent.
   */
  async send(
    to: string,
    encryptedPayload: string,
    messageType: string,
    iceCandidates?: unknown[]
  ): Promise<boolean> {
    if (!this.isConnected) {
      throw new NetworkError('Not connected to relay', 'NOT_CONNECTED');
    }

    const message: RelayMessage = {
      type: 'send',
      to,
      encrypted_payload: encryptedPayload,
      message_type: messageType,
    };

    if (iceCandidates) {
      message.ice_candidates = iceCandidates;
    }

    try {
      this.ws!.send(JSON.stringify(message));
      return true;
    } catch (error) {
      throw new NetworkError(
        `Failed to send message: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'SEND_ERROR'
      );
    }
  }

  /**
   * Send KNOCK with message (optimistic send).
   * Combines KNOCK handshake with first message in a single packet.
   * The relay will deliver the message immediately if the recipient's policy accepts.
   */
  async sendOptimistic(
    to: string,
    knockPayload: string,
    messagePayload: string,
    messageType: string
  ): Promise<boolean> {
    if (!this.isConnected) {
      throw new NetworkError('Not connected to relay', 'NOT_CONNECTED');
    }

    const message: RelayMessage = {
      type: 'knock_with_message',
      to,
      knock_payload: knockPayload,
      message_payload: messagePayload,
      message_type: messageType,
    };

    try {
      this.ws!.send(JSON.stringify(message));
      return true;
    } catch (error) {
      throw new NetworkError(
        `Failed to send optimistic message: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'SEND_ERROR'
      );
    }
  }

  /**
   * Update presence status.
   */
  async updatePresence(status: string): Promise<boolean> {
    if (!this.isConnected) {
      return false;
    }

    try {
      this.ws!.send(JSON.stringify({ type: 'presence', status }));
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Query another agent's presence status.
   */
  async queryPresence(amid: string): Promise<void> {
    if (!this.isConnected) {
      return;
    }

    try {
      this.ws!.send(JSON.stringify({ type: 'presence_query', amid }));
    } catch {
      // Ignore errors
    }
  }

  /**
   * Register a handler for a message type.
   */
  onMessage(messageType: string, handler: MessageHandler): void {
    this.messageHandlers.set(messageType, handler);
  }

  /**
   * Remove a message handler.
   */
  offMessage(messageType: string): void {
    this.messageHandlers.delete(messageType);
  }

  /**
   * Register a transport event handler.
   */
  onTransportEvent(event: TransportEventType, handler: TransportEventHandler): void {
    const handlers = this.eventHandlers.get(event) || [];
    handlers.push(handler);
    this.eventHandlers.set(event, handlers);
  }

  /**
   * Emit a transport event.
   */
  private emitTransportEvent(event: TransportEventType, data: unknown): void {
    const handlers = this.eventHandlers.get(event) || [];
    for (const handler of handlers) {
      try {
        handler(data);
      } catch {
        // Ignore handler errors
      }
    }
  }

  /**
   * Request pending messages from relay (store-and-forward).
   */
  async requestPendingMessages(): Promise<PendingMessage[]> {
    if (!this.isConnected) {
      throw new NetworkError('Not connected to relay', 'NOT_CONNECTED');
    }

    if (this.pendingMessages === 0) {
      return [];
    }

    try {
      this.ws!.send(JSON.stringify({ type: 'get_pending_messages' }));

      // Wait for pending_messages response (set in onmessage handler)
      return new Promise((resolve) => {
        const checkInterval = setInterval(() => {
          if (this.pendingMessageQueue.length > 0 || this.pendingMessages === 0) {
            clearInterval(checkInterval);
            const messages = [...this.pendingMessageQueue];
            this.pendingMessageQueue = [];
            resolve(messages);
          }
        }, 50);

        // Timeout after 5 seconds
        setTimeout(() => {
          clearInterval(checkInterval);
          resolve([]);
        }, 5000);
      });
    } catch (error) {
      throw new NetworkError(
        `Failed to request pending messages: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'PENDING_REQUEST_ERROR'
      );
    }
  }

  /**
   * Process pending messages through handlers (FIFO order).
   * Returns count of successfully processed messages.
   */
  async processPendingMessages(
    messageProcessor: (msg: PendingMessage) => Promise<boolean>
  ): Promise<{ processed: number; failed: number }> {
    const messages = await this.requestPendingMessages();
    let processed = 0;
    let failed = 0;

    // Process in FIFO order (sorted by timestamp)
    const sorted = messages.sort((a, b) => a.timestamp - b.timestamp);

    for (const msg of sorted) {
      try {
        const success = await messageProcessor(msg);
        if (success) {
          // Send ACK to relay
          await this.acknowledgePendingMessage(msg.id, true);
          processed++;
        } else {
          // Send NACK to relay
          await this.acknowledgePendingMessage(msg.id, false, 'processing_failed');
          failed++;
          this.emitTransportEvent('pending_failed', {
            messageId: msg.id,
            from: msg.from,
            reason: 'processing_failed',
          });
        }
      } catch (error) {
        // Send NACK with error reason
        const reason = error instanceof Error ? error.message : 'unknown_error';
        await this.acknowledgePendingMessage(msg.id, false, reason);
        failed++;
        this.emitTransportEvent('pending_failed', {
          messageId: msg.id,
          from: msg.from,
          reason,
          error,
        });
      }
    }

    // Emit completion event
    this.emitTransportEvent('pending_processed', { processed, failed, total: messages.length });
    this.pendingMessages = 0;

    return { processed, failed };
  }

  /**
   * Send ACK or NACK for a pending message.
   */
  private async acknowledgePendingMessage(
    messageId: string,
    success: boolean,
    reason?: string
  ): Promise<void> {
    if (!this.isConnected) return;

    try {
      this.ws!.send(JSON.stringify({
        type: success ? 'ack' : 'nack',
        message_id: messageId,
        reason: reason,
      }));
    } catch {
      // Ignore ACK errors
    }
  }

  /**
   * Attempt reconnection with exponential backoff.
   */
  private attemptReconnect(): void {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error('Max reconnection attempts reached');
      return;
    }

    const delay = this.reconnectBaseDelay * Math.pow(2, this.reconnectAttempts);
    this.reconnectAttempts++;

    console.log(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts}/${this.maxReconnectAttempts})`);

    this.reconnectTimeout = setTimeout(async () => {
      const success = await this.connect();
      if (!success) {
        this.attemptReconnect();
      }
    }, delay);
  }
}

/**
 * P2P transport stub for environments without WebRTC.
 * Always falls back to relay transport.
 */
export class P2PTransport {
  private readonly identity: Identity;
  private readonly targetAmid: string;

  constructor(identity: Identity, targetAmid: string) {
    this.identity = identity;
    this.targetAmid = targetAmid;
  }

  /**
   * Check if P2P is available.
   * Always returns false in non-browser environments.
   */
  get isAvailable(): boolean {
    return false;
  }

  /**
   * Check if P2P connection is established.
   */
  get isConnected(): boolean {
    return false;
  }

  /**
   * Get P2P transport metrics.
   */
  getMetrics(): {
    available: boolean;
    connected: boolean;
    mode: string;
    stunServers: string[];
  } {
    return {
      available: false,
      connected: false,
      mode: 'relay-only',
      stunServers: [],
    };
  }

  /**
   * Attempt to establish P2P connection.
   * Always fails in non-browser environments.
   */
  async connect(): Promise<boolean> {
    console.warn('P2P transport not available in this environment');
    return false;
  }

  /**
   * Close P2P connection.
   */
  async close(): Promise<void> {
    // No-op
  }
}

/**
 * Create P2P transport (convenience function).
 */
export function createP2PTransport(identity: Identity, targetAmid: string): P2PTransport {
  return new P2PTransport(identity, targetAmid);
}
