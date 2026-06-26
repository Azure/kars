// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test, memoryCommand } from "./memory.js";

const { buildMemorySpecFromFlags, validateMemorySpec, summarizeMemoryRow } = __test;

describe("memoryCommand — registration", () => {
  it("registers all four sub-verbs at top level", () => {
    const cmd = memoryCommand();
    expect(cmd.name()).toBe("memory");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["apply", "delete", "get", "list"]);
  });

  it("apply exposes --sandbox, --store, --scope, --retention-days, --from-file", () => {
    const cmd = memoryCommand();
    const apply = cmd.commands.find((c) => c.name() === "apply")!;
    const flags = apply.options.map((o) => o.long);
    expect(flags).toContain("--sandbox");
    expect(flags).toContain("--store");
    expect(flags).toContain("--scope");
    expect(flags).toContain("--retention-days");
    expect(flags).toContain("--from-file");
    expect(flags).toContain("--no-delete-on-sandbox-delete");
  });
});

describe("memoryCommand — buildMemorySpecFromFlags", () => {
  it("emits sandboxRef.name + storeName + scope from flags", () => {
    const spec = buildMemorySpecFromFlags({ sandbox: "my-agent", store: "episodic", scope: "agent_my-agent" });
    expect(spec).toEqual({
      sandboxRef: { name: "my-agent" },
      storeName: "episodic",
      scope: "agent_my-agent",
    });
  });

  it("attaches retentionDays + displayName when set", () => {
    const spec = buildMemorySpecFromFlags({
      sandbox: "a", store: "s", scope: "x", retentionDays: 30, displayName: "30-day episodic",
    });
    expect(spec.retentionDays).toBe(30);
    expect(spec.displayName).toBe("30-day episodic");
  });

  it("emits deleteOnSandboxDelete=false only when --no-delete-on-sandbox-delete passed", () => {
    const off = buildMemorySpecFromFlags({ sandbox: "a", store: "s", scope: "x", noDeleteOnSandboxDelete: true });
    expect(off.deleteOnSandboxDelete).toBe(false);
    const on = buildMemorySpecFromFlags({ sandbox: "a", store: "s", scope: "x" });
    expect("deleteOnSandboxDelete" in on).toBe(false);
  });
});

describe("memoryCommand — validateMemorySpec", () => {
  const ok = { sandboxRef: { name: "a" }, storeName: "s", scope: "agent_a" };

  it("accepts a minimal valid spec", () => {
    expect(validateMemorySpec(ok)).toEqual([]);
  });

  it("requires sandboxRef.name", () => {
    const errs = validateMemorySpec({ storeName: "s", scope: "x" });
    expect(errs.join(" ")).toMatch(/sandboxRef\.name/);
  });

  it("requires storeName as DNS-label", () => {
    const errs = validateMemorySpec({ ...ok, storeName: "Bad_Name" });
    expect(errs.join(" ")).toMatch(/DNS-label/);
  });

  it("rejects empty scope", () => {
    const errs = validateMemorySpec({ ...ok, scope: "" });
    expect(errs.join(" ")).toMatch(/spec\.scope/);
  });

  it("rejects a scope with a colon (Foundry charset)", () => {
    const errs = validateMemorySpec({ ...ok, scope: "agent:a" });
    expect(errs.join(" ")).toMatch(/colons are rejected|may only contain/);
  });

  it("accepts a scope using the valid charset separators", () => {
    expect(validateMemorySpec({ ...ok, scope: "session_1-a.b@c/d" })).toEqual([]);
  });

  it("rejects non-positive retentionDays", () => {
    const errs = validateMemorySpec({ ...ok, retentionDays: 0 });
    expect(errs.join(" ")).toMatch(/retentionDays/);
    const errs2 = validateMemorySpec({ ...ok, retentionDays: -3 });
    expect(errs2.join(" ")).toMatch(/retentionDays/);
  });

  it("rejects oversize storeName (>63)", () => {
    const errs = validateMemorySpec({ ...ok, storeName: "a".repeat(64) });
    expect(errs.join(" ")).toMatch(/DNS-label/);
  });

  it("accepts retentionDays as positive integer", () => {
    expect(validateMemorySpec({ ...ok, retentionDays: 30 })).toEqual([]);
  });
});

describe("memoryCommand — summarizeMemoryRow", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("renders name, sandbox, store, scope, retention, age, phase", () => {
    const row = summarizeMemoryRow(
      {
        metadata: { name: "mem1", creationTimestamp: "2024-12-25T00:00:00Z" },
        spec: { sandboxRef: { name: "a" }, storeName: "episodic", scope: "agent_a", retentionDays: 30 },
        status: { phase: "Bound" },
      },
      NOW,
    );
    expect(row).toEqual(["mem1", "a", "episodic", "agent_a", "30d", "7d", "Bound"]);
  });

  it("renders '-' for missing retentionDays", () => {
    const row = summarizeMemoryRow(
      {
        metadata: { name: "mem2" },
        spec: { sandboxRef: { name: "b" }, storeName: "s", scope: "x" },
      },
      NOW,
    );
    expect(row[4]).toBe("-");
  });
});
