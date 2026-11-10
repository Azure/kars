// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { accessRequest, registerAccessRequestTool } from "./access-request.js";
import { TaskPhaseGuard } from "../task-phase.js";

const scope = "scope-1";
const entry = { request_id: "request-1", scope_id: scope, kind: "clarification", status: "pending" };
const args = { operation: "request", kind: "clarification", target: "question-1", reason: "Which report?" };
const exchangeFor = (result: unknown) => vi.fn(async (_method: string, path: string) => path === "/v1/access-requests"
  ? { scope: { id: scope }, requests: [] } : path === "/v1/access-request"
    ? { status: "queued", request: entry, enforcement_changed: false } : result);

describe("scoped access request client", () => {
  it("registers the human review tool", () => {
    const registerTool = vi.fn(); registerAccessRequestTool({ registerTool });
    expect(registerTool.mock.calls[0][0].name).toBe("kars_access_request");
  });
  it("queues once then waits using the issued scope without changing enforcement", async () => {
    const exchange = exchangeFor({ ...entry, status: "approved", answer: "Report A" });
    const result = await accessRequest(args, { exchange });
    expect(result).toMatchObject({ request: { answer: "Report A" }, enforcement_changed: false });
    expect(exchange.mock.calls.map(call => call[1])).toEqual(["/v1/access-requests", "/v1/access-request",
      "/v1/access-requests/request-1/wait?scope_id=scope-1&timeout_ms=300000"]);
  });
  it("does not wait when explicitly queued only", async () => {
    const exchange = exchangeFor({});
    expect(await accessRequest({ ...args, timeout_ms: 0 }, { exchange, scope })).toMatchObject({ status: "queued", request: entry });
    expect(exchange).toHaveBeenCalledTimes(1);
  });
  it.each(["", " ", "🔥".repeat(2049), undefined])("rejects an invalid approved answer", async answer => {
    const result = await accessRequest(args, { scope, exchange: exchangeFor({ ...entry, status: "approved", answer }) });
    expect(result.error).toContain("no valid answer");
  });
  it("accepts an answer at the UTF-8 bound", async () => {
    const answer = "🔥".repeat(2048);
    expect((await accessRequest(args, { scope, exchange: exchangeFor({ ...entry, status: "approved", answer }) })).request.answer).toBe(answer);
  });
  it.each(["wait_timed_out", "stale_scope", "request_expired", "request_not_found"])("keeps correlation on %s", async error => {
    const result = await accessRequest(args, { scope, exchange: exchangeFor({ error }) });
    expect(result).toMatchObject({ error, request_id: entry.request_id, scope_id: scope, enforcement_changed: false });
  });
  it.each([undefined, scope])("uses active scope and only egress fields for the observed request (bound=%s)", async bound => {
    const exchange = exchangeFor({ ...entry, kind: "egress", status: "approved" });
    const observed = { operation: "request", kind: "egress", target: "example.net", reason: "Review page access",
      tier: 1, port: 443, scope_id: "", request_id: "", timeout_ms: 300000 };
    expect(await accessRequest(observed, { exchange, scope: bound })).toMatchObject({ enforcement_changed: false });
    expect(exchange).toHaveBeenCalledWith("POST", "/v1/access-request",
      { kind: "egress", target: "example.net", reason: "Review page access", port: 443, scope_id: scope },
      15000, { "x-kars-service-scope": scope }, undefined, { maxBytes: 4 * 1024 * 1024, structuredErrors: true });
    expect(observed.scope_id).toBe(""); expect(observed.tier).toBe(1);
  });
  it.each(["request", "list", "wait", "cancel"])("rejects a nonempty stale scope for %s", async operation => {
    const exchange = exchangeFor({});
    await expect(accessRequest({ ...args, operation, scope_id: "old", request_id: entry.request_id }, { scope, exchange })).rejects.toThrow("stale_scope");
    expect(exchange).not.toHaveBeenCalled();
  });
  it.each(["wait", "cancel"].flatMap(operation => ["scope_id", "request_id"].map(field => [operation, field])))
    ("never rebinds %s with empty %s", async (operation, field) => {
      const exchange = exchangeFor({});
      await expect(accessRequest({ operation, scope_id: scope, request_id: entry.request_id, [field]: "" }, { scope, exchange }))
        .rejects.toThrow("original scope_id and request_id");
      expect(exchange).not.toHaveBeenCalled();
    });
  it.each([null, false, 0, " ", "scope/other"])("rejects invalid supplied scope %s", async supplied => {
    const exchange = exchangeFor({});
    await expect(accessRequest({ ...args, scope_id: supplied }, { scope, exchange })).rejects.toThrow("scope_id");
    expect(exchange).not.toHaveBeenCalled();
  });
  it("lists with the active scope when the optional ID is empty", async () => {
    const exchange = exchangeFor({});
    await accessRequest({ operation: "list", scope_id: "" }, { scope, exchange });
    expect(exchange).toHaveBeenCalledExactlyOnceWith("GET", "/v1/access-requests", undefined,
      15000, { "x-kars-service-scope": scope }, undefined, { maxBytes: 4 * 1024 * 1024, structuredErrors: true });
  });
  it.each(["clarification", "tool", "skill", "mcp", "command", "permission", "tier"])("only sends applicable authority fields for %s", async kind => {
    const exchange = vi.fn(async (_method: string, _path: string, body?: unknown) => {
      expect(body).toEqual({ kind, target: kind === "tier" ? "" : "resource", reason: args.reason, scope_id: scope,
        ...(kind === "tier" ? { tier: 5 } : {}) });
      return { status: "queued", request: entry };
    });
    await accessRequest({ ...args, kind, target: kind === "tier" ? "" : "resource", tier: 5, port: 443, timeout_ms: 0 }, { scope, exchange });
  });
  it("never binds a resumed request to a newer assignment", async () => {
    const exchange = exchangeFor({});
    await expect(accessRequest({ operation: "wait", request_id: entry.request_id, scope_id: "old" }, { scope, exchange })).rejects.toThrow("stale_scope");
    expect(exchange).not.toHaveBeenCalled();
  });
  it("cancels with the original scope and surfaces cancellation failure", async () => {
    const exchange = exchangeFor({ error: "request_terminal" });
    expect(await accessRequest({ operation: "cancel", request_id: entry.request_id, scope_id: scope }, { exchange })).toMatchObject({ error: "request_terminal", enforcement_changed: false });
    expect(exchange.mock.calls[0].slice(0, 2)).toEqual(["POST", "/v1/access-requests/request-1/cancel"]);
  });
  it("retains cancellation identity across transport failure", async () => {
    const result = await accessRequest({ operation: "cancel", request_id: entry.request_id, scope_id: scope },
      { exchange: vi.fn(async () => { throw new Error("timeout"); }) });
    expect(result).toMatchObject({ error: "Error: timeout", request_id: entry.request_id, scope_id: scope, enforcement_changed: false });
  });
  it("does not turn tier approval into an autonomy grant", async () => {
    const exchange = exchangeFor({ ...entry, kind: "tier", status: "approved", tier: 5 });
    expect(await accessRequest({ operation: "request", kind: "tier", tier: 5, reason: "Need more autonomy" }, { scope, exchange }))
      .toMatchObject({ request: { kind: "tier", status: "approved" }, enforcement_changed: false });
  });
  it("honors cancellation before dispatch", async () => {
    const exchange = exchangeFor({});
    const controller = new AbortController(); controller.abort();
    await expect(accessRequest(args, { scope, exchange, signal: controller.signal })).rejects.toThrow();
    expect(exchange).not.toHaveBeenCalled();
  });
  it("keeps historical reviewed phases unchanged and does not grant network", () => {
    const phase = { name: "review", objective: "Review the supplied documents", maxToolCalls: 5, capabilities: [] };
    expect(new TaskPhaseGuard(phase).allowsTool("access_request")).toBe(false);
    const native = new TaskPhaseGuard({ ...phase, executionMode: "native" });
    expect(native.allowsTool("access_request")).toBe(true);
    expect(native.allowsTool("http_fetch")).toBe(false);
  });
});
