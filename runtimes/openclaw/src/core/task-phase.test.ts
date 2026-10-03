// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { TaskPhaseGuard } from "./task-phase.js";

const phase = (overrides: Record<string, unknown> = {}) => ({
  name: "briefing", objective: "Produce the reviewed briefing.", maxToolCalls: 32,
  capabilities: ["filesystem-read", "filesystem-write"], ...overrides,
});

describe("filesystem phase boundaries", () => {
  it("accepts canonical omitted defaults and exact name, UTF8 and call boundaries", () => {
    const guard = new TaskPhaseGuard(phase({ name: "a".repeat(48), objective: "é".repeat(600) }));
    expect(guard.snapshot()).toMatchObject({ minToolCalls: 0, maxToolCalls: 32 });
    expect(guard.instructions()).toContain('"freshContext":false');
    guard.reserveBatch(32);
    for (let i = 0; i < 32; i++) guard.recordSuccess();
    guard.finish();
    expect(() => guard.reserveBatch(1)).toThrow("exceeds maxToolCalls");
    expect(() => guard.recordSuccess()).toThrow("no reserved attempt");
    expect(guard.snapshot()).toMatchObject({ attemptedToolCalls: 32, successfulToolCalls: 32 });
  });

  it.each([0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 33])("does not consume invalid or overflowing batch %s", count => {
    const guard = new TaskPhaseGuard(phase());
    expect(() => guard.reserveBatch(count)).toThrow();
    expect(guard.snapshot().attemptedToolCalls).toBe(0);
  });

  it("does not expose mutable accounting or retain caller-owned capabilities", () => {
    const input = phase();
    const guard = new TaskPhaseGuard(input);
    input.capabilities.splice(0);
    input.maxToolCalls = 0;
    const snapshot = guard.snapshot();
    snapshot.attemptedToolCalls = 32;
    snapshot.successfulToolCalls = 32;
    snapshot.maxToolCalls = 0;
    expect(guard.allowsTool("file_write")).toBe(true);
    expect(guard.snapshot()).toEqual({ name: "briefing", attemptedToolCalls: 0, successfulToolCalls: 0, minToolCalls: 0, maxToolCalls: 32 });
    expect(() => guard.recordSuccess()).toThrow();
  });

  it.each([undefined, null, 1, 16 * 1024 * 1024])("accepts supported read size %s", max_bytes => {
    expect(new TaskPhaseGuard(phase()).argumentError("file_read", { path: "/tmp/evidence", max_bytes })).toBeUndefined();
  });

  it.each([0, -1, 1.5, "1", Number.NaN, Number.POSITIVE_INFINITY, 16 * 1024 * 1024 + 1])("rejects unsupported read size %s", max_bytes => {
    expect(new TaskPhaseGuard(phase()).argumentError("file_read", { path: "/tmp/evidence", max_bytes })).toContain("max_bytes");
  });

  it.each(["/tmp", "/sandbox", "/tmp-other/evidence", "/sandbox-other/evidence", "/tmp/../etc/evidence", "relative.txt"])("rejects path outside allowed directory prefixes: %s", path => {
    expect(new TaskPhaseGuard(phase()).argumentError("file_read", { path })).toContain("path must resolve");
  });
});
