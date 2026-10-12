// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { EventEmitter } from "node:events";
import WebSocket from "ws";
import { afterEach, describe, expect, it, vi } from "vitest";
import { dispatcherAuthRequest, dispatcherSocketFactory, verifyDispatcherIdentity, type DispatcherEntraBinding } from "./mission-entra.js";
import { RequiredMeshAuth, type meshAuthRequest } from "./required-mesh-auth.js";

vi.mock("ws", () => ({ default: vi.fn() }));

const binding: DispatcherEntraBinding = {
  version: "v1", state: "ready", namespaceUid: "namespace-uid", deploymentUid: "deployment-uid", rootUid: "root-uid", journalUid: "entra-uid",
  tenantId: "a1111111-1111-4111-8111-111111111111", blueprintId: "b2222222-2222-4222-8222-222222222222",
  appId: "c3333333-3333-4333-8333-333333333333", objectId: "d4444444-4444-4444-8444-444444444444",
};
const did = `did:mesh:${"a".repeat(32)}`;
const verified = { did, tier: "verified", verified_app_id: binding.appId, verified_tenant_id: binding.tenantId };
const sdkUrl = `http://entra-auth-sidecar.kars-system.svc:5000/AuthorizationHeaderUnauthenticated/AgentMesh?AgentIdentity=${binding.appId}`;
const registryUrl = "https://registry.example/agt";

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllEnvs(); });

describe("dispatcher official SDK token adapter", () => {
  it("requests the pinned app from the namespace SDK and returns its conservative lease", async () => {
    vi.stubEnv("AGT_OAUTH_TOKEN", "stale-environment-token");
    const request = vi.fn<typeof meshAuthRequest>().mockResolvedValue({ authorizationHeader: "Bearer fresh-sdk-token" });
    const signal = new AbortController().signal;
    const adapted = dispatcherAuthRequest("kars-system", registryUrl, binding, request);
    await expect(adapted("GET", "/v1/mesh-token", undefined, signal)).resolves.toEqual({
      access_token: "fresh-sdk-token", token_type: "Bearer", expires_in: 3000,
    });
    expect(request).toHaveBeenCalledExactlyOnceWith("GET", sdkUrl, undefined, signal);
  });
  it("forwards verification body and cancellation unchanged to the configured registry", async () => {
    const request = vi.fn<typeof meshAuthRequest>().mockResolvedValue(verified);
    const signal = new AbortController().signal;
    const body = { amid: did, verification_token: "fresh-sdk-token" };
    await expect(dispatcherAuthRequest("kars-system", registryUrl, binding, request)("POST", "/agt/registry/v1/registry/verify", body, signal))
      .resolves.toBe(verified);
    expect(request).toHaveBeenCalledExactlyOnceWith("POST", `${registryUrl}/v1/registry/verify`, body, signal);
    expect(request.mock.calls[0][2]).toBe(body);
  });
  it.each([
    undefined, null, {}, { authorizationHeader: 7 }, { authorizationHeader: "" },
    { authorizationHeader: "Basic private-token" }, { authorizationHeader: "Bearer " },
    { authorizationHeader: "Bearer token with spaces" }, { authorizationHeader: "Bearer token\ntrailer" },
    { authorizationHeader: "Bearer private-token", success: false }, { authorizationHeader: "Bearer private-token", error: "private error" },
  ])("rejects malformed or negative SDK success bodies %# without reflecting secrets", async result => {
    const request = vi.fn<typeof meshAuthRequest>().mockResolvedValue(result);
    await expect(dispatcherAuthRequest("kars-system", registryUrl, binding, request)("GET", "/v1/mesh-token"))
      .rejects.toThrow(/^Invalid dispatcher SDK token response$/);
  });
  it.each([
    ["POST", "/v1/mesh-token"], ["GET", "/agt/registry/v1/registry/verify"],
    ["GET", "https://unexpected.example/token"], ["POST", "/v1/registry/verify"],
  ])("rejects unsupported auth operation %s %s without a request", async (method, path) => {
    const request = vi.fn<typeof meshAuthRequest>();
    await expect(dispatcherAuthRequest("kars-system", registryUrl, binding, request)(method, path))
      .rejects.toThrow(/^Invalid dispatcher auth operation$/);
    expect(request).not.toHaveBeenCalled();
  });
  it.each(["GET", "POST"])("does not retry or downgrade an unavailable %s auth endpoint", async method => {
    const failure = new Error("Mesh auth request unavailable");
    const request = vi.fn<typeof meshAuthRequest>().mockRejectedValue(failure);
    const path = method === "GET" ? "/v1/mesh-token" : "/agt/registry/v1/registry/verify";
    await expect(dispatcherAuthRequest("kars-system", registryUrl, binding, request)(method, path)).rejects.toBe(failure);
    expect(request).toHaveBeenCalledOnce();
  });
});

describe("dispatcher registry identity pinning", () => {
  it.each([undefined, "verified", "success", "ok"])("accepts the registry-verified DID, app and tenant with status %s", status => {
    expect(() => verifyDispatcherIdentity({ ...verified, status }, did, did, binding)).not.toThrow();
  });
  it.each([
    null, {}, { ...verified, did: `did:mesh:${"b".repeat(32)}` }, { ...verified, tier: "anonymous" },
    { ...verified, success: false }, { ...verified, verified: false }, { ...verified, status: "pending" },
    { ...verified, error: "private-error" }, { ...verified, verified_app_id: binding.objectId },
    { ...verified, verified_tenant_id: binding.blueprintId }, { ...verified, verified_app_id: "" },
    { ...verified, verified_tenant_id: null },
  ])("rejects negative verification and app/tenant substitution %#", result => {
    expect(() => verifyDispatcherIdentity(result, did, did, binding)).toThrow(/^Registry did not verify the (expected mesh|pinned dispatcher) identity$/);
  });
  it("does not accept another legitimately verified DID instead of the derived dispatcher", () => {
    const otherDid = `did:mesh:${"b".repeat(32)}`;
    expect(() => verifyDispatcherIdentity({ ...verified, did: otherDid }, otherDid, did, binding))
      .toThrow(/^Registry did not verify the pinned dispatcher identity$/);
  });
});

async function socketHarness() {
  const sends: ReturnType<typeof vi.fn>[] = [];
  const sockets: Array<EventEmitter & { send: (data: unknown, ...rest: unknown[]) => unknown }> = [];
  vi.mocked(WebSocket).mockImplementation(function () {
    const send = vi.fn();
    const socket = Object.assign(new EventEmitter(), { send });
    sends.push(send); sockets.push(socket);
    return socket as unknown as WebSocket;
  });
  const disconnects: Array<() => void> = [];
  let factory!: (url: string) => unknown;
  const raw = {
    currentDid: did, isConnected: false,
    connect: vi.fn(async () => { factory("wss://relay.example/ws"); raw.isConnected = true; }),
    disconnect: vi.fn(async () => { raw.isConnected = false; }),
    onDisconnect: (handler: () => void) => { disconnects.push(handler); },
  };
  let minted = 0;
  const request = vi.fn<typeof meshAuthRequest>(async method => method === "GET"
    ? { access_token: `sdk-token-${++minted}`, token_type: "Bearer", expires_in: 3000 } : verified);
  const auth = new RequiredMeshAuth(raw, { request, publish: vi.fn(), revoke: vi.fn(), fatal: vi.fn() });
  factory = await dispatcherSocketFactory(() => auth);
  return { auth, factory, raw, request, sends, sockets, drop: () => { raw.isConnected = false; disconnects.forEach(fn => fn()); } };
}

describe("dispatcher socket token adapter", () => {
  it("injects the current shared lease for every connect frame, preserving SDK options and callbacks", async () => {
    const h = await socketHarness();
    await h.auth.connect();
    try {
      expect(WebSocket).toHaveBeenCalledExactlyOnceWith("wss://relay.example/ws");
      const callback = vi.fn(); const options = { compress: false };
      const frame = { type: "connect", amid: did, token: "stale-sdk-token", capabilities: ["kars:mission-dispatch"] };
      h.sockets[0].send(JSON.stringify(frame), options, callback);
      h.sockets[0].send(JSON.stringify({ type: "connect", amid: did }));
      expect(JSON.parse(h.sends[0].mock.calls[0][0])).toEqual({ ...frame, token: "sdk-token-1" });
      expect(h.sends[0].mock.calls[0].slice(1)).toEqual([options, callback]);
      expect(JSON.parse(h.sends[0].mock.calls[1][0]).token).toBe("sdk-token-1");
      h.drop(); await h.auth.connect();
      h.sockets[1].send(JSON.stringify(frame));
      expect(JSON.parse(h.sends[1].mock.calls[0][0]).token).toBe("sdk-token-2");
      expect(h.raw.connect).toHaveBeenCalledTimes(2);
    } finally { await h.auth.close(); }
  });
  it("preserves non-connect, non-JSON and binary traffic exactly", async () => {
    const h = await socketHarness(); await h.auth.connect();
    try {
      const frames = [JSON.stringify({ type: "message", token: "payload-value" }), "not-json", "null", Buffer.from([0, 1, 2])];
      const callback = vi.fn();
      for (const frame of frames) h.sockets[0].send(frame, callback);
      expect(h.sends[0].mock.calls).toEqual(frames.map(frame => [frame, callback]));
      expect(h.sends[0].mock.calls[3][0]).toBe(frames[3]);
    } finally { await h.auth.close(); }
  });
  it("cannot send a connect frame before authentication or after retirement", async () => {
    const h = await socketHarness();
    h.factory("wss://relay.example/ws");
    expect(() => h.sockets[0].send('{"type":"connect","token":"stale"}')).toThrow("connect token unavailable");
    expect(h.sends[0]).not.toHaveBeenCalled();
    await h.auth.connect(); await h.auth.close();
    expect(() => h.sockets[1].send('{"type":"connect"}')).toThrow("connect token unavailable");
    expect(h.sends[1]).not.toHaveBeenCalled();
  });
  it("binds socket message custody to the shared lease and retires old socket events", async () => {
    const h = await socketHarness(); await h.auth.connect();
    try {
      const delivered = vi.fn(); const closed = vi.fn();
      h.sockets[0].on("message", delivered); h.sockets[0].on("close", closed);
      expect(h.sockets[0].emit("message", "current")).toBe(true);
      h.drop(); await h.auth.connect();
      expect(h.sockets[0].emit("message", "late")).toBe(false);
      expect(delivered).toHaveBeenCalledExactlyOnceWith("current");
      expect(h.sockets[0].emit("close")).toBe(true); expect(closed).toHaveBeenCalledOnce();
      h.sockets[1].on("message", delivered);
      expect(h.sockets[1].emit("message", "fresh")).toBe(true);
      expect(delivered).toHaveBeenLastCalledWith("fresh");
    } finally { await h.auth.close(); }
  });
});
