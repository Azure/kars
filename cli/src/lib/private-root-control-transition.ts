// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, digest, matchesReviewedExecution, patchNamespace, PRIVATE_PREFIX as P, read, record, reviewed, reviewedOwner, templateDigest,
  validateActivationShape, validateQualifiedActivation, type Execute, type PrivateActivation,
} from "./private-activation.js";
import { replicaIntent, retirementBinding, retirementState, type RootRetirement } from "./private-activation-retirement.js";
import { restoreSreRuntimeImages, reviewSreProjection, SRE_PROJECTION_FIELDS, validateSreProjection, type RootSreProjection } from "./private-root-sre-projection.js";

type ObjectValue = ReturnType<typeof record>;
const FIELD = `${P}root-retirement`;
const VERSION = "kars.azure.com/services-credential-version";
const ERROR = "Root migration control rotation is not an exact, retired controller-owned transition";
export interface RootControlTransition {
  before: string;
  after: string;
  qualification?: PrivateActivation;
  consumers: { namespace: string; before: ObjectValue; after: ObjectValue; secretUid: string; reconciliation?: RootSreProjection }[];
  applied?: boolean;
  previous?: RootControlTransition;
}

function proof(raw: string, qualification?: PrivateActivation): { activation: PrivateActivation; state: RootRetirement; value: ObjectValue } {
  const original = record(JSON.parse(raw));
  const state = retirementState({ metadata: { annotations: { [FIELD]: raw } } });
  if (canonical(original) !== raw || !state || state.phase !== "restoring"
    || (qualification ? original.version !== 1 : original.version !== 2)) throw new Error(ERROR);
  const value = qualification ? record({ version: 2, retirement: canonical(state), retirementDigest: digest(state), activation: qualification }) : original;
  const activation = value.activation as unknown as PrivateActivation;
  validateActivationShape(activation, "qualified");
  if (state.binding !== retirementBinding(activation)) throw new Error(ERROR);
  return { activation, state, value };
}

function version(value: ObjectValue): string {
  const marker = at(value, "spec", "template", "metadata", "annotations", VERSION);
  if (typeof marker !== "string" || !/^[^:]+:control-v1:[a-f0-9]{64}$/.test(marker)) throw new Error(ERROR);
  return marker;
}

function withVersion(value: ObjectValue, marker: string): ObjectValue {
  const result = structuredClone(value);
  record(at(result, "spec", "template", "metadata", "annotations"))[VERSION] = marker;
  return result;
}

function withProjection(value: ObjectValue, source: ObjectValue, reconciled = false, runtime?: RootSreProjection["runtime"]): ObjectValue {
  const result = withVersion(value, version(source));
  if (runtime) restoreSreRuntimeImages(result, source, runtime.routerImage !== undefined);
  if (reconciled) {
    const fields = record(at(result, "spec", "template", "metadata", "annotations"));
    for (const key of SRE_PROJECTION_FIELDS) fields[key] = at(source, "spec", "template", "metadata", "annotations", key)!;
    record(result.spec).replicas = at(source, "spec", "replicas")!;
  }
  return result;
}

export function validateRootControlTransition(transition: RootControlTransition): void {
  const before = proof(transition.before, transition.qualification), after = proof(transition.after);
  if (transition.previous) {
    validateRootControlTransition(transition.previous);
    if (transition.previous.before !== transition.before
      || canonical(transition.previous.qualification ?? null) !== canonical(transition.qualification ?? null)
      || transition.previous.after === transition.after) throw new Error(ERROR);
    for (const prior of transition.previous.consumers) {
      const current = transition.consumers.find(value => reviewed(value.before).uid === reviewed(prior.before).uid);
      if (!current || !current.reconciliation || canonical(current.before.spec) !== canonical(prior.before.spec)
        || current.secretUid !== prior.secretUid) throw new Error(ERROR);
    }
  }
  if (!Array.isArray(transition.consumers) || !transition.consumers.length
    || new Set(transition.consumers.map(value => reviewed(value.before).uid)).size !== transition.consumers.length
    || (transition.applied !== undefined && transition.applied !== true)) throw new Error(ERROR);
  const expected = structuredClone(before.value);
  const activation = expected.activation as unknown as PrivateActivation;
  for (const change of transition.consumers) {
    if (change.reconciliation) validateSreProjection(change.reconciliation, change.before, change.after, before.activation);
    const scope = activation.namespaces.find(value => value.namespace.name === change.namespace);
    const consumer = scope?.consumers.find(value => value.object.uid === reviewed(change.before).uid);
    if (!scope || scope.namespace.uid === activation.root.namespace.uid || scope.consumers.length !== 1
      || !consumer || consumer.kind !== "Deployment" || change.before.kind !== "Deployment" || change.after.kind !== "Deployment"
      || at(change.before, "metadata", "namespace") !== change.namespace || at(change.after, "metadata", "namespace") !== change.namespace
      || reviewed(change.before).uid !== reviewed(change.after).uid || reviewed(change.before).name !== reviewed(change.after).name
      || replicaIntent(change.before) !== 0 || replicaIntent(change.after) !== (change.reconciliation?.replicas ?? 0)
      || (change.reconciliation && change.namespace !== "kars-sre")
      || version(change.before) === version(change.after)
      || !version(change.before).startsWith(`${change.secretUid}:control-v1:`)
      || !version(change.after).startsWith(`${change.secretUid}:control-v1:`)
      || canonical(withProjection(change.after, change.before, !!change.reconciliation, change.reconciliation?.runtime).spec) !== canonical(change.before.spec)
      || templateDigest(change.before) !== consumer.templateDigest
      || at(change.after, "spec", "template", "metadata", "annotations", `${P}epoch`) !== scope.epoch) throw new Error(ERROR);
    consumer.templateDigest = templateDigest(change.after);
  }
  const state = { ...before.state, binding: retirementBinding(activation) };
  expected.retirement = canonical(state);
  expected.retirementDigest = digest(state);
  if (canonical(expected) !== transition.after || canonical(after.state) !== canonical(state)) throw new Error(ERROR);
}

export function rootControlRetirement(transition: RootControlTransition): RootRetirement {
  validateRootControlTransition(transition);
  return proof(transition.after).state;
}

export function rootControlSpec(spec: ObjectValue, uid: string, transition?: RootControlTransition): ObjectValue {
  const change = transition?.consumers.find(value => reviewed(value.before).uid === uid);
  return change ? record(withProjection({ spec }, change.after, !!change.reconciliation, change.reconciliation?.runtime).spec) : spec;
}

export function rootControlReplicas(value: ObjectValue, transition?: RootControlTransition): number {
  return transition?.consumers.find(change => reviewed(change.before).uid === reviewed(value).uid)?.reconciliation?.replicas
    ?? replicaIntent(value);
}

async function verifyMaterial(run: Execute, activation: PrivateActivation, change: RootControlTransition["consumers"][number]): Promise<void> {
  const scope = activation.namespaces.find(value => value.namespace.name === change.namespace)!;
  const namespace = await read(run, "namespace", change.namespace);
  const fields = record(at(namespace, "metadata", "annotations"));
  const name = reviewed(change.after).name;
  const workspace = fields["kars.azure.com/sandbox-namespace"];
  if (reviewed(namespace).uid !== scope.namespace.uid || fields["kars.azure.com/namespace-claim-version"] !== "v1"
    || fields["kars.azure.com/sandbox-name"] !== name || change.namespace !== `kars-${name}`
    || typeof workspace !== "string" || fields[`${P}epoch`] !== scope.epoch) throw new Error(ERROR);
  const sandbox = await read(run, "karssandbox", name, workspace);
  const source = reviewed(sandbox).uid;
  if (fields["kars.azure.com/sandbox-uid"] !== source
    || at(sandbox, "metadata", "annotations", "kars.azure.com/namespace-uid") !== scope.namespace.uid) throw new Error(ERROR);
  const secret = await read(run, "secret", "router-services-admin", change.namespace);
  const data = record(secret.data);
  if (reviewed(secret).uid !== change.secretUid || secret.type !== "Opaque"
    || at(secret, "metadata", "labels", "app.kubernetes.io/managed-by") !== "kars-controller"
    || at(secret, "metadata", "annotations", "kars.azure.com/sandbox-uid") !== source
    || at(secret, "metadata", "annotations", "kars.azure.com/namespace-uid") !== scope.namespace.uid
    || at(secret, "metadata", "annotations", `${P}epoch`) !== scope.epoch
    || Object.keys(data).sort().join(",") !== "builtin-memory.json,control-token"
    || typeof data["control-token"] !== "string" || !data["control-token"]
    || version(change.after) !== `${change.secretUid}:control-v1:${digest({ "control-token": data["control-token"] })}`) throw new Error(ERROR);
  if (change.reconciliation && canonical(await reviewSreProjection(run, activation, change.namespace, change.after, change.before)) !== canonical(change.reconciliation)) throw new Error(ERROR);
  const pods = record(JSON.parse(await run(["get", "pods", "-n", change.namespace, "--chunk-size=0", "-o", "json"])));
  if (at(pods, "metadata", "continue") || !Array.isArray(pods.items) || (!change.reconciliation && pods.items.length)) throw new Error(ERROR);
  const projectedScope = structuredClone(scope);
  projectedScope.consumers[0]!.templateDigest = templateDigest(change.after);
  for (const pod of pods.items) {
    if (at(pod, "metadata", "annotations", `${P}epoch`) !== scope.epoch
      || !await reviewedOwner(run, pod, projectedScope, undefined, activation.root)) throw new Error(ERROR);
  }
  const current = await read(run, "deployment", name, change.namespace);
  if (reviewed(current).uid !== reviewed(change.after).uid || canonical(current.spec) !== canonical(change.after.spec)) throw new Error(ERROR);
}

function knownSeal(transition: RootControlTransition, raw: unknown): boolean {
  return raw === transition.before || raw === transition.after || (transition.previous !== undefined && raw === transition.previous.after);
}

/** Reconstruct only the independently verified controller projections in the historical read view. */
function historical(run: Execute, transition: RootControlTransition): Execute {
  return async (args, input) => {
    const podProjection = args[0] === "create" && args[1] === "--raw"
      && /^\/api\/v1\/namespaces\/[^/]+\/pods\?dryRun=All&fieldValidation=Strict$/.test(args[2] ?? "");
    if (!["get", "auth"].includes(args[0]!) && !podProjection) throw new Error(ERROR);
    const response = await run(args, input);
    if (args[0] === "get" && args[1] === "namespace"
      && args[2] === proof(transition.before, transition.qualification).activation.root.namespace.name) {
      const current = record(JSON.parse(response));
      if (!knownSeal(transition, at(current, "metadata", "annotations", FIELD))) throw new Error(ERROR);
      record(at(current, "metadata", "annotations"))[FIELD] = canonical(proof(transition.before, transition.qualification).value);
      return JSON.stringify(current);
    }
    const runtimeChange = args[0] === "get" ? transition.consumers.find(change => change.reconciliation?.runtime
      && change.namespace === args[args.indexOf("-n") + 1]) : undefined;
    if (runtimeChange && args[1] === "pods") {
      const inventory = record(JSON.parse(response));
      if (at(inventory, "metadata", "continue") || !Array.isArray(inventory.items)) throw new Error(ERROR);
      const activation = proof(transition.after).activation;
      const scope = activation.namespaces.find(value => value.namespace.name === runtimeChange.namespace)!;
      for (const pod of inventory.items) {
        if (at(pod, "metadata", "annotations", `${P}epoch`) !== scope.epoch
          || !await reviewedOwner(run, pod, scope, undefined, activation.root)) throw new Error(ERROR);
        restoreSreRuntimeImages({ spec: { template: { spec: record(pod).spec! } } }, runtimeChange.before, runtimeChange.reconciliation?.runtime?.routerImage !== undefined);
      }
      return JSON.stringify(inventory);
    }
    if (runtimeChange && args[1] === "replicasets.apps") {
      const current = record(JSON.parse(response));
      const owners = at(current, "metadata", "ownerReferences");
      const controllers = Array.isArray(owners) ? owners.filter(owner => at(owner, "controller") === true) : [];
      if (controllers.length !== 1 || at(controllers[0], "apiVersion") !== "apps/v1"
        || at(controllers[0], "kind") !== "Deployment" || at(controllers[0], "uid") !== reviewed(runtimeChange.after).uid
        || at(controllers[0], "name") !== reviewed(runtimeChange.after).name
        || !matchesReviewedExecution(at(current, "spec", "template", "spec"), at(runtimeChange.after, "spec", "template", "spec"), false)) throw new Error(ERROR);
      restoreSreRuntimeImages(current, runtimeChange.before, runtimeChange.reconciliation?.runtime?.routerImage !== undefined);
      return JSON.stringify(current);
    }
    if (args[0] !== "get" || !["deployment", "deployments.apps"].includes(args[1]!)) return response;
    const change = transition.consumers.find(value => reviewed(value.after).name === args[2]
      && value.namespace === args[args.indexOf("-n") + 1]);
    if (!change) return response;
    const current = record(JSON.parse(response));
    if (reviewed(current).uid !== reviewed(change.after).uid || canonical(current.spec) !== canonical(change.after.spec)) throw new Error(ERROR);
    return JSON.stringify(withProjection(current, change.before, !!change.reconciliation, change.reconciliation?.runtime));
  };
}

export async function prepareRootControlTransition(run: Execute, namespace: string, companions: ObjectValue[],
  qualification?: PrivateActivation, previous?: RootControlTransition): Promise<RootControlTransition | undefined> {
  const root = await read(run, "namespace", namespace);
  const liveRaw = at(root, "metadata", "annotations", FIELD);
  if (previous) {
    validateRootControlTransition(previous);
    if (!knownSeal(previous, liveRaw)) throw new Error(ERROR);
    qualification = previous.qualification;
  }
  const raw = previous?.before ?? liveRaw;
  if (typeof raw !== "string" || (!qualification && record(JSON.parse(raw)).version !== 2)) return undefined;
  const sealed = proof(raw, qualification), after = structuredClone(sealed.value);
  const activation = after.activation as unknown as PrivateActivation;
  const consumers: RootControlTransition["consumers"] = [];
  for (const scope of activation.namespaces) {
    if (scope.namespace.uid === activation.root.namespace.uid) continue;
    for (const consumer of scope.consumers) {
      const current = await read(run, "deployment", consumer.object.name, scope.namespace.name);
      if (templateDigest(current) === consumer.templateDigest) continue;
      const captured = companions.find(value => reviewed(value).uid === consumer.object.uid);
      if (!captured) throw new Error(ERROR);
      let before = withVersion(current, version(captured));
      let reconciliation: RootSreProjection | undefined;
      if (templateDigest(before) !== consumer.templateDigest || replicaIntent(current) !== 0) {
        reconciliation = await reviewSreProjection(run, sealed.activation, scope.namespace.name, current, captured);
        before = withProjection(current, captured, true, reconciliation.runtime);
        record(before.spec).replicas = 0;
      }
      if (templateDigest(before) !== consumer.templateDigest) throw new Error(ERROR);
      consumers.push({ namespace: scope.namespace.name, before, after: current, secretUid: version(captured).split(":")[0]!,
        ...(reconciliation ? { reconciliation } : {}) });
      consumer.templateDigest = templateDigest(current);
    }
  }
  if (!consumers.length) return undefined;
  const state = { ...sealed.state, binding: retirementBinding(activation) };
  after.retirement = canonical(state);
  after.retirementDigest = digest(state);
  if (previous && canonical(after) === previous.after) return previous;
  const transition = { before: raw, after: canonical(after), consumers, ...(qualification ? { qualification } : {}),
    ...(previous ? { previous } : {}) };
  validateRootControlTransition(transition);
  for (const change of consumers) await verifyMaterial(run, sealed.activation, change);
  await validateQualifiedActivation(historical(run, transition), sealed.activation);
  return transition;
}

export async function applyRootControlTransition(run: Execute, transition: RootControlTransition,
  fence: (historical: Execute) => Promise<void>): Promise<void> {
  validateRootControlTransition(transition);
  const before = proof(transition.before, transition.qualification), after = proof(transition.after);
  const namespace = before.activation.root.namespace.name;
  const current = await read(run, "namespace", namespace);
  const raw = at(current, "metadata", "annotations", FIELD);
  if (!knownSeal(transition, raw)) throw new Error(ERROR);
  for (const change of transition.consumers) await verifyMaterial(run, before.activation, change);
  const previous = historical(run, transition);
  await fence(previous);
  await validateQualifiedActivation(previous, before.activation);
  if (raw !== transition.after) {
    for (const change of transition.consumers) await verifyMaterial(run, before.activation, change);
    await patchNamespace(run, { ...before.activation.namespaces.find(value => value.namespace.name === namespace)!, namespace: reviewed(current) },
      { [FIELD]: transition.after }, { [FIELD]: String(raw) }, true);
  }
  for (const change of transition.consumers) await verifyMaterial(run, after.activation, change);
  await validateQualifiedActivation(run, after.activation);
}
