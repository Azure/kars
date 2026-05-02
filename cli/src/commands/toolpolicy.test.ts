// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test, toolPolicyCommand } from "./toolpolicy.js";

const { buildToolPolicySpecFromFlags, validateToolPolicySpec, summarizeToolPolicyRow } = __test;

describe("toolPolicyCommand — registration", () => {
  it("registers all four sub-verbs", () => {
    const cmd = toolPolicyCommand();
    expect(cmd.name()).toBe("toolpolicy");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["apply", "delete", "get", "list"]);
  });

  it("apply exposes the documented flags", () => {
    const cmd = toolPolicyCommand();
    const apply = cmd.commands.find((c) => c.name() === "apply")!;
    const flags = apply.options.map((o) => o.long);
    expect(flags).toContain("--from-file");
    expect(flags).toContain("--tool");
    expect(flags).toContain("--mcp-server");
    expect(flags).toContain("--rps");
    expect(flags).toContain("--daily-cap");
    expect(flags).toContain("--approval-mode");
    expect(flags).toContain("--namespace");
  });
});

describe("toolPolicyCommand — buildToolPolicySpecFromFlags", () => {
  it("emits a minimal spec with just --tool", () => {
    const spec = buildToolPolicySpecFromFlags({ tool: "*" });
    expect(spec).toEqual({ appliesTo: { tool: "*" } });
  });

  it("merges sandbox labels into appliesTo.sandboxMatchLabels", () => {
    const spec = buildToolPolicySpecFromFlags({
      tool: "fetch",
      sandboxLabel: ["env=dev", "team=infra"],
    });
    expect(spec).toEqual({
      appliesTo: {
        tool: "fetch",
        sandboxMatchLabels: { env: "dev", team: "infra" },
      },
    });
  });

  it("attaches rateLimit when any rate-limit flag is set", () => {
    const spec = buildToolPolicySpecFromFlags({
      tool: "fetch",
      rps: "10",
      burst: "20",
      window: "1m",
    });
    expect(spec.rateLimit).toEqual({ rps: 10, burst: 20, window: "1m" });
  });

  it("attaches commerce when any commerce flag is set", () => {
    const spec = buildToolPolicySpecFromFlags({
      tool: "pay",
      dailyCap: "USD 100.00",
      monthlyCap: "USD 1000.00",
      counterparty: ["did:example:bob"],
    });
    expect(spec.commerce).toEqual({
      dailyCap: "USD 100.00",
      monthlyCap: "USD 1000.00",
      counterpartyAllowlist: ["did:example:bob"],
    });
  });

  it("attaches approval when any approval flag is set", () => {
    const spec = buildToolPolicySpecFromFlags({
      tool: "pay",
      approvalMode: "always",
      approvalChannel: "telegram",
    });
    expect(spec.approval).toEqual({ mode: "always", channel: "telegram" });
  });

  it("rejects an invalid approvalMode", () => {
    expect(() =>
      buildToolPolicySpecFromFlags({ tool: "x", approvalMode: "bogus" }),
    ).toThrow(/never\|always\|aboveThreshold/);
  });

  it("rejects negative / non-integer rps", () => {
    expect(() => buildToolPolicySpecFromFlags({ tool: "x", rps: "-1" })).toThrow(/non-negative/);
    expect(() => buildToolPolicySpecFromFlags({ tool: "x", rps: "abc" })).toThrow(/non-negative/);
  });

  it("omits rateLimit/commerce/approval when no related flags are set", () => {
    const spec = buildToolPolicySpecFromFlags({ tool: "*" });
    expect(spec.rateLimit).toBeUndefined();
    expect(spec.commerce).toBeUndefined();
    expect(spec.approval).toBeUndefined();
  });

  it("preserves a displayName when provided", () => {
    const spec = buildToolPolicySpecFromFlags({ tool: "fetch", displayName: "Fetch quota" });
    expect(spec.displayName).toBe("Fetch quota");
  });
});

describe("toolPolicyCommand — validateToolPolicySpec", () => {
  it("requires appliesTo", () => {
    const errs = validateToolPolicySpec({});
    expect(errs[0]).toMatch(/spec.appliesTo/);
  });

  it("requires at least one selector inside appliesTo", () => {
    const errs = validateToolPolicySpec({ appliesTo: {} });
    expect(errs[0]).toMatch(/at least one of/);
  });

  it("accepts a tool selector", () => {
    expect(validateToolPolicySpec({ appliesTo: { tool: "fetch" } })).toEqual([]);
  });

  it("accepts a sandboxMatchLabels selector", () => {
    expect(
      validateToolPolicySpec({ appliesTo: { sandboxMatchLabels: { env: "dev" } } }),
    ).toEqual([]);
  });

  it("rejects empty sandboxMatchLabels", () => {
    const errs = validateToolPolicySpec({ appliesTo: { sandboxMatchLabels: {} } });
    expect(errs.length).toBeGreaterThan(0);
  });
});

describe("toolPolicyCommand — summarizeToolPolicyRow", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("formats a row with name, tool, dailyCap, age, phase", () => {
    const row = summarizeToolPolicyRow(
      {
        metadata: { name: "p1", creationTimestamp: "2024-12-31T23:55:00Z" },
        spec: {
          appliesTo: { tool: "fetch" },
          commerce: { dailyCap: "USD 50.00" },
        },
        status: { phase: "Ready" },
      },
      NOW,
    );
    expect(row).toEqual(["p1", "fetch", "USD 50.00", "5m", "Ready"]);
  });

  it("falls back to '*' for missing tool and '-' for missing fields", () => {
    const row = summarizeToolPolicyRow(
      { metadata: { name: "p2" }, spec: { appliesTo: {} } },
      NOW,
    );
    expect(row[0]).toBe("p2");
    expect(row[1]).toBe("*");
    expect(row[2]).toBe("-");
    expect(row[4]).toBe("-");
  });
});
