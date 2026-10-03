// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { MissionDispatchService, type MissionServiceConfig } from "./mission-service.js";
import { MissionDispatcher, type MissionCandidate } from "./mission-dispatcher.js";
import { KubernetesMissionStore } from "./mission-store.js";
import { missionIdentity } from "./mission-identity.js";
import type { DispatcherCustody } from "./mission-bootstrap.js";
import type { IMeshTransport } from "./transport-interface.js";
import type { KubernetesJson } from "./kubernetes-json.js";

const config: MissionServiceConfig = {
  location: { namespace: "kars-system", deployment: "kars-mission-dispatcher", release: "kars", podName: "dispatcher-abc", podUid: "pod-uid" },
  root: "a".repeat(64), relayUrl: "ws://relay:8765", registryUrl: "http://registry:8080", pollMs: 100, timeoutMs: 1_000,
};
const identity = missionIdentity(config.root, "dispatcher", "deployment-uid", "pod-uid");
const custody: DispatcherCustody = { deploymentUid: "deployment-uid", podUid: "pod-uid", namespaceUid: "namespace-uid", rootUid: "root-uid" };
const candidate: MissionCandidate = { namespace: "kars-system", taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "runtime-pod-uid",
  runNonce: "run-1", agentDid: `did:mesh:${"b".repeat(32)}`, dispatcherDid: identity.did, agentName: "Briefing writer", content: "Write a useful briefing." };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const until = (assertion: () => void) => vi.waitFor(assertion, { interval: 5, timeout: 2_000 });
function harness() {
  const order: string[] = [];
  const abort = new AbortController();
  let connected = false;
  let knock!: Parameters<IMeshTransport["onKnock"]>[0];
  let disconnected!: Parameters<IMeshTransport["onDisconnect"]>[0];
  const mesh = {
    currentDid: identity.did, get isConnected() { return connected; },
    connect: vi.fn(async () => { order.push("connect"); connected = true; }),
    disconnect: vi.fn(async () => { order.push("disconnect"); connected = false; }),
    getPlaintextPeers: vi.fn(() => [] as string[]),
    onKnock: vi.fn((handler: typeof knock) => { order.push("knock"); knock = handler; }),
    onError: vi.fn(), onDisconnect: vi.fn((handler: typeof disconnected) => { disconnected = handler; }),
  } as unknown as IMeshTransport;
  const close = vi.fn(async () => { order.push("release"); });
  const deps = {
    acquireWriter: vi.fn(async () => { order.push("writer"); return { close }; }),
    custody: vi.fn(async () => { order.push("custody"); return { ...custody }; }),
    transport: vi.fn(async () => { order.push("transport"); return mesh; }),
    now: vi.fn(() => 1_000), warn: vi.fn(),
  };
  const api: KubernetesJson = { request: vi.fn(async () => { throw new Error("Unexpected API call"); }) };
  const pending = vi.spyOn(KubernetesMissionStore.prototype, "pendingTasks").mockImplementation(async function* () {});
  const resolveCandidate = vi.spyOn(KubernetesMissionStore.prototype, "candidate").mockResolvedValue(candidate);
  const isCurrent = vi.spyOn(KubernetesMissionStore.prototype, "isCurrent").mockResolvedValue(true);
  const dispatch = vi.spyOn(MissionDispatcher.prototype, "dispatch").mockResolvedValue("already-claimed");
  const service = new MissionDispatchService(api, config, deps);
  let running: Promise<void> | undefined;
  let failure: unknown;
  return { service, deps, mesh, close, order, pending, resolveCandidate, isCurrent, dispatch, abort,
    knock: (from = candidate.agentDid) => knock(from, {}),
    disconnectEvent: () => { connected = false; disconnected("server"); },
    start: () => { running = service.run(abort.signal).catch(error => { failure = error; }); return running; },
    stop: async () => { abort.abort(); await running; }, failure: () => failure,
    tasks: () => pending.mockImplementation(async function* () { yield { namespace: candidate.namespace, taskName: candidate.taskName }; }),
  };
}
afterEach(() => vi.restoreAllMocks());

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
