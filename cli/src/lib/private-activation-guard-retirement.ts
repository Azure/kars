// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, canonical, read, record, reviewed, validatePrivateActivation,
  type Execute, type Json, type PrivateActivation, type ReviewedObject,
} from "./private-activation.js";

const PREFIX = "kars.azure.com/credential-reader-";
const failure = "Namespace changed outside the selected grant's expected writer-guard retirement; re-review";
interface NamespaceSnapshot {
  identity: ReviewedObject;
  object: Record<string, Json>;
  guarded: boolean;
}
export interface GuardRetirementReview {
  activation: PrivateActivation;
  key: string;
  namespaces: NamespaceSnapshot[];
}

export async function captureGuardRetirement(
  execute: Execute, activation: PrivateActivation, grant: unknown,
): Promise<GuardRetirementReview> {
  const key = `${PREFIX}${reviewed(grant).uid}`;
  const writers = at(grant, "spec", "writers");
  if (!Array.isArray(writers) || writers.length > 16) throw new Error("Prior writer review is malformed");
  const writerNamespaces = new Set<string>();
  for (const writer of writers) {
    const name = at(writer, "namespace");
    if (typeof name !== "string" || !name) throw new Error("Prior writer namespace is malformed");
    writerNamespaces.add(name);
  }
  const names = new Set([...activation.namespaces.map(scope => scope.namespace.name), ...writerNamespaces]);
  if (names.size > 80) throw new Error("Writer retirement namespace review exceeds its bound");
  const previousScopes = at(grant, "spec", "privateActivation", "namespaces");
  const namespaces: NamespaceSnapshot[] = [];
  for (const name of names) {
    const object = await read(execute, "namespace", name);
    const identity = reviewed(object);
    const expected = activation.namespaces.find(scope => scope.namespace.name === name)?.namespace;
    if (expected && (identity.uid !== expected.uid || identity.resourceVersion !== expected.resourceVersion)) throw new Error(failure);
    const previous = Array.isArray(previousScopes) ? previousScopes.find(scope => at(scope, "namespace", "name") === name) : undefined;
    if (previous && at(previous, "namespace", "uid") !== identity.uid) throw new Error(failure);
    const finalizers = at(object, "metadata", "finalizers") ?? [];
    if (!Array.isArray(finalizers) || finalizers.some(value => typeof value !== "string")) throw new Error(failure);
    const label = at(object, "metadata", "labels", key);
    const annotation = at(object, "metadata", "annotations", key);
    const present = label !== undefined || annotation !== undefined || finalizers.includes(key);
    const guarded = writerNamespaces.has(name) && present;
    if (guarded && (label !== identity.uid || annotation !== activation.root.account.uid
      || !finalizers.includes(key) || !Array.isArray(at(object, "spec", "finalizers"))
      || !(at(object, "spec", "finalizers") as Json[]).includes("kubernetes"))) throw new Error(failure);
    namespaces.push({ identity, object, guarded });
  }
  return { activation: structuredClone(activation), key, namespaces };
}

function comparable(value: Record<string, Json>, removed = false): string {
  const object = structuredClone(value);
  const metadata = record(object.metadata);
  delete metadata.resourceVersion;
  if (removed) {
    // Kubernetes omits empty metadata maps/lists after the last guard is removed.
    for (const key of ["labels", "annotations"]) {
      if (metadata[key] !== undefined && !Object.keys(record(metadata[key])).length) delete metadata[key];
    }
    if (Array.isArray(metadata.finalizers) && metadata.finalizers.length === 0) delete metadata.finalizers;
  }
  return canonical(object);
}

function released(snapshot: NamespaceSnapshot, key: string): Record<string, Json> {
  const expected = structuredClone(snapshot.object);
  const metadata = record(expected.metadata);
  delete record(metadata.labels)[key];
  delete record(metadata.annotations)[key];
  metadata.finalizers = (metadata.finalizers as Json[]).filter(value => value !== key);
  return expected;
}

/** Called only after the selected grant's acknowledgement and owned-role absence checks. */
export async function refreshGuardRetirement(
  execute: Execute, review: GuardRetirementReview,
): Promise<PrivateActivation | undefined> {
  const activation = structuredClone(review.activation);
  let pending = false;
  for (const snapshot of review.namespaces) {
    const current = await read(execute, "namespace", snapshot.identity.name);
    const identity = reviewed(current);
    if (identity.uid !== snapshot.identity.uid) throw new Error(failure);
    if (canonical(current) === canonical(snapshot.object)) {
      pending ||= snapshot.guarded;
      continue;
    }
    if (!snapshot.guarded || identity.resourceVersion === snapshot.identity.resourceVersion
      || comparable(current, true) !== comparable(released(snapshot, review.key), true)) throw new Error(failure);
    const scope = activation.namespaces.find(scope => scope.namespace.name === identity.name);
    if (scope) scope.namespace.resourceVersion = identity.resourceVersion;
  }
  if (pending) return undefined;
  await validatePrivateActivation(execute, activation);
  return activation;
}
