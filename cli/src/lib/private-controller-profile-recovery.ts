// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  annotations, at, bundleDefinition, canonical, digest, PRIVATE_PREFIX, read, record,
  requiresPrivatePodReview, reviewed, templateDigest, validateActivationShape, validatePrivateActivation,
  type Execute, type PrivateActivation, type ReviewedObject,
} from "./private-activation.js";
import {
  retirementBinding, retirementReview, type RootRetirement,
} from "./private-activation-retirement.js";

const RETIREMENT = "kars.azure.com/private-root-retirement";
const failure = "Controller profile recovery requires an unchanged initial Pending retirement; protection was preserved";

export interface ControllerProfileRecovery {
  version: 1;
  activation: PrivateActivation;
  retirement: RootRetirement;
  controllerPods: { identity: ReviewedObject; templateDigest: string }[];
  controllerUids: Record<string, string>;
  grants: { identity: ReviewedObject; namespace: string; specDigest: string }[];
}

function items(value: unknown): unknown[] {
  const found = at(value, "items");
  if (at(value, "metadata", "continue") || !Array.isArray(found)) {
    throw new Error("Controller profile recovery inventory is incomplete");
  }
  return found;
}

function privateAnnotations(value: unknown): Record<string, unknown> {
  return Object.fromEntries(Object.entries(record(at(value, "metadata", "annotations") ?? {}))
    .filter(([key]) => key.startsWith(PRIVATE_PREFIX)));
}

export async function previewControllerProfileRecovery(
  execute: Execute, original: PrivateActivation,
): Promise<ControllerProfileRecovery> {
  validateActivationShape(original, "reviewed");
  if ((await execute(["auth", "can-i", "manage", "karscredentialgrants.kars.azure.com/workspace",
    "--all-namespaces"])).trim() !== "yes") throw new Error("Cluster-scoped credential operator authority is required");
  const activation = structuredClone(original);
  const root = activation.root;
  const scope = activation.namespaces[0];
  if (activation.profile !== "kcm-certificate" || activation.namespaces.length !== 1
    || !scope || scope.namespace.uid !== root.namespace.uid
    || scope.consumers.length !== 1 || scope.consumers[0]?.kind !== "Deployment"
    || scope.consumers[0]?.object.uid !== root.deployment.uid) throw new Error(failure);

  const namespace = await read(execute, "namespace", root.namespace.name);
  const deployment = await read(execute, "deployment", root.deployment.name, root.namespace.name);
  const state = retirementReview(activation, namespace, deployment);
  if (!state || state.phase !== "pausing" || state.baseline || !state.pauseRoot
    || at(deployment, "spec", "replicas") !== state.replicaIntent) throw new Error(failure);
  const expected = { ...annotations(activation, scope, "Pending"), [RETIREMENT]: canonical(state) };
  if (canonical(privateAnnotations(namespace)) !== canonical(expected)) throw new Error(failure);

  scope.namespace = reviewed(namespace);
  root.namespace = reviewed(namespace);
  root.deployment = reviewed(deployment);
  scope.consumers[0].object = reviewed(deployment);
  await validatePrivateActivation(execute, activation);
  const rootPods = items(JSON.parse(await execute([
    "get", "pods", "-n", root.namespace.name, "--chunk-size=0", "-o", "json",
  ])));
  const captured = new Set(state.captured[root.namespace.uid] ?? []);
  // Never rebind a newly appeared consuming Pod or discard an old captured UID.
  for (const pod of rootPods) {
    if (!captured.has(reviewed(pod, true).uid)
      && (at(pod, "spec", "serviceAccountName") === root.account.name
        || await requiresPrivatePodReview(execute, record(pod), scope, activation))) throw new Error(failure);
  }

  const grants = items(JSON.parse(await execute([
    "get", "karscredentialgrants.kars.azure.com", "--all-namespaces", "--chunk-size=0", "-o", "json",
  ]))).map((grant) => {
    const writers = at(grant, "spec", "writers");
    if (!Array.isArray(writers) || writers.length || at(grant, "spec", "privateActivation") != null) {
      throw new Error("Controller profile recovery refuses existing private grants or writer authority");
    }
    const namespace = at(grant, "metadata", "namespace");
    if (typeof namespace !== "string" || !namespace) throw new Error(failure);
    return { identity: reviewed(grant), namespace, specDigest: digest(at(grant, "spec")) };
  }).sort((a, b) => a.identity.uid.localeCompare(b.identity.uid));

  const controllerPods = items(JSON.parse(await execute([
    "get", "pods", "-n", "kube-system", "-l", "component=kube-controller-manager",
    "--chunk-size=0", "-o", "json",
  ]))).map((pod) => {
    const containers = at(pod, "spec", "containers");
    if (!at(pod, "metadata", "annotations", "kubernetes.io/config.mirror")
      || !Array.isArray(containers) || containers.length !== 1) {
      throw new Error("Controller profile recovery requires the observed static controller-manager configuration");
    }
    const command = at(containers[0], "command");
    const args = at(containers[0], "args") ?? [];
    if (!Array.isArray(command) || !Array.isArray(args)) throw new Error(failure);
    const flags = [...command, ...args].filter((arg) =>
      typeof arg === "string" && arg.startsWith("--use-service-account-credentials"));
    if (canonical(flags) !== canonical(["--use-service-account-credentials=true"])) {
      throw new Error("Controller-manager ServiceAccount credential use was not verified");
    }
    return { identity: reviewed(pod), templateDigest: templateDigest(pod) };
  }).sort((a, b) => a.identity.uid.localeCompare(b.identity.uid));
  if (!controllerPods.length) throw new Error("Controller-manager configuration is unavailable; no profile is inferred");
  const controllerUids: Record<string, string> = {};
  const names = bundleDefinition().controllers;
  if (!Array.isArray(names)) throw new Error(failure);
  for (const name of names) {
    if (typeof name !== "string") throw new Error(failure);
    controllerUids[name] = reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid;
  }
  return { version: 1, activation, retirement: state, controllerPods, controllerUids, grants };
}

export async function applyControllerProfileRecovery(
  execute: Execute, review: ControllerProfileRecovery,
): Promise<void> {
  if (!review || review.version !== 1) throw new Error("A reviewed controller profile recovery is required");
  const current = await previewControllerProfileRecovery(execute, review.activation);
  if (canonical(current) !== canonical(review)) throw new Error("Controller profile recovery changed; regenerate its review");
  const next = structuredClone(current.activation);
  next.profile = "service-accounts";
  next.controllerUids = current.controllerUids;
  const scope = next.namespaces[0]!;
  const nextState = { ...current.retirement, binding: retirementBinding(next) };
  const fields = { ...annotations(next, scope, "Pending"), [RETIREMENT]: canonical(nextState) };
  const updated = JSON.parse(await execute([
    "patch", "namespace", scope.namespace.name, "--type=merge", "-p", JSON.stringify({
      metadata: { uid: scope.namespace.uid, resourceVersion: scope.namespace.resourceVersion, annotations: fields },
    }), "-o", "json",
  ]));
  if (reviewed(updated).uid !== scope.namespace.uid
    || canonical(privateAnnotations(updated)) !== canonical(fields)) {
    throw new Error("Controller profile recovery acknowledgement differs; inspect current protection before proceeding");
  }
}
