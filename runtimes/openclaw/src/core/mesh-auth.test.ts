// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type RequestListener, type Server } from "node:http";
import { EventEmitter } from "node:events";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RequiredMeshAuth, meshAuthRequest, requiredMeshAuth, validateMeshVerification } from "./mesh-auth.js";

const did = `did:mesh:${"a".repeat(32)}`;
const verified = { did, tier: "verified", verified_app_id: "app", verified_tenant_id: "tenant", verified_at: "2026-09-25T00:00:00Z" };
const token = (value = "fresh-token", expires = 3000) => ({ access_token: value, token_type: "Bearer", expires_in: expires });
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}
function fixture() {
  const disconnects: Array<() => void> = [];
  const messages: Array<(...args: any[]) => any> = [];
  const knocks: Array<(...args: any[]) => any> = [];
  const raw = {
    isConnected: false, currentDid: did,
    connect: vi.fn(async () => { raw.isConnected = true; }),
    disconnect: vi.fn(() => { raw.isConnected = false; return Promise.resolve(); }),
    onDisconnect: (handler: () => void) => { disconnects.push(handler); },
    onMessage: (handler: (...args: any[]) => any) => { messages.push(handler); },
    onKnock: (handler: (...args: any[]) => any) => { knocks.push(handler); },
    send: vi.fn(async () => "sent"),
  };
  let minted = 0;
  const request = vi.fn<typeof meshAuthRequest>(async (method) => method === "GET" ? token(`token-${++minted}`) : verified);
  const hooks = { publish: vi.fn(), revoke: vi.fn(), fatal: vi.fn(), request };
  const auth = new RequiredMeshAuth(raw, hooks);
  return { auth, raw, hooks, request, messages, knocks, drop: () => { raw.isConnected = false; disconnects.forEach((h) => h()); } };
}

afterEach(() => { vi.useRealTimers(); vi.unstubAllEnvs(); });

describe("required Entra configuration and registry contract", () => {
  it.each(["AGT_SKIP_INIT", "AGT_SKIP_ENTRA", "AGT_SKIP_VERIFY"])("rejects required %s", (flag) => {
    expect(() => requiredMeshAuth({ MESH_AUTH_BACKEND: "EntraAgentIdentity", [flag]: "1" })).toThrow(/conflicts/);
  });
  it("preserves explicit development and optional OAuth", () => {
    expect(requiredMeshAuth({})).toBe(false);
    expect(requiredMeshAuth({ AGT_OAUTH_TOKEN: "legacy", AGT_SKIP_VERIFY: "1" })).toBe(false);
    expect(requiredMeshAuth({ AGT_SKIP_INIT: "1", AGT_SKIP_ENTRA: "1" })).toBe(false);
    expect(requiredMeshAuth({ MESH_AUTH_BACKEND: "EntraAgentIdentity" })).toBe(true);
  });
  it.each([
    {}, { ...verified, did: "another-agent" }, { ...verified, tier: "anonymous" },
    { ...verified, success: false }, { ...verified, verified: false }, { ...verified, error: "denied" },
    { ...verified, status: "failed" }, { ...verified, verified_app_id: "" }, { ...verified, verified_tenant_id: null },
  ])("rejects a negative or mismatched HTTP-success body %#", (body) => {
    expect(() => validateMeshVerification(body, did)).toThrow(/expected mesh identity/);
  });
  it("accepts the pinned official registry response", () => {
    expect(() => validateMeshVerification(verified, did)).not.toThrow();
  });
});

describe("one required Entra transport lease", () => {
  it("withholds publication, KNOCK, mission callbacks and sends until verification", async () => {
    const f = fixture();
    const gate = deferred<any>();
    f.request.mockImplementation(async (method) => method === "GET" ? token() : gate.promise);
    const mission = vi.fn();
    f.auth.client.onMessage(mission);
    f.auth.client.onKnock(() => ({ accept: true }));
    const connecting = f.auth.connect();
    await vi.waitFor(() => expect(f.request).toHaveBeenCalledTimes(2));
    expect(f.auth.client.isConnected).toBe(false);
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.knocks[0]()).toMatchObject({ accept: false });
    f.messages[0]("assignment");
    expect(mission).not.toHaveBeenCalled();
    expect(() => f.auth.client.send()).toThrow(/unavailable/);
    gate.resolve(verified);
    await connecting;
    expect(f.auth.client.isConnected).toBe(true);
    expect(f.hooks.publish).toHaveBeenCalledOnce();
    expect(await f.knocks[0]()).toEqual({ accept: true });
    f.messages[0]("assignment");
    expect(mission).toHaveBeenCalledOnce();
    expect(f.request.mock.calls[1][2]).toEqual({ amid: did, verification_token: "fresh-token" });
    await f.auth.close();
  });

  it.each([undefined, {}, { error: "unavailable" }, { ...token(), access_token: "" },
    { ...token(), token_type: "Basic" }, { ...token(), token_type: 7 },
    token("short", 60), token("long", 86401), { ...token(), success: false },
  ])("fails closed on missing/malformed mint %#", async (minted) => {
    const f = fixture();
    f.request.mockResolvedValue(minted);
    await expect(f.auth.connect()).rejects.toThrow(/authentication failed/);
    expect(f.raw.connect).not.toHaveBeenCalled();
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.raw.disconnect).toHaveBeenCalledOnce();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
    expect(f.auth.client.isConnected).toBe(false);
  });

  it.each(["mint", "connect", "verify"])("retires and stops on %s failure without token leakage", async (stage) => {
    const f = fixture();
    const sensitive = new Error("secret-token response body");
    if (stage === "mint") f.request.mockRejectedValue(sensitive);
    if (stage === "connect") f.raw.connect.mockRejectedValue(sensitive);
    if (stage === "verify") f.request.mockResolvedValueOnce(token()).mockRejectedValue(sensitive);
    await expect(f.auth.connect()).rejects.toThrow("Required Entra mesh authentication failed; process restart required");
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
    await expect(f.auth.connect()).rejects.toThrow(/restart required/);
  });

  it("rejects false verification and noncanonical client identity", async () => {
    for (const invalidDid of [false, true]) {
      const f = fixture();
      if (invalidDid) f.raw.currentDid = "legacy-amid";
      else f.request.mockResolvedValueOnce(token()).mockResolvedValue({ ...verified, success: false });
      await expect(f.auth.connect()).rejects.toThrow(/authentication failed/);
      expect(f.hooks.publish).not.toHaveBeenCalled();
    }
  });

  it("rejects a verified response when the connected DID changed during verification", async () => {
    const f = fixture();
    const verification = deferred<any>();
    f.request.mockImplementation(async method => method === "GET" ? token() : verification.promise);
    const connecting = f.auth.connect();
    await vi.waitFor(() => expect(f.request).toHaveBeenCalledTimes(2));
    f.raw.currentDid = `did:mesh:${"b".repeat(32)}`;
    verification.resolve(verified);
    await expect(connecting).rejects.toThrow(/authentication failed/);
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.auth.usable).toBe(false);
    expect(f.raw.disconnect).toHaveBeenCalledOnce();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
  });

  it("does not admit pre-verification socket messages that finish decrypting after publication", async () => {
    const f = fixture();
    const verification = deferred<any>();
    const decrypt = deferred<void>();
    f.request.mockImplementation(async method => method === "GET" ? token() : verification.promise);
    const mission = vi.fn();
    f.auth.client.onMessage(mission);
    const connecting = f.auth.connect();
    await vi.waitFor(() => expect(f.request).toHaveBeenCalledTimes(2));
    const socket = f.auth.bindSocketMessages(new EventEmitter());
    let delivered!: Promise<void>;
    socket.on("message", () => { delivered = decrypt.promise.then(() => f.messages[0]("assignment")); });
    socket.emit("message");
    verification.resolve(verified);
    await connecting;
    decrypt.resolve(); await delivered;
    expect(f.auth.usable).toBe(true);
    expect(mission).not.toHaveBeenCalled();
    socket.emit("message"); await delivered;
    expect(mission).toHaveBeenCalledOnce();
    await f.auth.close();
  });

  it("prevents an old callback from disconnecting or reconnecting a newly admitted lease", async () => {
    const f = fixture();
    const gate = deferred<void>();
    let callback!: Promise<void>;
    f.auth.client.onMessage(() => {
      callback = gate.promise.then(async () => {
        await expect(f.auth.client.disconnect()).rejects.toThrow(/Stale/);
        await expect(f.auth.client.connect()).rejects.toThrow(/Stale/);
      });
    });
    await f.auth.connect();
    f.messages[0]();
    f.drop(); await f.auth.connect();
    gate.resolve(); await callback;
    expect(f.auth.usable).toBe(true);
    expect(f.raw.disconnect).toHaveBeenCalledOnce();
    await f.auth.close();
  });

  it("coalesces concurrent startup into one connection and mint", async () => {
    const f = fixture();
    await Promise.all([f.auth.connect(), f.auth.connect(), f.auth.client.connect()]);
    expect(f.raw.connect).toHaveBeenCalledOnce();
    expect(f.request).toHaveBeenCalledTimes(2);
    await f.auth.close();
  });

  it("renews before expiry using a newly minted token for both relay and registry", async () => {
    vi.useFakeTimers();
    vi.stubEnv("AGT_OAUTH_TOKEN", "stale-environment-token");
    const f = fixture();
    const frames: string[] = [];
    f.raw.connect.mockImplementation(async () => { frames.push(f.auth.connectToken()); f.raw.isConnected = true; });
    await f.auth.connect();
    await vi.advanceTimersByTimeAsync(2_700_000);
    expect(f.hooks.revoke).toHaveBeenCalledOnce();
    expect(f.hooks.publish).toHaveBeenCalledTimes(2);
    expect(f.raw.disconnect).toHaveBeenCalledOnce();
    expect(frames).toEqual(["token-1", "token-2"]);
    expect(f.request.mock.calls.filter(([method]) => method === "POST").map((call) => (call[2] as any).verification_token)).toEqual(frames);
    await f.auth.close();
  });

  it("revokes synchronously on disconnect and waits for retirement before fresh reconnect", async () => {
    const f = fixture();
    await f.auth.connect();
    const retired = deferred<void>();
    f.raw.disconnect.mockImplementation(() => { f.raw.isConnected = false; return retired.promise; });
    f.drop();
    const retry = f.auth.client.connect();
    expect(f.hooks.revoke).toHaveBeenCalledOnce();
    expect(f.raw.disconnect).toHaveBeenCalledOnce();
    expect(f.auth.client.isConnected).toBe(false);
    expect(f.request).toHaveBeenCalledTimes(2);
    retired.resolve();
    await retry;
    expect(f.request).toHaveBeenCalledTimes(4);
    expect(f.hooks.publish).toHaveBeenCalledTimes(2);
    await f.auth.close();
  });

  it("handles a disconnect immediately inside initial publication without reusing its completed auth promise", async () => {
    const f = fixture();
    f.hooks.publish.mockImplementationOnce(() => { f.drop(); });
    await f.auth.connect();
    await vi.waitFor(() => expect(f.hooks.publish).toHaveBeenCalledTimes(2));
    expect(f.auth.client.isConnected).toBe(true);
    expect(f.request).toHaveBeenCalledTimes(4);
    await f.auth.close();
  });

  it.each(["mint", "verify"])("revokes and stops on renewal %s outage", async (stage) => {
    vi.useFakeTimers();
    const f = fixture();
    await f.auth.connect();
    if (stage === "mint") f.request.mockRejectedValue(new Error("offline"));
    else f.request.mockResolvedValueOnce(token("new-token")).mockResolvedValue({ ...verified, tier: "anonymous" });
    await vi.advanceTimersByTimeAsync(2_700_000);
    expect(f.auth.client.isConnected).toBe(false);
    expect(f.hooks.publish).toHaveBeenCalledOnce();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
    expect(() => f.auth.client.send()).toThrow(/unavailable/);
  });

  it("blocks stale mission continuations and delayed KNOCK approval after successful reconnect", async () => {
    const f = fixture();
    const gate = deferred<void>();
    f.auth.client.onMessage(async () => { await gate.promise; f.auth.assertUsable(); await f.auth.client.send(); });
    f.auth.client.onKnock(async () => { await gate.promise; return true; });
    await f.auth.connect();
    const task = expect(f.messages[0]()).rejects.toThrow(/unavailable/);
    const knock = f.knocks[0]();
    f.drop();
    await f.auth.connect();
    gate.resolve();
    await task;
    expect(await knock).toMatchObject({ accept: false });
    expect(f.raw.send).not.toHaveBeenCalled();
    await f.auth.close();
  });

  it("rejects old socket callbacks even when SDK decryption finishes after a new verified connection", async () => {
    const f = fixture();
    const sockets: EventEmitter[] = [];
    const decrypted = deferred<void>();
    const delivered = deferred<void>();
    const mission = vi.fn();
    f.auth.client.onMessage(mission);
    f.raw.connect.mockImplementation(async () => {
      const socket = f.auth.bindSocketMessages(new EventEmitter());
      sockets.push(socket);
      socket.on("message", async () => {
        await decrypted.promise;
        f.messages[0]("old-assignment");
        delivered.resolve();
      });
      f.raw.isConnected = true;
    });
    await f.auth.connect();
    sockets[0].emit("message", "ciphertext");
    f.drop();
    await f.auth.connect();
    decrypted.resolve();
    await delivered.promise;
    expect(mission).not.toHaveBeenCalled();
    expect(sockets[0].emit("message", "late-ciphertext")).toBe(false);
    const close = vi.fn();
    sockets[0].on("close", close);
    sockets[0].emit("close");
    expect(close).toHaveBeenCalledOnce();
    await f.auth.close();
  });

  it("clears pre-admission inbox messages before publication", async () => {
    const f = fixture();
    const drain = vi.fn();
    Object.assign(f.raw, { drainInbox: drain });
    f.hooks.publish.mockImplementation(() => expect(drain).toHaveBeenCalledOnce());
    await f.auth.connect();
    await f.auth.close();
  });

  it("gates composite helpers' delayed sends and pending inbox operations", async () => {
    const f = fixture();
    const gate = deferred<void>();
    Object.assign(f.raw, {
      async sendWithAck(this: { send: () => Promise<void> }) { await gate.promise; return this.send(); },
      waitForInbox: () => gate.promise.then(() => true),
    });
    await f.auth.connect();
    const send = expect((f.auth.client as any).sendWithAck()).rejects.toThrow(/unavailable/);
    const inbox = expect((f.auth.client as any).waitForInbox()).rejects.toThrow(/unavailable/);
    const verifying = deferred<any>();
    f.request.mockResolvedValueOnce(token("replacement")).mockImplementation(() => verifying.promise);
    f.drop();
    const reconnect = f.auth.connect();
    await vi.waitFor(() => expect(f.raw.connect).toHaveBeenCalledTimes(2));
    gate.resolve();
    await Promise.all([send, inbox]);
    expect(f.raw.send).not.toHaveBeenCalled();
    verifying.resolve(verified);
    await reconnect;
    await f.auth.close();
  });

  it("bounds a hung relay handshake without publishing a client", async () => {
    vi.useFakeTimers();
    const f = fixture();
    f.raw.connect.mockReturnValue(new Promise(() => {}));
    const connecting = expect(f.auth.connect()).rejects.toThrow(/authentication failed/);
    await vi.advanceTimersByTimeAsync(35_000);
    await connecting;
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
  });

  it("quarantines failed retirement and never reconnects or republishes", async () => {
    const f = fixture();
    await f.auth.connect();
    f.raw.disconnect.mockRejectedValue(new Error("socket still owns prekeys"));
    f.drop();
    await expect(f.auth.connect()).rejects.toThrow(/restart required/);
    expect(f.auth.state).toBe("quarantined");
    expect(f.raw.connect).toHaveBeenCalledOnce();
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
    await expect(f.auth.connect()).rejects.toThrow(/restart required/);
  });

  it("bounds cleanup and keeps quarantine when the socket never closes", async () => {
    vi.useFakeTimers();
    const f = fixture();
    await f.auth.connect();
    f.raw.disconnect.mockReturnValue(new Promise(() => {}));
    const closing = f.auth.close();
    expect(f.auth.client.isConnected).toBe(false);
    await vi.advanceTimersByTimeAsync(10_000);
    await closing;
    expect(f.auth.state).toBe("quarantined");
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
  });

  it("never publishes a late verification after shutdown", async () => {
    const f = fixture();
    const gate = deferred<any>();
    f.request.mockResolvedValueOnce(token()).mockImplementation(() => gate.promise);
    const connecting = expect(f.auth.connect()).rejects.toThrow(/authentication failed/);
    await vi.waitFor(() => expect(f.request).toHaveBeenCalledTimes(2));
    await f.auth.close();
    gate.resolve(verified);
    await connecting;
    expect(f.hooks.publish).not.toHaveBeenCalled();
    expect(f.auth.client.isConnected).toBe(false);
  });

  it("uses JWT expiry only to shorten the router lease and gates readiness at the exact deadline", async () => {
    vi.useFakeTimers();
    const f = fixture();
    const start = Date.now();
    const jwt = `header.${Buffer.from(JSON.stringify({ exp: (start + 120_000) / 1000 })).toString("base64url")}.signature`;
    f.request.mockResolvedValueOnce(token(jwt)).mockResolvedValue(verified);
    await f.auth.connect();
    vi.setSystemTime(start + 120_000);
    expect(f.auth.client.isConnected).toBe(false);
    expect(() => f.auth.connectToken()).toThrow(/unavailable/);
    expect(() => f.auth.client.send()).toThrow(/unavailable/);
    await f.auth.close();
  });

  it("expiry during outstanding verification prevents later publication", async () => {
    vi.useFakeTimers();
    const f = fixture();
    const gate = deferred<any>();
    f.request.mockResolvedValueOnce(token("token", 61)).mockImplementation(() => gate.promise);
    const connecting = expect(f.auth.connect()).rejects.toThrow(/authentication failed/);
    await vi.advanceTimersByTimeAsync(61_000);
    expect(f.hooks.fatal).toHaveBeenCalledOnce();
    gate.resolve(verified);
    await connecting;
    expect(f.hooks.publish).not.toHaveBeenCalled();
  });
});

describe("bounded router authentication HTTP", () => {
  let server: Server | undefined;
  afterEach(async () => {
    server?.closeAllConnections();
    if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
    server = undefined;
  });
  async function listen(handler: RequestListener) {
    server = createServer(handler);
    await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
    const address = server.address() as { port: number };
    vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
  }
  it.each([401, 403, 500])("rejects HTTP %s even with verified body and redacts credentials", async (status) => {
    await listen((_req, res) => { res.writeHead(status); res.end(JSON.stringify({ ...verified, secret: "do-not-log" })); });
    await expect(meshAuthRequest("POST", "/verify", { verification_token: "do-not-log" })).rejects.toThrow("Mesh auth request rejected");
  });
  it.each(["not-json", "x".repeat(65537)])("bounds malformed/oversized responses %#", async (body) => {
    await listen((_req, res) => { res.end(body); });
    await expect(meshAuthRequest("GET", "/token")).rejects.toThrow(/Invalid mesh auth response|response too large/);
  });
  it("sends the expected POST and parses the pinned registry response", async () => {
    await listen((req, res) => {
      let body = "";
      req.on("data", (chunk) => { body += chunk; });
      req.on("end", () => {
        expect(req.method).toBe("POST");
        expect(JSON.parse(body)).toEqual({ amid: did, verification_token: "fresh" });
        res.end(JSON.stringify(verified));
      });
    });
    expect(await meshAuthRequest("POST", "/verify", { amid: did, verification_token: "fresh" })).toEqual(verified);
  });
  it("enforces an absolute deadline on a stalled response", async () => {
    const received = deferred<void>();
    await listen((_req, res) => { res.writeHead(200); res.write("{"); received.resolve(); });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const request = expect(meshAuthRequest("GET", "/stalled")).rejects.toThrow(/timed out/);
    await received.promise;
    await vi.advanceTimersByTimeAsync(8000);
    await request;
  });
  it("aborts a pending request when the lease is revoked", async () => {
    const received = deferred<void>();
    await listen(() => { received.resolve(); });
    const controller = new AbortController();
    const request = expect(meshAuthRequest("GET", "/pending", undefined, controller.signal)).rejects.toThrow(/unavailable/);
    await received.promise;
    controller.abort();
    await request;
  });
});
