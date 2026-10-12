// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  canonicalSchema, normalizedCrdForGroup, readSchemaObject, schemaIdentity,
  type ObjectMap, type SchemaExecute,
} from "./schema-documents.js";
import { waitForPublishedSchemas, type SchemaWait } from "./schema-discovery.js";

const names = new Set([
  "alertmanagerconfigs", "alertmanagers", "podmonitors", "probes", "prometheusagents",
  "prometheuses", "prometheusrules", "scrapeconfigs", "servicemonitors", "thanosrulers",
].map(name => `${name}.monitoring.coreos.com`));

export function isMonitoringCrd(object: ObjectMap): boolean {
  return object.kind === "CustomResourceDefinition" && object.spec?.group === "monitoring.coreos.com"
    && names.has(object.metadata?.name);
}

/** Dependency CRDs have their own lifecycle; never adopt them as Kars schemas. */
export function coreSchemaDocuments(documents: ObjectMap[]): ObjectMap[] {
  return documents.filter(object => !isMonitoringCrd(object));
}

function spec(object: ObjectMap) {
  return normalizedCrdForGroup(object, "monitoring.coreos.com");
}

export function monitoringSchemaFingerprint(documents: ObjectMap[]): string {
  return canonicalSchema(documents.filter(isMonitoringCrd)
    .map(object => ({ name: object.metadata.name, spec: spec(object) }))
    .sort((a, b) => a.name.localeCompare(b.name)));
}

export async function planMonitoringSchemas(
  execute: SchemaExecute,
  documents: ObjectMap[],
  options: SchemaWait & { checkOnly?: boolean; beforeWrite?: () => Promise<void> } = {},
): Promise<() => Promise<void>> {
  const desired = structuredClone(documents.filter(isMonitoringCrd));
  if (new Set(desired.map(object => object.metadata.name)).size !== desired.length) {
    throw new Error("Duplicate monitoring dependency CRDs");
  }
  const identities = new Map<string, string>();
  for (const object of desired) {
    spec(object);
    if (object.metadata.namespace || object.metadata.uid || object.metadata.resourceVersion
      || object.metadata.deletionTimestamp || object.metadata.ownerReferences?.length) {
      throw new Error("Monitoring dependency definitions must not carry live object identities");
    }
    const current = await readSchemaObject(execute, "customresourcedefinition", object.metadata.name);
    if (current) {
      if (canonicalSchema(spec(current)) !== canonicalSchema(spec(object))) {
        throw new Error(`Monitoring CRD ${object.metadata.name} differs; review and upgrade that dependency explicitly. No adoption or overwrite is permitted.`);
      }
      identities.set(object.metadata.name, schemaIdentity(current).uid);
    } else {
      if (options.checkOnly) throw new Error(`Monitoring dependency ${object.metadata.name} is not installed`);
      const preview: ObjectMap = JSON.parse((await execute("kubectl",
        ["create", "-f", "-", "--dry-run=server", "-o", "json", "--request-timeout=20s"],
        { stdio: "pipe", input: JSON.stringify(object), timeout: 25_000 })).stdout);
      if (typeof preview.metadata?.uid !== "string" || !preview.metadata.uid
        || preview.metadata.namespace || preview.metadata.deletionTimestamp || preview.metadata.resourceVersion
        || canonicalSchema(spec(preview)) !== canonicalSchema(spec(object))) {
        throw new Error("Monitoring CRD server preview differs from its reviewed definition");
      }
    }
  }
  return async () => {
    if (!desired.length) return;
    await options.beforeWrite?.();
    // Recheck the complete dependency set before creating any definition.
    for (const object of desired) {
      const current = await readSchemaObject(execute, "customresourcedefinition", object.metadata.name);
      if ((current ? schemaIdentity(current).uid : undefined) !== identities.get(object.metadata.name)
        || current && canonicalSchema(spec(current)) !== canonicalSchema(spec(object))) {
        throw new Error("Monitoring dependency changed after review");
      }
    }
    for (const object of desired.filter(item => !identities.has(item.metadata.name))) {
      const created: ObjectMap = JSON.parse((await execute("kubectl", ["create", "-f", "-", "-o", "json", "--request-timeout=20s"],
        { stdio: "pipe", input: JSON.stringify(object), timeout: 25_000 })).stdout);
      if (canonicalSchema(spec(created)) !== canonicalSchema(spec(object))) throw new Error("Created monitoring schema differs from its preview");
      identities.set(object.metadata.name, schemaIdentity(created).uid);
    }
    const types = desired.flatMap(object => object.spec.versions.filter((version: ObjectMap) => version.served)
      .map((version: ObjectMap) => ({
        group: object.spec.group, version: version.name, kind: object.spec.names.kind,
        plural: object.spec.names.plural, namespaced: object.spec.scope === "Namespaced",
        schema: version.schema.openAPIV3Schema,
      })));
    await waitForPublishedSchemas(execute, types, async () => {
      let established = true;
      for (const object of desired) {
        const current = await readSchemaObject(execute, "customresourcedefinition", object.metadata.name);
        if (!current || schemaIdentity(current).uid !== identities.get(object.metadata.name)
          || canonicalSchema(spec(current)) !== canonicalSchema(spec(object))) throw new Error("Monitoring dependency changed during publication");
        established &&= current.status?.conditions?.some((condition: ObjectMap) =>
          condition.type === "Established" && condition.status === "True") === true;
      }
      return established;
    }, options);
  };
}
