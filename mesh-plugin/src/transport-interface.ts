/**
 * AGT Migration — Transport abstraction interface.
 *
 * This interface defines the contract between the mesh-plugin's
 * application-level code (chunking, file transfer, inbox) and the
 * underlying mesh transport (currently vendored @agentmesh/sdk,
 * migrating to @microsoft/agentmesh-sdk).
 *
 * Phase 1: extract interface from existing MeshConnection.
 * Phase 2: implement AgtTransport behind this interface.
 * Phase 3: swap implementation, remove vendor.
 */

export interface IMeshTransport {
  // ── Connection lifecycle ─────────────────────────────────────
  connect(opts?: { capabilities?: string[]; displayName?: string }): Promise<void>;
  disconnect(): Promise<void>;
  readonly isConnected: boolean;
  readonly agentId: string; // AMID or DID

  // ── Messaging ────────────────────────────────────────────────
  send(toId: string, payload: unknown): Promise<string | undefined>;
  onMessage(handler: (fromId: string, payload: unknown) => void): void;

  // ── KNOCK ────────────────────────────────────────────────────
  onKnock(handler: (fromId: string, intent: unknown) => Promise<{ accept: boolean }>): void;

  // ── Plaintext peers (Rust controller compat) ─────────────────
  addPlaintextPeer(id: string): void;
  removePlaintextPeer(id: string): void;
  isPlaintextPeer(id: string): boolean;
  getPlaintextPeers(): string[];

  // ── Discovery ────────────────────────────────────────────────
  discover(opts?: { capabilities?: string[]; limit?: number }): Promise<Array<{ id: string; displayName?: string; capabilities?: string[] }>>;
  search(capability: string, opts?: { limit?: number }): Promise<Array<Record<string, unknown>>>;

  // ── Heartbeat / presence ─────────────────────────────────────
  sendHeartbeat(): void;
}

/**
 * Identity abstraction — decouples mesh-plugin from a specific
 * identity implementation.
 */
export interface IMeshIdentity {
  /** Agent identifier (AMID or DID) */
  readonly agentId: string;
  /** Ed25519 signing public key (raw 32 bytes) */
  readonly signingPublicKey: Uint8Array;
  /** Ed25519 signing private key (raw 32 bytes) */
  readonly signingPrivateKey: Uint8Array;
}
