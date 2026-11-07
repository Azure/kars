// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, digest, kinds, PRIVATE_PREFIX, read, record, reviewed, reviewBudgetTls, sameBudgetTls,
  template, templateDigest, validateActivationShape, validateObservedControllerProfile,
  type Execute, type NamespaceReview, type PrivateActivation,
} from "./private-activation.js";
import { reviewPrivateContinuity, reviewSealedRootPredecessor } from "./private-activation-continuity.js";
import { replicaIntent, retirementBinding } from "./private-activation-retirement.js";
import { assertFirstInstallRouterContinuity, assertRootImageDelta } from "./private-root-image-delta.js";
import { companionImageDelta, type CompanionImageDelta } from "./private-root-migration-images.js";

const GRANTS = "karscredentialgrants.kars.azure.com";
const HISTORY = `${PRIVATE_PREFIX}root-retirement`;
const ERROR = "Private root migration inventory changed or is incomplete; no cutover is authorized";
const MAX_SCOPES = 80;
const MAX_GRANTS = 128;
type ObjectValue = ReturnType<typeof record>;

function qualifiedActivation(value: unknown): PrivateActivation {
  const activation = value as PrivateActivation;
  validateActivationShape(activation, "qualified");
  return activation;
}

export interface PrivateRootMigrationReview {
  version: 1;
  phase: "reviewed";
  activation: PrivateActivation;
  predecessor: string;
  release: { name: string; namespace: string };
  renderDigest: string;
  target: { templateDigest: string; specDigest: string };
  companionImages?: {
    identity: ReturnType<typeof reviewed>;
    original: { templateDigest: string; specDigest: string };
    target: { templateDigest: string; specDigest: string };
    images: CompanionImageDelta[];
  }[];
  grants: { namespace: string; identity: ReturnType<typeof reviewed>; specDigest: string }[];
  inventoryDigest: string;
  credentialRollouts?: { namespaceUid: string; deploymentUid: string; baseline: string; template: string }[];
  recovery?: { historicalRoot: ObjectValue; observedRoot: ObjectValue };
}

/** Namespace resource versions are CAS tokens, not authority; the inventory binds their content. */
export function rootMigrationReviewDigest(review: PrivateRootMigrationReview): string {
  return digest({ ...review, activation: { ...review.activation,
    namespaces: review.activation.namespaces.map(scope => ({ ...scope,
      namespace: { name: scope.namespace.name, uid: scope.namespace.uid },
    })),
  } });
}

/** A predecessor is authority history, never qualification of the changed live execution. */
export async function reviewMigrationPredecessor(
  execute: Execute, activation: PrivateActivation, historicalRoot: ObjectValue,
): Promise<void> {
  const continuity = await reviewSealedRootPredecessor(execute, historicalRoot);
  if (retirementBinding(activation) !== retirementBinding(continuity.proof.activation)
    || activation.namespaces.length !== 1
    || !sameBudgetTls(activation.root.budgetTls, continuity.proof.activation.root.budgetTls)
    || activation.namespaces[0]?.epoch !== continuity.proof.activation.namespaces[0]?.epoch) throw new Error(ERROR);
}

async function inventory(execute: Execute, resource: string, namespace?: string): Promise<ObjectValue[]> {
  const response = record(JSON.parse(await execute([
    "get", resource, ...(namespace ? ["-n", namespace] : ["--all-namespaces"]), "--chunk-size=0", "-o", "json",
  ])));
  if (at(response, "metadata", "continue") || !Array.isArray(response.items)) throw new Error(ERROR);
  const seen = new Set<string>();
  return response.items.map(item => {
    const object = record(item);
    const identity = reviewed(object);
    if (seen.has(identity.uid) || (namespace && at(object, "metadata", "namespace") !== namespace)) throw new Error(ERROR);
    seen.add(identity.uid);
    return object;
  });
}

function rootMatches(activation: PrivateActivation, expected: PrivateActivation): boolean {
  const identity = (value: PrivateActivation) => ({
    namespace: { name: value.root.namespace.name, uid: value.root.namespace.uid },
    account: { name: value.root.account.name, uid: value.root.account.uid },
    deployment: { name: value.root.deployment.name, uid: value.root.deployment.uid },
  });
  return canonical(identity(activation)) === canonical(identity(expected));
}

async function scopeInventory(execute: Execute, namespace: ObjectValue, recovery = false): Promise<NamespaceReview> {
  const fields = record(at(namespace, "metadata", "annotations"));
  const epoch = fields[`${PRIVATE_PREFIX}epoch`];
  if (!(fields[`${PRIVATE_PREFIX}state`] === "Qualified" || (recovery && fields[`${PRIVATE_PREFIX}state`] === "Pending"))
    || typeof epoch !== "string" || !/^[a-f0-9]{64}$/.test(epoch)) throw new Error(ERROR);
  const parents = Object.entries(fields).filter(([key, value]) =>
    key.startsWith(`${PRIVATE_PREFIX}parent-`) && value === epoch,
  ).map(([key]) => key.slice(`${PRIVATE_PREFIX}parent-`.length));
  if (parents.length > 64) throw new Error(ERROR);
  const objects = await inventory(execute, Object.values(kinds).join(","), reviewed(namespace).name);
  const consumers: NamespaceReview["consumers"] = [];
  for (const uid of parents) {
    const object = objects.find(value => reviewed(value).uid === uid);
    const kind = at(object, "kind");
    if (!object || typeof kind !== "string" || !kinds[kind]) throw new Error(ERROR);
    consumers.push({ kind, object: reviewed(object), templateDigest: templateDigest(object) });
  }
  consumers.sort((a, b) => a.object.uid.localeCompare(b.object.uid));
  return { namespace: reviewed(namespace), epoch, consumers };
}

function immutableImages(deployment: ObjectValue): void {
  const spec = record(template(deployment).spec);
  if (!Array.isArray(spec.containers) || !spec.containers.length) throw new Error(ERROR);
  for (const group of [spec.containers, spec.initContainers ?? []]) {
    if (!Array.isArray(group)) throw new Error(ERROR);
    for (const container of group) {
      const image = at(container, "image");
      if (typeof image !== "string" || !/^[^@\s]+@sha256:[a-f0-9]{64}$/.test(image)) {
        throw new Error("Private root migration requires immutable old and target container images");
      }
    }
  }
}

async function projected(execute: Execute, object: ObjectValue): Promise<ObjectValue> {
  const result = record(JSON.parse(await execute([
    "apply", "--server-side", "--dry-run=server", "--force-conflicts", "--field-manager=helm", "-f", "-", "-o", "json",
  ], JSON.stringify(object))));
  reviewed(result);
  return result;
}

/** A read-only inventory and server projection, never a Helm/upgrade authorization. */
export async function previewPrivateRootMigration(
  execute: Execute, rootNamespace: string, releaseName: string, documents: readonly unknown[],
  historicalRoot?: ObjectValue, companionImages: readonly string[] = [],
): Promise<PrivateRootMigrationReview> {
  if (!Array.isArray(companionImages) || companionImages.length > 64
    || new Set(companionImages).size !== companionImages.length
    || companionImages.some(name => typeof name !== "string" || name.length > 253 || !/^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$/.test(name))
    || (historicalRoot && companionImages.length)) throw new Error(ERROR);
  if (!/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/.test(rootNamespace) || rootNamespace.length > 63
    || !/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/.test(releaseName) || releaseName.length > 53
    || !documents.length || documents.length > 1024) throw new Error(ERROR);
  if ((await execute(["auth", "can-i", "manage", `${GRANTS}/workspace`, "--all-namespaces"])).trim() !== "yes") {
    throw new Error("Cluster-scoped credential operator authority is required");
  }
  const namespace = await read(execute, "namespace", rootNamespace);
  const raw = at(namespace, "metadata", "annotations", HISTORY);
  if (typeof raw !== "string" || Buffer.byteLength(raw) > 131_072) throw new Error(ERROR);
  const seal = record(JSON.parse(raw));
  if (seal.version !== 2 || canonical(seal) !== raw) throw new Error(ERROR);
  const activation = structuredClone(qualifiedActivation(seal.activation));
  if (activation.root.namespace.name !== rootNamespace || activation.root.namespace.uid !== reviewed(namespace).uid) throw new Error(ERROR);
  const continuity = historicalRoot
    ? await reviewSealedRootPredecessor(execute, historicalRoot)
    : await reviewPrivateContinuity(execute, activation, false, true);
  if (!continuity?.sealed || canonical(continuity.proof) !== raw) throw new Error(ERROR);
  const reviewAuthority = (value: PrivateActivation) => historicalRoot
    ? reviewMigrationPredecessor(execute, value, historicalRoot)
    : reviewPrivateContinuity(execute, value, false, true);
  await validateObservedControllerProfile(execute, activation.profile);

  const namespaces = await inventory(execute, "namespaces");
  const grants = await inventory(execute, GRANTS);
  if (grants.length > MAX_GRANTS) throw new Error("Private root migration grant inventory exceeds its bound");
  const affiliated = namespaces.filter(value => {
    const fields = record(at(value, "metadata", "annotations") ?? {});
    if (!Object.keys(fields).some(key => key.startsWith(PRIVATE_PREFIX))) return false;
    const rootName = fields[`${PRIVATE_PREFIX}root-namespace`];
    const rootUid = fields[`${PRIVATE_PREFIX}root-namespace-uid`];
    if (typeof rootName !== "string" || !rootName || typeof rootUid !== "string" || !rootUid) throw new Error(ERROR);
    if (rootName !== rootNamespace && rootUid !== activation.root.namespace.uid) return false;
    if (rootName !== rootNamespace || rootUid !== activation.root.namespace.uid) throw new Error(ERROR);
    return true;
  });
  if (!affiliated.length || affiliated.length > MAX_SCOPES) throw new Error(ERROR);
  if (activation.namespaces.some(scope => !affiliated.some(value => reviewed(value).uid === scope.namespace.uid))) throw new Error(ERROR);
  activation.namespaces = [];
  for (const scope of affiliated.sort((a, b) => reviewed(a).name.localeCompare(reviewed(b).name))) {
    activation.namespaces.push(await scopeInventory(execute, scope, !!historicalRoot));
  }
  if (historicalRoot) {
    const scope = activation.namespaces[0];
    if (activation.namespaces.length !== 1 || scope?.namespace.uid !== activation.root.namespace.uid
      || scope.consumers.length !== 1 || scope.consumers[0]?.kind !== "Deployment"
      || scope.consumers[0].object.uid !== activation.root.deployment.uid) throw new Error(ERROR);
    scope.consumers[0].templateDigest = activation.root.templateDigest;
  }
  await reviewAuthority(activation);

  const selected: PrivateRootMigrationReview["grants"] = [];
  for (const grant of grants) {
    const namespace = at(grant, "metadata", "namespace");
    if (typeof namespace !== "string" || !namespace) throw new Error(ERROR);
    const candidate = at(grant, "spec", "privateActivation");
    const writers = at(grant, "spec", "writers");
    if (!Array.isArray(writers) || writers.length > 16) throw new Error(ERROR);
    let affiliatedGrant = affiliated.some(value => reviewed(value).name === namespace
      || writers.some(writer => at(writer, "namespace") === reviewed(value).name));
    if (candidate != null) {
      const prior = qualifiedActivation(candidate);
      if (prior.root.namespace.name === rootNamespace || prior.root.namespace.uid === activation.root.namespace.uid) {
        if (!rootMatches(prior, activation)) throw new Error(ERROR);
        affiliatedGrant = true;
        if (prior.namespaces.some(scope => !activation.namespaces.some(current =>
          current.namespace.name === scope.namespace.name && current.namespace.uid === scope.namespace.uid))) throw new Error(ERROR);
        await reviewAuthority(prior);
      } else if (affiliatedGrant) throw new Error(ERROR);
    }
    if (!affiliatedGrant) continue;
    if (reviewed(grant).name !== "workspace" || (writers.length && candidate == null)) throw new Error(ERROR);
    selected.push({ namespace, identity: reviewed(grant), specDigest: digest(record(grant.spec)) });
  }
  selected.sort((a, b) => a.namespace.localeCompare(b.namespace));

  const deployment = await read(execute, "deployment", activation.root.deployment.name, rootNamespace);
  if (reviewed(deployment).uid !== activation.root.deployment.uid
    || (!historicalRoot && templateDigest(deployment) !== activation.root.templateDigest)
    || at(deployment, "metadata", "annotations", "meta.helm.sh/release-name") !== releaseName
    || at(deployment, "metadata", "annotations", "meta.helm.sh/release-namespace") !== rootNamespace) throw new Error(ERROR);
  immutableImages(deployment);
  const rendered = documents.map(record);
  const targets = rendered.filter(value => value.kind === "Deployment"
    && at(value, "metadata", "name") === activation.root.deployment.name
    && (at(value, "metadata", "namespace") ?? rootNamespace) === rootNamespace);
  if (targets.length !== 1 || targets[0]!.apiVersion !== "apps/v1"
    || at(targets[0], "metadata", "annotations", "helm.sh/hook") !== undefined) throw new Error(ERROR);
  const target = await projected(execute, targets[0]!);
  immutableImages(target);
  if (!historicalRoot) assertFirstInstallRouterContinuity(record(deployment.spec), record(target.spec));
  if (historicalRoot) {
    assertRootImageDelta(record(historicalRoot.spec), record(deployment.spec));
    if (canonical(target.spec) !== canonical(deployment.spec)) {
      throw new Error("Recovery must retain the exact observed image delta; further changes need a later migration");
    }
  }
  if (reviewed(target).uid !== activation.root.deployment.uid
    || at(target, "spec", "template", "spec", "serviceAccountName") !== activation.root.account.name
    || replicaIntent(target) !== activation.root.replicaIntent
    || canonical(at(target, "spec", "selector")) !== canonical(at(deployment, "spec", "selector"))
    || !sameBudgetTls(await reviewBudgetTls(execute, target, rootNamespace),
      await reviewBudgetTls(execute, deployment, rootNamespace))) throw new Error(ERROR);
  const changes: NonNullable<PrivateRootMigrationReview["companionImages"]> = [];
  const observedCompanions: ObjectValue[] = [];
  for (const scope of activation.namespaces) {
    for (const consumer of scope.consumers) {
      if (consumer.object.uid === activation.root.deployment.uid) continue;
      const selected = scope.namespace.name === rootNamespace && companionImages.includes(consumer.object.name);
      const matches = rendered.filter(value => value.kind === consumer.kind
        && at(value, "metadata", "name") === consumer.object.name
        && (at(value, "metadata", "namespace") ?? rootNamespace) === scope.namespace.name);
      if (matches.length > 1 || (selected && matches.length !== 1)) throw new Error(ERROR);
      if (matches.length) {
        const proposed = await projected(execute, matches[0]!);
        if (reviewed(proposed).uid !== consumer.object.uid) throw new Error(ERROR);
        if (selected) {
          if (consumer.kind !== "Deployment" || matches[0]!.apiVersion !== "apps/v1"
            || at(matches[0], "metadata", "annotations", "helm.sh/hook") !== undefined) throw new Error(ERROR);
          const before = await read(execute, "deployment", consumer.object.name, rootNamespace);
          if (reviewed(before).uid !== consumer.object.uid || templateDigest(before) !== consumer.templateDigest
            || reviewed(proposed).name !== consumer.object.name || at(proposed, "metadata", "namespace") !== rootNamespace) throw new Error(ERROR);
          replicaIntent(before);
          changes.push({ identity: reviewed(before),
            original: { templateDigest: templateDigest(before), specDigest: digest(before.spec) },
            target: { templateDigest: templateDigest(proposed), specDigest: digest(proposed.spec) },
            images: companionImageDelta(record(before.spec), record(proposed.spec)) });
          observedCompanions.push(before);
        } else {
          const before = await read(execute, kinds[consumer.kind]!, consumer.object.name, scope.namespace.name);
          if (reviewed(before).uid !== consumer.object.uid || templateDigest(before) !== consumer.templateDigest
            || canonical(proposed.spec) !== canonical(before.spec)) {
            throw new Error("Root migration cannot silently replace another reviewed private consumer");
          }
        }
      }
    }
  }
  if (changes.length !== companionImages.length) throw new Error("Companion image review must name only existing same-root enrolled Deployments");
  changes.sort((a, b) => a.identity.uid.localeCompare(b.identity.uid));
  for (const before of observedCompanions) {
    const current = await read(execute, "deployment", reviewed(before).name, rootNamespace);
    if (canonical(reviewed(current)) !== canonical(reviewed(before)) || canonical(current.spec) !== canonical(before.spec)) throw new Error(ERROR);
  }
  // Bind the complete inventories, including unrelated grants, so creation,
  // deletion and re-affiliation cannot disappear between the read boundaries.
  const binding = (ns: ObjectValue[], gs: ObjectValue[]) => digest({
    namespaces: ns.map(value => ({ identity: { name: reviewed(value).name, uid: reviewed(value).uid },
      fields: at(value, "metadata", "annotations") ?? {}, labels: at(value, "metadata", "labels") ?? {},
      finalizers: at(value, "metadata", "finalizers") ?? [], owners: at(value, "metadata", "ownerReferences") ?? [],
      spec: value.spec ?? {},
    })).sort((a, b) => a.identity.uid.localeCompare(b.identity.uid)),
    grants: gs.map(value => ({ identity: reviewed(value), namespace: at(value, "metadata", "namespace"), spec: value.spec }))
      .sort((a, b) => a.identity.uid.localeCompare(b.identity.uid)),
  });
  const inventoryDigest = binding(namespaces, grants);
  if (inventoryDigest !== binding(await inventory(execute, "namespaces"), await inventory(execute, GRANTS))
    || canonical((await read(execute, "deployment", activation.root.deployment.name, rootNamespace)).spec) !== canonical(deployment.spec)
    || at(await read(execute, "namespace", rootNamespace), "metadata", "annotations", HISTORY) !== raw) throw new Error(ERROR);
  await reviewAuthority(activation);
  const credentialRollouts: NonNullable<PrivateRootMigrationReview["credentialRollouts"]> = [];
  for (const scope of continuity.proof.activation.namespaces) {
    const current = activation.namespaces.find(value => value.namespace.uid === scope.namespace.uid);
    for (const prior of scope.consumers) {
      const live = current?.consumers.find(value => value.kind === prior.kind && value.object.uid === prior.object.uid
        && value.object.name === prior.object.name);
      if (live && live.templateDigest !== prior.templateDigest) credentialRollouts.push({
        namespaceUid: scope.namespace.uid, deploymentUid: prior.object.uid, baseline: prior.templateDigest, template: live.templateDigest,
      });
    }
  }
  return { version: 1, phase: "reviewed", activation, predecessor: digest(seal),
    release: { name: releaseName, namespace: rootNamespace }, renderDigest: digest(rendered),
    target: { templateDigest: templateDigest(target), specDigest: digest(target.spec) },
    grants: selected, inventoryDigest,
    ...(credentialRollouts.length ? { credentialRollouts } : {}),
    ...(changes.length ? { companionImages: changes } : {}),
    ...(historicalRoot ? { recovery: { historicalRoot: structuredClone(historicalRoot), observedRoot: deployment } } : {}) };
}
