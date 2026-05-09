// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT-backed mesh transport — implements IMeshTransport using the
 * upstream Microsoft Agent Governance SDK.
 *
 * Package: @microsoft/agent-governance-sdk (^3.5.0)
 * Module:  @microsoft/agent-governance-sdk/dist/encryption (or default export)
 *
 * Replaces the vendored @agentmesh/sdk for all mesh operations when
 * AZURECLAW_MESH_PROVIDER=agt is set.
 *
 * Wire compatibility: speaks AgentMesh Wire Protocol v1.0 — same wire as
 * our vendored relay. Upstream MeshClient already includes the AzureClaw
 * compatibility features (plaintextPeers, wsFactory hook, KNOCK pending
 * queue). See agent-governance-typescript/src/encryption/mesh-client.ts.
 */

import type {
  IMeshIdentity,
  IMeshTransport,
  DiscoveredPeer,
} from "./transport-interface.js";

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

interface AgtMeshClientOptions {
  relayUrl: string;
  registryUrl: string;
  keyManager: AgtX3DHKeyManager;
  agentDid: string;
  displayName?: string;
  wsFactory?: (url: string) => unknown;
  plaintextPeers?: string[];
  knockTimeout?: number;
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
  /** Number of one-time prekeys to mint at connect. Default: 10. */
  oneTimePreKeyCount?: number;
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

  constructor(options: AgtTransportOptions) {
    this.options = options;
    this._plaintextPeers = new Set(options.plaintextPeers ?? []);
  }

  get isConnected(): boolean {
    return this._connected && this.client?.isConnected === true;
  }

  get agentId(): string {
    return this.options.identity.agentId;
  }

  async connect(opts?: {
    capabilities?: string[];
    displayName?: string;
  }): Promise<void> {
    if (this.isConnected) return;
    const sdk = await loadAgtSdk();

    const keyManager = new sdk.X3DHKeyManager(
      this.options.identity.signingPrivateKey,
      this.options.identity.signingPublicKey,
    );
    keyManager.generateSignedPreKey();
    keyManager.generateOneTimePreKeys(this.options.oneTimePreKeyCount ?? 10);

    this.client = new sdk.MeshClient({
      relayUrl: this.options.relayUrl,
      registryUrl: this.options.registryUrl,
      keyManager,
      agentDid: this.options.identity.agentId,
      displayName: opts?.displayName ?? this.options.displayName,
      wsFactory: this.options.wsFactory,
      plaintextPeers: [...this._plaintextPeers],
      knockTimeout: this.options.knockTimeout,
    });

    // Bridge SDK callbacks → our handler arrays.
    this.client.onMessage((from, payload, _isPlaintext) => {
      for (const handler of this.messageHandlers) {
        try {
          handler(from, payload);
        } catch (e) {
          // Handler errors must not poison the SDK callback chain.
          // eslint-disable-next-line no-console
          console.error("[agt-transport] message handler threw:", e);
        }
      }
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

    await this.client.connect();
    this._connected = true;
  }

  async disconnect(): Promise<void> {
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
   * Supports both single-capability (legacy MeshConnection.discover)
   * and multi-capability (Phase 2 caller convenience). Results are
   * deduplicated by AMID/DID and capped at `limit` (default 50).
   */
  async discover(opts?: {
    capability?: string;
    capabilities?: string[];
    limit?: number;
  }): Promise<DiscoveredPeer[]> {
    const limit = opts?.limit ?? 50;
    const registryUrl = this.options.registryUrl.replace(/\/$/, "");

    const caps: string[] = [];
    if (opts?.capabilities) caps.push(...opts.capabilities);
    if (opts?.capability) caps.push(opts.capability);

    if (caps.length === 0) {
      // No capability filter → list all (best effort; not all registries support this).
      try {
        const resp = await fetch(`${registryUrl}/agents?limit=${limit}`);
        if (!resp.ok) return [];
        const data = (await resp.json()) as Array<Record<string, unknown>>;
        return data.slice(0, limit).map(mapAgent);
      } catch {
        return [];
      }
    }

    const seen = new Map<string, DiscoveredPeer>();
    await Promise.all(
      caps.map(async (cap) => {
        try {
          const resp = await fetch(
            `${registryUrl}/agents?capability=${encodeURIComponent(cap)}&limit=${limit}`,
          );
          if (!resp.ok) return;
          const data = (await resp.json()) as Array<Record<string, unknown>>;
          for (const a of data) {
            const mapped = mapAgent(a);
            if (mapped.amid && !seen.has(mapped.amid))
              seen.set(mapped.amid, mapped);
          }
        } catch {
          // best-effort — registry may be down
        }
      }),
    );
    return Array.from(seen.values()).slice(0, limit);
  }

  sendHeartbeat(): void {
    this.client?.sendHeartbeat();
  }
}

function mapAgent(a: Record<string, unknown>): DiscoveredPeer {
  return {
    amid: String(a.amid ?? a.agent_id ?? a.did ?? a.id ?? ""),
    displayName:
      typeof a.displayName === "string"
        ? a.displayName
        : typeof a.display_name === "string"
          ? (a.display_name as string)
          : undefined,
    capabilities: Array.isArray(a.capabilities)
      ? (a.capabilities as string[])
      : undefined,
  };
}
