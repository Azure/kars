// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test, inferencePolicyCommand } from "./inferencepolicy.js";

const {
  buildInferencePolicySpecFromFlags,
  validateInferencePolicySpec,
  summarizeInferencePolicyRow,
} = __test;

describe("inferencePolicyCommand — registration", () => {
  it("registers all four sub-verbs", () => {
    const cmd = inferencePolicyCommand();
    expect(cmd.name()).toBe("inferencepolicy");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["apply", "delete", "get", "list"]);
  });

  it("apply exposes --model and --token-budget and --content-safety-severity", () => {
    const cmd = inferencePolicyCommand();
    const apply = cmd.commands.find((c) => c.name() === "apply")!;
    const flags = apply.options.map((o) => o.long);
    expect(flags).toContain("--model");
    expect(flags).toContain("--provider");
    expect(flags).toContain("--token-budget");
    expect(flags).toContain("--content-safety-severity");
    expect(flags).toContain("--from-file");
  });
});

describe("inferencePolicyCommand — buildInferencePolicySpecFromFlags", () => {
  it("builds a model preference with default provider 'azure-openai'", () => {
    const spec = buildInferencePolicySpecFromFlags({ model: "gpt-4o" });
    expect(spec.modelPreference).toEqual({
      primary: { provider: "azure-openai", deployment: "gpt-4o" },
      fallback: [],
    });
  });

  it("honours --provider override", () => {
    const spec = buildInferencePolicySpecFromFlags({ model: "claude-3", provider: "anthropic" });
    const mp = spec.modelPreference as { primary: { provider: string } };
    expect(mp.primary.provider).toBe("anthropic");
  });

  it("parses fallback entries 'provider:deployment'", () => {
    const spec = buildInferencePolicySpecFromFlags({
      model: "gpt-4o",
      fallback: ["anthropic:claude-3", "azure-openai:gpt-35"],
    });
    expect((spec.modelPreference as { fallback: unknown[] }).fallback).toEqual([
      { provider: "anthropic", deployment: "claude-3" },
      { provider: "azure-openai", deployment: "gpt-35" },
    ]);
  });

  it("rejects malformed --fallback entries", () => {
    expect(() =>
      buildInferencePolicySpecFromFlags({ model: "x", fallback: ["nope"] }),
    ).toThrow(/provider:deployment/);
  });

  it("attaches tokenBudget when --token-budget is set", () => {
    const spec = buildInferencePolicySpecFromFlags({
      model: "gpt-4o",
      tokenBudget: "1000000",
      monthlyTokens: "30000000",
    });
    expect(spec.tokenBudget).toEqual({ dailyTokens: 1000000, monthlyTokens: 30000000 });
  });

  it("attaches contentSafety when --content-safety-severity is set", () => {
    const spec = buildInferencePolicySpecFromFlags({
      model: "gpt-4o",
      contentSafetySeverity: "High",
      requirePromptShields: true,
    });
    expect(spec.contentSafety).toEqual({
      hate: "High",
      selfHarm: "High",
      sexual: "High",
      violence: "High",
      requirePromptShields: true,
    });
  });

  it("rejects an unknown severity", () => {
    expect(() =>
      buildInferencePolicySpecFromFlags({ model: "x", contentSafetySeverity: "extreme" }),
    ).toThrow(/Safe\|Low\|Medium\|High/);
  });

  it("merges --sandbox + --action into appliesTo", () => {
    const spec = buildInferencePolicySpecFromFlags({
      model: "gpt-4o",
      sandbox: "agent-1",
      action: "chat",
    });
    expect(spec.appliesTo).toEqual({ sandboxName: "agent-1", action: "chat" });
  });
});

describe("inferencePolicyCommand — validateInferencePolicySpec", () => {
  it("requires --model (or another shaping flag) when not from file", () => {
    const errs = validateInferencePolicySpec({ appliesTo: {} }, false);
    expect(errs[0]).toMatch(/missing required spec.model/);
  });

  it("accepts a token-budget-only policy as shaping", () => {
    expect(
      validateInferencePolicySpec(
        { appliesTo: {}, tokenBudget: { dailyTokens: 100 } },
        false,
      ),
    ).toEqual([]);
  });

  it("does not require shaping flags when from-file is used", () => {
    expect(validateInferencePolicySpec({ appliesTo: {} }, true)).toEqual([]);
  });

  it("requires appliesTo regardless of source", () => {
    expect(validateInferencePolicySpec({}, true).join("")).toMatch(/appliesTo/);
  });
});

describe("inferencePolicyCommand — summarizeInferencePolicyRow", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("renders provider:deployment for the MODEL column", () => {
    const row = summarizeInferencePolicyRow(
      {
        metadata: { name: "ip1", creationTimestamp: "2024-12-31T20:00:00Z" },
        spec: {
          appliesTo: { sandboxName: "agent-1" },
          modelPreference: { primary: { provider: "azure-openai", deployment: "gpt-4o" } },
          tokenBudget: { dailyTokens: 1000000 },
        },
        status: { phase: "Ready" },
      },
      NOW,
    );
    expect(row).toEqual(["ip1", "agent-1", "azure-openai:gpt-4o", "1000000", "4h", "Ready"]);
  });

  it("falls back to '-' for missing model + status", () => {
    const row = summarizeInferencePolicyRow(
      { metadata: { name: "ip2" }, spec: { appliesTo: {} } },
      NOW,
    );
    expect(row[2]).toBe("-");
    expect(row[3]).toBe("-");
    expect(row[5]).toBe("-");
  });
});
