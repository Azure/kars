// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes } from "node:crypto";
import {
  at, canonical, consumesPrivateAuthority, digest, patchNamespace, PRIVATE_PREFIX,
  read, record, reviewed, templateDigest,
  type Execute, type NamespaceReview, type PrivateActivation,
} from "./private-activation.js";

const FIELD = "kars.azure.com/private-root-retirement";
const failure = "Private root retirement identity, intent, or attempt changed; preserve protection and re-review";
interface Baseline { secretUid: string; resourceVersion: string; keyDigest: string }
export interface RootRetirement {
  version: 1;
  attempt: string;
  binding: string;
  replicaIntent: number;
  originalVersion: string;
  pauseRoot: boolean;
  phase: "pausing" | "retired" | "restoring";
  captured: Record<string, string[]>;
  baseline?: Baseline;
}

export function replicaIntent(deployment: unknown): number {
  const value = at(deployment, "spec", "replicas") ?? 1;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > 2_147_483_647) {
    throw new Error("Reviewed root replica intent is invalid");
  }
  return value;
}

function binding(activation: PrivateActivation): string {
  const identity = ({ name, uid }: { name: string; uid: string }) => ({ name, uid });
  const root = activation.root;
  return digest({
    contract: activation.contract, bundleRevision: activation.bundleRevision,
    profile: activation.profile, controllerUids: activation.controllerUids,
    root: { namespace: identity(root.namespace), account: identity(root.account),
      deployment: identity(root.deployment), templateDigest: root.templateDigest, replicaIntent: root.replicaIntent,
      budget: root.budgetTls ? { namespace: identity(root.budgetTls.namespace), secret: identity(root.budgetTls.secret) } : null },
    namespaces: activation.namespaces.map(scope => ({
      namespace: identity(scope.namespace),
      consumers: scope.consumers.map(c => ({ kind: c.kind, object: identity(c.object), templateDigest: c.templateDigest }))
        .sort((a, b) => canonical(a).localeCompare(canonical(b))),
    })).sort((a, b) => a.namespace.name.localeCompare(b.namespace.name)),
  });
}

function decode(namespace: unknown): RootRetirement | undefined {
  const raw = at(namespace, "metadata", "annotations", FIELD);
  if (raw === undefined) return undefined;
  if (typeof raw !== "string") throw new Error(failure);
  let value: unknown;
  try { value = JSON.parse(raw); } catch { throw new Error(failure); }
  const state = record(value);
  const hex = (v: unknown): v is string => typeof v === "string" && /^[a-f0-9]{64}$/.test(v);
  const text = (v: unknown): v is string => typeof v === "string" && v.length > 0 && v.length <= 253;
  if (Object.keys(state).some(k => !["version", "attempt", "binding", "replicaIntent", "originalVersion",
    "pauseRoot", "phase", "captured", "baseline"].includes(k))
    || state.version !== 1 || !hex(state.attempt) || !hex(state.binding)
    || !text(state.originalVersion) || typeof state.pauseRoot !== "boolean"
    || (state.phase !== "pausing" && state.phase !== "retired" && state.phase !== "restoring")
    || typeof state.replicaIntent !== "number" || !Number.isInteger(state.replicaIntent)
    || state.replicaIntent < 0 || state.replicaIntent > 2_147_483_647) throw new Error(failure);
  const captured: Record<string, string[]> = {};
  for (const [ns, ids] of Object.entries(record(state.captured))) {
    if (!text(ns) || !Array.isArray(ids) || !ids.every(text)) throw new Error(failure);
    captured[ns] = ids;
  }
  let baseline: Baseline | undefined;
  if (state.baseline !== undefined) {
    const value = record(state.baseline);
    if (Object.keys(value).sort().join(",") !== "keyDigest,resourceVersion,secretUid"
      || !hex(value.keyDigest) || !text(value.resourceVersion) || !text(value.secretUid)
      || state.phase === "pausing") throw new Error(failure);
    baseline = { keyDigest: value.keyDigest, resourceVersion: value.resourceVersion, secretUid: value.secretUid };
  }
  return { version: state.version, attempt: state.attempt, binding: state.binding, replicaIntent: state.replicaIntent,
    originalVersion: state.originalVersion, pauseRoot: state.pauseRoot, phase: state.phase, captured,
    ...(baseline ? { baseline } : {}) };
}

export function retirementReview(
  activation: PrivateActivation, namespace: unknown, deployment: unknown, recoverIntent = false,
): RootRetirement | undefined {
  const state = decode(namespace);
  if (reviewed(namespace).uid !== activation.root.namespace.uid
    || reviewed(deployment).uid !== activation.root.deployment.uid
    || templateDigest(deployment) !== activation.root.templateDigest) throw new Error(failure);
  if (!state) {
    if (at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}state`) === "Pending") {
      throw new Error("Pending private activation lacks its original retirement intent; explicit operator qualification is required");
    }
    if (activation.root.replicaIntent !== replicaIntent(deployment)) throw new Error(failure);
    return undefined;
  }
  if (recoverIntent) activation.root.replicaIntent = state.replicaIntent;
  if (binding(activation) !== state.binding || activation.root.replicaIntent !== state.replicaIntent
    || state.pauseRoot !== consumesPrivateAuthority(deployment, activation.root.namespace.name, activation)) throw new Error(failure);
  const replicas = replicaIntent(deployment);
  const paused = state.pauseRoot ? 0 : state.replicaIntent;
  if (state.phase === "retired" ? replicas !== paused : replicas !== paused && replicas !== state.replicaIntent) {
    throw new Error(failure);
  }
  if (Object.keys(state.captured).some(uid => !activation.namespaces.some(scope => scope.namespace.uid === uid))
    || (state.phase !== "pausing" && Boolean(state.baseline) !== Boolean(activation.root.budgetTls))
    || (state.baseline && state.baseline.secretUid !== activation.root.budgetTls?.secret.uid)) throw new Error(failure);
  return state;
}

export function startRetirement(
  activation: PrivateActivation, deployment: unknown, previous: RootRetirement | undefined,
): RootRetirement {
  if (previous && previous.phase !== "restoring") return structuredClone(previous);
  return { version: 1, attempt: randomBytes(32).toString("hex"), binding: binding(activation),
    replicaIntent: activation.root.replicaIntent, originalVersion: reviewed(deployment).resourceVersion,
    pauseRoot: consumesPrivateAuthority(deployment, activation.root.namespace.name, activation),
    phase: "pausing", captured: previous?.captured ?? {} };
}

export async function saveRetirement(
  execute: Execute, scope: NamespaceReview, previous: RootRetirement | undefined, next: RootRetirement,
  fields: Record<string, string> = {},
): Promise<void> {
  await patchNamespace(execute, scope, { ...fields, [FIELD]: canonical(next) },
    { [FIELD]: previous ? canonical(previous) : undefined });
}

export async function assertRetiredRoot(execute: Execute, activation: PrivateActivation, state: RootRetirement): Promise<void> {
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const root = await read(execute, "deployment", activation.root.deployment.name, activation.root.namespace.name);
  const current = retirementReview(activation, namespace, root);
  const account = await read(execute, "serviceaccount", activation.root.account.name, activation.root.namespace.name);
  if (!current || canonical(current) !== canonical(state) || reviewed(account).uid !== activation.root.account.uid
    || replicaIntent(root) !== (state.pauseRoot ? 0 : state.replicaIntent)) throw new Error(failure);
  for (const scope of activation.namespaces) {
    if (reviewed(await read(execute, "namespace", scope.namespace.name)).uid !== scope.namespace.uid) throw new Error(failure);
    const inventory = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
    if (at(inventory, "metadata", "continue") || !Array.isArray(inventory.items)) throw new Error("Private retirement inventory is incomplete");
    if (inventory.items.some(pod => state.captured[scope.namespace.uid]?.includes(reviewed(pod, true).uid)
      || consumesPrivateAuthority(pod, scope.namespace.name, activation))) throw new Error("Private authority remains after retirement; no fresh key was requested or accepted");
  }
}

export function capturedRetirement(state: RootRetirement, scopes: NamespaceReview[]): Map<string, Set<string>> {
  return new Map(scopes.map(scope => [scope.namespace.name, new Set(state.captured[scope.namespace.uid] ?? [])]));
}

export async function qualifyRetiredBudget(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, state: RootRetirement,
): Promise<RootRetirement> {
  await assertRetiredRoot(execute, activation, state);
  const budget = activation.root.budgetTls;
  if (state.phase === "pausing") {
    const next: RootRetirement = { ...state, phase: "retired", ...(budget ? {
      baseline: { secretUid: budget.secret.uid, resourceVersion: budget.secret.resourceVersion, keyDigest: budget.keyDigest },
    } : {}) };
    await saveRetirement(execute, scope, state, next);
    if (budget) throw new Error("Retired root requires budget TLS operator rotation and public-CA update; keep it paused and re-preview afterwards");
    return next;
  }
  if (budget && (!state.baseline || state.baseline.secretUid !== budget.secret.uid
    || state.baseline.keyDigest === budget.keyDigest || state.baseline.resourceVersion === budget.secret.resourceVersion)) {
    throw new Error("Budget TLS public key is unchanged since verified root retirement; copying or pre-retirement rotation cannot qualify");
  }
  return state;
}
