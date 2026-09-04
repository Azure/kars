// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test } from "./approval.js";

const { defaultDecider, formatList } = __test;

describe("approval — defaultDecider", () => {
  it("uses the explicit --by when given", () => {
    expect(defaultDecider("alice@example.com")).toBe("alice@example.com");
  });

  it("trims whitespace and falls back to the OS user when blank", () => {
    expect(defaultDecider("  bob ")).toBe("bob");
    // Blank → some non-empty username (OS-dependent, just assert non-empty).
    expect(defaultDecider("   ").length).toBeGreaterThan(0);
    expect(defaultDecider(undefined).length).toBeGreaterThan(0);
  });
});

describe("approval — formatList", () => {
  it("renders an empty state", () => {
    expect(formatList([])).toContain("No approvals");
  });

  it("renders task, action, and decision metadata", () => {
    const out = formatList([
      {
        metadata: { name: "raise-tier", namespace: "kars-system" },
        spec: {
          taskRef: { name: "migrate" },
          action: { kind: "tierRaise", summary: "raise to tier 4" },
        },
        status: { phase: "Approved", decider: "alice", decidedAt: "2026-06-26T10:00:00Z" },
      },
    ]);
    expect(out).toContain("raise-tier");
    expect(out).toContain("migrate");
    expect(out).toContain("tierRaise");
    expect(out).toContain("alice");
  });
});
