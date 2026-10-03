// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { KubernetesError, type KubernetesJson } from "./kubernetes-json.js";
import { KubernetesMissionStore, missionObjective } from "./mission-store.js";
import { MissionDispatcher, missionAttemptName, missionContentDigest, type MissionAttempt, type MissionCandidate } from "./mission-dispatcher.js";
import type { MissionReply } from "./mission-protocol.js";
import type { IMeshTransport } from "./transport-interface.js";
import type { MissionMetadata, MissionWorkloadResources } from "./mission-workload.js";

const c: MissionCandidate = { namespace: "kars-system", taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid",
  runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`, agentName: "Briefing writer", content: "Write an approval checklist." };
const p = "kars.azure.com/";
const taskPath = `/apis/kars.azure.com/v1alpha1/namespaces/${c.namespace}/karstasks/${c.taskName}`;
const mapsPath = `/api/v1/namespaces/${c.namespace}/configmaps`;
const owner = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: c.taskName, uid: c.taskUid, controller: true };
const evidence = { model: "gpt-5.4-mini", rounds: 1, usage: { promptTokens: 20, completionTokens: 30, totalTokens: 50 } };
const stamp = "2026-10-02T20:00:00.000Z";
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
interface Resource {
  metadata: MissionMetadata;
  spec?: NonNullable<MissionWorkloadResources["deployment"]["spec"]> & { objective?: string; execution?: { launch: boolean }; suspended?: boolean };
  status?: NonNullable<MissionWorkloadResources["deployment"]["status"]> & { executionPhase?: string; sandboxRef?: { name: string }; phase?: string; conditions?: Array<{ type: string; status: string }> };
  data?: Record<string, string>; immutable?: boolean;
}

/** Test-only Kubernetes-shaped API with server-side resourceVersion and CREATE exclusion. */
class Api implements KubernetesJson {
  readonly resources = new Map<string, Resource>();
  readonly calls: Array<{ method: string; path: string; body?: Resource }> = [];
  before?: (method: string, path: string, body?: Resource) => void;
  private rv = 0;
  put(path: string, value: Resource): Resource {
    const result = clone(value); result.metadata.resourceVersion = String(++this.rv); this.resources.set(path, result); return clone(result);
  }
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const value = body as Resource | undefined;
    this.calls.push({ method, path, body: value ? clone(value) : undefined });
    this.before?.(method, path, value);
    const target = method === "POST" ? `${path}/${value!.metadata.name}` : path;
    const prior = this.resources.get(target);
    const fail = (status: number): never => { throw new KubernetesError(status, method, path); };
    if (method === "GET") return (prior ? clone(prior) : fail(404)) as T;
    if (method === "POST") return (prior ? fail(409) : this.put(target, value!)) as T;
    if (!prior) return fail(404);
    if (!value) return fail(400);
    if (value.metadata.resourceVersion !== prior.metadata.resourceVersion || (value.metadata.uid && value.metadata.uid !== prior.metadata.uid)) return fail(409);
    if (method === "PATCH") return this.put(target, { ...prior, metadata: { ...prior.metadata, annotations: { ...prior.metadata.annotations, ...value.metadata.annotations } } }) as T;
    if (method === "PUT") return (prior.immutable ? fail(422) : this.put(target, value)) as T;
    throw new Error(`Unexpected API operation ${method}`);
  }
}
function fixture() {
  const api = new Api();
  const task = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: { name: c.taskName, namespace: c.namespace, uid: c.taskUid, generation: 1, annotations: { [`${p}run-requested`]: c.runNonce } },
    spec: { objective: c.content, execution: { launch: true } }, status: { observedGeneration: 1, executionPhase: "Running", sandboxRef: { name: "briefing" } } };
  api.put(taskPath, task);
  const bindingPath = `${mapsPath}/kars-mission-binding-${c.taskName}`;
  api.put(bindingPath, { metadata: { name: `kars-mission-binding-${c.taskName}`, namespace: c.namespace, uid: "binding-uid", ownerReferences: [owner] },
    data: { "binding.json": JSON.stringify({ ...c, sandboxName: "briefing", podName: "briefing-abc", namespaceUid: "namespace-uid",
      deploymentUid: "deployment-uid", deploymentGeneration: 1, replicaSetName: "briefing-rs", replicaSetUid: "replicaset-uid" }) } });
  const sandboxPath = `/apis/kars.azure.com/v1alpha1/namespaces/${c.namespace}/karssandboxes/briefing`;
  api.put(sandboxPath, { metadata: { name: "briefing", namespace: c.namespace, uid: c.sandboxUid, ownerReferences: [owner], annotations: { [`${p}namespace-uid`]: "namespace-uid" } } });
  const namespacePath = "/api/v1/namespaces/kars-briefing";
  api.put(namespacePath, { metadata: { name: "kars-briefing", uid: "namespace-uid", annotations: {
    [`${p}namespace-claim-version`]: "v1", [`${p}sandbox-namespace`]: c.namespace, [`${p}sandbox-name`]: "briefing", [`${p}sandbox-uid`]: c.sandboxUid,
  } }, status: { phase: "Active" } });
  const deploymentPath = "/apis/apps/v1/namespaces/kars-briefing/deployments/briefing";
  const template = { metadata: { labels: { [`${p}sandbox`]: "briefing" } }, spec: { containers: [{ name: "openclaw", image: "sandbox:latest" }] } };
  const replicas = { observedGeneration: 1, replicas: 1, readyReplicas: 1, availableReplicas: 1 };
  api.put(deploymentPath, { metadata: { name: "briefing", namespace: "kars-briefing", uid: "deployment-uid", generation: 1,
    labels: { [`${p}sandbox`]: "briefing", [`${p}component`]: "sandbox", [`${p}parent-namespace`]: c.namespace },
    annotations: { "deployment.kubernetes.io/revision": "1" },
    managedFields: [{ manager: "kars-controller/karssandbox", operation: "Apply", fieldsV1: { "f:spec": {} } }],
  }, spec: { replicas: 1, template }, status: { ...replicas, updatedReplicas: 1 } });
  const replicaSetPath = "/apis/apps/v1/namespaces/kars-briefing/replicasets/briefing-rs";
  api.put(replicaSetPath, { metadata: { name: "briefing-rs", namespace: "kars-briefing", uid: "replicaset-uid", generation: 1,
    ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "briefing", uid: "deployment-uid", controller: true }],
    annotations: { "deployment.kubernetes.io/revision": "1" }, labels: { "pod-template-hash": "abc" },
  }, spec: { replicas: 1, template: { ...template, metadata: { labels: { ...template.metadata.labels, "pod-template-hash": "abc" } } } }, status: replicas });
  const podPath = "/api/v1/namespaces/kars-briefing/pods/briefing-abc";
  api.put(podPath, { metadata: { name: "briefing-abc", namespace: "kars-briefing", uid: c.podUid, labels: { "pod-template-hash": "abc" },
    ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "briefing-rs", uid: "replicaset-uid", controller: true }],
  }, status: { phase: "Running", conditions: [{ type: "Ready", status: "True" }] } });
  const paths = { task: taskPath, binding: bindingPath, sandbox: sandboxPath, namespace: namespacePath, deployment: deploymentPath, replicaSet: replicaSetPath, pod: podPath };
  return { api, store: new KubernetesMissionStore(api), task, bindingPath, sandboxPath, namespacePath, deploymentPath, replicaSetPath, podPath, paths };
}
function initial(candidate = c): MissionAttempt {
  return { version: 1, candidate, assignment: { ...candidate, type: "mission:assign", version: 1, bootId: "boot-1", assignmentId: "assignment-1" },
    contentDigest: missionContentDigest(candidate.content), ownerSession: "dispatcher-process", startedAt: stamp, updatedAt: stamp, phase: "dispatching", events: [] };
}
function next(prior: MissionAttempt, status: "accepted" | "running" | "succeeded" | "failed" | "rejected"): MissionAttempt {
  const reply: MissionReply = { ...prior.assignment, type: "mission:reply", status,
    ...(status === "succeeded" ? { output: "Actual approval checklist.", evidence } : {}) };
  return { ...prior, phase: status, reply, events: [...prior.events, { at: stamp, status, ...(reply.evidence ? { evidence: reply.evidence } : {}) }] };
}
async function terminal(store: KubernetesMissionStore, status: "succeeded" | "failed" = "succeeded") {
  const created = (await store.create(initial()))!;
  return (await store.replace(created, next(created.attempt, status))).attempt;
}
const key = missionAttemptName(c).replace("kars-mission-attempt-", "");
const outputPath = `${mapsPath}/kars-mission-output-${key}`;
const artifactPath = `${mapsPath}/kars-mission-artifacts-${key}`;
const projectionPath = `${mapsPath}/kars-mission-output-${c.taskName}`;

describe("mission candidate discovery", () => {
  it("rereads the Task and validates its live binding before returning a candidate", async () => {
    const h = fixture();
    expect(await h.store.candidate(c, c.dispatcherDid)).toEqual({ ...c, agentName: "briefing" });
    h.api.resources.get(taskPath)!.metadata.annotations![`${p}run-completed`] = c.runNonce;
    expect(await h.store.candidate(c, c.dispatcherDid)).toBeNull();
  });
  it.each(["dispatching", "succeeded", "uncertain"] as const)("recovers a %s claim before looking for a replacement binding", async phase => {
    const h = fixture();
    const created = (await h.store.create(initial()))!;
    if (phase === "succeeded") await h.store.replace(created, next(created.attempt, "succeeded"));
    if (phase === "uncertain") await h.store.replace(created, { ...created.attempt, phase, error: "Send outcome unknown" });
    h.api.resources.delete(h.bindingPath);
    h.api.calls.length = 0;
    expect(await h.store.candidate(c, c.agentDid)).toEqual(c);
    expect(h.api.calls.map(call => call.path)).toEqual([taskPath, `${mapsPath}/${missionAttemptName(c)}`]);
  });
  it("does not turn a malformed durable claim into a new dispatch", async () => {
    const h = fixture(); await h.store.create(initial());
    h.api.resources.get(`${mapsPath}/${missionAttemptName(c)}`)!.data!["attempt.json"] = "{}";
    await expect(h.store.candidate(c, c.dispatcherDid)).rejects.toThrow("Malformed");
  });
  it("does not recover an old Task UID's claim", async () => {
    const h = fixture(); await h.store.create(initial());
    h.api.resources.get(taskPath)!.metadata.uid = "replacement";
    expect(await h.store.candidate(c, c.dispatcherDid)).toBeNull();
  });
  it("requires current dispatcher custody for new candidates", async () => {
    const h = fixture(); const store = new KubernetesMissionStore(h.api, async () => false);
    expect(await store.isCurrent(c)).toBe(false);
    expect(await store.candidate(c, c.dispatcherDid)).toBeNull();
  });
  it("discovers only pending Tasks across encoded continuation pages", async () => {
    const paths: string[] = [];
    const task = (name: string, annotations: Record<string, string> = { [`${p}run-requested`]: "run" }) => ({ metadata: { name, namespace: c.namespace, uid: name, annotations } });
    const api: KubernetesJson = { async request<T>(_method: string, path: string): Promise<T> {
      paths.push(path);
      return (paths.length === 1 ? { items: [task("pending"), task("idle", {}), task("done", { [`${p}run-requested`]: "run", [`${p}run-completed`]: "run" })], metadata: { continue: "page+/=" } }
        : { items: [task("next"), { ...task("deleting"), metadata: { ...task("deleting").metadata, deletionTimestamp: stamp } }] }) as T;
    } };
    const found = []; for await (const ref of new KubernetesMissionStore(api).pendingTasks()) found.push(ref);
    expect(found).toEqual([{ namespace: c.namespace, taskName: "pending" }, { namespace: c.namespace, taskName: "next" }]);
    expect(paths).toEqual(["/apis/kars.azure.com/v1alpha1/karstasks?limit=10", "/apis/kars.azure.com/v1alpha1/karstasks?limit=10&continue=page%2B%2F%3D"]);
  });
  it.each([{ items: null }, { items: [], metadata: { continue: 7 } }, { items: [], metadata: { continue: "repeat" } }])("rejects malformed or repeated pagination %j", async page => {
    const api: KubernetesJson = { async request<T>(): Promise<T> { return page as T; } };
    await expect((async () => { for await (const _ref of new KubernetesMissionStore(api).pendingTasks()) { /* drain */ } })()).rejects.toThrow();
  });
});

describe("Kubernetes mission attempt store", () => {
  it("requires a current owned Task, Sandbox, binding and exact Ready Pod", async () => {
    const h = fixture(); expect(await h.store.isCurrent(c)).toBe(true);
    expect(await h.store.isCurrent({ ...c, taskUid: "replacement" })).toBe(false);
    expect(await h.store.isCurrent({ ...c, content: "Changed objective" })).toBe(false);
    expect(await h.store.isCurrent({ ...c, podUid: "replacement" })).toBe(false);
    expect(await h.store.isCurrent({ ...c, dispatcherDid: c.agentDid })).toBe(false);
  });
  it.each(["task", "binding", "sandbox", "pod"] as const)("rejects a missing %s", async kind => {
    const h = fixture(); h.api.resources.delete({ task: taskPath, binding: h.bindingPath, sandbox: h.sandboxPath, pod: h.podPath }[kind]);
    expect(await h.store.isCurrent(c)).toBe(false);
  });
  it.each(["task", "sandbox", "pod"] as const)("rejects a deleting or recreated %s", async kind => {
    const h = fixture(); const path = { task: taskPath, sandbox: h.sandboxPath, pod: h.podPath }[kind];
    const resource = h.api.resources.get(path)!; resource.metadata.deletionTimestamp = stamp;
    expect(await h.store.isCurrent(c)).toBe(false);
    delete resource.metadata.deletionTimestamp; resource.metadata.uid = "replacement";
    expect(await h.store.isCurrent(c)).toBe(false);
  });
  it.each([null, [], 7, {}, { podName: "../elsewhere" }, { podName: 7 }, { sandboxName: "x".repeat(59) }, { sandboxName: "a.b" }])("rejects malformed runtime bindings %j without issuing resource lookups", async mutation => {
    const h = fixture(); const cm = h.api.resources.get(h.bindingPath)!;
    const binding = JSON.parse(cm.data!["binding.json"]);
    cm.data!["binding.json"] = JSON.stringify(mutation && !Array.isArray(mutation) && typeof mutation === "object"
      && Object.keys(mutation).length ? { ...binding, ...mutation } : mutation);
    expect(await h.store.isCurrent(c)).toBe(false);
    expect(h.api.calls).toHaveLength(2);
  });
  it.each(["task", "binding", "sandbox", "pod"] as const)("rejects mismatched %s resource metadata", async kind => {
    const h = fixture(); const path = { task: taskPath, binding: h.bindingPath, sandbox: h.sandboxPath, pod: h.podPath }[kind];
    const metadata = h.api.resources.get(path)!.metadata; const name = metadata.name;
    metadata.name = "other"; expect(await h.store.isCurrent(c)).toBe(false);
    metadata.name = name; metadata.namespace = "other"; expect(await h.store.isCurrent(c)).toBe(false);
  });
  it("rejects a stale generation and not-Ready Pod", async () => {
    const h = fixture(); h.api.resources.get(taskPath)!.status!.observedGeneration = 0;
    expect(await h.store.isCurrent(c)).toBe(false);
    h.api.resources.get(taskPath)!.status!.observedGeneration = 1;
    h.api.resources.get(h.podPath)!.status!.conditions = [{ type: "Ready", status: "False" }];
    expect(await h.store.isCurrent(c)).toBe(false);
  });
  it.each(["task", "binding", "sandbox", "namespace", "deployment", "replicaSet", "pod"] as const)("requires every %s identity/revision and detects races during the collection", async kind => {
    for (const field of ["uid", "resourceVersion"] as const) {
      const h = fixture(); delete h.api.resources.get(h.paths[kind])!.metadata[field];
      expect(await h.store.isCurrent(c)).toBe(false);
    }
    for (const replacement of [false, true]) {
      const h = fixture(); let reads = 0;
      h.api.before = (method, path) => {
        if (method === "GET" && path === h.paths[kind] && ++reads === 2) {
          const current = clone(h.api.resources.get(path)!);
          if (replacement) current.metadata.uid = "replacement";
          h.api.put(path, current);
        }
      };
      expect(await h.store.isCurrent(c)).toBe(false);
      expect(reads).toBe(2);
      expect(h.api.calls.every(call => call.method === "GET")).toBe(true);
    }
  });
  it.each(["namespace", "deployment", "replicaSet"] as const)("rejects missing, deleting, or replaced %s workloads", async kind => {
    const h = fixture(); const path = h.paths[kind]; const saved = clone(h.api.resources.get(path)!);
    h.api.resources.delete(path); expect(await h.store.isCurrent(c)).toBe(false);
    h.api.put(path, { ...saved, metadata: { ...saved.metadata, deletionTimestamp: stamp } }); expect(await h.store.isCurrent(c)).toBe(false);
    h.api.put(path, { ...saved, metadata: { ...saved.metadata, uid: "replacement" } }); expect(await h.store.isCurrent(c)).toBe(false);
  });
  it.each(["binding", "sandbox", "replicaSet", "pod"] as const)("requires exactly one correct controller owner for %s", async kind => {
    const h = fixture(); const meta = h.api.resources.get(h.paths[kind])!.metadata; const correct = clone(meta.ownerReferences![0]);
    for (const refs of [[], [{ ...correct, uid: "other" }], [{ ...correct, name: "other" }], [{ ...correct, kind: "Other" }],
      [{ ...correct, apiVersion: "other/v1" }], [{ ...correct, controller: false }], [correct, correct]]) {
      meta.ownerReferences = refs; expect(await h.store.isCurrent(c)).toBe(false);
    }
    meta.ownerReferences = [correct, { ...correct, kind: "Observer", controller: false }];
    expect(await h.store.isCurrent(c)).toBe(true);
  });
  it.each([
    ["namespace", `${p}namespace-claim-version`], ["namespace", `${p}sandbox-namespace`],
    ["namespace", `${p}sandbox-name`], ["namespace", `${p}sandbox-uid`], ["sandbox", `${p}namespace-uid`],
  ] as const)("requires exact %s claim annotation %s", async (kind, annotation) => {
    const h = fixture(); const meta = h.api.resources.get(h.paths[kind])!.metadata;
    delete meta.annotations![annotation]; expect(await h.store.isCurrent(c)).toBe(false);
    meta.annotations![annotation] = "other"; expect(await h.store.isCurrent(c)).toBe(false);
  });
  it("rejects namespace adoption, suspended sandboxes and credential-rebind holds", async () => {
    for (const mutate of [
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.namespacePath)!.metadata.annotations![`${p}namespace-prestage`] = "bind-next-sandbox"; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.namespacePath)!.metadata.ownerReferences = [owner]; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.namespacePath)!.status!.phase = "Terminating"; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.sandboxPath)!.spec = { suspended: true }; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.sandboxPath)!.metadata.annotations![`${p}credential-rebind-task-uid`] = c.taskUid; },
    ]) { const h = fixture(); mutate(h); expect(await h.store.isCurrent(c)).toBe(false); }
  });
  it("requires Deployment custody, not labels or a cross-namespace Sandbox owner", async () => {
    const h = fixture(); const meta = h.api.resources.get(h.deploymentPath)!.metadata;
    delete meta.managedFields; expect(await h.store.isCurrent(c)).toBe(false);
    meta.managedFields = [{ manager: "other", operation: "Apply", fieldsV1: { "f:spec": {} } }]; expect(await h.store.isCurrent(c)).toBe(false);
    meta.managedFields[0].manager = "kars-controller/karssandbox"; meta.managedFields[0].operation = "Update"; expect(await h.store.isCurrent(c)).toBe(false);
    meta.annotations![`${p}credential-sandbox-uid`] = c.sandboxUid;
    meta.annotations![`${p}credential-namespace-uid`] = "namespace-uid"; expect(await h.store.isCurrent(c)).toBe(true);
    meta.ownerReferences = [owner]; expect(await h.store.isCurrent(c)).toBe(false);
    delete meta.ownerReferences; meta.annotations![`${p}credential-namespace-uid`] = "old"; expect(await h.store.isCurrent(c)).toBe(false);
  });
  it.each(["task", "deployment", "replicaSet"] as const)("requires positive, observed %s generations", async kind => {
    for (const generation of [undefined, 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, 2]) {
      const h = fixture(); const r = h.api.resources.get(h.paths[kind])!;
      r.metadata.generation = generation;
      if (generation !== 2) r.status!.observedGeneration = generation;
      expect(await h.store.isCurrent(c)).toBe(false);
    }
  });
  it.each(["deployment", "replicaSet"] as const)("waits for the complete single-replica %s rollout", async kind => {
    for (const field of ["replicas", "readyReplicas", "availableReplicas"] as const) {
      for (const count of [undefined, 0, 2]) {
        const h = fixture(); h.api.resources.get(h.paths[kind])!.status![field] = count;
        expect(await h.store.isCurrent(c)).toBe(false);
      }
    }
    const h = fixture(); const r = h.api.resources.get(h.paths[kind])!;
    r.status!.terminatingReplicas = 1; expect(await h.store.isCurrent(c)).toBe(false);
    delete r.status!.terminatingReplicas; r.spec!.replicas = 0; expect(await h.store.isCurrent(c)).toBe(false);
  });
  it("rejects paused or partially updated Deployments and mismatched templates/revisions/hashes", async () => {
    for (const mutate of [
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.deploymentPath)!.spec!.paused = true; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.deploymentPath)!.status!.updatedReplicas = 0; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.replicaSetPath)!.metadata.annotations!["deployment.kubernetes.io/revision"] = "2"; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.podPath)!.metadata.labels!["pod-template-hash"] = "old"; },
      (h: ReturnType<typeof fixture>) => { h.api.resources.get(h.replicaSetPath)!.spec!.template!.spec!.containers![0].image = "other:latest"; },
      (h: ReturnType<typeof fixture>) => { delete h.api.resources.get(h.deploymentPath)!.spec!.template; },
    ]) { const h = fixture(); mutate(h); expect(await h.store.isCurrent(c)).toBe(false); }
  });
  it.each([{ namespaceUid: "" }, { deploymentUid: null }, { replicaSetUid: "" }, { replicaSetName: "../bad" },
    { deploymentGeneration: 0 }, { deploymentGeneration: "1" }, { deploymentGeneration: 1.5 }])("rejects malformed workload binding fields %j before workload reads", async mutation => {
    const h = fixture(); const cm = h.api.resources.get(h.bindingPath)!;
    cm.data!["binding.json"] = JSON.stringify({ ...JSON.parse(cm.data!["binding.json"]), ...mutation });
    expect(await h.store.isCurrent(c)).toBe(false); expect(h.api.calls).toHaveLength(2);
  });
  it("does not dispatch or claim when the bound Pod has no current ReplicaSet owner", async () => {
    const h = fixture(); delete h.api.resources.get(h.podPath)!.metadata.ownerReferences;
    const mesh = { currentDid: c.dispatcherDid, isConnected: true, isPlaintextPeer: () => false } as unknown as IMeshTransport;
    expect(await new MissionDispatcher(mesh, h.store, "process", 1000).dispatch(c)).toBe("stale");
    expect(h.api.calls.every(call => call.method === "GET")).toBe(true);
  });
  it("decodes only an exact nonce-bound, digest-checked UTF-8 revision objective", () => {
    const { task } = fixture(); const text = "Revise café guidance ✓";
    Object.assign(task.metadata.annotations, { [`${p}run-objective-nonce`]: "rev-1", [`${p}run-objective-b64`]: Buffer.from(text).toString("base64"), [`${p}run-objective-digest`]: missionContentDigest(text) });
    expect(missionObjective(task, "rev-1")).toBe(text);
    expect(missionObjective(task, "rev-2")).toBeNull();
    Object.assign(task.metadata.annotations, { [`${p}run-objective-b64`]: "invalid!" });
    expect(missionObjective(task, "rev-1")).toBeNull();
    const invalid = Buffer.from([0xff]);
    Object.assign(task.metadata.annotations, { [`${p}run-objective-b64`]: invalid.toString("base64"), [`${p}run-objective-digest`]: missionContentDigest(invalid.toString()) });
    expect(missionObjective(task, "rev-1")).toBeNull();
  });
  it("atomically excludes competing creates and rejects stale resourceVersion updates", async () => {
    const h = fixture(); const results = await Promise.all([h.store.create(initial()), h.store.create(initial())]);
    expect(results.filter(Boolean)).toHaveLength(1);
    const first = results.find(Boolean)!;
    await h.store.replace(first, next(first.attempt, "accepted"));
    await expect(h.store.replace(first, next(first.attempt, "failed"))).rejects.toMatchObject({ status: 409 });
  });
  it("retains the old attempt after Pod replacement rather than authorizing reexecution", async () => {
    const h = fixture(); await h.store.create(initial());
    expect((await h.store.get({ ...c, podUid: "new-pod" }))!.attempt.candidate.podUid).toBe(c.podUid);
  });
  it("rejects forged terminal creates and mutated claim identities", async () => {
    const h = fixture(); await expect(h.store.create(next(initial(), "succeeded"))).rejects.toThrow("start dispatching");
    const old = (await h.store.create(initial()))!;
    for (const changed of [ { ...next(old.attempt, "accepted"), ownerSession: "other-process" },
      { ...next(old.attempt, "accepted"), startedAt: "2026-10-01T00:00:00Z" },
      { ...next(old.attempt, "accepted"), candidate: { ...c, podUid: "other" } } ]) {
      await expect(h.store.replace(old, changed)).rejects.toThrow("fenced or terminal");
    }
  });
  it("enforces append-only journals, monotonic dates and terminal immutability", async () => {
    const h = fixture(); const created = (await h.store.create(initial()))!;
    const running = await h.store.replace(created, next(created.attempt, "running"));
    await expect(h.store.replace(running, next(running.attempt, "accepted"))).rejects.toThrow();
    await expect(h.store.replace(running, { ...next(running.attempt, "failed"), updatedAt: "2026-10-01T00:00:00Z" })).rejects.toThrow();
    const edited = next(running.attempt, "succeeded"); edited.events[0] = { at: stamp, status: "accepted" };
    await expect(h.store.replace(running, edited)).rejects.toThrow();
    const done = await h.store.replace(running, next(running.attempt, "succeeded"));
    await expect(h.store.replace(done, { ...done.attempt, phase: "uncertain", error: "lost" })).rejects.toThrow();
  });
  it("rejects malformed persisted journals and wrong ConfigMap ownership", async () => {
    const h = fixture(); await h.store.create(initial());
    const cm = h.api.resources.get(`${mapsPath}/${missionAttemptName(c)}`)!;
    const broken = initial(); broken.events = [{ at: stamp, status: "ready" }]; cm.data!["attempt.json"] = JSON.stringify(broken);
    await expect(h.store.get(c)).rejects.toThrow();
    cm.data!["attempt.json"] = JSON.stringify(initial()); cm.metadata.ownerReferences![0].uid = "other-task";
    await expect(h.store.get(c)).rejects.toThrow("ownership");
  });
  it("publishes immutable actual output, one retrievable response file, attribution and measured tokens before completion", async () => {
    const h = fixture(); const done = await terminal(h.store);
    expect(await h.store.publish(done)).toBe(true);
    const output = h.api.resources.get(outputPath)!;
    expect(output.immutable).toBe(true);
    expect(output.data).toMatchObject({ taskUid: c.taskUid, assignmentNonce: c.runNonce, agentDid: c.agentDid, agentName: c.agentName,
      output: done.reply!.output, status: "ok", totalTokens: "50", usageKnown: "true", artifactCount: "1" });
    expect(h.api.resources.get(artifactPath)!.data).toEqual({ "response.md": done.reply!.output });
    for (const path of [outputPath, artifactPath, projectionPath, `${mapsPath}/kars-mission-artifacts-${c.taskName}`]) {
      expect(h.api.resources.get(path)!.metadata.annotations).toMatchObject({
        [`${p}mission-task-uid`]: c.taskUid, [`${p}mission-run-nonce`]: c.runNonce,
      });
    }
    expect(h.api.resources.get(projectionPath)!.data).toEqual(output.data);
    expect(h.api.calls.at(-1)).toMatchObject({ method: "PATCH", path: taskPath, body: { metadata: { uid: c.taskUid, annotations: { [`${p}run-completed`]: c.runNonce } } } });
  });
  it("requires persisted matching terminal evidence before publishing", async () => {
    const h = fixture(); await expect(h.store.publish(next(initial(), "succeeded"))).rejects.toThrow("persisted");
    const done = await terminal(h.store); done.reply!.output = "Modified output";
    await expect(h.store.publish(done)).rejects.toThrow("persisted");
    expect(h.api.resources.has(outputPath)).toBe(false);
  });
  it("retries immutable publication independently of JSON object key order", async () => {
    const h = fixture(); const done = await terminal(h.store); await h.store.publish(done);
    for (const path of [outputPath, artifactPath]) {
      const cm = h.api.resources.get(path)!; cm.data = Object.fromEntries(Object.entries(cm.data!).reverse());
      cm.metadata.ownerReferences = cm.metadata.ownerReferences!.map(o => Object.fromEntries(Object.entries(o).reverse()) as typeof owner);
    }
    expect(await h.store.publish(clone(done))).toBe(true);
    expect(h.api.calls.filter(call => call.method === "PATCH")).toHaveLength(1);
  });
  it("rejects immutable evidence collisions", async () => {
    const h = fixture(); const done = await terminal(h.store); await h.store.publish(done);
    h.api.resources.get(artifactPath)!.data!["response.md"] = "Different output";
    await expect(h.store.publish(done)).rejects.toThrow("collision");
  });
  it("records failed unknown usage as unknown, with no manufactured artifact or zero usage", async () => {
    const h = fixture(); const done = await terminal(h.store, "failed"); await h.store.publish(done);
    expect(h.api.resources.get(outputPath)!.data).toMatchObject({ status: "failed", output: "", artifactCount: "0", usageKnown: "false" });
    expect(h.api.resources.get(outputPath)!.data).not.toHaveProperty("totalTokens");
    expect(h.api.resources.get(artifactPath)!.data).toEqual({});
  });
  it("archives an old run but never completes a newer requested run", async () => {
    const h = fixture(); const done = await terminal(h.store);
    h.api.resources.get(taskPath)!.metadata.annotations![`${p}run-requested`] = "rev-2";
    expect(await h.store.publish(done)).toBe(false);
    expect(h.api.resources.has(outputPath)).toBe(true);
    expect(h.api.resources.has(projectionPath)).toBe(false);
    expect(h.api.calls.some(call => call.method === "PATCH")).toBe(false);
  });
  it("does not report a previously completed run as current after a new request", async () => {
    const h = fixture(); const done = await terminal(h.store); await h.store.publish(done);
    h.api.resources.get(taskPath)!.metadata.annotations![`${p}run-requested`] = "rev-2";
    expect(await h.store.publish(done)).toBe(false);
  });
  it("fences completion with the Task resourceVersion against a concurrent revision", async () => {
    const h = fixture(); const done = await terminal(h.store);
    h.api.before = (method, path) => {
      if (method === "PATCH" && path === taskPath) {
        const current = clone(h.api.resources.get(taskPath)!); current.metadata.annotations![`${p}run-requested`] = "rev-2"; h.api.put(taskPath, current);
      }
    };
    await expect(h.store.publish(done)).rejects.toMatchObject({ status: 409 });
    expect(h.api.resources.get(taskPath)!.metadata.annotations).not.toHaveProperty(`${p}run-completed`);
    expect(h.api.resources.get(outputPath)!.data!.assignmentNonce).toBe(c.runNonce);
  });
  it("fences a stale projection writer if a newer publisher wins after the current-Task read", async () => {
    const h = fixture(); const done = await terminal(h.store);
    h.api.put(projectionPath, { metadata: { name: `kars-mission-output-${c.taskName}`, namespace: c.namespace, ownerReferences: [owner] }, data: { assignmentNonce: "older" } });
    h.api.before = (method, path) => {
      if (method === "PUT" && path === projectionPath) {
        const current = clone(h.api.resources.get(taskPath)!); current.metadata.annotations![`${p}run-requested`] = "rev-2"; h.api.put(taskPath, current);
        const projection = clone(h.api.resources.get(path)!); projection.data = { assignmentNonce: "rev-2" }; h.api.put(path, projection);
      }
    };
    await expect(h.store.publish(done)).rejects.toMatchObject({ status: 409 });
    expect(h.api.resources.get(projectionPath)!.data!.assignmentNonce).toBe("rev-2");
  });
  it("runs dispatcher persistence and recovery through the Kubernetes-shaped adapter", async () => {
    const h = fixture(); const replies: MissionReply[] = [];
    const mesh = { currentDid: c.dispatcherDid, isConnected: true, isPlaintextPeer: () => false,
      sendWithAck: async (_to: string, probe: object, match: (payload: unknown, from: string, security: string) => MissionReply) => match({ ...probe, type: "mission:reply", bootId: "boot-1", status: "ready" }, c.agentDid, "encrypted"),
      send: async (_to: string, assignment: MissionAttempt["assignment"]) => { for (const status of ["accepted", "running", "succeeded"] as const) replies.push({ ...assignment, type: "mission:reply", status, ...(status === "succeeded" ? { output: "Real test reply", evidence } : {}) }); },
      waitForMessage: async (match: (payload: unknown, from: string, security: string) => MissionReply) => match(replies.shift(), c.agentDid, "encrypted"),
    } as unknown as IMeshTransport;
    const dispatcher = new MissionDispatcher(mesh, h.store, "process", 1000);
    expect(await dispatcher.dispatch(c)).toBe("succeeded");
    expect((await h.store.get(c))!.attempt.events).toHaveLength(3);
    expect(await dispatcher.dispatch(c)).toBe("already-claimed");
    expect(h.api.resources.get(artifactPath)!.data!["response.md"]).toBe("Real test reply");
  });
});
