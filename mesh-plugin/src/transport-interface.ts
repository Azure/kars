// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT Migration — Transport abstraction interface.
 *
 * Extracted from MeshConnection's public surface so the AGT adapter
 * implements the same contract without changing callers.
 *
 * Phase 5.2: Vendored fork removed; AgtTransport
 * (@microsoft/agent-governance-sdk) is the only implementation, plus
 * the LocalInbox composition for file transfer / discover / sendWithAck
 * / waitForMessage / pingPeer / resolveAmid.
 */

import type { InboxDiagnostics, InboxMessage } from "./local-inbox.js";

export type { InboxDiagnostics, InboxMessage };

export interface IMeshIdentity {
  /** Stable agent ID (DID or AMID base58). */
  readonly agentId: string;
  /** Ed25519 signing private key (32 bytes seed). */
  readonly signingPrivateKey: Uint8Array;
  /** Ed25519 signing public key (32 bytes). */
  readonly signingPublicKey: Uint8Array;
}

export interface DiscoveredPeer {
  /** Stable peer ID (DID or AMID — caller-side normalised). */
  amid: string;
  displayName?: string;
  capabilities?: string[];
}

export interface FileTransferAck {
  success: boolean;
  file_name: string;
  saved_to?: string;
  error?: string;
}

export interface IMeshTransport {
  // ── Connection lifecycle ─────────────────────────────────────
  connect(opts?: { capabilities?: string[]; displayName?: string }): Promise<void>;
  disconnect(): Promise<void>;
  readonly isConnected: boolean;
  readonly agentId: string;
  /** Alias for agentId — kept for legacy callers using the AMID nomenclature. */
  readonly amid: string;

  // ── Messaging ────────────────────────────────────────────────
  send(toAmid: string, payload: unknown): Promise<string | undefined>;
  onMessage(handler: (fromAmid: string, payload: unknown) => void): void;
  onKnock(
    handler: (fromAmid: string, intent: unknown) => Promise<{ accept: boolean }>,
  ): void;

  // ── Plaintext peers (Rust controller compat) ─────────────────
  addPlaintextPeer(amid: string): void;
  removePlaintextPeer(amid: string): void;
  isPlaintextPeer(amid: string): boolean;
  getPlaintextPeers(): string[];

  // ── Discovery ────────────────────────────────────────────────
  discover(opts?: {
    capability?: string;
    capabilities?: string[];
    limit?: number;
  }): Promise<DiscoveredPeer[]>;

  /** Resolve a friendly name to an AMID/DID via the registry. */
  resolveAmid(name: string): Promise<string | null>;

  // ── Inbox ────────────────────────────────────────────────────
  getInbox(limit?: number): InboxMessage[];
  markRead(ids: string[]): number;
  getUnreadCount(): number;
  drainInbox(): InboxMessage[];
  consumeInbox(predicate: (msg: InboxMessage) => boolean): InboxMessage[];
  waitForMessage<T>(
    predicate: (content: unknown, from: string) => T | null,
    timeoutMs?: number,
    opts?: { consume?: boolean },
  ): Promise<T>;
  waitForInbox(timeoutMs: number): Promise<boolean>;
  getDiagnostics(): InboxDiagnostics;

  // ── App-layer helpers ────────────────────────────────────────
  sendWithAck<T>(
    toAmid: string,
    payload: unknown,
    ackPredicate: (content: unknown, from: string) => T | null,
    opts?: { timeoutMs?: number; retries?: number; retryDelayMs?: number },
  ): Promise<T>;
  pingPeer(
    toAmid: string,
    opts?: { timeoutMs?: number; retries?: number },
  ): Promise<{ rttMs: number; pong: Record<string, unknown> }>;
  sendFile(
    toAmid: string,
    filePath: string,
    opts?: { description?: string; timeoutMs?: number; retries?: number },
  ): Promise<FileTransferAck>;
  handleFileTransfer(
    fromAmid: string,
    message: Record<string, unknown>,
    saveDir: string,
  ): Promise<{ savedPath: string; fileName: string; sizeBytes: number } | null>;

  // ── Liveness ─────────────────────────────────────────────────
  sendHeartbeat(): void;

  // ── Reputation / lookup (registry RPC) ───────────────────────
  /**
   * Look up a peer's registry record by AMID.
   * Returns `null` when the registry is unreachable or the AMID is unknown.
   * `reputationScore` is on a 0..1 scale; the caller normalises to whatever
   * threshold scheme it uses.
   */
  lookup(amid: string): Promise<{ reputationScore?: number; displayName?: string; capabilities?: string[] } | null>;

  /**
   * Submit a reputation review for a completed mesh interaction.
   * `score` is 0..1. Returns true iff the registry accepted the review.
   * Best-effort: never throws — registry-down paths return false.
   */
  submitReputation(toAmid: string, sessionId: string, score: number, tags?: string[]): Promise<boolean>;

  // ── Governance toggles ───────────────────────────────────────
  /**
   * Enable strict KNOCK enforcement: reject encrypted messages from peers
   * that have not completed a KNOCK handshake. AGT-backed transports always
   * enforce this and treat the call as a no-op for compatibility.
   */
  enableKnockEnforcement(): void;

  // ── Diagnostic event hooks ───────────────────────────────────
  /**
   * Surface protocol-layer errors. `kind` is one of:
   *   - `"ws"`              WebSocket error (handshake or mid-stream)
   *   - `"decrypt_failed"`  message decrypt threw or session was missing
   *   - `"no_session"`      encrypted message arrived but no X3DH session exists
   *   - `"session_desync"`  ratchet desync (recoverable on next send)
   *   - `"knock_rejected"`  policy/trust rejected an inbound KNOCK
   * Multiple handlers may be registered; thrown handler errors are swallowed.
   */
  onError(handler: (kind: string, fromAmid: string, detail: string) => void): void;

  /**
   * Fires the first time a peer's encrypted channel is verified end-to-end
   * (X3DH + Double Ratchet completed and we successfully decrypted a frame).
   * `isFirstPeer` is true only for the very first verified peer of the
   * client's lifetime; subsequent peers fire with `false`.
   */
  onE2EVerified(handler: (peerAmid: string, isFirstPeer: boolean) => void): void;

  /**
   * Fires when the underlying WebSocket transitions from connected to
   * disconnected. `reason` is `"client"` for caller-initiated disconnects,
   * `"server"` for relay-side / network drops, and `"ws-error"` for an
   * error event on an already-connected socket.
   */
  onDisconnect(handler: (reason: "client" | "server" | "ws-error", code?: number) => void): void;
}

