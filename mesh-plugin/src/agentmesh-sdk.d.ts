// Type declarations for vendored @agentmesh/sdk (no published @types package).
declare module "@agentmesh/sdk" {
  export interface IdentityData {
    amid: string;
    signing_public_key: string;
    signing_private_key: string;
    exchange_public_key: string;
    exchange_private_key: string;
    created_at: string;
    framework?: string;
    framework_version?: string;
    [key: string]: unknown;
  }

  export class Identity {
    readonly amid: string;
    readonly signingPublicKey: CryptoKey;
    readonly signingPublicKeyRaw: Uint8Array;
    readonly signingPrivateKey: CryptoKey;
    readonly exchangePublicKey: CryptoKey;
    readonly exchangePublicKeyRaw: Uint8Array;
    readonly exchangePrivateKey: CryptoKey;
    readonly createdAt: Date;

    static generate(): Promise<Identity>;
    static load(storage: unknown, path: string): Promise<Identity>;
    static fromData(data: IdentityData): Promise<Identity>;
    save(storage: unknown, path: string): Promise<void>;
    toData(): Promise<IdentityData>;
    get signingPublicKeyB64(): string;
  }

  export class MemoryStorage {
    constructor();
    get(key: string): Promise<string | null>;
    set(key: string, value: string): Promise<void>;
    delete(key: string): Promise<void>;
  }

  export class AgentMeshClient {
    static fromIdentity(
      identity: Identity,
      opts?: Record<string, unknown>,
    ): AgentMeshClient;
    connect(opts?: Record<string, unknown>): Promise<void>;
    disconnect(): Promise<void>;
    send(to: string, payload: unknown): Promise<string | undefined>;
    onMessage(handler: (from: string, payload: unknown) => void): void;
    onKnock(
      handler: (
        from: string,
        intent: unknown,
      ) => Promise<{ accept: boolean }>,
    ): void;
    addPlaintextPeer(amid: string): void;
    removePlaintextPeer(amid: string): void;
    getPlaintextPeers(): string[];
    search(
      capability: string,
      opts?: Record<string, unknown>,
    ): Promise<unknown[]>;
    get isConnected(): boolean;
    get amid(): string;
  }

  export class RegistryClient {
    constructor(opts?: Record<string, unknown>);
  }

  export const VERSION: string;
}
