// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { KubernetesError, type KubernetesJson } from "./kubernetes-json.js";
import { missionAttemptName, missionContentDigest, type MissionAttempt, type MissionAttemptStore, type MissionCandidate, type StoredMissionAttempt } from "./mission-dispatcher.js";
import { missionContract, missionEvidenceAdvances, parseMissionMessage, sameMissionContract, sameMissionTarget, type MissionReply } from "./mission-protocol.js";
import { currentMissionWorkload, singleControllerOwner, type MissionMetadata as Metadata, type MissionWorkloadBinding, type MissionWorkloadResources } from "./mission-workload.js";
interface ConfigMap { apiVersion: "v1"; kind: "ConfigMap"; metadata: Metadata; data: Record<string, string>; immutable?: boolean }
interface Task {
  apiVersion: string; kind: string; metadata: Metadata;
  spec: { objective: string; execution?: { launch?: boolean }; blueprint?: { executionPlan?: unknown } };
  status?: { observedGeneration?: number; executionPhase?: string; sandboxRef?: { name: string } };
}
interface Binding extends MissionWorkloadBinding {
  taskName: string; taskUid: string; agentDid: string; dispatcherDid: string;
}
function parseBinding(value: unknown): Binding | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const b = value as Record<string, unknown>;
  if (![b.taskName, b.taskUid, b.sandboxName, b.sandboxUid, b.podName, b.podUid, b.agentDid, b.dispatcherDid,
    b.namespaceUid, b.deploymentUid, b.replicaSetName, b.replicaSetUid]
    .every(v => typeof v === "string" && v.length > 0)
    || !Number.isSafeInteger(b.deploymentGeneration) || (b.deploymentGeneration as number) < 1) return null;
  const name = /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/;
  if (![b.sandboxName, b.podName, b.replicaSetName].every(v => name.test(v as string) && (v as string).length <= 253)
    || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(`kars-${b.sandboxName}`)
    || `kars-${b.sandboxName}`.length > 63) return null;
  return value as Binding;
}
const prefix = "kars.azure.com/";
const requested = `${prefix}run-requested`;
const completed = `${prefix}run-completed`;
const segment = (s: string): string => {
  if (!/^[a-z0-9][a-z0-9.-]{0,252}$/.test(s) || s === "." || s === "..") throw new Error("Invalid Kubernetes resource name");
  return encodeURIComponent(s);
};
const taskPath = (c: Pick<MissionCandidate, "namespace" | "taskName">): string => `/apis/kars.azure.com/v1alpha1/namespaces/${segment(c.namespace)}/karstasks/${segment(c.taskName)}`;
const mapsPath = (namespace: string): string => `/api/v1/namespaces/${segment(namespace)}/configmaps`;
const mapPath = (namespace: string, name: string): string => `${mapsPath(namespace)}/${segment(name)}`;
const isMissing = (e: unknown): boolean => e instanceof KubernetesError && e.status === 404;
const isConflict = (e: unknown): boolean => e instanceof KubernetesError && e.status === 409;
const owner = (candidate: MissionCandidate) => ({ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: candidate.taskName, uid: candidate.taskUid, controller: true });
const owned = (metadata: Metadata, candidate: Pick<MissionCandidate, "taskName" | "taskUid">): boolean =>
  !metadata.deletionTimestamp && singleControllerOwner(metadata, "kars.azure.com/v1alpha1", "KarsTask", candidate.taskName, candidate.taskUid);

export function missionObjective(task: Task, nonce: string): string | null {
  const annotations = task.metadata.annotations ?? {};
  if (annotations[`${prefix}run-objective-nonce`] === nonce) {
    const encoded = annotations[`${prefix}run-objective-b64`];
    if (!encoded || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) return null;
    const bytes = Buffer.from(encoded, "base64");
    const text = bytes.toString("utf8");
    if (!Buffer.from(text).equals(bytes) || !text.trim() || missionContentDigest(text) !== annotations[`${prefix}run-objective-digest`]) return null;
    return text;
  }
  // A revision nonce without its bound objective must not silently execute the original request.
  if (nonce.startsWith("rev-")) return null;
  return typeof task.spec.objective === "string" && task.spec.objective.trim() ? task.spec.objective : null;
}

function supportsTaskExecution(task: Task): boolean {
  // A single phase is not authority to execute a reviewed multi-role plan.
  return task.spec.blueprint?.executionPlan === undefined
    && task.metadata.annotations?.[`${prefix}mission-decomposition`] === undefined;
}

/** ConfigMap create/CAS supplies durable exclusion, including across dispatcher restarts. */
export class KubernetesMissionStore implements MissionAttemptStore {
  constructor(private readonly api: KubernetesJson, private readonly dispatchCurrent: () => Promise<boolean> = async () => true) {}

  async *pendingTasks(): AsyncGenerator<{ namespace: string; taskName: string }> {
    let continuation = "";
    const seen = new Set<string>();
    do {
      const page = await this.api.request<{ items: Task[]; metadata?: { continue?: string } }>("GET",
        `/apis/kars.azure.com/v1alpha1/karstasks?limit=10${continuation ? `&continue=${encodeURIComponent(continuation)}` : ""}`);
      if (!Array.isArray(page.items)) throw new Error("Invalid Task listing");
      for (const task of page.items) {
        const m = task.metadata;
        if (m?.namespace && m.name && m.uid && !m.deletionTimestamp && m.annotations?.[requested]
          && m.annotations[requested] !== m.annotations[completed]) {
          segment(m.namespace); segment(m.name);
          yield { namespace: m.namespace, taskName: m.name };
        }
      }
      continuation = page.metadata?.continue ?? "";
      if (typeof continuation !== "string" || (continuation && seen.has(continuation)) || seen.size >= 10_000) {
        throw new Error("Invalid or excessive Task pagination");
      }
      if (continuation) seen.add(continuation);
    } while (continuation);
  }

  async candidate(reference: Pick<MissionCandidate, "namespace" | "taskName">, dispatcherDid: string): Promise<MissionCandidate | null> {
    const task = await this.task(reference);
    const m = task?.metadata;
    const runNonce = m?.annotations?.[requested];
    if (!task || !m?.uid || !m.resourceVersion || m.deletionTimestamp || m.name !== reference.taskName
      || m.namespace !== reference.namespace || !runNonce || m.annotations?.[completed] === runNonce) return null;
    const key = { ...reference, taskUid: m.uid, runNonce };
    try {
      // Recover the original persisted target even after a Pod or dispatcher replacement.
      const cm = await this.api.request<ConfigMap>("GET", mapPath(reference.namespace, missionAttemptName(key)));
      return this.unpack(cm, key).attempt.candidate;
    } catch (e) { if (!isMissing(e)) throw e; }
    const content = missionObjective(task, runNonce);
    if (!content || task.spec.execution?.launch !== true || !supportsTaskExecution(task)) return null;
    try {
      const cm = await this.api.request<ConfigMap>("GET", mapPath(reference.namespace, `kars-mission-binding-${reference.taskName}`));
      const binding = parseBinding(JSON.parse(cm.data?.["binding.json"] ?? "null"));
      if (!binding || binding.taskName !== reference.taskName || binding.taskUid !== m.uid || binding.dispatcherDid !== dispatcherDid) return null;
      const candidate: MissionCandidate = { ...key, sandboxUid: binding.sandboxUid, podUid: binding.podUid,
        agentDid: binding.agentDid, dispatcherDid, agentName: binding.sandboxName, content };
      if (!parseMissionMessage({ ...candidate, type: "mission:probe", version: 1, challenge: "candidate-validation" })
        || !await this.isCurrent(candidate)) return null;
      return candidate;
    } catch (e) { if (isMissing(e) || e instanceof SyntaxError) return null; throw e; }
  }

  private async task(candidate: Pick<MissionCandidate, "namespace" | "taskName">): Promise<Task | null> {
    try { return await this.api.request<Task>("GET", taskPath(candidate)); }
    catch (e) { if (isMissing(e)) return null; throw e; }
  }
  private current(task: Task | null, candidate: MissionCandidate): task is Task {
    return !!task && supportsTaskExecution(task) && candidate.reviewedPhase === undefined
      && !task.metadata.deletionTimestamp && task.metadata.uid === candidate.taskUid
      && task.metadata.name === candidate.taskName && task.metadata.namespace === candidate.namespace
      && task.metadata.annotations?.[requested] === candidate.runNonce
      && task.metadata.annotations?.[completed] !== candidate.runNonce
      && task.spec.execution?.launch === true && missionObjective(task, candidate.runNonce) === candidate.content;
  }

  async isCurrent(candidate: MissionCandidate): Promise<boolean> {
    const task = await this.task(candidate);
    if (!this.current(task, candidate) || !task.metadata.resourceVersion
      || !Number.isSafeInteger(task.metadata.generation) || task.metadata.generation! < 1
      || task.status?.observedGeneration !== task.metadata.generation || task.status?.executionPhase !== "Running") return false;
    try {
      const cm = await this.api.request<ConfigMap>("GET", mapPath(candidate.namespace, `kars-mission-binding-${candidate.taskName}`));
      if (!owned(cm.metadata, candidate) || !cm.metadata.uid || !cm.metadata.resourceVersion
        || cm.metadata.namespace !== candidate.namespace
        || cm.metadata.name !== `kars-mission-binding-${candidate.taskName}` || !cm.data?.["binding.json"]) return false;
      const binding = parseBinding(JSON.parse(cm.data["binding.json"]));
      if (!binding || binding.taskName !== candidate.taskName || binding.taskUid !== candidate.taskUid
        || binding.sandboxUid !== candidate.sandboxUid || binding.podUid !== candidate.podUid
        || binding.agentDid !== candidate.agentDid || binding.dispatcherDid !== candidate.dispatcherDid
        || binding.sandboxName !== task.status.sandboxRef?.name) return false;
      const runtimeNamespace = segment(`kars-${binding.sandboxName}`);
      const paths = {
        sandbox: `/apis/kars.azure.com/v1alpha1/namespaces/${segment(candidate.namespace)}/karssandboxes/${segment(binding.sandboxName)}`,
        namespace: `/api/v1/namespaces/${runtimeNamespace}`,
        deployment: `/apis/apps/v1/namespaces/${runtimeNamespace}/deployments/${segment(binding.sandboxName)}`,
        replicaSet: `/apis/apps/v1/namespaces/${runtimeNamespace}/replicasets/${segment(binding.replicaSetName)}`,
        pod: `/api/v1/namespaces/${runtimeNamespace}/pods/${segment(binding.podName)}`,
      };
      const sandbox = await this.api.request<MissionWorkloadResources["sandbox"]>("GET", paths.sandbox);
      if (!owned(sandbox.metadata, candidate)) return false;
      const namespace = await this.api.request<MissionWorkloadResources["namespace"]>("GET", paths.namespace);
      const deployment = await this.api.request<MissionWorkloadResources["deployment"]>("GET", paths.deployment);
      const replicaSet = await this.api.request<MissionWorkloadResources["replicaSet"]>("GET", paths.replicaSet);
      const pod = await this.api.request<MissionWorkloadResources["pod"]>("GET", paths.pod);
      if (!currentMissionWorkload(binding, candidate.namespace, { sandbox, namespace, deployment, replicaSet, pod })) return false;
      // A bounded second collection catches changes during traversal, not changes after this check.
      // This is deliberately not a multi-resource transaction or an execution lease.
      const snapshots: Array<[string, { metadata: Metadata }]> = [
        [paths.pod, pod], [paths.replicaSet, replicaSet], [paths.deployment, deployment],
        [paths.namespace, namespace], [paths.sandbox, sandbox],
        [mapPath(candidate.namespace, cm.metadata.name), cm], [taskPath(candidate), task],
      ];
      for (const [path, prior] of snapshots) {
        const fresh = await this.api.request<{ metadata: Metadata }>("GET", path);
        if (fresh.metadata.deletionTimestamp || fresh.metadata.uid !== prior.metadata.uid
          || fresh.metadata.resourceVersion !== prior.metadata.resourceVersion) return false;
      }
      return await this.dispatchCurrent();
    } catch (e) { if (isMissing(e) || e instanceof SyntaxError) return false; throw e; }
  }

  private unpack(cm: ConfigMap, candidate: Pick<MissionCandidate, "namespace" | "taskName" | "taskUid" | "runNonce">): StoredMissionAttempt {
    if (!owned(cm.metadata, candidate) || !cm.metadata.resourceVersion || cm.metadata.namespace !== candidate.namespace
      || cm.metadata.name !== missionAttemptName(candidate)) throw new Error("Mission attempt ownership or revision mismatch");
    if (!cm.data?.["attempt.json"] || Buffer.byteLength(cm.data["attempt.json"]) > 900 * 1024) throw new Error("Mission attempt exceeds storage bounds");
    const attempt = JSON.parse(cm.data["attempt.json"]) as MissionAttempt;
    if (!attempt || attempt.version !== 1 || attempt.candidate?.taskUid !== candidate.taskUid || attempt.candidate?.runNonce !== candidate.runNonce
      || attempt.candidate?.taskName !== candidate.taskName || attempt.candidate?.namespace !== candidate.namespace
      || parseMissionMessage(attempt.assignment)?.type !== "mission:assign"
      || !sameMissionTarget(attempt.candidate, attempt.assignment)
      || !sameMissionContract(missionContract(attempt.candidate.reviewedPhase), attempt.assignment)
      || attempt.contentDigest !== missionContentDigest(attempt.candidate.content)
      || attempt.assignment.content !== attempt.candidate.content || !Array.isArray(attempt.events) || attempt.events.length > 128
      || !["dispatching", "accepted", "running", "succeeded", "failed", "rejected", "uncertain"].includes(attempt.phase)
      || typeof attempt.candidate.agentName !== "string" || !attempt.candidate.agentName.trim() || Buffer.byteLength(attempt.candidate.agentName) > 512
      || typeof attempt.ownerSession !== "string" || !attempt.ownerSession || attempt.ownerSession.length > 253
      || typeof attempt.startedAt !== "string" || typeof attempt.updatedAt !== "string"
      || !Number.isFinite(Date.parse(attempt.startedAt)) || !Number.isFinite(Date.parse(attempt.updatedAt))
      || Date.parse(attempt.updatedAt) < Date.parse(attempt.startedAt)
      || (attempt.error !== undefined && (typeof attempt.error !== "string" || attempt.error.length > 2048))
      || (attempt.phase === "dispatching" && (attempt.events.length !== 0 || attempt.reply !== undefined || attempt.error !== undefined))
      || (attempt.phase === "uncertain" && (!attempt.error || (attempt.reply && !["accepted", "running"].includes(attempt.reply.status))))
      || (attempt.reply && (parseMissionMessage(attempt.reply)?.type !== "mission:reply"
        || !sameMissionTarget(attempt.assignment, attempt.reply) || !sameMissionContract(attempt.assignment, attempt.reply)
        || attempt.reply.bootId !== attempt.assignment.bootId
        || attempt.reply.assignmentId !== attempt.assignment.assignmentId
        || (attempt.reply.artifactFormat !== undefined && attempt.reply.artifactFormat !== attempt.assignment.artifactFormat)))
      || (["accepted", "running", "succeeded", "failed", "rejected"].includes(attempt.phase) && attempt.reply?.status !== attempt.phase)) {
      throw new Error("Malformed durable mission attempt");
    }
    let lastTime = Date.parse(attempt.startedAt);
    let lastStatus: string | undefined;
    let lastEvidence: MissionReply["evidence"];
    for (const event of attempt.events) {
      if (!event || typeof event.at !== "string" || !Number.isFinite(Date.parse(event.at))
        || Date.parse(event.at) < lastTime || Date.parse(event.at) > Date.parse(attempt.updatedAt)
        || !["accepted", "running", "succeeded", "failed", "rejected"].includes(event.status)
        || (lastStatus === "running" && event.status === "accepted")
        || (["accepted", "running"].includes(lastStatus ?? "") && event.status === "rejected")
        || ["succeeded", "failed", "rejected"].includes(lastStatus ?? "")
        || (attempt.assignment.version === 2 && !missionEvidenceAdvances(lastEvidence, event.evidence))
        || !parseMissionMessage({ ...attempt.assignment, type: "mission:reply", status: event.status,
          evidence: event.evidence, ...(event.status === "succeeded" ? { output: attempt.reply?.output } : {}) })) {
        throw new Error("Malformed mission event journal");
      }
      lastTime = Date.parse(event.at);
      lastStatus = event.status;
      lastEvidence = event.evidence;
    }
    if ((attempt.reply?.status !== lastStatus) || !isDeepStrictEqual(attempt.reply?.evidence, attempt.events.at(-1)?.evidence)) {
      throw new Error("Mission reply does not match event journal");
    }
    return { revision: cm.metadata.resourceVersion, attempt };
  }
  async get(candidate: MissionCandidate): Promise<StoredMissionAttempt | null> {
    try {
      return this.unpack(await this.api.request<ConfigMap>("GET", mapPath(candidate.namespace, missionAttemptName(candidate))), candidate);
    } catch (e) { if (isMissing(e)) return null; throw e; }
  }
  private record(attempt: MissionAttempt, revision?: string): ConfigMap {
    const record: ConfigMap = { apiVersion: "v1", kind: "ConfigMap", metadata: {
      name: missionAttemptName(attempt.candidate), namespace: attempt.candidate.namespace,
      ...(revision ? { resourceVersion: revision } : {}), ownerReferences: [owner(attempt.candidate)],
      labels: { [`${prefix}mission-attempt`]: "true" },
    }, data: { "attempt.json": JSON.stringify(attempt) } };
    this.unpack({ ...record, metadata: { ...record.metadata, resourceVersion: revision ?? "validate" } }, attempt.candidate);
    return record;
  }
  async create(attempt: MissionAttempt): Promise<StoredMissionAttempt | null> {
    if (attempt.phase !== "dispatching" || attempt.startedAt !== attempt.updatedAt) throw new Error("A mission claim must start dispatching");
    try { return this.unpack(await this.api.request<ConfigMap>("POST", mapsPath(attempt.candidate.namespace), this.record(attempt)), attempt.candidate); }
    catch (e) { if (isConflict(e)) return null; throw e; }
  }
  async replace(previous: StoredMissionAttempt, attempt: MissionAttempt): Promise<StoredMissionAttempt> {
    if (previous.attempt.ownerSession !== attempt.ownerSession
      || !isDeepStrictEqual(previous.attempt.assignment, attempt.assignment)
      || !isDeepStrictEqual(previous.attempt.candidate, attempt.candidate) || previous.attempt.startedAt !== attempt.startedAt
      || Date.parse(attempt.updatedAt) < Date.parse(previous.attempt.updatedAt)
      || !isDeepStrictEqual(previous.attempt.events, attempt.events.slice(0, previous.attempt.events.length))
      || (previous.attempt.phase === "running" && attempt.phase === "accepted")
      || attempt.phase === "dispatching"
      || (attempt.phase === "uncertain" ? attempt.events.length !== previous.attempt.events.length
        || !isDeepStrictEqual(attempt.reply, previous.attempt.reply) : attempt.events.length !== previous.attempt.events.length + 1)
      || ["succeeded", "failed", "rejected", "uncertain"].includes(previous.attempt.phase)) throw new Error("Mission attempt is fenced or terminal");
    return this.unpack(await this.api.request<ConfigMap>("PUT", mapPath(attempt.candidate.namespace, missionAttemptName(attempt.candidate)),
      this.record(attempt, previous.revision)), attempt.candidate);
  }

  private async annotate(candidate: MissionCandidate, key: string): Promise<void> {
    const task = await this.task(candidate);
    if (!this.current(task, candidate) || !task.metadata.resourceVersion) throw new Error("Run changed before annotation");
    await this.api.request("PATCH", taskPath(candidate), {
      metadata: { uid: candidate.taskUid, resourceVersion: task.metadata.resourceVersion, annotations: { [key]: candidate.runNonce } },
    }, "application/merge-patch+json");
  }
  async acknowledge(candidate: MissionCandidate): Promise<void> { await this.annotate(candidate, `${prefix}run-ack`); }

  private async immutable(cm: ConfigMap): Promise<void> {
    try { await this.api.request("POST", mapsPath(cm.metadata.namespace!), { ...cm, immutable: true }); }
    catch (e) {
      if (!isConflict(e)) throw e;
      const prior = await this.api.request<ConfigMap>("GET", mapPath(cm.metadata.namespace!, cm.metadata.name));
      if (prior.immutable !== true || prior.metadata.deletionTimestamp
        || !isDeepStrictEqual(prior.data, cm.data)
        || !isDeepStrictEqual(prior.metadata.ownerReferences, cm.metadata.ownerReferences)
        || !isDeepStrictEqual(prior.metadata.annotations, cm.metadata.annotations)
        || !isDeepStrictEqual(prior.metadata.labels, cm.metadata.labels)) throw new Error("Immutable mission evidence collision");
    }
  }
  private async projection(candidate: MissionCandidate, cm: ConfigMap): Promise<void> {
    let prior: ConfigMap;
    try { prior = await this.api.request<ConfigMap>("GET", mapPath(candidate.namespace, cm.metadata.name)); }
    catch (e) {
      if (!isMissing(e)) throw e;
      if (!this.current(await this.task(candidate), candidate)) throw new Error("Run changed before evidence projection");
      await this.api.request("POST", mapsPath(candidate.namespace), cm);
      return;
    }
    if (!owned(prior.metadata, candidate) || !prior.metadata.resourceVersion) throw new Error("Mission projection ownership mismatch");
    // Read the projection revision before the Task: a newer publisher must either
    // change the Task seen here or invalidate the ConfigMap CAS below.
    if (!this.current(await this.task(candidate), candidate)) throw new Error("Run changed before evidence projection");
    await this.api.request("PUT", mapPath(candidate.namespace, cm.metadata.name), {
      ...cm, metadata: { ...cm.metadata, resourceVersion: prior.metadata.resourceVersion },
    });
  }

  async publish(attempt: MissionAttempt): Promise<boolean> {
    const { candidate, reply, assignment } = attempt;
    if (!reply || !["succeeded", "failed", "rejected"].includes(attempt.phase) || reply.status !== attempt.phase
      || parseMissionMessage(reply)?.type !== "mission:reply" || !sameMissionTarget(assignment, reply) || !sameMissionContract(assignment, reply)
      || assignment.assignmentId !== reply.assignmentId || assignment.bootId !== reply.bootId
      || (reply.artifactFormat !== undefined && reply.artifactFormat !== assignment.artifactFormat)) throw new Error("Mission has no valid terminal reply");
    const durable = await this.get(candidate);
    if (!durable || !isDeepStrictEqual(durable.attempt, attempt)) throw new Error("Terminal reply must be persisted before publication");
    const key = missionAttemptName(candidate).replace("kars-mission-attempt-", "");
    const metadata = (name: string, role: "canonical" | "current", label: string): Metadata => ({
      name, namespace: candidate.namespace, ownerReferences: [owner(candidate)],
      labels: { [`${prefix}${label}`]: key },
      annotations: { [`${prefix}mission-evidence-key`]: key, [`${prefix}mission-evidence-role`]: role,
        [`${prefix}mission-principal-name`]: candidate.taskName, [`${prefix}mission-task-uid`]: candidate.taskUid,
        [`${prefix}mission-run-nonce`]: candidate.runNonce },
    });
    const success = reply.status === "succeeded";
    const artifacts: Record<string, string> = success ? { "response.md": reply.output!, ...reply.artifacts } : {};
    const data: Record<string, string> = {
      taskName: candidate.taskName, taskUid: candidate.taskUid, assignmentNonce: candidate.runNonce,
      assignmentId: assignment.assignmentId, agentName: candidate.agentName, agentDid: candidate.agentDid,
      dispatcherDid: candidate.dispatcherDid, sandboxUid: candidate.sandboxUid, podUid: candidate.podUid,
      runtimeBootId: assignment.bootId, status: success ? "ok" : reply.status, output: success ? reply.output! : "",
      error: reply.error ?? "", startedAt: attempt.startedAt, finishedAt: attempt.updatedAt,
      model: reply.evidence?.model ?? "",
      ...(reply.evidence?.usage ? { totalTokens: String(reply.evidence.usage.totalTokens) } : {}),
      usageKnown: String(reply.evidence?.usage !== null && reply.evidence?.usage !== undefined),
      artifactCount: String(Object.keys(artifacts).length), "evidence.json": JSON.stringify(reply),
    };
    await this.immutable({ apiVersion: "v1", kind: "ConfigMap", metadata: metadata(`kars-mission-output-${key}`, "canonical", "mission-output"), data });
    await this.immutable({ apiVersion: "v1", kind: "ConfigMap", metadata: metadata(`kars-mission-artifacts-${key}`, "canonical", "mission-artifacts"), data: artifacts });
    const task = await this.task(candidate);
    if (task?.metadata.uid === candidate.taskUid && !task.metadata.deletionTimestamp
      && task.metadata.name === candidate.taskName && task.metadata.namespace === candidate.namespace
      && task.metadata.annotations?.[requested] === candidate.runNonce && task.metadata.annotations?.[completed] === candidate.runNonce) return true;
    if (!this.current(task, candidate)) return false;
    await this.projection(candidate, { apiVersion: "v1", kind: "ConfigMap", metadata: metadata(`kars-mission-artifacts-${candidate.taskName}`, "current", "mission-artifacts"), data: artifacts });
    await this.projection(candidate, { apiVersion: "v1", kind: "ConfigMap", metadata: metadata(`kars-mission-output-${candidate.taskName}`, "current", "mission-output"), data });
    await this.annotate(candidate, completed);
    return true;
  }
}
