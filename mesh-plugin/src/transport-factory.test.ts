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
  it("defaults to vendored when env var is unset", () => {
    expect(resolveMeshProvider({})).toBe("vendored");
  });
  it("returns agt for AZURECLAW_MESH_PROVIDER=agt (case-insensitive)", () => {
    expect(resolveMeshProvider({ AZURECLAW_MESH_PROVIDER: "agt" })).toBe("agt");
    expect(resolveMeshProvider({ AZURECLAW_MESH_PROVIDER: "AGT" })).toBe("agt");
    expect(resolveMeshProvider({ AZURECLAW_MESH_PROVIDER: " agt " })).toBe(
      "agt",
    );
  });
  it("falls back to vendored on unknown values", () => {
    expect(resolveMeshProvider({ AZURECLAW_MESH_PROVIDER: "fancy" })).toBe(
      "vendored",
    );
    expect(resolveMeshProvider({ AZURECLAW_MESH_PROVIDER: "" })).toBe(
      "vendored",
    );
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

  it("constructs an AgtTransport when provider=agt", async () => {
    const t = await createMeshTransport(
      {
        relayUrl: "ws://r",
        registryUrl: "http://reg",
        identity: agtUnifiedIdentity,
      },
      { AZURECLAW_MESH_PROVIDER: "agt" },
    );
    expect(t.constructor.name).toBe("AgtTransport");
    expect(t.agentId).toBe("did:agentmesh:factory-test");
  });

  it("rejects provider=vendored without sdkIdentity", async () => {
    await expect(
      createMeshTransport(
        {
          relayUrl: "ws://r",
          registryUrl: "http://reg",
          identity: agtUnifiedIdentity,
        },
        {},
      ),
    ).rejects.toThrow(/sdkIdentity/);
  });
});
