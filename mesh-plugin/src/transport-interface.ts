// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT Migration — Transport abstraction interface.
 *
 * Extracted from MeshConnection's public surface so the AGT adapter
 * (Phase 2) can implement the same contract without changing callers.
 *
 * Phase 1: MeshConnection implements IMeshTransport (grounded).
 * Phase 2: AgtTransport implements IMeshTransport (AGT SDK-backed).
 * Phase 3: Swap implementation, remove vendor.
 */

export interface IMeshTransport {
  // ── Connection lifecycle ─────────────────────────────────────
  connect(): Promise<void>;
  disconnect(): Promise<void>;
  readonly isConnected: boolean;

  // ── Messaging ────────────────────────────────────────────────
  send(toAmid: string, payload: unknown): Promise<string | undefined>;

  // ── Plaintext peers (Rust controller compat) ─────────────────
  addPlaintextPeer(amid: string): void;
  removePlaintextPeer(amid: string): void;
  getPlaintextPeers(): string[];

  // ── Discovery ────────────────────────────────────────────────
  discover(opts?: { capability?: string; limit?: number }): Promise<
    Array<{ amid: string; displayName?: string; capabilities?: string[] }>
  >;
}
