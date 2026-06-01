// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, beforeEach, vi, type Mock } from "vitest";
import { AgtTransport, __setAgtSdkForTesting } from "./agt-transport.js";
import type { IMeshIdentity } from "./transport-interface.js";

interface FakeClient {
  isConnected: boolean;
  connect: Mock;
  disconnect: Mock;
  reconnect: Mock;
  send: Mock;
  onMessage: Mock;
  onKnock: Mock;
  sendHeartbeat: Mock;
  addPlaintextPeer: Mock;
  removePlaintextPeer: Mock;
  isPlaintextPeer: Mock;
  establishSessionWithPeer: Mock;
  __msgHandler?: (from: string, payload: unknown, isPlaintext: boolean) => void;
  __knockHandler?: (from: string, intent: unknown) => Promise<boolean>;
}

function makeFakeClient(): FakeClient {
  const c: FakeClient = {
    isConnected: false,
    connect: vi.fn(async () => {
      c.isConnected = true;
    }),
    disconnect: vi.fn(async () => {
      c.isConnected = false;
    }),
    reconnect: vi.fn(async () => {}),
    send: vi.fn(async () => {}),
    onMessage: vi.fn((h: unknown) => {
      c.__msgHandler = h as FakeClient["__msgHandler"];
    }),
    onKnock: vi.fn((h: unknown) => {
      c.__knockHandler = h as FakeClient["__knockHandler"];
    }),
    sendHeartbeat: vi.fn(),
    addPlaintextPeer: vi.fn(),
    removePlaintextPeer: vi.fn(),
    isPlaintextPeer: vi.fn(() => false),
    establishSessionWithPeer: vi.fn(async () => {}),
  };
  return c;
}

const identity: IMeshIdentity = {
  agentId: "did:agentmesh:test-agent",
  signingPrivateKey: new Uint8Array(32),
  signingPublicKey: new Uint8Array(32),
};

describe("AgtTransport", () => {
  let fakeClient: FakeClient;
  let MeshClient: Mock;

  beforeEach(() => {
    fakeClient = makeFakeClient();
    // Vitest 4 enforces constructor semantics on mocks invoked via
    // `new`. Use `vi.fn(function(...) { return fakeClient; })` (note
    // `function` keyword — arrow fns aren't constructable) so that
    // `new sdk.MeshClient(...)` resolves to the test fake.
    MeshClient = vi.fn(function () {
      return fakeClient;
    });
    // Same pattern for X3DHKeyManager — a real class works cleanly
    // with `new`. Vitest 3's arrow-function trick is no longer allowed.
    class FakeX3DHKeyManager {
      generateSignedPreKey() {
        return {
          keyId: 1,
          publicKey: new Uint8Array(32),
          signature: new Uint8Array(64),
        };
      }
      generateOneTimePreKeys() {
        return [];
      }
    }
    const X3DHKeyManager = FakeX3DHKeyManager;
    __setAgtSdkForTesting({
      MeshClient: MeshClient as unknown as never,
      X3DHKeyManager: X3DHKeyManager as unknown as never,
    });
  });

  it("connects and disconnects, tracking state", async () => {
    const t = new AgtTransport({
      relayUrl: "http://localhost:8083",
      registryUrl: "http://localhost:8082",
      identity,
    });
    expect(t.isConnected).toBe(false);
    await t.connect();
    expect(t.isConnected).toBe(true);
    expect(MeshClient).toHaveBeenCalledTimes(1);
    expect(fakeClient.connect).toHaveBeenCalledTimes(1);

    await t.disconnect();
    expect(t.isConnected).toBe(false);
    expect(fakeClient.disconnect).toHaveBeenCalledTimes(1);
  });

  it("forwards send to underlying MeshClient", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    await t.connect();
    await t.send("did:agentmesh:peer", { hello: "world" });
    expect(fakeClient.send).toHaveBeenCalledWith("did:agentmesh:peer", {
      hello: "world",
    });
  });

  it("throws if send is called before connect", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    await expect(t.send("x", {})).rejects.toThrow(/not connected/i);
  });

  it("invokes onMessage handlers when SDK delivers a frame", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    const seen: Array<[string, unknown]> = [];
    t.onMessage((from, payload) => seen.push([from, payload]));
    await t.connect();
    fakeClient.__msgHandler?.("peerA", { x: 1 }, false);
    expect(seen).toEqual([["peerA", { x: 1 }]]);
  });

  it("rejects knock when any handler rejects", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    t.onKnock(async () => ({ accept: true }));
    t.onKnock(async () => ({ accept: false }));
    await t.connect();
    const accepted = await fakeClient.__knockHandler?.("peer", {});
    expect(accepted).toBe(false);
  });

  it("accepts knock when all handlers accept (and when none registered)", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    await t.connect();
    expect(await fakeClient.__knockHandler?.("peer", {})).toBe(true);
  });

  it("tracks plaintextPeers locally and forwards to client when connected", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
      plaintextPeers: ["initial"],
    });
    expect(t.getPlaintextPeers()).toContain("initial");
    expect(t.isPlaintextPeer("initial")).toBe(true);
    await t.connect();
    t.addPlaintextPeer("late");
    expect(fakeClient.addPlaintextPeer).toHaveBeenCalledWith("late");
    t.removePlaintextPeer("initial");
    expect(t.isPlaintextPeer("initial")).toBe(false);
  });

  it("agentId returns the identity's DID", () => {
    const t = new AgtTransport({
      relayUrl: "ws://r",
      registryUrl: "http://reg",
      identity,
    });
    expect(t.agentId).toBe("did:agentmesh:test-agent");
  });
});

