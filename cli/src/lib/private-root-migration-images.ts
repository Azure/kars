// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, canonical, record } from "./private-activation.js";

type ObjectValue = ReturnType<typeof record>;
const FAILURE = "Companion migration requires a changed immutable image-only Deployment spec";
const immutable = (value: unknown): value is string => typeof value === "string" && /^[^@\s]+@sha256:[a-f0-9]{64}$/.test(value);
export interface CompanionImageDelta {
  group: "containers" | "initContainers";
  name: string;
  before: string;
  after: string;
}

export function immutableMigrationImages(spec: ObjectValue): void {
  const pod = record(at(spec, "template", "spec"));
  if (!Array.isArray(pod.containers) || !pod.containers.length) throw new Error(FAILURE);
  for (const group of [pod.containers, pod.initContainers ?? []]) {
    if (!Array.isArray(group) || group.some(value => !immutable(at(value, "image")))) throw new Error(FAILURE);
  }
}

/** Only existing image fields may differ; ordering, names and every other spec byte remain bound. */
export function companionImageDelta(original: ObjectValue, target: ObjectValue): CompanionImageDelta[] {
  immutableMigrationImages(original);
  immutableMigrationImages(target);
  const expected = structuredClone(original), changes: CompanionImageDelta[] = [];
  for (const group of ["containers", "initContainers"] as const) {
    const before = (at(expected, "template", "spec", group) ?? []) as ObjectValue[];
    const after = (at(target, "template", "spec", group) ?? []) as ObjectValue[];
    if (before.length !== after.length) throw new Error(FAILURE);
    const names = new Set<string>();
    for (const [index, container] of before.entries()) {
      const proposed = after[index]!;
      if (typeof container.name !== "string" || !container.name || names.has(container.name)
        || proposed.name !== container.name) throw new Error(FAILURE);
      names.add(container.name);
      if (container.image !== proposed.image) changes.push({ group, name: container.name,
        before: container.image as string, after: proposed.image as string });
      container.image = proposed.image;
    }
  }
  if (!changes.length || canonical(expected) !== canonical(target)) throw new Error(FAILURE);
  return changes;
}
