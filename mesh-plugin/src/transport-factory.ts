// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Mesh transport factory. Selects between the vendored AgentMesh SDK
 * (MeshConnection) and the upstream Microsoft AGT SDK (AgtTransport)
 * based on the AZURECLAW_MESH_PROVIDER environment variable.
 *
 * Default is "vendored" so the swap is opt-in and zero-risk for existing
 * deployments. Callers should treat the returned object as an IMeshTransport
 * — provider-specific extensions remain accessible by narrowing the type
 * if needed during the migration window.
 */

import type { IMeshIdentity, IMeshTransport } from "./transport-interface.js";

export type MeshProvider = "vendored" | "agt";

/**
 * Unified identity shape accepted by the factory. Matches the vendored
 * MeshIdentity (which carries everything we need for both providers):
 * raw Ed25519 keys for AGT, plus the SDK-native Identity for vendored.
 *
 * We avoid importing the concrete MeshIdentity type to keep the factory
 * decoupled from the vendored SDK at the type level.
 */
export interface UnifiedIdentity {
  amid: string;
  did?: string;
  signingPublicKey: Uint8Array | Buffer;
  signingPrivateKey: Uint8Array | Buffer;
  /** SDK-native Identity (vendored only). */
  sdkIdentity?: unknown;
}

export interface MeshTransportFactoryConfig {
  relayUrl: string;
  registryUrl: string;
  identity: UnifiedIdentity;
  plaintextPeers?: string[];
  capabilities?: string[];
  displayName?: string;
}

/** Re-export so legacy callers can `import type { MeshTransportConfig }`. */
export type MeshTransportConfig = MeshTransportFactoryConfig;

/**
 * Resolve the provider from the environment. Anything other than "agt"
 * (case-insensitive) maps to "vendored" so typos fall back to the safe path.
 */
export function resolveMeshProvider(
  env: NodeJS.ProcessEnv = process.env,
): MeshProvider {
  const raw = (env.AZURECLAW_MESH_PROVIDER || "").trim().toLowerCase();
  return raw === "agt" ? "agt" : "vendored";
}

/**
 * Create an IMeshTransport using the configured provider. Throws if the
 * caller failed to supply the identity shape required by the active
 * provider — better to fail loudly at construction than to mis-wire keys.
 */
export async function createMeshTransport(
  config: MeshTransportFactoryConfig,
  env: NodeJS.ProcessEnv = process.env,
): Promise<IMeshTransport> {
  const provider = resolveMeshProvider(env);

  if (provider === "agt") {
    // Adapt the unified identity into the IMeshIdentity shape AgtTransport expects.
    const agtIdentity: IMeshIdentity = {
      agentId: config.identity.did ?? config.identity.amid,
      signingPrivateKey: toUint8(config.identity.signingPrivateKey),
      signingPublicKey: toUint8(config.identity.signingPublicKey),
    };
    const { AgtTransport } = await import("./agt-transport.js");
    return new AgtTransport({
      relayUrl: config.relayUrl,
      registryUrl: config.registryUrl,
      identity: agtIdentity,
      plaintextPeers: config.plaintextPeers,
    });
  }

  if (!config.identity.sdkIdentity) {
    throw new Error(
      "AZURECLAW_MESH_PROVIDER=vendored requires identity.sdkIdentity (use loadOrCreateIdentity()).",
    );
  }
  const { MeshConnection } = await import("./connection.js");
  // MeshConnection's ConnectionConfig is private to that module; the runtime
  // shape matches what we accept here so we cast at the seam to keep the
  // factory's surface narrow.
  return new MeshConnection({
    relayUrl: config.relayUrl,
    registryUrl: config.registryUrl,
    identity: config.identity,
    plaintextPeers: config.plaintextPeers,
    capabilities: config.capabilities,
    displayName: config.displayName,
  } as unknown as ConstructorParameters<typeof MeshConnection>[0]);
}

function toUint8(b: Uint8Array | Buffer): Uint8Array {
  return b instanceof Uint8Array ? b : new Uint8Array(b);
}
