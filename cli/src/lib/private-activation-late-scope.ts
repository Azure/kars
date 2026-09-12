// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes } from "node:crypto";
import {
  annotations, at, bundleDefinition, canonical, consumesPrivateAuthority, digest, patchNamespace,
  PRIVATE_PREFIX as P, read, record, reviewed, reviewedOwner, template, templateDigest,
  type Execute, type Json, type NamespaceReview, type PrivateActivation, type ReviewedObject,
} from "./private-activation.js";
import { replicaIntent } from "./private-activation-retirement.js";

const HISTORY = "kars.azure.com/private-root-retirement";
const ADMIN = "router-services-admin";
const VERSION = "kars.azure.com/services-credential-version";
const SOURCE = "kars.azure.com/sandbox-uid";
const NS = "kars.azure.com/namespace-uid";
const failure = "Late private runtime retirement changed or is unsupported; preserve the runtime and re-preview its original review";
const phases = ["Pausing", "Retired", "Rotating", "Restoring", "Qualified"] as const;
type Phase = typeof phases[number];
interface Runtime {
  sandbox: ReviewedObject;
  workspace: string;
  spec: string;
  owners: string;
  generation: number;
  suspended: boolean | null;
  task?: { object: ReviewedObject; spec: string; generation: number; authorization: string };
}
interface Material { object: ReviewedObject; key: string }
interface Receipt {
  version: 4;
  root: string;
  binding: string;
  attempt: string;
  phase: Phase;
  runtime: Runtime;
  deployment: ReviewedObject;
  structure: string;
  replicas: number;
  captured: string[];
  baseline: Material;
  epoch?: string;
  qualified?: { binding: string; template: string; material: Material };
}

function items(value: unknown): Json[] {
  if (!Array.isArray(value)) throw new Error(failure);
  return value as Json[];
}
function text(value: unknown): string {
  if (typeof value !== "string" || !value.length || value.length > 253) throw new Error(failure);
  return value;
}
function hash(value: unknown): string {
  if (typeof value !== "string" || !/^[a-f0-9]{64}$/.test(value)) throw new Error(failure);
  return value;
}
function taskAuthorization(value: unknown): string {
  if (typeof value !== "string" || value.length !== 71 || !/^sha256:[a-f0-9]{64}$/.test(value)) {
    throw new Error("Task authorization must retain its exact production sha256: digest");
  }
  return value;
}
function generation(value: unknown): number {
  const result = at(value, "metadata", "generation");
  if (typeof result !== "number" || !Number.isSafeInteger(result) || result < 1) throw new Error(failure);
  return result;
}
function binding(scope: NamespaceReview): string {
  return digest({ namespace: { name: scope.namespace.name, uid: scope.namespace.uid },
    consumers: scope.consumers.map(c => ({ kind: c.kind, name: c.object.name,
      uid: c.object.uid, templateDigest: c.templateDigest })) });
}
function structure(deployment: unknown): string {
  const spec = structuredClone(record(at(deployment, "spec")));
  delete spec.replicas;
  const meta = record(at(spec, "template", "metadata"));
  const fields = record(meta.annotations ?? {});
  delete fields[`${P}epoch`];
  delete fields[VERSION];
  if (Object.keys(fields).length) meta.annotations = fields;
  else delete meta.annotations;
  return digest(spec);
}
function sandboxSpec(sandbox: unknown): string {
  const spec = structuredClone(record(at(sandbox, "spec")));
  delete spec.suspended;
  return digest(spec);
}
function suspended(sandbox: unknown): boolean | null {
  const value = at(sandbox, "spec", "suspended");
  if (value !== undefined && value !== null && typeof value !== "boolean") throw new Error(failure);
  return value ?? null;
}
function encoded(receipt: Receipt): string {
  const value = canonical(receipt);
  if (Buffer.byteLength(value) > 131_072) throw new Error("Late private retirement exceeds its bounded receipt size");
  return value;
}

function receipt(namespace: unknown): Receipt | undefined {
  const raw = at(namespace, "metadata", "annotations", HISTORY);
  if (raw === undefined) return undefined;
  if (typeof raw !== "string" || Buffer.byteLength(raw) > 131_072) throw new Error(failure);
  const value = record(JSON.parse(raw));
  if (value.version !== 4) return undefined;
  if (Object.keys(value).some(k => !["version", "root", "binding", "attempt", "phase", "runtime",
    "deployment", "structure", "replicas", "captured", "baseline", "epoch", "qualified"].includes(k))
    || !phases.includes(value.phase as Phase)) throw new Error(failure);
  for (const key of ["root", "binding", "attempt", "structure"]) hash(value[key]);
  const runtime = record(value.runtime);
  if (Object.keys(runtime).some(k => !["sandbox", "workspace", "spec", "owners", "generation", "suspended", "task"].includes(k))) throw new Error(failure);
  for (const key of ["spec", "owners"]) hash(runtime[key]);
  text(runtime.workspace);
  if (runtime.suspended !== null && typeof runtime.suspended !== "boolean") throw new Error(failure);
  for (const object of [runtime.sandbox, value.deployment, at(value.baseline, "object")]) reviewed({ metadata: object });
  generation({ metadata: runtime });
  hash(at(value.baseline, "key"));
  if (runtime.task !== undefined) {
    const task = record(runtime.task);
    reviewed({ metadata: task.object });
    hash(task.spec); taskAuthorization(task.authorization);
    generation({ metadata: task });
  }
  replicaIntent({ spec: { replicas: value.replicas } });
  items(value.captured).forEach(text);
  if (value.epoch !== undefined) hash(value.epoch);
  if (["Rotating", "Restoring", "Qualified"].includes(String(value.phase)) !== (value.epoch !== undefined)) throw new Error(failure);
  if (["Restoring", "Qualified"].includes(String(value.phase)) !== (value.qualified !== undefined)) throw new Error(failure);
  if (value.qualified !== undefined) {
    hash(at(value.qualified, "binding")); hash(at(value.qualified, "template"));
    hash(at(value.qualified, "material", "key"));
    reviewed({ metadata: at(value.qualified, "material", "object") });
    if (at(value.qualified, "material", "key") === at(value.baseline, "key")
      || at(value.qualified, "material", "object", "uid") !== at(value.baseline, "object", "uid")) throw new Error(failure);
  }
  const result = value as unknown as Receipt;
  if (encoded(result) !== raw) throw new Error(failure);
  return result;
}

async function namespaceFor(execute: Execute, scope: NamespaceReview): Promise<ReturnType<typeof record>> {
  const namespace = await read(execute, "namespace", scope.namespace.name);
  if (reviewed(namespace).uid !== scope.namespace.uid) throw new Error(failure);
  return namespace;
}

async function runtimeFor(execute: Execute, scope: NamespaceReview, namespace: unknown, deployment: unknown): Promise<Runtime> {
  const fields = record(at(namespace, "metadata", "annotations"));
  const name = text(fields["kars.azure.com/sandbox-name"]);
  const workspace = text(fields["kars.azure.com/sandbox-namespace"]);
  const sandbox = await read(execute, "karssandbox", name, workspace);
  const identity = reviewed(sandbox);
  const sourceUid = at(deployment, "metadata", "annotations", "kars.azure.com/credential-sandbox-uid");
  const namespaceUid = at(deployment, "metadata", "annotations", "kars.azure.com/credential-namespace-uid");
  const authored = items(at(deployment, "metadata", "managedFields") ?? []).some(field =>
    at(field, "manager") === "kars-controller/karssandbox" && at(field, "operation") === "Apply"
    && at(field, "fieldsV1", "f:spec") !== undefined);
  if (at(sandbox, "apiVersion") !== "kars.azure.com/v1alpha1" || at(sandbox, "kind") !== "KarsSandbox"
    || fields["kars.azure.com/namespace-claim-version"] !== "v1"
    || fields["kars.azure.com/namespace-prestage"] !== undefined
    || fields[SOURCE] !== identity.uid || at(sandbox, "metadata", "annotations", NS) !== scope.namespace.uid
    || scope.namespace.name !== `kars-${name}` || reviewed(deployment).name !== name
    || items(at(namespace, "metadata", "ownerReferences") ?? []).length
    || items(at(deployment, "metadata", "ownerReferences") ?? []).length
    || at(deployment, "metadata", "namespace") !== scope.namespace.name
    || (sourceUid !== undefined && sourceUid !== identity.uid)
    || (namespaceUid !== undefined && namespaceUid !== scope.namespace.uid)
    || (!authored && (sourceUid !== identity.uid || namespaceUid !== scope.namespace.uid))
    || at(deployment, "metadata", "labels", "kars.azure.com/sandbox") !== name
    || at(deployment, "metadata", "labels", "kars.azure.com/component") !== "sandbox"
    || at(sandbox, "spec", "githubBinding") != null
    || at(sandbox, "metadata", "annotations", "kars.azure.com/github-grant-uid") !== undefined
    || at(sandbox, "metadata", "annotations", "kars.azure.com/credential-rebind-task-uid") !== undefined
    || at(sandbox, "status", "serviceObservation") != null) throw new Error(failure);
  const owners = items(at(sandbox, "metadata", "ownerReferences") ?? []);
  let task: Runtime["task"];
  if (owners.length) {
    const owner = record(owners[0]);
    if (owners.length !== 1 || owner.apiVersion !== "kars.azure.com/v1alpha1"
      || owner.kind !== "KarsTask" || owner.controller !== true) throw new Error(failure);
    const current = await read(execute, "karstask", text(owner.name), workspace);
    const taskIdentity = reviewed(current);
    const currentGeneration = generation(current);
    const authorization = taskAuthorization(at(current, "status", "envelopeDigest"));
    const router = items(at(template(deployment), "spec", "containers")).find(c => at(c, "name") === "inference-router");
    const env = items(at(router, "env") ?? []).filter(e => at(e, "name") === "KARS_SERVICE_IDENTITY_JSON");
    const raw = at(env[0], "value");
    const configured = env.length === 1 && typeof raw === "string" && raw.length <= 131_072
      ? record(JSON.parse(raw)) : {};
    if (taskIdentity.uid !== owner.uid || at(current, "spec", "execution", "launch") !== true
      || at(current, "metadata", "annotations", "kars.azure.com/credential-rebind-pending") !== undefined
      || at(current, "status", "phase") !== "Ready" || at(current, "status", "observedGeneration") !== currentGeneration
      || at(current, "status", "sandboxRef", "name") !== name
      || at(configured, "task", "uid") !== taskIdentity.uid
      || configured.task_authorization !== authorization || configured.task_generation !== currentGeneration
      || !items(at(current, "status", "conditions") ?? []).some(c => at(c, "type") === "Ready" && at(c, "status") === "True")) throw new Error(failure);
    task = { object: taskIdentity, spec: digest({ spec: current.spec, owners: at(current, "metadata", "ownerReferences") ?? [] }),
      generation: currentGeneration, authorization };
  }
  return { sandbox: identity, workspace, spec: sandboxSpec(sandbox), owners: digest(owners),
    generation: generation(sandbox), suspended: suspended(sandbox), ...(task ? { task } : {}) };
}

function sameRuntime(current: Runtime, original: Runtime, phase: Phase): void {
  if (current.sandbox.uid !== original.sandbox.uid || current.workspace !== original.workspace
    || current.spec !== original.spec || current.owners !== original.owners
    || (current.task?.object.uid ?? "") !== (original.task?.object.uid ?? "")
    || current.task?.spec !== original.task?.spec || current.task?.generation !== original.task?.generation
    || current.task?.authorization !== original.task?.authorization) throw new Error(failure);
  const allowed = phase === "Pausing" || phase === "Restoring"
    ? [original.suspended, true] : [phase === "Qualified" ? original.suspended : true];
  if (!allowed.includes(current.suspended)) throw new Error(failure);
  const expectedGeneration = original.generation + (original.suspended === true ? 0
    : current.suspended === true ? 1 : phase === "Restoring" || phase === "Qualified" ? 2 : 0);
  if (current.generation !== expectedGeneration) throw new Error(failure);
}

async function inventory(execute: Execute, scope: NamespaceReview): Promise<Json[]> {
  const result = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
  if (at(result, "metadata", "continue")) throw new Error("Late private retirement inventory is incomplete");
  const pods = items(result.items);
  for (const pod of pods) {
    reviewed(pod, true);
    if (!await reviewedOwner(execute, pod, scope)) throw new Error("Unreviewed late private consumer preserved");
  }
  return pods;
}

async function secretMetadata(execute: Execute, scope: NamespaceReview, name: string): Promise<ReturnType<typeof record> | undefined> {
  const raw = await execute(["get", "secret", name, "-n", scope.namespace.name, "--ignore-not-found",
    "-o", "go-template={{json .metadata}}"]);
  if (!raw.trim()) return undefined;
  return record({ metadata: JSON.parse(raw) });
}
function ownedMaterial(secret: unknown, scope: NamespaceReview, runtime: Runtime): ReviewedObject {
  const identity = reviewed(secret);
  if (identity.name !== ADMIN || at(secret, "metadata", "namespace") !== scope.namespace.name
    || at(secret, "metadata", "labels", "app.kubernetes.io/managed-by") !== "kars-controller"
    || at(secret, "metadata", "annotations", SOURCE) !== runtime.sandbox.uid
    || at(secret, "metadata", "annotations", NS) !== scope.namespace.uid
    || items(at(secret, "metadata", "ownerReferences") ?? []).length) throw new Error("Late private credential provenance is missing or conflicting");
  return identity;
}
async function materialInventory(execute: Execute, scope: NamespaceReview, runtime: Runtime): Promise<ReviewedObject> {
  let admin: ReviewedObject | undefined;
  for (const name of items(bundleDefinition().secrets).map(text)) {
    const secret = await secretMetadata(execute, scope, name);
    if (!secret) continue;
    if (name !== ADMIN) throw new Error("Existing observer, TLS or App private material requires its owner-specific rotation; late admin-only enrollment preserved it");
    admin = ownedMaterial(secret, scope, runtime);
  }
  if (!admin) throw new Error("Late private runtime requires its existing controller-owned admin credential; missing material was not adopted");
  return admin;
}
async function material(execute: Execute, scope: NamespaceReview, runtime: Runtime): Promise<Material> {
  const secret = await read(execute, "secret", ADMIN, scope.namespace.name);
  const object = ownedMaterial(secret, scope, runtime);
  const data = record(secret.data);
  const token = typeof data["control-token"] === "string" ? Buffer.from(data["control-token"], "base64") : Buffer.alloc(0);
  if (secret.type !== "Opaque" || Object.keys(data).join(",") !== "control-token"
    || token.length !== 64 || token.toString("base64") !== data["control-token"]
    || !/^[a-zA-Z0-9]{64}$/.test(token.toString("utf8"))) {
    throw new Error("Customized or missing late private credential keys require explicit operator recovery");
  }
  return { object, key: digest(token.toString("base64")) };
}

function supportedTemplate(deployment: unknown, scope: NamespaceReview, activation: PrivateActivation): void {
  const current = structuredClone(record(deployment));
  const pod = record(template(current).spec);
  let admin = false;
  const removed = new Set<string>();
  pod.volumes = items(pod.volumes ?? []).filter(volume => {
    if (at(volume, "secret", "secretName") === ADMIN) {
      if (canonical(at(volume, "secret", "items")) !== canonical([{ key: "control-token", path: "control-token" }])
        || at(volume, "secret", "optional") === true || admin
        || at(volume, "name") !== "governed-services-control") throw new Error(failure);
      admin = true;
      removed.add(text(at(volume, "name")));
      return false;
    }
    // The controller's legacy optional App mount has no authority when its
    // Secret is absent. materialInventory rejects any existing App material.
    if (at(volume, "secret", "secretName") === "router-github-app" && at(volume, "secret", "optional") === true) {
      removed.add(text(at(volume, "name")));
      return false;
    }
    return true;
  });
  for (const container of [...items(pod.containers ?? []), ...items(pod.initContainers ?? []), ...items(pod.ephemeralContainers ?? [])]) {
    const c = record(container);
    for (const mount of items(c.volumeMounts ?? []).filter(m => at(m, "name") === "governed-services-control")) {
      if (c.name !== "inference-router" || canonical(mount) !== canonical({
        name: "governed-services-control", mountPath: "/etc/kars/services", readOnly: true,
      })) throw new Error("Customized admin credential mount requires explicit recovery");
    }
    c.volumeMounts = items(c.volumeMounts ?? []).filter(mount => !removed.has(String(at(mount, "name"))));
  }
  if (!admin || consumesPrivateAuthority(current, scope.namespace.name, activation)) {
    throw new Error("Late enrollment only retires the reviewed runtime's controller-owned admin token, not host access, privileged tokens or other private authority");
  }
}

async function current(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, root: string, state?: Receipt,
): Promise<{ runtime: Runtime; deployment: ReturnType<typeof record>; pods: Json[]; namespace: ReturnType<typeof record> }> {
  if (scope.consumers.length !== 1 || scope.consumers[0]?.kind !== "Deployment") throw new Error(failure);
  const consumer = scope.consumers[0];
  const namespace = await namespaceFor(execute, scope);
  const deployment = await read(execute, "deployments.apps", consumer.object.name, scope.namespace.name);
  if (reviewed(deployment).uid !== consumer.object.uid) throw new Error(failure);
  const runtime = await runtimeFor(execute, scope, namespace, deployment);
  if (state) {
    sameRuntime(runtime, state.runtime, state.phase);
    if (state.root !== root || state.deployment.uid !== consumer.object.uid || structure(deployment) !== state.structure
      || encoded(receipt(namespace)!) !== encoded(state)
      || (state.epoch !== undefined && scope.epoch !== undefined && scope.epoch !== state.epoch)) throw new Error(failure);
    if (["Pausing", "Retired"].includes(state.phase) && binding(scope) !== state.binding) throw new Error(failure);
    const replicas = replicaIntent(deployment);
    if (!(["Pausing", "Restoring", "Qualified"].includes(state.phase) ? [0, state.replicas] : [0]).includes(replicas)) throw new Error(failure);
    if (state.phase === "Qualified" && (replicas !== state.replicas
      || binding(scope) !== state.qualified?.binding || templateDigest(deployment) !== state.qualified.template)) throw new Error(failure);
    if (state.epoch) {
      if (at(namespace, "metadata", "annotations", `${P}epoch`) !== state.epoch
        || Object.entries(annotations(activation, scope, "Qualified")).some(([k, v]) => at(namespace, "metadata", "annotations", k) !== v)) throw new Error(failure);
    } else if (Object.entries(annotations(activation, scope, "Pending")).some(([k, v]) => at(namespace, "metadata", "annotations", k) !== v)
      || at(namespace, "metadata", "annotations", `${P}epoch`) !== undefined) throw new Error(failure);
  } else {
    if (templateDigest(deployment) !== consumer.templateDigest
      || reviewed(deployment).resourceVersion !== consumer.object.resourceVersion
      || at(deployment, "status", "observedGeneration") !== generation(deployment)
      || replicaIntent(deployment) !== (runtime.suspended === true ? 0 : 1)
      || at(template(deployment), "metadata", "annotations", `${P}epoch`) !== undefined) throw new Error(failure);
    const sandbox = await read(execute, "karssandbox", runtime.sandbox.name, runtime.workspace);
    if (reviewed(sandbox).resourceVersion !== runtime.sandbox.resourceVersion
      || at(sandbox, "status", "observedGeneration") !== runtime.generation
      || at(sandbox, "status", "phase") !== "Running"
      || !items(at(sandbox, "status", "conditions") ?? []).some(condition =>
        at(condition, "type") === "Ready" && at(condition, "status") === "True"
        && at(condition, "observedGeneration") === runtime.generation)) throw new Error(failure);
  }
  supportedTemplate(deployment, scope, activation);
  const secret = await materialInventory(execute, scope, runtime);
  if (state && secret.uid !== state.baseline.object.uid) throw new Error(failure);
  if (state?.qualified && canonical(secret) !== canonical(state.qualified.material.object)) throw new Error(failure);
  if (!state && at(template(deployment), "metadata", "annotations", VERSION) !== `${secret.uid}:${secret.resourceVersion}`) {
    throw new Error("Reviewed runtime has not consumed its current controller-owned admin credential version");
  }
  // During controller rotation only the exact token-version annotation and
  // private epoch may change, never the reviewed executable Pod specification.
  const liveScope = { ...scope, consumers: [{ ...consumer, templateDigest: templateDigest(deployment) }] };
  const pods = await inventory(execute, liveScope);
  if (state && state.phase !== "Pausing" && state.phase !== "Qualified" && state.phase !== "Restoring" && pods.length) throw new Error(failure);
  if (state && ["Qualified", "Restoring"].includes(state.phase) && pods.some(pod =>
    state.captured.includes(reviewed(pod, true).uid) || at(pod, "metadata", "annotations", `${P}epoch`) !== state.epoch
    || at(pod, "metadata", "annotations", VERSION) !== `${state.qualified!.material.object.uid}:${state.qualified!.material.object.resourceVersion}`)) throw new Error(failure);
  return { runtime, deployment, pods, namespace };
}

/** Read-only. Public activation JSON remains v1; recovery lives only in the existing operator-only namespace field. */
export async function reviewLateScope(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, root: string,
): Promise<"Late" | "Qualified" | undefined> {
  const namespace = await namespaceFor(execute, scope);
  const state = receipt(namespace);
  if (!state) {
    if (at(namespace, "metadata", "annotations", HISTORY) !== undefined) return undefined;
    if (at(namespace, "metadata", "annotations", "kars.azure.com/sandbox-name") === undefined) return undefined;
    if (!scope.consumers.some(c => c.kind === "Deployment")) return undefined;
    const deployment = await read(execute, "deployments.apps", scope.consumers[0]!.object.name, scope.namespace.name);
    if (!consumesPrivateAuthority(deployment, scope.namespace.name, activation)) return undefined;
  }
  await current(execute, activation, scope, root, state);
  if (state?.phase === "Qualified") {
    scope.epoch = state.epoch;
    return "Qualified";
  }
  return "Late";
}

export async function stageLateScope(
  execute: Execute, activation: PrivateActivation, scope: NamespaceReview, root: string, assertRoot: () => Promise<void>,
): Promise<void> {
  let state = receipt(await namespaceFor(execute, scope));
  let live = await current(execute, activation, scope, root, state);
  const save = async (next: Receipt, fields: Record<string, string> = {}) => {
    await assertRoot();
    await patchNamespace(execute, scope, { ...fields, [HISTORY]: encoded(next) },
      { [HISTORY]: state ? encoded(state) : undefined }, true);
    state = next;
  };
  if (!state) {
    const baseline = await material(execute, scope, live.runtime);
    await save({ version: 4, root, binding: binding(scope), attempt: randomBytes(32).toString("hex"), phase: "Pausing",
      runtime: live.runtime, deployment: reviewed(live.deployment), structure: structure(live.deployment),
      replicas: replicaIntent(live.deployment), captured: live.pods.map(p => reviewed(p, true).uid).sort(), baseline },
    annotations(activation, scope, "Pending"));
  }
  if (!state) throw new Error(failure);
  const deadline = Date.now() + 120_000;
  if (state.phase === "Pausing") {
    live = await current(execute, activation, scope, root, state);
    if (live.runtime.suspended !== true) {
      await assertRoot();
      await execute(["patch", "karssandbox", state.runtime.sandbox.name, "-n", state.runtime.workspace, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: live.runtime.sandbox.uid, resourceVersion: live.runtime.sandbox.resourceVersion },
          spec: { suspended: true } })]);
    }
    live = await current(execute, activation, scope, root, state);
    if (replicaIntent(live.deployment) !== 0) {
      await assertRoot();
      await execute(["patch", "deployments.apps", state.deployment.name, "-n", scope.namespace.name, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: state.deployment.uid, resourceVersion: reviewed(live.deployment).resourceVersion }, spec: { replicas: 0 } })]);
    }
    for (;;) {
      live = await current(execute, activation, scope, root, state);
      const captured = [...new Set([...state.captured, ...live.pods.map(p => reviewed(p, true).uid)])].sort();
      if (canonical(captured) !== canonical(state.captured)) await save({ ...state, captured });
      if (!live.pods.length && replicaIntent(live.deployment) === 0) break;
      if (Date.now() >= deadline) throw new Error("Late private Pods, including terminating UIDs, remain; runtime suspension and recovery were preserved");
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    if ((await material(execute, scope, live.runtime)).key !== state.baseline.key) throw new Error("Late private key changed before retirement; no pre-retirement rotation was qualified");
    await save({ ...state, phase: "Retired" });
  }
  if (state.phase === "Retired") {
    await current(execute, activation, scope, root, state);
    const epoch = randomBytes(32).toString("hex");
    const budget = activation.root.budgetTls;
    scope.epoch = epoch;
    await save({ ...state, phase: "Rotating", epoch }, {
      ...annotations(activation, scope, "Qualified"), [`${P}epoch`]: epoch,
      [`${P}parent-${state.deployment.uid}`]: epoch,
      ...(budget ? {
        [`${P}budget-qualified-bundle`]: activation.bundleRevision,
        [`${P}budget-qualified-key`]: budget.keyDigest, [`${P}budget-qualified-secret`]: budget.secret.uid,
        [`${P}budget-rotation-bundle`]: "", [`${P}budget-before-key`]: "",
      } : {}),
    });
  }
  scope.epoch = state.epoch;
  if (state.phase === "Rotating") {
    for (;;) {
      live = await current(execute, activation, scope, root, state);
      const fresh = await material(execute, scope, live.runtime);
      const metadata = await secretMetadata(execute, scope, ADMIN);
      const version = `${fresh.object.uid}:${fresh.object.resourceVersion}`;
      if (fresh.object.uid !== state.baseline.object.uid) throw new Error(failure);
      if (at(metadata, "metadata", "annotations", `${P}epoch`) === state.epoch
        && at(metadata, "metadata", "annotations", "kars.azure.com/services-credential-retired") === undefined
        && at(template(live.deployment), "metadata", "annotations", `${P}epoch`) === state.epoch
        && at(template(live.deployment), "metadata", "annotations", VERSION) === version) {
        if (fresh.key === state.baseline.key || fresh.object.resourceVersion === state.baseline.object.resourceVersion) {
          throw new Error("Late private authentication key was not rotated; epoch/version stamps alone cannot qualify");
        }
        scope.consumers[0]!.templateDigest = templateDigest(live.deployment);
        await save({ ...state, phase: "Restoring", qualified: {
          binding: binding(scope), template: scope.consumers[0]!.templateDigest, material: fresh,
        } });
        break;
      }
      if (Date.now() >= deadline) throw new Error("Controller has not reissued the retired private key and exact template; runtime remains suspended for re-preview");
      await new Promise(resolve => setTimeout(resolve, 500));
    }
  }
  if (state.phase === "Restoring") {
    live = await current(execute, activation, scope, root, state);
    if (canonical(await material(execute, scope, live.runtime)) !== canonical(state.qualified!.material)) throw new Error(failure);
    scope.consumers[0]!.templateDigest = state.qualified!.template;
    if (live.runtime.suspended !== state.runtime.suspended) {
      await assertRoot();
      await execute(["patch", "karssandbox", state.runtime.sandbox.name, "-n", state.runtime.workspace, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: live.runtime.sandbox.uid, resourceVersion: live.runtime.sandbox.resourceVersion },
          spec: { suspended: state.runtime.suspended } })]);
    }
    for (;;) {
      live = await current(execute, activation, scope, root, state);
      if (replicaIntent(live.deployment) === state.replicas
        && at(live.deployment, "status", "observedGeneration") === generation(live.deployment)
        && (state.replicas === 0 || (at(live.deployment, "status", "updatedReplicas") === state.replicas
          && at(live.deployment, "status", "availableReplicas") === state.replicas))) break;
      if (Date.now() >= deadline) throw new Error("Late private restore is incomplete; original intent and new authority remain recorded for re-preview");
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    await save({ ...state, phase: "Qualified" });
  }
  await current(execute, activation, scope, root, state);
}
