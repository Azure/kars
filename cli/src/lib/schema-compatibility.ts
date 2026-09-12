// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { canonicalSchema, normalizedCrd, type ObjectMap } from "./schema-documents.js";

export const CRD_RETENTION = "helm.sh/resource-policy";
const documentation = new Set(["description", "title", "$comment", "example", "examples"]);

function migration(path: string): never {
  throw new Error(`Potentially lossy or incompatible schema transition at ${path}; explicit reviewed data/schema migration is required`);
}

function attributes(schema: ObjectMap): ObjectMap {
  return Object.fromEntries(Object.entries(schema).filter(([key]) =>
    !documentation.has(key) && !["properties", "items", "additionalProperties"].includes(key)));
}

function containsDefault(value: unknown): boolean {
  return !!value && typeof value === "object" && (Object.hasOwn(value, "default")
    || Object.values(value).some(containsDefault));
}

function retainedSchema(before: ObjectMap, after: ObjectMap, path: string): void {
  if (canonicalSchema(attributes(before)) !== canonicalSchema(attributes(after))) migration(path);
  for (const key of ["items", "additionalProperties"]) {
    const old = before[key];
    const next = after[key];
    if (old === undefined && next === undefined) continue;
    if (old && next && typeof old === "object" && typeof next === "object" && !Array.isArray(old) && !Array.isArray(next)) {
      retainedSchema(old, next, `${path}/${key}`);
    } else if (canonicalSchema(old ?? null) !== canonicalSchema(next ?? null)) {
      migration(`${path}/${key}`);
    }
  }
  const previous = before.properties ?? {};
  const desired = after.properties ?? {};
  for (const [name, schema] of Object.entries(previous)) {
    if (!Object.hasOwn(desired, name)) migration(`${path}/properties/${name}`);
    retainedSchema(schema as ObjectMap, desired[name], `${path}/properties/${name}`);
  }
  for (const [name, schema] of Object.entries(desired)) {
    if (Object.hasOwn(previous, name)) continue;
    // A newly typed property can narrow previously persisted arbitrary data.
    // Defaults can also change existing objects and ancestor CEL validation.
    if (before["x-kubernetes-preserve-unknown-fields"] || before.additionalProperties || containsDefault(schema)) {
      migration(`${path}/properties/${name}`);
    }
  }
}

/** Deliberately conservative: unchanged constraints and storage semantics,
 * retained fields/types, and only optional, non-defaulted property additions. */
export function assertSchemaCompatibility(before: ObjectMap, after: ObjectMap): void {
  const from = normalizedCrd(before);
  const to = normalizedCrd(after);
  const name = before.metadata.name;
  if (name !== after.metadata.name || canonicalSchema({ ...from, versions: [] }) !== canonicalSchema({ ...to, versions: [] })) {
    migration(name);
  }
  if (from.versions.length !== to.versions.length) migration(`${name}/versions`);
  for (const version of from.versions) {
    const next = to.versions.find((candidate: ObjectMap) => candidate.name === version.name);
    if (!next) migration(`${name}/${version.name}`);
    const descriptor = (value: ObjectMap) => Object.fromEntries(Object.entries(value).filter(([key]) =>
      !["schema", "additionalPrinterColumns", "deprecated", "deprecationWarning"].includes(key)));
    if (canonicalSchema(descriptor(version)) !== canonicalSchema(descriptor(next))) migration(`${name}/${version.name}`);
    retainedSchema(version.schema.openAPIV3Schema, next.schema.openAPIV3Schema, `${name}/${version.name}/schema`);
  }
}

export function requireCrdRetention(documents: ObjectMap[]): void {
  for (const object of documents.filter(value => value.kind === "CustomResourceDefinition")) {
    if (object.metadata.annotations?.[CRD_RETENTION] !== "keep"
      || object.metadata.annotations?.["helm.sh/hook"] || object.metadata.annotations?.["helm.sh/hook-delete-policy"]) {
      throw new Error(`CRD ${object.metadata.name} lacks complete Helm retention; explicit retention migration is required`);
    }
  }
}

export function assertNoCrdRemoval(before: ObjectMap[], after: ObjectMap[]): void {
  const names = new Set(after.filter(object => object.kind === "CustomResourceDefinition").map(object => object.metadata.name));
  if (before.some(object => object.kind === "CustomResourceDefinition" && !names.has(object.metadata.name))) {
    throw new Error("Operation would remove a core CRD; explicit schema/data migration is required");
  }
}

export function assertRollbackCompatibility(from: ObjectMap[], rollback: ObjectMap[]): void {
  requireCrdRetention(rollback);
  const previous = new Map(rollback.filter(object => object.kind === "CustomResourceDefinition").map(object => [object.metadata.name, object]));
  for (const object of from.filter(value => value.kind === "CustomResourceDefinition")) {
    const target = previous.get(object.metadata.name);
    if (target) assertSchemaCompatibility(object, target);
    else requireCrdRetention([object]); // New CRDs must survive failed-install/rollback cleanup.
  }
}
