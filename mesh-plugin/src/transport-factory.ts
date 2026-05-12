// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Mesh transport factory. Currently has a single provider — the upstream
 * Microsoft Agent Governance Toolkit SDK (`AgtTransport`). The
 * provider-selection seam is preserved (returning IMeshTransport) so a
 * second provider can be re-introduced without touching call sites.
 *
 * The legacy vendored AgentMesh fork was removed in Phase 5.2 once
 * the upstream AGT SDK reached feature parity.
 */

import type { IMeshIdentity, IMeshTransport } from "./transport-interface.js";

export type MeshProvider = "agt";

/** Identity shape accepted by the factory: raw Ed25519 signing keys. */
export interface UnifiedIdentity {
  amid: string;
  did?: string;
  signingPublicKey: Uint8Array | Buffer;
  signingPrivateKey: Uint8Array | Buffer;
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
 * Resolve the provider from the environment. Currently always returns
 * "agt" — kept as a function so future providers can be wired in.
 * `AZURECLAW_MESH_PROVIDER` is still honored as an env knob but only
 * "agt" is supported; unknown values fall back to the same default.
 */
export function resolveMeshProvider(
  _env: NodeJS.ProcessEnv = process.env,
): MeshProvider {
  return "agt";
}

/** Create an AGT-backed `IMeshTransport`. */
export async function createMeshTransport(
  config: MeshTransportFactoryConfig,
  _env: NodeJS.ProcessEnv = process.env,
): Promise<IMeshTransport> {
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

function toUint8(b: Uint8Array | Buffer): Uint8Array {
  return b instanceof Uint8Array ? b : new Uint8Array(b);
}
