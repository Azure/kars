// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, createPrivateKey, createPublicKey, X509Certificate } from "node:crypto";
import {
  annotations, assertNoCorePublication, at, canonical, CORE_PUBLICATION, CORE_PUBLICATION_FENCE, digest, PRIVATE_PREFIX as P, previewPrivateActivation,
  read, record, reviewed, reviewedOwner, requiresPrivatePodReview, reviewBudgetTls, reviewSealedAuthorityMetadata,
  stagePrivateActivation, templateDigest, validateActivationShape, validatePrivateActivation, validateQualifiedActivation,
  validateQualifiedMetadata, type Execute, type PrivateActivation,
} from "./private-activation.js";
import { PrivateQualificationConsumerChanged, reviewPrivateContinuity, verifyRetiredSealedRoot } from "./private-activation-continuity.js";
import { captureGuardRetirement, type GuardRetirementReview } from "./private-activation-guard-retirement.js";
import { reviewedControllerRollout } from "./private-activation-reconciliation.js";
import { retireGrantWriters } from "./private-grant-writer-retirement.js";
import type { HistoricalHookProof } from "./private-core-history.js";
import {
  assertRetiredRoot, recordRetiredBudgetBaseline, replicaIntent, retirementBinding, retirementState, startRetirement, type RootRetirement,
} from "./private-activation-retirement.js";
import { previewPrivateRootMigration, reviewMigrationPredecessor, rootMigrationReviewDigest, type PrivateRootMigrationReview } from "./private-root-migration-review.js";
import { assertRootImageDelta } from "./private-root-image-delta.js";
import { companionImageDelta, immutableMigrationImages } from "./private-root-migration-images.js";
import { reviewPublicationBundle, type PublicationBundleReview } from "./private-publication-bundle.js";
import {
  applyRootControlTransition, prepareRootControlTransition, rootControlReplicas, rootControlRetirement, rootControlSpec,
  validateRootControlTransition, type RootControlTransition,
} from "./private-root-control-transition.js";

type ObjectValue = ReturnType<typeof record>;
const GRANTS = "karscredentialgrants.kars.azure.com";
const FIELD = `${P}root-retirement`;
const FAILURE = "Private root migration changed or exceeds the supported one-root Deployment topology; preserve the receipt and protection";

interface Inventory { namespaces: ObjectValue[]; grants: ObjectValue[] }
interface Plan {
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

async function inventory(run: Execute, resource: string, namespace?: string): Promise<ObjectValue[]> {
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
async function inventories(run: Execute): Promise<Inventory> {
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
function quiescent(plan: Plan): ObjectValue { return { ...record(plan.grant.spec), writers: [] }; }
function originalActivation(plan: Plan): PrivateActivation {
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
function supportedTopology(review: PrivateRootMigrationReview): void {
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
function routerBaselineTarget(source: RootMigrationReceipt, original: ObjectValue): ObjectValue {
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

function assertTarget(plan: Plan): void {
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
function retiringRoot(plan: Plan): ObjectValue {
  return plan.review.recovery?.observedRoot ?? plan.original;
}
function retiringExecution(plan: Plan): PrivateActivation {
  const activation = originalActivation(plan);
  if (plan.review.recovery) {
    activation.phase = "reviewed";
    activation.root.templateDigest = templateDigest(retiringRoot(plan));
    activation.namespaces[0]!.consumers[0]!.templateDigest = activation.root.templateDigest;
  }
  return activation;
}
async function predecessorAuthority(run: Execute, plan: Plan, qualified = false, retiring = false): Promise<void> {
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
async function retiredPredecessor(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
  const old = originalActivation(receipt.plan);
  if (!receipt.plan.review.recovery) return verifyRetiredSealedRoot(run, old,
    (receipt.plan.companions ?? []).map(value => reviewed(value).uid), true, true);
  await predecessorAuthority(run, receipt.plan);
  const root = await read(run, "deployment", old.root.deployment.name, old.root.namespace.name);
  if (at(root, "spec", "replicas") !== 0) throw new Error(FAILURE);
  await assertNoPods(run, receipt, old);
}

/** Native workload-controller replay uses the same pinned UID as the activation, not a name-only impersonation. */
function replay(run: Execute, activation: PrivateActivation): Execute {
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
async function authority(run: Execute): Promise<void> {
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

function companionReferences(plan: Plan): string[] {
  return (plan.companions ?? []).map(value => `${at(value, "metadata", "namespace")}/Deployment/${reviewed(value).name}`);
}

function scopeSnapshot(receipt: RootMigrationReceipt, uid: string): ObjectValue {
  const value = uid === receipt.plan.review.activation.root.namespace.uid ? receipt.namespace : receipt.namespaces?.find(value => reviewed(value).uid === uid);
  return value ?? receipt.plan.inventory.namespaces.find(value => reviewed(value).uid === uid)!;
}

function pendingScope(receipt: RootMigrationReceipt, before: ObjectValue): ObjectValue {
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

async function companionFence(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
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

async function scopeFence(run: Execute, receipt: RootMigrationReceipt, options: {
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
async function noOwnedAuthority(run: Execute, plan: Plan): Promise<void> {
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
async function sameBudget(run: Execute, plan: Plan): Promise<void> {
  const original = originalActivation(plan), current = await reviewBudgetTls(run, plan.original, original.root.namespace.name);
  const binding = (value: typeof current) => value ? { secret: value.secret, keyDigest: value.keyDigest, namespace: value.namespace.uid } : null;
  if (canonical(binding(current)) !== canonical(binding(original.root.budgetTls))) throw new Error("Budget changed before the post-retirement rotation boundary");
}
async function patchSpec(run: Execute, before: ObjectValue, spec: ObjectValue, fence: () => Promise<void>): Promise<void> {
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
async function patchFields(run: Execute, before: ObjectValue, fields: ObjectValue): Promise<void> {
  const result = record(JSON.parse(await run(["patch", "namespace", reviewed(before).name, "--type=json", "--patch-file=/dev/stdin", "-o", "json"],
    JSON.stringify([
      { op: "test", path: "/metadata/uid", value: reviewed(before).uid },
      { op: "test", path: "/metadata/resourceVersion", value: reviewed(before).resourceVersion },
      { op: "test", path: "/metadata/annotations", value: at(before, "metadata", "annotations") },
      { op: "replace", path: "/metadata/annotations", value: fields },
    ]))));
  if (reviewed(result).uid !== reviewed(before).uid || canonical(at(result, "metadata", "annotations")) !== canonical(fields)) throw new Error("Migration namespace acknowledgement differs");
}
async function save(journal: RootMigrationJournal, receipt: RootMigrationReceipt): Promise<void> {
  if (Buffer.byteLength(canonical(receipt)) > 2_097_152) throw new Error("Migration receipt exceeds 2 MiB");
  await journal.save(structuredClone(receipt));
}

/** The callback must prove normal exit, version-fence resources and irreversibly
 * fence release storage before returning. An error leaves durable publishing;
 * neither history, process cancellation nor exit alone is a terminal proof. */
export async function commitRootPublication(
  receipt: RootMigrationReceipt, journal: RootMigrationJournal, publish: (publishing: RootMigrationReceipt) => Promise<void>,
): Promise<RootMigrationReceipt> {
  if (receipt.publication?.phase !== "prepared") throw new Error("Publication outcome is unknown or ambiguous; do not relaunch Helm");
  const next = structuredClone(receipt);
  next.publication!.phase = "publishing";
  await save(journal, next);
  await publish(next);
  next.publication!.phase = "succeeded";
  await save(journal, next);
  return next;
}

function publicationOwner(receipt: RootMigrationReceipt): string {
  const value = receipt.publication;
  if (!value || !/^[a-f0-9]{64}$/.test(value.attempt) || !value.context
    || value.release !== receipt.plan.review.release.name || value.namespace !== receipt.plan.review.release.namespace) {
    throw new Error(FAILURE);
  }
  return canonical({ version: 1, attempt: value.attempt, planDigest: receipt.planDigest, context: value.context,
    phase: ["preparing", "prepared"].includes(value.phase) ? "preparing" : "publishing" });
}

/** Acquire a cluster-visible refusal marker with an identity/version-tested patch.
 * A crash during this metadata-only CAS can be resumed; it never writes TLS or Helm storage. */
export async function ownRootPublication(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal,
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  if (receipt.phase !== "rotation-required" || receipt.publication?.phase !== "preparing") throw new Error(FAILURE);
  const next = structuredClone(receipt), ns = next.plan.review.release.namespace;
  const marker = publicationOwner(next);
  const current = await read(run, "namespace", ns);
  const fields = { ...record(at(next.namespace, "metadata", "annotations")), [CORE_PUBLICATION]: marker };
  const actual = at(current, "metadata", "annotations");
  if (canonical(actual) !== canonical(fields)) {
    if (canonical(actual) !== canonical(at(next.namespace, "metadata", "annotations"))) throw new Error(FAILURE);
    await assertRetiredRootPublication(run, next, false);
    await patchFields(run, current, fields);
  }
  next.namespace = await read(run, "namespace", ns);
  if (canonical(at(next.namespace, "metadata", "annotations")) !== canonical(fields)) throw new Error(FAILURE);
  await save(journal, next);
  await assertRetiredRootPublication(run, next);
  return next;
}

/** Consume launch ownership once in the API, not merely in a local journal lock.
 * Copied preparing/prepared receipts cannot launch after this CAS has won. */
export async function claimRootPublicationLaunch(run: Execute, receipt: RootMigrationReceipt): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  if (receipt.publication?.phase !== "publishing") throw new Error(FAILURE);
  const prepared = structuredClone(receipt);
  prepared.publication!.phase = "prepared";
  await assertRetiredRootPublication(run, prepared);
  const next = structuredClone(receipt), ns = next.plan.review.release.namespace;
  const current = await read(run, "namespace", ns);
  if (at(current, "metadata", "annotations", CORE_PUBLICATION) !== publicationOwner(prepared)) throw new Error(FAILURE);
  await patchFields(run, current, { ...record(at(current, "metadata", "annotations")), [CORE_PUBLICATION]: publicationOwner(next) });
  next.namespace = await read(run, "namespace", ns);
  await assertRetiredRootPublication(run, next);
  return next;
}

/** Reserve one successor before invalidating the failed snapshot. This does not
 * authorize launch: the publisher must fence every resource and release first. */
export async function claimRootPublicationRecovery(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal, frozen: readonly ObjectValue[],
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  const publication = receipt.publication, recovery = publication?.recovery;
  if (!publication || !["publishing", "succeeded"].includes(publication.phase) || !recovery || !/^[a-f0-9]{64}$/.test(recovery.attempt)
    || recovery.attempt === publication.attempt) throw new Error(FAILURE);
  const next = structuredClone(receipt), successor = structuredClone(receipt);
  successor.publication!.phase = "preparing";
  successor.publication!.attempt = recovery.attempt;
  const marker = publicationOwner(successor), ns = publication.namespace;
  const current = await read(run, "namespace", ns);
  const fields = { ...record(at(receipt.namespace, "metadata", "annotations")), [CORE_PUBLICATION]: marker };
  // A failed SSA write may already have advanced admission versions. This proof
  // authorizes only successor reservation; retirement is rebound after fencing.
  await reviewPublicationBundle(run, frozen, receipt.activation!.bundleRevision, `${recovery.attempt}:failed-publication`, false,
    publication.attempt);
  if (at(current, "metadata", "annotations", CORE_PUBLICATION) === publicationOwner(receipt)) {
    await assertRetiredRootPublicationScope(run, receipt);
    await patchFields(run, current, { ...record(at(current, "metadata", "annotations")), [CORE_PUBLICATION]: marker });
  }
  successor.namespace = structuredClone(receipt.namespace!);
  record(successor.namespace.metadata).annotations = fields;
  await assertRetiredRootPublicationScope(run, successor);
  next.namespace = await read(run, "namespace", ns);
  await save(journal, next);
  return next;
}

async function assertRetiredRootPublicationScope(
  run: Execute, receipt: RootMigrationReceipt, owned = true,
): Promise<void> {
  validateReceipt(receipt);
  if (receipt.phase !== "rotation-required" || !receipt.activation || receipt.retirement?.phase !== "retired") throw new Error(FAILURE);
  const old = originalActivation(receipt.plan), ns = old.root.namespace.name;
  run = replay(run, old);
  await authority(run);
  await scopeFence(run, receipt);
  await noOwnedAuthority(run, receipt.plan);
  const namespace = await read(run, "namespace", ns);
  if (owned) {
    if (at(namespace, "metadata", "annotations", CORE_PUBLICATION) !== publicationOwner(receipt)) throw new Error(FAILURE);
  } else assertNoCorePublication(namespace);
  const root = await read(run, "deployment", old.root.deployment.name, ns);
  if (reviewed(root).uid !== old.root.deployment.uid
    || canonical(root.spec) !== canonical({ ...receipt.plan.targetSpec, replicas: 0 })
    || at(namespace, "metadata", "annotations", `${P}state`) !== "Pending"
    || canonical(retirementState(namespace)) !== canonical(receipt.retirement)) throw new Error(FAILURE);
  await assertRetiredRoot(run, receipt.activation, receipt.retirement);
}

export async function assertRetiredRootPublication(
  run: Execute, receipt: RootMigrationReceipt, owned = true,
): Promise<void> {
  await assertRetiredRootPublicationScope(run, receipt, owned);
  const old = originalActivation(receipt.plan), ns = old.root.namespace.name;
  run = replay(run, old);
  const activation = await previewPrivateActivation(run, ns, old.namespaces.map(scope => ({ namespace: scope.namespace.name })), [], ns, old.profile, companionReferences(receipt.plan));
  if (activation.bundleRevision !== publicationBundleRevision(receipt) || canonical(activation.controllerUids) !== canonical(old.controllerUids)
    || activation.root.replicaIntent !== old.root.replicaIntent || activation.root.account.uid !== old.root.account.uid) throw new Error(FAILURE);
  await rotatedPublicCa(run, receipt, activation);
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

function publicationBundleRevision(receipt: RootMigrationReceipt): string {
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

/** Preserve the retirement attempt, captured authority and key history while
 * journaling an exact metadata-only revision transition before namespace CAS. */
export async function advanceRootPublicationBundle(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal, frozen: readonly ObjectValue[],
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  const publication = receipt.publication!;
  const recovery = ["publishing", "succeeded"].includes(publication.phase) ? publication.recovery : undefined;
  if (publication.phase !== "succeeded" && !recovery) throw new Error(FAILURE);
  const fence = recovery ? `${recovery.attempt}:failed-publication` : publication.attempt;
  const scope = (value: RootMigrationReceipt) => {
    const result = structuredClone(value);
    if (recovery) {
      result.publication!.attempt = recovery.attempt;
      result.publication!.phase = "preparing";
    }
    return result;
  };
  let next = structuredClone(receipt);
  let transition = next.bundleTransitions?.at(-1);
  if (transition?.applied) transition = undefined;
  if (transition && (transition.publication !== publication.attempt || transition.fence !== fence)) throw new Error(FAILURE);
  const review = await reviewPublicationBundle(run, frozen,
    transition?.activation.bundleRevision ?? next.activation!.bundleRevision, fence, true);
  if (!transition) {
    if (review.revision === next.activation!.bundleRevision) return next;
    await assertRetiredRootPublicationScope(run, scope(next));
    transition = { publication: publication.attempt, fence, review, activation: structuredClone(next.activation!),
      retirement: structuredClone(next.retirement!), namespace: await read(run, "namespace", publication.namespace),
      ...(next.namespaces?.length ? { namespaces: structuredClone(next.namespaces) } : {}) };
    next.bundleTransitions = [...(next.bundleTransitions ?? []), transition];
    await save(journal, next);
  }
  if (canonical(review) !== canonical(transition.review)) throw new Error(FAILURE);
  const before = transition.namespace;
  const activation = { ...structuredClone(transition.activation), bundleRevision: review.revision };
  const retirement = { ...structuredClone(transition.retirement), binding: retirementBinding(activation) };
  const fields = { ...record(at(before, "metadata", "annotations")), [`${P}bundle-revision`]: review.revision, [FIELD]: canonical(retirement) };
  const after = structuredClone(before);
  record(after.metadata).annotations = fields;
  const live = await read(run, "namespace", publication.namespace);
  const actual = canonical(at(live, "metadata", "annotations"));
  const completed = structuredClone(next);
  completed.activation = activation;
  completed.retirement = retirement;
  completed.namespace = after;
  completed.bundleTransitions!.at(-1)!.applied = true;
  completed.namespaces = [];
  for (const snapshot of transition.namespaces ?? []) {
    const name = reviewed(snapshot).name;
    const current = await read(run, "namespace", name);
    const fields = { ...record(at(snapshot, "metadata", "annotations")), [`${P}bundle-revision`]: review.revision };
    if (canonical(at(current, "metadata", "annotations")) !== canonical(fields)) {
      await assertRetiredRootPublicationScope(run, scope(next));
      await patchFields(run, current, fields);
    }
    const updated = await read(run, "namespace", name);
    if (reviewed(updated).uid !== reviewed(snapshot).uid || canonical(at(updated, "metadata", "annotations")) !== canonical(fields)) throw new Error(FAILURE);
    completed.namespaces.push(updated);
  }
  if (actual === canonical(at(before, "metadata", "annotations"))) {
    await assertRetiredRootPublicationScope(run, scope(next));
    await patchFields(run, live, fields);
  } else if (actual !== canonical(fields)) throw new Error(FAILURE);
  await assertRetiredRootPublication(run, scope(completed));
  completed.namespace = await read(run, "namespace", publication.namespace);
  await save(journal, completed);
  return completed;
}

/** Called only after durable normal completion and exact resource/storage verification. */
export async function releaseRootPublication(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal,
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  if (receipt.publication?.phase !== "succeeded") throw new Error("No durable successful Core publication exists");
  const next = structuredClone(receipt), ns = next.plan.review.release.namespace;
  const current = await read(run, "namespace", ns);
  const fields = { ...record(at(current, "metadata", "annotations")) };
  delete fields[CORE_PUBLICATION];
  const comparable = (value: unknown) => {
    const copy = { ...record(value) };
    delete copy[CORE_PUBLICATION];
    delete copy[CORE_PUBLICATION_FENCE];
    return canonical(copy);
  };
  if (comparable(fields) !== comparable(at(next.namespace, "metadata", "annotations"))) throw new Error(FAILURE);
  if (at(current, "metadata", "annotations", CORE_PUBLICATION) !== undefined) {
    await assertRetiredRootPublication(run, next);
    await patchFields(run, current, fields);
  }
  next.namespace = await read(run, "namespace", ns);
  await assertRetiredRootPublication(run, next, false);
  next.publication!.phase = "verified";
  await save(journal, next);
  return next;
}

export async function prepareRootMigration(
  run: Execute, review: PrivateRootMigrationReview, documents: readonly unknown[], journal: RootMigrationJournal,
): Promise<RootMigrationReceipt> {
  supportedTopology(review);
  run = replay(run, review.activation);
  const imageNames = (review.companionImages ?? []).map(value => value.identity.name);
  const checked = await previewPrivateRootMigration(run, review.release.namespace, review.release.name, documents, review.recovery?.historicalRoot, imageNames);
  if (rootMigrationReviewDigest(checked) !== rootMigrationReviewDigest(review)) throw new Error("Migration review is stale; regenerate it before retirement");
  const namespace = await read(run, "namespace", review.release.namespace);
  assertNoCorePublication(namespace);
  const original = review.recovery?.historicalRoot
    ?? await read(run, "deployment", review.activation.root.deployment.name, review.release.namespace);
  const grant = await read(run, GRANTS, "workspace", review.release.namespace);
  if (reviewed(grant).uid !== review.grants[0]!.identity.uid || digest(grant.spec) !== review.grants[0]!.specDigest
    || !Array.isArray(at(grant, "spec", "writers"))
    || (at(grant, "spec", "writers") as ObjectValue[]).some(writer => writer.namespace !== review.release.namespace)) throw new Error(FAILURE);
  const rendered = documents.map(record).find(value => value.kind === "Deployment"
    && at(value, "metadata", "name") === review.activation.root.deployment.name
    && (at(value, "metadata", "namespace") ?? review.release.namespace) === review.release.namespace)!;
  const target = record(JSON.parse(await run(["apply", "--server-side", "--dry-run=server", "--force-conflicts", "--field-manager=helm", "-f", "-", "-o", "json"], JSON.stringify(rendered))));
  const activation = structuredClone(review.activation);
  activation.phase = "reviewed";
  const capturedInventory = await inventories(run);
  const guard = await captureGuardRetirement(run, activation, grant, capturedInventory.namespaces);
  const companions: ObjectValue[] = [];
  const companionTargets: NonNullable<Plan["companionTargets"]> = [];
  for (const scope of activation.namespaces) for (const consumer of scope.consumers) {
    if (consumer.object.uid === activation.root.deployment.uid) continue;
    const current = await read(run, "deployment", consumer.object.name, scope.namespace.name);
    if (reviewed(current).uid !== consumer.object.uid || templateDigest(current) !== consumer.templateDigest) throw new Error(FAILURE);
    replicaIntent(current);
    companions.push(current);
    if (scope.namespace.name === review.release.namespace && imageNames.includes(consumer.object.name)) {
      const document = documents.map(record).find(value => value.kind === "Deployment"
        && at(value, "metadata", "name") === consumer.object.name
        && (at(value, "metadata", "namespace") ?? review.release.namespace) === review.release.namespace)!;
      const projected = record(JSON.parse(await run(["apply", "--server-side", "--dry-run=server", "--force-conflicts",
        "--field-manager=helm", "-f", "-", "-o", "json"], JSON.stringify(document))));
      if (reviewed(projected).uid !== consumer.object.uid || reviewed(projected).name !== consumer.object.name
        || at(projected, "metadata", "namespace") !== review.release.namespace) throw new Error(FAILURE);
      companionTargets.push({ uid: consumer.object.uid, spec: record(projected.spec) });
    }
  }
  const plan: Plan = { review, seal: String(at(namespace, "metadata", "annotations", FIELD)),
    original, targetSpec: record(target.spec), grant, inventory: capturedInventory, guard, pods: [],
    ...(companions.length ? { companions } : {}),
    ...(companionTargets.length ? { companionTargets } : {}),
    ...(activation.root.budgetTls ? { publicCa: await read(run, "configmap", "kars-inference-budget-contracts", review.release.namespace) } : {}) };
  originalActivation(plan);
  assertTarget(plan);
  const execution = retiringExecution(plan);
  for (const scope of execution.namespaces) for (const pod of await inventory(run, "pods", scope.namespace.name)) {
    const owner = await reviewedOwner(run, pod, scope, undefined, execution.root);
    if (!owner && !await requiresPrivatePodReview(run, pod, scope, activation)) continue;
    if (!owner) throw new Error(FAILURE);
    plan.pods.push({ namespaceUid: scope.namespace.uid, identity: record(reviewed(pod, true)), owners: at(pod, "metadata", "ownerReferences")!, templateDigest: templateDigest(pod) });
  }
  if (rootMigrationReviewDigest(await previewPrivateRootMigration(run, review.release.namespace, review.release.name, documents, review.recovery?.historicalRoot, imageNames)) !== rootMigrationReviewDigest(review)) throw new Error(FAILURE);
  const receipt: RootMigrationReceipt = { version: 1, plan, planDigest: digest(plan), phase: "prepared" };
  validateReceipt(receipt);
  await companionFence(run, receipt);
  await save(journal, receipt);
  return receipt;
}

/** Prepare a successor from a sealed failed qualification; never edit or reinterpret its original target. */
export async function prepareRouterBaselineRestart(
  execute: Execute, source: RootMigrationReceipt, journal: RootMigrationJournal,
): Promise<RootMigrationReceipt> {
  validateReceipt(source);
  const old = originalActivation(source.plan), ns = old.root.namespace.name;
  const run = replay(execute, old);
  await authority(run);
  const original = await read(run, "deployment", old.root.deployment.name, ns);
  const targetSpec = routerBaselineTarget(source, original);
  const namespace = await read(run, "namespace", ns);
  assertNoCorePublication(namespace);
  const preview = await previewPrivateActivation(run, ns, old.namespaces.map(scope => ({ namespace: scope.namespace.name })), [], ns, old.profile, companionReferences(source.plan));
  const continuity = await reviewPrivateContinuity(run, preview);
  if (!continuity?.sealed) throw new Error("Router baseline restart requires actual sealed qualification of the failed target");
  const qualified = continuity.proof.activation;
  await validateQualifiedActivation(run, qualified, true);
  await rotatedPublicCa(run, source, preview);
  await scopeFence(run, source, { rootFlexible: true, qualification: qualified });
  await noOwnedAuthority(run, source.plan);
  const capturedInventory = await inventories(run), companions: ObjectValue[] = [];
  for (const before of source.plan.companions ?? []) {
    const current = await read(run, "deployment", reviewed(before).name, String(at(before, "metadata", "namespace")));
    if (replicaIntent(current) !== 0) throw new Error("Router baseline restart requires stopped reviewed companions");
    companions.push({ ...current, spec: { ...record(current.spec), replicas: replicaIntent(before) } });
  }
  const projected = record(JSON.parse(await run(["apply", "--server-side", "--dry-run=server", "--force-conflicts",
    "--field-manager=helm", "-f", "-", "-o", "json"], JSON.stringify({ ...original, spec: targetSpec }))));
  if (reviewed(projected).uid !== reviewed(original).uid || canonical(projected.spec) !== canonical(targetSpec)) throw new Error("Router baseline correction changed during server projection");
  const grant = await read(run, GRANTS, "workspace", ns);
  const review: PrivateRootMigrationReview = {
    version: 1, phase: "reviewed", activation: qualified,
    predecessor: digest(JSON.parse(String(at(namespace, "metadata", "annotations", FIELD)))),
    release: structuredClone(source.plan.review.release), renderDigest: digest(targetSpec),
    target: { specDigest: digest(targetSpec), templateDigest: templateDigest({ spec: targetSpec }) },
    grants: [{ namespace: ns, identity: reviewed(grant), specDigest: digest(grant.spec) }],
    inventoryDigest: digest(capturedInventory),
  };
  const plan: Plan = {
    review, seal: String(at(namespace, "metadata", "annotations", FIELD)), original, targetSpec,
    grant: structuredClone(source.plan.grant), guard: structuredClone(source.plan.guard),
    inventory: capturedInventory, companions, pods: [], restartFrom: structuredClone(source),
    publicCa: await read(run, "configmap", "kars-inference-budget-contracts", ns),
  };
  for (const scope of qualified.namespaces) for (const pod of await inventory(run, "pods", scope.namespace.name)) {
    const owner = await reviewedOwner(run, pod, scope, undefined, qualified.root);
    if (!owner && !await requiresPrivatePodReview(run, pod, scope, qualified)) continue;
    if (!owner) throw new Error(FAILURE);
    plan.pods.push({ namespaceUid: scope.namespace.uid, identity: record(reviewed(pod, true)),
      owners: at(pod, "metadata", "ownerReferences")!, templateDigest: templateDigest(pod) });
  }
  const receipt: RootMigrationReceipt = { version: 1, plan, planDigest: digest(plan), phase: "quiescent",
    namespace, activation: { ...structuredClone(qualified), phase: "reviewed" } };
  validateReceipt(receipt);
  await scopeFence(run, receipt);
  await noOwnedAuthority(run, plan);
  await validateQualifiedActivation(run, qualified, true);
  await sameBudget(run, plan);
  if (canonical((await read(run, "deployment", old.root.deployment.name, ns)).spec) !== canonical(original.spec)
    || at(await read(run, "namespace", ns), "metadata", "annotations", FIELD) !== plan.seal) throw new Error(FAILURE);
  await save(journal, receipt);
  return receipt;
}

function validateReceipt(receipt: RootMigrationReceipt): void {
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

/** Stops at a durable, genuinely retired v1 baseline. Never rotates a key or starts target execution. */
export async function retireRootMigration(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal,
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  if (receipt.supersededBy) throw new Error("Migration was superseded; use its successor receipt");
  receipt = structuredClone(receipt);
  const { plan } = receipt, old = originalActivation(plan), ns = old.root.namespace.name;
  run = replay(run, old);
  await authority(run);
  assertNoCorePublication(await read(run, "namespace", ns));
  if (["rotation-required", "qualifying", "complete"].includes(receipt.phase)) throw new Error("Retirement already recorded; use migration finalize after real budget rotation");
  if (receipt.phase === "prepared") {
    await scopeFence(run, receipt, { rootFlexible: true, allowInitialGrant: true, writerTransition: true });
    await sameBudget(run, plan);
    if (plan.review.recovery) await predecessorAuthority(run, plan);
    const result = await retireGrantWriters(run, plan.grant, plan.guard, undefined, plan.review.recovery
      ? async () => { await predecessorAuthority(run, plan); }
      : (execute, activation) => validatePrivateActivation(execute, activation, true));
    receipt.namespace = await read(run, "namespace", ns);
    receipt.activation = result.activation;
    receipt.phase = "quiescent";
    await scopeFence(run, receipt);
    await save(journal, receipt);
  }
  if (receipt.phase === "quiescent") {
    await scopeFence(run, receipt);
    await noOwnedAuthority(run, plan);
    await predecessorAuthority(run, plan, true);
    receipt.captured = structuredClone(plan.pods);
    receipt.phase = "retiring";
    await save(journal, receipt);
  }
  if (receipt.phase === "retiring") {
    const deadline = Date.now() + 120_000;
    for (;;) {
      await scopeFence(run, receipt);
      await noOwnedAuthority(run, plan);
      await sameBudget(run, plan);
      await predecessorAuthority(run, plan, false, true);
      const root = await read(run, "deployment", old.root.deployment.name, ns);
      const zero = { ...record(retiringRoot(plan).spec), replicas: 0 };
      if (reviewed(root).uid !== old.root.deployment.uid
        || ![canonical(retiringRoot(plan).spec), canonical(zero)].includes(canonical(root.spec))) throw new Error(FAILURE);
      let pending = false;
      const execution = retiringExecution(plan);
      for (const scope of execution.namespaces) for (const pod of await inventory(run, "pods", scope.namespace.name)) {
        const owner = await reviewedOwner(run, pod, scope, undefined, execution.root);
        if (!owner && !receipt.captured!.some(value => at(value, "identity", "uid") === reviewed(pod, true).uid)
          && !await requiresPrivatePodReview(run, pod, scope, old)) continue;
        if (!owner) throw new Error("Unreviewed private execution prevents migration retirement");
        pending = true;
        const captured = receipt.captured!;
        const evidence = { namespaceUid: scope.namespace.uid, identity: reviewed(pod, true), owners: at(pod, "metadata", "ownerReferences")!, templateDigest: templateDigest(pod) };
        const prior = captured.find(value => at(value, "identity", "uid") === reviewed(pod, true).uid);
        if (prior && (prior.templateDigest !== evidence.templateDigest || canonical(prior.owners) !== canonical(evidence.owners))) throw new Error(FAILURE);
        if (!prior) { captured.push(record(evidence)); await save(journal, receipt); }
      }
      // Admission permits an operator to pause foreign budget workloads only after authority is withdrawn.
      for (const scope of old.namespaces) {
        if (scope.namespace.uid === old.root.namespace.uid) continue;
        await scopeFence(run, receipt);
        await noOwnedAuthority(run, plan);
        await sameBudget(run, plan);
        await predecessorAuthority(run, plan, false, true);
        const current = await read(run, "namespace", scope.namespace.name);
        const fields = record(at(current, "metadata", "annotations"));
        if (fields[`${P}state`] !== "Pending") await patchFields(run, current, { ...fields, [`${P}state`]: "Pending" });
      }
      let pausedCompanion = false;
      for (const before of plan.companions ?? []) {
        const current = await read(run, "deployment", reviewed(before).name, String(at(before, "metadata", "namespace")));
        if (replicaIntent(current) === 0) continue;
        await patchSpec(run, current, { ...record(current.spec), replicas: 0 }, async () => {
          await scopeFence(run, receipt);
          await noOwnedAuthority(run, plan);
          await sameBudget(run, plan);
          await predecessorAuthority(run, plan, false, true);
        });
        pausedCompanion = true;
      }
      if (at(root, "spec", "replicas") !== 0) {
        await scopeFence(run, receipt);
        await patchSpec(run, root, zero, async () => {
          await scopeFence(run, receipt);
          await noOwnedAuthority(run, plan);
          await sameBudget(run, plan);
          await predecessorAuthority(run, plan, false, true);
        });
      } else if (!pending && !pausedCompanion) break;
      if (Date.now() >= deadline) throw new Error("Root Pod retirement timed out; root stays zero and the receipt is resumable");
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    await retiredPredecessor(run, receipt);
    await scopeFence(run, receipt);
    const root = await read(run, "deployment", old.root.deployment.name, ns);
    const next = structuredClone(receipt.activation!);
    next.root.templateDigest = plan.review.target.templateDigest;
    next.root.deployment = reviewed(root);
    next.namespaces.find(scope => scope.namespace.uid === next.root.namespace.uid)!.consumers.find(consumer => consumer.object.uid === next.root.deployment.uid)!.templateDigest = next.root.templateDigest;
    for (const change of plan.review.companionImages ?? []) {
      next.namespaces.find(scope => scope.namespace.uid === next.root.namespace.uid)!.consumers.find(consumer => consumer.object.uid === change.identity.uid)!.templateDigest = change.target.templateDigest;
    }
    for (const scope of next.namespaces) delete scope.epoch;
    const previous = retirementState(await read(run, "namespace", ns));
    if (!previous || previous.phase !== "restoring") throw new Error(FAILURE);
    const state = startRetirement(next, root, previous);
    for (const scope of old.namespaces) {
      state.captured[scope.namespace.uid] = [...new Set([...(previous.captured[scope.namespace.uid] ?? []),
        ...receipt.captured!.filter(value => (value.namespaceUid ?? old.root.namespace.uid) === scope.namespace.uid)
          .map(value => String(at(value, "identity", "uid")))])].sort();
    }
    receipt.activation = next;
    receipt.retirement = state;
    receipt.phase = "cutover";
    await save(journal, receipt);
  }
  if (receipt.phase === "cutover") {
    const next = receipt.activation!, state = receipt.retirement!;
    const base = record(at(receipt.namespace, "metadata", "annotations"));
    const pending = { ...base, [`${P}state`]: "Pending" };
    const fields = { ...pending, ...annotations(next, next.namespaces.find(scope => scope.namespace.uid === next.root.namespace.uid)!, "Pending"), [FIELD]: canonical(state) };
    const retiredState: RootRetirement = { ...state, phase: "retired", ...(next.root.budgetTls ? {
      baseline: { secretUid: next.root.budgetTls.secret.uid, resourceVersion: next.root.budgetTls.secret.resourceVersion, keyDigest: next.root.budgetTls.keyDigest },
    } : {}) };
    const retiredFields = { ...fields, [FIELD]: canonical(retiredState) };
    await scopeFence(run, receipt, { rootFlexible: true });
    await noOwnedAuthority(run, plan);
    await sameBudget(run, plan);
    let namespace = await read(run, "namespace", ns);
    let actual = canonical(at(namespace, "metadata", "annotations"));
    if (![base, pending, fields, retiredFields].some(value => canonical(value) === actual)) throw new Error(FAILURE);
    const root = await read(run, "deployment", old.root.deployment.name, ns);
    const oldZero = { ...record(retiringRoot(plan).spec), replicas: 0 }, targetZero = { ...plan.targetSpec, replicas: 0 };
    if (reviewed(root).uid !== old.root.deployment.uid
      || ![oldZero, targetZero].some(value => canonical(value) === canonical(root.spec))) throw new Error(FAILURE);
    if (actual === canonical(base)) {
      await retiredPredecessor(run, receipt);
      await patchFields(run, namespace, pending);
      namespace = await read(run, "namespace", ns);
      actual = canonical(at(namespace, "metadata", "annotations"));
    }
    for (const scope of next.namespaces) {
      if (scope.namespace.uid === next.root.namespace.uid) continue;
      await scopeFence(run, receipt, { rootFlexible: true });
      await noOwnedAuthority(run, plan);
      await sameBudget(run, plan);
      await assertNoPods(run, receipt, old);
      const current = await read(run, "namespace", scope.namespace.name);
      const expected = pendingScope(receipt, scopeSnapshot(receipt, scope.namespace.uid));
      if (canonical(at(current, "metadata", "annotations")) !== canonical(expected)) await patchFields(run, current, expected);
    }
    if (canonical(root.spec) === canonical(oldZero) && canonical(oldZero) !== canonical(targetZero)) {
      if (actual !== canonical(pending)) throw new Error(FAILURE);
      await assertNoPods(run, receipt, old);
      await patchSpec(run, root, targetZero, async () => {
        await scopeFence(run, receipt, { rootFlexible: true });
        await noOwnedAuthority(run, plan);
        await sameBudget(run, plan);
        await assertNoPods(run, receipt, old);
        if (canonical(at(await read(run, "namespace", ns), "metadata", "annotations")) !== canonical(pending)) throw new Error(FAILURE);
      });
    }
    const cutoverFence = async () => {
      await scopeFence(run, receipt, { rootFlexible: true });
      await noOwnedAuthority(run, plan);
      await sameBudget(run, plan);
      await assertNoPods(run, receipt, next);
      const currentRoot = await read(run, "deployment", old.root.deployment.name, ns);
      if (reviewed(currentRoot).uid !== old.root.deployment.uid || canonical(currentRoot.spec) !== canonical(targetZero)
        || canonical(at(await read(run, "namespace", ns), "metadata", "annotations")) !== canonical(pending)) throw new Error(FAILURE);
    };
    for (const target of plan.companionTargets ?? []) {
      let update = receipt.companionUpdates?.find(value => value.uid === target.uid);
      if (update?.phase === "updated") continue;
      await cutoverFence();
      if (!update) {
        update = { uid: target.uid, phase: "updating" };
        receipt.companionUpdates = [...(receipt.companionUpdates ?? []), update];
        await save(journal, receipt);
      }
      const before = plan.companions!.find(value => reviewed(value).uid === target.uid)!;
      const current = await read(run, "deployment", reviewed(before).name, ns);
      const expected = { ...target.spec, replicas: 0 };
      if (canonical(current.spec) !== canonical(expected)) {
        if (reviewed(current).uid !== target.uid
          || canonical(current.spec) !== canonical({ ...record(before.spec), replicas: 0 })) throw new Error(FAILURE);
        await patchSpec(run, current, expected, cutoverFence);
      }
      const applied = await read(run, "deployment", reviewed(before).name, ns);
      if (reviewed(applied).uid !== target.uid || canonical(applied.spec) !== canonical(expected)) throw new Error(FAILURE);
      update.phase = "updated";
      await cutoverFence();
      await save(journal, receipt);
    }
    await scopeFence(run, receipt, { rootFlexible: true });
    await noOwnedAuthority(run, plan);
    await sameBudget(run, plan);
    await assertNoPods(run, receipt, next);
    const appliedRoot = await read(run, "deployment", old.root.deployment.name, ns);
    if (reviewed(appliedRoot).uid !== old.root.deployment.uid || canonical(appliedRoot.spec) !== canonical(targetZero)) throw new Error(FAILURE);
    namespace = await read(run, "namespace", ns);
    actual = canonical(at(namespace, "metadata", "annotations"));
    if (actual === canonical(pending)) await patchFields(run, namespace, fields);
    else if (![fields, retiredFields].some(value => canonical(value) === actual)) throw new Error(FAILURE);
    next.namespaces.find(scope => scope.namespace.uid === next.root.namespace.uid)!.namespace = reviewed(await read(run, "namespace", ns));
    if (canonical(retirementState(await read(run, "namespace", ns))) === canonical(state)) {
      receipt.retirement = await recordRetiredBudgetBaseline(run, next, next.namespaces.find(scope => scope.namespace.uid === next.root.namespace.uid)!, state);
    } else {
      await assertRetiredRoot(run, next, retiredState);
      receipt.retirement = retiredState;
    }
    receipt.namespace = await read(run, "namespace", ns);
    receipt.namespaces = [];
    for (const scope of next.namespaces) {
      if (scope.namespace.uid !== next.root.namespace.uid) receipt.namespaces.push(await read(run, "namespace", scope.namespace.name));
    }
    receipt.phase = "rotation-required";
    await save(journal, receipt);
  }
  return receipt;
}

/** Prepare one atomic rotation CAS for the activation-bound legacy TLS Secret.
 * Ownership is added only after genuine retirement; never relabel a qualified key.
 * Persist this patch before applying it. A lost acknowledgement must not be replayed
 * with a refreshed resourceVersion; finalize binds the resulting rotated identity. */
export async function prepareRootBudgetTlsRotation(
  run: Execute, receipt: RootMigrationReceipt, expectedSecret: ObjectValue, data: Record<string, string>,
): Promise<ObjectValue[]> {
  validateReceipt(receipt);
  if (receipt.phase !== "rotation-required" || !receipt.activation || receipt.retirement?.phase !== "retired"
    || !receipt.retirement.baseline || receipt.publication) throw new Error(FAILURE);
  const old = originalActivation(receipt.plan), ns = old.root.namespace.name;
  run = replay(run, old);
  await authority(run);
  await scopeFence(run, receipt);
  await noOwnedAuthority(run, receipt.plan);
  await sameBudget(run, receipt.plan);
  const namespace = await read(run, "namespace", ns);
  assertNoCorePublication(namespace);
  const root = await read(run, "deployment", old.root.deployment.name, ns);
  if (reviewed(root).uid !== old.root.deployment.uid
    || canonical(root.spec) !== canonical({ ...receipt.plan.targetSpec, replicas: 0 })
    || at(namespace, "metadata", "annotations", `${P}state`) !== "Pending"
    || canonical(at(namespace, "metadata", "annotations")) !== canonical(at(receipt.namespace, "metadata", "annotations"))
    || canonical(retirementState(namespace)) !== canonical(receipt.retirement)) throw new Error(FAILURE);
  await assertNoPods(run, receipt, receipt.activation);
  await assertRetiredRoot(run, receipt.activation, receipt.retirement);
  const budget = receipt.activation.root.budgetTls;
  if (!budget || budget.namespace.name !== ns) throw new Error(FAILURE);
  const secret = record(JSON.parse(await run(["get", "secret", budget.secret.name, "-n", ns,
    "-o", "json", "--show-managed-fields=true"])));
  const meta = record(secret.metadata), stored = record(secret.data);
  const annotations = record(meta.annotations ?? {}), labels = record(meta.labels ?? {});
  if (canonical(secret) !== canonical(expectedSecret) || reviewed(secret).uid !== receipt.retirement.baseline.secretUid
    || reviewed(secret).resourceVersion !== receipt.retirement.baseline.resourceVersion
    || meta.namespace !== ns || meta.deletionTimestamp || secret.type !== "kubernetes.io/tls" || secret.immutable
    || annotations["kars.azure.com/inference-budget-tls"] !== "v1"
    || (meta.ownerReferences !== undefined && canonical(meta.ownerReferences) !== "[]")) throw new Error(FAILURE);
  const release = receipt.plan.review.release.name;
  const fields = { "meta.helm.sh/release-name": release, "meta.helm.sh/release-namespace": ns };
  const owned = (object: ObjectValue) => Object.entries(fields).every(([key, value]) => at(object, "metadata", "annotations", key) === value)
    && at(object, "metadata", "labels", "app.kubernetes.io/managed-by") === "Helm";
  const catalog = await read(run, "configmap", "kars-inference-budget-contracts", ns);
  if (!owned(root) || !owned(catalog) || canonical(catalog) !== canonical(receipt.plan.publicCa)) throw new Error(FAILURE);
  if (Object.entries(fields).some(([key, value]) => annotations[key] !== undefined && annotations[key] !== value)
    || (labels["app.kubernetes.io/managed-by"] !== undefined
      && labels["app.kubernetes.io/managed-by"] !== "Helm")) throw new Error("Cannot rotate/adopt another owner's budget TLS Secret");
  const unchanged = { ...data, "tls.crt": stored["tls.crt"], "tls.key": stored["tls.key"] };
  if (canonical(unchanged) !== canonical(stored) || data["tls.key"] === stored["tls.key"]) throw new Error(FAILURE);
  const certificate = new X509Certificate(Buffer.from(data["tls.crt"]!, "base64"));
  const publicKey = certificate.publicKey.export({ format: "der", type: "spki" });
  const keyDigest = createHash("sha256").update(publicKey).digest("hex");
  if (receipt.retirement.exposedKeys.includes(keyDigest) || keyDigest === receipt.retirement.baseline.keyDigest
    || !publicKey.equals(createPublicKey(createPrivateKey(Buffer.from(data["tls.key"]!, "base64"))).export({ format: "der", type: "spki" }))) throw new Error(FAILURE);
  const metadata = { ...meta, annotations: { ...annotations, ...fields },
    labels: { ...labels, "app.kubernetes.io/managed-by": "Helm" } };
  return [
    { op: "test", path: "/metadata/uid", value: meta.uid },
    { op: "test", path: "/metadata/resourceVersion", value: meta.resourceVersion },
    { op: "test", path: "/metadata", value: secret.metadata },
    { op: "test", path: "/type", value: secret.type },
    { op: "test", path: "/data", value: secret.data },
    { op: "add", path: "/metadata/annotations", value: metadata.annotations },
    { op: "add", path: "/metadata/labels", value: metadata.labels },
    { op: "replace", path: "/data", value: data },
  ];
}

async function assertNoPods(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation): Promise<void> {
  const captured = new Set(receipt.captured?.map(value => at(value, "identity", "uid")));
  for (const scope of activation.namespaces) for (const pod of await inventory(run, "pods", scope.namespace.name)) {
    if (captured.has(reviewed(pod, true).uid) || await requiresPrivatePodReview(run, pod, scope, activation)
      || await reviewedOwner(run, pod, scope, undefined, activation.root)) {
      throw new Error("Private execution reappeared during migration; Pending protection was preserved");
    }
  }
}

async function rotatedPublicCa(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation): Promise<void> {
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

export async function finalizeRootMigration(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal,
  publishGrant: (run: Execute, document: unknown) => Promise<void>,
): Promise<RootMigrationReceipt> {
  validateReceipt(receipt);
  if (receipt.supersededBy) throw new Error("Migration was superseded; use its successor receipt");
  receipt = structuredClone(receipt);
  const { plan } = receipt, old = originalActivation(plan), ns = old.root.namespace.name;
  run = replay(run, old);
  await authority(run);
  assertNoCorePublication(await read(run, "namespace", ns));
  if (receipt.historicalHookRetirement && receipt.historicalHookRetirement.phase !== "fenced") {
    throw new Error("Resume historical monitoring hook retirement before finalize");
  }
  if (receipt.publication && (receipt.publication.phase !== "verified"
    || (receipt.publication.witnessWriter && !receipt.publication.witnessWriter.restored))) {
    throw new Error("Core publication is not durably verified or its datapath writer is not restored; resume its original publisher before finalize");
  }
  if (!["rotation-required", "qualifying", "complete"].includes(receipt.phase)) throw new Error("Migration must finish real root retirement before finalize");
  await settleControlTransition(run, receipt, journal);
  const root = await read(run, "deployment", old.root.deployment.name, ns);
  const normalized = (spec: unknown) => {
    const value = structuredClone(record(spec));
    value.replicas = 0;
    const fields = at(value, "template", "metadata", "annotations");
    if (fields) {
      delete record(fields)[`${P}epoch`];
      if (!Object.keys(record(fields)).length) delete record(at(value, "template", "metadata")).annotations;
    }
    return canonical(value);
  };
  if (reviewed(root).uid !== old.root.deployment.uid || normalized(root.spec) !== normalized(plan.targetSpec)) throw new Error(FAILURE);
  const activation = await previewPrivateActivation(run, ns, old.namespaces.map(scope => ({ namespace: scope.namespace.name })), [], ns, old.profile, companionReferences(plan));
  if (activation.bundleRevision !== publicationBundleRevision(receipt) || canonical(activation.controllerUids) !== canonical(old.controllerUids)
    || activation.root.replicaIntent !== old.root.replicaIntent
    || activation.root.account.uid !== old.root.account.uid) throw new Error(FAILURE);
  await rotatedPublicCa(run, receipt, activation);
  let qualified: PrivateActivation;
  const continuity = await reviewPrivateContinuity(run, activation);
  if (continuity?.sealed) {
    qualified = continuity.proof.activation;
    if (!receipt.retirement || continuity.state.attempt !== receipt.retirement.attempt
      || continuity.state.binding !== (receipt.controlTransition?.applied ? rootControlRetirement(receipt.controlTransition).binding : receipt.retirement.binding)
      || canonical(continuity.state.captured) !== canonical(receipt.retirement.captured)
      || canonical(continuity.state.baseline ?? null) !== canonical(receipt.retirement.baseline ?? null)
      || canonical(continuity.state.exposedKeys) !== canonical(receipt.retirement.exposedKeys)) throw new Error(FAILURE);
  } else {
    await scopeFence(run, receipt, { rootFlexible: receipt.phase === "qualifying" });
    await noOwnedAuthority(run, plan);
    const current = retirementState(await read(run, "namespace", ns));
    if (!current || !receipt.retirement || current.attempt !== receipt.retirement.attempt
      || current.binding !== receipt.retirement.binding || canonical(current.captured) !== canonical(receipt.retirement.captured)
      || canonical(current.baseline ?? null) !== canonical(receipt.retirement.baseline ?? null)
      || canonical(current.exposedKeys) !== canonical(receipt.retirement.exposedKeys)) throw new Error(FAILURE);
    receipt.phase = "qualifying";
    await save(journal, receipt);
    const fencedStage: Execute = async (args, input) => {
      if (args[0] === "patch") {
        await scopeFence(run, receipt, { rootFlexible: true });
        await noOwnedAuthority(run, plan);
      }
      return run(args, input);
    };
    try {
      qualified = await stagePrivateActivation(fencedStage, activation);
    } catch (error) {
      if (!(error instanceof PrivateQualificationConsumerChanged)) throw error;
      await settleControlTransition(run, receipt, journal);
      if (!receipt.controlTransition?.applied) throw error;
      qualified = JSON.parse(receipt.controlTransition.after).activation as PrivateActivation;
    }
    await settleControlTransition(run, receipt, journal);
    if (receipt.controlTransition?.applied) qualified = JSON.parse(receipt.controlTransition.after).activation as PrivateActivation;
  }
  if (qualified.namespaces.some(scope => scope.epoch === old.namespaces.find(value => value.namespace.uid === scope.namespace.uid)?.epoch)) throw new Error("Migration did not establish a fresh epoch");
  await validateQualifiedActivation(run, qualified);
  qualified = await restoreCompanions(run, receipt, qualified, journal);
  await readyExecution(run, receipt, qualified);
  const current = await read(run, GRANTS, "workspace", ns);
  const alreadyPublished = canonical(current.spec) !== canonical(quiescent(plan));
  let spec: ObjectValue;
  let publicationReview: PrivateActivation | undefined;
  if (alreadyPublished) {
    const published = at(current, "spec", "privateActivation") as unknown as PrivateActivation;
    await validateQualifiedActivation(run, published);
    spec = record({ ...record(plan.grant.spec), privateActivation: published });
    if (canonical(current.spec) !== canonical(spec)) throw new Error("Retained grant intent changed during migration publication");
  } else {
    const writers = at(plan.grant, "spec", "writers") as ObjectValue[];
    if (at(plan.grant, "spec", "enabled") !== false && writers.length) {
      publicationReview = await previewPrivateActivation(run, ns, old.namespaces.map(scope => ({ namespace: scope.namespace.name })), [], ns, old.profile, companionReferences(plan));
      spec = record({ ...record(plan.grant.spec), privateActivation: { ...publicationReview, phase: "qualified" } });
    } else spec = record({ ...record(plan.grant.spec), privateActivation: qualified });
  }
  await scopeFence(run, receipt, { rootFlexible: true, grantSpec: alreadyPublished ? spec : quiescent(plan), writerTransition: alreadyPublished, qualification: qualified });
  if (!alreadyPublished) {
    await noOwnedAuthority(run, plan);
    const fencedPublication: Execute = async (args, input) => {
      if (args[0] === "patch") {
        await scopeFence(run, receipt, { rootFlexible: true, qualification: qualified });
        await noOwnedAuthority(run, plan);
        await readyExecution(run, receipt, qualified);
      }
      return run(args, input);
    };
    await publishGrant(fencedPublication, { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: ns, uid: reviewed(current).uid, resourceVersion: reviewed(current).resourceVersion },
      spec: publicationReview ? { ...spec, privateActivation: publicationReview } : spec });
  }
  await scopeFence(run, receipt, { rootFlexible: true, grantSpec: spec, writerTransition: true, qualification: qualified });
  await validateQualifiedActivation(run, qualified);
  const deadline = Date.now() + 120_000;
  for (;;) {
    const grant = await read(run, GRANTS, "workspace", ns);
    const conditions = at(grant, "status", "conditions"), writers = at(spec, "writers") as ObjectValue[];
    if (reviewed(grant).uid !== reviewed(plan.grant).uid || canonical(grant.spec) !== canonical(spec)) throw new Error(FAILURE);
    const expected = spec.enabled !== false && writers.length ? "True" : "False";
    if (at(grant, "status", "observedGeneration") === at(grant, "metadata", "generation")
      && Array.isArray(conditions) && conditions.some(c => at(c, "type") === "WriterReady" && at(c, "status") === expected)) break;
    if (Date.now() >= deadline) throw new Error("Restored grant is not acknowledged; migration finalize is resumable, not accepted");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  await scopeFence(run, receipt, { rootFlexible: true, grantSpec: spec, writerTransition: true, qualification: qualified });
  await validateQualifiedActivation(run, qualified);
  await readyExecution(run, receipt, qualified);
  await rotatedPublicCa(run, receipt, await previewPrivateActivation(run, ns, old.namespaces.map(scope => ({ namespace: scope.namespace.name })), [], ns, old.profile, companionReferences(plan)));
  receipt.phase = "complete";
  await save(journal, receipt);
  return receipt;
}

/** Recheck completed migration evidence without changing the cluster or its retained journal. */
export async function reviewCompletedRootMigration(run: Execute, receipt: RootMigrationReceipt): Promise<ObjectValue[]> {
  if (receipt.phase !== "complete") throw new Error("No-op upgrade review requires a completed root migration");
  const readonly: Execute = async (args, input) => {
    const projection = args[0] === "create" && args[1] === "--raw"
      && /^\/api\/v1\/namespaces\/[^/]+\/pods\?dryRun=All&fieldValidation=Strict$/.test(args[2] ?? "");
    if (!["get", "auth"].includes(args[0]!) && !projection) throw new Error("Completed migration review cannot change execution or authority");
    return run(args, input);
  };
  validateReceipt(receipt);
  const deployments: ObjectValue[] = [];
  for (const original of [receipt.plan.original, ...receipt.plan.companions ?? []]) {
    const current = await read(readonly, "deployment", reviewed(original).name, String(at(original, "metadata", "namespace")));
    if (reviewed(current).uid !== reviewed(original).uid || replicaIntent(current) !== rootControlReplicas(original, receipt.controlTransition)) throw new Error(FAILURE);
    deployments.push(current);
  }
  await finalizeRootMigration(readonly, receipt, { save: async current => {
    if (canonical(current) !== canonical(receipt)) throw new Error("Completed migration evidence changed during no-op review");
  } }, async () => { throw new Error("Completed migration review cannot republish grant intent"); });
  for (const [index, before] of deployments.entries()) {
    const current = await read(readonly, "deployment", reviewed(before).name, String(at(before, "metadata", "namespace")));
    if (reviewed(current).uid !== reviewed(before).uid || canonical(current.spec) !== canonical(before.spec)) throw new Error(FAILURE);
    deployments[index] = current;
  }
  return deployments;
}

async function settleControlTransition(run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal): Promise<void> {
  if (receipt.phase !== "qualifying") return;
  if (receipt.controlTransition) {
    const previous = receipt.controlTransition;
    const transition = await prepareRootControlTransition(run, receipt.plan.review.release.namespace,
      receipt.plan.companions ?? [], previous.qualification, previous);
    if (!transition) throw new Error(FAILURE);
    if (transition !== previous) {
      await noOwnedAuthority(run, receipt.plan);
      receipt.controlTransition = transition;
      validateReceipt(receipt);
      await save(journal, receipt);
    }
    if (receipt.controlTransition.applied) return;
  } else {
    const namespace = receipt.plan.review.release.namespace;
    const current = await read(run, "namespace", namespace);
    const raw = at(current, "metadata", "annotations", FIELD);
    let qualification: PrivateActivation | undefined;
    if (typeof raw === "string" && record(JSON.parse(raw)).version === 1 && retirementState(current)?.phase === "restoring") {
      qualification = structuredClone(receipt.activation!);
      qualification.phase = "qualified";
      const root = await read(run, "deployment", qualification.root.deployment.name, namespace);
      const tls = await reviewBudgetTls(run, root, namespace);
      if (tls) qualification.root.budgetTls = tls;
      for (const scope of qualification.namespaces) {
        const live = await read(run, "namespace", scope.namespace.name);
        const epoch = at(live, "metadata", "annotations", `${P}epoch`);
        if (reviewed(live).uid !== scope.namespace.uid || typeof epoch !== "string") throw new Error(FAILURE);
        scope.epoch = epoch;
      }
    }
    const transition = await prepareRootControlTransition(run, namespace, receipt.plan.companions ?? [], qualification);
    if (!transition) return;
    await noOwnedAuthority(run, receipt.plan);
    receipt.controlTransition = transition;
    validateReceipt(receipt);
    await save(journal, receipt);
  }
  await applyRootControlTransition(run, receipt.controlTransition, async historical => {
    await scopeFence(historical, receipt, { rootFlexible: true });
    await noOwnedAuthority(run, receipt.plan);
  });
  receipt.controlTransition.applied = true;
  await save(journal, receipt);
}

async function restoreCompanions(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation,
  journal: RootMigrationJournal): Promise<PrivateActivation> {
  const { plan } = receipt;
  for (const before of plan.companions ?? []) {
    const ns = String(at(before, "metadata", "namespace"));
    await companionFence(run, receipt);
    const current = await read(run, "deployment", reviewed(before).name, ns);
    const namespace = await read(run, "namespace", ns);
    const controllerOwned = at(namespace, "metadata", "labels", "kars.azure.com/inference-budget") === "v1";
    const expected = rootControlReplicas(before, receipt.controlTransition), epoch = activation.namespaces.find(scope => scope.namespace.name === ns)!.epoch;
    if (!epoch) throw new Error(FAILURE);
    const needsEpoch = at(current, "spec", "template", "metadata", "annotations", `${P}epoch`) !== epoch;
    if ((!controllerOwned && replicaIntent(current) !== expected) || needsEpoch) {
      if (receipt.phase !== "qualifying" || (needsEpoch && replicaIntent(current) !== 0)) {
        throw new Error("Companion restoration requires unpublished migration qualification");
      }
      const spec = structuredClone(record(current.spec)), template = record(spec.template);
      const metadata = record(template.metadata ?? {});
      template.metadata = { ...metadata, annotations: { ...record(metadata.annotations ?? {}), [`${P}epoch`]: epoch } };
      spec.replicas = controllerOwned ? 0 : expected;
      await patchSpec(run, current, spec, async () => {
        await scopeFence(run, receipt, { rootFlexible: true, qualification: activation });
        await noOwnedAuthority(run, plan);
        await validateQualifiedActivation(run, activation);
      });
    }
    const deadline = Date.now() + 120_000;
    for (;;) {
      if (controllerOwned) {
        await settleControlTransition(run, receipt, journal);
        if (receipt.controlTransition?.applied) activation = JSON.parse(receipt.controlTransition.after).activation as PrivateActivation;
      }
      await companionFence(run, receipt);
      const live = await read(run, "deployment", reviewed(before).name, ns);
      const replicas = rootControlReplicas(before, receipt.controlTransition);
      if (replicaIntent(live) === replicas
        && at(live, "status", "observedGeneration") === at(live, "metadata", "generation")
        && (at(live, "status", "updatedReplicas") ?? 0) === replicas
        && (at(live, "status", "availableReplicas") ?? 0) === replicas) break;
      if (Date.now() >= deadline) throw new Error("Restored companion is not Ready; migration finalize is resumable and writers remain unpublished");
      await new Promise(resolve => setTimeout(resolve, 500));
    }
  }
  await validateQualifiedActivation(run, activation);
  return activation;
}

async function readyExecution(run: Execute, receipt: RootMigrationReceipt, activation: PrivateActivation): Promise<void> {
  const counts = new Map<string, number>();
  for (const scope of activation.namespaces) {
    const captured = new Set(receipt.retirement?.captured[scope.namespace.uid]);
    for (const pod of await inventory(run, "pods", scope.namespace.name)) {
      const owner = await reviewedOwner(run, pod, scope, undefined, activation.root);
      if (!owner && !await requiresPrivatePodReview(run, pod, scope, activation)) continue;
      const conditions = at(pod, "status", "conditions");
      if (!owner || captured.has(reviewed(pod, true).uid)
        || at(pod, "metadata", "deletionTimestamp") || at(pod, "metadata", "annotations", `${P}epoch`) !== scope.epoch
        || at(pod, "status", "phase") !== "Running" || !Array.isArray(conditions)
        || !conditions.some(value => at(value, "type") === "Ready" && at(value, "status") === "True")) {
        throw new Error("Migration target Pods are not genuinely new, reviewed and Ready; writers remain unpublished");
      }
      counts.set(owner.object.uid, (counts.get(owner.object.uid) ?? 0) + 1);
    }
  }
  const expected = [[activation.root.deployment.uid, activation.root.replicaIntent],
    ...(receipt.plan.companions ?? []).map(value => [reviewed(value).uid, rootControlReplicas(value, receipt.controlTransition)])] as [string, number][];
  if (expected.some(([uid, replicas]) => (counts.get(uid) ?? 0) !== replicas)) {
    throw new Error("Migration target Ready Pod count differs from retained replica intent");
  }
}
