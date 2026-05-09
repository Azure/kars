// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT Migration — Transport abstraction interface.
 *
 * Extracted from MeshConnection's public surface so the AGT adapter
 * implements the same contract without changing callers.
 *
 * Phase 1: MeshConnection implements IMeshTransport (vendored @agentmesh/sdk).
 * Phase 2: AgtTransport implements IMeshTransport (@microsoft/agent-governance-sdk 3.5.0).
 * Phase 3: Swap implementation behind AZURECLAW_MESH_PROVIDER flag.
 * Phase 5: Extended with the full surface index.ts uses (inbox / file
 *          transfer / discover / sendWithAck / waitForMessage / pingPeer /
 *          resolveAmid). AgtTransport composes a LocalInbox to satisfy
 *          the inbox half; file/ping/ack helpers are app-layer protocols
 *          on top of plain `send`, so both transports share the same
 *          implementations (delegated through the interface).
 * Phase 7: Drop vendored fork.
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
}

