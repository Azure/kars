// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { TaskPhaseGuard } from "./task-phase.js";
import { REVIEWED_MEMORY_SCHEMA } from "./foundry-memory.js";

const phase = (overrides: Record<string, unknown> = {}) => ({
  name: "briefing", objective: "Produce the reviewed briefing.", maxToolCalls: 32,
  capabilities: ["filesystem-read", "filesystem-write"], ...overrides,
});

describe("required fetch accounting", () => {
  const args = { method: "GET", url: "https://www.example.com/first" };
  const second = { ...args, url: "https://www.example.com/second" };
  const reviewed = () => phase({ capabilities: ["filesystem-read", "filesystem-write", "network"], minToolCalls: 1,
    requiredToolCalls: [args, second].map(arguments_ => ({ name: "http_fetch", arguments: arguments_ })) });
  it("requires each exact verified call, not retries or filesystem successes", () => {
    const guard = new TaskPhaseGuard(reviewed());
    guard.reserveBatch(4);
    guard.recordSuccess("file_read", { path: "/sandbox/input" });
    expect(() => guard.finish()).toThrow("Required reviewed fetch");
    expect(guard.argumentError("file_write", { path: "/sandbox/report", content: "draft" })).toContain("Complete required");
    guard.recordSuccess("http_fetch", args);
    guard.recordSuccess("http_fetch", args);
    expect(guard.snapshot().completedRequiredToolCalls).toBe(1);
    expect(guard.pendingRequiredCalls()).toEqual([{ name: "http_fetch", arguments: second }]);
    expect(() => guard.finish()).toThrow("Required reviewed fetch");
    guard.recordSuccess("http_fetch", second);
    expect(guard.snapshot().completedRequiredToolCalls).toBe(2);
    expect(guard.argumentError("file_write", { path: "/sandbox/report", content: "draft" })).toBeUndefined();
    guard.finish();
  });
  it("does not credit failed, unreviewed or unreserved fetches or expose mutable authority", () => {
    const guard = new TaskPhaseGuard(reviewed());
    expect(() => guard.recordSuccess("http_fetch", args)).toThrow("no reserved attempt");
    guard.reserveBatch(1);
    guard.pendingRequiredCalls()[0].arguments.url = "https://www.example.com/changed";
    expect(guard.argumentError("http_fetch", args)).toBeUndefined();
    for (const changed of [{ ...args, headers: {} }, { ...args, method: "POST" }, { ...args, url: "https://www.example.com/changed" }]) {
      expect(guard.argumentError("http_fetch", changed)).toBeTruthy();
      expect(() => guard.recordSuccess("http_fetch", changed)).toThrow("Unreviewed fetch");
    }
    expect(guard.snapshot()).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 0, completedRequiredToolCalls: 0 });
    expect(() => guard.finish()).toThrow("Required reviewed fetch");
  });
});

describe("native capability phases", () => {
  it("accepts omitted or empty MCP list scope without granting invocation arguments or capabilities", () => {
    const guard = new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["network"] }));
    for (const args of [{}, { server: "" }, { server: "everything" }]) {
      expect(guard.argumentError("mcp_list", args)).toBeUndefined();
    }
    for (const args of [null, [], { name: "everything.echo" }, { arguments: {} },
      ...[null, false, [], " ", "../other", "a".repeat(254)].map(server => ({ server }))]) {
      expect(guard.argumentError("mcp_list", args)).toContain("MCP listing accepts only");
    }
    expect(guard.argumentError("mcp_call", { name: "everything.echo", server: "" })).toBeTruthy();
    expect(guard.argumentError("mcp_call", {})).toBeTruthy();
    expect(guard.argumentError("mcp_call", { name: "everything.echo", arguments: [] })).toBeTruthy();
    expect(guard.argumentError("mcp_call", { name: "everything.echo", arguments: {} })).toBeUndefined();
    expect(new TaskPhaseGuard(phase({ executionMode: "native" })).argumentError("mcp_list", { server: "" }))
      .toContain("outside the reviewed phase");
  });
  it("grants memory independently from network and preserves exact router-owned scopes", () => {
    expect(REVIEWED_MEMORY_SCHEMA.properties.scope.type).toEqual(["string", "null"]);
    expect(REVIEWED_MEMORY_SCHEMA.required).toEqual(["operation"]);
    const guard = new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["memory"] }));
    expect(guard.allowsTool("foundry_memory")).toBe(true);
    for (const name of ["http_fetch", "mcp_call", "file_read"]) expect(guard.allowsTool(name)).toBe(false);
    expect(new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["network"] })).allowsTool("foundry_memory")).toBe(false);
    expect(new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["memory"], maxToolCalls: 0 })).allowsTool("foundry_memory")).toBe(false);
    for (const args of [{ operation: "search" }, { operation: "search", scope: null }, { operation: "search", query: "", top_k: 1 },
      { operation: "update", text: "observe-only first", scope: "Exact-Scope" }]) {
      expect(guard.argumentError("foundry_memory", args)).toBeUndefined();
    }
    expect(() => new TaskPhaseGuard(phase({ capabilities: ["memory"] }))).toThrow();
  });
  it.each([{}, { operation: "delete_scope" }, { operation: "update", text: " " },
    { operation: "search", text: null }, { operation: "search", query: 1 }, { operation: "search", scope: " " },
    { operation: "search", top_k: 0 }, { operation: "search", top_k: 1.5 },
    { operation: "search", top_k: Number.MAX_SAFE_INTEGER + 1 }, { operation: "search", store_name: "other" }])
  ("rejects unreviewed memory arguments %j", args => {
    const guard = new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["memory"] }));
    expect(guard.argumentError("foundry_memory", args)).toContain("Reviewed memory requires");
  });
  it("requires explicit native network authority for every ordinary peer tool", () => {
    for (const name of ["mesh_send", "discover", "mesh_inbox", "mesh_await"]) {
      expect(new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["network"] })).allowsTool(name)).toBe(true);
      expect(new TaskPhaseGuard(phase({ capabilities: ["network"], requiredToolCalls: [
        { name: "http_fetch", arguments: { method: "GET", url: "https://example.com/reviewed" } },
      ] })).allowsTool(name)).toBe(false);
      expect(new TaskPhaseGuard(phase({ executionMode: "native" })).allowsTool(name)).toBe(false);
      expect(new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["network"], maxToolCalls: 0 })).allowsTool(name)).toBe(false);
    }
  });
  it("allows new router-bound requests without imposing a required sequence", () => {
    const guard = new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["filesystem-read", "filesystem-write", "network"] }));
    expect(guard.native).toBe(true);
    expect(guard.argumentError("file_write", { path: "/sandbox/report.md", content: "First draft" })).toBeUndefined();
    expect(guard.argumentError("http_fetch", { method: "POST", url: "https://example.com/chosen", headers: { Accept: "text/plain" }, body: "query" })).toBeUndefined();
    guard.reserveBatch(1);
    guard.recordSuccess("http_fetch", { method: "POST", url: "https://example.com/chosen" });
    guard.finish();
    expect(guard.snapshot()).not.toHaveProperty("completedRequiredToolCalls");
  });
  it("keeps explicit exact calls as obligations without blocking an earlier write", () => {
    const required = { method: "GET", url: "https://example.com/required" };
    const guard = new TaskPhaseGuard(phase({ executionMode: "native", capabilities: ["filesystem-write", "network"],
      requiredToolCalls: [{ name: "http_fetch", arguments: required }] }));
    expect(guard.argumentError("file_write", { path: "/sandbox/report.md", content: "draft" })).toBeUndefined();
    guard.reserveBatch(3);
    guard.recordSuccess("http_fetch", { ...required, url: "https://example.com/unlisted" });
    guard.recordSuccess("http_fetch", { ...required, headers: {} });
    expect(guard.snapshot().completedRequiredToolCalls).toBe(0);
    expect(() => guard.finish()).toThrow("Required reviewed fetch");
    guard.recordSuccess("http_fetch", required);
    expect(guard.snapshot().completedRequiredToolCalls).toBe(1);
    guard.finish();
  });
});

describe("filesystem phase boundaries", () => {
  it("allows digest copy only in native phases with both filesystem capabilities", () => {
    const args = { path: "/sandbox/out.py", source_path: "/sandbox/.kars-inputs/source.py", sha256: `sha256:${"a".repeat(64)}` };
    const native = new TaskPhaseGuard(phase({ executionMode: "native" }));
    expect(native.argumentError("file_write", args)).toBeUndefined();
    expect(native.argumentError("file_write", { ...args, content: null })).toBeUndefined();
    expect(native.argumentError("file_write", { path: args.path, content: "still supported" })).toBeUndefined();
    expect(native.argumentError("file_write", { path: args.path, content: "still supported", source_path: null, sha256: null })).toBeUndefined();
    expect(native.argumentError("file_write", { path: args.path, content: null, source_path: null, sha256: null })).toBeTruthy();
    for (const changed of [{ ...args, content: "mixed" }, { ...args, sha256: null }, { ...args, sha256: "a".repeat(64) },
      { ...args, source_path: "/etc/passwd" }, { ...args, source_path: "/sandbox/../../etc/passwd" },
      { path: args.path, source_path: args.source_path }, { path: args.path, sha256: args.sha256 }]) {
      expect(native.argumentError("file_write", changed)).toBeTruthy();
    }
    expect(new TaskPhaseGuard(phase()).argumentError("file_write", args)).toBeTruthy();
    for (const capabilities of [["filesystem-write"], ["filesystem-read"]]) {
      expect(new TaskPhaseGuard(phase({ executionMode: "native", capabilities })).argumentError("file_write", args)).toBeTruthy();
    }
  });
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
