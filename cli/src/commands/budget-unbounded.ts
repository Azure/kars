// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { bridgeReactivationDigest, fundingView } from "./budget-topup.js";

type Value = Record<string, any>;
type Execute = (args: string[], input?: string) => Promise<string>;
const key = "unbounded-token-allowances.json";

function object(value: unknown): Value {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a budget object");
  return value as Value;
}
function exact(value: unknown, fields: string[]): Value {
  const result = object(value);
  if (!isDeepStrictEqual(Object.keys(result).sort(), [...fields].sort())) throw new Error("Expected exactly the documented unbounded authorization fields");
  return result;
}
function uid(value: unknown): string {
  if (typeof value !== "string" || !/^[a-zA-Z0-9-]{1,128}$/.test(value)) throw new Error("Invalid authorization UID or ID");
  return value;
}
function name(value: unknown): string {
  if (typeof value !== "string" || !/^[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?$/.test(value)) throw new Error("Invalid Kubernetes name");
  return value;
}
function authorization(value: unknown): Value {
  const grant = exact(value, ["id", "configUid", "accountUid", "root", "originalLimits", "ledgerDigest", "previousTokenTopUpIds"]);
  uid(grant.id); uid(grant.configUid); uid(grant.accountUid);
  const root = exact(grant.root, ["kind", "resource", "workspaceUid", "clusterUid"]);
  const resource = exact(root.resource, ["name", "namespace", "uid"]);
  uid(root.clusterUid); uid(root.workspaceUid); uid(resource.uid); name(resource.name); name(resource.namespace);
  if (!["KarsTask", "KarsTeam"].includes(root.kind)
    && !(root.kind === "KarsSandbox" && ["sre", "bridge-orchestrator"].includes(resource.name))) {
    throw new Error("Unbounded tokens support governed Tasks, Teams and built-in SRE or Bridge only");
  }
  const limits = object(grant.originalLimits);
  if (Object.entries(limits).some(([field, amount]) =>
    !["tokens", "usdMicros"].includes(field) || !Number.isSafeInteger(amount) || amount <= 0)) {
    throw new Error("Original limits must retain exact positive configured caps");
  }
  if (typeof grant.ledgerDigest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(grant.ledgerDigest)
    || !Array.isArray(grant.previousTokenTopUpIds) || grant.previousTokenTopUpIds.length > 64
    || grant.previousTokenTopUpIds.some((id: unknown, index: number) =>
      uid(id) === grant.id || (index > 0 && grant.previousTokenTopUpIds[index - 1] >= String(id)))) {
    throw new Error("Exact ledger digest and sorted unique prior receipt IDs are required");
  }
  return grant;
}
function metadata(value: Value, expected: Value): void {
  const meta = object(value.metadata);
  if (Object.entries(expected).some(([field, pin]) => meta[field] !== pin)
    || !meta.resourceVersion || meta.deletionTimestamp) throw new Error("Live object identity was replaced, deleted or is incomplete");
}
function list(data: Value, field: string): Value[] {
  const entries: unknown = JSON.parse(data[field] ?? "[]");
  if (!Array.isArray(entries)) throw new Error("Malformed operator authorization list");
  return entries.map(object);
}

async function validateSource(execute: Execute, grant: Value, ledger: Value, namespace: string, data: Value): Promise<void> {
  const root = grant.root;
  const resource = root.resource;
  for (const [namespaceName, expectedUid] of [["kube-system", root.clusterUid], [resource.namespace, root.workspaceUid]]) {
    const current = object(JSON.parse(await execute(["get", "namespace", namespaceName, "-o", "json"])));
    metadata(current, { name: namespaceName, uid: expectedUid });
  }
  const source = object(JSON.parse(await execute(["get", root.kind.toLowerCase(), resource.name, "-n", resource.namespace, "-o", "json"])));
  metadata(source, resource);
  const spec = object(source.spec);
  const reference = { name: `inference-budget-${resource.uid}`, namespace, uid: grant.accountUid };
  if (root.kind !== "KarsSandbox") {
    const status = object(source.status);
    const budget = object(object(spec.envelope).budget);
    const limits: Value = {};
    for (const field of ["tokens", "usdMicros"]) {
      const value = budget[field] ?? 0;
      if (!Number.isSafeInteger(value) || value < 0) throw new Error("Invalid live root budget");
      if (value > 0) limits[field] = value;
    }
    if (budget.scope !== "GovernedInference" || !isDeepStrictEqual(limits, grant.originalLimits)
      || ["Cancelled", "Failed", "Completed", "Rejected", "Expired"].includes(status.phase)) {
      throw new Error("Root funding was changed or root authority is no longer active");
    }
    if (root.kind === "KarsTeam") {
      if (!isDeepStrictEqual(status.inferenceBudgetAccount, reference)) throw new Error("Team account binding was replaced or revoked");
      return;
    }
  }
  const binding = object(root.kind === "KarsSandbox" ? spec.inferenceBudgetRef : source.status.inferenceBudget);
  const node = object(object(ledger.nodes)[resource.uid]);
  const authority = object(node.authority);
  if (!isDeepStrictEqual(binding.account, reference) || !isDeepStrictEqual(binding.root, root)
    || binding.scope !== "GovernedInference" || binding.taskUid !== resource.uid
    || binding.rootTaskUid !== resource.uid || binding.parentTaskUid != null
    || authority.parentUid != null || authority.rootTaskUid !== resource.uid
    || !isDeepStrictEqual(authority.task, resource) || !isDeepStrictEqual(authority.limits, grant.originalLimits)
    || typeof authority.authorizationDigest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(authority.authorizationDigest)
    || binding.authorizationDigest !== authority.authorizationDigest || node.active !== (ledger.phase === "Active")) {
    throw new Error("Source no longer binds the exact account and original root authority");
  }
  if (root.kind !== "KarsSandbox") return;
  const original = object(authority.effectiveAuthorization);
  if (!isDeepStrictEqual(original.root, root) || !isDeepStrictEqual(original.limits, grant.originalLimits)
    || (original.limits.tokens === undefined && (original.unboundedTokens !== true || original.expiresAt !== undefined))
    || bridgeReactivationDigest(original) !== authority.authorizationDigest) {
    throw new Error("Original built-in authority differs from the sealed account");
  }
  if (resource.name === "bridge-orchestrator") {
    const originals = list(data, "built-in-funding.json").filter(value => value.root?.resource?.name === resource.name);
    if (source.metadata.ownerReferences?.length || source.metadata.annotations?.["kars.azure.com/sre-first-install-intent"] !== undefined
      || originals.length !== 1 || !isDeepStrictEqual(originals[0], original)) {
      throw new Error("Original Bridge funding was revoked, replaced or modified");
    }
    return;
  }
  const registration = object(JSON.parse(await execute(["get", "karssreregistration", "canonical", "-o", "json"])));
  const regSpec = object(registration.spec);
  const sealed = object(regSpec.firstInstallFunding);
  const intentPin = object(sealed.intent);
  metadata(registration, { name: "canonical" }); uid(registration.metadata.uid); uid(intentPin.uid);
  if (regSpec.enabled !== true || !isDeepStrictEqual(regSpec.sandbox, resource)
    || !isDeepStrictEqual(sealed.grant, original)
    || source.metadata.annotations?.["kars.azure.com/sre-first-install-intent"] !== intentPin.name) {
    throw new Error("Original SRE registration or sealed funding was revoked or replaced");
  }
  const intent = object(JSON.parse(await execute(["get", "configmap", name(intentPin.name), "-n", resource.namespace, "-o", "json"])));
  metadata(intent, { name: intentPin.name, namespace: resource.namespace, uid: intentPin.uid });
  if (intent.immutable !== true || typeof intent.data?.["intent.json"] !== "string"
    || createHash("sha256").update(intent.data["intent.json"]).digest("hex") !== intentPin.digest) {
    throw new Error("Immutable SRE first-install intent was replaced or changed");
  }
}

/** Publish explicit operator intent; only the controller may consume it into the ledger. */
export async function authorizeUnbounded(
  execute: Execute, text: string, namespace: string, catalog: string, now = Date.now(),
): Promise<Value> {
  name(namespace); name(catalog);
  const grant = authorization(JSON.parse(text));
  const resource = grant.root.resource;
  const accountName = name(`inference-budget-${resource.uid}`);
  const account = object(JSON.parse(await execute(["get", "karsbudgetaccount", accountName, "-n", namespace, "-o", "json"])));
  metadata(account, { name: accountName, namespace, uid: grant.accountUid });
  const spec = object(account.spec);
  const ledger = object(object(account.status).ledger);
  const consumed = ledger.unboundedTokens != null;
  if (account.metadata.annotations?.["kars.azure.com/inference-budget-bootstrap"] !== "sealed"
    || spec.scope !== "GovernedInference" || ledger.scope !== spec.scope || ledger.version !== "governed-inference/v1"
    || ledger.accountUid !== grant.accountUid || !isDeepStrictEqual(spec.root, grant.root)
    || !isDeepStrictEqual(ledger.root, grant.root) || !isDeepStrictEqual(spec.limits, grant.originalLimits)
    || !isDeepStrictEqual(ledger.limits, grant.originalLimits)) throw new Error("Account differs from sealed original funding");
  if (consumed && !isDeepStrictEqual(ledger.unboundedTokens, grant)) throw new Error("Consumed unbounded receipt has different immutable content");
  const receipts = object(ledger.tokenTopUps ?? {});
  if (!isDeepStrictEqual(grant.previousTokenTopUpIds, Object.keys(receipts).sort())
    || (!consumed && bridgeReactivationDigest(ledger) !== grant.ledgerDigest)) {
    throw new Error("Stale ledger digest or prior top-up receipt pins");
  }
  if (ledger.phase !== "Active") {
    const rootNode = object(object(ledger.nodes)[resource.uid]);
    const expiry = Date.parse(fundingView(ledger).effectiveExpiresAt);
    if (consumed || grant.root.kind !== "KarsSandbox" || resource.name !== "bridge-orchestrator"
      || ledger.phase !== "Closed" || !Number.isFinite(expiry) || expiry > now
      || !isDeepStrictEqual(Object.keys(ledger.nodes), [resource.uid]) || rootNode.active !== false
      || !isDeepStrictEqual(object(ledger.meters).reserved, { tokens: 0, usdMicros: 0 })
      || Object.values(object(ledger.sessions)).some(session => session.closed !== true)
      || Object.values(object(ledger.attempts)).some(attempt =>
        !["Expired", "Settled", "Uncertain"].includes(attempt.phase) || attempt.observedBreach != null)) {
      throw new Error("Only Active accounts or unchanged expired Closed Bridge accounts with closed sessions and no pending/breached attempts qualify");
    }
  }
  const map = object(JSON.parse(await execute(["get", "configmap", catalog, "-n", namespace, "-o", "json"])));
  metadata(map, { name: catalog, namespace, uid: grant.configUid });
  if (map.metadata.annotations?.["kars.azure.com/inference-budget-contracts"] !== "v1" || map.immutable === true) {
    throw new Error("Operator ConfigMap is not an active writable contracts source");
  }
  const data = object(map.data);
  const prior = list(data, "token-top-ups.json");
  if (Object.entries(receipts).some(([id, receipt]) => {
    const entries = prior.filter(value => value.id === id);
    return entries.length !== 1 || !isDeepStrictEqual(entries[0], receipt);
  })) throw new Error("Original top-up authorizations were removed or changed");
  await validateSource(execute, grant, ledger, namespace, data);
  fundingView({ ...ledger, unboundedTokens: grant });
  const configured = list(data, key).map(authorization);
  const ids = new Set<string>();
  const accounts = new Set<string>();
  for (const entry of configured) {
    if (entry.configUid !== grant.configUid || ids.has(entry.id) || accounts.has(entry.accountUid)) {
      throw new Error("Duplicate or replaced operator unbounded authorization");
    }
    ids.add(entry.id); accounts.add(entry.accountUid);
  }
  const existing = configured.find(value => value.id === grant.id || value.accountUid === grant.accountUid);
  if (existing && !isDeepStrictEqual(existing, grant)) throw new Error("Account already has a different immutable allowance");
  if (consumed && !existing) throw new Error("Consumed authorization was removed from the catalog; refusing to restore revoked authority");
  // Admission enforces fund as well; these checks make missing operator rights explicit before publication.
  for (const [verb, target] of [["fund", "karsbudgetaccounts.kars.azure.com"], ["patch", `configmap/${catalog}`]]) {
    if ((await execute(["auth", "can-i", verb, target, "-n", namespace])).trim() !== "yes") {
      throw new Error(`Operator ${verb} permission is required`);
    }
  }
  if (existing) return { id: grant.id, published: true, applied: consumed };
  const encoded = JSON.stringify([...configured, grant]);
  if (Buffer.byteLength(encoded) > 262_144) throw new Error("Operator unbounded configuration capacity exceeded");
  await execute(["patch", "configmap", catalog, "-n", namespace, "--type=json", "-p", JSON.stringify([
    { op: "test", path: "/metadata/uid", value: grant.configUid },
    { op: "test", path: "/metadata/resourceVersion", value: map.metadata.resourceVersion },
    { op: "add", path: `/data/${key}`, value: encoded },
  ]), "-o", "name"]);
  return { id: grant.id, published: true, applied: false,
    next: "Wait for the controller's UID/CAS receipt. Tokens have no ceiling or expiry; configured currency caps and live authorization remain enforced. Retain this entry in Helm values." };
}
