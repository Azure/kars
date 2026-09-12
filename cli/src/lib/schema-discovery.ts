// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { canonicalSchema, type ObjectMap, type SchemaExecute } from "./schema-documents.js";

export interface PublishedType {
  group: string; version: string; kind: string; plural: string; namespaced: boolean; schema: ObjectMap;
}
export interface SchemaWait {
  timeoutMs?: number;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

function refOf(schema: ObjectMap): string | undefined {
  if (typeof schema.$ref === "string") return schema.$ref;
  for (const item of schema.allOf ?? []) {
    const ref = refOf(item);
    if (ref) return ref;
  }
  return undefined;
}

function resolve(schema: ObjectMap, definitions: ObjectMap, visited = new Set<string>(), depth = 0): ObjectMap | undefined {
  if (depth > 128) throw new Error("OpenAPI schema reference depth exceeds the supported bound");
  if (!schema || typeof schema !== "object" || Array.isArray(schema)) throw new Error("OpenAPI schema is malformed");
  const reference = refOf(schema);
  let value = schema;
  if (reference) {
    if (!reference.startsWith("#/components/schemas/")) throw new Error("OpenAPI contains an unsupported external schema reference");
    if (visited.has(reference)) return { type: "object" }; // KCM PopulateRefs uses the same cycle boundary.
    const name = reference.slice("#/components/schemas/".length);
    if (!Object.hasOwn(definitions, name)) return undefined;
    value = definitions[name];
    visited = new Set([...visited, reference]);
  }
  const result = { ...value };
  if (value.properties) {
    result.properties = Object.create(null);
    for (const [key, property] of Object.entries(value.properties)) {
      const child = resolve(property as ObjectMap, definitions, visited, depth + 1);
      if (!child) return undefined;
      result.properties[key] = child;
    }
  }
  for (const key of ["items", "additionalProperties"]) {
    if (value[key] && typeof value[key] === "object" && !Array.isArray(value[key])) {
      const child = resolve(value[key], definitions, visited, depth + 1);
      if (!child) return undefined;
      result[key] = child;
    }
  }
  return result;
}

// CRD publication enriches TypeMeta/ObjectMeta and unfolds int-or-string.
// Compare the declared CEL type surface, not descriptions or server enrichment.
function typeShape(schema: ObjectMap, root = false): ObjectMap {
  const output: ObjectMap = {};
  for (const key of ["type", "format", "nullable", "x-kubernetes-preserve-unknown-fields", "x-kubernetes-int-or-string",
    "x-kubernetes-embedded-resource", "x-kubernetes-list-type", "x-kubernetes-list-map-keys", "x-kubernetes-map-type",
    "maxLength", "maxItems", "maxProperties"]) {
    if (schema[key] !== undefined && schema[key] !== false) output[key] = schema[key];
  }
  if (schema.required?.length) output.required = [...schema.required].sort();
  if (schema.properties) {
    output.properties = Object.create(null);
    for (const [key, property] of Object.entries(schema.properties)) {
      if ((root || schema["x-kubernetes-embedded-resource"]) && ["apiVersion", "kind", "metadata"].includes(key)) continue;
      output.properties[key] = typeShape(property as ObjectMap);
    }
  }
  for (const key of ["items", "additionalProperties"]) {
    if (schema[key] !== undefined) output[key] = typeof schema[key] === "object" ? typeShape(schema[key]) : schema[key];
  }
  return output;
}

export function resolvesPublishedType(document: ObjectMap, expected: PublishedType): boolean {
  const definitions = document.components?.schemas;
  if (!definitions || typeof definitions !== "object" || Array.isArray(definitions)) throw new Error("OpenAPI v3 components are malformed");
  const matches = Object.entries(definitions).filter(([, value]: [string, any]) => value["x-kubernetes-group-version-kind"]?.some(
    (gvk: ObjectMap) => gvk.group === expected.group && gvk.version === expected.version && gvk.kind === expected.kind,
  )) as [string, ObjectMap][];
  if (matches.length > 1) throw new Error(`Ambiguous OpenAPI definition for ${expected.kind}`);
  if (!matches.length) return false;
  const resolved = resolve(matches[0][1], definitions, new Set([`#/components/schemas/${matches[0][0]}`]));
  return !!resolved && canonicalSchema(typeShape(resolved, true)) === canonicalSchema(typeShape(expected.schema, true));
}

async function raw(execute: SchemaExecute, path: string): Promise<ObjectMap> {
  const { stdout } = await execute("kubectl", ["get", "--raw", path, "--request-timeout=20s"],
    { stdio: "pipe", timeout: 25_000 });
  const object = JSON.parse(stdout);
  if (!object || typeof object !== "object" || Array.isArray(object)) throw new Error(`Invalid discovery response at ${path}`);
  return object;
}

async function publishedRoute(execute: SchemaExecute, path: string): Promise<ObjectMap | undefined> {
  try { return await raw(execute, path); } catch (error) {
    if (error && typeof error === "object" && "stderr" in error && typeof error.stderr === "string"
      && error.stderr.startsWith("Error from server (NotFound):")) return undefined;
    throw error;
  }
}

export async function waitForPublishedSchemas(
  execute: SchemaExecute, expected: PublishedType[], checkCrds: () => Promise<boolean>, options: SchemaWait = {},
): Promise<void> {
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)));
  const timeout = options.timeoutMs ?? 120_000;
  if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 600_000) throw new Error("Schema readiness timeout must be between 1 and 600000 ms");
  const deadline = now() + timeout;
  let detail = "CRD establishment";
  for (;;) {
    let ready = await checkCrds();
    if (ready) {
      const index = await raw(execute, "/openapi/v3");
      if (!index.paths || typeof index.paths !== "object") throw new Error("OpenAPI v3 discovery index is malformed");
      const links = new Map<string, string>();
      const groups = new Set(expected.map(type => `apis/${type.group}/${type.version}`));
      for (const group of groups) {
        detail = `OpenAPI/discovery publication for ${group}`;
        const link = index.paths[group]?.serverRelativeURL;
        if (link === undefined) { ready = false; continue; }
        if (typeof link !== "string") throw new Error("OpenAPI schema URL is malformed");
        const url = new URL(link, "https://schema.invalid");
        if (url.origin !== "https://schema.invalid" || url.pathname !== `/openapi/v3/${group}`
          || !link.startsWith(`/openapi/v3/${group}?`) || !url.searchParams.get("hash")
          || [...url.searchParams.keys()].some(key => key !== "hash") || url.searchParams.getAll("hash").length !== 1) {
          throw new Error("Discovery returned an untrusted or unhashed OpenAPI schema URL");
        }
        links.set(group, link);
        const schema = await publishedRoute(execute, link);
        const discovery = await publishedRoute(execute, `/${group}`);
        if (!schema || !discovery) { ready = false; continue; }
        if (!Array.isArray(discovery.resources) || discovery.groupVersion !== group.slice(5)) throw new Error("Resource discovery is malformed");
        for (const type of expected.filter(type => group === `apis/${type.group}/${type.version}`)) {
          if (!discovery.resources.some((resource: ObjectMap) => resource.name === type.plural
            && resource.kind === type.kind && resource.namespaced === type.namespaced) || !resolvesPublishedType(schema, type)) {
            ready = false;
            detail = `resolvable current OpenAPI schema for ${type.kind}`;
          }
        }
      }
      if (ready) {
        const fresh = await raw(execute, "/openapi/v3");
        ready = [...links].every(([group, link]) => fresh.paths?.[group]?.serverRelativeURL === link) && await checkCrds();
        detail = "stable OpenAPI hash and CRD identity";
      }
    }
    if (ready) return;
    if (now() >= deadline) throw new Error(`Timed out waiting for ${detail}; no admission policies were installed`);
    await sleep(Math.min(500, Math.max(1, deadline - now())));
  }
}
