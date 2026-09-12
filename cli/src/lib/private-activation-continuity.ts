// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes } from "node:crypto";
import {
  annotations, at, canonical, consumesPrivateAuthority, digest, kinds, patchNamespace,
  PRIVATE_PREFIX, read, record, reviewed, reviewedOwner, template, templateDigest,
  validateActivationShape, validateQualifiedMetadata,
  type Execute, type Json, type NamespaceReview, type PrivateActivation,
} from "./private-activation.js";
import {
  replicaIntent, retirementBinding, retirementReview, retirementState, type RootRetirement,
} from "./private-activation-retirement.js";
import { reviewLateScope, stageLateScope } from "./private-activation-late-scope.js";

const RETIREMENT = "kars.azure.com/private-root-retirement";
// Unlike other private annotations, this existing field is operator-only,
// even for the root projector. Both completion and scope recovery belong here.
const ROOT = RETIREMENT;
const SCOPE = RETIREMENT;
const failure = "Shared private qualification changed or is incomplete; existing authority was preserved";
interface RootQualification {
  version: 2;
  retirement: string;
  retirementDigest: string;
  activation: PrivateActivation;
}
interface ScopeQualification {
  version: 3;
  root: string;
  binding: string;
  phase: "Pending" | "Stamping" | "Qualified";
  epoch?: string;
}
export interface PrivateContinuity {
  proof: RootQualification;
  state: RootRetirement;
  sealed: boolean;
}

function encoded(value: unknown): string {
  const text = canonical(value);
  if (Buffer.byteLength(text) > 131_072) throw new Error("Private qualification metadata exceeds its bounded receipt size");
  return text;
}

function decode(value: unknown, keys: string[]): Record<string, Json> {
  if (typeof value !== "string" || Buffer.byteLength(value) > 131_072) throw new Error(failure);
  const object = record(JSON.parse(value));
  if (Object.keys(object).some(key => !keys.includes(key))) throw new Error(failure);
  return object;
}

function scopeBinding(scope: NamespaceReview): string {
  return digest({
    namespace: { name: scope.namespace.name, uid: scope.namespace.uid },
    consumers: scope.consumers.map(consumer => ({
      kind: consumer.kind, name: consumer.object.name, uid: consumer.object.uid, templateDigest: consumer.templateDigest,
    })).sort((a, b) => canonical(a).localeCompare(canonical(b))),
  });
}

function rootProof(namespace: unknown): RootQualification | undefined {
  const raw = at(namespace, "metadata", "annotations", ROOT);
  if (raw === undefined) return undefined;
  if (record(JSON.parse(String(raw))).version === 1) return undefined;
  const proof = decode(raw, ["version", "retirement", "retirementDigest", "activation"]);
  if (proof.version !== 2 || typeof proof.retirement !== "string"
    || typeof proof.retirementDigest !== "string" || !/^[a-f0-9]{64}$/.test(proof.retirementDigest)) {
    throw new Error(failure);
  }
  const activation = proof.activation as unknown as PrivateActivation;
  validateActivationShape(activation, "qualified");
  return { version: 2, retirement: proof.retirement, retirementDigest: proof.retirementDigest, activation };
}

function rootReady(deployment: unknown, state: RootRetirement): boolean {
  return replicaIntent(deployment) === state.replicaIntent && (state.replicaIntent === 0
    || (at(deployment, "metadata", "generation") !== undefined
      && at(deployment, "status", "observedGeneration") === at(deployment, "metadata", "generation")
      && at(deployment, "status", "updatedReplicas") === state.replicaIntent
      && at(deployment, "status", "availableReplicas") === state.replicaIntent));
}

async function pods(execute: Execute, scope: NamespaceReview): Promise<Json[]> {
  const inventory = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
  if (at(inventory, "metadata", "continue") || !Array.isArray(inventory.items)) throw new Error("Private continuity inventory is incomplete");
  for (const pod of inventory.items) reviewed(pod, true);
  return inventory.items;
}

async function consumers(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, captured: string[] = [], qualified = true, stamping = false,
): Promise<void> {
  for (const consumer of scope.consumers) {
    if (!kinds[consumer.kind]) throw new Error(failure);
    const current = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
    if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) throw new Error(failure);
    if (consumesPrivateAuthority(current, scope.namespace.name, activation)) {
      const epoch = at(template(current), "metadata", "annotations", `${PRIVATE_PREFIX}epoch`);
      if (!qualified && (["Pod", "Job"].includes(consumer.kind) || epoch !== undefined)) {
        throw new Error("Additional Pod/Job or previously marked template requires explicit private recovery; shared root was preserved");
      }
      if (qualified && epoch !== scope.epoch && !(stamping && epoch === undefined)) throw new Error(failure);
    }
  }
  for (const pod of await pods(execute, scope)) {
    if (captured.includes(reviewed(pod, true).uid)) throw new Error("Captured old private consumer UID remains; retirement recovery was preserved");
    if (!consumesPrivateAuthority(pod, scope.namespace.name, activation)) continue;
    if (!qualified) {
      throw new Error("Additional scope still consumes private authority; owner-specific retirement/rotation is required without restarting the shared root");
    }
    if (at(pod, "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== scope.epoch
      || !await reviewedOwner(execute, pod, scope)) {
      throw new Error("Unreviewed or stale-epoch private consumer in shared qualification; existing authority was preserved");
    }
  }
}

async function qualifiedScope(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, captured: string[] = [], stamping = false,
): Promise<void> {
  const current = await read(execute, "namespace", scope.namespace.name);
  const fields = record(at(current, "metadata", "annotations"));
  if (!scope.epoch || !/^[a-f0-9]{64}$/.test(scope.epoch)
    || reviewed(current).uid !== scope.namespace.uid || fields[`${PRIVATE_PREFIX}epoch`] !== scope.epoch
    || Object.entries(annotations(activation, scope, "Qualified")).some(([key, value]) => fields[key] !== value)) throw new Error(failure);
  const parents = Object.entries(fields).filter(([key, value]) => key.startsWith(`${PRIVATE_PREFIX}parent-`) && value === scope.epoch)
    .map(([key]) => key.slice(`${PRIVATE_PREFIX}parent-`.length)).sort();
  if (canonical(parents) !== canonical(scope.consumers.map(consumer => consumer.object.uid).sort())) throw new Error(failure);
  if (activation.root.budgetTls && [
    ["budget-qualified-bundle", activation.bundleRevision],
    ["budget-qualified-key", activation.root.budgetTls.keyDigest],
    ["budget-qualified-secret", activation.root.budgetTls.secret.uid],
    ["budget-rotation-bundle", ""], ["budget-before-key", ""],
  ].some(([key, value]) => fields[`${PRIVATE_PREFIX}${key}`] !== value)) throw new Error(failure);
  await consumers(execute, activation, scope, captured, true, stamping);
}

async function verifyProof(execute: Execute, proof: RootQualification, state: RootRetirement, ready: boolean): Promise<void> {
  const activation = proof.activation;
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const deployment = await read(execute, "deployment", activation.root.deployment.name, activation.root.namespace.name);
  const current = retirementReview(activation, namespace, deployment);
  if (!current || state.phase !== "restoring" || canonical(current) !== canonical(state)
    || proof.retirement !== canonical(state) || proof.retirementDigest !== digest(state) || retirementBinding(activation) !== state.binding
    || (ready && !rootReady(deployment, state))) throw new Error(failure);
  const budget = activation.root.budgetTls;
  if (budget && (!state.baseline || state.exposedKeys.includes(budget.keyDigest)
    || state.baseline.resourceVersion === budget.secret.resourceVersion)) throw new Error(failure);
  await validateQualifiedMetadata(execute, activation);
  for (const scope of activation.namespaces) await qualifiedScope(execute, activation, scope, state.captured[scope.namespace.uid]);
}

async function legacyProof(execute: Execute, activation: PrivateActivation, state: RootRetirement): Promise<RootQualification> {
  const inventory = record(JSON.parse(await execute([
    "get", "karscredentialgrants.kars.azure.com", "--all-namespaces", "--chunk-size=0", "-o", "json",
  ])));
  if (at(inventory, "metadata", "continue") || !Array.isArray(inventory.items)) throw new Error("Legacy qualification grant inventory is incomplete");
  for (const grant of inventory.items) {
    const candidate = at(grant, "spec", "privateActivation") as unknown as PrivateActivation | undefined;
    if (!candidate || candidate.phase !== "qualified") continue;
    validateActivationShape(candidate, "qualified");
    if (retirementBinding(candidate) !== state.binding) continue;
    const namespace = at(grant, "metadata", "namespace");
    if (typeof namespace !== "string" || reviewed(grant).name !== "workspace") throw new Error(failure);
    const fresh = await read(execute, "karscredentialgrants.kars.azure.com", "workspace", namespace);
    if (reviewed(fresh).uid !== reviewed(grant).uid || canonical(at(fresh, "spec")) !== canonical(at(grant, "spec"))) throw new Error(failure);
    return { version: 2, retirement: canonical(state), retirementDigest: digest(state), activation: candidate };
  }
  if (retirementBinding(activation) === state.binding) {
    const candidate = structuredClone(activation);
    candidate.phase = "qualified";
    for (const scope of candidate.namespaces) {
      const namespace = await read(execute, "namespace", scope.namespace.name);
      const epoch = at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}epoch`);
      if (typeof epoch !== "string" || (scope.epoch !== undefined && scope.epoch !== epoch)) throw new Error(failure);
      scope.epoch = epoch;
    }
    return { version: 2, retirement: canonical(state), retirementDigest: digest(state), activation: candidate };
  }
  throw new Error("Legacy restoring retirement requires its original exact review or stored qualified grant for verified migration; no shared scope was changed");
}

export async function reviewPrivateContinuity(
  execute: Execute, activation: PrivateActivation, recoverIntent = false,
): Promise<PrivateContinuity | undefined> {
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const deployment = await read(execute, "deployment", activation.root.deployment.name, activation.root.namespace.name);
  const state = retirementState(namespace);
  let proof = rootProof(namespace);
  if (!proof && state?.phase !== "restoring") {
    if (!state && (at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}state`) === "Qualified"
      || at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== undefined)) {
      throw new Error("Qualified root lacks its original retirement history; explicit operator recovery is required");
    }
    retirementReview(activation, namespace, deployment, recoverIntent);
    return undefined;
  }
  if (!state) throw new Error(failure);
  const sealed = Boolean(proof);
  if (!sealed && recoverIntent) activation.root.replicaIntent = state.replicaIntent;
  proof ??= await legacyProof(execute, activation, state);
  // The original full namespace/consumer binding remains intact and verified.
  // Substitution here compares only the incoming root to that verified history.
  if (retirementBinding({ ...activation, namespaces: proof.activation.namespaces }) !== state.binding) throw new Error(failure);
  await verifyProof(execute, proof, state, sealed);
  if (!sealed && !rootReady(deployment, state) && retirementBinding(activation) !== state.binding) {
    throw new Error("Original private root restore is incomplete; resume its exact review before adding another workspace");
  }
  const continuity = { proof, state, sealed };
  for (const scope of activation.namespaces) {
    const plan = await scopePlan(execute, activation, scope, continuity);
    if (recoverIntent && plan === "Late") {
      console.error(`Private enrollment of ${scope.namespace.name} requires reviewed runtime suspension, retirement of all old Pod UIDs, `
        + "controller admin-key rotation and restoration of the original suspension/replica intent. "
        + "Task, Sandbox, namespace and stored customer data are retained; Pod-local ephemeral state is restarted. Shared root and other grants are not reset.");
    }
  }
  return continuity;
}

async function scopePlan(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, continuity: PrivateContinuity,
): Promise<"Qualified" | "Stamping" | "Pending" | "New" | "Late"> {
  const original = continuity.proof.activation.namespaces.find(value => value.namespace.name === scope.namespace.name);
  if (original) {
    if (scopeBinding(scope) !== scopeBinding(original) || (scope.epoch !== undefined && scope.epoch !== original.epoch)) throw new Error(failure);
    scope.epoch = original.epoch;
    await qualifiedScope(execute, activation, scope, continuity.state.captured[scope.namespace.uid]);
    return "Qualified";
  }
  const namespace = await read(execute, "namespace", scope.namespace.name);
  if (reviewed(namespace).uid !== scope.namespace.uid) throw new Error(failure);
  const raw = at(namespace, "metadata", "annotations", SCOPE);
  if (raw !== undefined && record(JSON.parse(String(raw))).version === 4) {
    const plan = await reviewLateScope(execute, activation, scope, digest(continuity.proof));
    if (!plan) throw new Error(failure);
    if (plan === "Qualified") await qualifiedScope(execute, activation, scope);
    return plan;
  }
  if (raw === undefined) {
    if (scope.epoch !== undefined || Object.keys(record(at(namespace, "metadata", "annotations") ?? {}))
      .some(key => key.startsWith(PRIVATE_PREFIX))) {
      throw new Error("Additional namespace has unproven private lifecycle state; preserve it for explicit recovery");
    }
    const late = await reviewLateScope(execute, activation, scope, digest(continuity.proof));
    if (late) return late;
    await consumers(execute, activation, scope, [], false);
    return "New";
  }
  const receipt = decode(raw, ["version", "root", "binding", "phase", "epoch"]);
  if (receipt.version !== 3 || receipt.root !== digest(continuity.proof) || receipt.binding !== scopeBinding(scope)) throw new Error(failure);
  if (receipt.phase === "Qualified" || receipt.phase === "Stamping") {
    if (typeof receipt.epoch !== "string" || (scope.epoch !== undefined && scope.epoch !== receipt.epoch)) throw new Error(failure);
    scope.epoch = receipt.epoch;
    await qualifiedScope(execute, activation, scope, [], receipt.phase === "Stamping");
    return receipt.phase;
  }
  if (receipt.phase !== "Pending" || receipt.epoch !== undefined || scope.epoch !== undefined
    || at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== undefined
    || Object.entries(annotations(activation, scope, "Pending")).some(([key, value]) =>
      at(namespace, "metadata", "annotations", key) !== value)) throw new Error(failure);
  await consumers(execute, activation, scope, [], false);
  return "Pending";
}

async function seal(execute: Execute, continuity: PrivateContinuity): Promise<void> {
  const activation = continuity.proof.activation;
  const namespace = reviewed(await read(execute, "namespace", activation.root.namespace.name));
  await verifyProof(execute, continuity.proof, continuity.state, true);
  const scope = activation.namespaces.find(value => value.namespace.name === activation.root.namespace.name);
  if (!scope) throw new Error(failure);
  await patchNamespace(execute, { ...structuredClone(scope), namespace }, { [ROOT]: encoded(continuity.proof) }, {
    [RETIREMENT]: canonical(continuity.state),
  }, true);
  continuity.sealed = true;
}

export async function completePrivateQualification(
  execute: Execute, activation: PrivateActivation, state: RootRetirement,
): Promise<void> {
  await seal(execute, { proof: { version: 2, retirement: canonical(state), retirementDigest: digest(state),
    activation: structuredClone(activation) }, state, sealed: false });
}

async function assertSealed(execute: Execute, continuity: PrivateContinuity): Promise<void> {
  const namespace = await read(execute, "namespace", continuity.proof.activation.root.namespace.name);
  if (at(namespace, "metadata", "annotations", ROOT) !== encoded(continuity.proof)) throw new Error(failure);
  await verifyProof(execute, continuity.proof, continuity.state, true);
}

export async function stageSharedActivation(
  execute: Execute, activation: PrivateActivation, continuity: PrivateContinuity,
): Promise<PrivateActivation> {
  if (!continuity.sealed) {
    await verifyProof(execute, continuity.proof, continuity.state, false);
    const root = continuity.proof.activation.root;
    let deployment = await read(execute, "deployment", root.deployment.name, root.namespace.name);
    if (continuity.state.pauseRoot && replicaIntent(deployment) === 0 && continuity.state.replicaIntent > 0) {
      await execute(["patch", "deployment", root.deployment.name, "-n", root.namespace.name, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: root.deployment.uid, resourceVersion: reviewed(deployment).resourceVersion },
          spec: { replicas: continuity.state.replicaIntent } })]);
      deployment = await read(execute, "deployment", root.deployment.name, root.namespace.name);
    }
    const deadline = Date.now() + 120_000;
    while (!rootReady(deployment, continuity.state)) {
      if (Date.now() >= deadline) throw new Error("Original root restore remains incomplete; its epochs and retirement history were preserved");
      await new Promise(resolve => setTimeout(resolve, 500));
      await verifyProof(execute, continuity.proof, continuity.state, false);
      deployment = await read(execute, "deployment", root.deployment.name, root.namespace.name);
    }
    await seal(execute, continuity);
  }
  for (const scope of activation.namespaces) {
    const plan = await scopePlan(execute, activation, scope, continuity);
    if (plan === "Qualified") continue;
    await assertSealed(execute, continuity);
    if (plan === "Late") {
      await stageLateScope(execute, activation, scope, digest(continuity.proof), () => assertSealed(execute, continuity));
      await qualifiedScope(execute, activation, scope);
      continue;
    }
    const receipt: ScopeQualification = { version: 3, root: digest(continuity.proof), binding: scopeBinding(scope), phase: "Pending" };
    if (plan === "New") {
      await patchNamespace(execute, scope, { ...annotations(activation, scope, "Pending"), [SCOPE]: encoded(receipt) },
        { [SCOPE]: undefined, [`${PRIVATE_PREFIX}enabled`]: undefined, [`${PRIVATE_PREFIX}epoch`]: undefined }, true);
    }
    if (plan !== "Stamping") {
      await consumers(execute, activation, scope, [], false);
      await assertSealed(execute, continuity);
      scope.epoch = randomBytes(32).toString("hex");
      const budget = activation.root.budgetTls;
      await patchNamespace(execute, scope, {
        ...annotations(activation, scope, "Qualified"), [`${PRIVATE_PREFIX}epoch`]: scope.epoch,
        [SCOPE]: encoded({ ...receipt, phase: "Stamping", epoch: scope.epoch }),
        ...Object.fromEntries(scope.consumers.map(consumer => [`${PRIVATE_PREFIX}parent-${consumer.object.uid}`, scope.epoch!])),
        ...(budget ? {
          [`${PRIVATE_PREFIX}budget-qualified-bundle`]: activation.bundleRevision,
          [`${PRIVATE_PREFIX}budget-qualified-key`]: budget.keyDigest,
          [`${PRIVATE_PREFIX}budget-qualified-secret`]: budget.secret.uid,
          [`${PRIVATE_PREFIX}budget-rotation-bundle`]: "", [`${PRIVATE_PREFIX}budget-before-key`]: "",
        } : {}),
      }, { [SCOPE]: encoded(receipt), [`${PRIVATE_PREFIX}state`]: "Pending", [`${PRIVATE_PREFIX}epoch`]: undefined }, true);
    }
    for (const consumer of scope.consumers) {
      const current = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
      if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) throw new Error(failure);
      if (!consumesPrivateAuthority(current, scope.namespace.name, activation)) continue;
      if (["Pod", "Job"].includes(consumer.kind)) throw new Error("Additional Pod/Job requires owner-specific private qualification; shared root was preserved");
      if (at(template(current), "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) === scope.epoch) continue;
      const marker = { metadata: { annotations: { [`${PRIVATE_PREFIX}epoch`]: scope.epoch } } };
      const spec = consumer.kind === "CronJob" ? { jobTemplate: { spec: { template: marker } } } : { template: marker };
      await execute(["patch", kinds[consumer.kind]!, consumer.object.name, "-n", scope.namespace.name, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: consumer.object.uid, resourceVersion: reviewed(current).resourceVersion }, spec })]);
    }
    await qualifiedScope(execute, activation, scope);
    await assertSealed(execute, continuity);
    await patchNamespace(execute, scope, { [SCOPE]: encoded({ ...receipt, phase: "Qualified", epoch: scope.epoch }) },
      { [SCOPE]: encoded({ ...receipt, phase: "Stamping", epoch: scope.epoch }), [`${PRIVATE_PREFIX}epoch`]: scope.epoch }, true);
  }
  activation.phase = "qualified";
  await validateQualifiedMetadata(execute, activation);
  await verifySharedPublication(execute, activation);
  return activation;
}

export async function verifySharedPublication(execute: Execute, activation: PrivateActivation): Promise<void> {
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const proof = rootProof(namespace);
  const state = retirementState(namespace);
  if (!proof || !state || retirementBinding({ ...activation, namespaces: proof.activation.namespaces }) !== state.binding) throw new Error(failure);
  const continuity = { proof, state, sealed: true };
  await assertSealed(execute, continuity);
  for (const scope of activation.namespaces) {
    if (await scopePlan(execute, activation, scope, continuity) !== "Qualified") throw new Error(failure);
  }
}
