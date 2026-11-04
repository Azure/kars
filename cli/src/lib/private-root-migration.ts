// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, createPrivateKey, createPublicKey, X509Certificate } from "node:crypto";
import {
  annotations, assertNoCorePublication, at, canonical, digest, PRIVATE_PREFIX as P, previewPrivateActivation,
  read, record, reviewed, reviewedOwner, requiresPrivatePodReview, reviewBudgetTls, stagePrivateActivation,
  templateDigest, validatePrivateActivation, validateQualifiedActivation, type Execute,
  type PrivateActivation,
} from "./private-activation.js";
import { PrivateQualificationConsumerChanged, reviewPrivateContinuity } from "./private-activation-continuity.js";
import { captureGuardRetirement } from "./private-activation-guard-retirement.js";
import { retireGrantWriters } from "./private-grant-writer-retirement.js";
import {
  assertRetiredRoot, recordRetiredBudgetBaseline, replicaIntent, retirementState, startRetirement,
  type RootRetirement,
} from "./private-activation-retirement.js";
import {
  previewPrivateRootMigration, rootMigrationReviewDigest, type PrivateRootMigrationReview,
} from "./private-root-migration-review.js";
import {
  applyRootControlTransition, prepareRootControlTransition, rootControlReplicas, rootControlRetirement,
} from "./private-root-control-transition.js";
import {
  type RootMigrationJournal, type RootMigrationReceipt, supportedTopology, replay, GRANTS, type ObjectValue,
  FAILURE, inventories, type Plan, FIELD, originalActivation, assertTarget, retiringExecution, inventory,
  validateReceipt, companionFence, save, authority, routerBaselineTarget, companionReferences,
  rotatedPublicCa, scopeFence, noOwnedAuthority, sameBudget, predecessorAuthority, retiringRoot, patchFields,
  patchSpec, retiredPredecessor, assertNoPods, pendingScope, scopeSnapshot, publicationBundleRevision,
  quiescent,
} from "./private-root-migration-validation.js";

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

export { type RootMigrationReceipt, type RootMigrationJournal } from "./private-root-migration-validation.js";
export {
  commitRootPublication, ownRootPublication, claimRootPublicationLaunch, claimRootPublicationRecovery,
  assertRetiredRootPublication, advanceRootPublicationBundle, releaseRootPublication,
} from "./private-root-migration-publication.js";
