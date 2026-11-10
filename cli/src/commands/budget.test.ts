// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { budgetStatus, governedPlan } from "./budget.js";

const plan = {
  apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask",
  metadata: { name: "mission", namespace: "workspace" },
  spec: { objective: "Build", envelope: { tier: 3, authorityCeiling: 3, delegationDepth: 2,
    budget: { tokens: 500, usdMicros: 0 } }, execution: { launch: false } },
};

describe("explicit governed inference CLI", () => {
  it("creates a scoped plan without mapping aggregate limits into daily budgets", () => {
    const result = governedPlan(JSON.stringify(plan), "workspace");
    expect(result).toEqual({ ...plan, spec: { ...plan.spec,
      envelope: { ...plan.spec.envelope, budget: { ...plan.spec.envelope.budget, scope: "GovernedInference" } } } });
    expect(JSON.stringify(result)).not.toContain("daily");
  });

  it.each([-1, 0.5, Number.MAX_SAFE_INTEGER + 1, "10"])("rejects inexact or negative cap %s", (tokens) => {
    const bad = structuredClone(plan) as Record<string, any>;
    bad.spec.envelope.budget.tokens = tokens;
    expect(() => governedPlan(JSON.stringify(bad), "workspace")).toThrow(/exact nonnegative/);
  });

  it("rejects cross-workspace creation, existing UID imports and fully unbounded plans", () => {
    expect(() => governedPlan(JSON.stringify(plan), "other")).toThrow(/workspace/);
    expect(() => governedPlan(JSON.stringify({ ...plan, metadata: { ...plan.metadata, uid: "old" } }), "workspace")).toThrow(/new UID/);
    const unbounded = structuredClone(plan);
    unbounded.spec.envelope.budget.tokens = 0;
    expect(() => governedPlan(JSON.stringify(unbounded), "workspace")).toThrow(/positive/);
  });

  it("reports the built-in SRE's spec-pinned account without changing its original limits", async () => {
    const root = { kind: "KarsSandbox", resource: { namespace: "workspace", name: "sre", uid: "sre-uid" },
      workspaceUid: "workspace-uid", clusterUid: "cluster-uid" };
    const reference = { namespace: "accounting", name: "inference-budget-sre-uid", uid: "account-uid" };
    const sandbox = { metadata: { name: "sre", namespace: "workspace", uid: "sre-uid" }, status: { phase: "Ready" },
      spec: { inferenceBudgetRef: { root, account: reference, taskUid: "sre-uid" } } };
    const limits = { tokens: 100 };
    const grant = { id: "topup-1", accountUid: reference.uid, root, originalLimits: limits, tokens: 200 };
    const ledger = { root, accountUid: reference.uid, version: "governed-inference/v1", scope: "GovernedInference", phase: "Active", limits,
      attempts: {}, tokenTopUps: { [grant.id]: grant }, meters: { reserved: { tokens: 0, usdMicros: 0 },
        settled: { tokens: 20, usdMicros: 0 }, uncertain: { tokens: 30, usdMicros: 0 }, unpricedAttempts: 3 } };
    const stored = { metadata: { uid: reference.uid }, spec: { scope: ledger.scope, root }, status: { ledger } };
    const result = await budgetStatus(async (args) => JSON.stringify(args[1] === "karssandbox" ? sandbox : stored), "sandbox", "sre", "workspace");
    expect(result).toMatchObject({ originalLimits: limits, effectiveLimits: { tokens: 300 }, addedTokens: 200 });
    expect(ledger.limits.tokens).toBe(100);
  });

  it("reports unknown usage even with zero uncertain tokens and record-only pending prices", async () => {
    const root = { kind: "KarsTask", resource: { namespace: "workspace", name: "mission", uid: "task-uid" }, workspaceUid: "workspace-uid", clusterUid: "cluster-uid" };
    const account = { namespace: "accounting", name: "root-account", uid: "account-uid" };
    const task = { metadata: { ...plan.metadata, uid: "task-uid" }, status: { inferenceBudget: { root, account, taskUid: "task-uid" } } };
    const ledger = { version: "governed-inference/v1", scope: "GovernedInference", phase: "Active", accountUid: account.uid, limits: {},
      attempts: { pending: { phase: "InFlight", quote: { recordOnly: {} } } }, meters: {
        reserved: { tokens: 0, usdMicros: 0 }, settled: { tokens: 0, usdMicros: 0 }, uncertain: { tokens: 0, usdMicros: 0 }, unpricedAttempts: 0, unknownUsageAttempts: 1,
      } };
    const stored = { metadata: { uid: account.uid }, spec: { scope: "GovernedInference", root }, status: { ledger } };
    const result = await budgetStatus(async (args) => JSON.stringify(args[1] === "karstask" ? task : stored), "task", "mission", "workspace");
    expect(result.unknownUsageAttempts).toBe("1");
    expect(result.usageCoverage).toContain("unknown");
    expect(result.priceCoverage).toContain("unknown");
  });

  it("reports only a pinned account and refuses replacement/missing ledger instead of zeroing", async () => {
    const root = { kind: "KarsTask", resource: { namespace: "workspace", name: "mission", uid: "task-uid" },
      workspaceUid: "workspace-uid", clusterUid: "cluster-uid" };
    const account = { namespace: "accounting", name: "root-account", uid: "account-uid" };
    const task = { metadata: { ...plan.metadata, uid: "task-uid" }, status: {
      phase: "Ready", inferenceBudget: { root, account, taskUid: "task-uid" },
    } };
    const ledger = { version: "governed-inference/v1", scope: "GovernedInference", phase: "Active",
      accountUid: account.uid, limits: { tokens: 500 }, attempts: {}, meters: {
        reserved: { tokens: 40, usdMicros: 0 }, settled: { tokens: 30, usdMicros: 0 },
        uncertain: { tokens: 20, usdMicros: 0 }, unpricedAttempts: 2,
      } };
    const stored = { metadata: { uid: account.uid }, spec: { scope: "GovernedInference", root }, status: { ledger } };
    const execute = async (args: string[]) => JSON.stringify(args[1] === "karstask" ? task : stored);
    const result = await budgetStatus(execute, "task", "mission", "workspace");
    expect(result.meters).toMatchObject({ uncertain: { tokens: "20", usdMicros: "0" } });
    expect(result.priceCoverage).toContain("unknown");
    stored.metadata.uid = "replacement";
    await expect(budgetStatus(execute, "task", "mission", "workspace")).rejects.toThrow(/replaced/);
    stored.metadata.uid = account.uid;
    delete (stored.status as Record<string, unknown>).ledger;
    await expect(budgetStatus(execute, "task", "mission", "workspace")).rejects.toThrow(/missing/);
  });
});
