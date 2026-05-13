// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  AuditRow,
  matchesFilters,
  parseLines,
  parseRow,
  renderPrettyLine,
  renderHeader,
  todayUtcKey,
  validateDateKey,
} from "./audit.js";

const sample: AuditRow = {
  sandbox: "test-bot",
  seq: 42,
  ts: "2026-05-13T11:22:33.456Z",
  agent_id: "agent-alpha",
  action: "tool.call.search_web",
  decision: "allowed",
  prev_hash: "0".repeat(64),
  hash: "1".repeat(64),
};

describe("audit tail — parseLines", () => {
  it("returns the default when undefined", () => {
    expect(parseLines(undefined)).toBe(50);
  });
  it("parses valid positive integers", () => {
    expect(parseLines("200")).toBe(200);
  });
  it("rejects non-numeric input", () => {
    expect(() => parseLines("nope")).toThrow(/positive integer/);
  });
  it("rejects zero", () => {
    expect(() => parseLines("0")).toThrow(/positive integer/);
  });
  it("rejects negatives", () => {
    expect(() => parseLines("-5")).toThrow(/positive integer/);
  });
  it("rejects values above the cap", () => {
    expect(() => parseLines("10001")).toThrow(/safe cap of 10000/);
  });
  it("accepts the cap exactly", () => {
    expect(parseLines("10000")).toBe(10000);
  });
});

describe("audit tail — todayUtcKey", () => {
  it("formats UTC date as YYYY-MM-DD", () => {
    const fixed = new Date(Date.UTC(2026, 4, 13, 23, 59, 59));
    expect(todayUtcKey(fixed)).toBe("2026-05-13");
  });
  it("pads single-digit month and day", () => {
    const fixed = new Date(Date.UTC(2030, 0, 7, 0, 0, 0));
    expect(todayUtcKey(fixed)).toBe("2030-01-07");
  });
});

describe("audit tail — validateDateKey", () => {
  it("accepts well-formed keys", () => {
    expect(() => validateDateKey("2026-05-13")).not.toThrow();
  });
  it("rejects shorter formats", () => {
    expect(() => validateDateKey("26-5-13")).toThrow(/YYYY-MM-DD/);
  });
  it("rejects extra text", () => {
    expect(() => validateDateKey("2026-05-13.jsonl")).toThrow();
  });
  it("rejects shell-metacharacter attempts", () => {
    expect(() => validateDateKey("2026-05-13' || rm -rf /")).toThrow();
  });
});

describe("audit tail — parseRow", () => {
  it("round-trips a valid record", () => {
    const line = JSON.stringify(sample);
    expect(parseRow(line)).toEqual(sample);
  });
  it("ignores extra fields gracefully (forward-compat)", () => {
    const line = JSON.stringify({ ...sample, future_field: "x" });
    const parsed = parseRow(line);
    expect(parsed).not.toBeNull();
    expect(parsed!.seq).toBe(sample.seq);
  });
  it("returns null on malformed JSON", () => {
    expect(parseRow("{ not json")).toBeNull();
  });
  it("returns null when fields have wrong types", () => {
    const bad = JSON.stringify({ ...sample, seq: "not a number" });
    expect(parseRow(bad)).toBeNull();
  });
  it("returns null when fields are missing", () => {
    const partial = JSON.stringify({ sandbox: "s", seq: 1 });
    expect(parseRow(partial)).toBeNull();
  });
});

describe("audit tail — matchesFilters", () => {
  it("matches by decision exactly", () => {
    expect(matchesFilters(sample, { decision: "allowed" })).toBe(true);
    expect(matchesFilters(sample, { decision: "denied" })).toBe(false);
  });
  it("matches by agent_id exactly", () => {
    expect(matchesFilters(sample, { agent: "agent-alpha" })).toBe(true);
    expect(matchesFilters(sample, { agent: "agent-beta" })).toBe(false);
  });
  it("matches by action substring", () => {
    expect(matchesFilters(sample, { action: "search_web" })).toBe(true);
    expect(matchesFilters(sample, { action: "tool.call" })).toBe(true);
    expect(matchesFilters(sample, { action: "nope" })).toBe(false);
  });
  it("combines filters via AND", () => {
    expect(
      matchesFilters(sample, {
        decision: "allowed",
        agent: "agent-alpha",
        action: "tool.call",
      })
    ).toBe(true);
    expect(
      matchesFilters(sample, {
        decision: "allowed",
        agent: "agent-alpha",
        action: "missing",
      })
    ).toBe(false);
  });
  it("passes through when no filters are set", () => {
    expect(matchesFilters(sample, {})).toBe(true);
  });
});

describe("audit tail — rendering", () => {
  it("produces a header row with the expected columns", () => {
    const header = renderHeader();
    expect(header).toContain("seq");
    expect(header).toContain("timestamp");
    expect(header).toContain("decision");
    expect(header).toContain("agent");
    expect(header).toContain("action");
  });
  it("renders a row with seq, truncated timestamp, decision, agent, action", () => {
    const line = renderPrettyLine(sample);
    expect(line).toContain("42");
    expect(line).toContain("2026-05-13T11:22:33");
    expect(line).toContain("allowed");
    expect(line).toContain("agent-alpha");
    expect(line).toContain("tool.call.search_web");
  });
  it("truncates very long agent_ids and actions", () => {
    const long: AuditRow = {
      ...sample,
      agent_id: "x".repeat(120),
      action: "y".repeat(200),
    };
    const line = renderPrettyLine(long);
    expect(line).toContain("…");
  });
});
