// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { ACTION_CRD } from "./sre-action-crd.js";
import { assertSchemaCompatibility } from "./schema-compatibility.js";
import { canonicalSchema, normalizedCrd, readSchemaObject, schemaIdentity, type ObjectMap, type SchemaExecute } from "./schema-documents.js";

/** The legacy template path owns only its reviewed SRE API writes. A broader
 * core migration must fail before the separately reviewed action conversion. */
export async function planTemplateAuthoritySchemas(
  execute: SchemaExecute, documents: ObjectMap[],
): Promise<() => Promise<void>> {
  const snapshots: { name: string; current?: ObjectMap }[] = [];
  for (const desired of documents.filter(object => object.kind === "CustomResourceDefinition")) {
    if (desired.metadata.name === ACTION_CRD) continue;
    const current = await readSchemaObject(execute, "customresourcedefinition", desired.metadata.name);
    const registration = desired.metadata.name === "karssreregistrations.kars.azure.com";
    if (!current && !registration) throw new Error("Template authority requires its complete installed core schema inventory");
    if (current && canonicalSchema(normalizedCrd(current)) !== canonicalSchema(normalizedCrd(desired))) {
      if (!registration) throw new Error("Template authority requires a separately reviewed core schema migration before any SRE write");
      assertSchemaCompatibility(current, desired);
    }
    snapshots.push({ name: desired.metadata.name, current });
  }
  return async () => {
    for (const snapshot of snapshots) {
      const current = await readSchemaObject(execute, "customresourcedefinition", snapshot.name);
      if (!snapshot.current) {
        if (current) throw new Error("SRE schema appeared after template preflight");
      } else if (!current || canonicalSchema(schemaIdentity(current)) !== canonicalSchema(schemaIdentity(snapshot.current))
        || canonicalSchema(normalizedCrd(current)) !== canonicalSchema(normalizedCrd(snapshot.current))) {
        throw new Error("SRE/core schema UID/resourceVersion changed after template preflight");
      }
    }
  };
}
