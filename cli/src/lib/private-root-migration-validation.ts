// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { X509Certificate } from "node:crypto";
import {
  annotations, at, canonical, CORE_PUBLICATION_FENCE, digest, PRIVATE_PREFIX as P, read, record, reviewed,
  reviewedOwner, requiresPrivatePodReview, reviewBudgetTls, reviewSealedAuthorityMetadata, templateDigest,
  validateActivationShape, validateQualifiedActivation, validateQualifiedMetadata, type Execute,
  type PrivateActivation,
} from "./private-activation.js";
import { verifyRetiredSealedRoot } from "./private-activation-continuity.js";
import { type GuardRetirementReview } from "./private-activation-guard-retirement.js";
import { reviewedControllerRollout } from "./private-activation-reconciliation.js";
import type { HistoricalHookProof } from "./private-core-history.js";
import {
  replicaIntent, retirementBinding, retirementState, type RootRetirement,
} from "./private-activation-retirement.js";
import { reviewMigrationPredecessor, type PrivateRootMigrationReview } from "./private-root-migration-review.js";
import { assertRootImageDelta } from "./private-root-image-delta.js";
import { companionImageDelta, immutableMigrationImages } from "./private-root-migration-images.js";
import { type PublicationBundleReview } from "./private-publication-bundle.js";
import {
  rootControlReplicas, rootControlSpec, validateRootControlTransition, type RootControlTransition,
} from "./private-root-control-transition.js";

export type ObjectValue = ReturnType<typeof record>;

export const GRANTS = "karscredentialgrants.kars.azure.com";

export const FIELD = `${P}root-retirement`;

export const FAILURE = "Private root migration changed or exceeds the supported one-root Deployment topology; preserve the receipt and protection";

interface Inventory { namespaces: ObjectValue[]; grants: ObjectValue[] }

export interface Plan {
  review: PrivateRootMigrationReview;
  seal: string;
  original: ObjectValue;
  targetSpec: ObjectValue;
  grant: ObjectValue;
  inventory: Inventory;
  guard: GuardRetirementReview;
  pods: ObjectValue[];
  companions?: ObjectValue[];
  companionTargets?: { uid: string; spec: ObjectValue }[];
  publicCa?: ObjectValue;
  restartFrom?: RootMigrationReceipt;
}

export interface RootMigrationReceipt {
  version: 1;
  plan: Plan;
  planDigest: string;
  supersededBy?: string;
  phase: "prepared" | "quiescent" | "retiring" | "cutover" | "rotation-required" | "qualifying" | "complete";
  namespace?: ObjectValue;
  namespaces?: ObjectValue[];
  captured?: ObjectValue[];
  activation?: PrivateActivation;
  retirement?: RootRetirement;
  companionUpdates?: { uid: string; phase: "updating" | "updated" }[];
  controlTransition?: RootControlTransition;
  publication?: {
    phase: "preparing" | "prepared" | "publishing" | "succeeded" | "verified";
    attempt: string;
    context: string;
    release: string;
    namespace: string;
    requestDigest: string;
    revision?: number;
    artifact?: string;
    digest?: string;
    storage?: { name: string; uid: string; revision: number; digest: string }[];
    resources?: { kind: string; name: string; namespace?: string; uid: string; digest: string; deploymentRevision?: string | null }[];
    adoption?: {
      uid: string; resourceVersion: string; annotations: ObjectValue;
      beforeDigest: string; afterDigest: string; appliedResourceVersion?: string;
    };
    witnessWriter?: {
      deployment: ObjectValue; publishedSpec?: ObjectValue; restored?: boolean;
      evidenceFence?: { uid: string; resourceVersion: string };
    };
    recovery?: {
      attempt: string; requestDigest: string;
      storage: { name: string; uid: string; revision: number; digest: string }[];
      fenced?: boolean;
    };
  };
  failedPublications?: NonNullable<RootMigrationReceipt["publication"]>[];
  historicalHookRetirement?: {
    storage: NonNullable<NonNullable<RootMigrationReceipt["publication"]>["storage"]>;
    proofs: HistoricalHookProof[];
    phase: "prepared" | "fenced";
  };
  bundleTransitions?: {
    publication: string; fence: string; review: PublicationBundleReview;
    activation: PrivateActivation; retirement: RootRetirement; namespace: ObjectValue;
    namespaces?: ObjectValue[];
    applied?: boolean;
  }[];
}

/** save must durably commit the complete receipt before resolving, including the very first call. */
export interface RootMigrationJournal { save(receipt: RootMigrationReceipt): Promise<void> }

export async function inventory(run: Execute, resource: string, namespace?: string): Promise<ObjectValue[]> {
  const response = record(JSON.parse(await run(["get", resource, ...(namespace ? ["-n", namespace] : ["--all-namespaces"]),
    "--chunk-size=0", "-o", "json"])));
  if (at(response, "metadata", "continue") || !Array.isArray(response.items) || response.items.length > 512) throw new Error(FAILURE);
  const seen = new Set<string>();
  return response.items.map(value => {
    const object = record(value), id = reviewed(object, resource === "pods");
    if (seen.has(id.uid) || (namespace && at(object, "metadata", "namespace") !== namespace)) throw new Error(FAILURE);
    seen.add(id.uid);
    return object;
  }).sort((a, b) => reviewed(a, true).uid.localeCompare(reviewed(b, true).uid));
}

export async function inventories(run: Execute): Promise<Inventory> {
  return { namespaces: await inventory(run, "namespaces"), grants: await inventory(run, GRANTS) };
}

function nsBinding(value: ObjectValue, privateFields = true): ObjectValue {
  const fields = { ...record(at(value, "metadata", "annotations") ?? {}) };
  delete fields[CORE_PUBLICATION_FENCE];
  return structuredClone({ name: reviewed(value).name, uid: reviewed(value).uid,
    labels: at(value, "metadata", "labels") ?? {}, finalizers: at(value, "metadata", "finalizers") ?? [],
    annotations: privateFields ? fields : Object.fromEntries(Object.entries(fields).filter(([key]) => !key.startsWith(P))),
    spec: value.spec ?? {} });
}

function grantBinding(value: ObjectValue): ObjectValue {
  return { name: reviewed(value).name, uid: reviewed(value).uid, namespace: at(value, "metadata", "namespace")!, spec: value.spec! };
}

export function quiescent(plan: Plan): ObjectValue { return { ...record(plan.grant.spec), writers: [] }; }

export function originalActivation(plan: Plan): PrivateActivation {
  const proof = record(JSON.parse(plan.seal));
  const value = proof.activation as unknown as PrivateActivation;
  validateActivationShape(value, "qualified");
  if (proof.version !== 2 || canonical(proof) !== plan.seal || digest(proof) !== plan.review.predecessor) throw new Error(FAILURE);
  const complete = plan.review.activation;
  if (retirementBinding({ ...complete, namespaces: value.namespaces }) !== retirementBinding(value)
    || value.namespaces.some(scope => {
      const current = structuredClone(complete.namespaces.find(entry => entry.namespace.uid === scope.namespace.uid));
      if (current) for (const consumer of current.consumers) {
        const rollout = plan.review.credentialRollouts?.find(entry => entry.namespaceUid === scope.namespace.uid
          && entry.deploymentUid === consumer.object.uid);
        if (rollout && consumer.object.uid !== complete.root.deployment.uid && consumer.templateDigest === rollout.template) {
          consumer.templateDigest = rollout.baseline;
        }
      }
      return !current || current.epoch !== scope.epoch || retirementBinding({ ...value, namespaces: [current] }) !== retirementBinding({ ...value, namespaces: [scope] });
    })) throw new Error(FAILURE);
  return structuredClone(complete);
}

export function supportedTopology(review: PrivateRootMigrationReview): void {
  const a = review.activation, scope = a.namespaces.find(value => value.namespace.uid === a.root.namespace.uid);
  validateActivationShape(a, "qualified");
  if (review.version !== 1 || review.phase !== "reviewed" || !scope
    || scope.namespace.uid !== a.root.namespace.uid || scope.namespace.name !== a.root.namespace.name
    || !scope.consumers.length || a.namespaces.some(value => value.consumers.some(consumer => consumer.kind !== "Deployment"))
    || scope.consumers.filter(consumer => consumer.object.uid === a.root.deployment.uid).length !== 1
    || (review.recovery && (a.namespaces.length !== 1 || scope.consumers.length !== 1)) || a.root.deployment.name !== "kars-controller"
    || a.root.replicaIntent < 1 || review.grants.length !== 1 || review.grants[0]?.namespace !== a.root.namespace.name
    || (a.root.budgetTls && a.root.budgetTls.namespace.uid !== a.root.namespace.uid)) throw new Error(FAILURE);
}

export function routerBaselineTarget(source: RootMigrationReceipt, original: ObjectValue): ObjectValue {
  if (source.supersededBy || source.plan.restartFrom || source.phase !== "qualifying" || source.publication || source.controlTransition
    || source.bundleTransitions?.length || !source.retirement) throw new Error("Router baseline restart requires an unpublished, sealed qualification with no control transition");
  validateReceipt(source);
  const env = (spec: ObjectValue): ObjectValue[] => {
    const containers = at(spec, "template", "spec", "containers");
    const controllers = Array.isArray(containers) ? containers.filter(value => at(value, "name") === "controller") : [];
    const entries = controllers.length === 1 ? at(controllers[0], "env") : undefined;
    if (!Array.isArray(entries)) throw new Error(FAILURE);
    return entries.map(record);
  };
  const key = "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST";
  const prior = env(record(source.plan.original.spec));
  const baseline = prior.filter(value => value.name === "KARS_INFERENCE_BUDGET_ROUTER_DIGEST");
  const corrected = structuredClone(source.plan.targetSpec), entries = env(corrected);
  if (prior.some(value => value.name === key) || entries.some(value => value.name === key)
    || baseline.length !== 1 || typeof baseline[0]!.value !== "string") throw new Error("Router baseline restart only corrects an omitted original verifier");
  // Validate the correction against the genuinely reviewed predecessor, not the new router pin.
  const controller = (at(corrected, "template", "spec", "containers") as ObjectValue[]).find(value => value.name === "controller")!;
  controller.env = [...entries, { name: key, value: baseline[0]!.value }];
  assertRootImageDelta(record(source.plan.original.spec), corrected);
  if (reviewed(original).uid !== reviewed(source.plan.original).uid
    || canonical({ ...executionSpec(original.spec), replicas: 0 }) !== canonical({ ...executionSpec(source.plan.targetSpec), replicas: 0 })) throw new Error(FAILURE);
  const result = structuredClone(record(original.spec));
  const liveController = (at(result, "template", "spec", "containers") as ObjectValue[]).find(value => value.name === "controller")!;
  liveController.env = controller.env;
  return result;
}

function assertRestartPlan(plan: Plan): void {
  const source = plan.restartFrom!;
  if (canonical(plan.targetSpec) !== canonical(routerBaselineTarget(source, plan.original))
    || canonical(plan.grant) !== canonical(source.plan.grant) || canonical(plan.guard) !== canonical(source.plan.guard)
    || canonical(plan.review.release) !== canonical(source.plan.review.release)
    || plan.review.recovery || plan.review.companionImages?.length || plan.companionTargets?.length) throw new Error(FAILURE);
  const proof = record(JSON.parse(plan.seal)), state = retirementState({ metadata: { annotations: { [FIELD]: plan.seal } } });
  const old = source.retirement!, activation = originalActivation(plan);
  if (!state || proof.version !== 2 || state.attempt !== old.attempt || state.binding !== old.binding
    || canonical(state.captured) !== canonical(old.captured) || canonical(state.exposedKeys) !== canonical(old.exposedKeys)
    || canonical(state.baseline) !== canonical(old.baseline)
    || activation.root.replicaIntent !== source.plan.review.activation.root.replicaIntent
    || activation.root.account.uid !== source.plan.review.activation.root.account.uid
    || canonical(activation.controllerUids) !== canonical(source.plan.review.activation.controllerUids)
    || activation.bundleRevision !== publicationBundleRevision(source)
    || activation.namespaces.length !== source.plan.review.activation.namespaces.length
    || (plan.companions ?? []).length !== (source.plan.companions ?? []).length) throw new Error(FAILURE);
  for (const before of source.plan.companions ?? []) {
    const next = plan.companions?.find(value => reviewed(value).uid === reviewed(before).uid);
    const target = source.plan.companionTargets?.find(value => value.uid === reviewed(before).uid)?.spec ?? before.spec;
    if (!next || replicaIntent(next) !== replicaIntent(before)
      || canonical(executionSpec(next.spec)) !== canonical(executionSpec(target))) throw new Error(FAILURE);
  }
}

export function assertTarget(plan: Plan): void {
  if (plan.restartFrom) {
    assertRestartPlan(plan);
  } else if (canonical(plan.original.spec) === canonical(plan.targetSpec) && plan.review.companionImages?.length) {
    immutableMigrationImages(plan.targetSpec);
  } else assertRootImageDelta(record(plan.original.spec), plan.targetSpec);
  if (digest(plan.targetSpec) !== plan.review.target.specDigest
    || templateDigest({ spec: plan.targetSpec }) !== plan.review.target.templateDigest
    || templateDigest(plan.original) !== plan.review.activation.root.templateDigest) throw new Error(FAILURE);
  const recovery = plan.review.recovery;
  if (recovery && (canonical(recovery.historicalRoot) !== canonical(plan.original)
    || reviewed(recovery.observedRoot).uid !== plan.review.activation.root.deployment.uid
    || canonical(recovery.observedRoot.spec) !== canonical(plan.targetSpec))) throw new Error(FAILURE);
}

export function retiringRoot(plan: Plan): ObjectValue {
  return plan.review.recovery?.observedRoot ?? plan.original;
}

export function retiringExecution(plan: Plan): PrivateActivation {
  const activation = originalActivation(plan);
  if (plan.review.recovery) {
    activation.phase = "reviewed";
    activation.root.templateDigest = templateDigest(retiringRoot(plan));
    activation.namespaces[0]!.consumers[0]!.templateDigest = activation.root.templateDigest;
  }
  return activation;
}

export async function predecessorAuthority(run: Execute, plan: Plan, qualified = false, retiring = false): Promise<void> {
  const old = originalActivation(plan);
  if (!plan.review.recovery) {
    await (qualified ? validateQualifiedActivation(run, old, true)
      : retiring ? reviewSealedAuthorityMetadata(run, old) : validateQualifiedMetadata(run, old));
    return;
  }
  await reviewMigrationPredecessor(run, old, plan.original);
  const current = await read(run, "deployment", old.root.deployment.name, old.root.namespace.name);
  const spec = retiringRoot(plan).spec;
  if (reviewed(current).uid !== old.root.deployment.uid
    || ![spec, { ...record(spec), replicas: 0 }].some(value => canonical(value) === canonical(current.spec))) throw new Error(FAILURE);
}

export async function retiredPredecessor(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
  const old = originalActivation(receipt.plan);
  if (!receipt.plan.review.recovery) return verifyRetiredSealedRoot(run, old,
    (receipt.plan.companions ?? []).map(value => reviewed(value).uid), true, true);
  await predecessorAuthority(run, receipt.plan);
  const root = await read(run, "deployment", old.root.deployment.name, old.root.namespace.name);
  if (at(root, "spec", "replicas") !== 0) throw new Error(FAILURE);
  await assertNoPods(run, receipt, old);
}

/** Native workload-controller replay uses the same pinned UID as the activation, not a name-only impersonation. */
export function replay(run: Execute, activation: PrivateActivation): Execute {
  return (args, input) => {
    if (args[0] !== "create" || args[1] !== "--raw" || !args[2]?.includes("/pods?dryRun=All&fieldValidation=Strict")) return run(args, input);
    const identity = activation.profile === "service-accounts"
      ? ["--as", "system:serviceaccount:kube-system:replicaset-controller", "--as-uid",
        activation.controllerUids["replicaset-controller"]!, "--as-group", "system:serviceaccounts",
        "--as-group", "system:serviceaccounts:kube-system", "--as-group", "system:authenticated"]
      : ["--as", "system:kube-controller-manager", "--as-group", "system:authenticated"];
    if (identity.some(value => !value)) throw new Error(FAILURE);
    return run([...args, ...identity], input);
  };
}

export async function authority(run: Execute): Promise<void> {
  if ((await run(["auth", "can-i", "manage", `${GRANTS}/workspace`, "--all-namespaces"])).trim() !== "yes") throw new Error("Cluster-scoped credential operator authority is required");
}

function unrelatedNamespace(value: ObjectValue, plan: Plan): boolean {
  const identity = reviewed(value);
  const referencesIdentity = (entry: unknown): boolean => {
    if (typeof entry === "string") return entry === identity.name || entry === identity.uid;
    return !!entry && typeof entry === "object" && Object.values(entry).some(referencesIdentity);
  };
  const metadata = record(value.metadata);
  const authorityFields = [metadata.labels, metadata.annotations].flatMap(fields => Object.keys(record(fields ?? {})));
  if (authorityFields.some(key => key.startsWith(P) || key.startsWith("kars.azure.com/credential-reader-"))
    || (Array.isArray(metadata.finalizers) && metadata.finalizers.length > 0)) return false;
  return !referencesIdentity([plan.review.activation, plan.inventory.grants, plan.guard]);
}

export function companionReferences(plan: Plan): string[] {
  return (plan.companions ?? []).map(value => `${at(value, "metadata", "namespace")}/Deployment/${reviewed(value).name}`);
}

export function scopeSnapshot(receipt: RootMigrationReceipt, uid: string): ObjectValue {
  const value = uid === receipt.plan.review.activation.root.namespace.uid ? receipt.namespace : receipt.namespaces?.find(value => reviewed(value).uid === uid);
  return value ?? receipt.plan.inventory.namespaces.find(value => reviewed(value).uid === uid)!;
}

export function pendingScope(receipt: RootMigrationReceipt, before: ObjectValue): ObjectValue {
  const scope = receipt.activation!.namespaces.find(value => value.namespace.uid === reviewed(before).uid)!;
  return { ...record(at(before, "metadata", "annotations")), ...annotations(receipt.activation!, scope, "Pending") };
}

function executionSpec(spec: unknown): ObjectValue {
  const copy = structuredClone(record(spec));
  const fields = at(copy, "template", "metadata", "annotations");
  if (fields) {
    delete record(fields)[`${P}epoch`];
    if (!Object.keys(record(fields)).length) delete record(at(copy, "template", "metadata")).annotations;
  }
  return copy;
}

export async function companionFence(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
  const { plan } = receipt;
  for (const before of plan.companions ?? []) {
    const ns = String(at(before, "metadata", "namespace"));
    const namespace = await read(run, "namespace", ns);
    const epoch = at(namespace, "metadata", "annotations", `${P}epoch`);
    const current = await read(run, "deployment", reviewed(before).name, ns);
    const replicas = replicaIntent(current);
    const templateEpoch = at(current, "spec", "template", "metadata", "annotations", `${P}epoch`);
    const oldEpoch = at(before, "spec", "template", "metadata", "annotations", `${P}epoch`);
    const restoring = ["qualifying", "complete"].includes(receipt.phase);
    const update = receipt.companionUpdates?.find(value => value.uid === reviewed(before).uid);
    const target = plan.companionTargets?.find(value => value.uid === reviewed(before).uid);
    const specs = update?.phase === "updated" ? [target!.spec]
      : update?.phase === "updating" ? [before.spec, target!.spec] : [before.spec];
    const allowed = ["prepared", "quiescent", "retiring"].includes(receipt.phase) || restoring
      ? [0, rootControlReplicas(before, receipt.controlTransition?.applied ? receipt.controlTransition : undefined)] : [0];
    if (reviewed(current).uid !== reviewed(before).uid || !allowed.includes(replicas)
      || (templateEpoch !== oldEpoch && (!restoring || typeof epoch !== "string" || templateEpoch !== epoch))
      || !specs.some(spec => canonical({ ...executionSpec(current.spec), replicas: 0 }) === canonical({
        ...executionSpec(rootControlSpec(record(spec), reviewed(before).uid, receipt.controlTransition?.applied ? receipt.controlTransition : undefined)), replicas: 0,
      }))) {
      throw new Error("Reviewed companion Deployment changed during root migration");
    }
  }
}

export async function scopeFence(run: Execute, receipt: RootMigrationReceipt, options: {
  rootFlexible?: boolean; grantSpec?: ObjectValue; allowInitialGrant?: boolean; writerTransition?: boolean;
  qualification?: PrivateActivation;
} = {}): Promise<void> {
  const { plan } = receipt, rootUid = plan.review.activation.root.namespace.uid;
  await companionFence(run, receipt);
  const actual = await inventories(run);
  const actualUids = new Set(actual.namespaces.map(value => reviewed(value).uid));
  const expectedNs = plan.inventory.namespaces.filter(value => {
    // The new controller may retire terminal public sandboxes while it qualifies.
    // Missing private scopes remain forbidden; additions and replacements still fail below.
    return actualUids.has(reviewed(value).uid) || (receipt.phase !== "qualifying" && receipt.phase !== "complete")
      || !unrelatedNamespace(value, plan);
  }).map(value => reviewed(value).uid === rootUid && receipt.namespace ? receipt.namespace
    : receipt.namespaces?.find(scope => reviewed(scope).uid === reviewed(value).uid) ?? value);
  const reconciled = new Map<string, string[]>();
  if (options.rootFlexible && options.qualification && ["qualifying", "complete"].includes(receipt.phase)) {
    for (const scope of options.qualification.namespaces) {
      if (scope.namespace.uid === rootUid) continue;
      const current = actual.namespaces.find(value => reviewed(value).uid === scope.namespace.uid);
      const before = expectedNs.find(value => reviewed(value).uid === scope.namespace.uid);
      if (!current || !before) throw new Error(FAILURE);
      for (const consumer of scope.consumers) {
        const key = `${P}reconciled-${consumer.object.uid}`;
        if (at(current, "metadata", "annotations", key) === at(before, "metadata", "annotations", key)) continue;
        const deployment = await read(run, "deployment", consumer.object.name, scope.namespace.name);
        if (!await reviewedControllerRollout(run, scope, consumer, deployment)) throw new Error(FAILURE);
        reconciled.set(scope.namespace.uid, [...reconciled.get(scope.namespace.uid) ?? [], key]);
      }
    }
  }
  const nsKey = (values: ObjectValue[]) => values.map(value => {
    const uid = reviewed(value).uid;
    const enrolled = plan.review.activation.namespaces.some(scope => scope.namespace.uid === uid);
    let compared = value;
    if (["retiring", "cutover"].includes(receipt.phase) && enrolled && uid !== rootUid) {
      const before = scopeSnapshot(receipt, uid);
      const fields = at(value, "metadata", "annotations");
      const allowed = [record(at(before, "metadata", "annotations")),
        { ...record(at(before, "metadata", "annotations")), [`${P}state`]: "Pending" },
        ...(receipt.phase === "cutover" ? [pendingScope(receipt, before)] : [])];
      if (!allowed.some(expected => canonical(expected) === canonical(fields))) throw new Error(FAILURE);
      compared = { ...value, metadata: { ...record(value.metadata), annotations: at(before, "metadata", "annotations")! } };
    }
    const transition = receipt.bundleTransitions?.at(-1);
    if (transition && !transition.applied && enrolled && uid !== rootUid) {
      const fields = { ...record(at(compared, "metadata", "annotations")) };
      if (![transition.activation.bundleRevision, transition.review.revision].includes(String(fields[`${P}bundle-revision`]))) throw new Error(FAILURE);
      fields[`${P}bundle-revision`] = transition.activation.bundleRevision;
      compared = { ...compared, metadata: { ...record(compared.metadata), annotations: fields } };
    }
    const binding = nsBinding(compared, !(options.rootFlexible && uid === rootUid));
    if (options.rootFlexible && enrolled && uid !== rootUid && ["qualifying", "complete"].includes(receipt.phase)) {
      const fields = record(binding.annotations);
      const scope = plan.review.activation.namespaces.find(scope => scope.namespace.uid === uid)!;
      // Qualification changes only epoch/rotation evidence, never namespace affiliation or consumer membership.
      for (const suffix of ["state", "epoch", "budget-tls-version", "budget-key", "budget-qualified-bundle",
        "budget-qualified-key", "budget-qualified-secret", "budget-rotation-bundle", "budget-before-key",
        ...scope.consumers.map(consumer => `parent-${consumer.object.uid}`)]) delete fields[`${P}${suffix}`];
      for (const key of reconciled.get(uid) ?? []) delete fields[key];
    }
    if (options.writerTransition && reviewed(value).uid === rootUid) {
      const key = plan.guard.key;
      for (const field of ["labels", "annotations"]) {
        const fields = record(binding[field]);
        const restoringWriters = options.grantSpec?.enabled !== false && Array.isArray(options.grantSpec?.writers)
          && options.grantSpec.writers.some(writer => at(writer, "namespace") === plan.review.release.namespace);
        const expected = restoringWriters
          ? (field === "labels" ? rootUid : plan.review.activation.root.account.uid)
          : at(plan.inventory.namespaces.find(ns => reviewed(ns).uid === rootUid), "metadata", field, key);
        if (fields[key] !== undefined && fields[key] !== expected) throw new Error(FAILURE);
        delete fields[key];
      }
      binding.finalizers = (binding.finalizers as string[]).filter(value => value !== key);
    }
    return binding;
  });
  if (canonical(nsKey(actual.namespaces)) !== canonical(nsKey(expectedNs))) throw new Error("Migration namespace inventory changed");
  const selected = reviewed(plan.grant).uid;
  const expectedGrants = plan.inventory.grants.map(value => reviewed(value).uid === selected
    ? { ...value, spec: options.grantSpec ?? quiescent(plan) } : value);
  if (options.allowInitialGrant) {
    const current = actual.grants.find(value => reviewed(value).uid === selected);
    if (current && canonical(current.spec) === canonical(plan.grant.spec)) {
      const item = expectedGrants.find(value => reviewed(value).uid === selected)!;
      item.spec = plan.grant.spec!;
    }
  }
  if (canonical(actual.grants.map(grantBinding)) !== canonical(expectedGrants.map(grantBinding))) throw new Error("Migration grant inventory changed");
}

export async function noOwnedAuthority(run: Execute, plan: Plan): Promise<void> {
  const grant = await read(run, GRANTS, "workspace", plan.review.release.namespace);
  const conditions = at(grant, "status", "conditions");
  if (reviewed(grant).uid !== reviewed(plan.grant).uid || canonical(grant.spec) !== canonical(quiescent(plan))
    || at(grant, "metadata", "generation") === undefined
    || at(grant, "status", "observedGeneration") !== at(grant, "metadata", "generation")
    || !Array.isArray(conditions) || !conditions.some(c => at(c, "type") === "WriterReady" && at(c, "status") === "False")) {
    throw new Error("Migration requires controller-acknowledged writer retirement");
  }
  const roles = record(JSON.parse(await run(["get", "roles,rolebindings,clusterroles,clusterrolebindings", "--all-namespaces", "--chunk-size=0", "-o", "json"])));
  if (!Array.isArray(roles.items) || at(roles, "metadata", "continue")
    || roles.items.some(value => at(value, "metadata", "annotations", "kars.azure.com/credential-grant-owner") === reviewed(plan.grant).uid)) {
    throw new Error("Migration still has grant-owned read authority or an incomplete inventory");
  }
}

export async function sameBudget(run: Execute, plan: Plan): Promise<void> {
  const original = originalActivation(plan), current = await reviewBudgetTls(run, plan.original, original.root.namespace.name);
  const binding = (value: typeof current) => value ? { secret: value.secret, keyDigest: value.keyDigest, namespace: value.namespace.uid } : null;
  if (canonical(binding(current)) !== canonical(binding(original.root.budgetTls))) throw new Error("Budget changed before the post-retirement rotation boundary");
}

export async function patchSpec(run: Execute, before: ObjectValue, spec: ObjectValue, fence: () => Promise<void>): Promise<void> {
  const args = ["patch", "deployment", reviewed(before).name, "-n", String(at(before, "metadata", "namespace")),
    "--type=json", "--patch-file=/dev/stdin", "-o", "json"];
  const patch = JSON.stringify([
    { op: "test", path: "/metadata/uid", value: reviewed(before).uid },
    { op: "test", path: "/metadata/resourceVersion", value: reviewed(before).resourceVersion },
    { op: "test", path: "/spec", value: before.spec },
    { op: "replace", path: "/spec", value: spec },
  ]);
  const projected = record(JSON.parse(await run([...args, "--dry-run=server"], patch)));
  if (reviewed(projected).uid !== reviewed(before).uid || canonical(projected.spec) !== canonical(spec)) throw new Error("Migration server projection differs");
  await fence();
  const result = record(JSON.parse(await run(args, patch)));
  if (reviewed(result).uid !== reviewed(before).uid || canonical(result.spec) !== canonical(spec)) throw new Error("Migration acknowledgement differs");
}

export async function patchFields(run: Execute, before: ObjectValue, fields: ObjectValue): Promise<void> {
  const result = record(JSON.parse(await run(["patch", "namespace", reviewed(before).name, "--type=json", "--patch-file=/dev/stdin", "-o", "json"],
    JSON.stringify([
      { op: "test", path: "/metadata/uid", value: reviewed(before).uid },
      { op: "test", path: "/metadata/resourceVersion", value: reviewed(before).resourceVersion },
      { op: "test", path: "/metadata/annotations", value: at(before, "metadata", "annotations") },
      { op: "replace", path: "/metadata/annotations", value: fields },
    ]))));
  if (reviewed(result).uid !== reviewed(before).uid || canonical(at(result, "metadata", "annotations")) !== canonical(fields)) throw new Error("Migration namespace acknowledgement differs");
}

export async function save(journal: RootMigrationJournal, receipt: RootMigrationReceipt): Promise<void> {
  if (Buffer.byteLength(canonical(receipt)) > 2_097_152) throw new Error("Migration receipt exceeds 2 MiB");
  await journal.save(structuredClone(receipt));
}

function validateForeignSnapshots(activation: PrivateActivation, snapshots: ObjectValue[] | undefined, revision: string): void {
  const scopes = activation.namespaces.filter(scope => scope.namespace.uid !== activation.root.namespace.uid);
  const values = snapshots ?? [];
  if (!Array.isArray(values) || values.length !== scopes.length
    || new Set(values.map(value => reviewed(value).uid)).size !== values.length) throw new Error(FAILURE);
  for (const value of values) {
    const identity = reviewed(value), scope = scopes.find(scope => scope.namespace.uid === identity.uid);
    if (!scope || identity.name !== scope.namespace.name
      || at(value, "metadata", "annotations", `${P}bundle-revision`) !== revision) throw new Error(FAILURE);
  }
}

export function publicationBundleRevision(receipt: RootMigrationReceipt): string {
  let revision = receipt.plan.review.activation.bundleRevision;
  const transitions = receipt.bundleTransitions ?? [];
  for (const [index, transition] of transitions.entries()) {
    validateForeignSnapshots(transition.activation, transition.namespaces, revision);
    if (transition.activation.bundleRevision !== revision || transition.retirement.phase !== "retired"
      || transition.retirement.binding !== retirementBinding(transition.activation)
      || digest(transition.review.identities) !== transition.review.revision
      || reviewed(transition.namespace).uid !== receipt.plan.review.activation.root.namespace.uid
      || at(transition.namespace, "metadata", "annotations", `${P}bundle-revision`) !== revision
      || canonical(retirementState(transition.namespace)) !== canonical(transition.retirement)
      || canonical({ ...transition.retirement, binding: "" }) !== canonical({ ...receipt.retirement, binding: "" })
      || (receipt.activation && retirementBinding({ ...transition.activation, bundleRevision: "" })
        !== retirementBinding({ ...receipt.activation, bundleRevision: "" }))
      || (!transition.applied && (canonical(transition.activation) !== canonical(receipt.activation)
        || canonical(transition.retirement) !== canonical(receipt.retirement)))
      || transition.review.revision === revision || (!transition.applied && index !== transitions.length - 1)) throw new Error(FAILURE);
    if (transition.applied) revision = transition.review.revision;
  }
  if (receipt.activation && receipt.activation.bundleRevision !== revision) throw new Error(FAILURE);
  return revision;
}

export function validateReceipt(receipt: RootMigrationReceipt): void {
  if (receipt?.version !== 1 || receipt.planDigest !== digest(receipt.plan)
    || !["prepared", "quiescent", "retiring", "cutover", "rotation-required", "qualifying", "complete"].includes(receipt.phase)) throw new Error(FAILURE);
  supportedTopology(receipt.plan.review);
  originalActivation(receipt.plan);
  publicationBundleRevision(receipt);
  assertTarget(receipt.plan);
  const { plan } = receipt;
  const consumers = plan.review.activation.namespaces.flatMap(scope => scope.consumers.map(consumer => ({ ...consumer, namespace: scope.namespace.name })))
    .filter(value => value.object.uid !== plan.review.activation.root.deployment.uid);
  const companions = plan.companions ?? [];
  if (!Array.isArray(companions) || companions.length !== consumers.length
    || new Set(companions.map(value => reviewed(value).uid)).size !== companions.length) throw new Error(FAILURE);
  for (const value of companions) {
    const consumer = consumers.find(item => item.object.uid === reviewed(value).uid);
    if (!consumer || consumer.object.name !== reviewed(value).name || value.kind !== "Deployment"
      || at(value, "metadata", "namespace") !== consumer.namespace
      || templateDigest(value) !== consumer.templateDigest) throw new Error(FAILURE);
    replicaIntent(value);
  }
  if (receipt.controlTransition) {
    validateRootControlTransition(receipt.controlTransition);
    const prior = retirementState({ metadata: { annotations: { [FIELD]: receipt.controlTransition.before } } });
    if (!["qualifying", "complete"].includes(receipt.phase) || !receipt.retirement || !prior
      || canonical(prior) !== canonical({ ...receipt.retirement, phase: "restoring" })
      || (receipt.phase === "complete" && !receipt.controlTransition.applied)) throw new Error(FAILURE);
    for (const change of receipt.controlTransition.consumers) {
      const captured = companions.find(value => reviewed(value).uid === reviewed(change.before).uid);
      if (!captured || canonical({ ...executionSpec(captured.spec), replicas: 0 }) !== canonical(executionSpec(change.before.spec))) throw new Error(FAILURE);
    }
  }
  const changes = plan.review.companionImages ?? [], targets = plan.companionTargets ?? [], updates = receipt.companionUpdates ?? [];
  if (!Array.isArray(changes) || !Array.isArray(targets) || !Array.isArray(updates)
    || (plan.review.recovery && changes.length) || targets.length !== changes.length
    || new Set(changes.map(value => value.identity.uid)).size !== changes.length
    || new Set(targets.map(value => value.uid)).size !== targets.length
    || new Set(updates.map(value => value.uid)).size !== updates.length
    || (updates.length && ["prepared", "quiescent", "retiring"].includes(receipt.phase))) throw new Error(FAILURE);
  for (const change of changes) {
    const before = companions.find(value => reviewed(value).uid === change.identity.uid);
    const target = targets.find(value => value.uid === change.identity.uid);
    if (!before || !target || at(before, "metadata", "namespace") !== plan.review.release.namespace || canonical(reviewed(before)) !== canonical(change.identity)
      || digest(before.spec) !== change.original.specDigest || templateDigest(before) !== change.original.templateDigest
      || digest(target.spec) !== change.target.specDigest || templateDigest({ spec: target.spec }) !== change.target.templateDigest
      || canonical(companionImageDelta(record(before.spec), target.spec)) !== canonical(change.images)) throw new Error(FAILURE);
  }
  for (const update of updates) {
    if (!targets.some(value => value.uid === update.uid) || !["updating", "updated"].includes(update.phase)
      || (update.phase === "updating" && receipt.phase !== "cutover")) throw new Error(FAILURE);
  }
  if (["rotation-required", "qualifying", "complete"].includes(receipt.phase)
    && targets.some(value => !updates.some(update => update.uid === value.uid && update.phase === "updated"))) throw new Error(FAILURE);
  if (["cutover", "rotation-required", "qualifying", "complete"].includes(receipt.phase) ) {
    if (!receipt.activation || receipt.retirement?.binding !== retirementBinding(receipt.activation)) throw new Error(FAILURE);
    if (!["reviewed", "qualified"].includes(receipt.activation.phase)) throw new Error(FAILURE);
    validateActivationShape(receipt.activation, receipt.activation.phase);
    if (receipt.activation.namespaces.length !== plan.review.activation.namespaces.length) throw new Error(FAILURE);
    for (const scope of plan.review.activation.namespaces) {
      const currentScope = receipt.activation.namespaces.find(value => value.namespace.uid === scope.namespace.uid);
      if (currentScope?.namespace.name !== scope.namespace.name || currentScope.consumers.length !== scope.consumers.length) throw new Error(FAILURE);
      for (const consumer of scope.consumers) {
        const actual = currentScope.consumers.find(value => value.object.uid === consumer.object.uid);
        const expected = consumer.object.uid === plan.review.activation.root.deployment.uid ? plan.review.target.templateDigest
          : changes.find(value => value.identity.uid === consumer.object.uid)?.target.templateDigest ?? consumer.templateDigest;
        if (!actual || actual.kind !== consumer.kind || actual.object.name !== consumer.object.name
          || actual.templateDigest !== expected) throw new Error(FAILURE);
      }
    }
    if (receipt.phase !== "cutover" || receipt.namespaces) {
      validateForeignSnapshots(receipt.activation, receipt.namespaces, receipt.activation.bundleRevision);
    }
  }
}

export async function assertNoPods(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation): Promise<void> {
  const captured = new Set(receipt.captured?.map(value => at(value, "identity", "uid")));
  for (const scope of activation.namespaces) for (const pod of await inventory(run, "pods", scope.namespace.name)) {
    if (captured.has(reviewed(pod, true).uid) || await requiresPrivatePodReview(run, pod, scope, activation)
      || await reviewedOwner(run, pod, scope, undefined, activation.root)) {
      throw new Error("Private execution reappeared during migration; Pending protection was preserved");
    }
  }
}

export async function rotatedPublicCa(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation): Promise<void> {
  const budget = activation.root.budgetTls;
  if (!budget) return;
  const before = receipt.plan.publicCa!, state = receipt.retirement!;
  if (!state.baseline || budget.secret.uid !== state.baseline.secretUid || state.exposedKeys.includes(budget.keyDigest)
    || budget.secret.resourceVersion === state.baseline.resourceVersion) throw new Error("Post-retirement budget key rotation is required");
  const ca = await read(run, "configmap", reviewed(before).name, budget.namespace.name);
  const text = at(ca, "data", "ca.crt");
  const expected = structuredClone(before);
  record(expected.data)["ca.crt"] = text!;
  const body = (value: ObjectValue) => {
    const copy = structuredClone(value);
    delete record(copy.metadata).resourceVersion;
    delete record(copy.metadata).managedFields;
    const fields = at(copy, "metadata", "annotations");
    if (fields) {
      delete record(fields)[CORE_PUBLICATION_FENCE];
      if (!Object.keys(record(fields)).length) delete record(copy.metadata).annotations;
    }
    return canonical(copy);
  };
  if (typeof text !== "string" || text.includes("PRIVATE KEY") || text === at(before, "data", "ca.crt")
    || reviewed(ca).resourceVersion === reviewed(before).resourceVersion || body(ca) !== body(expected)) throw new Error("Public CA must be updated in place without changing budget contracts or caps");
  const raw = await run(["get", "secret", budget.secret.name, "-n", budget.namespace.name, "-o", 'go-template={{index .data "tls.crt"}}']);
  const leaf = new X509Certificate(Buffer.from(raw.trim(), "base64"));
  const certificates = text.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g);
  if (!certificates?.length || certificates.length > 8
    || !certificates.some(pem => { const caCert = new X509Certificate(pem); return caCert.ca && leaf.verify(caCert.publicKey); })) {
    throw new Error("Updated public CA does not verify the rotated budget certificate");
  }
  const after = await reviewBudgetTls(run, await read(run, "deployment", activation.root.deployment.name, activation.root.namespace.name), activation.root.namespace.name);
  if (canonical(after) !== canonical(budget)
    || canonical(await read(run, "configmap", reviewed(ca).name, budget.namespace.name)) !== canonical(ca)) throw new Error(FAILURE);
}
