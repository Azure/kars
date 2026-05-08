// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Mesh connection adapter — wraps @agentmesh/sdk's `AgentMeshClient`.
 *
 * Before: this module reimplemented a subset of the AgentMesh protocol on top
 * of a raw WebSocket. That worked only because every peer on the network used
 * `base64(plain JSON)` in `encrypted_payload` — a misnomer, not real Signal.
 *
 * Now: we delegate WebSocket lifecycle, authentication, prekey registration,
 * X3DH, and Double Ratchet to the SDK. This module keeps ownership of:
 *   - app-layer chunking (compatible with sandbox plugin `meshSend()`)
 *   - waiters / inbox / consumeInbox (poll-style handlers)
 *   - file transfer protocol (file_transfer + file_transfer_ack dance)
 *   - mesh:ping / mesh:pong helpers
 *   - registry discovery fan-out (seed capability list)
 *   - CONNECT-tunnel for Node-22 HTTPS_PROXY (injected via wsFactory)
 *
 * Legacy plaintext-compat: the Rust controller still writes `base64(JSON)` on
 * the wire. For those peers only, we call `client.addPlaintextPeer(amid)` so
 * the SDK bypasses Signal and uses the legacy wire format.
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as http from "node:http";
import * as net from "node:net";
import { WebSocket as WsWebSocket } from "ws";
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

console.log(`[mesh] connection.js loaded (build: ${BUILD_HASH}, signal-sdk)`);

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
  /**
   * Capabilities advertised during registry upsert on connect. Defaults to
   * ["mesh-peer"] if not provided.
   */
  capabilities?: string[];
  /**
   * AMIDs of peers that should bypass Signal E2E and use the legacy
   * base64(JSON) wire format. The Rust controller is the primary user until
   * it ships an SDK-equivalent client. Can also be added at runtime via
   * {@link MeshConnection.addPlaintextPeer}.
   */
  plaintextPeers?: string[];
  /**
   * Maximum inbox messages. Default 5000. Old messages are trimmed FIFO.
   */
  maxInboxSize?: number;
}

interface InboxMessage {
  /** Stable id for this message — used by mesh_inbox to mark-read and dedupe. */
  id: string;
  from: string;
  content: unknown;
  timestamp: string;
  /**
   * ISO timestamp set when the LLM-facing `mesh_inbox` tool returned this
   * entry with `mark_read=true`. Untouched by `consumeInbox` / `waitForMessage`
   * because those paths *remove* the entry from the array entirely.
   */
  read_at?: string;
}

export interface MeshDiagnostics {
  build_hash: string;
  received_total: number;
  consumed_by_waiter: number;
  consumed_by_predicate: number;
  fifo_dropped: number;
  read_total: number;
  last_received_at: string | null;
  last_read_at: string | null;
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
import { IMeshTransport } from "./transport-interface.js";

// ---------------------------------------------------------------------------

export class MeshConnection implements IMeshTransport {
  private config: ConnectionConfig;
  private inbox: InboxMessage[] = [];
  private maxInboxSize: number;
  private pendingTransfers = new Map<string, PendingTransfer>();
  private transferCleanupTimer: ReturnType<typeof setInterval> | null = null;
  private connectInFlight: Promise<void> | null = null;

  // Inbox + lifecycle counters. Surfaced by `getDiagnostics()` for the
  // mesh_inbox tool so operators can distinguish "never received" from
  // "already consumed" without re-running the demo. Bumped from the only
  // four sites that mutate `inbox`: pushInbox, deliverToWaiters (claimed
  // path), consumeInbox, waitForMessage.
  private stats = {
    received_total: 0,
    consumed_by_waiter: 0,
    consumed_by_predicate: 0,
    fifo_dropped: 0,
    read_total: 0,
    last_received_at: null as string | null,
    last_read_at: null as string | null,
  };

  // Lazily loaded SDK client + the wsFactory's current CONNECT tunnel socket.
  // The SDK owns the WebSocket, auth, prekey upload, Signal E2E, and reconnect.
  private client: any | null = null;

  /**
   * Active predicate subscriptions for waitForMessage. Multiple concurrent
   * waiters are supported — each message is offered to every waiter until
   * one claims it.
   */
  private waiters = new Set<{
    predicate: (content: unknown, from: string) => unknown;
    resolve: (v: unknown) => void;
    consume: boolean;
  }>();

  // Inbox wakers — fired when a content (non-internal/non-claimed) message
  // arrives, so server-side blocking inbox/await tools can wake immediately.
  private inboxWakers = new Set<() => void>();

  constructor(config: ConnectionConfig) {
    this.config = config;
    this.maxInboxSize = config.maxInboxSize ?? 5000;
  }

  get isConnected(): boolean {
    return !!this.client?.isConnected;
  }

  get amid(): string {
    return this.config.identity.amid;
  }

  // ── Connect ──────────────────────────────────────────────────────────

  async connect(): Promise<void> {
    if (this.connectInFlight) return this.connectInFlight;
    if (this.isConnected) return;

    this.connectInFlight = this.doConnect().finally(() => {
      this.connectInFlight = null;
    });
    return this.connectInFlight;
  }

  private async doConnect(): Promise<void> {
    const sdk = await import("@agentmesh/sdk").catch((err) => {
      throw new Error(
        `@agentmesh/sdk is required for mesh connection: ${err?.message || err}`,
      );
    });

    const wsFactory = this.makeWsFactory();

    // The SDK's RegistryClient prepends paths like `/registry/register` and
    // `/registry/prekeys`. Legacy tokens carry a base URL without the `/v1`
    // prefix (the old mesh-plugin added it itself). Normalize here so the
    // SDK hits the correct routes on existing registries.
    const registryUrl = this.normalizeRegistryUrl(this.config.registryUrl);

    // Build the client from our existing Ed25519/X25519 identity so the AMID
    // on the wire matches what the pairing handshake advertised.
    this.client = sdk.AgentMeshClient.fromIdentity(this.config.identity.sdkIdentity, {
      registryUrl,
      relayUrl: this.config.relayUrl,
      storage: new sdk.MemoryStorage(),
      plaintextPeers: this.config.plaintextPeers || [],
      transportOptions: wsFactory ? { wsFactory } : undefined,
    });

    // Wire message handler BEFORE connect — messages can arrive immediately
    // after the WebSocket opens.
    this.client.onMessage((from: string, content: unknown) => {
      this.onClientMessage(from, content);
    });

    // Auto-accept KNOCKs. AzureClaw gating lives at the controller/router,
    // not in the mesh transport. Without this, peers that initiate Signal
    // sessions get rejected.
    this.client.onKnock(async () => ({ accept: true }));

    console.log(
      `[mesh] Connecting to relay via SDK: ${this.config.relayUrl} ` +
      `(amid=${this.config.identity.amid}, did=${this.config.identity.did}, ` +
      `plaintextPeers=${(this.config.plaintextPeers || []).length})`,
    );

    await this.client.connect({
      capabilities: this.config.capabilities ?? ["mesh-peer"],
      displayName: this.config.displayName,
      autoUploadPrekeys: true,
    });

    // The SDK sometimes resolves connect() even when the underlying WebSocket
    // never reached OPEN (e.g. wsFactory throws, tunnel fails). Guard against
    // that so the caller sees a real error instead of a false-positive.
    if (!this.client.isConnected) {
      const err = new Error(
        `SDK connect() resolved but isConnected=false — WebSocket never opened. ` +
          `Check /tmp/gateway.log for "Failed to connect to relay" lines.`,
      );
      try { await this.client.disconnect(); } catch { /* noop */ }
      this.client = null;
      throw err;
    }

    this.startTransferCleanup();
    console.log(`[mesh] ✅ Connected — Signal-ready (amid=${this.config.identity.amid.slice(0, 12)}..., did=${this.config.identity.did})`);
  }

  async disconnect(): Promise<void> {
    if (this.transferCleanupTimer) {
      clearInterval(this.transferCleanupTimer);
      this.transferCleanupTimer = null;
    }
    if (this.client) {
      try { await this.client.disconnect(); } catch { /* noop */ }
      this.client = null;
    }
    for (const w of this.waiters) {
      try { w.resolve(new Error("Connection closing")); } catch { /* noop */ }
    }
    this.waiters.clear();
  }

  // ── Plaintext-compat peer set ────────────────────────────────────────

  /**
   * Register `amid` as a legacy plaintext peer. Future sends to this AMID
   * skip Signal E2E and use the base64(JSON) wire format. Required for peers
   * that haven't adopted the full SDK yet (e.g. Rust controller).
   */
  addPlaintextPeer(amid: string): void {
    if (!amid) return;
    if (!this.config.plaintextPeers) this.config.plaintextPeers = [];
    if (!this.config.plaintextPeers.includes(amid)) {
      this.config.plaintextPeers.push(amid);
    }
    this.client?.addPlaintextPeer?.(amid);
  }

  removePlaintextPeer(amid: string): void {
    if (!amid) return;
    if (this.config.plaintextPeers) {
      this.config.plaintextPeers = this.config.plaintextPeers.filter((a) => a !== amid);
    }
    this.client?.removePlaintextPeer?.(amid);
  }

  getPlaintextPeers(): string[] {
    return this.client?.getPlaintextPeers?.() || [...(this.config.plaintextPeers || [])];
  }

  // ── URL helpers ──────────────────────────────────────────────────────

  /**
   * Normalize the registry URL so the SDK's RegistryClient lands on the
   * right versioned routes. Legacy pairing tokens carry URLs like
   *   http://host.docker.internal:18080
   * while the registry routes live under `/v1/registry/...`. The old
   * mesh-plugin added `/v1` manually inside each request; the SDK instead
   * prepends `/registry/...` to whatever base URL we hand it. Append `/v1`
   * iff the URL has no versioned path yet.
   */
  private normalizeRegistryUrl(url: string): string {
    const trimmed = url.replace(/\/$/, "");
    if (/\/v\d+(?:\/|$)/.test(trimmed)) return trimmed;
    return `${trimmed}/v1`;
  }

  // ── WS factory: CONNECT-tunnel for HTTPS_PROXY environments ──────────

  /**
   * Build a `wsFactory` for the SDK's RelayTransport if a proxy is present.
   * In sandbox namespaces (OpenShell/NemoClaw), only the proxy is reachable
   * over TCP — direct relay connections are iptables-rejected. We open a
   * `CONNECT` tunnel with raw net.createConnection (bypassing Node 22's
   * built-in EnvHttpProxyAgent which hijacks http.request).
   *
   * The SDK assigns `this.ws = wsFactory(url)` synchronously, so the factory
   * MUST return a WebSocket immediately — not a Promise. We therefore do the
   * async tunnel setup inside a custom `http.Agent.createConnection` callback,
   * which `ws` invokes at connect time and is contractually async-capable.
   * Returns undefined when no proxy is configured; the SDK then does a direct
   * connection with its default `new WebSocket()`.
   */
  private makeWsFactory(): ((url: string) => unknown) | undefined {
    const proxyUrl = process.env.HTTPS_PROXY || process.env.HTTP_PROXY;
    if (!proxyUrl) return undefined;

    const createProxyTunnel = this.createProxyTunnel.bind(this);

    return (relayUrl: string): unknown => {
      const agent = new http.Agent();
      (agent as any).createConnection = (
        _options: unknown,
        callback: (err: Error | null, sock?: net.Socket) => void,
      ) => {
        createProxyTunnel(proxyUrl, relayUrl).then(
          (sock) => callback(null, sock),
          (err) => callback(err),
        );
      };
      return new WsWebSocket(relayUrl, { agent });
    };
  }

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
            `CONNECT ${host}:${port} HTTP/1.1\r\nHost: ${host}:${port}\r\n\r\n`,
          );
        },
      );

      const timer = setTimeout(() => {
        sock.destroy();
        reject(new Error("Proxy CONNECT tunnel timed out (10s)"));
      }, 10_000);

      let buf = "";
      const onData = (data: Buffer) => {
        buf += data.toString();
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

  // ── Message handling ─────────────────────────────────────────────────

  /**
   * Deliver content to any pending waiters. Matches consume the message;
   * otherwise it's stored in the inbox.
   * Returns true if a waiter claimed it (and it should NOT be stored in inbox).
   */
  private deliverToWaiters(from: string, content: unknown): boolean {
    if (this.waiters.size === 0) return false;
    for (const waiter of [...this.waiters]) {
      let result: unknown;
      try {
        result = waiter.predicate(content, from);
      } catch (err) {
        this.waiters.delete(waiter);
        waiter.resolve(err);
        if (waiter.consume) this.stats.consumed_by_waiter += 1;
        return waiter.consume;
      }
      if (result !== null && result !== undefined) {
        this.waiters.delete(waiter);
        waiter.resolve(result);
        if (waiter.consume) {
          this.stats.consumed_by_waiter += 1;
          return true;
        }
      }
    }
    return false;
  }

  private pushInbox(msg: InboxMessage): void {
    this.inbox.push(msg);
    this.stats.received_total += 1;
    this.stats.last_received_at = msg.timestamp;
    while (this.inbox.length > this.maxInboxSize) {
      this.inbox.shift();
      this.stats.fifo_dropped += 1;
    }
    // Wake any blocking inbox/await waiters. Internal/transport messages were
    // already filtered upstream (handleTransportMessage absorbs them); anything
    // that reaches pushInbox is a real content message.
    if (this.inboxWakers.size > 0) {
      const wakers = Array.from(this.inboxWakers);
      this.inboxWakers.clear();
      for (const w of wakers) {
        try { w(); } catch { /* swallow — waker is best-effort */ }
      }
    }
  }

  /**
   * Block up to `timeoutMs` until a new content message arrives in the inbox.
   * Returns true if woken by a message, false on timeout. Used by mesh_inbox
   * (block_until_message=true) and mesh_await to avoid LLM polling.
   */
  waitForInbox(timeoutMs: number): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let done = false;
      const waker = () => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        this.inboxWakers.delete(waker);
        resolve(true);
      };
      const timer = setTimeout(() => {
        if (done) return;
        done = true;
        this.inboxWakers.delete(waker);
        resolve(false);
      }, Math.max(1, timeoutMs));
      if (typeof (timer as any).unref === "function") (timer as any).unref();
      this.inboxWakers.add(waker);
    });
  }

  /**
   * Handle a decrypted message from the SDK. The SDK has already unwrapped
   * Signal encryption (or plaintext base64 for controller peers) and given
   * us the app-layer JSON object.
   */
  private onClientMessage(from: string, content: unknown): void {
    // Offload idle-watcher signal: when running as an offload sandbox, poke
    // the activity file on every inbound message from the parent AMID. The
    // entrypoint's idle-watcher reads this mtime to decide when to self-
    // terminate. Failures (e.g. read-only FS in unit tests) are swallowed.
    const parentAmid = process.env.OFFLOAD_PARENT_AMID;
    const activityFile = process.env.OFFLOAD_ACTIVITY_FILE;
    if (parentAmid && activityFile && from === parentAmid) {
      try {
        const now = Date.now();
        fs.utimesSync(activityFile, now / 1000, now / 1000);
      } catch {
        // File missing — try to create it so subsequent touches land.
        try { fs.writeFileSync(activityFile, ""); } catch { /* give up */ }
      }
    }

    // Chunk-reassembly runs before fan-out.
    const transportResult = this.handleTransportMessage(from, content);
    if (transportResult === "absorbed") return;

    const final: unknown = transportResult !== null ? transportResult : content;
    const ts = new Date().toISOString();

    const claimed = this.deliverToWaiters(from, final);
    if (!claimed) {
      this.pushInbox({
        id: `mesh-${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}`,
        from,
        content: final,
        timestamp: ts,
      });
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
      // Start a new pending transfer
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
    const transfer = this.pendingTransfers.get(key);
    if (!transfer) {
      // Chunk arrived before manifest (rare) — drop
      return "absorbed";
    }

    const chunkIndex = message.chunk_index as number;
    const chunkData = message.data as string;
    const chunkHash = message.hash as string;

    // Verify per-chunk SHA-256
    const actualHash = crypto.createHash("sha256").update(chunkData).digest("hex");
    if (actualHash !== chunkHash) {
      console.warn(
        `[mesh] Chunk ${chunkIndex} hash mismatch for transfer ${transferId.slice(0, 8)} — dropping transfer`,
      );
      this.pendingTransfers.delete(key);
      return "absorbed";
    }

    transfer.chunks.set(chunkIndex, chunkData);

    // Still accumulating?
    if (transfer.chunks.size < transfer.total_chunks) {
      return "absorbed";
    }

    // All chunks received — reassemble
    const ordered: string[] = [];
    for (let i = 0; i < transfer.total_chunks; i++) {
      const c = transfer.chunks.get(i);
      if (c === undefined) {
        console.warn(`[mesh] Transfer ${transferId.slice(0, 8)} missing chunk ${i} — dropping`);
        this.pendingTransfers.delete(key);
        return "absorbed";
      }
      ordered.push(c);
    }
    const reassembled = ordered.join("");

    // Verify manifest hash
    const actualManifestHash = crypto
      .createHash("sha256")
      .update(transfer.chunk_hashes.join(":"))
      .digest("hex");
    if (transfer.manifest_hash && actualManifestHash !== transfer.manifest_hash) {
      console.warn(
        `[mesh] Transfer ${transferId.slice(0, 8)} manifest hash mismatch — dropping`,
      );
      this.pendingTransfers.delete(key);
      return "absorbed";
    }

    this.pendingTransfers.delete(key);

    try {
      return JSON.parse(reassembled);
    } catch {
      return "absorbed";
    }
  }

  // Expire stale incomplete transfers (5 minute TTL)
  private startTransferCleanup(): void {
    if (this.transferCleanupTimer) return;
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

  /**
   * Send a message, auto-chunking if the JSON-encoded payload exceeds 512KB.
   * Each chunk is a separate SDK `send()` call so the SDK's Signal layer
   * encrypts each chunk independently (or bypasses encryption for peers in
   * `plaintextPeers`).
   */
  async send(toAmid: string, payload: unknown): Promise<string | undefined> {
    if (!this.client || !this.isConnected) {
      throw new Error("Not connected to relay");
    }

    const json = typeof payload === "string" ? payload : JSON.stringify(payload);

    // Fast path — single message
    if (json.length <= CHUNK_THRESHOLD) {
      const obj = typeof payload === "object" && payload !== null
        ? (payload as Record<string, unknown>)
        : { type: "message", value: payload };
      await this.client.send(toAmid, obj);
      return undefined;
    }

    // Large payload — chunked transfer
    const totalChunks = Math.ceil(json.length / CHUNK_SIZE);
    if (totalChunks > MAX_CHUNKS) {
      throw new Error(
        `Payload too large: ${(json.length / 1024 / 1024).toFixed(1)} MB ` +
        `(${totalChunks} chunks exceeds max ${MAX_CHUNKS})`,
      );
    }

    const transferId = crypto.randomUUID();
    const displayName = this.config.displayName || this.config.identity.amid.slice(0, 12);

    const chunkHashes: string[] = [];
    for (let i = 0; i < totalChunks; i++) {
      const chunk = json.slice(i * CHUNK_SIZE, (i + 1) * CHUNK_SIZE);
      chunkHashes.push(crypto.createHash("sha256").update(chunk).digest("hex"));
    }
    const manifestHash = crypto.createHash("sha256").update(chunkHashes.join(":")).digest("hex");

    // 1. Send manifest
    await this.client.send(toAmid, {
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
      await this.client.send(toAmid, {
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

  // ── File transfer ───────────────────────────────────────────────────

  /**
   * Send a file via E2E encrypted mesh. Reads the file, base64-encodes it,
   * and sends via the chunked transfer protocol. Waits for ACK from receiver.
   */
  async sendFile(
    toAmid: string,
    filePath: string,
    opts?: { description?: string; timeoutMs?: number; retries?: number },
  ): Promise<FileTransferAck> {
    const fsMod = await import("node:fs");
    const pathMod = await import("node:path");

    // Open file once and read via fd to avoid stat→read TOCTOU race.
    const fd = fsMod.openSync(filePath, "r");
    let fileData: Buffer;
    let fileSize: number;
    try {
      const stat = fsMod.fstatSync(fd);
      if (!stat.isFile()) throw new Error(`Not a regular file: ${filePath}`);
      if (stat.size > MAX_FILE_SIZE) {
        throw new Error(`File too large: ${(stat.size / 1024 / 1024).toFixed(1)} MB (max 30MB)`);
      }
      fileSize = stat.size;
      fileData = Buffer.alloc(fileSize);
      fsMod.readSync(fd, fileData, 0, fileSize, 0);
    } finally {
      fsMod.closeSync(fd);
    }
    const b64Data = fileData.toString("base64");
    const fileName = pathMod.basename(filePath);
    const displayName = this.config.displayName || this.config.identity.amid.slice(0, 12);

    const fileMsg = {
      type: "file_transfer",
      file_name: fileName,
      file_path: filePath,
      file_data: b64Data,
      size_bytes: fileSize,
      description: opts?.description || "",
      from_agent: displayName,
      timestamp: new Date().toISOString(),
    };

    const maxAttempts = opts?.retries ?? 3;
    const ackTimeout = opts?.timeoutMs ?? 15_000;

    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      if (attempt > 0) {
        await new Promise((r) => setTimeout(r, 3000));
      }
      await this.send(toAmid, fileMsg);
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
   */
  async handleFileTransfer(
    fromAmid: string,
    message: Record<string, unknown>,
    saveDir: string,
  ): Promise<{ savedPath: string; fileName: string; sizeBytes: number } | null> {
    if (message.type !== "file_transfer" || !message.file_data || !message.file_name) {
      return null;
    }
    const fsMod = await import("node:fs");
    const pathMod = await import("node:path");

    const rawName = String(message.file_name);
    const safeName = pathMod.basename(rawName); // strip any directory traversal
    const destPath = pathMod.join(saveDir, safeName);

    let success = false;
    let error: string | undefined;
    let buf = Buffer.alloc(0);
    try {
      fsMod.mkdirSync(saveDir, { recursive: true });
      buf = Buffer.from(String(message.file_data), "base64");
      fsMod.writeFileSync(destPath, buf);
      success = true;
    } catch (err: any) {
      error = err?.message || String(err);
    }

    await this.send(fromAmid, {
      type: "file_transfer_ack",
      from_agent: this.config.displayName || this.config.identity.amid.slice(0, 12),
      file_name: safeName,
      success,
      saved_to: destPath,
      error,
      timestamp: new Date().toISOString(),
    });

    return success ? { savedPath: destPath, fileName: safeName, sizeBytes: buf.length } : null;
  }

  // ── Inbox ────────────────────────────────────────────────────────────

  getInbox(limit?: number): InboxMessage[] {
    const msgs = limit ? this.inbox.slice(-limit) : [...this.inbox];
    return msgs;
  }

  /**
   * Mark the given message ids as read (in-place). Used by the mesh_inbox
   * tool when called with `mark_read=true` so subsequent `unread_only`
   * queries don't re-list the same entries. Entries themselves stay in
   * the inbox until `consumeInbox` / `waitForMessage` claims them or the
   * FIFO cap evicts them.
   */
  markRead(ids: string[]): number {
    if (ids.length === 0) return 0;
    const idSet = new Set(ids);
    const now = new Date().toISOString();
    let count = 0;
    for (const m of this.inbox) {
      if (idSet.has(m.id) && !m.read_at) {
        m.read_at = now;
        count += 1;
      }
    }
    if (count > 0) {
      this.stats.read_total += count;
      this.stats.last_read_at = now;
    }
    return count;
  }

  /** Number of inbox entries with no `read_at` timestamp. */
  getUnreadCount(): number {
    let n = 0;
    for (const m of this.inbox) if (!m.read_at) n += 1;
    return n;
  }

  /** Snapshot of inbox + lifecycle counters for diagnostics. */
  getDiagnostics(): MeshDiagnostics {
    return {
      build_hash: BUILD_HASH,
      received_total: this.stats.received_total,
      consumed_by_waiter: this.stats.consumed_by_waiter,
      consumed_by_predicate: this.stats.consumed_by_predicate,
      fifo_dropped: this.stats.fifo_dropped,
      read_total: this.stats.read_total,
      last_received_at: this.stats.last_received_at,
      last_read_at: this.stats.last_read_at,
    };
  }

  drainInbox(): InboxMessage[] {
    const msgs = [...this.inbox];
    this.stats.consumed_by_predicate += msgs.length;
    this.inbox = [];
    return msgs;
  }

  /**
   * Remove and return all inbox messages matching `predicate`.
   */
  consumeInbox(
    predicate: (msg: InboxMessage) => boolean,
  ): InboxMessage[] {
    const claimed: InboxMessage[] = [];
    const kept: InboxMessage[] = [];
    for (const m of this.inbox) {
      if (predicate(m)) claimed.push(m);
      else kept.push(m);
    }
    if (claimed.length > 0) {
      this.inbox = kept;
      this.stats.consumed_by_predicate += claimed.length;
    }
    return claimed;
  }

  /**
   * Wait for a message matching a predicate, with timeout.
   */
  async waitForMessage<T>(
    predicate: (content: unknown, from: string) => T | null,
    timeoutMs = 15_000,
    opts: { consume?: boolean } = {},
  ): Promise<T> {
    const consume = opts.consume !== false;
    for (let i = 0; i < this.inbox.length; i++) {
      const msg = this.inbox[i];
      const result = predicate(msg.content, msg.from);
      if (result !== null && result !== undefined) {
        if (consume) {
          this.inbox.splice(i, 1);
          this.stats.consumed_by_waiter += 1;
        }
        return result;
      }
    }

    return new Promise<T>((resolve, reject) => {
      const waiter = {
        predicate: predicate as (content: unknown, from: string) => unknown,
        consume,
        resolve: (v: unknown) => {
          clearTimeout(timer);
          if (v instanceof Error) reject(v);
          else resolve(v as T);
        },
      };
      const timer = setTimeout(() => {
        this.waiters.delete(waiter);
        reject(new Error(`Timed out waiting for message (${timeoutMs}ms)`));
      }, timeoutMs);
      this.waiters.add(waiter);
    });
  }

  /**
   * Send a message and wait for an ack matching `ackPredicate`.
   */
  async sendWithAck<T>(
    toAmid: string,
    payload: unknown,
    ackPredicate: (content: unknown, from: string) => T | null,
    opts: { timeoutMs?: number; retries?: number; retryDelayMs?: number } = {},
  ): Promise<T> {
    const timeoutMs = opts.timeoutMs ?? 5000;
    const retries = opts.retries ?? 2;
    const retryDelayMs = opts.retryDelayMs ?? 1500;

    let lastErr: Error | null = null;
    for (let attempt = 0; attempt <= retries; attempt++) {
      if (!this.isConnected) {
        throw new Error("Not connected to relay");
      }
      if (attempt > 0) {
        await new Promise((r) => setTimeout(r, retryDelayMs));
      }
      try {
        await this.send(toAmid, payload);
      } catch (err: any) {
        lastErr = err;
        continue;
      }
      try {
        return await this.waitForMessage(ackPredicate, timeoutMs);
      } catch (err: any) {
        lastErr = err;
      }
    }
    throw lastErr || new Error(`No ack after ${retries + 1} attempts`);
  }

  /**
   * Send a mesh-level ping to a peer and wait for `mesh:pong` reply.
   */
  async pingPeer(
    toAmid: string,
    opts: { timeoutMs?: number; retries?: number } = {},
  ): Promise<{ rttMs: number; pong: Record<string, unknown> }> {
    const nonce = crypto.randomUUID();
    const sentAt = Date.now();
    const pong = await this.sendWithAck<Record<string, unknown>>(
      toAmid,
      {
        type: "mesh:ping",
        nonce,
        from_agent: this.config.displayName || this.config.identity.amid.slice(0, 12),
        timestamp: new Date().toISOString(),
      },
      (content, from) => {
        const m = content as Record<string, unknown>;
        if (from === toAmid && m?.type === "mesh:pong" && m?.nonce === nonce) {
          return m;
        }
        return null;
      },
      {
        timeoutMs: opts.timeoutMs ?? 3000,
        retries: opts.retries ?? 2,
        retryDelayMs: 1000,
      },
    );
    return { rttMs: Date.now() - sentAt, pong };
  }

  // ── Discovery (SDK-backed, with multi-capability fan-out) ────────────

  async discover(opts?: {
    capability?: string;
    limit?: number;
  }): Promise<Array<{ amid: string; displayName?: string; capabilities?: string[] }>> {
    if (!this.client) throw new Error("Not connected to relay");

    // Multi-capability fan-out when no capability filter is specified. The
    // registry's search requires `capability = ANY(capabilities)` — there's
    // no "list all" endpoint — so we aggregate well-known labels.
    if (!opts?.capability) {
      const seeds = [
        "azureclaw-agent",
        "task-execution",
        "cluster-controller",
        "offload",
        "offload-sandbox",
        "mesh-peer",
        "chat",
        "assistant",
      ];
      const seen = new Map<string, { amid: string; displayName?: string; capabilities?: string[] }>();
      await Promise.all(
        seeds.map(async (cap) => {
          try {
            const batch = await this.discover({ capability: cap, limit: opts?.limit ?? 50 });
            for (const a of batch) {
              if (a.amid && !seen.has(a.amid)) seen.set(a.amid, a);
            }
          } catch { /* best-effort aggregation */ }
        }),
      );
      return Array.from(seen.values()).slice(0, opts?.limit ?? 50);
    }

    try {
      const results = await this.client.search(opts.capability, { limit: opts.limit });
      return (results as Array<Record<string, unknown>>).map((a) => ({
        amid: String(a.amid ?? ""),
        displayName:
          (a.displayName as string | undefined) ??
          (a.display_name as string | undefined),
        capabilities: a.capabilities as string[] | undefined,
      }));
    } catch {
      return [];
    }
  }

  /** Resolve an agent name to its AMID via registry search. */
  async resolveAmid(name: string): Promise<string | null> {
    const byCap = await this.discover({ capability: name });
    let match = byCap.find(
      (a) => a.displayName === name || a.capabilities?.includes(name),
    );
    if (match?.amid) return match.amid;
    const all = await this.discover({ limit: 50 });
    match = all.find((a) => a.displayName === name);
    return match?.amid || null;
  }
}
