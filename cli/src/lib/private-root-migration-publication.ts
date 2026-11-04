// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  assertNoCorePublication, at, canonical, CORE_PUBLICATION, CORE_PUBLICATION_FENCE, PRIVATE_PREFIX as P,
  previewPrivateActivation, read, record, reviewed, type Execute,
} from "./private-activation.js";
import { assertRetiredRoot, retirementBinding, retirementState } from "./private-activation-retirement.js";
import { reviewPublicationBundle } from "./private-publication-bundle.js";
import {
  type RootMigrationReceipt, type RootMigrationJournal, save, FAILURE, validateReceipt, patchFields,
  type ObjectValue, originalActivation, replay, authority, scopeFence, noOwnedAuthority, companionReferences,
  publicationBundleRevision, rotatedPublicCa, FIELD,
} from "./private-root-migration-validation.js";

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
