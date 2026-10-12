// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { MissionDispatchService, type MissionServiceConfig, type MissionServiceDependencies } from "./mission-service.js";
import { MissionDispatcher, type MissionCandidate } from "./mission-dispatcher.js";
import { KubernetesMissionStore } from "./mission-store.js";
import { missionIdentity } from "./mission-identity.js";
import type { DispatcherCustody } from "./mission-bootstrap.js";
import type { IMeshTransport } from "./transport-interface.js";
import type { KubernetesJson } from "./kubernetes-json.js";
import type { DispatcherEntraBinding } from "./mission-entra.js";
import type { RequiredMeshAuth } from "./required-mesh-auth.js";

const config: MissionServiceConfig = {
  location: { namespace: "kars-system", deployment: "kars-mission-dispatcher", release: "kars", podName: "dispatcher-abc", podUid: "pod-uid" },
  root: "a".repeat(64), relayUrl: "ws://relay:8765", registryUrl: "http://registry:8080", pollMs: 100, timeoutMs: 1_000,
};
const identity = missionIdentity(config.root, "dispatcher", "deployment-uid", "pod-uid");
const custody: DispatcherCustody = { deploymentUid: "deployment-uid", podUid: "pod-uid", namespaceUid: "namespace-uid", rootUid: "root-uid" };
const binding: DispatcherEntraBinding = {
  version: "v1", state: "ready", namespaceUid: custody.namespaceUid, deploymentUid: custody.deploymentUid,
  rootUid: custody.rootUid, journalUid: "entra-uid",
  tenantId: "a1111111-1111-4111-8111-111111111111", blueprintId: "b2222222-2222-4222-8222-222222222222",
  appId: "c3333333-3333-4333-8333-333333333333", objectId: "d4444444-4444-4444-8444-444444444444",
};
const entra = { tenantId: binding.tenantId, blueprintId: binding.blueprintId };
const verified = { did: identity.did, tier: "verified", verified_app_id: binding.appId, verified_tenant_id: binding.tenantId };
const sdkUrl = `http://entra-auth-sidecar.kars-system.svc:5000/AuthorizationHeaderUnauthenticated/AgentMesh?AgentIdentity=${binding.appId}`;
const candidate: MissionCandidate = { namespace: "kars-system", taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "runtime-pod-uid",
  runNonce: "run-1", agentDid: `did:mesh:${"b".repeat(32)}`, dispatcherDid: identity.did, agentName: "Briefing writer", content: "Write a useful briefing." };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const until = (assertion: () => void) => vi.waitFor(assertion, { interval: 5, timeout: 2_000 });
function harness(withEntra = false) {
  const order: string[] = [];
  const abort = new AbortController();
  let connected = false;
  let knock!: Parameters<IMeshTransport["onKnock"]>[0];
  const disconnected: Array<Parameters<IMeshTransport["onDisconnect"]>[0]> = [];
  let auth: (() => RequiredMeshAuth<IMeshTransport>) | undefined;
  let discoveryGate: (() => Promise<boolean>) | undefined;
  const tokens: string[] = [];
  const drainInbox = vi.fn();
  const wsFactory = vi.fn();
  const mesh = {
    currentDid: identity.did, get isConnected() { return connected; },
    connect: vi.fn(async () => { order.push("connect"); if (auth) tokens.push(auth().connectToken()); connected = true; }),
    disconnect: vi.fn(async () => { order.push("disconnect"); connected = false; disconnected.forEach(handler => handler("client")); }),
    drainInbox,
    getPlaintextPeers: vi.fn(() => [] as string[]),
    onKnock: vi.fn((handler: typeof knock) => { order.push("knock"); knock = handler; }),
    onError: vi.fn(), onDisconnect: vi.fn((handler: Parameters<IMeshTransport["onDisconnect"]>[0]) => { disconnected.push(handler); }),
  } as unknown as IMeshTransport;
  const close = vi.fn(async () => { order.push("release"); });
  const deps = {
    acquireWriter: vi.fn(async () => { order.push("writer"); return { close }; }),
    custody: vi.fn<MissionServiceDependencies["custody"]>(async () => { order.push("custody"); return { ...custody, ...(withEntra ? { entra: { ...binding } } : {}) }; }),
    transport: vi.fn(async () => { order.push("transport"); return mesh; }),
    authRequest: vi.fn<MissionServiceDependencies["authRequest"]>(async method => {
      order.push(method === "GET" ? "mint" : "verify");
      return method === "GET" ? { authorizationHeader: "Bearer dispatcher-sdk-token" } : { ...verified };
    }),
    socketFactory: vi.fn<MissionServiceDependencies["socketFactory"]>(async getter => { auth = getter; return wsFactory; }),
    now: vi.fn(() => 1_000), warn: vi.fn(),
  };
  const api: KubernetesJson = { request: vi.fn(async () => { throw new Error("Unexpected API call"); }) };
  const pending = vi.spyOn(KubernetesMissionStore.prototype, "pendingTasks").mockImplementation(async function* (this: KubernetesMissionStore) {
    discoveryGate = Reflect.get(this, "dispatchCurrent");
    yield* [];
  });
  const resolveCandidate = vi.spyOn(KubernetesMissionStore.prototype, "candidate").mockResolvedValue(candidate);
  // Workload binding has its own store tests; exercise the actual service-installed gate in Entra mode.
  const isCurrent = vi.spyOn(KubernetesMissionStore.prototype, "isCurrent").mockImplementation(async function (this: KubernetesMissionStore) {
    return withEntra ? Reflect.get(this, "dispatchCurrent")() : true;
  });
  const dispatch = vi.spyOn(MissionDispatcher.prototype, "dispatch").mockResolvedValue("already-claimed");
  const service = new MissionDispatchService(api, { ...config, ...(withEntra ? { entra } : {}) }, deps);
  let running: Promise<void> | undefined;
  let failure: unknown;
  return { service, deps, mesh, close, order, pending, resolveCandidate, isCurrent, dispatch, abort, api, tokens, drainInbox, wsFactory,
    auth: () => auth?.(), admitted: () => discoveryGate!(),
    knock: (from = candidate.agentDid) => knock(from, {}),
    disconnectEvent: () => { connected = false; disconnected.forEach(handler => handler("server")); },
    start: () => { running = service.run(abort.signal).catch(error => { failure = error; }); return running; },
    stop: async () => { abort.abort(); await running; }, failure: () => failure,
    tasks: () => pending.mockImplementation(async function* (this: KubernetesMissionStore) {
      discoveryGate = Reflect.get(this, "dispatchCurrent");
      yield { namespace: candidate.namespace, taskName: candidate.taskName };
    }),
  };
}
afterEach(() => vi.restoreAllMocks());

describe("mission dispatcher Entra lifecycle", () => {
  it("withholds readiness, discovery and admission until the pinned registry identity is verified", async () => {
    const h = harness(true); const mint = deferred<unknown>(); const verification = deferred<unknown>(); const delivery = deferred<"succeeded">();
    h.deps.authRequest.mockReturnValueOnce(mint.promise).mockReturnValueOnce(verification.promise);
    h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    try {
      await until(() => expect(h.deps.authRequest).toHaveBeenCalledOnce());
      expect(h.service.ready()).toBe(false); expect(h.mesh.connect).not.toHaveBeenCalled();
      expect(await h.knock()).toMatchObject({ accept: false }); expect(h.pending).not.toHaveBeenCalled();
      mint.resolve({ authorizationHeader: "Bearer pinned-sdk-token" });
      await until(() => expect(h.deps.authRequest).toHaveBeenCalledTimes(2));
      expect(h.mesh.isConnected).toBe(true); expect(h.auth()?.client.isConnected).toBe(false);
      expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
      expect(h.pending).not.toHaveBeenCalled(); expect(h.dispatch).not.toHaveBeenCalled(); expect(h.drainInbox).not.toHaveBeenCalled();
      verification.resolve(verified);
      await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
      expect(h.service.ready()).toBe(true); expect(h.auth()?.usable).toBe(true);
      expect(h.auth()?.client).not.toBe(h.mesh);
      expect(h.deps.custody).toHaveBeenCalledWith(h.api, config.location, config.root, entra);
      expect(h.deps.transport).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ identity, wsFactory: h.wsFactory }));
      expect(h.deps.socketFactory).toHaveBeenCalledOnce(); expect(h.mesh.onDisconnect).toHaveBeenCalledTimes(2);
      expect(h.tokens).toEqual(["pinned-sdk-token"]); expect(h.drainInbox).toHaveBeenCalledOnce();
      expect(h.deps.authRequest.mock.calls.map(call => call.slice(0, 3))).toEqual([
        ["GET", sdkUrl, undefined], ["POST", `${config.registryUrl}/v1/registry/verify`, { amid: identity.did, verification_token: "pinned-sdk-token" }],
      ]);
      expect(h.deps.authRequest.mock.calls.every(call => call[3] instanceof AbortSignal)).toBe(true);
      expect(await h.admitted()).toBe(true); expect(await h.knock()).toEqual({ accept: true });
      expect(await h.knock("other-peer")).toEqual({ accept: false });
      h.abort.abort(); expect(h.service.ready()).toBe(false);
      expect(await h.admitted()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
    } finally { mint.resolve({ authorizationHeader: "Bearer pinned-sdk-token" }); verification.resolve(verified); delivery.resolve("succeeded"); await h.stop(); }
    expect(h.failure()).toBeUndefined(); expect(h.order.slice(-2)).toEqual(["disconnect", "release"]);
  });
  it.each(["missing binding", "unavailable custody"])("waits for %s without constructing any speculative auth or SDK", async reason => {
    const h = harness(true);
    if (reason === "missing binding") h.deps.custody.mockResolvedValue({ ...custody });
    else h.deps.custody.mockRejectedValue(new Error("private Secret response"));
    h.start();
    try {
      await until(() => expect(h.deps.warn).toHaveBeenCalledWith("custody-unavailable"));
      expect(h.service.ready()).toBe(false); expect(h.deps.transport).not.toHaveBeenCalled();
      expect(h.deps.socketFactory).not.toHaveBeenCalled(); expect(h.deps.authRequest).not.toHaveBeenCalled();
      expect(h.dispatch).not.toHaveBeenCalled(); expect(h.close).not.toHaveBeenCalled();
      h.deps.custody.mockResolvedValue({ ...custody, entra: { ...binding } });
      await until(() => expect(h.service.ready()).toBe(true));
      expect(h.deps.transport).toHaveBeenCalledOnce();
      expect(JSON.stringify(h.deps.warn.mock.calls)).not.toContain("private Secret response");
    } finally { await h.stop(); }
  });
  it.each<[string, unknown, boolean]>([
    ["missing SDK header", {}, false], ["SDK failure", { authorizationHeader: "Bearer private-token", success: false }, false],
    ["oversized SDK token", { authorizationHeader: `Bearer ${"x".repeat(8193)}` }, false],
    ["registry DID mismatch", { ...verified, did: candidate.agentDid }, true],
    ["registry app mismatch", { ...verified, verified_app_id: binding.objectId }, true],
    ["registry tenant mismatch", { ...verified, verified_tenant_id: binding.blueprintId }, true],
    ["unverified registry tier", { ...verified, tier: "anonymous" }, true],
    ["explicit registry failure", { ...verified, verified: false }, true],
  ])("retires and releases safely after %s without readiness or admission", async (_reason, result, registry) => {
    const h = harness(true);
    if (registry) h.deps.authRequest.mockResolvedValueOnce({ authorizationHeader: "Bearer private-token" }).mockResolvedValueOnce(result);
    else h.deps.authRequest.mockResolvedValueOnce(result);
    await h.start();
    expect(h.failure()).toEqual(new Error("Required Entra mesh authentication failed; process restart required"));
    expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
    expect(h.pending).not.toHaveBeenCalled(); expect(h.dispatch).not.toHaveBeenCalled(); expect(h.drainInbox).not.toHaveBeenCalled();
    expect(h.mesh.connect).toHaveBeenCalledTimes(registry ? 1 : 0);
    expect(h.mesh.disconnect).toHaveBeenCalledOnce(); expect(h.close).toHaveBeenCalledOnce();
    expect(h.order.slice(-2)).toEqual(["disconnect", "release"]); expect(h.deps.transport).toHaveBeenCalledOnce();
    expect(JSON.stringify(h.deps.warn.mock.calls)).not.toContain("private-token");
  });
  it.each([2, 3, 4, 5])("requires custody on both sides of both auth requests (read %s)", async rejectedRead => {
    const h = harness(true); let reads = 0;
    h.deps.custody.mockImplementation(async () => {
      if (++reads === rejectedRead) throw new Error("private custody error");
      return { ...custody, entra: { ...binding } };
    });
    await h.start();
    expect(h.failure()).toEqual(new Error("Required Entra mesh authentication failed; process restart required"));
    expect(h.deps.authRequest).toHaveBeenCalledTimes(rejectedRead === 2 ? 0 : rejectedRead === 5 ? 2 : 1);
    expect(h.mesh.connect).toHaveBeenCalledTimes(rejectedRead <= 3 ? 0 : 1);
    expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
    expect(h.pending).not.toHaveBeenCalled(); expect(h.mesh.disconnect).toHaveBeenCalledOnce(); expect(h.close).toHaveBeenCalledOnce();
    expect(JSON.stringify(h.deps.warn.mock.calls)).not.toContain("private custody error");
  });
  it.each(["outage", "rootUid", "journalUid", "appId", "objectId", "tenantId", "blueprintId"])("permanently revokes an active dispatcher after %s custody loss", async field => {
    const h = harness(true); const delivery = deferred<"succeeded">(); h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    try {
      await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
      expect(await h.knock()).toEqual({ accept: true }); expect(await h.admitted()).toBe(true);
      if (field === "outage") h.deps.custody.mockRejectedValue(new Error("private Secret failure"));
      else if (field === "rootUid") h.deps.custody.mockResolvedValue({ ...custody, rootUid: "replacement", entra: { ...binding } });
      else h.deps.custody.mockResolvedValue({ ...custody, entra: { ...binding, [field]: "e5555555-5555-4555-8555-555555555555" } });
      await until(() => expect(h.mesh.disconnect).toHaveBeenCalledOnce());
      expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false }); expect(await h.admitted()).toBe(false);
      expect(h.close).not.toHaveBeenCalled();
      h.deps.custody.mockResolvedValue({ ...custody, entra: { ...binding } });
      await expect(h.auth()!.connect()).rejects.toThrow("restart required");
      expect(h.service.ready()).toBe(false); expect(h.deps.transport).toHaveBeenCalledOnce();
      expect(h.deps.authRequest).toHaveBeenCalledTimes(2);
      delivery.resolve("succeeded");
      await until(() => expect(h.failure()).toBeInstanceOf(Error));
      expect(h.failure()).toEqual(new Error("Dispatcher identity custody changed; restart the process"));
    } finally { delivery.resolve("succeeded"); await h.stop(); }
    expect(h.close).toHaveBeenCalledOnce(); expect(h.mesh.disconnect).toHaveBeenCalledOnce();
  });
  it("retires before reconnecting one SDK with a fresh token and registry verification", async () => {
    const h = harness(true); const retirement = deferred<void>(); const verification = deferred<unknown>(); const delivery = deferred<"succeeded">();
    h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    try {
      await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
      vi.mocked(h.mesh.disconnect).mockReturnValueOnce(retirement.promise);
      h.deps.authRequest.mockResolvedValueOnce({ authorizationHeader: "Bearer fresh-reconnect-token" }).mockReturnValueOnce(verification.promise);
      h.disconnectEvent();
      expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false }); expect(await h.admitted()).toBe(false);
      expect(h.mesh.disconnect).toHaveBeenCalledOnce(); expect(h.deps.authRequest).toHaveBeenCalledTimes(2);
      expect(h.mesh.connect).toHaveBeenCalledOnce(); expect(h.close).not.toHaveBeenCalled();
      retirement.resolve(); await until(() => expect(h.deps.authRequest).toHaveBeenCalledTimes(4));
      expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
      expect(h.tokens).toEqual(["dispatcher-sdk-token", "fresh-reconnect-token"]);
      verification.resolve(verified); await until(() => expect(h.service.ready()).toBe(true));
      expect(await h.knock()).toEqual({ accept: true }); expect(await h.admitted()).toBe(true);
      expect(h.mesh.connect).toHaveBeenCalledTimes(2); expect(h.deps.transport).toHaveBeenCalledOnce();
      expect(h.deps.socketFactory).toHaveBeenCalledOnce(); expect(h.drainInbox).toHaveBeenCalledTimes(2);
    } finally { retirement.resolve(); verification.resolve(verified); delivery.resolve("succeeded"); await h.stop(); }
    expect(h.failure()).toBeUndefined(); expect(h.close).toHaveBeenCalledOnce();
  });
  it("rechecks admission after asynchronous custody loses its auth epoch", async () => {
    const h = harness(true); const check = deferred<DispatcherCustody>(); const retirement = deferred<void>(); const delivery = deferred<"succeeded">();
    h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    try {
      await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
      h.deps.custody.mockReturnValueOnce(check.promise);
      const admission = h.knock(); const discovery = h.admitted();
      vi.mocked(h.mesh.disconnect).mockReturnValueOnce(retirement.promise);
      h.disconnectEvent(); check.resolve({ ...custody, entra: { ...binding } });
      expect(await admission).toMatchObject({ accept: false }); expect(await discovery).toBe(false);
      expect(h.service.ready()).toBe(false);
    } finally { h.abort.abort(); check.resolve({ ...custody, entra: { ...binding } }); retirement.resolve(); delivery.resolve("succeeded"); await h.stop(); }
  });
  it.each(["mint", "verification"])("does not publish late %s success after shutdown", async stage => {
    const h = harness(true); const response = deferred<unknown>();
    if (stage === "verification") h.deps.authRequest.mockResolvedValueOnce({ authorizationHeader: "Bearer private-token" });
    h.deps.authRequest.mockReturnValueOnce(response.promise); h.start();
    try {
      await until(() => expect(h.deps.authRequest).toHaveBeenCalledTimes(stage === "mint" ? 1 : 2));
      h.abort.abort(); expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false });
      response.resolve(stage === "mint" ? { authorizationHeader: "Bearer private-token" } : verified);
      await h.stop();
      expect(h.service.ready()).toBe(false); expect(h.pending).not.toHaveBeenCalled(); expect(h.drainInbox).not.toHaveBeenCalled();
      expect(h.mesh.disconnect).toHaveBeenCalledOnce(); expect(h.close).toHaveBeenCalledOnce();
      expect(h.deps.authRequest.mock.calls.at(-1)![3]!.aborted).toBe(true);
    } finally { response.resolve(verified); await h.stop(); }
  });
  it("quarantines failed retirement without releasing the writer or permitting reauthentication", async () => {
    const h = harness(true); h.start();
    try {
      await until(() => expect(h.service.ready()).toBe(true));
      vi.mocked(h.mesh.disconnect).mockRejectedValue(new Error("private socket error"));
      h.deps.custody.mockRejectedValue(new Error("private Secret failure"));
      await until(() => expect(h.failure()).toBeInstanceOf(Error));
      expect(h.failure()).toEqual(new Error("Dispatcher transport retirement uncertain; writer retained until process exit"));
      expect(h.auth()?.state).toBe("quarantined"); expect(h.mesh.isConnected).toBe(true);
      expect(h.service.ready()).toBe(false); expect(await h.knock()).toMatchObject({ accept: false }); expect(await h.admitted()).toBe(false);
      await expect(h.auth()!.connect()).rejects.toThrow("restart required");
      expect(h.close).not.toHaveBeenCalled(); expect(h.mesh.disconnect).toHaveBeenCalledOnce(); expect(h.deps.transport).toHaveBeenCalledOnce();
    } finally { await h.stop(); }
  });
});

describe("mission dispatcher service lifecycle", () => {
  it("acquires custody before deriving one SDK identity and registers admission before connecting", async () => {
    const h = harness(); h.start();
    await until(() => expect(h.service.ready()).toBe(true));
    expect(h.order.slice(0, 5)).toEqual(["writer", "custody", "transport", "knock", "connect"]);
    expect(h.deps.transport).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ identity, capabilities: ["kars:mission-dispatch"] }));
    await h.stop();
    expect(h.order.slice(-2)).toEqual(["disconnect", "release"]);
    expect(h.service.ready()).toBe(false);
    await expect(h.service.run(new AbortController().signal)).rejects.toThrow("already started");
  });
  it("does not acquire a writer after an already-aborted start", async () => {
    const h = harness(); h.abort.abort(); await h.start();
    expect(h.deps.acquireWriter).not.toHaveBeenCalled();
    expect(h.deps.transport).not.toHaveBeenCalled();
  });
  it("releases a writer acquired after shutdown without bootstrapping", async () => {
    const h = harness(); const acquired = deferred<{ close: typeof h.close }>();
    h.deps.acquireWriter.mockReturnValueOnce(acquired.promise);
    h.start(); h.abort.abort(); acquired.resolve({ close: h.close }); await h.stop();
    expect(h.deps.custody).not.toHaveBeenCalled(); expect(h.close).toHaveBeenCalledOnce();
  });
  it("never initializes a transport when writer acquisition fails", async () => {
    const h = harness(); h.deps.acquireWriter.mockRejectedValueOnce(new Error("Writer held")); await h.start();
    expect(h.failure()).toBeInstanceOf(Error); expect(h.deps.custody).not.toHaveBeenCalled();
    expect(h.deps.transport).not.toHaveBeenCalled(); expect(h.close).not.toHaveBeenCalled();
  });
  it("waits for a committed root without creating a speculative SDK", async () => {
    const h = harness(); h.deps.custody.mockRejectedValueOnce(new Error("private API response")); h.start();
    await until(() => expect(h.deps.warn).toHaveBeenCalledWith("custody-unavailable"));
    expect(h.service.ready()).toBe(false); expect(h.deps.transport).not.toHaveBeenCalled();
    await until(() => expect(h.service.ready()).toBe(true)); await h.stop();
    expect(h.deps.transport).toHaveBeenCalledOnce();
    expect(JSON.stringify(h.deps.warn.mock.calls)).not.toContain("private API response");
  });
  it("can stop while waiting for bootstrap custody", async () => {
    const h = harness(); h.deps.custody.mockRejectedValue(new Error("Not ready")); h.start();
    await until(() => expect(h.deps.custody).toHaveBeenCalledOnce()); await h.stop();
    expect(h.deps.transport).not.toHaveBeenCalled(); expect(h.close).toHaveBeenCalledOnce();
  });
  it("does not create a second SDK after an ambiguous registration failure", async () => {
    const h = harness(); vi.mocked(h.mesh.connect).mockRejectedValue(new Error("Ambiguous registration")); await h.start();
    expect(h.failure()).toBeInstanceOf(Error); expect(h.deps.transport).toHaveBeenCalledOnce();
    expect(h.order.slice(-2)).toEqual(["disconnect", "release"]);
  });
  it.each(["identity", "plaintext"])("rejects a transport with invalid %s configuration", async reason => {
    const h = harness();
    if (reason === "identity") Object.defineProperty(h.mesh, "currentDid", { value: "wrong" });
    else vi.mocked(h.mesh.getPlaintextPeers).mockReturnValue([candidate.agentDid]);
    await h.start(); expect(h.failure()).toBeInstanceOf(Error); expect(h.pending).not.toHaveBeenCalled();
    expect(h.close).toHaveBeenCalledOnce();
  });
  it("rejects unsolicited peers when no assignment is active", async () => {
    const h = harness(); h.start(); await until(() => expect(h.service.ready()).toBe(true));
    expect(await h.knock()).toEqual({ accept: false }); expect(h.isCurrent).not.toHaveBeenCalled(); await h.stop();
  });
  it("admits only the active peer with a current binding, including during drain", async () => {
    const h = harness(); const delivery = deferred<"succeeded">(); h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise);
    h.start(); await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
    expect(await h.knock("arbitrary")).toEqual({ accept: false });
    expect(await h.knock()).toEqual({ accept: true });
    h.isCurrent.mockResolvedValueOnce(false); expect(await h.knock()).toEqual({ accept: false });
    h.isCurrent.mockRejectedValueOnce(new Error("Unavailable")); expect(await h.knock()).toEqual({ accept: false });
    h.abort.abort(); expect(h.service.ready()).toBe(false); expect(await h.knock()).toEqual({ accept: true });
    delivery.resolve("succeeded"); await h.stop();
    expect(h.dispatch).toHaveBeenCalledOnce(); expect(h.mesh.disconnect).toHaveBeenCalledOnce();
  });
  it("rejects admission if the active assignment ends during its custody check", async () => {
    const h = harness(); const delivery = deferred<"succeeded">(); const check = deferred<boolean>();
    h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
    h.isCurrent.mockReturnValueOnce(check.promise); const decision = h.knock();
    delivery.resolve("succeeded"); h.abort.abort(); await h.stop(); check.resolve(true);
    expect(await decision).toEqual({ accept: false });
  });
  it("monitors custody and freshness while a delivery is pending", async () => {
    const h = harness(); const delivery = deferred<"succeeded">(); h.tasks(); h.dispatch.mockReturnValueOnce(delivery.promise);
    h.start(); await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
    h.deps.now.mockReturnValue(46_000); expect(h.service.ready()).toBe(false);
    await until(() => expect(h.deps.custody.mock.calls.length).toBeGreaterThan(1));
    expect(h.service.ready()).toBe(true);
    h.deps.custody.mockRejectedValue(new Error("Temporary outage"));
    await until(() => expect(h.service.ready()).toBe(false));
    h.deps.custody.mockResolvedValue({ ...custody });
    await until(() => expect(h.service.ready()).toBe(true));
    h.disconnectEvent(); expect(h.service.ready()).toBe(false);
    delivery.resolve("succeeded"); await h.stop();
  });
  it("exits on a validated identity change rather than adopting another root", async () => {
    const h = harness(); h.start(); await until(() => expect(h.service.ready()).toBe(true));
    h.deps.custody.mockResolvedValue({ ...custody, rootUid: "replacement" });
    await until(() => expect(h.failure()).toBeInstanceOf(Error)); await h.stop();
    expect(h.service.ready()).toBe(false); expect(h.deps.transport).toHaveBeenCalledOnce();
    expect(h.close).toHaveBeenCalledOnce();
  });
  it("continues discovery after API failures without logging response bodies", async () => {
    const h = harness(); h.pending.mockImplementationOnce(async function* () {
      yield await Promise.reject(new Error("private response"));
    }); h.start();
    await until(() => expect(h.pending.mock.calls.length).toBeGreaterThan(1)); await h.stop();
    expect(h.deps.warn).toHaveBeenCalledWith("discovery-unavailable");
    expect(JSON.stringify(h.deps.warn.mock.calls)).not.toContain("private response");
  });
  it("isolates candidate failures and dispatches candidates serially", async () => {
    const h = harness(); const delivery = deferred<"already-claimed">();
    h.pending.mockImplementation(async function* () {
      yield { namespace: candidate.namespace, taskName: "invalid" };
      yield { namespace: candidate.namespace, taskName: "first" };
      yield { namespace: candidate.namespace, taskName: "second" };
    });
    h.resolveCandidate.mockRejectedValueOnce(new Error("Bad candidate"));
    h.dispatch.mockReturnValueOnce(delivery.promise); h.start();
    await until(() => expect(h.dispatch).toHaveBeenCalledOnce());
    expect(h.resolveCandidate).toHaveBeenCalledTimes(2);
    delivery.resolve("already-claimed"); await until(() => expect(h.dispatch).toHaveBeenCalledTimes(2)); await h.stop();
    expect(h.deps.warn).toHaveBeenCalledWith("candidate-unavailable");
  });
  it("retains the writer when socket retirement cannot be proven", async () => {
    const h = harness(); vi.mocked(h.mesh.disconnect).mockRejectedValue(new Error("Socket still alive"));
    h.start(); await until(() => expect(h.service.ready()).toBe(true)); await h.stop();
    expect(h.failure()).toBeInstanceOf(Error); expect(h.close).not.toHaveBeenCalled();
  });
  it.each([0, 99, 30_001, NaN])("rejects an invalid poll interval %s", pollMs => {
    expect(() => new MissionDispatchService({ request: vi.fn() }, { ...config, pollMs })).toThrow("timing");
  });
});
