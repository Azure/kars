// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  canonicalSchema, normalizedCrd, SCHEMA_DIGEST, schemaDigest, schemaIdentity, schemaOwnerFields,
  verifyNewSchemaPreviewOwner, verifySchemaOwner, type ObjectMap, type SchemaOwner,
} from "./schema-documents.js";

export function buildSchemaWriteRequest(desired: ObjectMap, current: ObjectMap | undefined, owner: SchemaOwner): {
  object: ObjectMap; args: string[];
} {
  const fields = schemaOwnerFields(owner);
  const object = { apiVersion: desired.apiVersion, kind: desired.kind, spec: desired.spec, metadata: {
    ...desired.metadata,
    ...(current ? schemaIdentity(current) : {}),
    labels: { ...desired.metadata.labels, ...fields.labels },
    annotations: { ...desired.metadata.annotations, ...fields.annotations,
      [SCHEMA_DIGEST]: schemaDigest(normalizedCrd(desired)) },
  } };
  const manager = owner.ownership === "helm" ? "helm" : "kars-schema-stage";
  const args = current
    ? ["apply", "--server-side", `--field-manager=${manager}`, "-f", "-", "-o", "json"]
    : ["create", `--field-manager=${manager}`, "-f", "-", "-o", "json"];
  return { object, args };
}

export function verifySchemaWritePreview(
  checked: ObjectMap, desired: ObjectMap, owner: SchemaOwner, currentUid?: string,
): void {
  if ((currentUid && schemaIdentity(checked).uid !== currentUid)
    || canonicalSchema(normalizedCrd(checked)) !== canonicalSchema(normalizedCrd(desired))) {
    throw new Error("Migration schema dry-run returned another identity or schema");
  }
  if (currentUid) verifySchemaOwner(checked, owner);
  else verifyNewSchemaPreviewOwner(checked, owner);
}
