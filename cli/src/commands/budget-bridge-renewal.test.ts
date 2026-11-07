// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { authorizeRenewal, authorizeTopUp, bridgeReactivationDigest, fundingView } from "./budget-topup.js";

type Value = Record<string, any>;
const now = Date.parse("2026-10-20T00:00:00Z");

function fixture() {
  const { ledger, grant } = JSON.parse(readFileSync(new URL("../../../shared/inference_budget/bridge-renewal-fixture.json", import.meta.url), "utf8"));
  const original = ledger.nodes[ledger.root.resource.uid].authority.effectiveAuthorization;
  const source: Value = { metadata: { ...ledger.root.resource, resourceVersion: "4" }, spec: {
    inferenceBudgetRef: { scope: "GovernedInference", root: ledger.root,
      account: { name: "inference-budget-sandbox-uid", namespace: "kars-system", uid: ledger.accountUid },
      taskUid: "sandbox-uid", rootTaskUid: "sandbox-uid", parentTaskUid: null,
      authorizationDigest: grant.nodes["sandbox-uid"] },
  } };
  const account: Value = { metadata: { name: "inference-budget-sandbox-uid", uid: ledger.accountUid, resourceVersion: "3",
    annotations: { "kars.azure.com/inference-budget-bootstrap": "sealed" } },
    spec: { root: ledger.root, limits: ledger.limits, scope: ledger.scope }, status: { ledger } };
  const catalog: Value = { metadata: { uid: grant.configUid, resourceVersion: "7",
    annotations: { "kars.azure.com/inference-budget-contracts": "v1" } }, data: {
    "built-in-funding.json": JSON.stringify([original]), "token-top-ups.json": JSON.stringify(Object.values(ledger.tokenTopUps)),
  } };
  const namespaces: Value = { "kube-system": { metadata: { uid: "cluster-uid" } }, workspace: { metadata: { uid: "workspace-uid" } } };
  const calls: string[][] = [];
  const execute = async (args: string[]) => {
    calls.push(args);
    if (args[0] === "patch" && args[1] === "configmap") return "configmap/contracts";
    const value = args[1] === "karsbudgetaccount" ? account : args[1] === "karssandbox" ? source
      : args[1] === "configmap" ? catalog : args[1] === "namespace" ? namespaces[args[2]] : undefined;
    if (args[0] !== "get" || !value) throw new Error(`Unexpected operation: ${args.join(" ")}`);
    return JSON.stringify(value);
  };
  return { ledger, grant, original, source, account, catalog, namespaces, calls, execute };
}

describe("explicit expired Bridge reactivation", () => {
  it("shares canonical numeric-key ordering and complete historical state with Rust readers", () => {
    const f = fixture();
    expect(bridgeReactivationDigest(f.ledger)).toBe(f.grant.fundingRenewal.bridgeReactivation.closedLedgerDigest);
    expect(fundingView(f.ledger).closedLedgerDigest).toBe("sha256:2e5ceb147247116976f9bb2a9e7c669172ab68cdaeaed2f1eb43153e8eb9d6bd");
    const empty = { ...f.ledger, tokenTopUps: {} };
    const omitted = { ...empty } as Value; delete omitted.tokenTopUps;
    expect(bridgeReactivationDigest(empty)).toBe(bridgeReactivationDigest(omitted));
    expect(() => bridgeReactivationDigest({ ...empty, unsafe: Number.MAX_SAFE_INTEGER + 1 })).toThrow(/exact integers/);
  });

  it("publishes one catalog CAS preserving original authority, other grants and the entire ledger", async () => {
    const f = fixture();
    const unrelated = { ...f.grant, id: "beta-team-headroom-20261020", accountUid: "another-account" };
    const existing = [...Object.values(f.ledger.tokenTopUps), unrelated];
    f.catalog.data["token-top-ups.json"] = JSON.stringify(existing);
    const before = structuredClone({ account: f.account, source: f.source, original: f.original });
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now))
      .toMatchObject({ published: true, applied: false });
    expect({ account: f.account, source: f.source, original: f.original }).toEqual(before);
    const writes = f.calls.filter(args => args[0] !== "get");
    expect(writes).toHaveLength(1);
    expect(writes[0].slice(0, 3)).toEqual(["patch", "configmap", "contracts"]);
    const patch = JSON.parse(writes[0][writes[0].indexOf("-p") + 1]);
    expect(patch.slice(0, 2)).toEqual([{ op: "test", path: "/metadata/uid", value: "config-uid" },
      { op: "test", path: "/metadata/resourceVersion", value: "7" }]);
    expect(patch[2].path).toBe("/data/token-top-ups.json");
    expect(JSON.parse(patch[2].value)).toEqual([...existing, f.grant]);
    expect(f.calls.some(args => args[1] === "karssreregistration")).toBe(false);
  });

  it("reports published/applied idempotently but cannot reuse consent after another closure", async () => {
    const f = fixture();
    f.catalog.data["token-top-ups.json"] = JSON.stringify([...Object.values(f.ledger.tokenTopUps), f.grant]);
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now))
      .toMatchObject({ published: true, applied: false });
    f.ledger.tokenTopUps[f.grant.id] = structuredClone(f.grant);
    f.ledger.phase = "Active"; f.ledger.nodes["sandbox-uid"].active = true;
    expect(await authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now))
      .toMatchObject({ published: true, applied: true });
    expect(fundingView(f.ledger)).toMatchObject({ effectiveLimits: { tokens: 500 }, effectiveExpiresAt: f.grant.expiresAt });
    f.ledger.phase = "Closed"; f.ledger.nodes["sandbox-uid"].active = false;
    await expect(authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now)).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it.each(["Frozen", "Closing", "Active", "source-uid", "source-delete", "source-owner", "source-rv", "source-name",
    "binding", "account-uid", "account-delete", "account-rv", "unsealed", "root", "catalog-uid", "catalog-delete",
    "revoked-original", "changed-original", "duplicate-original", "revoked-receipt", "modified-receipt", "cluster", "workspace-delete",
    "active-node", "extra-node", "authority-digest", "authority-task", "attenuated", "open-session", "pending-attempt", "breach",
    "stale-liability", "stale-replay-fence", "wrong-digest", "missing-prior", "duplicate-prior", "extra-prior", "self-prior",
    "sre-pins", "extra-pin", "no-consent", "wrong-root", "wrong-original-expiry", "nonextension", "expired-new", "unexpired-old"])
    ("rejects %s without publishing or mutating the ledger", async fault => {
      const f = fixture();
      const node = f.ledger.nodes["sandbox-uid"];
      const renewal = f.grant.fundingRenewal;
      if (["Frozen", "Closing", "Active"].includes(fault)) f.ledger.phase = fault;
      if (fault === "source-uid") f.source.metadata.uid = "replacement";
      if (fault === "source-delete") f.source.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "source-owner") f.source.metadata.ownerReferences = [{ uid: "owner" }];
      if (fault === "source-rv") delete f.source.metadata.resourceVersion;
      if (fault === "source-name") f.source.metadata.name = "other";
      if (fault === "binding") f.source.spec.inferenceBudgetRef.account.uid = "replacement";
      if (fault === "account-uid") f.account.metadata.uid = "replacement";
      if (fault === "account-delete") f.account.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "account-rv") delete f.account.metadata.resourceVersion;
      if (fault === "unsealed") f.account.metadata.annotations["kars.azure.com/inference-budget-bootstrap"] = "pending";
      if (fault === "root") f.grant.root = { ...f.grant.root, clusterUid: "replacement" };
      if (fault === "catalog-uid") f.catalog.metadata.uid = "replacement";
      if (fault === "catalog-delete") f.catalog.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "revoked-original") f.catalog.data["built-in-funding.json"] = "[]";
      if (fault === "changed-original") f.catalog.data["built-in-funding.json"] = JSON.stringify([{ ...f.original, limits: { tokens: 101 } }]);
      if (fault === "duplicate-original") f.catalog.data["built-in-funding.json"] = JSON.stringify([f.original, f.original]);
      if (fault === "revoked-receipt") f.catalog.data["token-top-ups.json"] = "[]";
      if (fault === "modified-receipt") f.catalog.data["token-top-ups.json"] = JSON.stringify([{ ...f.ledger.tokenTopUps["topup-1"], tokens: 201 }]);
      if (fault === "cluster") f.namespaces["kube-system"].metadata.uid = "replacement";
      if (fault === "workspace-delete") f.namespaces.workspace.metadata.deletionTimestamp = "2026-10-20T00:00:00Z";
      if (fault === "active-node") node.active = true;
      if (fault === "extra-node") f.ledger.nodes.other = structuredClone(node);
      if (fault === "authority-digest") node.authority.authorizationDigest = `sha256:${"a".repeat(64)}`;
      if (fault === "authority-task") node.authority.task = { ...node.authority.task, uid: "replacement" };
      if (fault === "attenuated") node.authority.limits = { tokens: 99 };
      if (fault === "open-session") f.ledger.sessions["2"].closed = false;
      if (fault === "pending-attempt") f.ledger.attempts.pending = { phase: "InFlight" };
      if (fault === "breach") f.ledger.attempts.breach = { phase: "Settled", observedBreach: "{}" };
      if (fault === "stale-liability") f.ledger.meters.settled.tokens++;
      if (fault === "stale-replay-fence") f.ledger.sessions["2"].closedThrough++;
      if (fault === "wrong-digest") renewal.bridgeReactivation.closedLedgerDigest = `sha256:${"a".repeat(64)}`;
      if (fault === "missing-prior") renewal.previousTopUpIds = [];
      if (fault === "duplicate-prior") renewal.previousTopUpIds.push("topup-1");
      if (fault === "extra-prior") renewal.previousTopUpIds.push("unknown");
      if (fault === "self-prior") renewal.previousTopUpIds = [f.grant.id, "topup-1"];
      if (fault === "sre-pins") renewal.registrationUid = "registration";
      if (fault === "extra-pin") renewal.bridgeReactivation.bypass = true;
      if (fault === "no-consent") delete renewal.bridgeReactivation;
      if (fault === "wrong-root") f.grant.root = { ...f.grant.root, resource: { ...f.grant.root.resource, name: "sre" } };
      if (fault === "wrong-original-expiry") renewal.originalExpiresAt = "2021-01-01T00:00:00Z";
      if (fault === "nonextension") f.grant.expiresAt = f.original.expiresAt;
      if (fault === "expired-new") f.grant.expiresAt = "2026-10-19T00:00:00Z";
      if (fault === "unexpired-old") f.original.expiresAt = "2028-01-01T00:00:00Z";
      if (!["wrong-digest", "stale-liability", "stale-replay-fence", "no-consent"].includes(fault)) {
        renewal.bridgeReactivation.closedLedgerDigest = bridgeReactivationDigest(f.ledger);
      }
      const before = structuredClone(f.account);
      await expect(authorizeRenewal(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now)).rejects.toThrow();
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      expect(f.account).toEqual(before);
    });

  it("does not allow an ordinary top-up to publish reactivation consent", async () => {
    const f = fixture();
    await expect(authorizeTopUp(f.execute, JSON.stringify(f.grant), "kars-system", "contracts", now)).rejects.toThrow(/fields/);
    expect(f.calls).toHaveLength(0);
  });
});
