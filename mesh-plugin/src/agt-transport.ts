// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT-backed mesh transport — implements IMeshTransport using the
 * upstream Microsoft Agent Governance SDK.
 *
 * Package: @microsoft/agent-governance-sdk (^3.5.0)
 * Module:  @microsoft/agent-governance-sdk/dist/encryption (or default export)
 *
 * Sole mesh transport implementation. Wire compatibility: speaks
 * AgentMesh Wire Protocol v1.0. Upstream MeshClient includes the
 * Kars compatibility features (plaintextPeers, wsFactory hook,
 * KNOCK pending queue). See agent-governance-typescript/src/encryption/mesh-client.ts.
 */

import type {
  IMeshIdentity,
  IMeshTransport,
  DiscoveredPeer,
  FileTransferAck,
  InboxMessage,
  InboxDiagnostics,
} from "./transport-interface.js";
import { LocalInbox } from "./local-inbox.js";
import * as crypto from "node:crypto";

const MAX_FILE_SIZE = 30 * 1024 * 1024;

// All registry HTTP plumbing (retries, base64url marshalling, error
// classes) lives in @microsoft/agent-governance-sdk's RegistryClient
// now (upstream port of vendored SDK patch #12). We reach it via
// MeshClient.getRegistry() — see lookup() below.

// ── Lazy SDK loading (optional dependency) ───────────────────────
//
// The upstream SDK is an *optional* npm dependency: vendored deployments
// don't need it installed. Loading is deferred to connect() so that
// `new AgtTransport(...)` is cheap and side-effect-free.

interface AgtSdkModule {
  X3DHKeyManager: new (
    edPriv: Uint8Array,
    edPub: Uint8Array,
  ) => AgtX3DHKeyManager;
  MeshClient: new (opts: AgtMeshClientOptions) => AgtMeshClient;
}

interface AgtX3DHKeyManager {
  generateSignedPreKey(): { keyId: number; publicKey: Uint8Array; signature: Uint8Array };
  generateOneTimePreKeys(
    count: number,
  ): Array<{ keyId: number; publicKey: Uint8Array }>;
}

interface AgtRegistryClient {
  register(
    did: string,
    identityKey: Uint8Array,
    capabilities?: string[],
    metadata?: Record<string, string>,
  ): Promise<void>;
  getAgent(did: string): Promise<{
    did: string;
    capabilities: string[];
    metadata: Record<string, string>;
    reputationScore: number;
  } | null>;
  discover(
    capability: string,
    limit?: number,
  ): Promise<
    Array<{
      did: string;
      capabilities: string[];
      reputationScore: number;
      lastSeen: Date;
    }>
  >;
}

interface AgtMeshClientOptions {
  relayUrl: string;
  registryUrl: string;
  keyManager: AgtX3DHKeyManager;
  agentDid: string;
  displayName?: string;
  wsFactory?: (url: string) => unknown;
  plaintextPeers?: string[];
  knockTimeout?: number;
  capabilities?: string[];
  registrationMetadata?: Record<string, string>;
  oneTimePrekeyCount?: number;
  autoRegister?: boolean;
}

interface AgtMeshClient {
  readonly isConnected: boolean;
  connect(): Promise<void>;
  disconnect(): Promise<void>;
  reconnect(): Promise<void>;
  send(peerId: string, payload: unknown): Promise<void>;
  onMessage(
    handler: (from: string, payload: unknown, isPlaintext: boolean) => void,
  ): void;
  onKnock(handler: (from: string, intent: unknown) => Promise<boolean>): void;
  sendHeartbeat(): void;
  addPlaintextPeer(peerId: string): void;
  removePlaintextPeer(peerId: string): void;
  isPlaintextPeer(peerId: string): boolean;
  // Registry helpers (added in kars-meshclient-event-hooks AGT branch).
  getRegistry?: () => AgtRegistryClient | null;
  discover?: (
    capability: string,
    limit?: number,
  ) => Promise<
    Array<{
      did: string;
      capabilities: string[];
      reputationScore: number;
      lastSeen: Date;
    }>
  >;
  registerSelf?: () => Promise<void>;
  // Phase 2 event hooks (added in kars-meshclient-event-hooks AGT branch)
  onError?: (
    handler: (kind: string, from: string, detail: string) => void,
  ) => void;
  onDisconnect?: (
    handler: (reason: "client" | "server" | "ws-error", code?: number) => void,
  ) => void;
  onE2EVerified?: (
    handler: (peerAmid: string, isFirstPeer: boolean) => void,
  ) => void;
  /**
   * (legacy/optional) Some SDK forks expose this name. The real upstream
   * method is establishSession(toAmid, options). We no longer call either
   * here — AgentMeshClient.send() auto-bootstraps the X3DH handshake on
   * first contact. Kept on the type only to document the historical API
   * surface; consumers should NOT depend on it.
   */
  establishSessionWithPeer?: (peerId: string) => Promise<unknown>;
}

let agtSdkPromise: Promise<AgtSdkModule> | null = null;
async function loadAgtSdk(): Promise<AgtSdkModule> {
  if (agtSdkPromise) return agtSdkPromise;
  agtSdkPromise = (async () => {
    try {
      // Use a computed specifier so TypeScript doesn't try to resolve the
      // optional dependency at compile time (it may not be installed in the
      // vendored deployment path).
      const pkg = "@microsoft/agent-governance-sdk";
      const mod = (await import(/* @vite-ignore */ pkg)) as Record<
        string,
        unknown
      >;
      // The package re-exports encryption primitives at top level (see
      // agent-governance-typescript/src/index.ts). Some bundlers expose
      // them under `.encryption.*`; tolerate both.
      const enc = (mod.encryption ?? mod) as Record<string, unknown>;
      const X3DHKeyManager = enc.X3DHKeyManager as AgtSdkModule["X3DHKeyManager"];
      const MeshClient = enc.MeshClient as AgtSdkModule["MeshClient"];
      if (!X3DHKeyManager || !MeshClient) {
        throw new Error(
          "MeshClient/X3DHKeyManager not found in @microsoft/agent-governance-sdk",
        );
      }
      return { X3DHKeyManager, MeshClient };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      throw new Error(
        `@microsoft/agent-governance-sdk is required for AGT mesh transport. ` +
          `Install: npm i @microsoft/agent-governance-sdk@^3.5.0. ` +
          `Underlying error: ${msg}`,
      );
    }
  })();
  return agtSdkPromise;
}

// Test-only hook to inject a mock SDK without npm install.
export function __setAgtSdkForTesting(sdk: AgtSdkModule | null): void {
  agtSdkPromise = sdk ? Promise.resolve(sdk) : null;
}

// ── Transport implementation ─────────────────────────────────────

export interface AgtTransportOptions {
  relayUrl: string;
  registryUrl: string;
  identity: IMeshIdentity;
  displayName?: string;
  wsFactory?: (url: string) => unknown;
  plaintextPeers?: string[];
  knockTimeout?: number;
  /** Number of one-time prekeys to mint at connect. Default: 20. */
  oneTimePreKeyCount?: number;
  /**
   * Capabilities to publish at registration time. The displayName is
   * auto-included by MeshClient, so adding it here is unnecessary.
   */
  capabilities?: string[];
}

export class AgtTransport implements IMeshTransport {
  private readonly options: AgtTransportOptions;
  private client: AgtMeshClient | null = null;
  private readonly messageHandlers: Array<
    (from: string, payload: unknown) => void
  > = [];
  private readonly knockHandlers: Array<
    (from: string, intent: unknown) => Promise<{ accept: boolean }>
  > = [];
  private readonly _plaintextPeers: Set<string>;
  private _connected = false;
  private readonly inbox: LocalInbox;

  // Phase 2 diagnostic hooks — fan out from AGT MeshClient callbacks
  // (registered in connect() once the client exists).
  private readonly _errorHandlers: Array<(kind: string, fromAmid: string, detail: string) => void> = [];
  private readonly _e2eVerifiedHandlers: Array<(peerAmid: string, isFirstPeer: boolean) => void> = [];
  private readonly _disconnectHandlers: Array<(reason: "client" | "server" | "ws-error", code?: number) => void> = [];

  // Auto-heartbeat ticker. The AGT Python relay
  // (agentmesh/relay/app.py) marks a connection stale after
  // OFFLINE_THRESHOLD = 90s without a `heartbeat` frame and routes
  // subsequent messages to its OFFLINE STORE instead of live delivery.
  // Stored frames are only replayed on (re)connect via _deliver_pending,
  // so a long-lived connection that never reconnects loses every message
  // that was stored while it was "stale". The AGT MeshClient exposes
  // sendHeartbeat() but does NOT auto-schedule it, so we run our own
  // 30s ticker here (matches relay's HEARTBEAT_INTERVAL constant).
  // The vendored Rust relay has no time-based stale check (only checks
  // for broken channels), which is why vendored mode worked without
  // this. Required for AGT mode reply-path symmetry — without this,
  // any reply that arrives >90s after the recipient's last connect
  // is silently swallowed by the relay.
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;

  constructor(options: AgtTransportOptions) {
    this.options = options;
    this._plaintextPeers = new Set(options.plaintextPeers ?? []);
    this.inbox = new LocalInbox({ buildHash: "agt" });
  }

  get isConnected(): boolean {
    return this._connected && this.client?.isConnected === true;
  }

  get agentId(): string {
    return this.options.identity.agentId;
  }

  /** Alias for agentId — kept for legacy callers using the AMID nomenclature. */
  get amid(): string {
    return this.options.identity.agentId;
  }

  async connect(opts?: {
    capabilities?: string[];
    displayName?: string;
  }): Promise<void> {
    if (this.isConnected) return;
    const sdk = await loadAgtSdk();

    // X3DH key manager — MeshClient.registerSelf() will call
    // generateSignedPreKey() and generateOneTimePreKeys() itself, so
    // we don't need to mint keys here.
    const keyManager = new sdk.X3DHKeyManager(
      this.options.identity.signingPrivateKey,
      this.options.identity.signingPublicKey,
    );

    // Resolve effective capabilities + displayName for this connect
    // call. The displayName is auto-included as a capability by the
    // upstream MeshClient so peers can find this agent via
    // registry.discover(displayName).
    const displayName = opts?.displayName ?? this.options.displayName;
    const capabilities = [
      ...(this.options.capabilities ?? []),
      ...(opts?.capabilities ?? []),
    ];

    this.client = new sdk.MeshClient({
      relayUrl: this.options.relayUrl,
      registryUrl: this.options.registryUrl,
      keyManager,
      agentDid: this.options.identity.agentId,
      displayName,
      wsFactory: this.options.wsFactory,
      plaintextPeers: [...this._plaintextPeers],
      knockTimeout: this.options.knockTimeout,
      capabilities,
      oneTimePrekeyCount: this.options.oneTimePreKeyCount ?? 20,
      autoRegister: true,
    });

    // Bridge SDK callbacks → our handler arrays + LocalInbox.
    this.client.onMessage((from, payload, _isPlaintext) => {
      // First fan out to subscribed handlers (Phase 2 callers); then deliver
      // to the inbox so polling code (mesh_inbox tool, waitForMessage) sees
      // every message exactly once via either the waiter set or the FIFO.
      for (const handler of this.messageHandlers) {
        try {
          handler(from, payload);
        } catch (e) {
          // Handler errors must not poison the SDK callback chain.
          // eslint-disable-next-line no-console
          console.error("[agt-transport] message handler threw:", e);
        }
      }
      this.inbox.deliver(from, payload);
    });

    this.client.onKnock(async (from, intent) => {
      // Accept-by-default if no handlers; reject if any handler rejects.
      for (const handler of this.knockHandlers) {
        try {
          const result = await handler(from, intent);
          if (!result.accept) return false;
        } catch (e) {
          // eslint-disable-next-line no-console
          console.error("[agt-transport] knock handler threw:", e);
          return false;
        }
      }
      return true;
    });

    // Bridge AGT MeshClient diagnostic hooks (added on the AGT side in
    // branch `kars-meshclient-event-hooks`). When AGT is too old to
    // expose these the methods are absent — we silently skip and the
    // runtime continues without them.
    this.client.onError?.((kind, from, detail) => {
      for (const h of this._errorHandlers) {
        try { h(kind, from, detail); } catch { /* swallow */ }
      }
    });
    this.client.onE2EVerified?.((peer, first) => {
      for (const h of this._e2eVerifiedHandlers) {
        try { h(peer, first); } catch { /* swallow */ }
      }
    });
    this.client.onDisconnect?.((reason, code) => {
      for (const h of this._disconnectHandlers) {
        try { h(reason, code); } catch { /* swallow */ }
      }
    });

    await this.client.connect();
    this._connected = true;

    // Start auto-heartbeat ticker (see field comment for rationale).
    if (this.heartbeatTimer === null) {
      this.heartbeatTimer = setInterval(() => {
        try {
          this.client?.sendHeartbeat();
        } catch {
          // Best-effort. A dead WS will be detected by onClose and
          // trigger reconnect; the next connect resets this loop.
        }
        // Registry presence ping. The AGT registry has no
        // server-side path that updates `last_seen` (see
        // agentmesh/registry/app.py — `update_last_seen()` exists in
        // store.py but is dead code in production). Without this,
        // every alive agent looks "stale" 90s after spawn and gets
        // filtered out of discover. Required for sibling
        // peer-discovery in the openclaw runtime (kars_discover
        // tool, runtimes/openclaw/src/core/agt-tools/agt.ts:~1579).
        // Best-effort; swallow errors so a flaky registry never
        // brings down message delivery.
        void this.pingRegistryPresence();
      }, 30_000);
      // Don't keep the Node event loop alive solely for the ticker.
      const timer = this.heartbeatTimer as unknown as { unref?: () => void };
      timer.unref?.();
    }
  }

  async disconnect(): Promise<void> {
    if (this.heartbeatTimer !== null) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    if (this.client) {
      try {
        await this.client.disconnect();
      } catch {
        // best-effort — connection may already be down
      }
    }
    this._connected = false;
    this.client = null;
  }

  async send(toAmid: string, payload: unknown): Promise<string | undefined> {
    if (!this.client) throw new Error("AgtTransport not connected");
    // @microsoft/agent-governance-sdk MeshClient.send() throws
    // "No encrypted session with <peer>. Call establishSession() first."
    // when no SecureChannel exists for the peer — it does NOT auto-bootstrap
    // X3DH. establishSessionWithPeer() is the SDK's high-level helper that
    // fetches the prekey bundle from the registry and runs the X3DH+KNOCK
    // flow; it's idempotent (returns the cached MeshSession when one
    // already exists — see SDK mesh-client.js L230-245), so this is cheap
    // on the hot path.
    if (!this._plaintextPeers.has(toAmid)) {
      try {
        await this.client.establishSessionWithPeer!(toAmid);
      } catch (e: unknown) {
        // Surface the real error verbatim — the caller's retry loop matches
        // on /prekey/i, so a generic "prekey bootstrap failed" wrapper hides
        // permanent errors (bad-key / X3DH failures) inside an infinite poll.
        // Log only a fixed string + error class — never include `toAmid`
        // (CodeQL traces it back to process.env taint) or `e.message` (may
        // contain sensitive session material). The full error is preserved
        // on the throw so the caller's matcher still works.
        const errClass = e instanceof Error ? e.constructor.name : "Unknown";
        try {
          // eslint-disable-next-line no-console
          console.error("[agt-transport] establishSessionWithPeer failed:", errClass);
        } catch {
          /* logger may be off */
        }
        throw e instanceof Error ? e : new Error(String(e));
      }
    }
    await this.client.send(toAmid, payload);
    return undefined;
  }

  onMessage(handler: (fromAmid: string, payload: unknown) => void): void {
    this.messageHandlers.push(handler);
  }

  onKnock(
    handler: (fromAmid: string, intent: unknown) => Promise<{ accept: boolean }>,
  ): void {
    this.knockHandlers.push(handler);
  }

  addPlaintextPeer(amid: string): void {
    this._plaintextPeers.add(amid);
    this.client?.addPlaintextPeer(amid);
  }

  removePlaintextPeer(amid: string): void {
    this._plaintextPeers.delete(amid);
    this.client?.removePlaintextPeer(amid);
  }

  isPlaintextPeer(amid: string): boolean {
    return this._plaintextPeers.has(amid);
  }

  getPlaintextPeers(): string[] {
    return [...this._plaintextPeers];
  }

  /**
   * Discovery via the AGT registry REST API.
   *
   * Calls MeshClient.discover(capability) which wraps GET /v1/discover
   * (returns {results,total}). Multi-capability lookups are issued in
   * parallel and deduplicated by DID. Empty capability list returns an
   * empty array — AGT's /v1/discover requires a `capability` query
   * parameter, so "list all" is not supported.
   */
  async discover(opts?: {
    capability?: string;
    capabilities?: string[];
    limit?: number;
  }): Promise<DiscoveredPeer[]> {
    if (!this.client?.discover) {
      throw new Error("AgtTransport.discover: MeshClient.discover unavailable");
    }
    const limit = opts?.limit ?? 50;
    const caps: string[] = [];
    if (opts?.capabilities) caps.push(...opts.capabilities);
    if (opts?.capability) caps.push(opts.capability);
    if (caps.length === 0) return [];

    const seen = new Map<string, DiscoveredPeer>();
    const results = await Promise.all(
      caps.map((cap) => this.client!.discover!(cap, limit).catch(() => [])),
    );
    for (const list of results) {
      for (const r of list) {
        if (!seen.has(r.did)) {
          seen.set(r.did, {
            amid: r.did,
            // The displayName was registered as a capability; pick the
            // first capability that doesn't look like a query token.
            displayName: pickDisplayName(r.capabilities),
            capabilities: r.capabilities,
          });
        }
      }
    }
    return Array.from(seen.values()).slice(0, limit);
  }

  sendHeartbeat(): void {
    this.client?.sendHeartbeat();
  }

  /** Resolve a friendly name → AMID via registry capability search. */
  async resolveAmid(name: string): Promise<string | null> {
    const matches = await this.discover({ capability: name });
    const exact = matches.find(
      (a) => a.displayName === name || a.capabilities?.includes(name),
    );
    return exact?.amid ?? null;
  }

  // ── Inbox surface (delegated to LocalInbox) ───────────────────

  getInbox(limit?: number): InboxMessage[] {
    return this.inbox.getInbox(limit);
  }

  markRead(ids: string[]): number {
    return this.inbox.markRead(ids);
  }

  getUnreadCount(): number {
    return this.inbox.getUnreadCount();
  }

  drainInbox(): InboxMessage[] {
    return this.inbox.drainInbox();
  }

  consumeInbox(predicate: (msg: InboxMessage) => boolean): InboxMessage[] {
    return this.inbox.consumeInbox(predicate);
  }

  waitForMessage<T>(
    predicate: (content: unknown, from: string) => T | null,
    timeoutMs?: number,
    opts?: { consume?: boolean },
  ): Promise<T> {
    return this.inbox.waitForMessage(predicate, timeoutMs, opts);
  }

  waitForInbox(timeoutMs: number): Promise<boolean> {
    return this.inbox.waitForInbox(timeoutMs);
  }

  getDiagnostics(): InboxDiagnostics {
    return this.inbox.getDiagnostics();
  }

  // ── App-layer helpers (built on top of send + waitForMessage) ──

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
      if (!this.isConnected) throw new Error("Not connected to relay");
      if (attempt > 0) {
        await new Promise((r) => setTimeout(r, retryDelayMs));
      }
      try {
        await this.send(toAmid, payload);
      } catch (err) {
        lastErr = err as Error;
        continue;
      }
      try {
        return await this.waitForMessage(ackPredicate, timeoutMs);
      } catch (err) {
        lastErr = err as Error;
      }
    }
    throw lastErr ?? new Error(`No ack after ${retries + 1} attempts`);
  }

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
        from_agent: this.options.displayName ?? this.agentId.slice(0, 12),
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

  async sendFile(
    toAmid: string,
    filePath: string,
    opts?: { description?: string; timeoutMs?: number; retries?: number },
  ): Promise<FileTransferAck> {
    const fsMod = await import("node:fs");
    const pathMod = await import("node:path");

    const fd = fsMod.openSync(filePath, "r");
    let fileData: Buffer;
    let fileSize: number;
    try {
      const stat = fsMod.fstatSync(fd);
      if (!stat.isFile()) throw new Error(`Not a regular file: ${filePath}`);
      if (stat.size > MAX_FILE_SIZE) {
        throw new Error(
          `File too large: ${(stat.size / 1024 / 1024).toFixed(1)} MB (max 30MB)`,
        );
      }
      fileSize = stat.size;
      fileData = Buffer.alloc(fileSize);
      fsMod.readSync(fd, fileData, 0, fileSize, 0);
    } finally {
      fsMod.closeSync(fd);
    }
    const b64Data = fileData.toString("base64");
    const fileName = pathMod.basename(filePath);
    const displayName = this.options.displayName ?? this.agentId.slice(0, 12);

    const fileMsg = {
      type: "file_transfer",
      file_name: fileName,
      file_path: filePath,
      file_data: b64Data,
      size_bytes: fileSize,
      description: opts?.description ?? "",
      from_agent: displayName,
      timestamp: new Date().toISOString(),
    };

    const maxAttempts = opts?.retries ?? 3;
    const ackTimeout = opts?.timeoutMs ?? 15_000;

    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      if (attempt > 0) await new Promise((r) => setTimeout(r, 3000));
      await this.send(toAmid, fileMsg);
      try {
        const ack = await this.waitForMessage<FileTransferAck>((content) => {
          const msg = content as Record<string, unknown>;
          if (
            msg?.type === "file_transfer_ack" &&
            msg?.file_name === fileName
          ) {
            return {
              success: !!msg.success,
              file_name: String(msg.file_name),
              saved_to: msg.saved_to as string | undefined,
              error: msg.error as string | undefined,
            };
          }
          return null;
        }, ackTimeout);
        return ack;
      } catch {
        // ACK timeout — retry
      }
    }
    return {
      success: false,
      file_name: fileName,
      error: `No ACK after ${maxAttempts} attempts`,
    };
  }

  async handleFileTransfer(
    fromAmid: string,
    message: Record<string, unknown>,
    saveDir: string,
  ): Promise<{ savedPath: string; fileName: string; sizeBytes: number } | null> {
    if (
      message.type !== "file_transfer" ||
      !message.file_data ||
      !message.file_name
    ) {
      return null;
    }
    const fsMod = await import("node:fs");
    const pathMod = await import("node:path");

    const fileName = String(message.file_name);
    if (fileName.includes("/") || fileName.includes("..") || fileName.includes("\0")) {
      throw new Error(`Refusing unsafe file name: ${fileName}`);
    }
    const buf = Buffer.from(String(message.file_data), "base64");
    const savedPath = pathMod.join(saveDir, fileName);
    fsMod.mkdirSync(saveDir, { recursive: true });
    fsMod.writeFileSync(savedPath, buf, { mode: 0o600 });

    // Send ack back to sender (best-effort).
    try {
      await this.send(fromAmid, {
        type: "file_transfer_ack",
        file_name: fileName,
        success: true,
        saved_to: savedPath,
        timestamp: new Date().toISOString(),
      });
    } catch {
      // ACK is best-effort; the sender will retry.
    }
    return { savedPath, fileName, sizeBytes: buf.length };
  }

  // ── Reputation / lookup (AGT registry REST) ─────────────────────
  //
  // Uses MeshClient.getRegistry() to call the AGT-native endpoints:
  //   GET  /v1/agents/{did}                 → record (capabilities, metadata, reputation_score)
  //   POST /v1/agents/{did}/reputation      → submit feedback (score 0..1)
  // These replace the vendored /registry/lookup and /registry/feedback
  // routes that AGT does not expose.

  async lookup(
    amid: string,
  ): Promise<{ reputationScore?: number; displayName?: string; capabilities?: string[] } | null> {
    const reg = this.client?.getRegistry?.();
    if (!reg) return null;
    try {
      const rec = await reg.getAgent(amid);
      if (!rec) return null;
      return {
        reputationScore: rec.reputationScore,
        displayName:
          rec.metadata?.display_name ?? pickDisplayName(rec.capabilities),
        capabilities: rec.capabilities,
      };
    } catch {
      return null;
    }
  }

  async submitReputation(
    toAmid: string,
    sessionId: string,
    score: number,
    tags: string[] = [],
  ): Promise<boolean> {
    // AGT's POST /v1/agents/{did}/reputation expects {score: 0..1, reason}.
    // Vendored callers pass arbitrary integer/range scores — clamp + scale
    // into [0, 1] so we don't silently 422 on the AGT side.
    const clamped = Math.max(0, Math.min(1, score > 1 ? score / 100 : score));
    const reason = tags.length > 0 ? `session=${sessionId} tags=${tags.join(",")}` : `session=${sessionId}`;
    const registryUrl = this.options.registryUrl.replace(/\/$/, "");
    try {
      const resp = await fetch(
        `${registryUrl}/v1/agents/${encodeURIComponent(toAmid)}/reputation`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ score: clamped, reason }),
        },
      );
      if (!resp.ok) {
        let body = "";
        try {
          body = (await resp.text()).slice(0, 512);
        } catch {
          /* body read failure is non-fatal */
        }
        console.warn(
          `[agt-transport] submitReputation rejected: ${resp.status} ${resp.statusText}` +
            (body ? ` body=${body}` : ""),
        );
        return false;
      }
      return true;
    } catch (err) {
      console.warn(
        `[agt-transport] submitReputation network error: ${(err as Error).message}`,
      );
      return false;
    }
  }

  /**
   * Bump our own `last_seen` on the AGT registry so we remain
   * discoverable. AGT registry has no autonomous presence model and
   * never updates `last_seen` after register, so without this every
   * agent silently goes stale 90s after spawn (see app.py heartbeat
   * endpoint comment + agt-tools/agt.ts STALE_AFTER_MS).
   *
   * Best-effort: 404 is treated as fatal-once (registry forgot us —
   * our register-on-reconnect path will fix it on next connect),
   * everything else is logged + swallowed.
   */
  private async pingRegistryPresence(): Promise<void> {
    if (!this.client?.isConnected) return;
    const did = this.options.identity.agentId;
    if (!did) return;
    const registryUrl = this.options.registryUrl.replace(/\/$/, "");
    try {
      const resp = await fetch(
        `${registryUrl}/v1/agents/${encodeURIComponent(did)}/heartbeat`,
        { method: "POST" },
      );
      if (!resp.ok && resp.status !== 404) {
        // 404 is benign — happens during the small window between
        // a registry restart and our re-register. Anything else is
        // worth surfacing once.
        console.warn(
          `[agt-transport] registry heartbeat ${resp.status} ${resp.statusText}`,
        );
      }
    } catch (err) {
      // Network error — registry pod restart, transient DNS, etc.
      // Mesh send keeps working; the next tick will retry.
      console.warn(
        `[agt-transport] registry heartbeat error: ${(err as Error).message}`,
      );
    }
  }

  /**
   * No-op: AGT MeshClient always enforces KNOCK gating. Kept for IMeshTransport
   * parity with the vendored adapter (which has a per-instance toggle).
   */
  enableKnockEnforcement(): void {
    /* always-on in AGT */
  }

  // ── Diagnostic event hooks (registered before connect; bridged in connect) ──

  onError(handler: (kind: string, fromAmid: string, detail: string) => void): void {
    this._errorHandlers.push(handler);
  }

  onE2EVerified(handler: (peerAmid: string, isFirstPeer: boolean) => void): void {
    this._e2eVerifiedHandlers.push(handler);
  }

  onDisconnect(handler: (reason: "client" | "server" | "ws-error", code?: number) => void): void {
    this._disconnectHandlers.push(handler);
  }
}

function pickDisplayName(capabilities: string[] | undefined): string | undefined {
  if (!capabilities || capabilities.length === 0) return undefined;
  // Convention: MeshClient registers displayName as the first capability.
  // Fall back to the first non-DID-looking entry otherwise.
  const first = capabilities[0];
  if (first && !first.startsWith("did:")) return first;
  return capabilities.find((c) => !c.startsWith("did:"));
}
