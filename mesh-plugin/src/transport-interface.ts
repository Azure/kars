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
 * Phase 6: Drop vendored fork.
 */

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

export interface IMeshTransport {
  // ── Connection lifecycle ─────────────────────────────────────
  connect(opts?: { capabilities?: string[]; displayName?: string }): Promise<void>;
  disconnect(): Promise<void>;
  readonly isConnected: boolean;
  readonly agentId: string;

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

  // ── Liveness ─────────────────────────────────────────────────
  sendHeartbeat(): void;
}
