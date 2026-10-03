// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, beforeEach, afterEach, vi, type Mock } from "vitest";
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
  onDisconnect: Mock;
  sendHeartbeat: Mock;
  addPlaintextPeer: Mock;
  removePlaintextPeer: Mock;
  isPlaintextPeer: Mock;
  establishSessionWithPeer: Mock;
  __msgHandler?: (from: string, payload: unknown, isPlaintext?: boolean) => void;
  __knockHandler?: (from: string, intent: unknown) => Promise<boolean>;
  __disconnectHandler?: (reason: "client" | "server" | "ws-error", code?: number) => void;
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
    onDisconnect: vi.fn((h: FakeClient["__disconnectHandler"]) => { c.__disconnectHandler = h; }),
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

  afterEach(() => vi.useRealTimers());

  it("serializes overlapping connects and reuses the client, keys and hooks after reconnect", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    let finish!: () => void;
    fakeClient.connect.mockImplementationOnce(() => new Promise<void>(resolve => {
      finish = () => { fakeClient.isConnected = true; resolve(); };
    }));
    const first = t.connect();
    const second = t.connect();
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    expect(fakeClient.connect).toHaveBeenCalledOnce();
    finish();
    await Promise.all([first, second]);
    await t.disconnect();
    await t.connect();
    expect(MeshClient).toHaveBeenCalledOnce();
    expect(MeshClient.mock.calls[0][0].autoReconnect).toBe(false);
    expect(fakeClient.connect).toHaveBeenCalledTimes(2);
    expect(fakeClient.onMessage).toHaveBeenCalledOnce();
    expect(fakeClient.onKnock).toHaveBeenCalledOnce();
    await t.disconnect();
  });

  it("cancels a queued connect before creating an SDK client", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await Promise.all([t.connect(), t.disconnect()]);
    expect(t.isConnected).toBe(false);
    expect(MeshClient).not.toHaveBeenCalled();
    await t.connect();
    expect(t.isConnected).toBe(true);
    await t.disconnect();
  });

  it("awaits an in-flight connect before shutdown and does not report it connected", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    let finish!: () => void;
    fakeClient.connect.mockImplementationOnce(() => new Promise<void>(resolve => {
      finish = () => { fakeClient.isConnected = true; resolve(); };
    }));
    const connecting = t.connect();
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    let stopped = false;
    const stopping = t.disconnect().then(() => { stopped = true; });
    await Promise.resolve();
    expect(stopped).toBe(false);
    expect(t.isConnected).toBe(false);
    finish();
    await Promise.all([connecting, stopping]);
    expect(t.isConnected).toBe(false);
    expect(fakeClient.isConnected).toBe(false);
    await t.connect();
    expect(MeshClient).toHaveBeenCalledOnce();
    expect(t.isConnected).toBe(true);
    await t.disconnect();
  });

  it("does not replace the client after cleanup failure", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    fakeClient.disconnect.mockRejectedValueOnce(new Error("cannot retire"));
    await expect(t.disconnect()).rejects.toThrow("cleanup failed");
    await expect(t.connect()).rejects.toThrow("cleanup failed");
    expect(MeshClient).toHaveBeenCalledOnce();
    expect(fakeClient.connect).toHaveBeenCalledOnce();
    expect(t.isConnected).toBe(false);
    await t.disconnect();
  });

  it("never resumes an application send on a different connection", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    let finish!: () => void;
    fakeClient.establishSessionWithPeer.mockReturnValueOnce(new Promise<void>(resolve => { finish = resolve; }));
    const sending = expect(t.send("peer", "one assignment")).rejects.toThrow("connection changed");
    await t.disconnect();
    await t.connect();
    finish();
    await sending;
    expect(fakeClient.send).not.toHaveBeenCalled();
    await t.disconnect();
  });

  it("reconnects after a remote close without another SDK registration owner", async () => {
    vi.useFakeTimers();
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    fakeClient.isConnected = false;
    fakeClient.__disconnectHandler?.("server", 1006);
    expect(t.isConnected).toBe(false);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(t.isConnected).toBe(true);
    expect(fakeClient.connect).toHaveBeenCalledTimes(2);
    expect(MeshClient).toHaveBeenCalledOnce();
    await t.disconnect();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fakeClient.connect).toHaveBeenCalledTimes(2);
  });

  it("cancels pending automatic reconnect on explicit disconnect", async () => {
    vi.useFakeTimers();
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    fakeClient.isConnected = false;
    fakeClient.__disconnectHandler?.("server", 1006);
    await t.disconnect();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fakeClient.connect).toHaveBeenCalledOnce();
  });

  it("can retry a failure before any socket opens without replacing the key manager", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    fakeClient.connect.mockRejectedValueOnce(new Error("connection refused"));
    await expect(t.connect()).rejects.toThrow("connection refused");
    await t.connect();
    expect(MeshClient).toHaveBeenCalledOnce();
    expect(t.isConnected).toBe(true);
    await t.disconnect();
  });

  it("quarantines uncertain registration instead of re-uploading prekeys", async () => {
    const raw = {
      readyState: 0,
      onopen: null as ((event: object) => void) | null,
      onerror: null, onmessage: null, onclose: null as ((event: object) => void) | null,
      send: vi.fn(),
      close: vi.fn(() => { raw.readyState = 3; raw.onclose?.({ code: 1000 }); }),
    };
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity, wsFactory: () => raw });
    fakeClient.connect.mockImplementationOnce(async () => {
      MeshClient.mock.calls[0][0].wsFactory("ws://r");
      raw.readyState = 1;
      raw.onopen?.({});
      throw new Error("registration response lost");
    });
    await expect(t.connect()).rejects.toThrow("registration outcome is uncertain");
    await expect(t.connect()).rejects.toThrow("registration outcome is uncertain");
    expect(fakeClient.connect).toHaveBeenCalledOnce();
    expect(raw.close).toHaveBeenCalledOnce();
    await t.disconnect();
  });

  it("quarantines a connect deadline even if registration later finishes", async () => {
    vi.useFakeTimers();
    let finish!: () => void;
    fakeClient.connect.mockReturnValueOnce(new Promise<void>(resolve => { finish = resolve; }));
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    const failed = expect(t.connect()).rejects.toThrow("registration outcome is uncertain");
    await vi.advanceTimersByTimeAsync(30_000);
    await failed;
    finish();
    await expect(t.connect()).rejects.toThrow("registration outcome is uncertain");
    expect(fakeClient.connect).toHaveBeenCalledOnce();
    await t.disconnect();
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

  it("establishes the encrypted session before sending", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    let establish!: () => void;
    fakeClient.establishSessionWithPeer.mockReturnValueOnce(new Promise<void>(resolve => { establish = resolve; }));
    await t.connect();
    const sending = t.send("peer", { message: "private" });
    expect(fakeClient.establishSessionWithPeer).toHaveBeenCalledWith("peer");
    await Promise.resolve();
    expect(fakeClient.send).not.toHaveBeenCalled();
    establish();
    await sending;
    expect(fakeClient.send).toHaveBeenCalledWith("peer", { message: "private" });
    await t.disconnect();
  });

  it("does not send or enable plaintext when session establishment fails", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    const failure = new Error("Missing peer prekeys");
    fakeClient.establishSessionWithPeer.mockRejectedValueOnce(failure);
    await t.connect();
    await expect(t.send("peer", { message: "private" })).rejects.toBe(failure);
    expect(fakeClient.send).not.toHaveBeenCalled();
    expect(fakeClient.addPlaintextPeer).not.toHaveBeenCalled();
    expect(t.getPlaintextPeers()).toEqual([]);
    await t.disconnect();
  });

  it("retains explicitly configured plaintext compatibility sends", async () => {
    const t = new AgtTransport({
      relayUrl: "ws://r", registryUrl: "http://reg", identity, plaintextPeers: ["legacy"],
    });
    await t.connect();
    await t.send("legacy", { message: "compatibility" });
    expect(fakeClient.establishSessionWithPeer).not.toHaveBeenCalled();
    expect(fakeClient.send).toHaveBeenCalledWith("legacy", { message: "compatibility" });
    await t.disconnect();
  });

  it.each([
    [false, "encrypted"], [true, "plaintext"], [undefined, "unknown"],
    [null, "unknown"], [0, "unknown"], ["false", "unknown"],
  ] as const)("propagates SDK evidence %s as %s, never payload claims", async (flag, security) => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    const handler = vi.fn();
    const payload = { security: "encrypted", plaintext: false };
    t.onMessage(handler);
    await t.connect();
    fakeClient.__msgHandler?.("peer", payload, flag as boolean | undefined);
    expect(handler).toHaveBeenCalledWith("peer", payload, security);
    expect(t.getInbox()[0]).toMatchObject({ from: "peer", content: payload, security });
    await expect(t.waitForMessage((_content, _from, evidence) => evidence)).resolves.toBe(security);
    const active = t.waitForMessage((_content, _from, evidence) => evidence);
    fakeClient.__msgHandler?.("peer", payload, flag as boolean | undefined);
    await expect(active).resolves.toBe(security);
    expect(t.getInbox()).toEqual([]);
    await t.disconnect();
  });

  it("does not treat later plaintext as encrypted after an encrypted message", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    fakeClient.__msgHandler?.("peer", { first: true }, false);
    fakeClient.__msgHandler?.("peer", { second: true }, true);
    fakeClient.__msgHandler?.("peer", { third: true });
    expect(t.drainInbox().map(message => message.security)).toEqual([
      "encrypted", "plaintext", "unknown",
    ]);
    await t.disconnect();
  });

  it("passes security evidence to ACK predicates without upgrading plaintext", async () => {
    const t = new AgtTransport({ relayUrl: "ws://r", registryUrl: "http://reg", identity });
    await t.connect();
    fakeClient.send.mockImplementationOnce(async () => {
      fakeClient.__msgHandler?.("peer", { id: "run-1", ack: true }, true);
      fakeClient.__msgHandler?.("peer", { id: "run-1", ack: true });
      fakeClient.__msgHandler?.("peer", { id: "run-1", ack: true }, false);
    });
    await expect(t.sendWithAck("peer", { id: "run-1" }, (content, from, security) =>
      from === "peer" && security === "encrypted" ? content : null, { retries: 0 },
    )).resolves.toEqual({ id: "run-1", ack: true });
    expect(t.getInbox().map(message => message.security)).toEqual(["plaintext", "unknown"]);
    await t.disconnect();
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

  describe("submitReputation Ed25519-Timestamp auth", () => {
    let realIdentity: IMeshIdentity;

    beforeEach(async () => {
      // Real Ed25519 keys — the AGT 4.0 registry verifies the
      // signature server-side, so a zero-filled key would let the test
      // pass against our mock but fail in production. Using @noble's
      // utility to generate a fresh keypair per test keeps the wire
      // shape honest.
      const { ed25519: e } = await import("@noble/curves/ed25519.js");
      const priv = e.utils.randomSecretKey();
      const pub = e.getPublicKey(priv);
      realIdentity = {
        agentId: "did:mesh:abcdef0123456789abcdef0123456789",
        signingPrivateKey: priv,
        signingPublicKey: pub,
      };
    });

    it("attaches Ed25519-Timestamp authorization header on success", async () => {
      const fetchMock = vi.fn<typeof fetch>(
        async () =>
          new Response(
            JSON.stringify({
              did: "did:mesh:peer",
              reputation_score: 0.8,
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
      );
      vi.stubGlobal("fetch", fetchMock);

      const t = new AgtTransport({
        relayUrl: "ws://r",
        registryUrl: "http://reg",
        identity: realIdentity,
      });
      const ok = await t.submitReputation("did:mesh:peer", "sess-1", 0.8, ["reliable"]);

      expect(ok).toBe(true);
      expect(fetchMock).toHaveBeenCalledTimes(1);
      const [url, init] = fetchMock.mock.calls[0];
      expect(url).toBe("http://reg/v1/agents/did%3Amesh%3Apeer/reputation");
      expect(init?.method).toBe("POST");

      const auth = new Headers(init?.headers).get("authorization")!;
      expect(auth).not.toBeNull();
      // Wire format: Ed25519-Timestamp <did> <iso8601> <base64url(sig)>
      const parts = auth.split(" ");
      expect(parts).toHaveLength(4);
      expect(parts[0]).toBe("Ed25519-Timestamp");
      expect(parts[1]).toBe(realIdentity.agentId);
      expect(parts[2]).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/);
      expect(parts[3]).toMatch(/^[A-Za-z0-9_-]+$/); // base64url
    });

    it("returns false (not throw) when registry rejects with 401", async () => {
      const fetchMock = vi.fn(
        async () =>
          new Response("unauthorized", {
            status: 401,
            statusText: "Unauthorized",
          }),
      );
      vi.stubGlobal("fetch", fetchMock);

      const t = new AgtTransport({
        relayUrl: "ws://r",
        registryUrl: "http://reg",
        identity: realIdentity,
      });
      const ok = await t.submitReputation("did:mesh:peer", "sess-1", 0.8);
      expect(ok).toBe(false);
    });
  });
});

