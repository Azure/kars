// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomBytes, X509Certificate } from "node:crypto";
import { readFileSync } from "node:fs";
import { requireBundledAsset } from "./repo-assets.js";

export type Execute = (args: string[], input?: string) => Promise<string>;
type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type RecordValue = { [key: string]: Json };
export interface ReviewedObject { name: string; uid: string; resourceVersion: string }
export interface ReviewedConsumer { kind: string; object: ReviewedObject; templateDigest: string }
export interface NamespaceReview { namespace: ReviewedObject; consumers: ReviewedConsumer[]; epoch?: string }
export interface BudgetTlsReview { namespace: ReviewedObject; secret: ReviewedObject; keyDigest: string }
export interface PrivateActivation {
  contract: string;
  phase: "reviewed" | "qualified";
  bundleRevision: string;
  root: { namespace: ReviewedObject; account: ReviewedObject; deployment: ReviewedObject; templateDigest: string; budgetTls?: BudgetTlsReview };
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

function rootEnvironment(deployment: unknown, name: string): string | undefined {
  const container = list(at(deployment, "spec", "template", "spec", "containers")).find(c => at(c, "name") === "controller");
  if (!container) throw new Error("Reviewed root controller container is missing");
  const entries = list(at(container, "env") ?? []).filter(e => at(e, "name") === name);
  if (entries.length > 1 || entries.some(e => at(e, "valueFrom") !== undefined)) {
    throw new Error("Private root budget inputs must have one explicit reviewed literal value");
  }
  const value = entries.length ? at(entries[0], "value") : undefined;
  if (value !== undefined && typeof value !== "string") throw new Error("Private root environment input is invalid");
  return value;
}

async function reviewBudgetTls(execute: Execute, deployment: unknown, rootNamespace: string): Promise<BudgetTlsReview | undefined> {
  const enabled = rootEnvironment(deployment, "KARS_INFERENCE_BUDGET_ENABLED");
  if (enabled === undefined || enabled === "" || enabled === "false") return undefined;
  if (enabled !== "true") throw new Error("Reviewed budget enablement is invalid");
  const name = rootEnvironment(deployment, "KARS_INFERENCE_BUDGET_TLS_SECRET");
  if (!name) throw new Error("Enabled budget broker lacks an explicit TLS Secret name");
  const configured = rootEnvironment(deployment, "KARS_NAMESPACE")?.trim();
  const controller = list(at(deployment, "spec", "template", "spec", "containers")).find(c => at(c, "name") === "controller");
  const podNamespace = list(at(controller, "env") ?? []).filter(e => at(e, "name") === "POD_NAMESPACE");
  if (podNamespace.length > 1) throw new Error("Root Pod namespace input is ambiguous");
  const value = podNamespace.length ? at(podNamespace[0], "value") : undefined;
  const downward = podNamespace.length ? at(podNamespace[0], "valueFrom", "fieldRef", "fieldPath") : undefined;
  if (value !== undefined && typeof value !== "string") throw new Error("Root Pod namespace input is invalid");
  if (podNamespace.length && value === undefined && downward !== "metadata.namespace") throw new Error("Root Pod namespace input requires explicit review");
  const namespace = configured || (typeof value === "string" ? value.trim() : downward ? rootNamespace : "") || "kars-system";
  const ns = reviewed(await read(execute, "namespace", namespace));
  const metadata = JSON.parse(await execute(["get", "secret", name, "-n", namespace, "-o", "go-template={{json .metadata}}"]));
  const secret = reviewed({ metadata });
  if (at(metadata, "annotations", "kars.azure.com/inference-budget-tls") !== "v1") {
    throw new Error("Budget TLS Secret is not the reviewed budget identity");
  }
  if ((await execute(["get", "secret", name, "-n", namespace, "-o", "go-template={{.type}}"])).trim() !== "kubernetes.io/tls") {
    throw new Error("Budget TLS Secret type is invalid");
  }
  const certificate = await execute(["get", "secret", name, "-n", namespace, "-o", 'go-template={{index .data "tls.crt"}}']);
  const publicKey = new X509Certificate(Buffer.from(certificate.trim(), "base64")).publicKey
    .export({ format: "der", type: "spki" });
  return { namespace: ns, secret, keyDigest: createHash("sha256").update(publicKey).digest("hex") };
}

function sameBudgetTls(a: BudgetTlsReview | undefined, b: BudgetTlsReview | undefined): boolean {
  const normalized = (value: BudgetTlsReview | undefined) => value ? {
    ...value, namespace: { name: value.namespace.name, uid: value.namespace.uid },
  } : null;
  return canonical(normalized(a)) === canonical(normalized(b));
}

function materialForNamespace(value: unknown, namespace: string, activation: PrivateActivation): boolean {
  const extra = activation.root.budgetTls?.namespace.name === namespace ? [activation.root.budgetTls.secret.name] : [];
  return privateMaterial(value, extra);
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
  const budgetTls = await reviewBudgetTls(execute, deployment, rootNamespace);
  const controllerUids: Record<string, string> = {};
  if (profile === "service-accounts") {
    for (const name of list(bundleDefinition().controllers)) {
      if (typeof name !== "string") throw new Error("Private controller profile is invalid");
      controllerUids[name] = reviewed(await read(execute, "serviceaccount", name, "kube-system")).uid;
    }
  }
  const names = new Set([workspace, rootNamespace, ...writers.map(w => w.namespace),
    ...targets.map(t => `kars-${t.name}`)]);
  if (budgetTls) names.add(budgetTls.namespace.name);
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
    root: { namespace: reviewed(rootNs), account: reviewed(account), deployment: reviewed(deployment), templateDigest: templateDigest(deployment),
      ...(budgetTls ? { budgetTls } : {}) },
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
  exact(activation.root, ["namespace", "account", "deployment", "templateDigest", "budgetTls"]);
  for (const value of [activation.root.namespace, activation.root.account, activation.root.deployment]) identityShape(value);
  if (activation.root.budgetTls) {
    exact(activation.root.budgetTls, ["namespace", "secret", "keyDigest"]);
    identityShape(activation.root.budgetTls.namespace);
    identityShape(activation.root.budgetTls.secret);
    if (!/^[a-f0-9]{64}$/.test(activation.root.budgetTls.keyDigest)) throw new Error("Budget TLS public-key review is malformed");
  }
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
    const rootDeployment = await read(execute, "deployment", root.deployment.name, root.namespace.name);
    const budgetTls = await reviewBudgetTls(execute, rootDeployment, root.namespace.name);
    if (!sameBudgetTls(budgetTls, root.budgetTls)) throw new Error("Reviewed budget TLS identity or public key changed");
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
  const budget = activation.root.budgetTls;
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
    ...(budget ? {
      [`${PRIVATE_PREFIX}budget-namespace`]: budget.namespace.name,
      [`${PRIVATE_PREFIX}budget-namespace-uid`]: budget.namespace.uid,
      [`${PRIVATE_PREFIX}budget-tls-name`]: budget.secret.name,
      [`${PRIVATE_PREFIX}budget-tls-uid`]: budget.secret.uid,
      [`${PRIVATE_PREFIX}budget-tls-version`]: budget.secret.resourceVersion,
      [`${PRIVATE_PREFIX}budget-key`]: budget.keyDigest,
    } : {}),
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
  if (staged.root.budgetTls) {
    const budget = staged.root.budgetTls;
    const scope = staged.namespaces.find(item => item.namespace.name === budget.namespace.name);
    if (!scope) throw new Error("Budget TLS namespace is missing from activation review");
    const current = await read(execute, "namespace", scope.namespace.name);
    const old = record(at(current, "metadata", "annotations"));
    const alreadyQualified = old[`${PRIVATE_PREFIX}budget-qualified-bundle`] === staged.bundleRevision
      && old[`${PRIVATE_PREFIX}budget-qualified-key`] === budget.keyDigest
      && old[`${PRIVATE_PREFIX}budget-qualified-secret`] === budget.secret.uid;
    if (!alreadyQualified && old[`${PRIVATE_PREFIX}budget-rotation-bundle`] !== staged.bundleRevision) {
      await patchNamespace(execute, scope, {
        [`${PRIVATE_PREFIX}budget-rotation-bundle`]: staged.bundleRevision,
        [`${PRIVATE_PREFIX}budget-before-key`]: budget.keyDigest,
      });
      throw new Error("Budget TLS key requires operator rotation and public-CA update through the existing budget workflow; re-preview afterwards");
    }
    if (!alreadyQualified && old[`${PRIVATE_PREFIX}budget-before-key`] === budget.keyDigest) {
      throw new Error("Budget TLS public key is unchanged; copying or re-encoding the key is not private requalification");
    }
  }
  const retire: { scope: NamespaceReview; consumer: ReviewedConsumer }[] = [];
  const captured = new Map<string, Set<string>>();
  const rootScope = staged.namespaces.find(scope => scope.namespace.name === staged.root.namespace.name);
  if (!rootScope) throw new Error("Reviewed root namespace is absent from activation");
  const rootBefore = await read(execute, "deployment", staged.root.deployment.name, staged.root.namespace.name);
  if (reviewed(rootBefore).uid !== staged.root.deployment.uid || templateDigest(rootBefore) !== staged.root.templateDigest) {
    throw new Error("Reviewed root changed before private consumer retirement");
  }
  const rootReplicas = at(rootBefore, "spec", "replicas") ?? 1;
  if (typeof rootReplicas !== "number" || !Number.isSafeInteger(rootReplicas) || rootReplicas < 0) {
    throw new Error("Reviewed root replica intent is invalid");
  }
  const retireRoot = consumesPrivateAuthority(rootBefore, rootScope.namespace.name, staged);
  if (retireRoot) {
    const rootConsumer = rootScope.consumers.find(consumer =>
      consumer.kind === "Deployment" && consumer.object.uid === staged.root.deployment.uid);
    if (!rootConsumer) throw new Error("Root retirement requires its explicit reviewed Deployment");
    retire.push({ scope: rootScope, consumer: rootConsumer });
  }
  for (const scope of staged.namespaces) {
    const pods = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
    if (at(pods, "metadata", "continue")) throw new Error("Private consumer inventory is incomplete");
    for (const pod of list(pods.items)) {
      if (!consumesPrivateAuthority(pod, scope.namespace.name, staged)) continue;
      const owner = await reviewedOwner(execute, pod, scope);
      if (!owner) throw new Error("Unexplained private consumer preserved; explicitly review its actual owner before activation");
      if (!["Deployment", "ReplicaSet", "StatefulSet", "ReplicationController"].includes(owner.kind)) {
        throw new Error("This reviewed private consumer requires its existing owner-specific retirement before activation; it was preserved");
      }
      const ids = captured.get(scope.namespace.name) ?? new Set<string>();
      ids.add(reviewed(pod, true).uid);
      captured.set(scope.namespace.name, ids);
      if (!retire.some(item => item.consumer.object.uid === owner.object.uid)) retire.push({ scope, consumer: owner });
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
  for (;;) {
    let pending = false;
    for (const scope of staged.namespaces) {
      const inventory = record(JSON.parse(await execute(["get", "pods", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
      if (at(inventory, "metadata", "continue")) throw new Error("Private consumer retirement inventory is incomplete");
      for (const pod of list(inventory.items)) {
        const capturedUid = captured.get(scope.namespace.name)?.has(reviewed(pod, true).uid);
        if (!capturedUid && !consumesPrivateAuthority(pod, scope.namespace.name, staged)) continue;
        if (!await reviewedOwner(execute, pod, scope)) throw new Error("Unexplained private consumer preserved during retirement");
        pending = true;
      }
    }
    if (!pending) break;
    if (Date.now() >= deadline) throw new Error("Approved private consumers have not finished retirement; protection remains enabled");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  if (await verifyPrivateBundle(execute) !== staged.bundleRevision) throw new Error("Private admission changed before epoch creation");
  const retiredRoot = await read(execute, "deployment", staged.root.deployment.name, staged.root.namespace.name);
  if (reviewed(retiredRoot).uid !== staged.root.deployment.uid || templateDigest(retiredRoot) !== staged.root.templateDigest
    || (retireRoot && at(retiredRoot, "spec", "replicas") !== 0)) {
    throw new Error("Reviewed root retirement changed before epoch creation");
  }
  for (const scope of staged.namespaces) {
    scope.epoch = randomBytes(32).toString("hex");
    await patchNamespace(execute, scope, {
      ...annotations(staged, scope, "Qualified"), [`${PRIVATE_PREFIX}epoch`]: scope.epoch,
      ...(staged.root.budgetTls ? {
        [`${PRIVATE_PREFIX}budget-qualified-bundle`]: staged.bundleRevision,
        [`${PRIVATE_PREFIX}budget-qualified-key`]: staged.root.budgetTls.keyDigest,
        [`${PRIVATE_PREFIX}budget-qualified-secret`]: staged.root.budgetTls.secret.uid,
        [`${PRIVATE_PREFIX}budget-rotation-bundle`]: "",
        [`${PRIVATE_PREFIX}budget-before-key`]: "",
      } : {}),
      ...Object.fromEntries(scope.consumers.map(c => [`${PRIVATE_PREFIX}parent-${c.object.uid}`, scope.epoch!])),
    });
    for (const consumer of scope.consumers) {
      if (consumer.kind === "Job" || consumer.kind === "Pod") continue;
      const current = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
      if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) {
        throw new Error("Reviewed consumer changed before template qualification");
      }
      if (!consumesPrivateAuthority(current, scope.namespace.name, staged)) continue;
      const marker = { metadata: { annotations: { [`${PRIVATE_PREFIX}epoch`]: scope.epoch } } };
      const spec = consumer.kind === "CronJob" ? { jobTemplate: { spec: { template: marker } } } : { template: marker };
      await execute(["patch", kinds[consumer.kind]!, consumer.object.name, "-n", scope.namespace.name, "--type=merge", "-p",
        JSON.stringify({ metadata: { uid: consumer.object.uid, resourceVersion: reviewed(current).resourceVersion }, spec })]);
    }
  }
  if (retireRoot) {
    const current = await read(execute, "deployment", staged.root.deployment.name, staged.root.namespace.name);
    const rootEpoch = rootScope.epoch;
    if (reviewed(current).uid !== staged.root.deployment.uid || templateDigest(current) !== staged.root.templateDigest
      || at(current, "spec", "replicas") !== 0
      || at(current, "spec", "template", "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== rootEpoch) {
      throw new Error("Reviewed root changed before restoring its captured replica intent");
    }
    await execute(["patch", "deployment", staged.root.deployment.name, "-n", staged.root.namespace.name, "--type=merge", "-p",
      JSON.stringify({ metadata: { uid: staged.root.deployment.uid, resourceVersion: reviewed(current).resourceVersion },
        spec: { replicas: rootReplicas } })]);
  }
  if (rootReplicas > 0) {
    const deadline = Date.now() + 120_000;
    for (;;) {
      const deployment = await read(execute, "deployment", staged.root.deployment.name, staged.root.namespace.name);
      if (reviewed(deployment).uid !== staged.root.deployment.uid || templateDigest(deployment) !== staged.root.templateDigest) {
        throw new Error("Reviewed root changed during private authority replacement");
      }
      const pods = record(JSON.parse(await execute(["get", "pods", "-n", staged.root.namespace.name, "--chunk-size=0", "-o", "json"])));
      if (at(pods, "metadata", "continue")) throw new Error("Root consumer retirement inventory is incomplete");
      const retiring = list(pods.items).some(pod => captured.get(staged.root.namespace.name)?.has(reviewed(pod, true).uid));
      const ready = at(deployment, "spec", "replicas") === rootReplicas
        && at(deployment, "status", "observedGeneration") === at(deployment, "metadata", "generation")
        && at(deployment, "status", "updatedReplicas") === rootReplicas
        && at(deployment, "status", "availableReplicas") === rootReplicas;
      if (!retiring && ready) break;
      if (Date.now() >= deadline) throw new Error("Old root authority has not retired or its replacement is unavailable; no writer activation was published");
      await new Promise(resolve => setTimeout(resolve, 500));
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
  const root = await read(execute, "deployment", activation.root.deployment.name, activation.root.namespace.name);
  if (!sameBudgetTls(await reviewBudgetTls(execute, root, activation.root.namespace.name), activation.root.budgetTls)) {
    throw new Error("Budget TLS review changed before grant publication");
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

export function privateMaterial(value: unknown, extraSecrets: string[] = []): boolean {
  const pod = record(value);
  const definition = bundleDefinition();
  const secrets = [...list(definition.secrets), ...extraSecrets];
  const volumes = list(pod.volumes ?? []);
  for (const v of volumes) {
    if (secrets.includes(at(v, "secret", "secretName") ?? null)
      || secrets.includes(at(v, "csi", "nodePublishSecretRef", "name") ?? null)) return true;
    for (const source of list(at(v, "projected", "sources") ?? [])) {
      if (secrets.includes(at(source, "secret", "name") ?? null)) return true;
      if (list(definition.tokenAudiences ?? []).includes(at(source, "serviceAccountToken", "audience") ?? null)) return true;
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
  return at(template(value), "metadata", "annotations", `${PRIVATE_PREFIX}epoch`) !== undefined
    || consumesPrivateAuthority(value, namespace, activation);
}

export function consumesPrivateAuthority(value: unknown, namespace: string, activation: PrivateActivation): boolean {
  const pod = record(template(value).spec);
  if (materialForNamespace(pod, namespace, activation)) return true;
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
