// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { harness, assignment, scope, phase, nativeState } from "./native-mission.test-support.js";
import { describe, expect, it, vi } from "vitest";
import { authorizeTaskAction } from "./task-policy.js";
import { callFoundryMemory } from "./foundry-memory.js";
import { nativeGithubPr } from "./native-github-pr.js";

describe("native human review continuation", () => {
  const requestArgs = { operation: "request", kind: "clarification", target: "source-choice", reason: "Which source should I use?" };
  it("waits for a scoped answer and continues in the same measured session", async () => {
    const h = harness();
    h.setProgram(async () => {
      const answer = await h.invoke("access_request", requestArgs);
      expect(answer.details).toMatchObject({ request: nativeState.accessDecision, enforcement_changed: false });
      h.endRound(); h.start();
      await h.invoke("file_write", { path: "/sandbox/report.md", content: (answer.details as any).request.answer, artifact_name: "report.md" });
    });
    const result = await h.execute(assignment(), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 2 });
    expect(result.rounds).toBe(2);
    expect(nativeState.routerRequests.find(req => req.url === "/v1/access-request")?.body).toEqual({ ...requestArgs,
      operation: undefined, scope_id: scope });
    expect(nativeState.routerRequests.find(req => req.url?.includes("/wait?"))?.url).toContain(`scope_id=${scope}`);
    expect(nativeState.routerEvents).toContainEqual(expect.objectContaining({ kind: "tool_result", name: "kars_mission_access_request", ok: true }));
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_access_request", expect.objectContaining({ scope_id: scope, tool_call_id: "tool-0-0" }));
  });
  it.each(["denied", "cancelled", "expired"])("returns %s decisions without granting permission or losing continuation", async status => {
    nativeState.accessDecision = { ...nativeState.accessDecision, status, answer: undefined };
    const h = harness();
    h.setProgram(async () => {
      expect((await h.invoke("access_request", requestArgs)).details).toMatchObject({ request: { status }, enforcement_changed: false });
    });
    await h.execute(assignment({ requiredOutputs: [] }), () => {}, () => {});
  });
  it("preserves the request identity on timeout and does not count a failed wait as success", async () => {
    nativeState.accessStatus = 408; nativeState.accessDecision = { error: "wait_timed_out" };
    const h = harness();
    h.setProgram(async () => {
      expect(await h.invoke("access_request", requestArgs)).toMatchObject({ isError: true, details: {
        error: "wait_timed_out", request_id: "request-1", scope_id: scope, enforcement_changed: false } });
    });
    const result = await h.execute(assignment({ requiredOutputs: [] }), () => {}, () => {});
    expect(result.phase?.successfulToolCalls).toBe(0);
  });
  it("binds fetch approval waits to observed scope and preserves denial request details", async () => {
    const h = harness();
    const args = { method: "GET", url: "https://vendor.example/research", wait_for_approval_ms: 300000 };
    nativeState.fetchStatus = 403; nativeState.fetchResponse = { error: "approval_pending", request: { request_id: "egress-1", scope_id: scope }, enforcement_changed: false };
    h.setProgram(async () => {
      expect(await h.invoke("http_fetch", args)).toMatchObject({ isError: true, details: { ...nativeState.fetchResponse, http_status: 403 } });
      h.endRound(); h.start();
      nativeState.fetchStatus = 200; nativeState.fetchResponse = { status: 200, headers: {}, body: "Verified source", truncated: false };
      expect(await h.invoke("http_fetch", args)).toMatchObject({ isError: false });
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"],
      requiredToolCalls: [{ name: "http_fetch", arguments: { method: args.method, url: args.url } }] }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ successfulToolCalls: 1, completedRequiredToolCalls: 1 });
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch").map(req => req.body)).toEqual([
      { ...args, scope_id: scope, telemetry: { scope_id: scope, call_id: "tool-0-0" } },
      { ...args, scope_id: scope, telemetry: { scope_id: scope, call_id: "tool-0-1" } },
    ]);
  });
});

describe("reviewed native GitHub publication", () => {
  const args = { operation: "create", repository: "owner/repo", path: "README.md", expected_base: "a".repeat(40),
    content: "Reviewed docs", title: "Docs", body: "One file" };
  const reviewed = () => assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["github-pr"] }) });
  it("exposes explicit authority and retains the observed upstream result and outcome receipt", async () => {
    const h = harness();
    const upstream = { http_status: 201, ambiguous: false, response: { html_url: "https://github.com/owner/repo/pull/17" } };
    vi.mocked(nativeGithubPr).mockResolvedValue(upstream);
    h.setProgram(async p => {
      expect(p.toolsAllow).toEqual(["kars_mission_access_request", "kars_mission_github_pr"]);
      expect(p.extraSystemPrompt).toContain("never represent a local file as a published PR");
      expect((await h.invoke("github_pr", args)).details).toEqual(upstream);
    });
    const result = await h.execute(reviewed(), () => {}, () => {});
    expect(result.phase?.successfulToolCalls).toBe(1);
    expect(nativeGithubPr).toHaveBeenCalledWith(args, expect.objectContaining({ taskUid: "task-uid", assignmentId: "assignment-1" }), expect.any(AbortSignal));
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_github_pr", expect.objectContaining({ arguments: args, scope_id: scope }));
    expect(nativeState.routerEvents.find(e => e.kind === "tool_result")).toMatchObject({ name: "kars_mission_github_pr", ok: true });
  });
  it("does not expose publication through network or filesystem grants", async () => {
    const h = harness();
    h.setProgram(async p => { expect(p.toolsAllow).not.toContain("kars_mission_github_pr"); });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network", "filesystem-write"] }) }), () => {}, () => {});
    expect(nativeGithubPr).not.toHaveBeenCalled();
  });
  it.each(["denied", "malformed", "unobserved"])("denies %s publication before helper dispatch", async failure => {
    if (failure === "denied") vi.mocked(authorizeTaskAction).mockResolvedValue(false);
    const h = harness();
    h.setProgram(async () => { await expect(h.invoke("github_pr", failure === "malformed" ? { ...args, branch: "main" } : args, failure !== "unobserved")).rejects.toThrow(); });
    const execution = h.execute(reviewed(), () => {}, () => {});
    if (failure === "unobserved") await expect(execution).rejects.toThrow("tool calls do not match observed attempts");
    else await execution;
    expect(nativeGithubPr).not.toHaveBeenCalled();
  });
  it("retains ambiguous errors as failed evidence without automatic replay", async () => {
    const h = harness();
    const failed = { error: "Response lost", ambiguous: true, replay_forbidden: true, branch: "kars/reviewed/id" };
    vi.mocked(nativeGithubPr).mockResolvedValue(failed);
    h.setProgram(async () => { expect(await h.invoke("github_pr", args)).toMatchObject({ isError: true, details: failed }); });
    const result = await h.execute(reviewed(), () => {}, () => {});
    expect(result.phase?.successfulToolCalls).toBe(0);
    expect(nativeGithubPr).toHaveBeenCalledTimes(1);
    expect(nativeState.routerEvents.find(e => e.kind === "tool_result")).toMatchObject({ ok: false });
  });
  it("halts further side effects after activity publication fails even if the host catches the tool error", async () => {
    const h = harness();
    vi.mocked(nativeGithubPr).mockResolvedValue({ http_status: 201 });
    const outcomes: PromiseSettledResult<unknown>[] = [];
    h.setProgram(async () => {
      outcomes.push(...await Promise.allSettled([h.invoke("github_pr", args)]));
      outcomes.push(...await Promise.allSettled([h.invoke("github_pr", args)]));
    });
    await expect(h.execute({ ...reviewed(), activityFormat: "router-observations-v1" }, (_evidence, activity) => {
      if (activity !== undefined) throw new Error("Activity validation failed");
    }, () => {})).rejects.toThrow("Activity validation failed");
    expect(outcomes).toHaveLength(2);
    expect(outcomes.every(outcome => outcome.status === "rejected")).toBe(true);
    expect(nativeGithubPr).toHaveBeenCalledTimes(1);
    expect(nativeState.routerEvents.find(e => e.kind === "tool_result")).toMatchObject({ ok: true });
  });
  it("halts on missing publication outcome receipt rather than replaying", async () => {
    const h = harness();
    vi.mocked(nativeGithubPr).mockResolvedValue({ http_status: 201 });
    nativeState.outcomeReceipt = { recorded: false };
    h.setProgram(async () => { await h.invoke("github_pr", args); });
    await expect(h.execute(reviewed(), () => {}, () => {})).rejects.toThrow();
    expect(nativeGithubPr).toHaveBeenCalledTimes(1);
  });
});

describe("governed native memory", () => {
  const memoryAssignment = () => assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["memory"] }) });
  it("exposes memory without network and binds its actual router call to observed authorization", async () => {
    const h = harness();
    const args = { operation: "search", query: "pilot", scope: "Exact-Scope", top_k: 3 };
    h.setProgram(async p => {
      expect(p.toolsAllow).toEqual(["kars_mission_access_request", "kars_mission_foundry_memory"]);
      expect(p.extraSystemPrompt).toContain("queued/in_progress is not persisted");
      const result = await h.invoke("foundry_memory", args);
      expect(result.details).toEqual(nativeState.memoryResponse);
    });
    await h.execute(memoryAssignment(), () => {}, () => {});
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_foundry_memory", expect.objectContaining({
      arguments: args, task_uid: "task-uid", assignment_id: "assignment-1", scope_id: scope,
    }));
    expect(nativeState.routerRequests.filter(r => r.url === "/platform/mcp")).toMatchObject([{
      headers: { accept: "application/json, text/event-stream" },
      body: { method: "tools/call", params: { name: "foundry.memory", arguments: args } },
    }]);
    expect(nativeState.routerEvents.find(e => e.kind === "tool_result")).toMatchObject({ name: "kars_mission_foundry_memory", ok: true });
  });
  it.each([{}, { scope: null }])("resolves the private binding without inventing a scope %j", async scopeArgs => {
    const h = harness();
    const args = { operation: "search", query: "pilot", ...scopeArgs };
    h.setProgram(async () => { await h.invoke("foundry_memory", args); });
    await h.execute(memoryAssignment(), () => {}, () => {});
    const requests = nativeState.routerRequests.filter(r => r.url === "/platform/mcp");
    expect(requests).toHaveLength(1);
    expect(requests[0].body.params.arguments).toEqual({ operation: "search", query: "pilot" });
    expect(args).toEqual({ operation: "search", query: "pilot", ...scopeArgs });
    expect(authorizeTaskAction).toHaveBeenCalledWith("tool:kars_mission_foundry_memory", expect.objectContaining({ arguments: args }));
  });
  it("does not expose memory from network authority", async () => {
    const h = harness();
    h.setProgram(async p => { expect(p.toolsAllow).not.toContain("kars_mission_foundry_memory"); });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
  });
  it.each(["queued", "in_progress", "completed"])("preserves actual update status %s without replay", async status => {
    nativeState.memoryResponse = { status, update_id: "update-1" };
    const h = harness();
    h.setProgram(async () => {
      expect((await h.invoke("foundry_memory", { operation: "update", text: "Observe-only first" })).details)
        .toEqual(nativeState.memoryResponse);
    });
    await h.execute(memoryAssignment(), () => {}, () => {});
    expect(nativeState.routerRequests.filter(r => r.url === "/platform/mcp")).toHaveLength(1);
  });
  it.each(["denied", "malformed", "unobserved"])("prevents %s memory calls before transport", async failure => {
    if (failure === "denied") vi.mocked(authorizeTaskAction).mockResolvedValue(false);
    const h = harness();
    h.setProgram(async () => {
      await expect(h.invoke("foundry_memory", { operation: failure === "malformed" ? "delete_scope" : "search" }, failure !== "unobserved"))
        .rejects.toThrow();
    });
    const execution = h.execute(memoryAssignment(), () => {}, () => {});
    if (failure === "unobserved") await expect(execution).rejects.toThrow("tool calls do not match observed attempts");
    else await execution;
    expect(nativeState.routerRequests.filter(r => r.url === "/platform/mcp")).toHaveLength(0);
  });
  it.each(["http", "platform", "invalid", "failed-update"])("retains %s memory failure and does not replay", async failure => {
    if (failure === "http") nativeState.memoryHttpStatus = 403;
    if (failure === "platform") nativeState.memoryError = true;
    if (failure === "invalid") nativeState.memoryResponse = [];
    if (failure === "failed-update") nativeState.memoryResponse = { status: "failed" };
    const h = harness();
    h.setProgram(async () => {
      await expect(h.invoke("foundry_memory", { operation: "update", text: "Observe-only first" })).rejects.toThrow();
    });
    await h.execute(memoryAssignment(), () => {}, () => {});
    expect(nativeState.routerRequests.filter(r => r.url === "/platform/mcp")).toHaveLength(1);
    expect(nativeState.routerEvents.find(e => e.kind === "tool_result")).toMatchObject({ name: "kars_mission_foundry_memory", ok: false });
  });
  it("cancels in-flight platform transport without retry", async () => {
    const controller = new AbortController();
    const started = new Promise<void>(resolve => { nativeState.memoryRequestStarted = resolve; });
    const pending = callFoundryMemory({ operation: "search" }, controller.signal);
    const rejected = expect(pending).rejects.toThrow(/abort/i);
    await started;
    controller.abort();
    await rejected;
    expect(nativeState.routerRequests.filter(r => r.url === "/platform/mcp")).toHaveLength(1);
  });
});
