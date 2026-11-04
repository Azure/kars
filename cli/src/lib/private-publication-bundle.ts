// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, bundleDefinition, canonical, CORE_PUBLICATION_FENCE, digest, read, record, reviewed, verifyPrivateBundle,
  type Execute, type ReviewedObject,
} from "./private-activation.js";

type ObjectValue = ReturnType<typeof record>;
export interface PublicationBundleReview {
  revision: string;
  identities: (ReviewedObject & { kind: string })[];
}
const failure = "Publication admission revision lacks unchanged frozen identities, policy bodies or owned fences";

/** Resource versions remain authoritative. Only an owned publication may prove
 * a new revision from its complete immutable predecessor projection. */
export async function reviewPublicationBundle(
  run: Execute, frozen: readonly ObjectValue[], expected: string, fence: string, requireFence: boolean, predecessorFence?: string,
): Promise<PublicationBundleReview> {
  const revision = await verifyPrivateBundle(run);
  if (revision === expected) return { revision, identities: [] };
  const definitions = bundleDefinition().objects as ObjectValue[];
  const before: (ReviewedObject & { kind: string })[] = [], identities: (ReviewedObject & { kind: string })[] = [];
  const body = (value: ObjectValue) => {
    const copy = structuredClone(value);
    delete copy.status;
    const metadata = record(copy.metadata);
    for (const field of ["uid", "resourceVersion", "generation", "managedFields", "creationTimestamp", "selfLink"]) delete metadata[field];
    if (metadata.annotations) {
      delete record(metadata.annotations)[CORE_PUBLICATION_FENCE];
      if (!Object.keys(record(metadata.annotations)).length) delete metadata.annotations;
    }
    return canonical(copy);
  };
  for (const definition of definitions) {
    const kind = String(definition.kind), name = String(at(definition, "metadata", "name"));
    const matches = frozen.filter(value => value.kind === kind && at(value, "metadata", "name") === name && !at(value, "metadata", "namespace"));
    if (matches.length !== 1) throw new Error(failure);
    const original = matches[0]!, current = await read(run, kind.toLowerCase(), name);
    const old = reviewed(original), live = reviewed(current);
    const marker = at(current, "metadata", "annotations", CORE_PUBLICATION_FENCE);
    if (live.uid !== old.uid || canonical(original.spec) !== canonical(definition.spec) || body(current) !== body(original)
      || (marker !== at(original, "metadata", "annotations", CORE_PUBLICATION_FENCE) && marker !== fence
        && (requireFence || !predecessorFence || marker !== predecessorFence))
      || (requireFence && (marker !== fence || live.resourceVersion === old.resourceVersion))) throw new Error(failure);
    before.push({ kind, ...old });
    identities.push({ kind, ...live });
  }
  if (digest(before) !== expected || digest(identities) !== revision || await verifyPrivateBundle(run) !== revision) throw new Error(failure);
  return { revision, identities };
}
