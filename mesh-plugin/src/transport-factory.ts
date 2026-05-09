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

export interface MeshTransportFactoryConfig {
  relayUrl: string;
  registryUrl: string;
  /** Vendored-style identity (with .sdkIdentity, .amid). Required when provider=vendored. */
  vendoredIdentity?: unknown;
  /** AGT-style raw-key identity. Required when provider=agt. */
  agtIdentity?: IMeshIdentity;
  plaintextPeers?: string[];
  capabilities?: string[];
  displayName?: string;
}

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
    if (!config.agtIdentity) {
      throw new Error(
        "AZURECLAW_MESH_PROVIDER=agt requires an agtIdentity (IMeshIdentity).",
      );
    }
    const { AgtTransport } = await import("./agt-transport.js");
    return new AgtTransport({
      relayUrl: config.relayUrl,
      registryUrl: config.registryUrl,
      identity: config.agtIdentity,
      plaintextPeers: config.plaintextPeers,
    });
  }

  if (!config.vendoredIdentity) {
    throw new Error(
      "AZURECLAW_MESH_PROVIDER=vendored requires a vendoredIdentity.",
    );
  }
  const { MeshConnection } = await import("./connection.js");
  // MeshConnection's ConnectionConfig is private to that module; the runtime
  // shape matches what we accept here so we cast at the seam to keep the
  // factory's surface narrow.
  return new MeshConnection({
    relayUrl: config.relayUrl,
    registryUrl: config.registryUrl,
    identity: config.vendoredIdentity,
    plaintextPeers: config.plaintextPeers,
    capabilities: config.capabilities,
    displayName: config.displayName,
  } as unknown as ConstructorParameters<typeof MeshConnection>[0]);
}
