/**
 * AGT-backed mesh transport — implements IMeshTransport using
 * @microsoft/agentmesh-sdk (MeshClient, X3DHKeyManager, SecureChannel).
 *
 * This replaces the vendored @agentmesh/sdk for all mesh operations.
 * Clean-room implementation against the AgentMesh Wire Protocol v1.0.
 */

import type { IMeshTransport, IMeshIdentity } from "./transport-interface.js";

// AGT SDK imports — @microsoft/agentmesh-sdk
// Lazy-loaded to allow graceful failure if the package isn't installed.
let agtSdk: typeof import("@microsoft/agentmesh-sdk") | null = null;

async function loadAgtSdk() {
  if (agtSdk) return agtSdk;
  try {
    agtSdk = await import("@microsoft/agentmesh-sdk");
    return agtSdk;
  } catch (e: unknown) {
    throw new Error(
      `@microsoft/agentmesh-sdk is required for AGT mesh transport. ` +
      `Install it: npm install @microsoft/agentmesh-sdk@^3.2.0. ` +
      `Error: ${(e as Error)?.message ?? e}`,
    );
  }
}

export interface AgtTransportOptions {
  relayUrl: string;
  registryUrl: string;
  identity: IMeshIdentity;
  displayName?: string;
  wsFactory?: (url: string) => unknown;
  plaintextPeers?: string[];
}

export class AgtTransport implements IMeshTransport {
  private options: AgtTransportOptions;
  private client: any | null = null;
  private messageHandlers: Array<(fromId: string, payload: unknown) => void> = [];
  private knockHandlers: Array<(fromId: string, intent: unknown) => Promise<{ accept: boolean }>> = [];
  private _isConnected = false;
  private _plaintextPeers: Set<string>;

  constructor(options: AgtTransportOptions) {
    this.options = options;
    this._plaintextPeers = new Set(options.plaintextPeers ?? []);
  }

  get isConnected(): boolean {
    return this._isConnected && this.client?.isConnected === true;
  }

  get agentId(): string {
    return this.options.identity.agentId;
  }

  async connect(opts?: { capabilities?: string[]; displayName?: string }): Promise<void> {
    const sdk = await loadAgtSdk();

    const keyManager = new sdk.X3DHKeyManager(
      this.options.identity.signingPrivateKey,
      this.options.identity.signingPublicKey,
    );
    keyManager.generateSignedPreKey();
    keyManager.generateOneTimePreKeys(10);

    this.client = new sdk.MeshClient({
      relayUrl: this.options.relayUrl,
      registryUrl: this.options.registryUrl,
      keyManager,
      agentDid: this.options.identity.agentId,
      displayName: opts?.displayName ?? this.options.displayName,
      wsFactory: this.options.wsFactory as any,
      plaintextPeers: [...this._plaintextPeers],
    });

    // Wire message handler
    this.client.onMessage((from: string, payload: unknown, _isPlaintext: boolean) => {
      for (const handler of this.messageHandlers) {
        handler(from, payload);
      }
    });

    // Wire KNOCK handler — delegate to registered handlers
    this.client.onKnock(async (from: string, intent: unknown) => {
      for (const handler of this.knockHandlers) {
        const result = await handler(from, intent);
        if (!result.accept) return false;
      }
      return true; // accept by default if no handler rejects
    });

    await this.client.connect();
    this._isConnected = true;
  }

  async disconnect(): Promise<void> {
    if (this.client) {
      await this.client.disconnect();
    }
    this._isConnected = false;
    this.client = null;
  }

  async send(toId: string, payload: unknown): Promise<string | undefined> {
    if (!this.client) throw new Error("Not connected");
    await this.client.send(toId, payload);
    return undefined;
  }

  onMessage(handler: (fromId: string, payload: unknown) => void): void {
    this.messageHandlers.push(handler);
  }

  onKnock(handler: (fromId: string, intent: unknown) => Promise<{ accept: boolean }>): void {
    this.knockHandlers.push(handler);
  }

  addPlaintextPeer(id: string): void {
    this._plaintextPeers.add(id);
    this.client?.addPlaintextPeer(id);
  }

  removePlaintextPeer(id: string): void {
    this._plaintextPeers.delete(id);
    this.client?.removePlaintextPeer(id);
  }

  isPlaintextPeer(id: string): boolean {
    return this._plaintextPeers.has(id);
  }

  getPlaintextPeers(): string[] {
    return [...this._plaintextPeers];
  }

  /**
   * Discovery via the AGT registry REST API.
   * Queries GET /agents?capability=<cap> for each requested capability.
   */
  async discover(opts?: { capabilities?: string[]; limit?: number }): Promise<Array<{ id: string; displayName?: string; capabilities?: string[] }>> {
    const limit = opts?.limit ?? 50;
    const registryUrl = this.options.registryUrl.replace(/\/$/, "");

    if (!opts?.capabilities || opts.capabilities.length === 0) {
      // No capability filter — try listing all agents
      try {
        const resp = await fetch(`${registryUrl}/agents?limit=${limit}`);
        if (!resp.ok) return [];
        const data = await resp.json() as Array<Record<string, unknown>>;
        return data.slice(0, limit).map(mapAgent);
      } catch {
        return [];
      }
    }

    // Search by each capability, deduplicate
    const seen = new Map<string, { id: string; displayName?: string; capabilities?: string[] }>();
    await Promise.all(
      opts.capabilities.map(async (cap) => {
        try {
          const resp = await fetch(`${registryUrl}/agents?capability=${encodeURIComponent(cap)}&limit=${limit}`);
          if (!resp.ok) return;
          const data = await resp.json() as Array<Record<string, unknown>>;
          for (const a of data) {
            const mapped = mapAgent(a);
            if (mapped.id && !seen.has(mapped.id)) seen.set(mapped.id, mapped);
          }
        } catch { /* best-effort */ }
      }),
    );

    return Array.from(seen.values()).slice(0, limit);
  }

  /**
   * Low-level search by single capability — backward compat with
   * connection.ts's discover() method.
   */
  async search(capability: string, opts?: { limit?: number }): Promise<Array<Record<string, unknown>>> {
    const registryUrl = this.options.registryUrl.replace(/\/$/, "");
    const limit = opts?.limit ?? 50;
    try {
      const resp = await fetch(`${registryUrl}/agents?capability=${encodeURIComponent(capability)}&limit=${limit}`);
      if (!resp.ok) return [];
      return await resp.json() as Array<Record<string, unknown>>;
    } catch {
      return [];
    }
  }

  sendHeartbeat(): void {
    this.client?.sendHeartbeat();
  }
}

function mapAgent(a: Record<string, unknown>): { id: string; displayName?: string; capabilities?: string[] } {
  return {
    id: String(a.amid ?? a.agent_id ?? a.id ?? ""),
    displayName: (a.displayName ?? a.display_name) as string | undefined,
    capabilities: a.capabilities as string[] | undefined,
  };
}
