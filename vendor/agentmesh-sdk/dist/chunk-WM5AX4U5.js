import { NetworkError } from './chunk-FBJD3DSJ.js';

// src/transport.ts
var RelayTransport = class {
  identity;
  relayUrl;
  p2pCapable;
  maxReconnectAttempts;
  reconnectBaseDelay;
  ws = null;
  sessionId = null;
  connected = false;
  messageHandlers = /* @__PURE__ */ new Map();
  eventHandlers = /* @__PURE__ */ new Map();
  pendingMessages = 0;
  reconnectAttempts = 0;
  reconnectTimeout = null;
  pendingMessageQueue = [];
  constructor(identity, options = {}) {
    this.identity = identity;
    this.relayUrl = options.relayUrl ?? "wss://relay.agentmesh.online/v1/connect";
    this.p2pCapable = options.p2pCapable ?? false;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 5;
    this.reconnectBaseDelay = options.reconnectBaseDelay ?? 1e3;
  }
  /**
   * Check if connected to relay.
   */
  get isConnected() {
    return this.connected && this.ws !== null && this.ws.readyState === WebSocket.OPEN;
  }
  /**
   * Get current session ID.
   */
  get currentSessionId() {
    return this.sessionId;
  }
  /**
   * Get count of pending messages on connect.
   */
  get pendingMessageCount() {
    return this.pendingMessages;
  }
  /**
   * Connect to the relay server.
   */
  async connect() {
    if (this.isConnected) {
      return true;
    }
    return new Promise((resolve) => {
      try {
        this.ws = new WebSocket(this.relayUrl);
        this.ws.onopen = async () => {
          const [timestamp, signature] = await this.identity.signTimestamp();
          const connectMsg = {
            type: "connect",
            protocol: "agentmesh/0.2",
            amid: this.identity.amid,
            public_key: this.identity.signingPublicKeyB64Raw,
            signature,
            timestamp,
            p2p_capable: this.p2pCapable
          };
          this.ws.send(JSON.stringify(connectMsg));
        };
        this.ws.onmessage = (event) => {
          try {
            const data = JSON.parse(event.data);
            const msgType = data.type;
            if (msgType === "connected") {
              this.sessionId = data.session_id;
              this.pendingMessages = data.pending_messages ?? 0;
              this.connected = true;
              this.reconnectAttempts = 0;
              if (this.pendingMessages > 0) {
                this.requestPendingMessages().catch((err) => {
                  console.error("Failed to request pending messages:", err);
                });
              }
              resolve(true);
            } else if (msgType === "pending_messages") {
              const messages = data.messages ?? [];
              this.pendingMessageQueue = messages;
            } else if (msgType === "error") {
              console.error("Relay error:", data.error);
              this.connected = false;
              resolve(false);
            } else {
              const handler = this.messageHandlers.get(msgType);
              if (handler) {
                Promise.resolve(handler(data)).catch((err) => {
                  console.error(`Handler error for ${msgType}:`, err);
                });
              }
            }
          } catch (err) {
            console.error("Failed to parse relay message:", err);
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
          console.error("WebSocket error:", error);
          resolve(false);
        };
        setTimeout(() => {
          if (!this.connected) {
            this.ws?.close();
            resolve(false);
          }
        }, 1e4);
      } catch (error) {
        console.error("Failed to connect to relay:", error);
        resolve(false);
      }
    });
  }
  /**
   * Disconnect from the relay server.
   */
  async disconnect(reason = "client_disconnect") {
    if (this.reconnectTimeout) {
      clearTimeout(this.reconnectTimeout);
      this.reconnectTimeout = null;
    }
    if (this.ws && this.connected) {
      try {
        this.ws.send(JSON.stringify({ type: "disconnect", reason }));
        this.ws.close();
      } catch {
      }
    }
    this.connected = false;
    this.ws = null;
    this.sessionId = null;
  }
  /**
   * Send a message to another agent.
   */
  async send(to, encryptedPayload, messageType, iceCandidates) {
    if (!this.isConnected) {
      throw new NetworkError("Not connected to relay", "NOT_CONNECTED");
    }
    const message = {
      type: "send",
      to,
      encrypted_payload: encryptedPayload,
      message_type: messageType
    };
    if (iceCandidates) {
      message.ice_candidates = iceCandidates;
    }
    try {
      this.ws.send(JSON.stringify(message));
      return true;
    } catch (error) {
      throw new NetworkError(
        `Failed to send message: ${error instanceof Error ? error.message : "Unknown error"}`,
        "SEND_ERROR"
      );
    }
  }
  /**
   * Send KNOCK with message (optimistic send).
   * Combines KNOCK handshake with first message in a single packet.
   * The relay will deliver the message immediately if the recipient's policy accepts.
   */
  async sendOptimistic(to, knockPayload, messagePayload, messageType) {
    if (!this.isConnected) {
      throw new NetworkError("Not connected to relay", "NOT_CONNECTED");
    }
    const message = {
      type: "knock_with_message",
      to,
      knock_payload: knockPayload,
      message_payload: messagePayload,
      message_type: messageType
    };
    try {
      this.ws.send(JSON.stringify(message));
      return true;
    } catch (error) {
      throw new NetworkError(
        `Failed to send optimistic message: ${error instanceof Error ? error.message : "Unknown error"}`,
        "SEND_ERROR"
      );
    }
  }
  /**
   * Update presence status.
   */
  async updatePresence(status) {
    if (!this.isConnected) {
      return false;
    }
    try {
      this.ws.send(JSON.stringify({ type: "presence", status }));
      return true;
    } catch {
      return false;
    }
  }
  /**
   * Query another agent's presence status.
   */
  async queryPresence(amid) {
    if (!this.isConnected) {
      return;
    }
    try {
      this.ws.send(JSON.stringify({ type: "presence_query", amid }));
    } catch {
    }
  }
  /**
   * Register a handler for a message type.
   */
  onMessage(messageType, handler) {
    this.messageHandlers.set(messageType, handler);
  }
  /**
   * Remove a message handler.
   */
  offMessage(messageType) {
    this.messageHandlers.delete(messageType);
  }
  /**
   * Register a transport event handler.
   */
  onTransportEvent(event, handler) {
    const handlers = this.eventHandlers.get(event) || [];
    handlers.push(handler);
    this.eventHandlers.set(event, handlers);
  }
  /**
   * Emit a transport event.
   */
  emitTransportEvent(event, data) {
    const handlers = this.eventHandlers.get(event) || [];
    for (const handler of handlers) {
      try {
        handler(data);
      } catch {
      }
    }
  }
  /**
   * Request pending messages from relay (store-and-forward).
   */
  async requestPendingMessages() {
    if (!this.isConnected) {
      throw new NetworkError("Not connected to relay", "NOT_CONNECTED");
    }
    if (this.pendingMessages === 0) {
      return [];
    }
    try {
      this.ws.send(JSON.stringify({ type: "get_pending_messages" }));
      return new Promise((resolve) => {
        const checkInterval = setInterval(() => {
          if (this.pendingMessageQueue.length > 0 || this.pendingMessages === 0) {
            clearInterval(checkInterval);
            const messages = [...this.pendingMessageQueue];
            this.pendingMessageQueue = [];
            resolve(messages);
          }
        }, 50);
        setTimeout(() => {
          clearInterval(checkInterval);
          resolve([]);
        }, 5e3);
      });
    } catch (error) {
      throw new NetworkError(
        `Failed to request pending messages: ${error instanceof Error ? error.message : "Unknown error"}`,
        "PENDING_REQUEST_ERROR"
      );
    }
  }
  /**
   * Process pending messages through handlers (FIFO order).
   * Returns count of successfully processed messages.
   */
  async processPendingMessages(messageProcessor) {
    const messages = await this.requestPendingMessages();
    let processed = 0;
    let failed = 0;
    const sorted = messages.sort((a, b) => a.timestamp - b.timestamp);
    for (const msg of sorted) {
      try {
        const success = await messageProcessor(msg);
        if (success) {
          await this.acknowledgePendingMessage(msg.id, true);
          processed++;
        } else {
          await this.acknowledgePendingMessage(msg.id, false, "processing_failed");
          failed++;
          this.emitTransportEvent("pending_failed", {
            messageId: msg.id,
            from: msg.from,
            reason: "processing_failed"
          });
        }
      } catch (error) {
        const reason = error instanceof Error ? error.message : "unknown_error";
        await this.acknowledgePendingMessage(msg.id, false, reason);
        failed++;
        this.emitTransportEvent("pending_failed", {
          messageId: msg.id,
          from: msg.from,
          reason,
          error
        });
      }
    }
    this.emitTransportEvent("pending_processed", { processed, failed, total: messages.length });
    this.pendingMessages = 0;
    return { processed, failed };
  }
  /**
   * Send ACK or NACK for a pending message.
   */
  async acknowledgePendingMessage(messageId, success, reason) {
    if (!this.isConnected) return;
    try {
      this.ws.send(JSON.stringify({
        type: success ? "ack" : "nack",
        message_id: messageId,
        reason
      }));
    } catch {
    }
  }
  /**
   * Attempt reconnection with exponential backoff.
   */
  attemptReconnect() {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error("Max reconnection attempts reached");
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
};
var P2PTransport = class {
  identity;
  targetAmid;
  constructor(identity, targetAmid) {
    this.identity = identity;
    this.targetAmid = targetAmid;
  }
  /**
   * Check if P2P is available.
   * Always returns false in non-browser environments.
   */
  get isAvailable() {
    return false;
  }
  /**
   * Check if P2P connection is established.
   */
  get isConnected() {
    return false;
  }
  /**
   * Get P2P transport metrics.
   */
  getMetrics() {
    return {
      available: false,
      connected: false,
      mode: "relay-only",
      stunServers: []
    };
  }
  /**
   * Attempt to establish P2P connection.
   * Always fails in non-browser environments.
   */
  async connect() {
    console.warn("P2P transport not available in this environment");
    return false;
  }
  /**
   * Close P2P connection.
   */
  async close() {
  }
};
function createP2PTransport(identity, targetAmid) {
  return new P2PTransport(identity, targetAmid);
}

export { P2PTransport, RelayTransport, createP2PTransport };
//# sourceMappingURL=chunk-WM5AX4U5.js.map
//# sourceMappingURL=chunk-WM5AX4U5.js.map