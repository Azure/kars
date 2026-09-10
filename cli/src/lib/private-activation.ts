// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { requireBundledAsset } from "./repo-assets.js";

export type Execute = (args: string[], input?: string) => Promise<string>;
type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type RecordValue = { [key: string]: Json };
export interface ReviewedObject { name: string; uid: string; resourceVersion: string }
export interface ReviewedConsumer { kind: string; object: ReviewedObject; templateDigest: string }
export interface NamespaceReview { namespace: ReviewedObject; consumers: ReviewedConsumer[]; epoch?: string }
export interface PrivateActivation {
  contract: string;
  phase: "reviewed" | "qualified";
  bundleRevision: string;
  root: { namespace: ReviewedObject; account: ReviewedObject; deployment: ReviewedObject; templateDigest: string };
  profile: "service-accounts" | "kcm-certificate";
  controllerUids: Record<string, string>;
  namespaces: NamespaceReview[];
}

export const PRIVATE_PREFIX = "kars.azure.com/private-";
export const PRIVATE_CONTRACT = "kars.azure.com/private-consumption/v1";
const grantResource = "karscredentialgrants.kars.azure.com";
const kinds: Record<string, string> = {
  Deployment: "deployments.apps", ReplicaSet: "replicasets.apps",
  StatefulSet: "statefulsets.apps", DaemonSet: "daemonsets.apps",
  ReplicationController: "replicationcontrollers", Job: "jobs.batch", CronJob: "cronjobs.batch", Pod: "pods",
};

export function record(value: unknown): RecordValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Private activation metadata is malformed");
  return value as RecordValue;
}

function list(value: unknown): Json[] {
  if (!Array.isArray(value)) throw new Error("Private activation inventory is malformed");
  return value as Json[];
}

function at(value: unknown, ...keys: string[]): Json | undefined {
  let current: unknown = value;
  for (const key of keys) {
    if (!current || typeof current !== "object" || Array.isArray(current)) return undefined;
    current = (current as Record<string, unknown>)[key];
  }
  return current as Json | undefined;
}

export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const fields = value as Record<string, unknown>;
    return `{${Object.keys(fields).sort().map(key => `${JSON.stringify(key)}:${canonical(fields[key])}`).join(",")}}`;
  }
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new Error("Private activation JSON is incomplete");
  return encoded;
}

export function digest(value: unknown): string {
  return createHash("sha256").update(canonical(value)).digest("hex");
}

export function reviewed(value: unknown, terminating = false): ReviewedObject {
  const meta = record(at(value, "metadata"));
  if ((!terminating && meta.deletionTimestamp) || ![meta.name, meta.uid, meta.resourceVersion].every(v => typeof v === "string" && v.length > 0)) {
    throw new Error("Private activation requires live API UID/resourceVersion identities");
  }
  return { name: meta.name as string, uid: meta.uid as string, resourceVersion: meta.resourceVersion as string };
}

export async function read(execute: Execute, kind: string, name: string, namespace?: string): Promise<RecordValue> {
  const value = record(JSON.parse(await execute(["get", kind, name, ...(namespace ? ["-n", namespace] : []), "-o", "json"])));
  reviewed(value);
  return value;
}

export function template(value: unknown): RecordValue {
  const kind = at(value, "kind");
  if (kind === "Pod" || (kind === undefined && Array.isArray(at(value, "spec", "containers")))) {
    return { metadata: { labels: at(value, "metadata", "labels") ?? {}, annotations: at(value, "metadata", "annotations") ?? {} },
      spec: record(at(value, "spec")) };
  }
  return record(kind === "CronJob" ? at(value, "spec", "jobTemplate", "spec", "template") : at(value, "spec", "template"));
}

export function templateDigest(value: unknown): string {
  const current = structuredClone(template(value));
  const annotations = at(current, "metadata", "annotations");
  if (annotations) {
    for (const key of Object.keys(record(annotations))) {
      if (key === `${PRIVATE_PREFIX}epoch`) delete record(annotations)[key];
    }
    if (!Object.keys(record(annotations)).length) delete record(current.metadata).annotations;
  }
  return digest(current);
}

export function bundleDefinition(): RecordValue {
  return record(JSON.parse(readFileSync(requireBundledAsset("deploy/helm/kars/files/private-consumption.json"), "utf8")));
}

export async function verifyPrivateBundle(execute: Execute): Promise<string> {
  const identities: { kind: string; name: string; uid: string; resourceVersion: string }[] = [];
  for (const definition of list(bundleDefinition().objects)) {
    const kind = at(definition, "kind");
    const name = at(definition, "metadata", "name");
    if (typeof kind !== "string" || typeof name !== "string") throw new Error("Private admission bundle is invalid");
    const current = await read(execute, kind.toLowerCase(), name);
    if (canonical(current.spec) !== canonical(at(definition, "spec"))) {
      throw new Error("Private admission differs from the complete required bundle; upgrade core prerequisites before enrollment");
    }
    if (kind === "ValidatingAdmissionPolicy"
      && (at(current, "status", "observedGeneration") !== at(current, "metadata", "generation")
        || !at(current, "status", "typeChecking")
        || list(at(current, "status", "typeChecking", "expressionWarnings") ?? []).length !== 0)) {
      throw new Error("Private admission is not currently observed and type-checked");
    }
    identities.push({ kind, ...reviewed(current) });
  }
  return digest(identities);
}

export async function previewPrivateActivation(
  execute: Execute, workspace: string, writers: { namespace: string }[], targets: { name: string }[],
  rootNamespace: string, profile: string, consumers: string[],
): Promise<PrivateActivation> {
  if (!rootNamespace || !["service-accounts", "kcm-certificate"].includes(profile)) {
    throw new Error("Re-preview private enrollment with --private-root and an explicit --private-controller-profile");
  }
  const bundleRevision = await verifyPrivateBundle(execute);
  const rootNs = await read(execute, "namespace", rootNamespace);
  const deployment = await read(execute, "deployment", "kars-controller", rootNamespace);
  const accountName = at(deployment, "spec", "template", "spec", "serviceAccountName");
  if (accountName !== "kars-controller") throw new Error("Private activation requires the explicitly supported controller identity");
  const account = await read(execute, "serviceaccount", accountName, rootNamespace);
  const controllerUids: Record<string, string> = {};
  if (profile === "service-accounts") {
    for (const name of list(bundleDefinition().controllers)) {
      if (typeof name !== "string") throw new Error("Private controller profile is invalid");
      controllerUids[name] = reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid;
    }
  }
  const names = new Set([workspace, rootNamespace, ...writers.map(w => w.namespace),
    ...targets.map(t => `kars-${t.name}`)]);
  const namespaces: NamespaceReview[] = [];
  const requested = [...consumers, `${rootNamespace}/Deployment/kars-controller`];
  for (const raw of requested) {
    const [namespace, kind, name, ...extra] = raw.split("/");
    if (!namespace || !kind || !name || extra.length || !kinds[kind]) throw new Error("Private consumer review must be namespace/Kind/name");
    if (!names.has(namespace)) {
      await verifyOwnedRuntimeNamespace(execute, workspace, namespace);
      names.add(namespace);
    }
  }
  for (const name of names) {
    const namespace = reviewed(await read(execute, "namespace", name));
    const approved: ReviewedConsumer[] = [];
    for (const raw of new Set(requested)) {
      const [ns, kind, resourceName, ...extra] = raw.split("/");
      if (!ns || !kind || !resourceName || extra.length || !kinds[kind]) throw new Error("Private consumer review must be namespace/Kind/name");
      if (!names.has(ns)) throw new Error("Private consumer lies outside the activation's protected namespaces");
      if (ns !== name) continue;
      const current = await read(execute, kinds[kind], resourceName, name);
      approved.push({ kind, object: reviewed(current), templateDigest: templateDigest(current) });
    }
    namespaces.push({ namespace, consumers: approved });
  }
  return {
    contract: PRIVATE_CONTRACT, phase: "reviewed", bundleRevision,
    root: { namespace: reviewed(rootNs), account: reviewed(account), deployment: reviewed(deployment), templateDigest: templateDigest(deployment) },
    profile: profile as PrivateActivation["profile"], controllerUids, namespaces,
  };
}

export async function verifyOwnedRuntimeNamespace(execute: Execute, workspace: string, namespace: string): Promise<void> {
  const ns = await read(execute, "namespace", namespace);
  const annotations = record(at(ns, "metadata", "annotations"));
  const name = annotations["kars.azure.com/sandbox-name"];
  if (annotations["kars.azure.com/sandbox-namespace"] !== workspace || typeof name !== "string"
    || namespace !== `kars-${name}`) throw new Error("Additional private namespace is not owned by this workspace");
  const sandbox = await read(execute, "karssandbox", name, workspace);
  if (reviewed(sandbox).uid !== annotations["kars.azure.com/sandbox-uid"]
    || at(sandbox, "metadata", "annotations", "kars.azure.com/namespace-uid") !== reviewed(ns).uid) {
    throw new Error("Additional private runtime namespace incarnation changed");
  }
}

export async function validatePrivateActivation(execute: Execute, activation: PrivateActivation): Promise<void> {
  if (activation?.contract !== PRIVATE_CONTRACT || activation.phase !== "reviewed"
    || !Array.isArray(activation.namespaces) || !activation.namespaces.length || activation.namespaces.length > 64) {
    throw new Error("A reviewed private activation is required; regenerate grant preview with --private-root");
  }
  const exact = (value: unknown, allowed: string[]) => {
    if (Object.keys(record(value)).some(key => !allowed.includes(key))) throw new Error("Private activation accepts only canonical reviewed metadata");
  };
  const identityShape = (value: unknown) => {
    exact(value, ["name", "uid", "resourceVersion"]);
    const item = record(value);
    if (![item.name, item.uid, item.resourceVersion].every(v => typeof v === "string" && v.length > 0 && v.length <= 253)) {
      throw new Error("Private activation identity is malformed");
    }
  };
  exact(activation, ["contract", "phase", "bundleRevision", "root", "profile", "controllerUids", "namespaces"]);
  exact(activation.root, ["namespace", "account", "deployment", "templateDigest"]);
  for (const value of [activation.root.namespace, activation.root.account, activation.root.deployment]) identityShape(value);
  if (!/^[a-f0-9]{64}$/.test(activation.bundleRevision) || !/^[a-f0-9]{64}$/.test(activation.root.templateDigest)) {
    throw new Error("Private activation digest is malformed");
  }
  for (const scope of activation.namespaces) {
    exact(scope, ["namespace", "consumers", "epoch"]);
    identityShape(scope.namespace);
    if (!Array.isArray(scope.consumers) || scope.consumers.length > 64
      || (scope.epoch !== undefined && !/^[a-f0-9]{64}$/.test(scope.epoch))) throw new Error("Private consumer review is malformed");
    for (const consumer of scope.consumers) {
      exact(consumer, ["kind", "object", "templateDigest"]);
      identityShape(consumer.object);
      if (!/^[a-f0-9]{64}$/.test(consumer.templateDigest)) throw new Error("Private consumer digest is malformed");
    }
  }
  if ((await execute(["auth", "can-i", "manage", `${grantResource}/workspace`, "--all-namespaces"])).trim() !== "yes") {
    throw new Error("Private activation staging requires the existing cluster-scoped credential operator authority");
  }
  if (await verifyPrivateBundle(execute) !== activation.bundleRevision) throw new Error("Private admission changed since review");
  const root = activation.root;
  for (const [kind, expected, namespace] of [
    ["namespace", root.namespace, undefined], ["serviceaccount", root.account, root.namespace.name],
    ["deployment", root.deployment, root.namespace.name],
  ] as const) {
    const current = await read(execute, kind, expected.name, namespace);
    if (reviewed(current).uid !== expected.uid
      || (kind === "deployment" && templateDigest(current) !== root.templateDigest)) {
      throw new Error("Reviewed private root identity or template changed");
    }
  }
  if (!["service-accounts", "kcm-certificate"].includes(activation.profile)) throw new Error("Private controller profile is invalid");
  const expectedControllers = activation.profile === "service-accounts" ? list(bundleDefinition().controllers) : [];
  if (canonical(Object.keys(activation.controllerUids).sort()) !== canonical([...expectedControllers].sort())) {
    throw new Error("Private controller profile is incomplete");
  }
  for (const name of Object.keys(activation.controllerUids)) {
    if (reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid !== activation.controllerUids[name]) {
      throw new Error("Reviewed workload-controller UID changed");
    }
  }
  const seen = new Set<string>();
  for (const scope of activation.namespaces) {
    if (seen.has(scope.namespace.name)) throw new Error("Private namespace review is duplicated");
    seen.add(scope.namespace.name);
    const current = await read(execute, "namespace", scope.namespace.name);
    if (reviewed(current).uid !== scope.namespace.uid || reviewed(current).resourceVersion !== scope.namespace.resourceVersion) {
      throw new Error("Reviewed private namespace changed");
    }
    for (const consumer of scope.consumers) {
      if (!kinds[consumer.kind]) throw new Error("Private consumer kind is unsupported");
      const object = await read(execute, kinds[consumer.kind], consumer.object.name, scope.namespace.name);
      if (reviewed(object).uid !== consumer.object.uid || templateDigest(object) !== consumer.templateDigest) {
        throw new Error("Reviewed private consumer identity or template changed");
      }
    }
  }
}

function annotations(activation: PrivateActivation, scope: NamespaceReview, state: string): Record<string, string> {
  return {
    [`${PRIVATE_PREFIX}enabled`]: "true", [`${PRIVATE_PREFIX}state`]: state,
    [`${PRIVATE_PREFIX}namespace-uid`]: scope.namespace.uid,
    [`${PRIVATE_PREFIX}root-namespace`]: activation.root.namespace.name,
    [`${PRIVATE_PREFIX}root-namespace-uid`]: activation.root.namespace.uid,
    [`${PRIVATE_PREFIX}root-account`]: activation.root.account.name,
    [`${PRIVATE_PREFIX}root-user`]: `system:serviceaccount:${activation.root.namespace.name}:${activation.root.account.name}`,
    [`${PRIVATE_PREFIX}root-uid`]: activation.root.account.uid,
    [`${PRIVATE_PREFIX}root-deployment-uid`]: activation.root.deployment.uid,
    [`${PRIVATE_PREFIX}root-deployment`]: activation.root.deployment.name,
    [`${PRIVATE_PREFIX}root-template-digest`]: activation.root.templateDigest,
    [`${PRIVATE_PREFIX}bundle-revision`]: activation.bundleRevision,
    [`${PRIVATE_PREFIX}profile`]: activation.profile,
    ...Object.fromEntries(Object.entries(activation.controllerUids).map(([name, uid]) => [`${PRIVATE_PREFIX}${name}-uid`, uid])),
  };
}

async function patchNamespace(execute: Execute, scope: NamespaceReview, fields: Record<string, string>): Promise<void> {
  const current = await read(execute, "namespace", scope.namespace.name);
  if (reviewed(current).uid !== scope.namespace.uid) throw new Error("Private namespace was replaced before staging");
  const result = record(JSON.parse(await execute(["patch", "namespace", scope.namespace.name, "--type=merge", "-p",
    JSON.stringify({ metadata: { uid: scope.namespace.uid, resourceVersion: reviewed(current).resourceVersion, annotations: fields } }), "-o", "json"])));
  if (reviewed(result).uid !== scope.namespace.uid) throw new Error("Private namespace staging returned another incarnation");
  scope.namespace.resourceVersion = reviewed(result).resourceVersion;
}

export async function stagePrivateActivation(execute: Execute, activation: PrivateActivation): Promise<PrivateActivation> {
  await validatePrivateActivation(execute, activation);
  const staged = structuredClone(activation);
  for (const scope of staged.namespaces) await patchNamespace(execute, scope, annotations(staged, scope, "Pending"));
  await validatePrivateActivation(execute, staged);
  const retire: { scope: NamespaceReview; consumer: ReviewedConsumer }[] = [];
  for (const scope of staged.namespaces) {
    const pods = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
    if (at(pods, "metadata", "continue")) throw new Error("Private consumer inventory is incomplete");
    for (const pod of list(pods.items)) {
      if (!privateConsumer(pod, scope.namespace.name, staged)) continue;
      const owner = await reviewedOwner(execute, pod, scope);
      if (!owner) throw new Error("Unexplained private consumer preserved; explicitly review its actual owner before activation");
      if (privateMaterial(template(pod).spec)) {
        if (!["Deployment", "ReplicaSet", "StatefulSet", "ReplicationController"].includes(owner.kind)) {
          throw new Error("This reviewed private consumer requires its existing owner-specific retirement before activation; it was preserved");
        }
        if (!retire.some(item => item.consumer.object.uid === owner.object.uid)) retire.push({ scope, consumer: owner });
      }
    }
  }
  for (const { scope, consumer } of retire) {
    const current = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
    if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) {
      throw new Error("Reviewed private consumer changed before retirement");
    }
    await execute(["patch", kinds[consumer.kind]!, consumer.object.name, "-n", scope.namespace.name, "--type=merge", "-p",
      JSON.stringify({ metadata: { uid: consumer.object.uid, resourceVersion: reviewed(current).resourceVersion },
        spec: { replicas: 0 } })]);
  }
  const deadline = Date.now() + 120_000;
  const preserved = new Map<string, Map<string, string>>();
  for (;;) {
    let pending = false;
    preserved.clear();
    for (const scope of staged.namespaces) {
      const inventory = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
      if (at(inventory, "metadata", "continue")) throw new Error("Private consumer retirement inventory is incomplete");
      for (const pod of list(inventory.items)) {
        if (!privateConsumer(pod, scope.namespace.name, staged)) continue;
        if (!await reviewedOwner(execute, pod, scope)) throw new Error("Unexplained private consumer preserved during retirement");
        const material = privateMaterial(template(pod).spec);
        pending ||= material;
        if (!material) {
          const entries = preserved.get(scope.namespace.name) ?? new Map<string, string>();
          entries.set(reviewed(pod, true).uid, digest(record(pod).spec));
          preserved.set(scope.namespace.name, entries);
        }
      }
    }
    if (!pending) break;
    if (Date.now() >= deadline) throw new Error("Approved private consumers have not finished retirement; protection remains enabled");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  if (await verifyPrivateBundle(execute) !== staged.bundleRevision) throw new Error("Private admission changed before epoch creation");
  for (const scope of staged.namespaces) {
    scope.epoch = randomBytes(32).toString("hex");
    await patchNamespace(execute, scope, {
      ...annotations(staged, scope, "Qualified"), [`${PRIVATE_PREFIX}epoch`]: scope.epoch,
      ...Object.fromEntries(scope.consumers.map(c => [`${PRIVATE_PREFIX}parent-${c.object.uid}`, scope.epoch!])),
      ...Object.fromEntries([...(preserved.get(scope.namespace.name) ?? [])].flatMap(([uid, spec]) => [
        [`${PRIVATE_PREFIX}pod-${uid}`, scope.epoch!], [`${PRIVATE_PREFIX}pod-spec-${uid}`, spec],
      ])),
    });
    for (const consumer of scope.consumers) {
      if (consumer.kind === "Job" || consumer.kind === "Pod") continue;
      const current = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
      if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) {
        throw new Error("Reviewed consumer changed before template qualification");
      }
      if (!privateConsumer(current, scope.namespace.name, staged)) continue;
      const marker = { metadata: { annotations: { [`${PRIVATE_PREFIX}epoch`]: scope.epoch } } };
      const spec = consumer.kind === "CronJob" ? { jobTemplate: { spec: { template: marker } } } : { template: marker };
      await execute(["patch", kinds[consumer.kind]!, consumer.object.name, "-n", scope.namespace.name, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: consumer.object.uid, resourceVersion: reviewed(current).resourceVersion }, spec })]);
    }
  }
  staged.phase = "qualified";
  return staged;
}

export async function validateQualifiedActivation(execute: Execute, activation: PrivateActivation): Promise<void> {
  if (activation.contract !== PRIVATE_CONTRACT || activation.phase !== "qualified"
    || await verifyPrivateBundle(execute) !== activation.bundleRevision) throw new Error("Private qualification changed");
  for (const [kind, identity, namespace] of [
    ["namespace", activation.root.namespace, undefined],
    ["serviceaccount", activation.root.account, activation.root.namespace.name],
    ["deployment", activation.root.deployment, activation.root.namespace.name],
  ] as const) {
    const current = await read(execute, kind, identity.name, namespace);
    if (reviewed(current).uid !== identity.uid
      || (kind === "deployment" && templateDigest(current) !== activation.root.templateDigest)) {
      throw new Error("Private root changed before qualified grant publication");
    }
  }
  for (const [name, uid] of Object.entries(activation.controllerUids)) {
    if (reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid !== uid) {
      throw new Error("Controller profile changed before qualified publication");
    }
  }
  for (const scope of activation.namespaces) {
    const current = await read(execute, "namespace", scope.namespace.name);
    const actual = record(at(current, "metadata", "annotations"));
    if (reviewed(current).uid !== scope.namespace.uid || !scope.epoch
      || actual[`${PRIVATE_PREFIX}epoch`] !== scope.epoch
      || Object.entries(annotations(activation, scope, "Qualified")).some(([key, value]) => actual[key] !== value)) {
      throw new Error("Private namespace qualification changed before grant publication");
    }
  }
}

export function privateMaterial(value: unknown): boolean {
  const pod = record(value);
  const secrets = list(bundleDefinition().secrets);
  const volumes = list(pod.volumes ?? []);
  for (const v of volumes) {
    if (secrets.includes(at(v, "secret", "secretName") ?? null)
      || secrets.includes(at(v, "csi", "nodePublishSecretRef", "name") ?? null)) return true;
    for (const source of list(at(v, "projected", "sources") ?? [])) {
      if (secrets.includes(at(source, "secret", "name") ?? null)) return true;
    }
    for (const kind of ["azureFile", "cephfs", "cinder", "flexVolume", "iscsi", "rbd", "scaleIO", "storageos"]) {
      if (secrets.includes(at(v, kind, "secretName") ?? null) || secrets.includes(at(v, kind, "secretRef", "name") ?? null)) return true;
    }
  }
  if (list(pod.imagePullSecrets ?? []).some(s => secrets.includes(at(s, "name") ?? null))) return true;
  for (const c of [...list(pod.containers ?? []), ...list(pod.initContainers ?? []), ...list(pod.ephemeralContainers ?? [])]) {
    if (list(at(c, "envFrom") ?? []).some(e => secrets.includes(at(e, "secretRef", "name") ?? null))
      || list(at(c, "env") ?? []).some(e => secrets.includes(at(e, "valueFrom", "secretKeyRef", "name") ?? null))) return true;
  }
  return false;
}

export function privateConsumer(value: unknown, namespace: string, activation: PrivateActivation): boolean {
  const pod = record(template(value).spec);
  if (at(template(value), "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== undefined) return true;
  if (privateMaterial(pod)) return true;
  const account = pod.serviceAccountName ?? "";
  const privilegedIdentity = (namespace === activation.root.namespace.name && account === activation.root.account.name)
    || (namespace === "kars-sre" && account === "sre-api-router")
    || (namespace === "kube-system" && list(bundleDefinition().controllers).includes(account));
  if (privilegedIdentity && (pod.automountServiceAccountToken !== false
    || list(pod.volumes ?? []).some(v => list(at(v, "projected", "sources") ?? []).some(s => at(s, "serviceAccountToken"))))) return true;
  return pod.hostNetwork === true || pod.hostPID === true || pod.hostIPC === true
    || list(pod.volumes ?? []).some(v => at(v, "hostPath") !== undefined)
    || [...list(pod.containers ?? []), ...list(pod.initContainers ?? []), ...list(pod.ephemeralContainers ?? [])]
      .some(c => at(c, "securityContext", "privileged") === true
        || list(at(c, "securityContext", "capabilities", "add") ?? []).some(k =>
          ["ALL", "SYS_ADMIN", "SYS_PTRACE", "SYS_MODULE", "SYS_RAWIO", "BPF", "PERFMON", "CHECKPOINT_RESTORE", "DAC_READ_SEARCH"].includes(String(k))));
}

async function reviewedOwner(execute: Execute, pod: Json, scope: NamespaceReview): Promise<ReviewedConsumer | undefined> {
  let current = record(pod);
  if (!current.kind) current = { ...current, kind: "Pod" };
  for (let depth = 0; depth < 4; depth++) {
    const id = reviewed(current, current.kind === "Pod");
    const approved = scope.consumers.find(c => c.object.uid === id.uid && c.kind === current.kind);
    if (approved) {
      if (templateDigest(current) !== approved.templateDigest) throw new Error("Private consumer template changed after protection was enabled");
      return approved;
    }
    const owners = list(at(current, "metadata", "ownerReferences") ?? []).map(record).filter(o => o.controller === true);
    if (owners.length !== 1) return undefined;
    const owner = owners[0]!;
    if (typeof owner.kind !== "string" || typeof owner.name !== "string" || !kinds[owner.kind]) return undefined;
    const version = ["Pod", "ReplicationController"].includes(owner.kind) ? "v1"
      : ["Job", "CronJob"].includes(owner.kind) ? "batch/v1" : "apps/v1";
    if (owner.apiVersion !== version) throw new Error("Private consumer owner API identity is invalid");
    const parent = await read(execute, kinds[owner.kind], owner.name, scope.namespace.name);
    if (reviewed(parent).uid !== owner.uid) throw new Error("Private consumer owner was replaced");
    if (canonical(executionSpec(template(current).spec, current.kind === "Pod"))
      !== canonical(executionSpec(template(parent).spec, false))) {
      throw new Error("Consumer execution differs from the reviewed controller template; preserve it for explicit Pod review");
    }
    current = parent;
  }
  return undefined;
}

function executionSpec(value: unknown, pod: boolean): RecordValue {
  const spec = structuredClone(record(value));
  for (const key of ["nodeName", "priority", "preemptionPolicy", "enableServiceLinks", "serviceAccount"]) delete spec[key];
  spec.serviceAccountName ??= "default";
  if (pod) {
    const automatic = new Set<string>();
    for (const volume of list(spec.volumes ?? [])) {
      const name = at(volume, "name");
      const sources = at(volume, "projected", "sources");
      if (typeof name !== "string" || !name.startsWith("kube-api-access-") || !Array.isArray(sources) || sources.length !== 3) continue;
      const token = sources.find(source => at(source, "serviceAccountToken") !== undefined);
      const ca = sources.find(source => at(source, "configMap") !== undefined);
      const namespace = sources.find(source => at(source, "downwardAPI") !== undefined);
      if (at(token, "serviceAccountToken", "path") === "token"
        && at(token, "serviceAccountToken", "audience") === undefined
        && at(ca, "configMap", "name") === "kube-root-ca.crt"
        && canonical(at(ca, "configMap", "items")) === canonical([{ key: "ca.crt", path: "ca.crt" }])
        && canonical(at(namespace, "downwardAPI", "items")) === canonical([
          { path: "namespace", fieldRef: { apiVersion: "v1", fieldPath: "metadata.namespace" } },
        ])) automatic.add(name);
    }
    spec.volumes = list(spec.volumes ?? []).filter(v => !automatic.has(String(at(v, "name"))));
    for (const kind of ["containers", "initContainers", "ephemeralContainers"]) {
      for (const container of list(spec[kind] ?? [])) {
        const value = record(container);
        value.volumeMounts = list(value.volumeMounts ?? []).filter(m =>
          !(automatic.has(String(at(m, "name"))) && at(m, "mountPath") === "/var/run/secrets/kubernetes.io/serviceaccount"
            && at(m, "readOnly") === true));
      }
    }
  }
  const normalize = (value: Json): Json => {
    if (Array.isArray(value)) return value.map(normalize);
    if (value && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).filter(([, entry]) =>
        !(Array.isArray(entry) && entry.length === 0)).map(([key, entry]) => [key, normalize(entry)]));
    }
    return value;
  };
  return record(normalize(spec));
}
