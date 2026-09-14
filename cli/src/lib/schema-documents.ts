// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { parseAllDocuments } from "yaml";

export type SchemaExecute = (file: string, args: readonly string[], options: {
  stdio: "pipe"; input?: string; timeout?: number;
}) => Promise<{ stdout: string }>;
export type ObjectMap = Record<string, any>;
export interface SchemaOwner { release: string; namespace: string; ownership: "helm" | "template" }
export const SCHEMA_OWNER = "kars.azure.com/core-schema-owner";
export const SCHEMA_DIGEST = "kars.azure.com/core-schema-spec";

export function canonicalSchema(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalSchema).join(",")}]`;
  if (value && typeof value === "object") {
    const fields = value as Record<string, unknown>;
    return `{${Object.keys(fields).sort().map(key => `${JSON.stringify(key)}:${canonicalSchema(fields[key])}`).join(",")}}`;
  }
  const result = JSON.stringify(value);
  if (result === undefined) throw new Error("Schema metadata is incomplete");
  return result;
}

export function schemaDigest(value: unknown): string {
  return createHash("sha256").update(canonicalSchema(value)).digest("hex");
}

export function schemaDocuments(text: string): ObjectMap[] {
  return parseAllDocuments(text).flatMap(document => {
    if (document.errors.length) throw new Error("Chart schema render contains invalid YAML");
    const value = document.toJSON();
    if (value === null) return [];
    if (typeof value !== "object" || Array.isArray(value) || !value.kind || !value.metadata?.name) {
      throw new Error("Chart render contains an unidentified object");
    }
    return [value];
  });
}

export function normalizedCrd(object: ObjectMap): ObjectMap {
  const spec = structuredClone(object.spec);
  if (!spec?.names?.kind || !spec.names.plural || spec.group !== "kars.azure.com"
    || object.apiVersion !== "apiextensions.k8s.io/v1" || object.kind !== "CustomResourceDefinition"
    || object.metadata?.name !== `${spec.names.plural}.${spec.group}` || !Array.isArray(spec.versions)
    || !spec.versions.length || !["Namespaced", "Cluster"].includes(spec.scope)) {
    throw new Error("Chart contains an unsupported or incomplete core CRD");
  }
  const names = new Set<string>();
  for (const version of spec.versions) {
    if (typeof version.name !== "string" || names.has(version.name) || typeof version.served !== "boolean"
      || typeof version.storage !== "boolean" || version.schema?.openAPIV3Schema?.type !== "object") {
      throw new Error(`Incomplete served schema in ${object.metadata.name}`);
    }
    names.add(version.name);
    if (version.deprecated === false) delete version.deprecated;
    for (const column of version.additionalPrinterColumns ?? []) if (column.priority === 0) delete column.priority;
  }
  if (spec.versions.filter((version: ObjectMap) => version.storage).length !== 1) throw new Error("CRD must have one storage version");
  for (const key of ["categories", "shortNames"]) if (spec.names[key]?.length === 0) delete spec.names[key];
  if (spec.names.listKind === `${spec.names.kind}List`) delete spec.names.listKind;
  if (spec.names.singular === spec.names.kind.toLowerCase()) delete spec.names.singular;
  if (canonicalSchema(spec.conversion ?? {}) === '{"strategy":"None"}') delete spec.conversion;
  if (spec.preserveUnknownFields === false) delete spec.preserveUnknownFields;
  return spec;
}

export function schemaIdentity(object: ObjectMap): { uid: string; resourceVersion: string } {
  const metadata = object.metadata;
  if (!metadata?.name || typeof metadata.uid !== "string" || !metadata.uid
    || typeof metadata.resourceVersion !== "string" || !metadata.resourceVersion || metadata.deletionTimestamp) {
    throw new Error("Core schema object lacks a live UID/resourceVersion");
  }
  return { uid: metadata.uid, resourceVersion: metadata.resourceVersion };
}

export function schemaOwnerFields(owner: SchemaOwner): { labels: ObjectMap; annotations: ObjectMap } {
  return {
    labels: { "app.kubernetes.io/managed-by": owner.ownership === "helm" ? "Helm" : "kars-schema-stage" },
    annotations: {
      [SCHEMA_OWNER]: canonicalSchema(owner),
      ...(owner.ownership === "helm" ? {
        "meta.helm.sh/release-name": owner.release, "meta.helm.sh/release-namespace": owner.namespace,
      } : {}),
    },
  };
}

export function verifySchemaOwner(object: ObjectMap, owner: SchemaOwner): void {
  schemaIdentity(object);
  verifyOwnerMetadata(object, owner);
}

/** Kubernetes dry-run CREATE has an ephemeral UID but no storage revision.
 * Never use this check for reads, updates, publication or a real create result. */
export function verifyNewSchemaPreviewOwner(object: ObjectMap, owner: SchemaOwner): void {
  normalizedCrd(object);
  const metadata = object.metadata;
  if (typeof metadata.uid !== "string" || !metadata.uid || metadata.namespace || metadata.deletionTimestamp
    || (metadata.resourceVersion !== undefined && metadata.resourceVersion !== "")) {
    throw new Error("New CRD preview must have an ephemeral UID and no persisted resourceVersion");
  }
  verifyOwnerMetadata(object, owner);
}

function verifyOwnerMetadata(object: ObjectMap, owner: SchemaOwner): void {
  const annotations = object.metadata.annotations ?? {};
  const manager = object.metadata.labels?.["app.kubernetes.io/managed-by"];
  const helm = owner.ownership === "helm" && manager === "Helm"
    && annotations["meta.helm.sh/release-name"] === owner.release
    && annotations["meta.helm.sh/release-namespace"] === owner.namespace;
  const template = owner.ownership === "template" && manager === "kars-schema-stage"
    && annotations[SCHEMA_OWNER] === canonicalSchema(owner)
    && annotations["meta.helm.sh/release-name"] === undefined && annotations["meta.helm.sh/release-namespace"] === undefined;
  if ((!helm && !template) || object.metadata.ownerReferences?.length
    || (annotations[SCHEMA_OWNER] !== undefined && annotations[SCHEMA_OWNER] !== canonicalSchema(owner))) {
    throw new Error(`Foreign or unproven CRD ownership: ${object.metadata.name}; no adoption is permitted`);
  }
}

export async function readSchemaObject(execute: SchemaExecute, kind: string, name: string): Promise<ObjectMap | undefined> {
  const { stdout } = await execute("kubectl", ["get", kind, name, "--ignore-not-found", "-o", "json", "--request-timeout=20s"],
    { stdio: "pipe", timeout: 25_000 });
  if (!stdout.trim()) return undefined;
  const object: ObjectMap = JSON.parse(stdout);
  schemaIdentity(object);
  if (object.metadata.name !== name) throw new Error("Schema read returned a different object");
  return object;
}
