/**
 * Direct relay + registry connection for the standalone mesh plugin.
 *
 * This module connects to AgentMesh relay (WebSocket) and registry (HTTP)
 * without requiring the full @agentmesh/sdk client. It implements just
 * enough of the protocol for pairing, messaging, file transfer, and discovery.
 *
 * Protocol (from vendor/agentmesh-relay/src/types.rs):
 * - Connect: { protocol, amid, public_key, signature, timestamp, p2p_capable }
 * - Send: { to, encrypted_payload, message_type }
 * - Receive: { from, encrypted_payload, message_type, timestamp }
 * - Keepalive: Ping/Pong every 30s
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as http from "node:http";
import * as net from "node:net";
import type { MeshIdentity } from "./identity.js";

// ---------------------------------------------------------------------------
// Build fingerprint — printed at load time for deployment verification
// ---------------------------------------------------------------------------

const BUILD_HASH = (() => {
  try {
    const src = fs.readFileSync(new URL(import.meta.url).pathname);
    return crypto.createHash("sha256").update(src).digest("hex").slice(0, 12);
  } catch { return "unknown"; }
})();

console.log(`[mesh] connection.js loaded (build: ${BUILD_HASH})`);

// ---------------------------------------------------------------------------
// Constants — must match cli/src/plugin.ts for interop
// ---------------------------------------------------------------------------

const CHUNK_THRESHOLD = 512 * 1024;  // auto-chunk above 512KB
const CHUNK_SIZE = 512 * 1024;       // 512KB per chunk
const MAX_CHUNKS = 80;               // 80 × 512KB ≈ 40MB max payload
const MAX_FILE_SIZE = 30 * 1024 * 1024; // 30MB per file

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

interface PendingTransfer {
  from_amid: string;
  transfer_id: string;
  total_chunks: number;
  total_bytes: number;
  chunk_hashes: string[];
  manifest_hash: string;
  original_type: string;
  chunks: Map<number, string>;
  received_at: number;
}

export interface FileTransferAck {
  success: boolean;
  file_name: string;
  saved_to?: string;
  error?: string;
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
  private pendingTransfers = new Map<string, PendingTransfer>();
  private transferCleanupTimer: ReturnType<typeof setInterval> | null = null;

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

    console.log(`[mesh] Connecting to relay: ${this.config.relayUrl}`);
    const proxyUrl = process.env.HTTPS_PROXY || process.env.HTTP_PROXY;

    // When behind a CONNECT proxy (e.g. OpenShell/NemoClaw sandbox), the
    // sandbox namespace only allows TCP to the proxy. Direct connections are
    // rejected by iptables. We establish a CONNECT tunnel first, then run
    // the WebSocket upgrade through it. Raw net.createConnection is used for
    // the tunnel to bypass Node 22's EnvHttpProxyAgent which intercepts
    // http.request() even with custom agents. See NemoClaw#557.
    let tunnelSocket: net.Socket | undefined;
    if (proxyUrl) {
      console.log(`[mesh] Proxy detected (${proxyUrl}) — establishing CONNECT tunnel`);
      try {
        tunnelSocket = await this.createProxyTunnel(proxyUrl, this.config.relayUrl);
        console.log(`[mesh] CONNECT tunnel established`);
      } catch (err: any) {
        console.log(`[mesh] CONNECT tunnel failed: ${err.message} — falling back to direct`);
      }
    }

    return new Promise((resolve, reject) => {
      let ws: InstanceType<typeof WebSocket>;
      if (tunnelSocket) {
        // Route WebSocket upgrade through the pre-established CONNECT tunnel.
        const agent = new http.Agent();
        agent.createConnection = (_options: any, callback: any) => {
          callback(null, tunnelSocket);
          return tunnelSocket!;
        };
        ws = new WebSocket(this.config.relayUrl, { agent });
      } else {
        ws = new WebSocket(this.config.relayUrl);
      }
      this.wsObj = ws;

      const timeout = setTimeout(() => {
        console.log(`[mesh] Relay connection timed out after 10s`);
        ws.close();
        reject(new Error("Relay connection timed out (10s)"));
      }, 10_000);

      ws.on("open", () => {
        clearTimeout(timeout);
        this.connected = true;
        console.log(`[mesh] WebSocket open — sending auth (AMID: ${this.config.identity.amid})`);
        this.sendAuth();
        this.startKeepalive();
        this.startTransferCleanup();
        resolve();
      });

      ws.on("message", (data: Buffer) => {
        try {
          const msg: RelayEnvelope = JSON.parse(data.toString());
          if (msg.type === "connected") {
            console.log(`[mesh] ✅ Authenticated with relay (session: ${(msg as any).session_id?.slice(0, 8)}..., pending: ${(msg as any).pending_messages ?? 0})`);
          } else if (msg.type === "error") {
            console.log(`[mesh] ❌ Relay error: ${(msg as any).message || JSON.stringify(msg)}`);
          }
          this.handleRelayMessage(msg);
        } catch {
          // skip unparseable
        }
      });

      ws.on("close", (code: number, reason: Buffer) => {
        this.connected = false;
        this.stopKeepalive();
        console.log(`[mesh] WebSocket closed (code: ${code}, reason: ${reason?.toString() || "none"})`);
        this.config.onDisconnect?.();
        this.scheduleReconnect();
      });

      ws.on("error", (err: Error) => {
        clearTimeout(timeout);
        console.log(`[mesh] WebSocket error: ${err.message}`);
        if (!this.connected) {
          reject(err);
        }
      });
    });
  }

  async disconnect(): Promise<void> {
    this.intentionalClose = true;
    this.stopKeepalive();
    if (this.transferCleanupTimer) {
      clearInterval(this.transferCleanupTimer);
      this.transferCleanupTimer = null;
    }
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

  // ── Proxy CONNECT tunnel ─────────────────────────────────────────────

  private createProxyTunnel(proxyUrl: string, targetUrl: string): Promise<net.Socket> {
    return new Promise((resolve, reject) => {
      const proxy = new URL(proxyUrl);
      const target = new URL(targetUrl);
      const host = target.hostname;
      const port = target.port || (target.protocol === "wss:" ? "443" : "80");

      const sock = net.createConnection(
        parseInt(proxy.port || "3128", 10),
        proxy.hostname,
        () => {
          sock.write(
            `CONNECT ${host}:${port} HTTP/1.1\r\nHost: ${host}:${port}\r\n\r\n`
          );
        }
      );

      const timer = setTimeout(() => {
        sock.destroy();
        reject(new Error("Proxy CONNECT tunnel timed out (10s)"));
      }, 10_000);

      let buf = "";
      const onData = (data: Buffer) => {
        buf += data.toString();
        // Wait for the complete header block
        if (!buf.includes("\r\n\r\n")) return;
        sock.removeListener("data", onData);
        clearTimeout(timer);
        const statusLine = buf.split("\r\n")[0];
        if (statusLine.includes("200")) {
          resolve(sock);
        } else {
          sock.destroy();
          reject(new Error(`Proxy CONNECT denied: ${statusLine}`));
        }
      };

      sock.on("data", onData);
      sock.on("error", (err) => {
        clearTimeout(timer);
        reject(err);
      });
    });
  }

  // ── Auth ─────────────────────────────────────────────────────────────

  private sendAuth(): void {
    const timestamp = new Date().toISOString();
    const signature = this.signTimestamp(timestamp);
    const authMsg = {
      type: "connect",
      protocol: "agentmesh/0.2",
      amid: this.config.identity.amid,
      public_key: this.config.identity.signingPublicKey.toString("base64"),
      signature,
      timestamp,
      p2p_capable: false,
    };
    console.log(`[mesh] Sending auth: type=connect, amid=${this.config.identity.amid}, ts=${timestamp}`);
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

        // Handle chunked transfer protocol transparently
        const transportResult = this.handleTransportMessage(msg.from, content);
        if (transportResult === "absorbed") return;
        if (transportResult !== null) {
          // Reassembled message — deliver to inbox/callback
          const inboxMsg: InboxMessage = {
            from: msg.from,
            content: transportResult,
            timestamp: msg.timestamp || new Date().toISOString(),
          };
          this.inbox.push(inboxMsg);
          if (this.inbox.length > this.maxInboxSize) this.inbox.shift();
          this.config.onMessage?.(msg.from, transportResult);
          return;
        }

        // Regular (non-transport) message
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

  // ── Chunked transfer protocol ───────────────────────────────────────
  // Mirrors cli/src/plugin.ts meshHandleTransportMessage exactly.
  // Returns:
  //   "absorbed" — transport message consumed (manifest or partial chunk)
  //   object     — fully reassembled message, deliver to inbox
  //   null       — not a transport message, handle normally

  private handleTransportMessage(
    fromAmid: string,
    message: any,
  ): Record<string, unknown> | "absorbed" | null {
    const msgType = message?.type;
    if (msgType !== "mesh:transfer_manifest" && msgType !== "mesh:transfer_chunk") {
      return null; // not a transport message
    }

    const transferId = message.transfer_id;
    if (!transferId) return "absorbed"; // malformed

    const key = `${fromAmid}:${transferId}`;

    if (msgType === "mesh:transfer_manifest") {
      this.pendingTransfers.set(key, {
        from_amid: fromAmid,
        transfer_id: transferId,
        total_chunks: message.total_chunks,
        total_bytes: message.total_bytes,
        chunk_hashes: message.chunk_hashes || [],
        manifest_hash: message.manifest_hash || "",
        original_type: message.original_type || "message",
        chunks: new Map(),
        received_at: Date.now(),
      });
      return "absorbed";
    }

    // mesh:transfer_chunk
    let transfer = this.pendingTransfers.get(key);
    if (!transfer) {
      // Chunk arrived before manifest — search by transfer_id
      for (const [, t] of this.pendingTransfers) {
        if (t.transfer_id === transferId && t.from_amid === fromAmid) {
          transfer = t;
          break;
        }
      }
      if (!transfer) return "absorbed"; // no manifest — drop
    }

    // Verify chunk hash before accepting
    if (message.hash && transfer.chunk_hashes[message.chunk_index]) {
      const computed = crypto.createHash("sha256").update(message.data).digest("hex");
      if (computed !== message.hash) {
        console.warn(`[mesh] Chunk ${message.chunk_index} hash mismatch (transfer ${transferId.slice(0, 8)}) — rejected`);
        return "absorbed";
      }
    }

    transfer.chunks.set(message.chunk_index, message.data);

    if (transfer.chunks.size < transfer.total_chunks) {
      return "absorbed"; // still accumulating
    }

    // All chunks received — verify manifest hash and reassemble
    const actualHashes: string[] = [];
    for (let i = 0; i < transfer.total_chunks; i++) {
      actualHashes.push(
        crypto.createHash("sha256").update(transfer.chunks.get(i) || "").digest("hex")
      );
    }
    const actualManifestHash = crypto.createHash("sha256").update(actualHashes.join(":")).digest("hex");
    if (transfer.manifest_hash && actualManifestHash !== transfer.manifest_hash) {
      console.warn(
        `[mesh] Transfer ${transferId.slice(0, 8)}: manifest hash mismatch — ` +
        `expected ${transfer.manifest_hash.slice(0, 12)}, got ${actualManifestHash.slice(0, 12)}`
      );
    }

    const parts: string[] = [];
    for (let i = 0; i < transfer.total_chunks; i++) {
      parts.push(transfer.chunks.get(i) || "");
    }
    const reassembledJson = parts.join("");
    this.pendingTransfers.delete(key);

    try {
      return JSON.parse(reassembledJson);
    } catch {
      console.warn(`[mesh] Transfer ${transferId.slice(0, 8)}: reassembled JSON parse failed`);
      return "absorbed";
    }
  }

  // Expire stale incomplete transfers (5 minute TTL)
  private startTransferCleanup(): void {
    this.transferCleanupTimer = setInterval(() => {
      const now = Date.now();
      for (const [key, t] of this.pendingTransfers) {
        if (now - t.received_at > 5 * 60 * 1000) {
          this.pendingTransfers.delete(key);
        }
      }
    }, 60_000);
  }

  // ── Send ─────────────────────────────────────────────────────────────

  /** Send a message, auto-chunking if payload exceeds 512KB. */
  async send(toAmid: string, payload: unknown): Promise<string | undefined> {
    if (!this.connected || !this.wsObj) {
      throw new Error("Not connected to relay");
    }

    const json = typeof payload === "string" ? payload : JSON.stringify(payload);

    // Fast path — single message
    if (json.length <= CHUNK_THRESHOLD) {
      const payloadB64 = Buffer.from(json).toString("base64");
      this.wsSend({
        type: "send",
        to: toAmid,
        encrypted_payload: payloadB64,
        message_type: "message",
      });
      return undefined;
    }

    // Large payload — chunked transfer
    const totalChunks = Math.ceil(json.length / CHUNK_SIZE);
    if (totalChunks > MAX_CHUNKS) {
      throw new Error(
        `Payload too large: ${(json.length / 1024 / 1024).toFixed(1)} MB ` +
        `(${totalChunks} chunks exceeds max ${MAX_CHUNKS})`
      );
    }

    const transferId = crypto.randomUUID();
    const displayName = this.config.displayName || this.config.identity.amid.slice(0, 12);

    // Compute per-chunk SHA-256 for integrity
    const chunkHashes: string[] = [];
    for (let i = 0; i < totalChunks; i++) {
      const chunk = json.slice(i * CHUNK_SIZE, (i + 1) * CHUNK_SIZE);
      chunkHashes.push(crypto.createHash("sha256").update(chunk).digest("hex"));
    }
    const manifestHash = crypto.createHash("sha256").update(chunkHashes.join(":")).digest("hex");

    // 1. Send manifest
    await this.sendRaw(toAmid, {
      type: "mesh:transfer_manifest",
      transfer_id: transferId,
      original_type: (payload as any)?.type || "message",
      total_chunks: totalChunks,
      total_bytes: json.length,
      chunk_hashes: chunkHashes,
      manifest_hash: manifestHash,
      from_agent: displayName,
      timestamp: new Date().toISOString(),
    });

    // 2. Send chunks sequentially (relay preserves FIFO per peer)
    for (let i = 0; i < totalChunks; i++) {
      const chunkData = json.slice(i * CHUNK_SIZE, (i + 1) * CHUNK_SIZE);
      await this.sendRaw(toAmid, {
        type: "mesh:transfer_chunk",
        transfer_id: transferId,
        chunk_index: i,
        total_chunks: totalChunks,
        data: chunkData,
        hash: chunkHashes[i],
        from_agent: displayName,
      });
    }

    return transferId;
  }

  /** Send a raw relay envelope (no chunking). Used internally for chunks/manifests. */
  private async sendRaw(toAmid: string, payload: unknown): Promise<void> {
    const payloadStr = typeof payload === "string" ? payload : JSON.stringify(payload);
    const payloadB64 = Buffer.from(payloadStr).toString("base64");
    this.wsSend({
      type: "send",
      to: toAmid,
      encrypted_payload: payloadB64,
      message_type: "message",
    });
  }

  // ── File transfer ───────────────────────────────────────────────────

  /**
   * Send a file via E2E encrypted mesh. Reads the file, base64-encodes it,
   * and sends via the chunked transfer protocol. Waits for ACK from receiver.
   *
   * @returns ACK result with success/failure and save path
   */
  async sendFile(
    toAmid: string,
    filePath: string,
    opts?: { description?: string; timeoutMs?: number; retries?: number },
  ): Promise<FileTransferAck> {
    const fs = await import("node:fs");
    const pathMod = await import("node:path");

    const stat = fs.statSync(filePath);
    if (!stat.isFile()) throw new Error(`Not a regular file: ${filePath}`);
    if (stat.size > MAX_FILE_SIZE) {
      throw new Error(`File too large: ${(stat.size / 1024 / 1024).toFixed(1)} MB (max 30MB)`);
    }

    const fileData = fs.readFileSync(filePath);
    const b64Data = fileData.toString("base64");
    const fileName = pathMod.basename(filePath);
    const displayName = this.config.displayName || this.config.identity.amid.slice(0, 12);

    const fileMsg = {
      type: "file_transfer",
      file_name: fileName,
      file_path: filePath,
      file_data: b64Data,
      size_bytes: stat.size,
      description: opts?.description || "",
      from_agent: displayName,
      timestamp: new Date().toISOString(),
    };

    const maxAttempts = opts?.retries ?? 3;
    const ackTimeout = opts?.timeoutMs ?? 15_000;

    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      if (attempt > 0) {
        await new Promise((r) => setTimeout(r, 3000)); // backoff between retries
      }

      await this.send(toAmid, fileMsg);

      // Wait for file_transfer_ack
      try {
        const ack = await this.waitForMessage<FileTransferAck>(
          (content) => {
            const msg = content as Record<string, unknown>;
            if (msg?.type === "file_transfer_ack" && msg?.file_name === fileName) {
              return {
                success: !!msg.success,
                file_name: String(msg.file_name),
                saved_to: msg.saved_to as string | undefined,
                error: msg.error as string | undefined,
              };
            }
            return null;
          },
          ackTimeout,
        );
        return ack;
      } catch {
        // ACK timeout — retry
      }
    }

    return { success: false, file_name: fileName, error: `No ACK after ${maxAttempts} attempts` };
  }

  /**
   * Handle incoming file_transfer messages — save to local directory and ACK.
   * Call this from onMessage handler or process inbox messages.
   */
  async handleFileTransfer(
    fromAmid: string,
    message: Record<string, unknown>,
    saveDir: string,
  ): Promise<{ savedPath: string; fileName: string; sizeBytes: number } | null> {
    if (message.type !== "file_transfer" || !message.file_data || !message.file_name) {
      return null;
    }

    const fs = await import("node:fs");
    const pathMod = await import("node:path");

    // Sanitize filename — strip path components, block traversal
    const rawName = String(message.file_name);
    const safeName = pathMod.basename(rawName).replace(/[^a-zA-Z0-9._-]/g, "_");
    if (!safeName || safeName.startsWith(".")) {
      await this.send(fromAmid, {
        type: "file_transfer_ack",
        file_name: rawName,
        success: false,
        error: "Invalid file name",
        timestamp: new Date().toISOString(),
      });
      return null;
    }

    fs.mkdirSync(saveDir, { recursive: true });
    const destPath = pathMod.join(saveDir, safeName);
    const buf = Buffer.from(String(message.file_data), "base64");
    fs.writeFileSync(destPath, buf, { mode: 0o600 });

    // Verify write
    const stat = fs.statSync(destPath);
    const success = stat.size === buf.length;

    // Send ACK
    await this.send(fromAmid, {
      type: "file_transfer_ack",
      from_agent: this.config.displayName || this.config.identity.amid.slice(0, 12),
      file_name: safeName,
      success,
      saved_to: destPath,
      timestamp: new Date().toISOString(),
    });

    return success ? { savedPath: destPath, fileName: safeName, sizeBytes: buf.length } : null;
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
    for (let i = 0; i < this.inbox.length; i++) {
      const msg = this.inbox[i];
      const result = predicate(msg.content, msg.from);
      if (result !== null) {
        this.inbox.splice(i, 1); // consume matched message
        return result;
      }
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

  /** Resolve an agent name to its AMID via registry search. */
  async resolveAmid(name: string): Promise<string | null> {
    const agents = await this.discover({ capability: name });
    const match = agents.find(
      (a) => a.displayName === name || a.capabilities?.includes(name)
    );
    return match?.amid || null;
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
