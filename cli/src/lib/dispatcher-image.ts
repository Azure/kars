// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, digest, kinds, PRIVATE_PREFIX, read, readSecretMetadata, record, reviewed,
  type Execute,
} from "./private-activation.js";
import { companionImageDelta } from "./private-root-migration-images.js";
import type { OperatorJournal } from "./operator-journal.js";
import { PrivateCommandFailure } from "./private-activation-command-diagnostics.js";

type ObjectValue = ReturnType<typeof record>;
const NAME = "kars-mission-dispatcher";
const CONTAINER = "mission-dispatcher";
const GRANTS = "karscredentialgrants.kars.azure.com";
const FAILURE = "Dispatcher image review changed, is private-enrolled, or has an unsupported deployment profile";
const CONTRACT = "kars.azure.com/non-private-dispatcher-image/v1";

export interface DispatcherImageReview {
  contract: typeof CONTRACT;
  context: string;
  namespace: string;
  release: string;
  image: string;
  original: ObjectValue;
  targetSpec: ObjectValue;
  inventoryDigest: string;
}
export interface DispatcherImageReceipt {
  version: 1;
  phase: "reviewed" | "attempting" | "applied" | "verified" | "failed";
  review: DispatcherImageReview;
  reviewDigest: string;
  observed?: { identity: ReturnType<typeof reviewed>; generation: number; specDigest: string };
  failure?: string;
  commandFailure?: PrivateCommandFailure["facts"];
}

function privateFields(metadata: unknown): ObjectValue {
  const result: ObjectValue = {};
  for (const group of ["labels", "annotations"]) {
    for (const [key, value] of Object.entries(record(at(metadata, group) ?? {}))) {
      if (key.startsWith(PRIVATE_PREFIX)) result[`${group}/${key}`] = value;
    }
  }
  return result;
}

function metadataBinding(object: ObjectValue): ObjectValue {
  const meta = structuredClone(record(object.metadata));
  for (const key of ["resourceVersion", "generation", "managedFields"]) delete meta[key];
  const annotations = at(meta, "annotations");
  if (annotations) delete record(annotations)["deployment.kubernetes.io/revision"];
  return meta;
}

async function readDispatcher(execute: Execute, namespace: string): Promise<ObjectValue> {
  // Default kubectl printing drops managedFields, making a whole-metadata CAS unequal to the API object.
  const deployment = record(JSON.parse(await execute([
    "get", "deployment", NAME, "-n", namespace, "--show-managed-fields=true", "-o", "json",
  ])));
  reviewed(deployment);
  return deployment;
}

function profile(deployment: ObjectValue, namespace: string, release: string): void {
  reviewed(deployment);
  const pod = record(at(deployment, "spec", "template", "spec"));
  const containers = pod.containers;
  const replicas = at(deployment, "spec", "replicas");
  const root = at(deployment, "metadata", "annotations", "kars.azure.com/mission-root-uid");
  const deadline = at(deployment, "spec", "progressDeadlineSeconds");
  observe(deployment);
  if (!Number.isSafeInteger(deadline) || (deadline as number) <= 0) throw new Error(FAILURE);
  if (deployment.apiVersion !== "apps/v1" || deployment.kind !== "Deployment"
    || at(deployment, "metadata", "name") !== NAME || at(deployment, "metadata", "namespace") !== namespace
    || at(deployment, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
    || at(deployment, "metadata", "labels", "app.kubernetes.io/component") !== CONTAINER
    || at(deployment, "metadata", "annotations", "meta.helm.sh/release-name") !== release
    || at(deployment, "metadata", "annotations", "meta.helm.sh/release-namespace") !== namespace
    || (at(deployment, "metadata", "ownerReferences") as unknown[] | undefined)?.length
    || typeof root !== "string" || !root
    || Object.keys(privateFields(deployment.metadata)).length
    || Object.keys(privateFields(at(deployment, "spec", "template", "metadata"))).length
    || (replicas !== 0 && replicas !== 1) || at(deployment, "spec", "paused") === true
    || at(deployment, "spec", "strategy", "type") !== "Recreate"
    || pod.serviceAccountName !== NAME || (pod.serviceAccount !== undefined && pod.serviceAccount !== NAME)
    || !Array.isArray(containers) || containers.length !== 1 || at(containers[0], "name") !== CONTAINER
    || (pod.initContainers !== undefined && (!Array.isArray(pod.initContainers) || pod.initContainers.length))) throw new Error(FAILURE);
  const env = at(containers[0], "env");
  if (!Array.isArray(env)) throw new Error(FAILURE);
  const expected = {
    KARS_MISSION_DISPATCHER_DEPLOYMENT: { value: NAME },
    KARS_MISSION_DISPATCHER_RELEASE: { value: release },
    KARS_MISSION_IDENTITY_ROOT: { valueFrom: { secretKeyRef: { name: `${NAME}-identity`, key: "root", optional: false } } },
  };
  for (const [name, value] of Object.entries(expected)) {
    const entries = env.filter(entry => at(entry, "name") === name);
    if (entries.length !== 1 || canonical(entries[0]) !== canonical({ name, ...value })) throw new Error(FAILURE);
  }
}

async function inventory(execute: Execute, resource: string, namespace?: string): Promise<ObjectValue[]> {
  const value = record(JSON.parse(await execute([
    "get", resource, ...(namespace ? ["-n", namespace] : ["--all-namespaces"]), "--chunk-size=0", "-o", "json",
  ])));
  if (at(value, "metadata", "continue") || !Array.isArray(value.items)) throw new Error(FAILURE);
  const seen = new Set<string>();
  return value.items.map(item => {
    const object = record(item), id = reviewed(object, true);
    if (seen.has(id.uid) || (namespace && at(object, "metadata", "namespace") !== namespace)) throw new Error(FAILURE);
    seen.add(id.uid);
    return object;
  }).sort((a, b) => reviewed(a, true).uid.localeCompare(reviewed(b, true).uid));
}

function references(value: unknown, identity: string): boolean {
  if (value === identity) return true;
  if (!value || typeof value !== "object") return false;
  return Object.entries(value).some(([key, item]) => key.endsWith(`parent-${identity}`) || references(item, identity));
}

async function privateInventory(execute: Execute, deployment: ObjectValue, namespace: string): Promise<string> {
  const account = await read(execute, "serviceaccount", NAME, namespace);
  if (reviewed(account).name !== NAME || at(account, "metadata", "namespace") !== namespace
    || Object.keys(privateFields(account.metadata)).length) throw new Error(FAILURE);
  const secret = await readSecretMetadata(execute, `${NAME}-identity`, namespace);
  if (reviewed({ metadata: secret }).name !== `${NAME}-identity`
    || at(secret, "namespace") !== namespace || Object.keys(privateFields(secret)).length) throw new Error(FAILURE);
  const namespaces = await inventory(execute, "namespaces");
  const scope = namespaces.find(value => reviewed(value, true).name === namespace);
  const cluster = namespaces.find(value => reviewed(value, true).name === "kube-system");
  if (!scope || !cluster) throw new Error(FAILURE);
  reviewed(scope); reviewed(cluster);
  const uid = reviewed(deployment).uid, accountUid = reviewed(account).uid;
  const protectedObjects: unknown[] = [];
  for (const ns of namespaces) {
    const fields = privateFields(ns.metadata);
    if (!Object.keys(fields).length) continue;
    reviewed(ns);
    // Retained parent entries and historical seals count even without a current epoch.
    const evidence = Object.fromEntries(Object.entries(fields).map(([key, value]) => {
      if (typeof value === "string" && /^\s*[[{]/.test(value)) return [key, JSON.parse(value)];
      return [key, value];
    }));
    if (references(evidence, uid) || references(evidence, accountUid) || references(evidence, NAME)
      || references(evidence, `system:serviceaccount:${namespace}:${NAME}`)) throw new Error(FAILURE);
    const objects = await inventory(execute, Object.values(kinds).join(","), reviewed(ns).name);
    for (const key of Object.keys(fields)) {
      const prefix = `annotations/${PRIVATE_PREFIX}parent-`;
      if (key.startsWith(prefix) && !objects.some(object => reviewed(object, true).uid === key.slice(prefix.length))) {
        throw new Error(FAILURE);
      }
    }
    const selected = objects.filter(object => references(evidence, reviewed(object, true).uid)
      || Object.keys(privateFields(object.metadata)).length
      || Object.keys(privateFields(at(object, "spec", "template", "metadata"))).length
      || Object.keys(privateFields(at(object, "spec", "jobTemplate", "spec", "template", "metadata"))).length);
    if (selected.some(object => reviewed(object, true).uid === uid)) throw new Error(FAILURE);
    protectedObjects.push({ namespace: metadataBinding(ns), objects: selected.map(object => ({
      kind: object.kind, metadata: metadataBinding(object), spec: object.spec,
    })) });
  }
  const grants = await inventory(execute, GRANTS);
  for (const grant of grants) {
    const spec = record(grant.spec);
    const activation = spec.privateActivation;
    if (activation != null) record(activation);
    const writers = spec.writers;
    if (!Array.isArray(writers) || references(spec, uid) || references(spec, accountUid) || references(spec, NAME)
      || writers.some(writer => at(writer, "uid") === accountUid
        || (at(writer, "namespace") === namespace && at(writer, "name") === NAME))) throw new Error(FAILURE);
  }
  return digest({ cluster: metadataBinding(cluster), namespace: metadataBinding(scope), protectedObjects,
    grants: grants.map(grant => ({ metadata: metadataBinding(grant), spec: grant.spec })),
    account: { metadata: metadataBinding(account), secrets: account.secrets ?? null, imagePullSecrets: account.imagePullSecrets ?? null,
      automountServiceAccountToken: account.automountServiceAccountToken ?? null },
    identitySecret: secret,
  });
}

function target(original: ObjectValue, image: string): ObjectValue {
  const spec = structuredClone(record(original.spec));
  const containers = at(spec, "template", "spec", "containers");
  if (!Array.isArray(containers) || containers.length !== 1) throw new Error(FAILURE);
  record(containers[0]).image = image;
  const changes = companionImageDelta(record(original.spec), spec);
  if (changes.length !== 1 || changes[0]!.group !== "containers" || changes[0]!.name !== CONTAINER) throw new Error(FAILURE);
  return spec;
}

export async function reviewDispatcherImage(
  execute: Execute, context: string, namespace: string, release: string, image: string,
): Promise<DispatcherImageReceipt> {
  if (!context || !/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/.test(namespace) || namespace.length > 63
    || !/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/.test(release) || release.length > 53) throw new Error(FAILURE);
  const original = await readDispatcher(execute, namespace);
  profile(original, namespace, release);
  const targetSpec = target(original, image);
  const inventoryDigest = await privateInventory(execute, original, namespace);
  const review: DispatcherImageReview = { contract: CONTRACT, context, namespace, release, image, original, targetSpec, inventoryDigest };
  return { version: 1, phase: "reviewed", review, reviewDigest: digest(review) };
}

function validate(receipt: DispatcherImageReceipt): DispatcherImageReview {
  if (receipt.version !== 1 || receipt.review?.contract !== CONTRACT || digest(receipt.review) !== receipt.reviewDigest) throw new Error(FAILURE);
  const review = receipt.review;
  profile(review.original, review.namespace, review.release);
  if (canonical(target(review.original, review.image)) !== canonical(review.targetSpec)) throw new Error(FAILURE);
  return review;
}

function observe(deployment: ObjectValue): NonNullable<DispatcherImageReceipt["observed"]> {
  const generation = at(deployment, "metadata", "generation");
  if (!Number.isSafeInteger(generation) || (generation as number) < 1) throw new Error(FAILURE);
  return { identity: reviewed(deployment), generation: generation as number, specDigest: digest(deployment.spec) };
}

function verifyTarget(current: ObjectValue, review: DispatcherImageReview) {
  profile(current, review.namespace, review.release);
  const observed = observe(current);
  if (canonical(metadataBinding(current)) !== canonical(metadataBinding(review.original))
    || observed.specDigest !== digest(review.targetSpec)
    || observed.generation <= (at(review.original, "metadata", "generation") as number)) throw new Error(FAILURE);
  return observed;
}

async function verify(execute: Execute, receipt: DispatcherImageReceipt): Promise<DispatcherImageReceipt> {
  const review = validate(receipt);
  const current = await readDispatcher(execute, review.namespace);
  const observed = verifyTarget(current, review);
  if (await privateInventory(execute, current, review.namespace) !== review.inventoryDigest) throw new Error(FAILURE);
  const replicas = at(current, "spec", "replicas") as number;
  if ((at(current, "status", "observedGeneration") as number) < observed.generation
    || at(current, "status", "observedGeneration") === undefined
    || ["replicas", "updatedReplicas", "readyReplicas", "availableReplicas"].some(key =>
      (at(current, "status", key) ?? 0) !== replicas)
    || (at(current, "status", "unavailableReplicas") ?? 0) !== 0
    || (at(current, "status", "terminatingReplicas") ?? 0) !== 0) {
    throw new Error("Dispatcher target Deployment has not completed its rollout; retain the receipt and use verify later");
  }
  return { ...receipt, phase: "verified", observed };
}

/** One image replace, exact full-spec/metadata CAS, never a Helm publication or mission restart. */
export async function applyDispatcherImage(
  execute: Execute, receipt: DispatcherImageReceipt, journal: OperatorJournal<DispatcherImageReceipt>,
): Promise<DispatcherImageReceipt> {
  const review = validate(receipt);
  if (receipt.phase !== "reviewed") throw new Error("Dispatcher image attempt already consumed; use read-only verify, never replay apply");
  let current: DispatcherImageReceipt = { ...receipt, phase: "attempting" };
  await journal.save(current);
  let stage = "inventory";
  try {
    const fresh = await reviewDispatcherImage(execute, review.context, review.namespace, review.release, review.image);
    // Status is not authority, but its resourceVersion remains part of the exact CAS.
    if (fresh.review.inventoryDigest !== review.inventoryDigest
      || canonical(fresh.review.original.metadata) !== canonical(review.original.metadata)
      || canonical(fresh.review.original.spec) !== canonical(review.original.spec)) throw new Error(FAILURE);
    const original = review.original;
    const patch = [
      { op: "test", path: "/metadata/uid", value: reviewed(original).uid },
      { op: "test", path: "/metadata/resourceVersion", value: reviewed(original).resourceVersion },
      { op: "test", path: "/metadata", value: original.metadata },
      { op: "test", path: "/spec", value: original.spec },
      { op: "replace", path: "/spec/template/spec/containers/0/image", value: review.image },
    ];
    stage = "image CAS";
    const result = record(JSON.parse(await execute([
      "patch", "deployment", NAME, "-n", review.namespace, "--type=json", "--patch-file=/dev/stdin", "-o", "json",
    ], JSON.stringify(patch))));
    current = { ...current, phase: "applied", observed: observe(result) };
    await journal.save(current);
    stage = "patch response verification";
    verifyTarget(result, review);
    const deadline = at(original, "spec", "progressDeadlineSeconds");
    stage = "rollout";
    await execute(["rollout", "status", `deployment/${NAME}`, "-n", review.namespace, `--timeout=${deadline}s`]);
    stage = "live verification";
    current = await verify(execute, current);
    await journal.save(current);
    return current;
  } catch (error) {
    await journal.save({ ...current, phase: "failed", failure: `${stage} failed; preserve all records. Inspect live state; do not replay this attempt.`,
      ...(error instanceof PrivateCommandFailure ? { commandFailure: error.facts } : {}),
    });
    throw error;
  }
}

/** Resolves an uncertain acknowledgement or delayed rollout without any cluster writes. */
export async function verifyDispatcherImage(
  execute: Execute, receipt: DispatcherImageReceipt, journal: OperatorJournal<DispatcherImageReceipt>,
): Promise<DispatcherImageReceipt> {
  validate(receipt);
  if (!["attempting", "applied", "failed", "verified"].includes(receipt.phase)) throw new Error(FAILURE);
  try {
    const result = await verify(execute, receipt);
    await journal.save(result);
    return result;
  } catch (error) {
    await journal.save({ ...receipt, phase: "failed", failure: "Read-only verification failed; prior apply and failure evidence retained.",
      ...(error instanceof PrivateCommandFailure ? { commandFailure: error.facts } : {}),
    });
    throw error;
  }
}
