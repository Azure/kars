// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, digest, kinds, PRIVATE_PREFIX, read, record, reviewed, reviewedOwner,
  requiresPrivatePodReview, templateDigest, validateActivationShape, validateObservedControllerProfile,
  validateQualifiedActivation, validateQualifiedMetadata, type Execute, type PrivateActivation,
} from "./private-activation.js";
import { retirementReview } from "./private-activation-retirement.js";
import { verifyRetiredSealedRoot } from "./private-activation-continuity.js";

const GRANTS = "karscredentialgrants.kars.azure.com";
const SEAL = `${PRIVATE_PREFIX}root-retirement`;
const FAILURE = "Root image recovery requires the exact sealed single-root qualification; protection was preserved";
type ObjectValue = ReturnType<typeof record>;

export interface RootImageRecovery {
  version: 1;
  namespace: string;
  image: string;
  observedImage: string;
  root: ReturnType<typeof reviewed>;
  specDigest: string;
  templateDigest: string;
  evidenceDigest: string;
  replicaIntent: number;
  retireBeforeRestore: boolean;
}

function image(value: unknown): asserts value is string {
  if (typeof value !== "string" || !/^[^@\s]+@sha256:[a-f0-9]{64}$/.test(value)) {
    throw new Error("Root image recovery requires immutable observed and previously qualified images");
  }
}

function qualification(value: unknown): PrivateActivation {
  const activation = value as PrivateActivation;
  validateActivationShape(activation, "qualified");
  return activation;
}

function singleRoot(activation: PrivateActivation, namespace: string): void {
  validateActivationShape(activation, "qualified");
  const scope = activation.namespaces[0];
  if (activation.root.namespace.name !== namespace || activation.root.deployment.name !== "kars-controller"
    || activation.namespaces.length !== 1 || !scope || scope.namespace.uid !== activation.root.namespace.uid
    || scope.namespace.name !== namespace || scope.consumers.length !== 1
    || scope.consumers[0]?.kind !== "Deployment"
    || scope.consumers[0].object.uid !== activation.root.deployment.uid
    || scope.consumers[0].templateDigest !== activation.root.templateDigest) throw new Error(FAILURE);
}

async function inventory(execute: Execute, resource: string, namespace?: string): Promise<ObjectValue[]> {
  const response = record(JSON.parse(await execute([
    "get", resource, ...(namespace ? ["-n", namespace] : ["--all-namespaces"]), "--chunk-size=0", "-o", "json",
  ])));
  if (at(response, "metadata", "continue") || !Array.isArray(response.items) || response.items.length > 512) {
    throw new Error("Root image recovery inventory is incomplete or exceeds its bound");
  }
  const seen = new Set<string>();
  return response.items.map(value => {
    const object = record(value);
    const id = reviewed(object, resource === "pods");
    if (seen.has(id.uid) || (namespace && at(object, "metadata", "namespace") !== namespace)) throw new Error(FAILURE);
    seen.add(id.uid);
    return object;
  }).sort((a, b) => reviewed(a, true).uid.localeCompare(reviewed(b, true).uid));
}

function container(deployment: ObjectValue): { value: ObjectValue; index: number } {
  const containers = at(deployment, "spec", "template", "spec", "containers");
  if (!Array.isArray(containers)) throw new Error(FAILURE);
  const matches = containers.map((value, index) => ({ value: record(value), index }))
    .filter(({ value }) => value.name === "controller");
  if (matches.length !== 1) throw new Error(FAILURE);
  return matches[0]!;
}

function protectedFields(namespace: ObjectValue): ObjectValue {
  return Object.fromEntries(Object.entries(record(at(namespace, "metadata", "annotations") ?? {}))
    .filter(([key]) => key.startsWith(PRIVATE_PREFIX)));
}

function restoredNamespace(namespace: ObjectValue): ObjectValue {
  const restored = structuredClone(namespace);
  const fields = record(at(restored, "metadata", "annotations"));
  if (fields[`${PRIVATE_PREFIX}state`] !== "Pending" && fields[`${PRIVATE_PREFIX}state`] !== "Qualified") {
    throw new Error(FAILURE);
  }
  fields[`${PRIVATE_PREFIX}state`] = "Qualified";
  return restored;
}

async function verifyRecoveryScope(
  execute: Execute, namespace: string, activation: PrivateActivation, expectedGrants: ObjectValue[],
): Promise<void> {
  const namespaces = await inventory(execute, "namespaces");
  const affiliated = namespaces.filter(value => {
    const fields = protectedFields(value);
    return fields[`${PRIVATE_PREFIX}root-namespace`] === namespace
      || fields[`${PRIVATE_PREFIX}root-namespace-uid`] === activation.root.namespace.uid;
  });
  const grants = await inventory(execute, GRANTS);
  const bindings = (values: ObjectValue[]) => values.map(value => ({
    namespace: at(value, "metadata", "namespace"), name: reviewed(value).name,
    uid: reviewed(value).uid, spec: digest(value.spec),
  }));
  if (affiliated.length !== 1 || reviewed(affiliated[0]!).uid !== activation.root.namespace.uid
    || canonical(bindings(grants)) !== canonical(bindings(expectedGrants))) {
    throw new Error("Private scope or grants changed during root image recovery");
  }
}

async function inspect(execute: Execute, namespace: string, candidate: string) {
  image(candidate);
  if (!/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/.test(namespace) || namespace.length > 63) throw new Error(FAILURE);
  if ((await execute(["auth", "can-i", "manage", `${GRANTS}/workspace`, "--all-namespaces"])).trim() !== "yes") {
    throw new Error("Cluster-scoped credential operator authority is required");
  }
  const ns = await read(execute, "namespace", namespace);
  const restoredNs = restoredNamespace(ns);
  const raw = at(ns, "metadata", "annotations", SEAL);
  if (typeof raw !== "string" || Buffer.byteLength(raw) > 131_072) throw new Error(FAILURE);
  const seal = record(JSON.parse(raw));
  if (seal.version !== 2 || canonical(seal) !== raw
    || Object.keys(seal).sort().join(",") !== "activation,retirement,retirementDigest,version") throw new Error(FAILURE);
  const activation = qualification(seal.activation);
  singleRoot(activation, namespace);
  const current = await read(execute, "deployment", "kars-controller", namespace);
  const selected = container(current);
  image(selected.value.image);
  const reconstructed = structuredClone(current);
  container(reconstructed).value.image = candidate;
  const replicas = at(current, "spec", "replicas");
  if (templateDigest(reconstructed) !== activation.root.templateDigest
    || (replicas !== 0 && replicas !== activation.root.replicaIntent)) throw new Error(FAILURE);
  const retirement = retirementReview(activation, ns, reconstructed);
  if (!retirement || retirement.phase !== "restoring" || canonical(retirement) !== seal.retirement
    || digest(retirement) !== seal.retirementDigest) throw new Error(FAILURE);

  // Counterfactual metadata review only. No proof is created or changed; actual
  // execution and continuity are checked again after the restored rollout.
  const projected: Execute = async (args, input) => {
    if (args[0] !== "get" && args[0] !== "auth") throw new Error("Recovery preview is read-only");
    const response = await execute(args, input);
    if (args[0] === "get" && ["deployment", kinds.Deployment].includes(args[1]) && args[2] === "kars-controller"
      && args[args.indexOf("-n") + 1] === namespace) {
      const fresh = record(JSON.parse(response));
      if (canonical(reviewed(fresh)) !== canonical(reviewed(current)) || digest(fresh.spec) !== digest(current.spec)) {
        throw new Error("Root changed during recovery preview");
      }
      return JSON.stringify(reconstructed);
    }
    if (args[0] === "get" && args[1] === "namespace" && args[2] === namespace) {
      const fresh = record(JSON.parse(response));
      if (canonical(reviewed(fresh)) !== canonical(reviewed(ns))
        || canonical(protectedFields(fresh)) !== canonical(protectedFields(ns))) throw new Error(FAILURE);
      return JSON.stringify(restoredNs);
    }
    return response;
  };
  await validateQualifiedMetadata(projected, activation);
  await validateObservedControllerProfile(execute, activation.profile);
  const namespaces = await inventory(execute, "namespaces");
  const affiliated = namespaces.filter(value => {
    const fields = protectedFields(value);
    return fields[`${PRIVATE_PREFIX}root-namespace`] === namespace
      || fields[`${PRIVATE_PREFIX}root-namespace-uid`] === activation.root.namespace.uid;
  });
  if (affiliated.length !== 1 || reviewed(affiliated[0]!).uid !== activation.root.namespace.uid
    || canonical(protectedFields(affiliated[0]!)) !== canonical(protectedFields(ns))) throw new Error(FAILURE);
  const grants = await inventory(execute, GRANTS);
  for (const grant of grants) {
    const rawActivation = at(grant, "spec", "privateActivation");
    const writers = at(grant, "spec", "writers");
    if (!Array.isArray(writers)) throw new Error(FAILURE);
    const affiliatedWriter = writers.some(writer => at(writer, "namespace") === namespace);
    if (rawActivation != null) {
      const prior = qualification(rawActivation);
      if (at(prior, "root", "namespace", "name") === namespace
        || at(prior, "root", "namespace", "uid") === activation.root.namespace.uid || affiliatedWriter) {
        singleRoot(prior, namespace);
        if (prior.root.namespace.uid !== activation.root.namespace.uid
          || prior.root.deployment.uid !== activation.root.deployment.uid
          || prior.root.templateDigest !== activation.root.templateDigest) throw new Error(FAILURE);
        await validateQualifiedMetadata(projected, prior);
      }
    } else if (affiliatedWriter) throw new Error(FAILURE);
  }
  const pods = await inventory(execute, "pods", namespace);
  const retireBeforeRestore = at(ns, "metadata", "annotations", `${PRIVATE_PREFIX}state`) === "Pending"
    || at(current, "spec", "replicas") === 0;
  const retiringPods: ReturnType<typeof reviewed>[] = [];
  for (const pod of pods) {
    if (!await requiresPrivatePodReview(execute, pod, activation.namespaces[0]!, activation)) continue;
    const execution = structuredClone(current);
    const podContainers = at(pod, "spec", "containers");
    if (!Array.isArray(podContainers)) throw new Error(FAILURE);
    const matches = podContainers.filter(value => at(value, "name") === "controller");
    if (matches.length !== 1) throw new Error(FAILURE);
    const priorImage = at(matches[0], "image");
    image(priorImage);
    container(execution).value.image = priorImage;
    const currentActivation = structuredClone(activation);
    currentActivation.root.templateDigest = templateDigest(execution);
    currentActivation.namespaces[0]!.consumers[0]!.templateDigest = templateDigest(execution);
    // Only retirement is authorized for an image-only predecessor. Its real
    // Pod/ReplicaSet ownership and all non-image execution fields still verify.
    const retiring: Execute = async (args, input) => {
      const result = await execute(args, input);
      if (args[0] === "get" && ["deployment", kinds.Deployment].includes(args[1]) && args[2] === "kars-controller"
        && args[args.indexOf("-n") + 1] === namespace) {
        const fresh = record(JSON.parse(result));
        if (canonical(reviewed(fresh)) !== canonical(reviewed(current)) || digest(fresh.spec) !== digest(current.spec)) {
          throw new Error(FAILURE);
        }
        return JSON.stringify(execution);
      }
      return result;
    };
    if (!await reviewedOwner(retiring, pod, currentActivation.namespaces[0]!, undefined, currentActivation.root)) {
      throw new Error("Unreviewed private execution prevents root image recovery");
    }
    if (retireBeforeRestore || priorImage !== candidate) retiringPods.push(reviewed(pod, true));
  }
  const evidence = {
    namespace: { identity: reviewed(ns), fields: protectedFields(ns) },
    grants: grants.map(value => ({ namespace: at(value, "metadata", "namespace"), identity: reviewed(value), spec: digest(value.spec) })),
    pods: pods.map(value => ({ identity: reviewed(value, true), template: templateDigest(value) })),
  };
  const review: RootImageRecovery = {
    version: 1, namespace, image: candidate, observedImage: selected.value.image,
    root: reviewed(current), specDigest: digest(current.spec),
    templateDigest: activation.root.templateDigest, evidenceDigest: digest(evidence),
    replicaIntent: activation.root.replicaIntent, retireBeforeRestore,
  };
  return { review, activation, current, reconstructed, grants, ns, retiringPods, index: selected.index };
}

export async function previewRootImageRecovery(
  execute: Execute, namespace: string, candidate: string,
): Promise<RootImageRecovery> {
  return (await inspect(execute, namespace, candidate)).review;
}

export async function applyRootImageRecovery(execute: Execute, review: RootImageRecovery): Promise<void> {
  if (!review || review.version !== 1 || Object.keys(review).sort().join(",")
    !== "evidenceDigest,image,namespace,observedImage,replicaIntent,retireBeforeRestore,root,specDigest,templateDigest,version") throw new Error(FAILURE);
  const checked = await inspect(execute, review.namespace, review.image);
  if (canonical(checked.review) !== canonical(review)) throw new Error("Root image recovery changed; regenerate the review");
  if (review.observedImage !== review.image || (review.retireBeforeRestore && at(checked.current, "spec", "replicas") !== 0)) {
    if (review.retireBeforeRestore) record(checked.reconstructed.spec).replicas = 0;
    const patch = JSON.stringify([
      { op: "test", path: "/metadata/uid", value: review.root.uid },
      { op: "test", path: "/metadata/resourceVersion", value: review.root.resourceVersion },
      { op: "test", path: "/spec", value: checked.current.spec },
      { op: "replace", path: `/spec/template/spec/containers/${checked.index}/image`, value: review.image },
      ...(review.retireBeforeRestore ? [{ op: "replace", path: "/spec/replicas", value: 0 }] : []),
    ]);
    const command = ["patch", "deployment", "kars-controller", "-n", review.namespace,
      "--type=json", "--patch-file=/dev/stdin", "-o", "json"];
    const projected = record(JSON.parse(await execute([...command, "--dry-run=server"], patch)));
    if (reviewed(projected).uid !== review.root.uid || digest(projected.spec) !== digest(checked.reconstructed.spec)) {
      throw new Error("Root image recovery server projection differs; no mutation authorized");
    }
    if (canonical(await previewRootImageRecovery(execute, review.namespace, review.image)) !== canonical(review)) {
      throw new Error("Root image recovery authority changed after server projection");
    }
    const updated = record(JSON.parse(await execute(command, patch)));
    if (reviewed(updated).uid !== review.root.uid || digest(updated.spec) !== digest(checked.reconstructed.spec)) {
      throw new Error("Root image recovery acknowledgement differs; inspect current protection");
    }
  } else if (canonical(await previewRootImageRecovery(execute, review.namespace, review.image)) !== canonical(review)) {
    throw new Error("Root quarantine recovery authority changed; regenerate the review");
  }
  if (!review.retireBeforeRestore) {
    await execute(["rollout", "status", "deployment/kars-controller", "-n", review.namespace, "--timeout=120s"]);
  }
  if (checked.retiringPods.length) {
    await execute(["wait", "--for=delete", ...checked.retiringPods.map(pod => `pod/${pod.name}`),
      "-n", review.namespace, "--timeout=120s"]);
  }
  const namespace = await read(execute, "namespace", review.namespace);
  if (reviewed(namespace).uid !== reviewed(checked.ns).uid
    || canonical(protectedFields(namespace)) !== canonical(protectedFields(checked.ns))) {
    throw new Error("Private protection changed during root image recovery");
  }
  if (at(namespace, "metadata", "annotations", `${PRIVATE_PREFIX}state`) === "Pending") {
    const qualified = restoredNamespace(namespace);
    const proposed: Execute = async (args, input) => {
      const response = await execute(args, input);
      if (args[0] === "get" && args[1] === "namespace" && args[2] === review.namespace) {
        const current = record(JSON.parse(response));
        if (canonical(reviewed(current)) !== canonical(reviewed(namespace))
          || canonical(protectedFields(current)) !== canonical(protectedFields(namespace))) throw new Error(FAILURE);
        return JSON.stringify(qualified);
      }
      return response;
    };
    // Native admission requires Qualified for startup. Retire every old root
    // execution first, then verify the original proof before releasing startup.
    await verifyRetiredSealedRoot(proposed, checked.activation);
    await verifyRecoveryScope(execute, review.namespace, checked.activation, checked.grants);
    await execute(["patch", "namespace", review.namespace, "--type=json",
      "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify([
      { op: "test", path: "/metadata/uid", value: reviewed(namespace).uid },
      { op: "test", path: "/metadata/resourceVersion", value: reviewed(namespace).resourceVersion },
      { op: "test", path: "/metadata/annotations", value: at(namespace, "metadata", "annotations") },
      { op: "replace", path: `/metadata/annotations/${PRIVATE_PREFIX.replaceAll("/", "~1")}state`, value: "Qualified" },
    ]));
  }
  if (review.retireBeforeRestore) {
    await verifyRetiredSealedRoot(execute, checked.activation);
    await verifyRecoveryScope(execute, review.namespace, checked.activation, checked.grants);
    const root = await read(execute, "deployment", "kars-controller", review.namespace);
    if (reviewed(root).uid !== review.root.uid || templateDigest(root) !== review.templateDigest
      || at(root, "spec", "replicas") !== 0) throw new Error(FAILURE);
    await execute(["patch", "deployment", "kars-controller", "-n", review.namespace,
      "--type=json", "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify([
      { op: "test", path: "/metadata/uid", value: review.root.uid },
      { op: "test", path: "/metadata/resourceVersion", value: reviewed(root).resourceVersion },
      { op: "test", path: "/spec", value: root.spec },
      { op: "replace", path: "/spec/replicas", value: review.replicaIntent },
    ]));
    await execute(["rollout", "status", "deployment/kars-controller", "-n", review.namespace, "--timeout=120s"]);
  }
  await validateQualifiedActivation(execute, checked.activation);
  await verifyRecoveryScope(execute, review.namespace, checked.activation, checked.grants);
  if (canonical(protectedFields(await read(execute, "namespace", review.namespace)))
    !== canonical(protectedFields(restoredNamespace(checked.ns)))) {
    throw new Error("Private proof changed during root image recovery");
  }
}
