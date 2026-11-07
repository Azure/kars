// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { authorizeUnbounded } from "./budget-unbounded.js";
import { bridgeReactivationDigest, fundingView } from "./budget-topup.js";
import { budgetCommand, budgetStatus } from "./budget.js";

type Value = Record<string, any>;
const now = Date.parse("2026-10-20T00:00:00Z");
const key = "unbounded-token-allowances.json";

function fixture(kind = "KarsSandbox", sandboxName = "bridge-orchestrator") {
  const { ledger } = JSON.parse(readFileSync(new URL("../../../shared/inference_budget/bridge-renewal-fixture.json", import.meta.url), "utf8"));
  ledger.root.kind = kind;
  ledger.root.resource.name = kind === "KarsSandbox" ? sandboxName : "mission";
  ledger.limits.usdMicros = 500;
  const root = ledger.root;
  const node = ledger.nodes[root.resource.uid];
  node.authority.task = structuredClone(root.resource);
  node.authority.limits = structuredClone(ledger.limits);
  node.authority.effectiveAuthorization.root = structuredClone(root);
  node.authority.effectiveAuthorization.limits = structuredClone(ledger.limits);
  node.authority.authorizationDigest = bridgeReactivationDigest(node.authority.effectiveAuthorization);
  const original = node.authority.effectiveAuthorization;
  for (const receipt of Object.values(ledger.tokenTopUps) as Value[]) {
    receipt.root = structuredClone(root); receipt.originalLimits = structuredClone(ledger.limits);
    receipt.nodes = { [root.resource.uid]: node.authority.authorizationDigest };
  }
  // Historical usage above the original token ceiling is never reset by an allowance.
  ledger.meters.settled.tokens = 250;
  node.meters.settled.tokens = 250;
  if (kind !== "KarsSandbox" || sandboxName === "sre") { ledger.phase = "Active"; node.active = true; }
  const reference = { name: `inference-budget-${root.resource.uid}`, namespace: "accounting", uid: ledger.accountUid };
  const binding = { scope: ledger.scope, root: structuredClone(root), account: reference,
    taskUid: root.resource.uid, rootTaskUid: root.resource.uid, parentTaskUid: null,
    authorizationDigest: node.authority.authorizationDigest };
  const source: Value = { metadata: { ...root.resource, resourceVersion: "4" }, spec: kind === "KarsSandbox"
    ? { inferenceBudgetRef: binding } : { envelope: { budget: { ...ledger.limits, scope: ledger.scope } } },
    status: { phase: "Ready", ...(kind === "KarsTeam" ? { inferenceBudgetAccount: reference } : { inferenceBudget: binding }) } };
  const account: Value = { metadata: { ...reference, resourceVersion: "3",
    annotations: { "kars.azure.com/inference-budget-bootstrap": "sealed" } },
    spec: { root: structuredClone(root), limits: structuredClone(ledger.limits), scope: ledger.scope }, status: { ledger } };
  const catalog: Value = { metadata: { name: "contracts", namespace: "accounting", uid: "config-uid", resourceVersion: "7",
    annotations: { "kars.azure.com/inference-budget-contracts": "v1" } }, data: {
    "contracts.json": "preserved-original-contracts", "built-in-funding.json": JSON.stringify([original]),
    "token-top-ups.json": JSON.stringify(Object.values(ledger.tokenTopUps)),
  } };
  const namespaces: Value = Object.fromEntries([["kube-system", root.clusterUid], [root.resource.namespace, root.workspaceUid]]
    .map(([name, uid]) => [name, { metadata: { name, uid, resourceVersion: "1" } }]));
  const grant: Value = { id: "beta-unbounded", configUid: "config-uid", accountUid: ledger.accountUid,
    root: structuredClone(root), originalLimits: structuredClone(ledger.limits),
    ledgerDigest: bridgeReactivationDigest(ledger), previousTokenTopUpIds: Object.keys(ledger.tokenTopUps).sort() };
  const intent: Value = { metadata: { name: "first-install", namespace: root.resource.namespace, uid: "intent-uid", resourceVersion: "1" },
    immutable: true, data: { "intent.json": JSON.stringify({ funding: original }) } };
  const registration: Value = { metadata: { name: "canonical", uid: "registration-uid", resourceVersion: "1" }, spec: {
    enabled: true, sandbox: structuredClone(root.resource), firstInstallFunding: { grant: structuredClone(original),
      intent: { name: "first-install", uid: "intent-uid", digest: createHash("sha256").update(intent.data["intent.json"]).digest("hex") } } } };
  if (sandboxName === "sre") source.metadata.annotations = { "kars.azure.com/sre-first-install-intent": "first-install" };
  const calls: string[][] = [];
  const rights: Value = { fund: "yes", patch: "yes" };
  const execute = async (args: string[]) => {
    calls.push(args);
    if (args[0] === "auth") return rights[args[2]];
    if (args[0] === "patch" && args[1] === "configmap") return "configmap/contracts";
    const value = args[1] === "karsbudgetaccount" ? account : args[1] === "namespace" ? namespaces[args[2]]
      : args[1] === "configmap" ? (args[2] === "first-install" ? intent : catalog)
        : args[1] === "karssreregistration" ? registration : args[1] === kind.toLowerCase() ? source : undefined;
    if (args[0] !== "get" || !value) throw new Error(`Unexpected operation: ${args.join(" ")}`);
    return JSON.stringify(value);
  };
  const publish = () => authorizeUnbounded(execute, JSON.stringify(grant), "accounting", "contracts", now);
  return { grant, ledger, original, source, account, catalog, namespaces, calls, execute, publish, rights, intent, registration };
}

describe("explicit durable unbounded token authorization", () => {
  it("registers a narrowly scoped command without a general bypass flag", () => {
    const command = budgetCommand().commands.find(value => value.name() === "authorize-unbounded")!;
    expect(command.options.map(value => value.long)).toEqual(["--file", "--namespace", "--catalog"]);
    expect(command.description()).toContain("preserves currency caps");
  });

  it.each(["KarsTask", "KarsTeam", "KarsSandbox"])("publishes %s with fund permission and actual ConfigMap UID/RV CAS only", async kind => {
    const f = fixture(kind);
    const other = { ...f.grant, id: "other", accountUid: "other-account" };
    f.catalog.data[key] = JSON.stringify([other]);
    const before = structuredClone({ account: f.account, source: f.source, catalog: f.catalog });
    expect(await f.publish()).toMatchObject({ published: true, applied: false });
    expect({ account: f.account, source: f.source, catalog: f.catalog }).toEqual(before);
    expect(f.calls.filter(args => args[0] === "auth")).toEqual([
      ["auth", "can-i", "fund", "karsbudgetaccounts.kars.azure.com", "-n", "accounting"],
      ["auth", "can-i", "patch", "configmap/contracts", "-n", "accounting"],
    ]);
    const writes = f.calls.filter(args => args[0] === "patch");
    expect(writes).toHaveLength(1);
    expect(writes[0].slice(0, 6)).toEqual(["patch", "configmap", "contracts", "-n", "accounting", "--type=json"]);
    const patch = JSON.parse(writes[0][writes[0].indexOf("-p") + 1]);
    expect(patch).toEqual([
      { op: "test", path: "/metadata/uid", value: "config-uid" },
      { op: "test", path: "/metadata/resourceVersion", value: "7" },
      { op: "add", path: `/data/${key}`, value: JSON.stringify([other, f.grant]) },
    ]);
    expect(f.grant).not.toHaveProperty("expiresAt"); expect(f.grant).not.toHaveProperty("tokens");
    expect(f.ledger.meters.settled.tokens).toBe(250);
  });

  it("supports SRE only with live sealed registration, source and immutable intent", async () => {
    const f = fixture("KarsSandbox", "sre");
    expect(await f.publish()).toMatchObject({ published: true });
    f.registration.spec.enabled = false;
    await expect(f.publish()).rejects.toThrow(/revoked/);
    f.registration.spec.enabled = true;
    f.intent.data["intent.json"] += " ";
    await expect(f.publish()).rejects.toThrow(/intent/);
  });

  it("reports original funding, complete receipt history, no token cap/expiry, and unchanged currency cap and meters", async () => {
    const f = fixture();
    f.ledger.unboundedTokens = structuredClone(f.grant);
    f.ledger.phase = "Active"; f.ledger.nodes["sandbox-uid"].active = true;
    const before = structuredClone(f.ledger);
    expect(fundingView(f.ledger)).toMatchObject({ originalLimits: { tokens: 100, usdMicros: 500 },
      effectiveLimits: { usdMicros: 500 }, tokenAllowance: "unbounded", unboundedTokens: f.grant,
      tokenTopUps: f.ledger.tokenTopUps, originalExpiresAt: "2020-01-01T00:00:00Z", effectiveExpiresAt: null,
      ledgerDigest: bridgeReactivationDigest(f.ledger) });
    expect(fundingView(f.ledger).effectiveLimits).not.toHaveProperty("tokens");
    const status = await budgetStatus(f.execute, "sandbox", "bridge-orchestrator", "workspace");
    expect(status).toMatchObject({ tokenAllowance: "unbounded", effectiveLimits: { usdMicros: 500 },
      meters: { settled: { tokens: "250", usdMicros: "0" } } });
    expect(f.ledger).toEqual(before);
  });

  it("retains finite fundingView fields exactly and exposes the digest before authorizing", () => {
    const f = fixture();
    const { ledgerDigest, ...view } = fundingView(f.ledger);
    expect(view).toEqual({ originalLimits: { tokens: 100, usdMicros: 500 }, effectiveLimits: { tokens: 300, usdMicros: 500 },
      addedTokens: 200, tokenTopUps: f.ledger.tokenTopUps, fundingRenewals: [],
      originalExpiresAt: "2020-01-01T00:00:00Z", effectiveExpiresAt: "2020-01-01T00:00:00Z",
      closedLedgerDigest: f.grant.ledgerDigest,
      fundingAuthority: "Recorded ceiling only; original grants, explicit renewals and live operator authority are rechecked before reserve/begin" });
    expect(ledgerDigest).toBe(f.grant.ledgerDigest);
    const empty = { ...f.ledger, tokenTopUps: {}, unboundedTokens: undefined };
    const omitted = { ...empty } as Value; delete omitted.tokenTopUps; delete omitted.unboundedTokens;
    expect(bridgeReactivationDigest(empty)).toBe(bridgeReactivationDigest(omitted));
  });

  it("is idempotent before/after consumption, but cannot change consumed content or restore revoked intent", async () => {
    const f = fixture();
    f.catalog.data[key] = JSON.stringify([f.grant]);
    expect(await f.publish()).toEqual({ id: f.grant.id, published: true, applied: false });
    f.ledger.unboundedTokens = structuredClone(f.grant);
    f.ledger.phase = "Active"; f.ledger.nodes["sandbox-uid"].active = true;
    f.ledger.meters.settled.tokens += 1000;
    expect(await f.publish()).toEqual({ id: f.grant.id, published: true, applied: true });
    f.grant.ledgerDigest = `sha256:${"a".repeat(64)}`;
    await expect(f.publish()).rejects.toThrow(/Consumed/);
    f.grant.ledgerDigest = f.ledger.unboundedTokens.ledgerDigest;
    delete f.catalog.data[key];
    await expect(f.publish()).rejects.toThrow(/revoked/);
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
  });

  it.each(["stale-hash", "account-uid", "account-name", "account-namespace", "account-rv", "unsealed", "catalog-uid", "catalog-rv",
    "catalog-deleted", "catalog-namespace", "catalog-annotation", "source-uid", "source-deleted", "source-owner", "source-binding",
    "cluster-uid", "workspace-uid", "revoked-original", "revoked-topup", "modified-topup", "missing-prior", "duplicate-prior",
    "extra-prior", "unknown-field", "expiry", "tokens", "unknown-root-field", "unknown-limit", "unsafe-limit", "root-active",
    "reserved", "InFlight", "breach", "open-session", "extra-node", "Frozen", "Closing", "unexpired", "different-allowance",
    "duplicate-id", "duplicate-account", "wrong-config-entry", "fund-denied", "patch-denied"])
    ("rejects %s without publication or ledger changes", async fault => {
      const f = fixture();
      if (fault === "stale-hash") f.ledger.meters.settled.tokens++;
      if (fault === "account-uid") f.account.metadata.uid = "wrong";
      if (fault === "account-name") f.account.metadata.name = "wrong";
      if (fault === "account-namespace") f.account.metadata.namespace = "wrong";
      if (fault === "account-rv") delete f.account.metadata.resourceVersion;
      if (fault === "unsealed") delete f.account.metadata.annotations;
      if (fault === "catalog-uid") f.catalog.metadata.uid = "wrong";
      if (fault === "catalog-rv") delete f.catalog.metadata.resourceVersion;
      if (fault === "catalog-deleted") f.catalog.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "catalog-namespace") f.catalog.metadata.namespace = "wrong";
      if (fault === "catalog-annotation") f.catalog.metadata.annotations = {};
      if (fault === "source-uid") f.source.metadata.uid = "wrong";
      if (fault === "source-deleted") f.source.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "source-owner") f.source.metadata.ownerReferences = [{ uid: "owner" }];
      if (fault === "source-binding") f.source.spec.inferenceBudgetRef.account.uid = "wrong";
      if (fault === "cluster-uid") f.namespaces["kube-system"].metadata.uid = "wrong";
      if (fault === "workspace-uid") f.namespaces.workspace.metadata.uid = "wrong";
      if (fault === "revoked-original") f.catalog.data["built-in-funding.json"] = "[]";
      if (fault === "revoked-topup") f.catalog.data["token-top-ups.json"] = "[]";
      if (fault === "modified-topup") f.catalog.data["token-top-ups.json"] = JSON.stringify([{ ...f.ledger.tokenTopUps["topup-1"], tokens: 300 }]);
      if (fault === "missing-prior") f.grant.previousTokenTopUpIds = [];
      if (fault === "duplicate-prior") f.grant.previousTokenTopUpIds.push("topup-1");
      if (fault === "extra-prior") f.grant.previousTokenTopUpIds.push("unknown");
      if (fault === "unknown-field") f.grant.bypass = true;
      if (fault === "expiry") f.grant.expiresAt = "2030-01-01T00:00:00Z";
      if (fault === "tokens") f.grant.tokens = 10000000;
      if (fault === "unknown-root-field") f.grant.root.bypass = true;
      if (fault === "unknown-limit") f.grant.originalLimits.cost = 50;
      if (fault === "unsafe-limit") f.grant.originalLimits.tokens = Number.MAX_SAFE_INTEGER + 1;
      if (fault === "root-active") f.ledger.nodes["sandbox-uid"].active = true;
      if (fault === "reserved") f.ledger.meters.reserved.tokens = 1;
      if (fault === "InFlight") f.ledger.attempts.pending = { phase: "InFlight" };
      if (fault === "breach") f.ledger.attempts.breach = { phase: "Settled", observedBreach: "{}" };
      if (fault === "open-session") f.ledger.sessions["2"].closed = false;
      if (fault === "extra-node") f.ledger.nodes.other = structuredClone(f.ledger.nodes["sandbox-uid"]);
      if (["Frozen", "Closing"].includes(fault)) f.ledger.phase = fault;
      if (fault === "unexpired") f.original.expiresAt = "2030-01-01T00:00:00Z";
      if (fault === "different-allowance") f.catalog.data[key] = JSON.stringify([{ ...f.grant, id: "other" }]);
      if (fault === "duplicate-id") f.catalog.data[key] = JSON.stringify([f.grant, { ...f.grant, accountUid: "other" }]);
      if (fault === "duplicate-account") f.catalog.data[key] = JSON.stringify([f.grant, { ...f.grant, id: "other" }]);
      if (fault === "wrong-config-entry") f.catalog.data[key] = JSON.stringify([{ ...f.grant, configUid: "wrong" }]);
      if (fault === "fund-denied") f.rights.fund = "no";
      if (fault === "patch-denied") f.rights.patch = "no";
      if (fault !== "stale-hash") f.grant.ledgerDigest = bridgeReactivationDigest(f.ledger);
      const before = structuredClone(f.account);
      await expect(f.publish()).rejects.toThrow();
      expect(f.calls.some(args => args[0] === "patch")).toBe(false);
      expect(f.account).toEqual(before);
    });

  it.each(["KarsTask", "KarsTeam", "sre"])("rejects Closed non-Bridge %s", async kind => {
    const f = fixture(kind === "sre" ? "KarsSandbox" : kind, kind === "sre" ? "sre" : "bridge-orchestrator");
    f.ledger.phase = "Closed"; f.ledger.nodes["sandbox-uid"].active = false;
    f.grant.ledgerDigest = bridgeReactivationDigest(f.ledger);
    await expect(f.publish()).rejects.toThrow(/Only Active/);
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
  });

  it.each(["KarsTask", "KarsTeam"])("rejects revoked live %s root binding/budget", async kind => {
    const f = fixture(kind);
    f.source.spec.envelope.budget.tokens++;
    await expect(f.publish()).rejects.toThrow(/changed/);
    f.source.spec.envelope.budget.tokens--;
    if (kind === "KarsTask") f.source.status.inferenceBudget.account.uid = "wrong";
    else f.source.status.inferenceBudgetAccount.uid = "wrong";
    await expect(f.publish()).rejects.toThrow(/binding|binds/);
  });

  it("propagates CAS denial without fallback or raw ledger writes", async () => {
    const f = fixture();
    await expect(authorizeUnbounded(async args => {
      if (args[0] === "patch") throw new Error("resourceVersion CAS conflict");
      return f.execute(args);
    }, JSON.stringify(f.grant), "accounting", "contracts", now)).rejects.toThrow(/CAS conflict/);
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]))).toBe(true);
  });
});
