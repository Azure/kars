/**
 * AGT-backed mesh transport — implements IMeshTransport using
 * @microsoft/agentmesh-sdk (MeshClient, X3DHKeyManager, SecureChannel).
 *
 * This replaces the vendored @agentmesh/sdk for all mesh operations.
 * Clean-room implementation against the AgentMesh Wire Protocol v1.0.
 */

import type { IMeshTransport, IMeshIdentity } from "./transport-interface.js";

// AGT SDK imports — @microsoft/agentmesh-sdk
// These will resolve once the dependency is swapped in package.json
let agtSdk: typeof import("@microsoft/agentmesh-sdk") | null = null;

async function loadAgtSdk() {
  if (agtSdk) return agtSdk;
  try {
    agtSdk = await import("@microsoft/agentmesh-sdk");
    return agtSdk;
  } catch (e: unknown) {
    throw new Error(
      `@microsoft/agentmesh-sdk is required for AGT mesh transport. ` +
      `Install it: npm install @microsoft/agentmesh-sdk. ` +
      `Error: ${(e as Error)?.message ?? e}`,
    );
  }
}

export interface AgtTransportOptions {
  relayUrl: string;
  registryUrl: string;
  identity: IMeshIdentity;
  displayName?: string;
  wsFactory?: (url: string) => WebSocket;
  plaintextPeers?: string[];
}

export class AgtTransport implements IMeshTransport {
  private options: AgtTransportOptions;
  private client: InstanceType<typeof import("@microsoft/agentmesh-sdk").MeshClient> | null = null;
  private messageHandlers: Array<(fromId: string, payload: unknown) => void> = [];
  private _isConnected = false;

  constructor(options: AgtTransportOptions) {
    this.options = options;
  }

  get isConnected(): boolean {
    return this._isConnected && this.client?.isConnected === true;
  }

  get agentId(): string {
    return this.options.identity.agentId;
  }

  async connect(): Promise<void> {
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
      displayName: this.options.displayName,
      wsFactory: this.options.wsFactory,
      plaintextPeers: this.options.plaintextPeers,
    });

    // Wire message handler
    this.client.onMessage((from: string, payload: unknown, _isPlaintext: boolean) => {
      for (const handler of this.messageHandlers) {
        handler(from, payload);
      }
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
    return undefined; // MeshClient doesn't return message IDs from send
  }

  onMessage(handler: (fromId: string, payload: unknown) => void): void {
    this.messageHandlers.push(handler);
  }

  addPlaintextPeer(id: string): void {
    this.client?.addPlaintextPeer(id);
  }

  removePlaintextPeer(id: string): void {
    this.client?.removePlaintextPeer(id);
  }

  isPlaintextPeer(id: string): boolean {
    return this.client?.isPlaintextPeer(id) ?? false;
  }

  async discover(opts?: { capabilities?: string[]; limit?: number }): Promise<Array<{ id: string; capabilities: string[] }>> {
    // Discovery goes through the registry REST API
    // For now, return empty — full registry client integration in next PR
    console.warn("[agt-transport] discover() not yet wired to AGT registry");
    return [];
  }

  sendHeartbeat(): void {
    this.client?.sendHeartbeat();
  }
}
