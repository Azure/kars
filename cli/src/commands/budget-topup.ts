// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { createHash } from "node:crypto";

type RecordValue = Record<string, any>;
type Execute = (args: string[], input?: string) => Promise<string>;
const key = "token-top-ups.json";

function object(value: unknown): RecordValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a budget object");
  return value as RecordValue;
}
function integer(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error("Top-up tokens must be a positive exact safe integer");
  }
  return value;
}
function uid(value: unknown): string {
  if (typeof value !== "string" || !/^[a-zA-Z0-9-]{1,128}$/.test(value)) throw new Error("Invalid top-up UID or ID");
  return value;
}
function name(value: string): string {
  if (!/^[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?$/.test(value)) throw new Error("Invalid Kubernetes name");
  return value;
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const fields = object(value);
    return `{${Object.keys(fields).sort().map(key => `${JSON.stringify(key)}:${canonical(fields[key])}`).join(",")}}`;
  }
  if (typeof value === "number" && !Number.isSafeInteger(value)) throw new Error("Ledger digest requires exact integers");
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new Error("Ledger digest requires complete JSON");
  return encoded;
}

function digest(value: unknown): string {
  return `sha256:${createHash("sha256").update(canonical(value)).digest("hex")}`;
}

export function bridgeReactivationDigest(ledger: RecordValue): string {
  const value = { ...ledger };
  // Match the shared ledger's skip_serializing_if for an empty receipt map.
  if (!Object.keys(object(value.tokenTopUps ?? {})).length) delete value.tokenTopUps;
  if (value.unboundedTokens == null) delete value.unboundedTokens;
  return digest(value);
}

export function fundingView(ledger: RecordValue): RecordValue {
  const original = object(ledger.limits);
  const receipts = object(ledger.tokenTopUps ?? {});
  let tokens = original.tokens;
  let addedTokens = 0;
  for (const [id, value] of Object.entries(receipts)) {
    const grant = object(value);
    if (id !== grant.id || grant.accountUid !== ledger.accountUid
      || !isDeepStrictEqual(grant.root, ledger.root) || !isDeepStrictEqual(grant.originalLimits, original)) {
      throw new Error("Top-up receipt identity differs from the original account");
    }
    addedTokens = integer(addedTokens + integer(grant.tokens));
    tokens = integer(integer(tokens) + grant.tokens);
  }
  const renewals = Object.values(receipts).filter((grant) => grant.fundingRenewal)
    .map((grant) => ({ id: grant.id, expiresAt: grant.expiresAt, ...grant.fundingRenewal }))
    .sort((a, b) => a.previousTopUpIds.length - b.previousTopUpIds.length);
  const originalExpiry = ledger.root?.kind === "KarsSandbox"
    ? ledger.nodes?.[ledger.root.resource.uid]?.authority?.effectiveAuthorization?.expiresAt : undefined;
  const unbounded = ledger.unboundedTokens == null ? undefined : object(ledger.unboundedTokens);
  if (unbounded && (!isDeepStrictEqual(Object.keys(unbounded).sort(),
    ["id", "configUid", "accountUid", "root", "originalLimits", "ledgerDigest", "previousTokenTopUpIds"].sort())
    || unbounded.accountUid !== ledger.accountUid || !isDeepStrictEqual(unbounded.root, ledger.root)
    || !isDeepStrictEqual(unbounded.originalLimits, original)
    || !isDeepStrictEqual(unbounded.previousTokenTopUpIds, Object.keys(receipts).sort()))) {
    throw new Error("Unbounded token receipt differs from the original account or receipt history");
  }
  const effectiveLimits = { ...original, ...(tokens === undefined ? {} : { tokens }) };
  if (unbounded) delete effectiveLimits.tokens;
  return { originalLimits: original, effectiveLimits,
    addedTokens, tokenTopUps: receipts, fundingRenewals: renewals,
    ledgerDigest: bridgeReactivationDigest(ledger),
    ...(unbounded ? { tokenAllowance: "unbounded", unboundedTokens: unbounded } : {}),
    ...(originalExpiry ? { originalExpiresAt: originalExpiry, effectiveExpiresAt: renewals.at(-1)?.expiresAt ?? originalExpiry } : {}),
    ...(unbounded && ledger.root?.kind === "KarsSandbox" ? { effectiveExpiresAt: null } : {}),
    ...(ledger.phase === "Closed" && ledger.root?.kind === "KarsSandbox" && ledger.root.resource.name === "bridge-orchestrator"
      ? { closedLedgerDigest: bridgeReactivationDigest(ledger) } : {}),
    fundingAuthority: "Recorded ceiling only; original grants, explicit renewals and live operator authority are rechecked before reserve/begin" };
}

async function validateBridgeRenewal(execute: Execute, grant: RecordValue, ledger: RecordValue, receipts: RecordValue, namespace: string, now: number): Promise<void> {
  const renewal = object(grant.fundingRenewal);
  const pin = object(renewal.bridgeReactivation);
  const root = grant.root;
  const ids = renewal.previousTopUpIds;
  if (!isDeepStrictEqual(Object.keys(renewal).sort(), ["bridgeReactivation", "originalExpiresAt", "previousTopUpIds"])
    || !isDeepStrictEqual(Object.keys(pin), ["closedLedgerDigest"])
    || typeof pin.closedLedgerDigest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(pin.closedLedgerDigest)
    || root.kind !== "KarsSandbox" || root.resource.name !== "bridge-orchestrator"
    || !isDeepStrictEqual(Object.keys(grant.nodes), [root.resource.uid])
    || !isDeepStrictEqual(Object.keys(object(ledger.nodes)), [root.resource.uid])
    || !Array.isArray(ids) || ids.length >= 64 || ids.some(id => uid(id) === grant.id)
    || ids.some((id, index) => index > 0 && ids[index - 1] >= id)
    || (!receipts[grant.id] && !isDeepStrictEqual(ids, Object.keys(receipts).sort()))) {
    throw new Error("Bridge reactivation requires explicit exact closed-ledger and prior-receipt pins, not SRE pins");
  }
  const authority = object(ledger.nodes[root.resource.uid].authority);
  const original = object(authority.effectiveAuthorization);
  if (!isDeepStrictEqual(original.root, root) || !isDeepStrictEqual(original.limits, grant.originalLimits)
    || !isDeepStrictEqual(authority.limits, grant.originalLimits)
    || !isDeepStrictEqual(authority.task, root.resource)
    || digest(original) !== authority.authorizationDigest
    || original.expiresAt !== renewal.originalExpiresAt || !Number.isFinite(Date.parse(original.expiresAt))
    || Date.parse(grant.expiresAt) <= Date.parse(original.expiresAt)
    || ids.some(id => !receipts[id] || (receipts[id].fundingRenewal
      && Date.parse(grant.expiresAt) <= Date.parse(receipts[id].expiresAt)))) {
    throw new Error("Bridge renewal must retain original authority and extend every prior renewal");
  }
  if (!receipts[grant.id]) {
    const effectiveExpiry = Date.parse(fundingView(ledger).effectiveExpiresAt);
    if (ledger.phase !== "Closed" || !Number.isFinite(effectiveExpiry) || effectiveExpiry > now
      || ledger.nodes[root.resource.uid].active !== false
      || bridgeReactivationDigest(ledger) !== pin.closedLedgerDigest
      || !isDeepStrictEqual(object(ledger.meters).reserved, { tokens: 0, usdMicros: 0 })
      || Object.values(object(ledger.sessions)).some(session => session.closed !== true)
      || Object.values(object(ledger.attempts)).some(attempt =>
        !["Expired", "Settled", "Uncertain"].includes(attempt.phase) || attempt.observedBreach != null)) {
      throw new Error("Bridge reactivation requires an unchanged expired Closed ledger, closed sessions and no pending or breached attempts");
    }
  }
  const source = object(JSON.parse(await execute(["get", "karssandbox", "bridge-orchestrator", "-n", name(root.resource.namespace), "-o", "json"])));
  const binding = object(object(source.spec).inferenceBudgetRef);
  if (source.metadata?.uid !== root.resource.uid || source.metadata?.name !== root.resource.name
    || source.metadata?.namespace !== root.resource.namespace || !source.metadata?.resourceVersion
    || source.metadata?.deletionTimestamp || source.metadata?.ownerReferences?.length
    || source.metadata?.annotations?.["kars.azure.com/sre-first-install-intent"] !== undefined
    || binding.account?.uid !== grant.accountUid || binding.account?.namespace !== namespace
    || binding.account?.name !== `inference-budget-${root.resource.uid}` || !isDeepStrictEqual(binding.root, root)
    || binding.scope !== "GovernedInference" || binding.taskUid !== root.resource.uid
    || binding.rootTaskUid !== root.resource.uid || binding.parentTaskUid != null
    || binding.authorizationDigest !== grant.nodes[root.resource.uid]) {
    throw new Error("Original Bridge source was deleted, replaced or no longer binds this account");
  }
  for (const [namespaceName, expectedUid] of [["kube-system", root.clusterUid], [root.resource.namespace, root.workspaceUid]]) {
    const current = object(JSON.parse(await execute(["get", "namespace", name(namespaceName), "-o", "json"])));
    if (current.metadata?.uid !== expectedUid || current.metadata?.deletionTimestamp) {
      throw new Error("Bridge cluster or workspace authority was replaced or deleted");
    }
  }
}

async function validateRenewal(execute: Execute, grant: RecordValue, ledger: RecordValue, receipts: RecordValue, namespace: string, now: number): Promise<void> {
  const renewal = object(grant.fundingRenewal);
  if (renewal.bridgeReactivation !== undefined) {
    return validateBridgeRenewal(execute, grant, ledger, receipts, namespace, now);
  }
  const fields = ["registrationUid", "intentUid", "intentDigest", "originalExpiresAt", "previousTopUpIds"];
  if (Object.keys(renewal).length !== fields.length || fields.some((field) => !(field in renewal))) {
    throw new Error("Expected exactly the documented funding renewal pins");
  }
  uid(renewal.registrationUid); uid(renewal.intentUid);
  const root = grant.root;
  const ids = renewal.previousTopUpIds;
  if (root.kind !== "KarsSandbox" || root.resource.name !== "sre"
    || !isDeepStrictEqual(Object.keys(grant.nodes), [root.resource.uid])
    || typeof renewal.intentDigest !== "string" || !/^[a-f0-9]{64}$/.test(renewal.intentDigest)
    || !Array.isArray(ids) || ids.length >= 64 || ids.some((id) => uid(id) === grant.id)
    || ids.some((id, index) => index > 0 && ids[index - 1] >= id)
    || (!receipts[grant.id] && !isDeepStrictEqual(ids, Object.keys(receipts).sort()))) {
    throw new Error("Renewal requires the exact SRE root and all previously applied receipt IDs");
  }
  const original = object(object(ledger.nodes[root.resource.uid].authority).effectiveAuthorization);
  if (original.expiresAt !== renewal.originalExpiresAt || !Number.isFinite(Date.parse(original.expiresAt))
    || Date.parse(grant.expiresAt) <= Date.parse(original.expiresAt)
    || ids.some((id) => !receipts[id] || (receipts[id].fundingRenewal
      && Date.parse(grant.expiresAt) <= Date.parse(receipts[id].expiresAt)))) {
    throw new Error("Renewal must extend the original and every prior renewal expiry");
  }
  const reg = object(JSON.parse(await execute(["get", "karssreregistration", "canonical", "-o", "json"])));
  const bound = object(object(reg.spec).firstInstallFunding);
  if (reg.metadata?.uid !== renewal.registrationUid || reg.metadata?.deletionTimestamp || !reg.spec.enabled
    || bound.intent?.uid !== renewal.intentUid || bound.intent?.digest !== renewal.intentDigest
    || !isDeepStrictEqual(bound.grant, original) || !isDeepStrictEqual(original.root, root)
    || !isDeepStrictEqual(original.limits, grant.originalLimits)
    || !isDeepStrictEqual(reg.spec.sandbox, root.resource)) {
    throw new Error("Original SRE registration or sealed funding was replaced or revoked");
  }
  const source = object(JSON.parse(await execute(["get", "karssandbox", "sre", "-n", name(root.resource.namespace), "-o", "json"])));
  const pin = object(object(source.spec).inferenceBudgetRef);
  if (source.metadata?.uid !== root.resource.uid || source.metadata?.deletionTimestamp
    || source.metadata?.annotations?.["kars.azure.com/sre-first-install-intent"] !== bound.intent.name
    || pin.account?.uid !== grant.accountUid || pin.account?.namespace !== namespace
    || pin.account?.name !== `inference-budget-${root.resource.uid}` || !isDeepStrictEqual(pin.root, root)
    || pin.scope !== "GovernedInference" || pin.taskUid !== root.resource.uid
    || pin.rootTaskUid !== root.resource.uid || pin.parentTaskUid != null
    || pin.authorizationDigest !== grant.nodes[root.resource.uid]) {
    throw new Error("SRE source no longer pins this exact account and original authority");
  }
  const intent = object(JSON.parse(await execute(["get", "configmap", name(bound.intent.name), "-n", name(root.resource.namespace), "-o", "json"])));
  if (intent.metadata?.uid !== renewal.intentUid || intent.metadata?.deletionTimestamp || intent.immutable !== true
    || typeof intent.data?.["intent.json"] !== "string"
    || createHash("sha256").update(intent.data["intent.json"]).digest("hex") !== renewal.intentDigest) {
    throw new Error("The immutable first-install intent was replaced or changed");
  }
}

export function authorizeTopUp(
  execute: Execute, text: string, namespace: string, catalog: string, now = Date.now(),
): Promise<RecordValue> {
  return authorizeFunding(execute, text, namespace, catalog, now, false);
}

export function authorizeRenewal(
  execute: Execute, text: string, namespace: string, catalog: string, now = Date.now(),
): Promise<RecordValue> {
  return authorizeFunding(execute, text, namespace, catalog, now, true);
}

/** Publish operator intent only. The controller authenticates and CAS-applies it. */
async function authorizeFunding(
  execute: Execute, text: string, namespace: string, catalog: string, now: number, renewal: boolean,
): Promise<RecordValue> {
  name(namespace); name(catalog);
  const grant = object(JSON.parse(text));
  const fields = ["id", "configUid", "accountUid", "root", "originalLimits", "tokens", "expiresAt", "nodes",
    ...(renewal ? ["fundingRenewal"] : [])];
  if (Object.keys(grant).length !== fields.length || fields.some((field) => !(field in grant))) {
    throw new Error("Expected exactly the documented token top-up authorization fields");
  }
  uid(grant.id); uid(grant.configUid); uid(grant.accountUid); integer(grant.tokens);
  const timestamp = typeof grant.expiresAt === "string"
    ? /^(\d{4})-(\d{2})-(\d{2})T([01]\d|2[0-3]):([0-5]\d):([0-5]\d)(?:\.\d+)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/.exec(grant.expiresAt) : null;
  const [year, month, day] = timestamp ? timestamp.slice(1, 4).map(Number) : [0, 0, 0];
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  const expiry = timestamp ? Date.parse(grant.expiresAt) : NaN;
  if (!Number.isFinite(expiry) || expiry <= now || month < 1 || month > 12 || day < 1 || day > days[month - 1]) {
    throw new Error("Top-up authorization must have a future RFC3339 expiry");
  }
  const root = object(grant.root);
  const resource = object(root.resource);
  uid(root.clusterUid); uid(root.workspaceUid); uid(resource.uid);
  if (root.kind !== "KarsTeam"
    && !(root.kind === "KarsSandbox" && ["sre", "bridge-orchestrator"].includes(resource.name))) {
    throw new Error("Top-ups support existing governed Teams and the built-in SRE or Bridge orchestrator only");
  }
  const accountName = name(`inference-budget-${resource.uid}`);
  const account = object(JSON.parse(await execute(["get", "karsbudgetaccount", accountName, "-n", namespace, "-o", "json"])));
  const meta = object(account.metadata);
  const spec = object(account.spec);
  const ledger = object(object(account.status).ledger);
  const reactivating = renewal && grant.fundingRenewal?.bridgeReactivation !== undefined
    && !ledger.tokenTopUps?.[grant.id];
  if (meta.uid !== grant.accountUid || meta.deletionTimestamp || !meta.resourceVersion
    || meta.annotations?.["kars.azure.com/inference-budget-bootstrap"] !== "sealed"
    || spec.scope !== "GovernedInference" || ledger.scope !== spec.scope
    || ledger.version !== "governed-inference/v1" || ledger.phase !== (reactivating ? "Closed" : "Active")
    || ledger.accountUid !== grant.accountUid || !isDeepStrictEqual(spec.root, root)
    || !isDeepStrictEqual(ledger.root, root) || !isDeepStrictEqual(spec.limits, grant.originalLimits)
    || !isDeepStrictEqual(ledger.limits, grant.originalLimits)) {
    throw new Error("Account is missing, replaced, inactive, unsealed or differs from original funding");
  }
  const limits = object(grant.originalLimits);
  integer(limits.tokens);
  const nodes = object(grant.nodes);
  if (Object.keys(nodes).length === 0 || Object.keys(nodes).length > 128) throw new Error("Explicit root-node UID/digest targets are required");
  for (const [id, digest] of Object.entries(nodes)) {
    uid(id);
    const node = object(object(ledger.nodes)[id]);
    const authority = object(node.authority);
    if (typeof digest !== "string" || !/^sha256:[a-fA-F0-9]{64}$/.test(digest)
      || node.active !== !reactivating || authority.parentUid != null || authority.rootTaskUid !== id
      || authority.authorizationDigest !== digest || authority.limits?.tokens !== limits.tokens) {
      throw new Error("Only explicitly pinned, active, unattenuated root nodes may receive a lifetime extension");
    }
  }
  const receipts = object(ledger.tokenTopUps ?? {});
  if (receipts[grant.id] && !isDeepStrictEqual(receipts[grant.id], grant)) throw new Error("Top-up ID already has different immutable content");
  if (!receipts[grant.id] && Object.keys(receipts).length >= 64) throw new Error("Lifetime top-up receipt capacity is full");
  if (renewal) await validateRenewal(execute, grant, ledger, receipts, namespace, now);
  fundingView({ ...ledger, tokenTopUps: { ...receipts, [grant.id]: grant } });
  const map = object(JSON.parse(await execute(["get", "configmap", catalog, "-n", namespace, "-o", "json"])));
  const mapMeta = object(map.metadata);
  const data = object(map.data);
  if (mapMeta.uid !== grant.configUid || !mapMeta.resourceVersion || mapMeta.deletionTimestamp
    || mapMeta.annotations?.["kars.azure.com/inference-budget-contracts"] !== "v1") {
    throw new Error("Operator ConfigMap was replaced or is not an active contracts source");
  }
  if (renewal && grant.fundingRenewal?.bridgeReactivation !== undefined) {
    const originals: unknown = JSON.parse(data["built-in-funding.json"] ?? "[]");
    if (!Array.isArray(originals)
      || originals.filter(value => value?.root?.resource?.name === "bridge-orchestrator").length !== 1
      || !isDeepStrictEqual(originals.find(value => value?.root?.resource?.name === "bridge-orchestrator"),
        ledger.nodes[resource.uid].authority.effectiveAuthorization)) {
      throw new Error("Original Bridge funding was removed, replaced or modified in the live catalog");
    }
  }
  const configured: unknown = JSON.parse(data[key] ?? "[]");
  if (!Array.isArray(configured)) throw new Error("Malformed operator top-up list");
  const ids = new Set<string>();
  for (const value of configured) {
    const entry = object(value);
    if (ids.has(uid(entry.id))) throw new Error("Duplicate operator top-up ID");
    ids.add(entry.id);
  }
  if (renewal && Object.entries(receipts).some(([id, receipt]) =>
    !isDeepStrictEqual(configured.find((value) => object(value).id === id), receipt))) {
    throw new Error("Renewal cannot restore removed or modified operator receipts");
  }
  const existing = configured.find((value) => object(value).id === grant.id);
  if (existing) {
    if (!isDeepStrictEqual(existing, grant)) throw new Error("Top-up ID already has different immutable content");
    return { id: grant.id, published: true, applied: !!receipts[grant.id] };
  }
  const encoded = JSON.stringify([...configured, grant]);
  if (Buffer.byteLength(encoded) > 262_144) throw new Error("Operator top-up configuration capacity exceeded");
  // Both tests precede the only write. Never replace the entire ConfigMap or ledger.
  await execute(["patch", "configmap", catalog, "-n", namespace, "--type=json", "-p", JSON.stringify([
    { op: "test", path: "/metadata/uid", value: grant.configUid },
    { op: "test", path: "/metadata/resourceVersion", value: mapMeta.resourceVersion },
    { op: "add", path: `/data/${key}`, value: encoded },
  ]), "-o", "name"]);
  return { id: grant.id, published: true, applied: false,
    next: "Wait for the controller's UID/CAS receipt; publication alone does not authorize dispatch. Retain this entry in Helm values." };
}
