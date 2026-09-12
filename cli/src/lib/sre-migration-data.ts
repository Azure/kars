// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { canonicalSchema, schemaDigest, schemaIdentity, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { schemaField, schemaStep } from "./sre-schema-diagnostics.js";

export interface MigrationDataSnapshot {
  crd: ObjectMap;
  digest: string;
  count: number;
  bytes: number;
}

function addedFieldsAbsent(before: ObjectMap, after: ObjectMap, data: unknown, path: string): void {
  if (data === null || data === undefined) return;
  if (Array.isArray(data)) {
    if (before.items && after.items) for (const value of data) addedFieldsAbsent(before.items, after.items, value, `${path}/*`);
    return;
  }
  if (typeof data !== "object") return;
  const object = data as ObjectMap;
  for (const [name, next] of Object.entries(after.properties ?? {})) {
    if (!Object.hasOwn(object, name)) continue;
    if (!Object.hasOwn(before.properties ?? {}, name)) {
      schemaField(`${path}/${name}`.split("/").slice(1).join("/"));
      throw new Error(`Existing data contains a post-BASE365 field at ${path}/${name}; no authority is grandfathered`);
    }
    addedFieldsAbsent(before.properties[name], next as ObjectMap, object[name], `${path}/${name}`);
  }
  if (before.additionalProperties && after.additionalProperties
    && typeof before.additionalProperties === "object" && typeof after.additionalProperties === "object") {
    for (const [name, value] of Object.entries(object)) {
      if (!Object.hasOwn(before.properties ?? {}, name)) {
        addedFieldsAbsent(before.additionalProperties, after.additionalProperties, value, `${path}/*`);
      }
    }
  }
}

function endpoint(crd: ObjectMap, object?: ObjectMap): string {
  const base = `/apis/kars.azure.com/v1alpha1`;
  const namespace = object && crd.spec.scope === "Namespaced" ? `/namespaces/${encodeURIComponent(object.metadata.namespace)}` : "";
  return `${base}${namespace}/${crd.spec.names.plural}${object ? `/${encodeURIComponent(object.metadata.name)}` : ""}`;
}

async function inventory(execute: SchemaExecute, crd: ObjectMap): Promise<ObjectMap[]> {
  schemaStep("data-inventory", crd.spec.names.kind);
  const { stdout } = await execute("kubectl", ["get", "--raw", `${endpoint(crd)}?limit=513`, "--request-timeout=20s"],
    { stdio: "pipe", timeout: 25_000 });
  schemaStep("data-list-shape", crd.spec.names.kind);
  const result: unknown = JSON.parse(stdout);
  if (!result || typeof result !== "object" || Array.isArray(result)) throw new Error("Migration data inventory is malformed");
  const list = result as ObjectMap;
  if (!Array.isArray(list.items) || list.items.length > 512 || list.metadata?.continue) {
    throw new Error("Migration requires a complete bounded custom-resource inventory (at most 512 objects)");
  }
  const seen = new Set<string>();
  for (const object of list.items) {
    schemaStep("data-item-identity", crd.spec.names.kind, object);
    const { uid } = schemaIdentity(object);
    if (seen.has(uid) || object.apiVersion !== "kars.azure.com/v1alpha1" || object.kind !== crd.spec.names.kind
      || (crd.spec.scope === "Namespaced" && (typeof object.metadata.namespace !== "string" || !object.metadata.namespace))) {
      throw new Error("Migration data inventory contains an ambiguous object identity");
    }
    seen.add(uid);
  }
  return list.items.sort((a: ObjectMap, b: ObjectMap) => a.metadata.uid.localeCompare(b.metadata.uid));
}

export async function qualifyMigrationData(
  execute: SchemaExecute, current: ObjectMap, desired: ObjectMap,
): Promise<MigrationDataSnapshot> {
  const objects = await inventory(execute, current);
  const before = current.spec.versions[0].schema.openAPIV3Schema;
  const after = desired.spec.versions[0].schema.openAPIV3Schema;
  let bytes = 0;
  for (const object of objects) {
    schemaStep("data-fields", current.spec.names.kind);
    bytes += Buffer.byteLength(canonicalSchema(object));
    if (bytes > 8 * 1024 * 1024) throw new Error("Migration data review exceeds its 8 MiB bound");
    addedFieldsAbsent(before, after, object, object.kind);
    if (object.kind === "KarsSandbox" && object.spec?.credentialsRef
      && (typeof object.spec.credentialsRef.name !== "string"
        || !/^kars-credential-source-[a-z0-9][a-z0-9-]*$/.test(object.spec.credentialsRef.name))) {
      schemaField("spec/credentialsRef");
      throw new Error("An existing Sandbox credential reference is not a canonical v1 source; no bundle authority is grandfathered");
    }
    // Server validation uses the still-installed before-schema and unchanged
    // object/UID/RV. It is a dry-run PUT, never a data migration or status write.
    schemaStep("data-server-validation", current.spec.names.kind);
    const validation = await execute("kubectl", ["replace", "--raw", `${endpoint(current, object)}?dryRun=All`,
      "-f", "-", "--request-timeout=20s"], { stdio: "pipe", input: JSON.stringify(object), timeout: 25_000 });
    const checked: ObjectMap = JSON.parse(validation.stdout);
    schemaStep("data-returned-identity", current.spec.names.kind, checked);
    const identity = schemaIdentity(checked);
    schemaStep("data-round-trip", current.spec.names.kind);
    if (identity.uid !== object.metadata.uid || checked.metadata.name !== object.metadata.name
      || checked.metadata.namespace !== object.metadata.namespace
      || ["labels", "annotations", "ownerReferences", "finalizers"].some(key =>
        canonicalSchema(checked.metadata[key] ?? null) !== canonicalSchema(object.metadata[key] ?? null))
      || canonicalSchema(checked.spec ?? null) !== canonicalSchema(object.spec ?? null)
      || canonicalSchema(checked.status ?? null) !== canonicalSchema(object.status ?? null)) {
      throw new Error("Stored custom-resource data does not round-trip unchanged through server validation");
    }
  }
  return { crd: current, digest: schemaDigest(objects), count: objects.length, bytes };
}

export async function recheckMigrationData(execute: SchemaExecute, snapshot: MigrationDataSnapshot): Promise<void> {
  const actual = await inventory(execute, snapshot.crd);
  schemaStep("data-recheck", snapshot.crd.spec.names.kind);
  if (schemaDigest(actual) !== snapshot.digest) {
    throw new Error("Custom-resource data/UID/resourceVersion changed during migration qualification; no unchecked writes may continue");
  }
}

export async function requireNoNewAuthorities(execute: SchemaExecute, crd: ObjectMap): Promise<void> {
  const objects = await inventory(execute, crd);
  schemaStep("new-authorities", crd.spec.names.kind);
  if (objects.length) {
    throw new Error(`BASE365 migration cannot grandfather existing post-baseline authority objects for ${crd.spec.names.kind}`);
  }
}
