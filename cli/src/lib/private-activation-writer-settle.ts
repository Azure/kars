// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, digest, read, readSecretMetadata, record, reviewed, reviewedOwner, template, templateDigest,
  type Execute, type Json, type PrivateActivation,
} from "./private-activation.js";
import { captureLateWriterScope } from "./private-activation-late-scope.js";
import { reviewPrivateContinuity } from "./private-activation-continuity.js";
import { replicaIntent } from "./private-activation-retirement.js";

const GRANTS = "karscredentialgrants.kars.azure.com";
const CREDENTIAL = "kars.azure.com/credential-";
const PROJECTION = `${CREDENTIAL}projection-version`;
const INPUTS = `${CREDENTIAL}input-state`;
const REVISION = "deployment.kubernetes.io/revision";
const ERROR = "Writer retirement changed the captured runtime authority; preserve quiescence and obtain explicit operator recovery";
type ObjectValue = ReturnType<typeof record>;
type Captured = NonNullable<Awaited<ReturnType<typeof captureLateWriterScope>>>;
interface RuntimeReview {
  captured: Captured;
  bundle: ObjectValue;
  projection: ObjectValue;
  sources: ObjectValue[];
  inputs: ObjectValue;
  admin: ObjectValue;
  pauseSeen: boolean;
  withdrawnVersion?: string;
  emptyVersion?: string;
  restored?: ObjectValue;
}
export interface WriterSettlement {
  grant: ObjectValue;
  quiescentSpec: Json;
  runtimes: RuntimeReview[];
  deadline: number;
}

function array(value: unknown): Json[] {
  if (!Array.isArray(value)) throw new Error(ERROR);
  return value as Json[];
}
function field(object: unknown, key: string): Json | undefined {
  return at(object, "metadata", "annotations", `${CREDENTIAL}${key}`);
}
function gen(object: unknown): number {
  const value = at(object, "metadata", "generation");
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) throw new Error(ERROR);
  return value;
}
function sameBody(a: Json, b: Json, status = false, generation = false): boolean {
  const normalized = (value: Json) => {
    const result = structuredClone(record(value));
    const metadata = record(result.metadata);
    delete metadata.resourceVersion;
    if (generation) delete metadata.generation;
    if (status) delete result.status;
    return canonical(result);
  };
  return normalized(a) === normalized(b);
}
function unchangedSecretMetadata(current: ObjectValue, before: ObjectValue, inputs = false): boolean {
  const comparable = (value: ObjectValue) => {
    const copy = structuredClone(value);
    delete copy.data;
    if (inputs) delete record(at(copy, "metadata", "annotations"))[INPUTS];
    return copy;
  };
  return current.type === "Opaque" && sameBody(comparable(current), comparable(before));
}
function projectionMetadataView(metadata: ObjectValue, before: ObjectValue): ObjectValue {
  const comparable = structuredClone(metadata);
  // Default kubectl JSON omits managedFields, whereas JSONPath retains them.
  // Match only that printer difference; a captured field remains authoritative.
  if (!Object.hasOwn(record(before.metadata), "managedFields")) delete comparable.managedFields;
  return comparable;
}
function data(secret: ObjectValue): ObjectValue { return record(secret.data ?? {}); }
function readyTask(task: ObjectValue, original: Json): boolean {
  const condition = array(at(task, "status", "conditions") ?? []);
  return at(task, "status", "phase") === "Ready"
    && at(task, "status", "observedGeneration") === gen(original)
    && at(task, "status", "envelopeDigest") === at(original, "status", "envelopeDigest")
    && condition.some(value => at(value, "type") === "Ready" && at(value, "status") === "True");
}
function withdrawn(task: ObjectValue, original: Json): boolean {
  return at(task, "status", "phase") === "Degraded"
    && at(task, "status", "observedGeneration") === gen(original)
    && at(task, "status", "envelopeDigest") == null
    && array(at(task, "status", "conditions") ?? []).some(value =>
      at(value, "type") === "Ready" && at(value, "status") === "False"
      && at(value, "reason") === "CredentialAuthorityUnavailable");
}
function bodySpec(deployment: ObjectValue, replicas: number, revision: string): ObjectValue {
  const result = structuredClone(deployment);
  record(result.spec).replicas = replicas;
  record(at(template(result), "metadata", "annotations"))[PROJECTION] = revision;
  if (revision !== at(template(deployment), "metadata", "annotations", PROJECTION)) {
    const original = at(deployment, "metadata", "annotations", REVISION);
    if (typeof original !== "string" || !/^[1-9][0-9]*$/.test(original)
      || !Number.isSafeInteger(Number(original) + 1)) throw new Error(ERROR);
    record(at(result, "metadata", "annotations"))[REVISION] = String(Number(original) + 1);
  }
  return result;
}
function possibleTransition(current: ObjectValue, runtime: RuntimeReview): boolean {
  const before = runtime.captured.deployment;
  const revision = at(template(current), "metadata", "annotations", PROJECTION);
  if (typeof revision !== "string" || !revision.startsWith(`${reviewed(runtime.projection).uid}:`)
    || revision.endsWith(":") || ![0, replicaIntent(before)].includes(replicaIntent(current))
    || gen(current) < gen(before) || gen(current) > gen(before) + 2) return false;
  const expected = bodySpec(before, replicaIntent(current), revision);
  if (sameBody(current, expected, true, true)) return true;
  record(at(expected, "metadata", "annotations"))[REVISION] = at(before, "metadata", "annotations", REVISION)!;
  return at(current, "status", "observedGeneration") !== gen(current) && sameBody(current, expected, true, true);
}

export async function captureWriterSettlement(
  execute: Execute, activation: PrivateActivation, grant: unknown,
): Promise<WriterSettlement | undefined> {
  const original = structuredClone(record(grant));
  const grantId = reviewed(original);
  const runtimes: RuntimeReview[] = [];
  const continuity = await reviewPrivateContinuity(execute, activation);
  const lateScopes = new Set(continuity?.lateScopes ?? []);
  for (const scope of activation.namespaces) {
    if (!lateScopes.has(scope.namespace.uid)) continue;
    const captured = await captureLateWriterScope(execute, activation, scope);
    if (!captured) continue;
    const bindings = record(at(captured.task, "spec", "blueprint", "credentialBindings"));
    if (at(bindings, "grant", "uid") !== grantId.uid) continue;
    const task = reviewed(captured.task);
    const sandbox = reviewed(captured.sandbox);
    const workspace = String(at(captured.task, "metadata", "namespace"));
    if (task.name !== sandbox.name || at(bindings, "grant", "name") !== "workspace"
      || workspace !== at(original, "metadata", "namespace")
      || canonical(at(captured.sandbox, "spec", "credentialBindings")) !== canonical(bindings)
      || at(captured.deployment, "spec", "strategy", "type") !== "Recreate"
      || array(at(original, "spec", "writers")).some(writer => at(writer, "namespace") === scope.namespace.name)) {
      throw new Error("Writer settling requires the exact declared v2 Task-owned runtime and its Recreate policy");
    }
    const bundle = await read(execute, "secret", `kars-credential-bundle-karstask-${task.name}`, workspace);
    const projection = await read(execute, "secret", `${sandbox.name}-credential-projection`, scope.namespace.name);
    const owner = [{ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: task.name, uid: task.uid, controller: true, blockOwnerDeletion: false }];
    if (field(captured.task, "bundle-uid") !== reviewed(bundle).uid
      || field(bundle, "purpose") !== "agent-bundle-v2"
      || field(bundle, "grant-uid") !== grantId.uid || field(bundle, "target-uid") !== task.uid
      || canonical(at(bundle, "metadata", "ownerReferences")) !== canonical(owner)
      || field(projection, "purpose") !== "agent-projection-v1"
      || field(projection, "sandbox-uid") !== sandbox.uid || field(projection, "namespace-uid") !== scope.namespace.uid
      || field(projection, "projection-uid") !== reviewed(projection).uid
      || field(projection, "source-uid") !== reviewed(bundle).uid
      || canonical(at(projection, "metadata", "ownerReferences")) !== canonical([
        { apiVersion: "v1", kind: "Namespace", name: scope.namespace.name, uid: scope.namespace.uid, controller: true, blockOwnerDeletion: false },
      ]) || bundle.type !== "Opaque" || projection.type !== "Opaque"
      || canonical(data(bundle)) !== canonical(data(projection))
      || at(template(captured.deployment), "metadata", "annotations", PROJECTION) !== `${reviewed(projection).uid}:${reviewed(projection).resourceVersion}`) throw new Error(ERROR);
    const inputs = record(JSON.parse(String(field(bundle, "input-state"))));
    if (inputs.grantUid !== grantId.uid || inputs.grantGeneration !== gen(original)
      || canonical(inputs.bindings) !== canonical(bindings)
      || canonical(inputs.target) !== canonical({ kind: "KarsTask", namespace: workspace, name: task.name, uid: task.uid })) throw new Error(ERROR);
    const sources: ObjectValue[] = [];
    const values: ObjectValue = {};
    const selections = array(bindings.sources);
    if (selections.length > 16 || array(inputs.sources).length !== selections.length) throw new Error(ERROR);
    for (const [index, selection] of selections.entries()) {
      const source = await read(execute, "secret", String(at(selection, "source", "name")), workspace);
      const id = reviewed(source);
      const input = array(inputs.sources)[index];
      if (id.uid !== at(selection, "source", "uid") || id.uid !== at(input, "uid")
        || id.name !== at(input, "name") || id.resourceVersion !== at(input, "resourceVersion")
        || source.type !== "Opaque" || field(source, "purpose") !== "agent-input-v2"
        || field(source, "grant-uid") !== grantId.uid
        || canonical(at(selection, "keys")) !== canonical(at(input, "keys"))
        || at(selection, "scope") !== at(input, "scope")) throw new Error(ERROR);
      for (const key of array(at(selection, "keys"))) {
        if (typeof key !== "string") throw new Error(ERROR);
        if (data(source)[key] === undefined) delete values[key]; else values[key] = data(source)[key]!;
      }
      sources.push(source);
    }
    if (canonical(values) !== canonical(data(bundle))) throw new Error(ERROR);
    const admin = await read(execute, "secret", "router-services-admin", scope.namespace.name);
    if (reviewed(admin).uid !== captured.admin.object.uid || reviewed(admin).resourceVersion !== captured.admin.object.resourceVersion
      || digest(String(data(admin)["control-token"])) !== captured.admin.key) throw new Error(ERROR);
    runtimes.push({ captured, bundle, projection, sources, inputs, admin, pauseSeen: replicaIntent(captured.deployment) === 0 });
  }
  return runtimes.length ? { grant: original, quiescentSpec: { ...record(original.spec), writers: [] },
    runtimes, deadline: Date.now() + 120_000 } : undefined;
}

/** Observations never publish attestation, restore replicas, or replace Secrets. */
export async function observeWriterSettlement(
  execute: Execute, activation: PrivateActivation, review: WriterSettlement,
): Promise<boolean> {
  const workspace = String(at(review.grant, "metadata", "namespace"));
  const grant = await read(execute, GRANTS, "workspace", workspace);
  if (reviewed(grant).uid !== reviewed(review.grant).uid || gen(grant) !== gen(review.grant) + 1
    || canonical(grant.spec) !== canonical(review.quiescentSpec)) throw new Error(ERROR);
  const names = new Set(review.runtimes.map(value => value.captured.scope.namespace.name));
  const rootReview = structuredClone(activation);
  rootReview.namespaces = rootReview.namespaces.filter(scope => !names.has(scope.namespace.name));
  if (!await reviewPrivateContinuity(execute, rootReview)) throw new Error(ERROR);
  const grantReady = at(grant, "status", "phase") === "Ready" && at(grant, "status", "observedGeneration") === gen(grant);
  let allReady = grantReady;
  for (const runtime of review.runtimes) {
    const before = runtime.captured;
    const ns = before.scope.namespace.name;
    const task = await read(execute, "karstask", reviewed(before.task).name, workspace);
    const sandbox = await read(execute, "karssandbox", reviewed(before.sandbox).name, workspace);
    const namespace = await read(execute, "namespace", ns);
    const deployment = await read(execute, "deployments.apps", reviewed(before.deployment).name, ns);
    if (!sameBody(task, before.task, true) || !sameBody(sandbox, before.sandbox, true)
      || canonical(namespace) !== canonical(before.namespace)) throw new Error(ERROR);
    const taskReady = readyTask(task, before.task);
    if (!taskReady) {
      if (!withdrawn(task, before.task)) throw new Error("Task lost authority for an unreviewed reason during writer retirement");
      runtime.withdrawnVersion = reviewed(task).resourceVersion;
    } else if (runtime.withdrawnVersion && [runtime.withdrawnVersion, reviewed(before.task).resourceVersion]
      .includes(reviewed(task).resourceVersion)) throw new Error("Stale Task attestation cannot settle writer retirement");
    for (const source of runtime.sources) {
      if (canonical(await read(execute, "secret", reviewed(source).name, workspace)) !== canonical(source)) throw new Error("Captured credential source/key changed during writer retirement");
    }
    if (canonical(await read(execute, "secret", "router-services-admin", ns)) !== canonical(runtime.admin)) throw new Error("Unreviewed private material changed during writer retirement");
    for (const name of ["router-services-observer", "router-services-observer-identity", "router-github-app", "kars-observation-privacy-tls", "sre-api-router-identity"]) {
      if (await readSecretMetadata(execute, name, ns, true) !== undefined) throw new Error("Additional private material appeared during writer retirement");
    }
    const bundle = await read(execute, "secret", reviewed(runtime.bundle).name, workspace);
    const projection = await read(execute, "secret", reviewed(runtime.projection).name, ns);
    if (!unchangedSecretMetadata(bundle, runtime.bundle, true) || canonical(data(bundle)) !== canonical(data(runtime.bundle))
      || !unchangedSecretMetadata(projection, runtime.projection)) throw new Error(ERROR);
    const inputs = record(JSON.parse(String(field(bundle, "input-state"))));
    const expectedInputs = { ...runtime.inputs, grantGeneration: gen(grant) };
    if (canonical(inputs) !== canonical(runtime.inputs) && canonical(inputs) !== canonical(expectedInputs)) throw new Error(ERROR);
    const oldRevision = `${reviewed(runtime.projection).uid}:${reviewed(runtime.projection).resourceVersion}`;
    const revision = `${reviewed(projection).uid}:${reviewed(projection).resourceVersion}`;
    const projectionSame = canonical(data(projection)) === canonical(data(runtime.projection));
    const replicas = replicaIntent(deployment);
    const initialReplicas = replicaIntent(before.deployment);
    if (replicas !== 0 && replicas !== initialReplicas) throw new Error(ERROR);
    const paused = bodySpec(before.deployment, 0, oldRevision);
    const unchanged = sameBody(deployment, before.deployment, true, true);
    const isPause = sameBody(deployment, paused, true, true) && replicas === 0;
    if (isPause) {
      if (gen(deployment) !== gen(before.deployment) + initialReplicas) throw new Error(ERROR);
      runtime.pauseSeen = true;
    }
    if (!projectionSame) {
      if (!isPause || !runtime.withdrawnVersion || Object.keys(data(projection)).length) throw new Error("Projection changed without the captured authority withdrawal and owned pause");
      runtime.emptyVersion = reviewed(projection).resourceVersion;
    }
    const restored = bodySpec(before.deployment, initialReplicas, revision);
    const restoredShape = sameBody(deployment, restored, true, true);
    const changedRevision = revision !== oldRevision;
    const awaitingRevision = structuredClone(restored);
    record(at(awaitingRevision, "metadata", "annotations"))[REVISION] = at(before.deployment, "metadata", "annotations", REVISION)!;
    const restoringShape = restoredShape || (at(deployment, "status", "observedGeneration") !== gen(deployment)
      && sameBody(deployment, awaitingRevision, true, true));
    if (projectionSame && ((changedRevision && !runtime.emptyVersion)
      || (runtime.emptyVersion && [runtime.emptyVersion, reviewed(runtime.projection).resourceVersion]
        .includes(reviewed(projection).resourceVersion)))) {
      throw new Error("Projection revision advanced without witnessed fresh revoke/refill; exact review preserved");
    }
    const restoredGeneration = gen(before.deployment) + (initialReplicas ? 2 : Number(changedRevision));
    if (!unchanged && !isPause && (!restoringShape || !runtime.pauseSeen || gen(deployment) !== restoredGeneration)) {
      throw new Error("Unreviewed template or controller pause/restore generation changed");
    }
    if (unchanged && gen(deployment) !== gen(before.deployment) && (!runtime.pauseSeen || gen(deployment) !== restoredGeneration)) throw new Error(ERROR);
    const projectionAfter = await readSecretMetadata(execute, reviewed(projection).name, ns);
    const deploymentAfter = await read(execute, "deployments.apps", reviewed(deployment).name, ns);
    const checks = {
      projectionMetadataPresent: projectionAfter !== undefined,
      projectionMetadataMatches: projectionAfter !== undefined && unchangedSecretMetadata({
        metadata: projectionMetadataView(projectionAfter, runtime.projection), type: "Opaque",
      }, { metadata: runtime.projection.metadata!, type: "Opaque" }),
      deploymentTransitionMatches: possibleTransition(deploymentAfter, runtime),
    };
    if (!checks.projectionMetadataPresent || !checks.projectionMetadataMatches || !checks.deploymentTransitionMatches) {
      console.error(`KARS_PRIVATE_WRITER_RECHECK ${JSON.stringify(checks)}`);
      throw new Error(ERROR);
    }
    if (reviewed({ metadata: projectionAfter }).resourceVersion !== reviewed(projection).resourceVersion
      || reviewed(deploymentAfter).resourceVersion !== reviewed(deployment).resourceVersion) {
      allReady = false;
      continue;
    }
    const list = record(JSON.parse(await execute(["get", "pods", "-n", ns, "--chunk-size=0", "-o", "json"])));
    if (at(list, "metadata", "continue")) throw new Error(ERROR);
    const pods = array(list.items);
    const liveScope = { ...before.scope, consumers: [{ ...before.scope.consumers[0]!, templateDigest: templateDigest(deployment) }] };
    for (const pod of pods) if (!await reviewedOwner(execute, pod, liveScope)) throw new Error("Unreviewed consumer appeared during writer retirement");
    const oldGone = pods.every(pod => !before.pods.some(old => reviewed(old, true).uid === reviewed(pod, true).uid));
    const sandboxReady = at(sandbox, "status", "phase") === "Running"
      && at(sandbox, "status", "observedGeneration") === gen(before.sandbox)
      && array(at(sandbox, "status", "conditions") ?? []).some(condition =>
        at(condition, "type") === "Ready" && at(condition, "status") === "True"
        && at(condition, "observedGeneration") === gen(before.sandbox));
    const deploymentReady = replicas === initialReplicas && at(deployment, "status", "observedGeneration") === gen(deployment)
      && (!initialReplicas || (at(deployment, "status", "availableReplicas") === initialReplicas
        && at(deployment, "status", "updatedReplicas") === initialReplicas));
    const consumed = canonical(inputs) === canonical(expectedInputs);
    const changedDeployment = reviewed(deployment).resourceVersion !== reviewed(before.deployment).resourceVersion;
    if (grantReady && taskReady && sandboxReady && deploymentReady && consumed && changedDeployment && !runtime.pauseSeen) {
      throw new Error("Consumer revision advanced without a witnessed owned pause; exact review preserved");
    }
    const complete = grantReady && taskReady && sandboxReady && deploymentReady && projectionSame && consumed
      && restoredShape && (!runtime.withdrawnVersion || reviewed(task).resourceVersion !== reviewed(before.task).resourceVersion)
      && (!changedDeployment || (runtime.pauseSeen && oldGone));
    runtime.restored = complete ? deployment : undefined;
    allReady &&= complete;
  }
  return allReady;
}

export async function settleWriterRetirement(
  execute: Execute, activation: PrivateActivation, review: WriterSettlement,
): Promise<PrivateActivation> {
  for (;;) {
    const ready = await observeWriterSettlement(execute, activation, review);
    if (ready) {
      const roles = record(JSON.parse(await execute(["get", "roles,rolebindings,clusterroles,clusterrolebindings",
        "--all-namespaces", "--chunk-size=0", "-o", "json"])));
      if (at(roles, "metadata", "continue") || array(roles.items).some(value =>
        at(value, "metadata", "annotations", "kars.azure.com/credential-grant-owner") === reviewed(review.grant).uid)) throw new Error(ERROR);
      const settled = structuredClone(activation);
      for (const runtime of review.runtimes) {
        const scope = settled.namespaces.find(value => value.namespace.uid === runtime.captured.scope.namespace.uid);
        if (!scope || !runtime.restored) throw new Error(ERROR);
        scope.consumers[0]!.object = reviewed(runtime.restored);
        scope.consumers[0]!.templateDigest = templateDigest(runtime.restored);
      }
      return settled;
    }
    if (Date.now() >= review.deadline) throw new Error("Writer retirement is still awaiting fresh Task attestation and the captured owned runtime; no stale authority or new activation was published");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
}
