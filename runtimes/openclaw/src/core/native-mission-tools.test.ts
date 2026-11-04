// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  harness, peerFixture, peerDid, assignment, phase, webAssignment, model, scope, nativeState,
} from "./native-mission.test-support.js";
import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { PEER_TOOLS } from "./mission-peer.js";
import { authorizeTaskAction } from "./task-policy.js";

describe("pinned native Mission execution", () => {
  it("reaches all native peer adapters through observed admission, policy and outcome accounting", async () => {
    const h = harness(); const { deps, peers } = peerFixture();
    const entry = { from_amid: peerDid, from_agent: "reviewer", content: "answer", timestamp: new Date().toISOString(), id: "peer-msg" };
    h.setProgram(async () => {
      expect(h.params.disableMessageTool).toBe(true);
      await h.invoke("discover", { did: peerDid }); h.endRound(); h.start();
      await h.invoke("mesh_send", { to_agent: peerDid, content: "review this" }); h.endRound(); h.start();
      await h.invoke("mesh_inbox", {}); h.endRound(); h.start();
      deps.inbox.push(entry); deps.partition.add(entry);
      const result = await h.invoke("mesh_await", { senders: [peerDid], timeout_seconds: 1 });
      expect(JSON.stringify(result)).toContain("answer");
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {}, peers);
    expect(result.phase).toMatchObject({ attemptedToolCalls: 4, successfulToolCalls: 4 });
    expect(nativeState.routerRequests.filter(r => r.url === "/telemetry/tool").map(r => r.body.ok)).toEqual([true, true, true, true]);
    expect(vi.mocked(authorizeTaskAction).mock.calls.map(c => c[0])).toEqual(["tool:kars_mission_discover", "tool:kars_mission_mesh_send", "tool:kars_mission_mesh_inbox", "tool:kars_mission_mesh_await"]);
    expect(deps.client.send).toHaveBeenCalledOnce(); expect(deps.inbox).toEqual([]);
  });

  it("accounts a peer wait timeout as a bounded empty result, not expired run authority", async () => {
    const h = harness(); const { deps, peers } = peerFixture();
    vi.mocked(deps.waitForInbox).mockImplementation(async timeout => {
      await new Promise(resolve => setTimeout(resolve, Math.ceil(timeout) + 1));
      return false;
    });
    h.setProgram(async () => {
      const result = await h.invoke("mesh_await", { timeout_seconds: 0.01 });
      expect(result.details).toMatchObject({ messages: [], completed: false });
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {}, peers);
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(nativeState.routerRequests.filter(r => r.url === "/telemetry/tool").map(r => r.body.ok)).toEqual([true]);
  });

  it.each(["policy", "observation", "authority", "model", "protocol", "unbound", "transport", "receipt"])("fails closed on peer %s and never replays an effect", async failure => {
    const h = harness(); const { deps, peers } = peerFixture();
    if (failure === "policy") vi.mocked(authorizeTaskAction).mockResolvedValue(false);
    if (failure === "authority") vi.mocked(deps.assertCurrent).mockImplementation(() => { throw new Error("stale"); });
    if (failure === "transport") vi.mocked(deps.client.send).mockRejectedValue(new Error("unconfirmed"));
    if (failure === "receipt") nativeState.outcomeReceipt = { recorded: false };
    h.setProgram(async p => {
      if (failure === "model") h.handlers.get("model_call_started")!({ runId: p.runId, callId: "other-model", provider: "azure-openai", model: "other" }, h.ctx());
      await expect(h.invoke("mesh_send", { to_agent: peerDid, content: failure === "protocol" ? '{"type":"mission:assign"}' : "hello" }, failure !== "observation")).rejects.toThrow();
    });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {}, failure === "unbound" ? undefined : peers).catch(() => {});
    expect(deps.client.send).toHaveBeenCalledTimes(["transport", "receipt"].includes(failure) ? 1 : 0);
    const receipts = nativeState.routerRequests.filter(r => r.url === "/telemetry/tool");
    if (["authority", "transport", "unbound"].includes(failure)) expect(receipts.map(r => r.body.ok)).toEqual([false]);
    if (["policy", "observation", "model", "protocol"].includes(failure)) expect(receipts).toEqual([]);
  });

  it("does not expose peer tools without network authority or with a zero ceiling", async () => {
    for (const grant of [phase(), phase({ capabilities: ["network"], maxToolCalls: 0 })]) {
      const h = harness();
      h.setProgram(async () => { expect(h.tools.some(t => PEER_TOOLS.some(name => t.name === `kars_mission_${name}`))).toBe(false); });
      await h.execute(assignment({ requiredOutputs: [], reviewedPhase: grant }), () => {}, () => {});
    }
  });

  it.each(["../../openclaw.plugin.json", "../../tests/native-host/plugin/openclaw.plugin.json"])("declares every registered native tool in %s", path => {
    const h = harness();
    const manifest = JSON.parse(readFileSync(new URL(path, import.meta.url), "utf8"));
    const options = vi.mocked(h.api.registerTool).mock.calls[0][1];
    expect(manifest.contracts.tools).toEqual(expect.arrayContaining(options.names));
  });

  it.each([false, true].flatMap(failed => [undefined, "", "everything"].map(server => ({ failed, server }))))
  ("uses the existing router MCP client with correlated outcomes; failed=$failed scope=$server", async ({ failed, server }) => {
    nativeState.mcpFailed = failed;
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("mcp_list", server === undefined ? {} : { server });
      h.endRound(); h.start();
      const result = await h.invoke("mcp_call", { name: "everything.echo", arguments: { text: "Reviewed input" }, server: "everything" });
      expect(JSON.stringify(result)).toContain("Actual MCP result");
      h.endRound(); h.start();
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }),
      activityFormat: "router-observations-v1" }), () => {}, () => {});
    const requests = nativeState.routerRequests.filter(request => request.url === "/mcp");
    expect(requests.map(request => request.body.method)).toEqual(["initialize", "notifications/initialized", "tools/list", "tools/call"]);
    expect(requests.slice(0, 3).every(request => request.headers["x-kars-mcp-server"] === (server || undefined))).toBe(true);
    expect(requests.at(-1)?.headers["x-kars-mcp-server"]).toBe("everything");
    expect(requests[2].body.params).toEqual({});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: failed ? 1 : 2 });
    const reports = nativeState.routerRequests.filter(request => request.url === "/telemetry/tool");
    expect(reports.map(request => request.body.ok)).toEqual([true, !failed]);
    expect(vi.mocked(authorizeTaskAction).mock.calls.map(call => call[0])).toEqual([
      "tool:kars_mission_mcp_list", "tool:kars_mission_mcp_call"]);
  });

  it("does not expose MCP without native network authority", async () => {
    const h = harness();
    h.setProgram(async () => { expect(h.tools.some(tool => tool.name.includes("mcp"))).toBe(false); });
    await h.execute(assignment({ requiredOutputs: [] }), () => {}, () => {});
    expect(nativeState.routerRequests.some(request => request.url === "/mcp")).toBe(false);
  });

  it.each([
    { name: "mcp_list", args: {} }, { name: "mcp_list", args: { server: "" } },
    { name: "mcp_call", args: { name: "everything.echo", arguments: {} } },
  ])("refuses $name before effects when router tool policy denies it", async ({ name, args }) => {
    vi.mocked(authorizeTaskAction).mockResolvedValue(false);
    const h = harness();
    h.setProgram(async () => {
      await expect(h.invoke(name, args)).rejects.toThrow("denied by policy");
    });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
    expect(nativeState.routerRequests.some(request => request.url === "/mcp")).toBe(false);
  });

  it("publishes negotiated correlated tool Activity during execution and attaches the final system artifact", async () => {
    const h = harness();
    const updates: any[] = [];
    h.setProgram(async () => {
      await h.invoke("file_read", { path: "/sandbox/input" });
      expect(updates.at(-1)).toMatchObject({ correlated_tools: 1, state: "observed" });
      h.endRound(); h.start();
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "Native findings", artifact_name: "report.md" });
      expect(updates.at(-1)).toMatchObject({ correlated_tools: 2, state: "observed" });
      h.endRound(); h.start();
    });
    const result = await h.execute(assignment({ activityFormat: "router-observations-v1" }), (_evidence, content) => {
      if (content) updates.push(JSON.parse(content));
    }, () => {});
    const artifact = JSON.parse(result.artifacts!["kars-router-observations.json"]);
    expect(artifact).toEqual(updates.at(-1));
    expect(artifact).toMatchObject({ coverage: "native-tool-correlated-only", correlated_tools: 2, rounds: [1, 2] });
    expect(artifact).not.toHaveProperty("responses");
    expect(result.rounds).toBe(3);
    expect(artifact.trace.events.filter((event: any) => event.kind === "tool_result")).toHaveLength(2);
    expect(artifact.trace.events.filter((event: any) => event.kind === "tool_result").every((event: any) => event.reported_in_round === undefined)).toBe(true);
    expect(result.artifacts!["report.md"]).toBe("Actual file bytes");
  });

  it("does not turn unavailable Activity export into failed native work", async () => {
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "Native findings", artifact_name: "report.md" });
      nativeState.changeTrace = trace => ({ ...trace, has_more: true });
    });
    const result = await h.execute(assignment({ activityFormat: "router-observations-v1" }), () => {}, () => {});
    expect(result.output).toBe("Report delivered");
    expect(JSON.parse(result.artifacts!["kars-router-observations.json"])).toMatchObject({ state: "unavailable", correlated_tools: 1 });
  });

  it("binds provider execution to a current admitted call and consumes authority once", async () => {
    const h = harness();
    const provider = vi.mocked(h.api.registerWebFetchProvider).mock.calls[0][0];
    const args = { url: "https://vendor.example/pricing" };
    let tool: NonNullable<ReturnType<typeof provider.createTool>>;
    nativeState.fetchStatus = 403;
    h.setProgram(async p => {
      expect(p.config).toMatchObject({ tools: { web: { fetch: { provider: "kars-router" } } } });
      tool = provider.createTool({ config: {} })!;
      const context = { signal: p.abortSignal };
      await expect(tool.execute(args)).rejects.toThrow("admitted invocation signal");
      await expect(tool.execute(args, context)).rejects.toThrow("unique authorized invocation");
      h.nativeFetch.mockImplementation(async () => {
        await expect(tool.execute(args, { signal: new AbortController().signal })).rejects.toThrow("unique authorized invocation");
        // Native normalization may supply defaults absent from the observed proposal.
        await expect(tool.execute({ ...args, extractMode: "markdown", maxChars: 20000 }, context)).rejects.toThrow("HTTP 403");
        await expect(tool.execute(args, context)).rejects.toThrow("unique authorized invocation");
        throw new Error("actual provider denial");
      });
      await expect(h.invoke("web_fetch", args)).rejects.toThrow("actual provider denial");
      await expect(tool.execute(args, context)).rejects.toThrow("unique authorized invocation");
    });
    const result = await h.execute(webAssignment(), () => {}, () => {});
    expect(result.phase?.successfulToolCalls).toBe(0);
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toHaveLength(1);
    expect(nativeState.routerRequests.find(req => req.url === "/telemetry/tool")?.body.ok).toBe(false);
    await expect(tool!.execute(args, { signal: h.params.abortSignal })).rejects.toThrow("unique authorized invocation");
  });

  it("refuses to guess between simultaneous identical native web admissions", async () => {
    const h = harness();
    const provider = vi.mocked(h.api.registerWebFetchProvider).mock.calls[0][0];
    const args = { url: "https://vendor.example/pricing" };
    let arrivals = 0;
    let release!: () => void;
    const both = new Promise<void>(resolve => { release = resolve; });
    h.setProgram(async p => {
      const tool = provider.createTool({ config: p.config })!;
      h.nativeFetch.mockImplementation(async () => {
        if (++arrivals === 2) release();
        await both;
        await expect(tool.execute(args, { signal: p.abortSignal })).rejects.toThrow("unique authorized invocation");
        throw new Error("ambiguous admission");
      });
      const outcomes = await Promise.allSettled([h.invoke("web_fetch", args), h.invoke("web_fetch", args)]);
      expect(outcomes.every(value => value.status === "rejected")).toBe(true);
    });
    await h.execute(webAssignment(), () => {}, () => {});
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toHaveLength(0);
  });

  it("authorizes the native web tool before execution and reports its actual readable result", async () => {
    const h = harness();
    const args = { url: "https://vendor.example/pricing", extractMode: "markdown", maxChars: 12000 };
    h.nativeFetch.mockImplementation(async received => {
      expect(received).toEqual(args);
      expect(authorizeTaskAction).toHaveBeenCalledWith("tool:web_fetch", expect.objectContaining({ arguments: args }));
      expect(nativeState.routerRequests.some(req => req.url === "/telemetry/tool")).toBe(false);
      return { content: [{ type: "text", text: "Actual native extraction" }], details: {
        status: 200, text: "Actual native extraction", truncated: true, extractor: "readability",
        spill: { path: "/tmp/native-fetch.txt" },
      } };
    });
    h.setProgram(async () => {
      const fetched = await h.invoke("web_fetch", args);
      expect(fetched.details).toMatchObject({ text: "Actual native extraction", spill: { path: "/tmp/native-fetch.txt" } });
    });
    const result = await h.execute(webAssignment({ minToolCalls: 1 }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(h.params.toolsAllow).toContain("web_fetch");
    expect(h.params.toolExecutionAllow).toContain("web_fetch");
    expect(h.params.config).toMatchObject({ tools: { web: { fetch: { useTrustedEnvProxy: true } } } });
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toHaveLength(0);
    expect(nativeState.routerRequests.find(req => req.url === "/telemetry/tool")?.body).toMatchObject({ ok: true, call_id: "tool-0-0" });
  });

  it.each([
    { url: "http://vendor.example" }, { url: "https://user:secret@vendor.example" },
    { url: "https://vendor.example", headers: { Authorization: "no" } },
    { url: "https://vendor.example", extractMode: "html" },
    { url: "https://vendor.example", maxChars: 99 },
  ])("blocks malformed native web arguments before effects: %j", async args => {
    const h = harness();
    h.setProgram(async () => { await h.invoke("web_fetch", args); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/Native web_fetch/);
    expect(h.nativeFetch).not.toHaveBeenCalled();
    expect(authorizeTaskAction).not.toHaveBeenCalled();
  });

  it.each(["denied", "unavailable", "missing proposal", "wrong model"])("blocks native effects when authority is %s", async failure => {
    const h = harness();
    if (failure === "denied") vi.mocked(authorizeTaskAction).mockResolvedValue(false);
    if (failure === "unavailable") vi.mocked(authorizeTaskAction).mockRejectedValue(new Error("policy unavailable"));
    if (failure === "missing proposal") nativeState.changeTrace = trace => ({ ...trace, events: [] });
    if (failure === "wrong model") nativeState.changeTrace = trace => ({ ...trace, events: trace.events.map((e: any) => ({ ...e, model: "other" })) });
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example" }); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/policy|proposal|correlation/);
    expect(h.nativeFetch).not.toHaveBeenCalled();
  });

  it("does not expose or execute the native web tool without network capability", async () => {
    const h = harness();
    h.setProgram(async () => {
      expect(h.params.toolsAllow).not.toContain("web_fetch");
      await expect(h.invoke("web_fetch", { url: "https://vendor.example" })).rejects.toThrow(/capabilities/);
    });
    const result = await h.execute(assignment({ requiredOutputs: [] }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 0 });
    expect(h.nativeFetch).not.toHaveBeenCalled();
  });

  it("bounds native web attempts before effects", async () => {
    const h = harness();
    h.setProgram(async () => {
      await h.invoke("web_fetch", { url: "https://vendor.example/one" });
      await h.invoke("web_fetch", { url: "https://vendor.example/two" });
    });
    await expect(h.execute(webAssignment({ maxToolCalls: 1 }), () => {}, () => {})).rejects.toThrow(/maxToolCalls/);
    expect(h.nativeFetch).toHaveBeenCalledTimes(1);
  });

  it("reports a failed native fetch and lets the agent choose a retry", async () => {
    const h = harness();
    h.nativeFetch.mockRejectedValueOnce(new Error("upstream 503"));
    h.setProgram(async () => {
      await expect(h.invoke("web_fetch", { url: "https://vendor.example" })).rejects.toThrow(/503/);
      await h.invoke("web_fetch", { url: "https://vendor.example/recovery" });
    });
    const result = await h.execute(webAssignment(), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 1 });
    expect(nativeState.routerRequests.filter(req => req.url === "/telemetry/tool").map(req => req.body.ok)).toEqual([false, true]);
  });

  it("cannot claim completion without an actual native outcome receipt", async () => {
    const h = harness();
    nativeState.outcomeReceipt = { recorded: false };
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example" }); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/completion report/);
  });

  it("does not satisfy exact raw-fetch obligations with extracted native content", async () => {
    const h = harness();
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example/pricing" }); });
    await expect(h.execute(webAssignment({ requiredToolCalls: [{ name: "http_fetch", arguments: {
      method: "GET", url: "https://vendor.example/pricing",
    } }] }), () => {}, () => {})).rejects.toThrow(/Required reviewed fetch/);
  });

  it.each(["http_proxy", "https_proxy", "no_proxy"])("rejects conflicting %s before inference", async key => {
    vi.stubEnv(key, "*");
    const h = harness();
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/router proxy/);
    expect(h.api.runtime.agent.runEmbeddedAgent).not.toHaveBeenCalled();
  });

  it.each(["sessionId", "sessionKey", "runId", "toolCallId"])("rejects stale native %s before effect", async key => {
    const h = harness();
    const before = h.handlers.get("before_tool_call")!;
    h.handlers.set("before_tool_call", (event, ctx) => before(event, { ...ctx, [key]: "stale" }));
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example" }); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/authority/);
    expect(h.nativeFetch).not.toHaveBeenCalled();
  });

  it("rejects cancelled native web calls before effects", async () => {
    const h = harness();
    const before = h.handlers.get("before_tool_call")!;
    h.handlers.set("before_tool_call", (event, ctx) => before(event, { ...ctx, abortSignal: AbortSignal.abort(new Error("cancelled")) }));
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example" }); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/cancelled/);
    expect(h.nativeFetch).not.toHaveBeenCalled();
  });

  it("does not complete with an unreported native effect", async () => {
    const h = harness();
    h.handlers.set("after_tool_call", async () => {});
    h.setProgram(async () => { await h.invoke("web_fetch", { url: "https://vendor.example" }); });
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/unreportedNativeEffects/);
  });

  it("rejects missing router proxy configuration before inference", async () => {
    const h = harness();
    vi.stubEnv("HTTPS_PROXY", undefined);
    await expect(h.execute(webAssignment(), () => {}, () => {})).rejects.toThrow(/proxy environment/);
    expect(h.api.runtime.agent.runEmbeddedAgent).not.toHaveBeenCalled();
  });

  it.each([
    {}, { sessionId: "prior-native-run" }, { agentHarnessId: "another-plugin" },
    { agentRuntimeOverride: "openclaw", providerOverride: "azure-openai", modelOverride: model },
  ])("rejects existing session %j before native inference instead of adopting or rewriting it", async entry => {
    const h = harness();
    vi.mocked(h.api.runtime.agent.session.getSessionEntry).mockReturnValue(entry);
    await expect(h.execute(assignment(), () => {}, () => {})).rejects.toThrow(/session already exists/);
    expect(h.api.runtime.agent.runEmbeddedAgent).not.toHaveBeenCalled();
    expect(nativeState.routerRequests).toHaveLength(0);
  });

  it("supplies reviewed discovery locators without replacing the native objective or granting peer tools", async () => {
    const h = harness();
    const peerContext = { rootUid: "reviewed-root", stateName: `kars-reviewed-${"a".repeat(40)}`, slot: "r0-p0",
      peers: [{ role: "writer", phase: "research", slot: "r0-p0", agentName: `rp-${"a".repeat(20)}-r0-p0` }] };
    const selected = assignment({ version: 5, peerContext, requiredOutputs: [], reviewedPhase: phase() });
    await h.execute(selected, () => {}, () => {});
    expect(h.params.prompt).toContain(JSON.stringify(peerContext));
    expect(h.params.prompt).toContain(selected.content);
    expect(h.params.prompt).toContain(selected.reviewedPhase!.objective);
    expect(h.params.prompt).toContain("not proof of readiness, identity, or permission");
    expect(h.tools.some(tool => /mesh|discover/.test(tool.name))).toBe(false);
  });

  it.each([200, 403])("explains scoped GitHub App reads without adding authority and preserves HTTP %s", async status => {
    nativeState.fetchResponse = { status, headers: {}, body: status === 200 ? '{"private":true}' : "Repository access denied", truncated: false };
    const h = harness();
    const args = { method: "GET", url: "https://api.github.com/repos/example/private-repo" };
    h.setProgram(async () => {
      const tool = h.tools.find(tool => tool.name === "kars_mission_http_fetch")!;
      expect(tool.description).toContain("GitHub App connection (not MCP)");
      expect((await h.invoke("http_fetch", args)).details).toEqual(nativeState.fetchResponse);
    });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
    expect(h.params.extraSystemPrompt).toContain("use kars_mission_http_fetch with method GET");
    expect(h.params.extraSystemPrompt).toContain("native GitHub App read adapter, not an MCP server or a separate tool");
    expect(h.params.extraSystemPrompt).toContain("This guidance grants no access");
    expect(h.params.extraSystemPrompt).toContain("/branches/{branch} and /git/ref/heads/{branch} are unsupported");
    expect(h.params.extraSystemPrompt).toContain("preserve the status and limitation");
    expect(h.tools.some(tool => tool.name.includes("github_pr"))).toBe(false);
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toEqual([
      expect.objectContaining({ method: "POST", body: { ...args, telemetry: { scope_id: scope, call_id: "tool-0-0" } } }),
    ]);
    expect(nativeState.routerRequests.some(req => req.url === "/mcp" || req.url?.startsWith("/access-requests"))).toBe(false);
  });

  it("lets the native engine choose write then a new URL through the router, without a fetch script", async () => {
    const h = harness();
    const args = { method: "POST", url: "https://example.com/native-choice", headers: { Accept: "text/plain" }, body: "query" };
    h.setProgram(async () => {
      await h.invoke("file_write", { path: "/sandbox/report.md", content: "First draft" });
      const fetched = await h.invoke("http_fetch", args);
      expect(fetched.details).toMatchObject({ body: "Actual upstream text" });
    });
    const result = await h.execute(assignment({ reviewedPhase: phase({ executionMode: "native", capabilities: ["filesystem-read", "filesystem-write", "network"] }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 2, successfulToolCalls: 2 });
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toEqual([
      expect.objectContaining({ url: "/egress/fetch", method: "POST", body: { ...args,
        telemetry: { scope_id: scope, call_id: "tool-1-0" } } }),
    ]);
    const reports = nativeState.routerRequests.filter(req => req.url === "/telemetry/tool");
    expect(reports).toHaveLength(2);
    expect(reports[1].body).toMatchObject({ scope_id: scope, call_id: "tool-1-0", ok: true });
    expect(reports[1].body).not.toHaveProperty("result_summary");
    expect(h.params.extraSystemPrompt).not.toContain("Next, call");
  });

  it.each([
    { status: 200, body: "x".repeat(70000), truncated: false },
    { status: 204, body: "", truncated: false },
    { status: 206, body: "partial", truncated: false },
    { status: 200, body: "x".repeat(2 * 1024 * 1024) + "... [truncated at 2097152 bytes]", truncated: true },
  ])("returns native HTTP $status content without a 64 KiB whole-page requirement", async response => {
    nativeState.fetchResponse = { headers: {}, ...response };
    const h = harness();
    h.setProgram(async () => {
      const fetched = await h.invoke("http_fetch", { method: "GET", url: "https://vendor.example/data" });
      expect(fetched.details).toEqual(nativeState.fetchResponse);
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"], minToolCalls: 1 }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
  });

  it.each([301, 403, 503])("returns upstream HTTP %i details without inventing a successful operation", async status => {
    nativeState.fetchResponse = { status, headers: { location: "https://vendor.example/new" }, body: "Upstream explanation", truncated: false };
    const h = harness();
    h.setProgram(async () => {
      expect((await h.invoke("http_fetch", { method: "GET", url: "https://vendor.example/data" })).details).toEqual(nativeState.fetchResponse);
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 0 });
    expect(nativeState.routerRequests.find(req => req.url === "/telemetry/tool")?.body).toMatchObject({ ok: false,
      result_summary: { kind: "failure", reason: "tool_execution_failed" } });
  });

  it("uses a canonical public GitHub GET through governed fetch without local URLs or credentials", async () => {
    const args = { method: "GET", url: "https://api.github.com/repos/owner/repo/issues" };
    nativeState.fetchResponse = { status: 200, headers: { "cache-control": "no-store" }, body: '[{"number":42}]', truncated: false,
      observed_at: "2026-10-31T08:30:00.123Z" };
    const h = harness();
    h.setProgram(async () => { expect((await h.invoke("http_fetch", args)).details).toEqual(nativeState.fetchResponse); });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"],
      requiredToolCalls: [{ name: "http_fetch", arguments: args }] }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 1 });
    expect(nativeState.routerRequests.filter(req => req.url === "/egress/fetch")).toEqual([
      expect.objectContaining({ method: "POST", body: { ...args, telemetry: { scope_id: scope, call_id: "tool-0-0" } } }),
    ]);
  });

  it("exposes partial native content without satisfying an explicit complete-fetch obligation", async () => {
    const args = { method: "GET", url: "https://vendor.example/data" };
    nativeState.fetchResponse = { status: 200, headers: {}, body: "partial", truncated: true };
    const h = harness();
    h.setProgram(async () => { expect((await h.invoke("http_fetch", args)).details).toEqual(nativeState.fetchResponse); });
    await expect(h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"],
      requiredToolCalls: [{ name: "http_fetch", arguments: args }] }) }), () => {}, () => {})).rejects.toThrow(/Required reviewed fetch/);
  });

  it.each([
    { status: 600 }, { status: "200" }, { truncated: undefined }, { headers: null },
    { body: "x".repeat(2 * 1024 * 1024 + 1) },
  ])("rejects malformed or oversized native fetch envelopes", async invalid => {
    nativeState.fetchResponse = { ...nativeState.fetchResponse, ...invalid };
    const h = harness();
    h.setProgram(async () => {
      await expect(h.invoke("http_fetch", { method: "GET", url: "https://vendor.example/data" })).rejects.toThrow(/Invalid bounded/);
    });
    const result = await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
    expect(result.phase).toMatchObject({ attemptedToolCalls: 1, successfulToolCalls: 0 });
  });

  it("keeps router refusal distinct from an upstream HTTP response", async () => {
    nativeState.fetchStatus = 403;
    const h = harness();
    h.setProgram(async () => {
      expect(await h.invoke("http_fetch", { method: "GET", url: "https://vendor.example/data" })).toMatchObject({
        isError: true, details: { http_status: 403, error: expect.any(String) } });
    });
    await h.execute(assignment({ requiredOutputs: [], reviewedPhase: phase({ capabilities: ["network"] }) }), () => {}, () => {});
    expect(nativeState.routerRequests.find(req => req.url === "/telemetry/tool")?.body.ok).toBe(false);
  });
});
