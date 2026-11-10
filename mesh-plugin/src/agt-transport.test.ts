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

describe("scoped sends through the installed SDK registry and socket hooks", () => {
  const peer = `did:mesh:${"b".repeat(32)}`;
  let transport: AgtTransport;
  let frames: any[];
  let prekey: (init?: RequestInit) => Promise<Response>;
  let prekeyCalls: number;
  let response: () => Response;
  let onFrame: (frame: any) => void;
  let receive: (frame: unknown) => void;
  const operation = (assertCurrent = () => {}, signal = new AbortController().signal) => ({
    signal, assertCurrent, deadline: Date.now() + 5000,
  });

  beforeEach(async () => {
    const sdk = await import("@microsoft/agent-governance-sdk");
    const { ed25519 } = await import("@noble/curves/ed25519.js");
    __setAgtSdkForTesting(sdk as never);
    const seed = new Uint8Array(32).fill(7);
    const keys = new sdk.X3DHKeyManager(seed, ed25519.getPublicKey(seed));
    keys.generateSignedPreKey();
    const bundle = keys.getPublicBundle();
    const b64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64url");
    response = () => Response.json({ identity_key: b64(bundle.identityKey), identity_key_ed: b64(bundle.identityKeyEd),
      signed_pre_key: { public_key: b64(bundle.signedPreKey), signature: b64(bundle.signedPreKeySignature), key_id: bundle.signedPreKeyId } });
    frames = []; prekeyCalls = 0; onFrame = () => {};
    prekey = async () => response();
    vi.stubGlobal("fetch", vi.fn(async (input, init) => {
      if (String(input).endsWith("/prekeys") && init?.method === "GET") { prekeyCalls++; return prekey(init); }
      return Response.json({ did: `did:mesh:${"a".repeat(32)}` });
    }));
    const socket = { readyState: 1, onopen: null as any, onclose: null as any, onmessage: null as any, onerror: null as any,
      send: (data: unknown) => { const frame = JSON.parse(String(data)); frames.push(frame); onFrame(frame); },
      close: () => { socket.readyState = 3; socket.onclose?.({ code: 1000 }); } };
    transport = new AgtTransport({ relayUrl: "ws://relay.invalid", registryUrl: "http://registry.invalid",
      identity: { agentId: `did:mesh:${"a".repeat(32)}`, signingPrivateKey: seed, signingPublicKey: ed25519.getPublicKey(seed) },
      wsFactory: () => { queueMicrotask(() => socket.onopen?.({})); return socket; } });
    await transport.connect();
    receive = frame => socket.onmessage?.({ data: JSON.stringify(frame) });
    frames = [];
  });
  afterEach(async () => { await transport?.disconnect(); vi.unstubAllGlobals(); vi.useRealTimers(); });

  it("preserves socket-arrival custody through SDK KNOCK waits and excludes replay", async () => {
    await transport.send(peer, {}, operation());
    const client = (transport as any).client;
    // Only decryption bytes are controlled; actual SDK frame dispatch, pending-KNOCK await and callbacks run.
    client.getSession(peer).channel.receive = () => Buffer.from('{"content":"answer"}');
    client.knockAccepted.delete(peer);
    const pending = { resolve: vi.fn() }; client.knockPending.set(peer, pending);
    let owner = "old-assignment";
    const seen = vi.fn();
    transport.onMessage(seen, () => owner);
    const frame = { v: 1, type: "message", from: peer, id: "old", header: { dh: "AA==", pn: 0, n: 0 }, ciphertext: "AA==" };
    receive(frame);
    expect(seen).not.toHaveBeenCalled();
    owner = "new-assignment";
    pending.resolve(true);
    await vi.waitFor(() => expect(seen).toHaveBeenCalledOnce());
    expect(seen.mock.calls[0][3]).toBe("old-assignment");
    client.knockPending.delete(peer); client.knockAccepted.add(peer);
    receive({ type: "pending_messages", messages: [{ ...frame, id: "replayed" }] });
    await vi.waitFor(() => expect(seen).toHaveBeenCalledTimes(2));
    expect(seen.mock.calls[1][3]).toBeUndefined();
    receive({ ...frame, id: "new" });
    await vi.waitFor(() => expect(seen).toHaveBeenCalledTimes(3));
    expect(seen.mock.calls[2][3]).toBe("new-assignment");
  });

  it("reports possibly issued socket failures without retry and discards the advanced ratchet", async () => {
    onFrame = frame => { if (frame.type === "message") throw new Error("queued then failed"); };
    await expect(transport.send(peer, {}, operation())).rejects.toThrow("must not be replayed");
    expect(frames.map(frame => frame.type)).toEqual(["knock", "message"]);
    expect(prekeyCalls).toBe(1);
    expect((transport as any).client.getSession(peer)).toBeUndefined();
  });

  it("a cancelled same-peer invocation does not close a session used by another scoped invocation", async () => {
    let release!: (response: Response) => void;
    prekey = () => new Promise(resolve => { release = resolve; });
    const abort = new AbortController();
    const pending = transport.send(peer, {}, operation(() => {}, abort.signal));
    const failed = expect(pending).rejects.toThrow();
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    prekey = async () => response();
    await transport.send(peer, { winner: true }, operation());
    const session = (transport as any).client.getSession(peer);
    abort.abort(); release(response()); await failed;
    expect((transport as any).client.getSession(peer)).toBe(session);
    expect(session.messageCount).toBe(1);
    expect(frames.map(frame => frame.type)).toEqual(["knock", "message"]);
    await transport.send(peer, { next: true }, operation());
    expect(prekeyCalls).toBe(2);
  });

  it("does not close a session advanced by an independent send before failed-send cleanup", async () => {
    await transport.send(peer, {}, operation());
    const client = (transport as any).client;
    const session = client.getSession(peer);
    let writes = 0;
    onFrame = frame => { if (frame.type === "message" && ++writes === 1) throw new Error("first write failed"); };
    const failed = expect(transport.send(peer, { failed: true }, operation())).rejects.toThrow("must not be replayed");
    const successful = transport.send(peer, { independent: true }, operation());
    await Promise.all([failed, successful]);
    expect(client.getSession(peer) === session).toBe(true);
    expect(session.messageCount).toBe(2);
    await transport.send(peer, { next: true }, operation());
    expect(prekeyCalls).toBe(1);
  });

  it("executes actual prekey, KNOCK and encrypted MESSAGE once", async () => {
    await transport.send(peer, { content: "ordinary" }, operation());
    expect(frames.map(frame => frame.type)).toEqual(["knock", "message"]);
    expect(frames[1].ciphertext).toBeTypeOf("string");
    expect(prekeyCalls).toBe(1);
    await transport.send(peer, { content: "second" }, operation());
    expect(prekeyCalls).toBe(1);
  });

  it.each(["abort", "revoke", "deadline"])("vetoes late prekeys on %s without KNOCK, and leaves unrelated sends live", async reason => {
    let release!: (value: Response) => void;
    let requestSignal: AbortSignal | undefined;
    prekey = init => { requestSignal = init?.signal as AbortSignal; return new Promise(resolve => { release = resolve; }); };
    const abort = new AbortController();
    let current = true;
    const op = operation(() => { if (!current) throw new Error("revoked"); }, abort.signal);
    if (reason === "deadline") op.deadline = Date.now() + 20;
    const pending = transport.send(peer, {}, op);
    const checked = expect(pending).rejects.toThrow();
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    if (reason === "abort") abort.abort();
    if (reason === "revoke") current = false;
    if (reason === "deadline") await new Promise(resolve => setTimeout(resolve, 25));
    if (reason !== "revoke") expect(requestSignal?.aborted).toBe(true);
    const other = `did:mesh:${"c".repeat(32)}`;
    prekey = async () => response();
    await transport.send(other, { unrelated: true });
    release(response());
    await checked;
    expect(frames.filter(frame => frame.to === peer)).toEqual([]);
    expect(frames.filter(frame => frame.to === other).map(frame => frame.type)).toEqual(["knock", "message"]);
    expect((transport as any).client.getSession(peer)).toBeUndefined();
    expect(prekeyCalls).toBe(2);
  });

  it.each(["http", "oversize", "network"])("does not replay the OPK-consuming request after %s failure", async failure => {
    prekey = async () => {
      if (failure === "network") throw new Error("connection lost");
      return failure === "http" ? new Response("failed", { status: 503 }) : new Response("x".repeat(65537));
    };
    await expect(transport.send(peer, {}, operation())).rejects.toThrow();
    expect(prekeyCalls).toBe(1);
    expect(frames).toEqual([]);
  });

  it("checks before encryption and closes only the vetoed ratchet session", async () => {
    await transport.send(peer, {}, operation());
    const client = (transport as any).client;
    const channel = client.getSession(peer).channel;
    const send = channel.send.bind(channel);
    let current = true;
    channel.send = (...args: any[]) => { const value = send(...args); current = false; return value; };
    frames = [];
    await expect(transport.send(peer, {}, operation(() => { if (!current) throw new Error("revoked at socket"); }))).rejects.toThrow("revoked");
    expect(frames).toEqual([]);
    expect(client.getSession(peer)).toBeUndefined();
    current = false;
    await expect(transport.send(peer, {}, operation(() => { if (!current) throw new Error("stale before encryption"); }))).rejects.toThrow("stale");
    expect(prekeyCalls).toBe(1);
  });

  it("cleans an unused KNOCK session on revocation but cannot recall an issued frame", async () => {
    let current = true;
    onFrame = frame => { if (frame.type === "knock") current = false; };
    await expect(transport.send(peer, {}, operation(() => { if (!current) throw new Error("revoked"); }))).rejects.toThrow("revoked");
    expect(frames.map(frame => frame.type)).toEqual(["knock"]);
    expect((transport as any).client.getSession(peer)).toBeUndefined();
    current = true; frames = [];
    onFrame = frame => { if (frame.type === "message") current = false; };
    await expect(transport.send(peer, {}, operation(() => { if (!current) throw new Error("revoked"); }))).rejects.toThrow("must not be replayed");
    expect(frames.map(frame => frame.type)).toEqual(["knock", "message"]);
    expect((transport as any).client.getSession(peer).messageCount).toBe(1);
  });
});

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

