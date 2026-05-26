// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, beforeEach, vi } from "vitest";
import { resolveMeshProvider, createMeshTransport } from "./transport-factory.js";
import { __setAgtSdkForTesting } from "./agt-transport.js";

const agtUnifiedIdentity = {
  amid: "did:agentmesh:factory-test",
  did: "did:agentmesh:factory-test",
  signingPrivateKey: new Uint8Array(32),
  signingPublicKey: new Uint8Array(32),
};

describe("resolveMeshProvider", () => {
  it("returns 'agt' regardless of env (vendored provider removed in Phase 5.2)", () => {
    expect(resolveMeshProvider({})).toBe("agt");
    expect(resolveMeshProvider({ KARS_MESH_PROVIDER: "agt" })).toBe("agt");
    expect(resolveMeshProvider({ KARS_MESH_PROVIDER: "vendored" })).toBe("agt");
    expect(resolveMeshProvider({ KARS_MESH_PROVIDER: "fancy" })).toBe("agt");
    expect(resolveMeshProvider({ KARS_MESH_PROVIDER: "" })).toBe("agt");
  });
});

describe("createMeshTransport", () => {
  beforeEach(() => {
    __setAgtSdkForTesting({
      MeshClient: vi.fn(() => ({
        isConnected: false,
        connect: vi.fn(),
        disconnect: vi.fn(),
        send: vi.fn(),
        onMessage: vi.fn(),
        onKnock: vi.fn(),
        addPlaintextPeer: vi.fn(),
        removePlaintextPeer: vi.fn(),
        isPlaintextPeer: vi.fn(() => false),
      })) as unknown as never,
      X3DHKeyManager: vi.fn(() => ({
        generateSignedPreKey: vi.fn(() => ({
          keyId: 1,
          publicKey: new Uint8Array(32),
          signature: new Uint8Array(64),
        })),
        generateOneTimePreKeys: vi.fn(() => []),
      })) as unknown as never,
    });
  });

  it("constructs an AgtTransport", async () => {
    const t = await createMeshTransport(
      {
        relayUrl: "ws://r",
        registryUrl: "http://reg",
        identity: agtUnifiedIdentity,
      },
      { KARS_MESH_PROVIDER: "agt" },
    );
    expect(t.constructor.name).toBe("AgtTransport");
    expect(t.agentId).toBe("did:agentmesh:factory-test");
  });

  it("ignores legacy provider=vendored and still returns AgtTransport", async () => {
    const t = await createMeshTransport(
      {
        relayUrl: "ws://r",
        registryUrl: "http://reg",
        identity: agtUnifiedIdentity,
      },
      { KARS_MESH_PROVIDER: "vendored" },
    );
    expect(t.constructor.name).toBe("AgtTransport");
  });
});
