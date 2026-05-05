// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test } from "./agent_detail.js";
import { emptyClusterState } from "../panels/types.js";

const { collectAttachments, ATTACH_CHOICES, ATTACH_FIELDS } = __test;

describe("agent_detail — collectAttachments", () => {
  it("returns empty when cluster has no CRs", () => {
    const rows = collectAttachments(emptyClusterState(), "agent-a");
    expect(rows).toEqual([]);
  });

  it("filters InferencePolicy by appliesToSandbox", () => {
    const s = emptyClusterState();
    s.inferencePolicies = [
      { name: "ip-a", namespace: "default", conditions: [], appliesToSandbox: "agent-a", modelPreference: ["gpt-5.4"] },
      { name: "ip-b", namespace: "default", conditions: [], appliesToSandbox: "agent-b" },
    ];
    const rows = collectAttachments(s, "agent-a");
    expect(rows.find((r) => r.name === "ip-a")).toBeDefined();
    expect(rows.find((r) => r.name === "ip-b")).toBeUndefined();
  });

  it("filters ClawMemory by sandboxRef", () => {
    const s = emptyClusterState();
    s.clawMemories = [
      { name: "m-a", namespace: "default", conditions: [], sandboxRef: "agent-a", storeName: "ep", scope: "agent:agent-a" },
      { name: "m-b", namespace: "default", conditions: [], sandboxRef: "agent-b", storeName: "ep", scope: "agent:agent-b" },
    ];
    const rows = collectAttachments(s, "agent-a");
    expect(rows.map((r) => r.name)).toEqual(["m-a"]);
  });

  it("filters ClawEval by sandboxRef", () => {
    const s = emptyClusterState();
    s.clawEvals = [
      { name: "e-a", namespace: "default", conditions: [], sandboxRef: "agent-a", suite: "smoke" },
      { name: "e-b", namespace: "default", conditions: [], sandboxRef: "other" },
    ];
    const rows = collectAttachments(s, "agent-a");
    expect(rows.map((r) => r.name)).toEqual(["e-a"]);
  });

  it("filters ToolPolicy by appliesToSandbox", () => {
    const s = emptyClusterState();
    s.toolPolicies = [
      { name: "tp-a", namespace: "default", conditions: [], appliesToSandbox: "agent-a", ruleCount: 4 },
      { name: "tp-b", namespace: "default", conditions: [], appliesToSandbox: "agent-b" },
    ];
    const rows = collectAttachments(s, "agent-a");
    expect(rows.map((r) => r.name)).toEqual(["tp-a"]);
  });

  it("groups by category in stable order", () => {
    const s = emptyClusterState();
    s.inferencePolicies = [
      { name: "ip", namespace: "default", conditions: [], appliesToSandbox: "a" },
    ];
    s.toolPolicies = [
      { name: "tp", namespace: "default", conditions: [], appliesToSandbox: "a" },
    ];
    s.clawMemories = [
      { name: "m", namespace: "default", conditions: [], sandboxRef: "a", storeName: "s", scope: "agent:a" },
    ];
    const rows = collectAttachments(s, "a");
    const cats = rows.map((r) => r.category);
    expect(cats[0]).toBe("Inference policy");
    expect(cats[1]).toBe("Tool policy");
    expect(cats[2]).toBe("Memory binding");
  });
});

describe("agent_detail — ATTACH_CHOICES wiring", () => {
  it("each choice has an azureclaw subcommand mapping", () => {
    const cmds = ATTACH_CHOICES.map((c) => c.cmd).sort();
    expect(cmds).toEqual(["a2a", "inferencepolicy", "mcp", "memory", "toolpolicy"]);
  });

  it("each choice has a non-empty attach field list", () => {
    for (const c of ATTACH_CHOICES) {
      const fields = ATTACH_FIELDS[c.cmd];
      expect(fields, `${c.cmd} has fields`).toBeDefined();
      expect(fields.length).toBeGreaterThan(0);
      expect(fields[0].key).toBe("name");
    }
  });

  it("memory attach prompts for store + scope", () => {
    const keys = ATTACH_FIELDS.memory.map((f) => f.key);
    expect(keys).toContain("store");
    expect(keys).toContain("scope");
  });
});
