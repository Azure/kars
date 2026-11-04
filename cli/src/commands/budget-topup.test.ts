// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { authorizeRenewal, authorizeTopUp, fundingView } from "./budget-topup.js";
import { createHash } from "node:crypto";

function fixture() {
  const root = { kind: "KarsTeam", resource: { name: "team", namespace: "workspace", uid: "team-uid" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" };
  const limits = { tokens: 100 };
  const digest = `sha256:${"a".repeat(64)}`;
  const grant = { id: "topup-1", configUid: "config-uid", accountUid: "account-uid", root,
    originalLimits: limits, tokens: 200, expiresAt: "2030-01-01T00:00:00Z", nodes: { principal: digest } };
  const ledger = { version: "governed-inference/v1", accountUid: "account-uid", root, limits, scope: "GovernedInference", phase: "Active",
    tokenTopUps: {} as Record<string, unknown>, nodes: { principal: { active: true,
      authority: { parentUid: null, rootTaskUid: "principal", authorizationDigest: digest, limits } } },
    meters: { reserved: { tokens: 10, usdMicros: 0 }, settled: { tokens: 40, usdMicros: 0 }, uncertain: { tokens: 50, usdMicros: 0 } } };
  const account = { metadata: { uid: "account-uid", resourceVersion: "3",
    annotations: { "kars.azure.com/inference-budget-bootstrap": "sealed" } },
    spec: { scope: ledger.scope, root, limits }, status: { ledger } };
  const map = { metadata: { uid: "config-uid", resourceVersion: "7", annotations: { "kars.azure.com/inference-budget-contracts": "v1" } },
    data: { "contracts.json": "original-contracts", "token-top-ups.json": "[]" } };
  const calls: string[][] = [];
  const execute = async (args: string[]) => {
    calls.push(args);
    if (args[0] === "patch") return "configmap/contracts";
    return JSON.stringify(args[1] === "karsbudgetaccount" ? account : map);
  };
  return { grant, ledger, account, map, calls, execute };
}

function renewalFixture() {
  const f = fixture();
  const root = f.grant.root;
  root.kind = "KarsSandbox"; root.resource.name = "sre"; root.resource.uid = "principal";
  const original = { root, limits: f.ledger.limits, expiresAt: "2020-01-01T00:00:00Z" };
  Object.assign(f.ledger.nodes.principal.authority, { effectiveAuthorization: original });
  const old = { ...f.grant, expiresAt: original.expiresAt };
  f.ledger.tokenTopUps[old.id] = old;
  f.map.data["token-top-ups.json"] = JSON.stringify([old]);
  const intent = { metadata: { uid: "intent-uid" }, immutable: true, data: { "intent.json": JSON.stringify({ funding: original }) } };
  const digest = createHash("sha256").update(intent.data["intent.json"]).digest("hex");
  const grant = { ...f.grant, id: "renewal-1", fundingRenewal: {
    registrationUid: "registration-uid", intentUid: "intent-uid", intentDigest: digest,
    originalExpiresAt: original.expiresAt, previousTopUpIds: [old.id],
  } };
  const registration = { metadata: { uid: "registration-uid" }, spec: { enabled: true, sandbox: root.resource,
    firstInstallFunding: { intent: { name: "kars-sre-first-install", uid: "intent-uid", digest }, grant: original } } };
  const source = { metadata: { uid: "principal", annotations: { "kars.azure.com/sre-first-install-intent": "kars-sre-first-install" } },
    spec: { inferenceBudgetRef: { root, account: { uid: f.grant.accountUid, namespace: "kars-system", name: "inference-budget-principal" },
      scope: "GovernedInference", taskUid: "principal", rootTaskUid: "principal", authorizationDigest: f.grant.nodes.principal } } };
  const execute = async (args: string[]) => {
    const value = args[1] === "karssreregistration" ? registration : args[1] === "karssandbox" ? source
      : args[1] === "configmap" && args[2] === "kars-sre-first-install" ? intent : undefined;
    if (value) { f.calls.push(args); return JSON.stringify(value); }
    return f.execute(args);
  };
  return { ...f, grant, old, original, registration, source, intent, execute };
}

describe("explicit operator funding renewal", () => {
  it("publishes only a fenced receipt and retains all expired grants, accounting and immutable source", async () => {
    const f = renewalFixture();
    const baseline = structuredClone({ account: f.account, registration: f.registration, source: f.source, intent: f.intent });
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts"))
      .toMatchObject({ published: true, applied: false });
    expect({ account: f.account, registration: f.registration, source: f.source, intent: f.intent }).toEqual(baseline);
    const writes = f.calls.filter(args => args[0] === "patch");
    expect(writes).toHaveLength(1);
    expect(writes[0].slice(0, 3)).toEqual(["patch", "configmap", "contracts"]);
    const patch = JSON.parse(writes[0][writes[0].indexOf("-p") + 1]);
    expect(patch.slice(0, 2)).toEqual([{ op: "test", path: "/metadata/uid", value: "config-uid" },
      { op: "test", path: "/metadata/resourceVersion", value: "7" }]);
    expect(JSON.parse(patch[2].value)).toEqual([f.old, f.grant]);
  });
  it("does not allow the ordinary top-up command to issue renewal", async () => {
    const f = renewalFixture();
    await expect(authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow(/fields/);
    expect(f.calls).toHaveLength(0);
  });
  it("replays published and applied renewal without issuing a write", async () => {
    const f = renewalFixture();
    f.map.data["token-top-ups.json"] = JSON.stringify([f.old, f.grant]);
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts"))
      .toEqual({ id: f.grant.id, published: true, applied: false });
    f.ledger.tokenTopUps[f.grant.id] = f.grant;
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts"))
      .toEqual({ id: f.grant.id, published: true, applied: true });
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(fundingView(f.ledger).fundingRenewals).toEqual([{ id: f.grant.id, expiresAt: f.grant.expiresAt, ...f.grant.fundingRenewal }]);
    expect(fundingView(f.ledger)).toMatchObject({ originalExpiresAt: f.original.expiresAt, effectiveExpiresAt: f.grant.expiresAt });
  });
  it.each(["missing-prior", "duplicate-prior", "stale-snapshot", "extra-field", "registration", "disabled", "intent-uid", "intent-bytes",
    "mutable-intent", "source", "binding", "root", "expired", "original-expiry", "removed", "modified", "frozen", "unsealed", "branch"])
    ("rejects %s without any write", async fault => {
      const f = renewalFixture();
      if (fault === "missing-prior") f.grant.fundingRenewal.previousTopUpIds = [];
      if (fault === "duplicate-prior") f.grant.fundingRenewal.previousTopUpIds.push(f.old.id);
      if (fault === "stale-snapshot") f.ledger.tokenTopUps.other = { ...f.old, id: "other" };
      if (fault === "extra-field") Object.assign(f.grant.fundingRenewal, { bypass: true });
      if (fault === "registration") f.registration.metadata.uid = "replaced";
      if (fault === "disabled") f.registration.spec.enabled = false;
      if (fault === "intent-uid") f.intent.metadata.uid = "replaced";
      if (fault === "intent-bytes") f.intent.data["intent.json"] += " ";
      if (fault === "mutable-intent") f.intent.immutable = false;
      if (fault === "source") f.source.metadata.uid = "replaced";
      if (fault === "binding") f.source.spec.inferenceBudgetRef.account.uid = "replaced";
      if (fault === "root") f.grant.root = { ...f.grant.root, clusterUid: "replacement" };
      if (fault === "expired") f.grant.expiresAt = "2020-01-01T00:00:00Z";
      if (fault === "original-expiry") f.grant.fundingRenewal.originalExpiresAt = "2021-01-01T00:00:00Z";
      if (fault === "removed") f.map.data["token-top-ups.json"] = "[]";
      if (fault === "modified") f.map.data["token-top-ups.json"] = JSON.stringify([{ ...f.old, tokens: 999 }]);
      if (fault === "frozen") f.ledger.phase = "Frozen";
      if (fault === "unsealed") f.account.metadata.annotations["kars.azure.com/inference-budget-bootstrap"] = "pending";
      if (fault === "branch") f.ledger.tokenTopUps[f.old.id] = { ...f.old, expiresAt: f.grant.expiresAt, fundingRenewal: f.grant.fundingRenewal };
      await expect(authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow();
      expect(f.calls.some(args => args[0] !== "get")).toBe(false);
    });
});

describe("operator lifetime top-ups", () => {
  it("requires a separate funding permission in the shared fail-closed admission bundle", () => {
    const bundle = JSON.parse(readFileSync(new URL("../../../deploy/helm/kars/files/inference-budget-admission.json", import.meta.url), "utf8"));
    const policy = bundle.items.find((item: { name: string }) => item.name === "kars-inference-budget-token-top-ups");
    expect(policy.spec.failurePolicy).toBe("Fail");
    expect(policy.spec.matchConstraints.resourceRules[0].operations).toEqual(["CREATE", "UPDATE", "DELETE"]);
    expect(policy.spec.validations[0].expression).toContain("authorizer.group('kars.azure.com').resource('karsbudgetaccounts').namespace(request.namespace).check('fund').allowed()");
    expect(policy.spec.validations[0].expression).not.toContain("request.userInfo");
    expect(policy.spec.variables.map((value: { expression: string }) => value.expression).join(" ")).toContain("token-top-ups.json");
    const template = readFileSync(new URL("../../../deploy/helm/kars/templates/inference-budget.yaml", import.meta.url), "utf8");
    expect(template).toContain("token-top-ups.json: {{ ($budget.tokenTopUps | default list) | toJson | quote }}");
    expect(template).toContain("inferenceBudget.tokenTopUps requires inferenceBudget.enabled");
    expect(template).not.toMatch(/verbs:.*["']fund["']/);
  });
  it("publishes only UID/RV-fenced operator intent, not account or workload changes", async () => {
    const f = fixture();
    const before = structuredClone(f.account);
    const result = await authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts");
    expect(result).toMatchObject({ published: true, applied: false });
    expect(f.account).toEqual(before);
    const writes = f.calls.filter((args) => args[0] === "patch");
    expect(writes).toHaveLength(1);
    const patch = JSON.parse(writes[0][writes[0].indexOf("-p") + 1]);
    expect(patch.slice(0, 2)).toEqual([
      { op: "test", path: "/metadata/uid", value: "config-uid" },
      { op: "test", path: "/metadata/resourceVersion", value: "7" },
    ]);
    expect(patch[2].path).toBe("/data/token-top-ups.json");
    expect(JSON.parse(patch[2].value)).toEqual([f.grant]);
  });

  it.each(["sre", "bridge-orchestrator"])("publishes a UID-bound %s top-up without changing the original account", async (name) => {
    const f = fixture();
    f.grant.root.kind = "KarsSandbox";
    f.grant.root.resource.name = name;
    f.grant.root.resource.uid = "principal";
    const original = { root: f.grant.root, limits: f.ledger.limits, expiresAt: "2030-01-01T00:00:00Z" };
    Object.assign(f.ledger.nodes.principal.authority, { effectiveAuthorization: original });
    const before = structuredClone(f.account);
    expect(await authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts"))
      .toMatchObject({ published: true, applied: false });
    expect(f.account).toEqual(before);
    const writes = f.calls.filter(args => args[0] === "patch");
    expect(writes).toHaveLength(1);
    const patch = JSON.parse(writes[0][writes[0].indexOf("-p") + 1]);
    expect(JSON.parse(patch[2].value)).toEqual([f.grant]);
    f.ledger.tokenTopUps[f.grant.id] = f.grant;
    expect(fundingView(f.ledger)).toMatchObject({ effectiveLimits: { tokens: 300 }, originalExpiresAt: original.expiresAt, effectiveExpiresAt: original.expiresAt });
  });

  it("does not authorize ordinary sandboxes or orchestrator renewal through the SRE path", async () => {
    const f = fixture();
    f.grant.root.kind = "KarsSandbox";
    f.grant.root.resource.name = "ordinary-sandbox";
    await expect(authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow(/built-in/);
    expect(f.calls).toHaveLength(0);
    const renewal = renewalFixture();
    renewal.grant.root.resource.name = "bridge-orchestrator";
    await expect(authorizeRenewal(renewal.execute, JSON.stringify(renewal.grant), "kars-system", "contracts")).rejects.toThrow(/exact SRE root/);
    expect(renewal.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("preserves other accounts' existing operator authorizations", async () => {
    const f = fixture();
    const other = { ...f.grant, id: "other-topup", accountUid: "other-account" };
    f.map.data["token-top-ups.json"] = JSON.stringify([other]);
    await authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts");
    const write = f.calls.find((args) => args[0] === "patch")!;
    const patch = JSON.parse(write[write.indexOf("-p") + 1]);
    expect(JSON.parse(patch[2].value)).toEqual([other, f.grant]);
  });

  it.each(["2030-02-29T00:00:00Z", "2030-04-31T00:00:00Z", "2030-01-01 00:00:00Z", "2030-01-01T24:00:00Z"])("rejects normalized but invalid RFC3339 expiry %s", async (expiresAt) => {
    const f = fixture();
    await expect(authorizeTopUp(f.execute, JSON.stringify({ ...f.grant, expiresAt }), "kars-system", "contracts"))
      .rejects.toThrow("future RFC3339 expiry");
    expect(f.calls).toHaveLength(0);
  });

  it("does not duplicate published or applied IDs and exposes original/effective limits", async () => {
    const f = fixture();
    f.map.data["token-top-ups.json"] = JSON.stringify([f.grant]);
    f.ledger.tokenTopUps[f.grant.id] = f.grant;
    expect(await authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts"))
      .toEqual({ id: f.grant.id, published: true, applied: true });
    expect(f.calls.every((args) => args[0] === "get")).toBe(true);
    expect(fundingView(f.ledger)).toMatchObject({ originalLimits: { tokens: 100 }, effectiveLimits: { tokens: 300 }, addedTokens: 200 });
    f.grant.tokens++;
    await expect(authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow(/different/);
  });

  it.each([0, -1, 0.5, Number.MAX_SAFE_INTEGER, "100"])("rejects zero/inexact/overflow increments %s", async (tokens) => {
    const f = fixture();
    await expect(authorizeTopUp(f.execute, JSON.stringify({ ...f.grant, tokens }), "kars-system", "contracts")).rejects.toThrow();
    expect(f.calls.some((args) => args[0] === "patch")).toBe(false);
  });

  it.each(["account", "config", "frozen", "closed", "child", "narrow", "digest", "expiry", "root"])("rejects %s mismatch before publishing", async (kind) => {
    const f = fixture();
    if (kind === "account") f.account.metadata.uid = "replacement";
    if (kind === "config") f.map.metadata.uid = "replacement";
    if (kind === "frozen") f.ledger.phase = "Frozen";
    if (kind === "closed") f.ledger.phase = "Closed";
    if (kind === "child") Object.assign(f.ledger.nodes.principal.authority, { parentUid: "parent" });
    if (kind === "narrow") f.ledger.nodes.principal.authority.limits = { tokens: 50 };
    if (kind === "digest") f.grant.nodes.principal = `sha256:${"b".repeat(64)}`;
    if (kind === "expiry") f.grant.expiresAt = "2000-01-01T00:00:00Z";
    if (kind === "root") f.account.spec.root = { ...f.grant.root, clusterUid: "replaced" };
    await expect(authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow();
    expect(f.calls.some((args) => args[0] === "patch")).toBe(false);
  });

  it("propagates CAS/API failure without falling back to a ledger write", async () => {
    const f = fixture();
    await expect(authorizeTopUp(async (args) => {
      if (args[0] === "patch") throw new Error("CAS conflict");
      return f.execute(args);
    }, JSON.stringify(f.grant), "kars-system", "contracts")).rejects.toThrow("CAS conflict");
    expect(f.calls).toHaveLength(2);
  });
});
