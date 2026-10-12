// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  annotations, at, canonical, PRIVATE_PREFIX, read, record, reviewed, reviewBudgetTls,
  templateDigest, validateActivationShape, validatePrivateActivation, verifyPrivateBundle,
  requiresPrivatePodReview, validateObservedControllerProfile,
  type Execute, type PrivateActivation,
} from "./private-activation.js";
import {
  assertRetiredRoot, retirementBinding, retirementReview, retirementState, type RootRetirement,
} from "./private-activation-retirement.js";

const FIELD = "kars.azure.com/private-root-retirement";
const CHECKSUM = "checksum/inference-budget-tls";
const failure = "Budget TLS recovery requires the same paused retired root and only its managed TLS checksum change";

export interface BudgetTlsRecovery {
  version: 1;
  activation: PrivateActivation;
  retirement: RootRetirement;
  checksum: string;
}

function validateScope(activation: PrivateActivation): void {
  validateActivationShape(activation, "reviewed");
  const scope = activation.namespaces[0];
  if (!activation.root.budgetTls || activation.namespaces.length !== 1 || !scope
    || scope.namespace.uid !== activation.root.namespace.uid || scope.consumers.length !== 1
    || scope.consumers[0]?.kind !== "Deployment"
    || scope.consumers[0]?.object.uid !== activation.root.deployment.uid) throw new Error(failure);
}

async function assertOperator(execute: Execute): Promise<void> {
  if ((await execute(["auth", "can-i", "manage", "karscredentialgrants.kars.azure.com/workspace",
    "--all-namespaces"])).trim() !== "yes") throw new Error("Cluster-scoped credential operator authority is required");
}

async function assertNoAuthority(execute: Execute, activation: PrivateActivation, state: RootRetirement): Promise<void> {
  for (const [kind, scope] of [
    ["karscredentialgrants.kars.azure.com", ["--all-namespaces"]],
    ["pods", ["-n", activation.root.namespace.name]],
  ] as const) {
    const inventory = JSON.parse(await execute(["get", kind, ...scope, "--chunk-size=0", "-o", "json"]));
    const items = at(inventory, "items");
    if (at(inventory, "metadata", "continue") || !Array.isArray(items)) throw new Error("Budget TLS recovery inventory is incomplete");
    for (const item of items) {
      if (kind === "pods") {
        if (state.captured[activation.root.namespace.uid]?.includes(reviewed(item, true).uid)
          || await requiresPrivatePodReview(execute, item, activation.namespaces[0]!, activation)) throw new Error(failure);
      } else {
        const writers = at(item, "spec", "writers");
        if (!Array.isArray(writers) || writers.length || at(item, "spec", "privateActivation") != null) {
          throw new Error("Budget TLS recovery refuses existing private grants or writer authority");
        }
      }
    }
  }
}

function assertPending(namespace: unknown, activation: PrivateActivation, state: RootRetirement): void {
  const expected = { ...annotations(activation, activation.namespaces[0]!, "Pending"), [FIELD]: canonical(state) };
  const actual = Object.fromEntries(Object.entries(record(at(namespace, "metadata", "annotations") ?? {}))
    .filter(([key]) => key.startsWith(PRIVATE_PREFIX)));
  if (canonical(actual) !== canonical(expected)) throw new Error(failure);
}

export async function previewBudgetTlsRecovery(execute: Execute, original: PrivateActivation): Promise<BudgetTlsRecovery> {
  validateScope(original);
  await assertOperator(execute);
  const activation = structuredClone(original);
  const namespace = await read(execute, "namespace", activation.root.namespace.name);
  const deployment = await read(execute, "deployment", activation.root.deployment.name, activation.root.namespace.name);
  const state = retirementReview(activation, namespace, deployment);
  if (!state || state.phase !== "retired" || !state.baseline || !state.pauseRoot) throw new Error(failure);
  assertPending(namespace, activation, state);
  activation.root.namespace = reviewed(namespace);
  activation.namespaces[0]!.namespace = reviewed(namespace);
  activation.root.deployment = reviewed(deployment);
  activation.namespaces[0]!.consumers[0]!.object = reviewed(deployment);
  await validatePrivateActivation(execute, activation);
  await assertRetiredRoot(execute, activation, state);
  await assertNoAuthority(execute, activation, state);
  const checksum = at(deployment, "spec", "template", "metadata", "annotations", CHECKSUM);
  if (typeof checksum !== "string" || !/^[a-f0-9]{64}$/.test(checksum)) throw new Error(failure);
  return { version: 1, activation, retirement: state, checksum };
}

export async function applyBudgetTlsRecovery(execute: Execute, review: BudgetTlsRecovery): Promise<void> {
  if (!review || Object.keys(review).sort().join(",") !== "activation,checksum,retirement,version"
    || review.version !== 1 || !/^[a-f0-9]{64}$/.test(review.checksum)) throw new Error(failure);
  validateScope(review.activation);
  await assertOperator(execute);
  const original = review.activation;
  const namespace = await read(execute, "namespace", original.root.namespace.name);
  const state = retirementState(namespace);
  if (!state || state.phase !== "retired" || !state.baseline || !state.pauseRoot
    || canonical(state) !== canonical(review.retirement) || state.binding !== retirementBinding(original)
    || canonical(reviewed(namespace)) !== canonical(original.root.namespace)) throw new Error(failure);
  assertPending(namespace, original, state);
  const deployment = await read(execute, "deployment", original.root.deployment.name, original.root.namespace.name);
  if (reviewed(deployment).uid !== original.root.deployment.uid || at(deployment, "spec", "replicas") !== 0) throw new Error(failure);
  const checksum = at(deployment, "spec", "template", "metadata", "annotations", CHECKSUM);
  if (typeof checksum !== "string" || !/^[a-f0-9]{64}$/.test(checksum) || checksum === review.checksum) throw new Error(failure);
  // Reconstruct the old template locally: its full digest proves that no other field changed.
  const reconstructed = structuredClone(deployment);
  record(at(reconstructed, "spec", "template", "metadata", "annotations"))[CHECKSUM] = review.checksum;
  retirementReview(original, namespace, reconstructed);
  if (await verifyPrivateBundle(execute) !== original.bundleRevision) throw new Error(failure);
  if (reviewed(await read(execute, "serviceaccount", original.root.account.name, original.root.namespace.name)).uid
    !== original.root.account.uid) throw new Error(failure);
  await validateObservedControllerProfile(execute, original.profile);
  for (const [name, uid] of Object.entries(original.controllerUids)) {
    if (reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid !== uid) throw new Error(failure);
  }
  const budget = await reviewBudgetTls(execute, deployment, original.root.namespace.name);
  if (!budget || budget.secret.uid !== original.root.budgetTls!.secret.uid
    || budget.namespace.uid !== original.root.budgetTls!.namespace.uid
    || budget.secret.resourceVersion === state.baseline.resourceVersion
    || state.exposedKeys.includes(budget.keyDigest)) throw new Error("Budget TLS key must be newly rotated after retirement");
  await assertNoAuthority(execute, original, state);
  const next = structuredClone(original);
  next.root.deployment = reviewed(deployment);
  next.root.templateDigest = templateDigest(deployment);
  next.root.budgetTls = budget;
  next.namespaces[0]!.consumers[0]!.object = reviewed(deployment);
  next.namespaces[0]!.consumers[0]!.templateDigest = next.root.templateDigest;
  const nextState = { ...state, binding: retirementBinding(next) };
  const fields = { ...annotations(next, next.namespaces[0]!, "Pending"), [FIELD]: canonical(nextState) };
  const updated = JSON.parse(await execute(["patch", "namespace", original.root.namespace.name, "--type=merge",
    "-p", JSON.stringify({ metadata: { uid: original.root.namespace.uid,
      resourceVersion: original.root.namespace.resourceVersion, annotations: fields } }), "-o", "json"]));
  if (reviewed(updated).uid !== original.root.namespace.uid) throw new Error("Budget TLS recovery returned another namespace");
  assertPending(updated, next, nextState);
}
