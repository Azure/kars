// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { namespaceClaimed } from "./namespace-ownership.js";
import { get, type Execute } from "./sre-authority.js";

const CATALOG = "kars-inference-budget-contracts";
const FUNDING = "built-in-funding.json";

export interface SreFunding { tokens: number; expiresAt: string }
interface CreatedSource { uid: string; namespaceUid: string }

export class SreFundingError extends Error {
  constructor(message: string) { super(message); this.name = "SreFundingError"; }
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new SreFundingError("SRE lifetime funding requires an object, not missing or malformed configuration");
  }
  return value as Record<string, unknown>;
}

export function parseSreFunding(tokens?: string, expiry?: string): SreFunding | undefined {
  if (tokens === undefined && expiry === undefined) return undefined;
  if (!tokens || !/^[1-9][0-9]*$/.test(tokens) || !Number.isSafeInteger(Number(tokens))) {
    throw new SreFundingError("SRE --lifetime-tokens must be a positive safe integer with --funding-expires-at");
  }
  const date = expiry && new Date(expiry);
  if (!expiry || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{3})?Z$/.test(expiry)
    || !date || !Number.isFinite(date.getTime()) || date.getTime() <= Date.now()
    || date.toISOString().replace(".000Z", "Z") !== expiry.replace(".000Z", "Z")) {
    throw new SreFundingError("SRE --funding-expires-at must be a future UTC timestamp (YYYY-MM-DDTHH:mm:ssZ)");
  }
  return { tokens: Number(tokens), expiresAt: expiry };
}

export function parseFirstInstallFunding(value: unknown): SreFunding | { unboundedTokens: true } {
  const funding = object(value);
  if (Object.keys(funding).join() === "unboundedTokens" && funding.unboundedTokens === true) {
    return { unboundedTokens: true };
  }
  if (Object.keys(funding).sort().join() !== "expiresAt,tokens" || typeof funding.tokens !== "number") {
    throw new SreFundingError("First-install funding requires exactly unboundedTokens: true or finite tokens and expiresAt");
  }
  return parseSreFunding(String(funding.tokens), funding.expiresAt as string)!;
}

function grants(value: unknown): unknown[] {
  if (!Array.isArray(value) || Buffer.byteLength(JSON.stringify(value)) > 32768) {
    throw new SreFundingError("SRE built-in funding declarations are malformed or oversized");
  }
  const names = new Set<string>();
  for (const item of value) {
    const grant = object(item);
    const root = object(grant.root);
    const resource = object(root.resource);
    const limits = object(grant.limits);
    if (Object.keys(grant).some(key => !["root", "limits", "expiresAt"].includes(key))
      || Object.keys(root).some(key => !["kind", "resource", "workspaceUid", "clusterUid"].includes(key))
      || Object.keys(resource).some(key => !["namespace", "name", "uid"].includes(key))
      || Object.keys(limits).some(key => !["tokens", "usdMicros"].includes(key))
      || root.kind !== "KarsSandbox"
      || ![root.workspaceUid, root.clusterUid, resource.uid].every(value => typeof value === "string" && /^[A-Za-z0-9-]{1,128}$/.test(value))
      || typeof resource.namespace !== "string" || resource.namespace.length > 63
      || !/^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(resource.namespace)
      || typeof resource.name !== "string" || !["sre", "bridge-orchestrator"].includes(resource.name)
      || names.has(resource.name)
      || typeof limits.tokens !== "number" || !Number.isSafeInteger(limits.tokens) || limits.tokens <= 0
      || (limits.usdMicros !== undefined && limits.usdMicros !== null
        && (typeof limits.usdMicros !== "number" || !Number.isSafeInteger(limits.usdMicros) || limits.usdMicros < 0))
      || typeof grant.expiresAt !== "string" || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/.test(grant.expiresAt)
      || !Number.isFinite(Date.parse(grant.expiresAt))) {
      throw new SreFundingError("Existing built-in funding declaration is invalid; preserve it for explicit review");
    }
    names.add(resource.name);
  }
  return value;
}

/** The source remains unenrolled until Helm has persisted its exact grant.
 * Failed publication leaves that source blocked, not calendar-only. */
export async function planSreFunding(
  execute: Execute, namespace: string, release: string, values: unknown, funding: SreFunding,
) {
  const budget = object(object(values).inferenceBudget);
  if (budget.enabled !== true) throw new SreFundingError("Configure the Core inference-budget broker before funding fresh SRE");
  const previous = grants(budget.builtInFunding ?? []);
  if (previous.some(grant => object(object(object(grant).root).resource).name === "sre")) {
    throw new SreFundingError("SRE funding already exists; installation cannot replace an identity, grant or history");
  }
  const readCatalog = async () => {
    const map = await get(execute, "configmap", CATALOG, namespace);
    if (!map || map.kind !== "ConfigMap" || map.metadata.name !== CATALOG
      || map.metadata.namespace !== namespace || map.metadata.deletionTimestamp
      || map.metadata.annotations?.["meta.helm.sh/release-name"] !== release
      || map.metadata.annotations?.["meta.helm.sh/release-namespace"] !== namespace
      || map.metadata.annotations?.["kars.azure.com/inference-budget-contracts"] !== "v1") {
      throw new SreFundingError("SRE funding requires the exact live Core-owned broker catalog");
    }
    const encoded = object(map.data)[FUNDING];
    if (typeof encoded !== "string" || Buffer.byteLength(encoded) > 32768) {
      throw new SreFundingError("Live SRE funding catalog is missing or oversized");
    }
    return { map, declarations: grants(JSON.parse(encoded)) };
  };
  const initial = await readCatalog();
  if (!isDeepStrictEqual(initial.declarations, previous)) {
    throw new SreFundingError("Live funding differs from saved Helm values; reconcile it explicitly before installation");
  }
  const identity = async (name: string) => {
    const value = await get(execute, "namespace", name);
    if (!value || value.metadata.name !== name || value.metadata.deletionTimestamp) {
      throw new SreFundingError("SRE funding namespace identity is unavailable");
    }
    return value.metadata.uid!;
  };
  const workspaceUid = await identity(namespace);
  const clusterUid = await identity("kube-system");
  let expected: unknown[] | undefined;

  const checkSource = async (created: CreatedSource) => {
    if (Date.parse(funding.expiresAt) <= Date.now()
      || await identity(namespace) !== workspaceUid || await identity("kube-system") !== clusterUid) {
      throw new SreFundingError("SRE funding expired or its cluster/workspace identity changed");
    }
    const source = await get(execute, "karssandbox", "sre", namespace);
    const runtime = await get(execute, "namespace", "kars-sre");
    if (!source || !runtime || source.metadata.uid !== created.uid
      || source.metadata.name !== "sre" || source.metadata.namespace !== namespace
      || source.metadata.deletionTimestamp || runtime.metadata.deletionTimestamp
      || runtime.metadata.uid !== created.namespaceUid || !namespaceClaimed(runtime, source)
      || source.metadata.annotations?.["kars.azure.com/namespace-uid"] !== created.namespaceUid
      || source.spec?.runtime?.kind !== "Hermes" || source.spec?.inferenceBudgetRef
      || runtime.metadata.annotations?.["kars.azure.com/calendar-initialization-uid"] !== created.uid
      || runtime.metadata.annotations?.["kars.azure.com/calendar-account-uid"]
      || await get(execute, "deployment", "sre", "kars-sre")) {
      throw new SreFundingError("SRE lifetime funding requires the just-created Hermes source and fresh, unused namespace; legacy migration is not supported");
    }
  };
  return {
    async flags(created: CreatedSource): Promise<string[]> {
      await checkSource(created);
      const current = await readCatalog();
      const saved = JSON.parse((await execute("helm", ["get", "values", release,
        "--namespace", namespace, "-o", "json"], { stdio: "pipe" })).stdout);
      if (current.map.metadata.uid !== initial.map.metadata.uid
        || current.map.metadata.resourceVersion !== initial.map.metadata.resourceVersion
        || !isDeepStrictEqual(current.map.data, initial.map.data) || !isDeepStrictEqual(saved, values)) {
        throw new SreFundingError("Core funding or release values changed during SRE staging; no enrollment was authorized");
      }
      expected = grants([...previous, {
        root: { kind: "KarsSandbox", resource: { namespace, name: "sre", uid: created.uid }, workspaceUid, clusterUid },
        limits: { tokens: funding.tokens }, expiresAt: funding.expiresAt,
      }]);
      return ["--set-json", `inferenceBudget.builtInFunding=${JSON.stringify(expected)}`];
    },
    async verify(created: CreatedSource): Promise<void> {
      await checkSource(created);
      const current = await readCatalog();
      const saved = object(JSON.parse((await execute("helm", ["get", "values", release,
        "--namespace", namespace, "-o", "json"], { stdio: "pipe" })).stdout));
      if (!expected || current.map.metadata.uid !== initial.map.metadata.uid
        || !isDeepStrictEqual(current.declarations, expected)
        || !isDeepStrictEqual(object(saved.inferenceBudget).builtInFunding, expected)) {
        throw new SreFundingError("SRE funding was not persisted in both Core values and the live catalog; source remains unenrolled");
      }
    },
  };
}
