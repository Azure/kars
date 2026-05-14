// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";

import { __test } from "./eval.js";

const { summarizeEvalRow, renderEvalShow, renderEvalDiff } = __test;

describe("summarizeEvalRow", () => {
  it("renders a row for a builtin-corpus, never-run eval", () => {
    const row = summarizeEvalRow({
      metadata: { name: "agt-baseline", creationTimestamp: new Date(Date.now() - 5 * 60 * 1000).toISOString() },
      spec: {
        targetSandboxRef: { name: "prod-bot" },
        corpus: { builtin: "jailbreak-baseline" },
      },
      status: { phase: "Pending" },
    });
    expect(row[0]).toBe("agt-baseline");
    expect(row[1]).toBe("prod-bot");
    expect(row[2]).toBe("builtin:jailbreak-baseline");
    expect(row[3]).toBe("Pending");
    expect(row[5]).toBe("—");
  });

  it("renders a bundle-corpus eval with completed run summary", () => {
    const row = summarizeEvalRow({
      metadata: { name: "memo-eval", creationTimestamp: new Date(Date.now() - 60 * 60 * 1000).toISOString() },
      spec: {
        targetSandboxRef: { name: "agent-x" },
        corpus: {
          bundleRef: { registry: "r", repository: "p", digest: "sha256:abcdef0123456789ab" },
        },
      },
      status: {
        phase: "Ready",
        lastResult: { totalCases: 12, passedCases: 11, failedCases: 1, drift: false },
      },
    });
    expect(row[2]).toContain("bundle:sha256:");
    expect(row[3]).toBe("Ready");
    expect(row[5]).toBe("11/12");
  });

  it("flags drift in the last-run summary", () => {
    const row = summarizeEvalRow({
      metadata: { name: "x" },
      spec: { targetSandboxRef: { name: "s" }, corpus: { builtin: "banned-tools" } },
      status: {
        phase: "Degraded",
        lastResult: { totalCases: 5, passedCases: 3, failedCases: 2, drift: true },
      },
    });
    expect(row[5]).toBe("3/5 ⚠ drift");
  });

  it("falls back gracefully when fields are missing", () => {
    const row = summarizeEvalRow({});
    expect(row[0]).toBe("<unknown>");
    expect(row[1]).toBe("—");
    expect(row[2]).toBe("—");
    expect(row[3]).toBe("Pending");
    expect(row[5]).toBe("—");
  });
});

describe("renderEvalShow", () => {
  it("renders spec + status + conditions", () => {
    const out = renderEvalShow({
      metadata: { name: "ev1", namespace: "azureclaw-system" },
      spec: {
        targetSandboxRef: { name: "bot" },
        corpus: { builtin: "prompt-injection-2026q1" },
        schedule: "0 */6 * * *",
        failSandboxOnDrift: true,
      },
      status: {
        phase: "Ready",
        observedGeneration: 3,
        conditions: [
          { type: "Ready", status: "True", reason: "AllPassed" },
          { type: "ConformanceDrift", status: "False", reason: "AllPassed" },
        ],
      },
    });
    expect(out).toContain("ClawEval/ev1");
    expect(out).toContain("target sandbox:      bot");
    expect(out).toContain("builtin:prompt-injection-2026q1");
    expect(out).toContain("schedule:            0 */6 * * *");
    expect(out).toContain("failSandboxOnDrift:  true");
    expect(out).toContain("Ready");
    expect(out).toContain("AllPassed");
  });

  it("indicates on-demand only when schedule is unset", () => {
    const out = renderEvalShow({
      metadata: { name: "ev2" },
      spec: { targetSandboxRef: { name: "s" }, corpus: { builtin: "egress-known-bad" } },
      status: { phase: "Pending" },
    });
    expect(out).toContain("(on-demand only)");
  });

  it("surfaces last-run failures with details", () => {
    const out = renderEvalShow({
      metadata: { name: "ev3" },
      spec: { targetSandboxRef: { name: "s" }, corpus: { builtin: "memory-isolation" } },
      status: {
        phase: "Degraded",
        lastResult: {
          corpusLabel: "builtin:memory-isolation",
          totalCases: 3,
          passedCases: 1,
          failedCases: 2,
          drift: true,
          cases: [
            { caseId: "mi-1", scenario: "MemoryRead", outcome: "Pass" },
            { caseId: "mi-2", scenario: "MemoryRead", outcome: "Fail", failureKind: "UnexpectedAllow", detail: "expected Blocked" },
            { caseId: "mi-3", scenario: "MemoryRead", outcome: "Fail", failureKind: "ReasonMismatch" },
          ],
        },
      },
    });
    expect(out).toContain("drift:               YES");
    expect(out).toContain("Failures");
    expect(out).toContain("mi-2");
    expect(out).toContain("UnexpectedAllow");
    expect(out).toContain("mi-3");
  });

  it("truncates failure list at 10 with an overflow marker", () => {
    const cases = Array.from({ length: 14 }, (_, i) => ({
      caseId: `c-${i}`,
      scenario: "ToolCall",
      outcome: "Fail",
      failureKind: "Mismatch",
    }));
    const out = renderEvalShow({
      metadata: { name: "ev4" },
      spec: { targetSandboxRef: { name: "s" }, corpus: { builtin: "banned-tools" } },
      status: {
        phase: "Degraded",
        lastResult: { totalCases: 14, passedCases: 0, failedCases: 14, cases },
      },
    });
    expect(out).toContain("c-0");
    expect(out).toContain("c-9");
    expect(out).not.toContain("c-10");
    expect(out).toContain("and 4 more");
  });
});

describe("renderEvalDiff", () => {
  it("flags regressions and fixes", () => {
    const out = renderEvalDiff(
      {
        startedAt: "2026-01-01T00:00:00Z",
        totalCases: 3,
        passedCases: 2,
        cases: [
          { caseId: "c1", scenario: "ToolCall", outcome: "Pass" },
          { caseId: "c2", scenario: "ToolCall", outcome: "Pass" },
          { caseId: "c3", scenario: "MemoryRead", outcome: "Fail", failureKind: "Mismatch" },
        ],
      },
      {
        startedAt: "2026-01-02T00:00:00Z",
        totalCases: 3,
        passedCases: 2,
        cases: [
          { caseId: "c1", scenario: "ToolCall", outcome: "Pass" },
          { caseId: "c2", scenario: "ToolCall", outcome: "Fail", failureKind: "Mismatch" },
          { caseId: "c3", scenario: "MemoryRead", outcome: "Pass" },
        ],
      },
    );
    expect(out).toContain("Regressions (1)");
    expect(out).toContain("c2");
    expect(out).toContain("Fixes (1)");
    expect(out).toContain("c3");
  });

  it("flags added/dropped cases", () => {
    const out = renderEvalDiff(
      { totalCases: 1, passedCases: 1, cases: [{ caseId: "a", outcome: "Pass" }] },
      { totalCases: 1, passedCases: 1, cases: [{ caseId: "b", outcome: "Pass" }] },
    );
    expect(out).toContain("Added cases (1): b");
    expect(out).toContain("Dropped cases (1): a");
  });

  it("flags drift transition", () => {
    const out = renderEvalDiff(
      { drift: false, totalCases: 1, passedCases: 1 },
      { drift: true, totalCases: 1, passedCases: 0 },
    );
    expect(out).toContain("drift:");
    expect(out).toContain("YES");
  });

  it("reports no per-case differences cleanly", () => {
    const out = renderEvalDiff(
      { totalCases: 2, passedCases: 2, cases: [{ caseId: "x", outcome: "Pass" }, { caseId: "y", outcome: "Pass" }] },
      { totalCases: 2, passedCases: 2, cases: [{ caseId: "x", outcome: "Pass" }, { caseId: "y", outcome: "Pass" }] },
    );
    expect(out).toContain("No per-case differences");
  });
});
