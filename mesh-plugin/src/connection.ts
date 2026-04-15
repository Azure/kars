/**
 * Direct relay + registry connection for the standalone mesh plugin.
 *
 * This module connects to AgentMesh relay (WebSocket) and registry (HTTP)
 * without requiring the full @agentmesh/sdk client. It implements just
 * enough of the protocol for pairing, messaging, and discovery.
 *
 * Protocol (from vendor/agentmesh-relay/src/types.rs):
 * - Connect: { protocol, amid, public_key, signature, timestamp, p2p_capable }
 * - Send: { to, encrypted_payload, message_type }
 * - Receive: { from, encrypted_payload, message_type, timestamp }
 * - Keepalive: Ping/Pong every 30s
 */

import * as crypto from "node:crypto";
import type { MeshIdentity } from "./identity.js";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface ConnectionConfig {
  relayUrl: string;
  registryUrl: string;
  identity: MeshIdentity;
  displayName?: string;
  onMessage?: (from: string, payload: unknown) => void;
  onDisconnect?: () => void;
}

interface RelayEnvelope {
  from?: string;
  to?: string;
  encrypted_payload?: string;
  message_type?: string;
  timestamp?: string;
  protocol?: string;
  amid?: string;
  public_key?: string;
  signature?: string;
  p2p_capable?: boolean;
  type?: string;
  code?: number;
  message?: string;
  pong?: boolean;
}

interface InboxMessage {
  from: string;
  content: unknown;
  timestamp: string;
}

// ---------------------------------------------------------------------------
// Connection class
// ---------------------------------------------------------------------------

export class MeshConnection {
  private ws: import("node:net").Socket | null = null;
  private wsObj: any = null;
  private config: ConnectionConfig;
  private inbox: InboxMessage[] = [];
  private connected = false;
  private intentionalClose = false;
  private reconnectAttempts = 0;
  private keepaliveTimer: ReturnType<typeof setInterval> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private maxInboxSize = 500;

  constructor(config: ConnectionConfig) {
    this.config = config;
  }

  get isConnected(): boolean {
    return this.connected;
  }

  get amid(): string {
    return this.config.identity.amid;
  }

  // ── Connect ──────────────────────────────────────────────────────────

  async connect(): Promise<void> {
    this.intentionalClose = false;
    const { WebSocket } = await import("ws").catch(() => {
      throw new Error(
        "ws package required for relay connection. Install with: npm install ws"
      );
    });

    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.config.relayUrl);
      this.wsObj = ws;

      const timeout = setTimeout(() => {
        ws.close();
        reject(new Error("Relay connection timed out (10s)"));
      }, 10_000);

      ws.on("open", () => {
        clearTimeout(timeout);
        this.connected = true;
        this.sendAuth();
        this.startKeepalive();
        resolve();
      });

      ws.on("message", (data: Buffer) => {
        try {
          const msg: RelayEnvelope = JSON.parse(data.toString());
          this.handleRelayMessage(msg);
        } catch {
          // skip unparseable
        }
      });

      ws.on("close", () => {
        this.connected = false;
        this.stopKeepalive();
        this.config.onDisconnect?.();
        this.scheduleReconnect();
      });

      ws.on("error", (err: Error) => {
        clearTimeout(timeout);
        if (!this.connected) {
          reject(err);
        }
      });
    });
  }

  async disconnect(): Promise<void> {
    this.intentionalClose = true;
    this.stopKeepalive();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.wsObj) {
      this.wsObj.close();
      this.wsObj = null;
    }
    this.connected = false;
  }

  private scheduleReconnect(): void {
    if (this.intentionalClose) return;
    if (this.reconnectTimer) return;
    const delay = Math.min(5000 * Math.pow(2, this.reconnectAttempts), 60_000);
    this.reconnectAttempts++;
    console.log(`[mesh] Reconnecting in ${delay / 1000}s (attempt ${this.reconnectAttempts})...`);
    this.reconnectTimer = setTimeout(async () => {
      this.reconnectTimer = null;
      try {
        await this.connect();
        this.reconnectAttempts = 0;
        console.log("[mesh] Reconnected to relay");
      } catch {
        this.scheduleReconnect();
      }
    }, delay);
  }

  // ── Auth ─────────────────────────────────────────────────────────────

  private sendAuth(): void {
    const timestamp = new Date().toISOString();
    const signature = this.signTimestamp(timestamp);
    const authMsg = {
      protocol: "agentmesh/0.2",
      amid: this.config.identity.amid,
      public_key: this.config.identity.signingPublicKey.toString("base64"),
      signature,
      timestamp,
      p2p_capable: false,
    };
    this.wsSend(authMsg);
  }

  private signTimestamp(timestamp: string): string {
    const sign = crypto.sign(
      null,
      Buffer.from(timestamp),
      {
        key: Buffer.concat([
          // Ed25519 PKCS#8 prefix (https://www.rfc-editor.org/rfc/rfc8032)
          Buffer.from("302e020100300506032b6570042204200", "hex"),
          // pad to 16 bytes then append the actual 32-byte private key
          this.config.identity.signingPrivateKey,
        ]),
        format: "der",
        type: "pkcs8",
      }
    );
    return sign.toString("base64");
  }

  // ── Message handling ─────────────────────────────────────────────────

  private handleRelayMessage(msg: RelayEnvelope): void {
    // Peer message (from relay)
    if (msg.from && msg.encrypted_payload) {
      try {
        const payloadBytes = Buffer.from(msg.encrypted_payload, "base64");
        const content = JSON.parse(payloadBytes.toString());
        const inboxMsg: InboxMessage = {
          from: msg.from,
          content,
          timestamp: msg.timestamp || new Date().toISOString(),
        };
        this.inbox.push(inboxMsg);
        if (this.inbox.length > this.maxInboxSize) {
          this.inbox.shift();
        }
        this.config.onMessage?.(msg.from, content);
      } catch {
        // non-JSON payload — store raw
        this.inbox.push({
          from: msg.from,
          content: msg.encrypted_payload,
          timestamp: msg.timestamp || new Date().toISOString(),
        });
      }
      return;
    }

    // Pong
    if (msg.pong || msg.type === "pong") return;

    // Error
    if (msg.type === "error") {
      console.error(`[mesh] Relay error ${msg.code}: ${msg.message}`);
    }
  }

  // ── Send ─────────────────────────────────────────────────────────────

  async send(toAmid: string, payload: unknown): Promise<void> {
    if (!this.connected || !this.wsObj) {
      throw new Error("Not connected to relay");
    }
    const payloadStr = typeof payload === "string" ? payload : JSON.stringify(payload);
    const payloadB64 = Buffer.from(payloadStr).toString("base64");
    const sendMsg = {
      to: toAmid,
      encrypted_payload: payloadB64,
      message_type: "message",
    };
    this.wsSend(sendMsg);
  }

  // ── Inbox ────────────────────────────────────────────────────────────

  getInbox(limit?: number): InboxMessage[] {
    const msgs = limit ? this.inbox.slice(-limit) : [...this.inbox];
    return msgs;
  }

  drainInbox(): InboxMessage[] {
    const msgs = [...this.inbox];
    this.inbox = [];
    return msgs;
  }

  /** Wait for a message matching a predicate, with timeout. */
  async waitForMessage<T>(
    predicate: (content: unknown, from: string) => T | null,
    timeoutMs = 15_000
  ): Promise<T> {
    // Check existing inbox first
    for (const msg of this.inbox) {
      const result = predicate(msg.content, msg.from);
      if (result !== null) return result;
    }

    // Wait for new messages
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out waiting for message (${timeoutMs}ms)`));
      }, timeoutMs);

      const originalHandler = this.config.onMessage;
      const cleanup = () => {
        clearTimeout(timer);
        this.config.onMessage = originalHandler;
      };

      this.config.onMessage = (from: string, content: unknown) => {
        originalHandler?.(from, content);
        const result = predicate(content, from);
        if (result !== null) {
          cleanup();
          resolve(result);
        }
      };
    });
  }

  // ── Discovery (registry HTTP) ────────────────────────────────────────

  async discover(opts?: {
    capability?: string;
    limit?: number;
  }): Promise<Array<{ amid: string; displayName?: string; capabilities?: string[] }>> {
    const url = new URL("/v1/agents", this.config.registryUrl);
    if (opts?.capability) url.searchParams.set("capability", opts.capability);
    if (opts?.limit) url.searchParams.set("limit", String(opts.limit));

    const resp = await fetch(url.toString(), {
      headers: { "X-AMID": this.config.identity.amid },
    });
    if (!resp.ok) return [];
    const data = await resp.json();
    return Array.isArray(data) ? data : data.agents || [];
  }

  // ── Keepalive ────────────────────────────────────────────────────────

  private startKeepalive(): void {
    this.keepaliveTimer = setInterval(() => {
      if (this.connected && this.wsObj) {
        this.wsObj.ping?.();
      }
    }, 25_000);
  }

  private stopKeepalive(): void {
    if (this.keepaliveTimer) {
      clearInterval(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
  }

  private wsSend(msg: unknown): void {
    if (this.wsObj?.readyState === 1 /* OPEN */) {
      this.wsObj.send(JSON.stringify(msg));
    }
  }
}
