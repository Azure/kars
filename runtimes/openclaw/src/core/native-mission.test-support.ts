// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, expect, vi } from "vitest";
import { createServer, type Server, type IncomingHttpHeaders } from "node:http";
import type { MissionAssignment } from "@kars/mesh/dist/mission-protocol.js";
import type { NativeMissionApi, NativeRunParams, NativeRunResult, NativeTool, NativeToolFactory } from "./native-mission-api.js";
import { createNativeMissionExecutor } from "./native-mission.js";
import { createMissionPeers, type MissionPeerDependencies } from "./mission-peer.js";
import { readTaskFile, writeTaskFile } from "./task-filesystem.js";
import { authorizeTaskAction } from "./task-policy.js";
import { resetMcpBridgeForTests } from "./agt-tools/mcp-bridge.js";

vi.mock("./native-python.js", async importOriginal => ({
  ...await importOriginal<typeof import("./native-python.js")>(), nativePython: vi.fn(),
}));

vi.mock("./native-github-pr.js", async importOriginal => ({
  ...await importOriginal<typeof import("./native-github-pr.js")>(), nativeGithubPr: vi.fn(),
}));

vi.mock("node:http", async importOriginal => {
  const actual = await importOriginal<typeof import("node:http")>();
  const request = actual.request as (...args: any[]) => ReturnType<typeof actual.request>;
  return { ...actual, request: (...args: any[]) => {
    if (args[0]?.path === "/mcp") {
      expect(args[0].hostname).toBe("127.0.0.1");
      expect(args[0].port).toBe(8443);
      args[0] = { ...args[0], port: new URL(process.env.KARS_ROUTER_URL!).port };
    }
    return request(...args);
  } };
});

vi.mock("./task-policy.js", () => ({ authorizeTaskAction: vi.fn(async () => true) }));

vi.mock("./task-filesystem.js", async importOriginal => ({
  ...await importOriginal<typeof import("./task-filesystem.js")>(),
  readTaskFile: vi.fn(async () => ({ content: "Actual file bytes", truncated: false })),
  writeTaskFile: vi.fn(async (_path: string, content: string) => Buffer.byteLength(content)),
}));

export const model = "reviewed-model";

export const scope = "router-issued-scope";

export const phase = (overrides = {}) => ({ name: "research", objective: "Deliver the reviewed report", executionMode: "native",
  capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 0, maxToolCalls: 8, freshContext: false, requiredToolCalls: [], ...overrides });

export function assignment(overrides = {}): MissionAssignment {
  return { type: "mission:assign", version: 3, taskName: "mission", taskUid: "task-uid", sandboxUid: "sandbox-uid",
    podUid: "pod-uid", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
    runNonce: "run-1", assignmentId: "assignment-1", bootId: "boot-1", content: "Produce the report",
    artifactFormat: "text-v1", requiredOutputs: ["report.md"], reviewedPhase: phase(), ...overrides } as MissionAssignment;
}

export function harness() {
  const handlers = new Map<string, (event: any, ctx: any) => any>();
  const nativeFetch = vi.fn(async (_args: Record<string, unknown>) => ({ content: [{ type: "text", text: "Readable page" }],
    details: { status: 200, text: "Readable page", truncated: false, extractor: "readability" } }));
  let factory: NativeToolFactory;
  let params: NativeRunParams;
  let transcript: any[] = [];
  let tools: NativeTool[] = [];
  let calls = 0;
  let started = 0;
  let toolsInRound: any[] = [];
  let program: (p: NativeRunParams) => Promise<void> = async () => {};
  let editResult = (_result: NativeRunResult) => {};
  let editMessages = (_messages: any[]) => {};
  const ctx = () => ({ runId: params.runId, sessionKey: params.sessionKey, sessionId: params.sessionId });
  const current = vi.fn(() => params.abortSignal.throwIfAborted());
  const start = () => handlers.get("model_call_started")!({ runId: params.runId, callId: `model-${++started}`, provider: "azure-openai", model }, ctx());
  const endRound = (stopReason = "toolUse") => {
    transcript.push({ role: "assistant", provider: "azure-openai", model, stopReason, timestamp: calls + 1,
      content: toolsInRound, usage: { input: 10, output: 5, cacheRead: 2, cacheWrite: 3, totalTokens: 20 } });
    toolsInRound = [];
    calls++;
  };
  const retry = (retainHistory = true, edit = (_messages: any[]) => {}) => {
    endRound("error");
    edit(transcript);
    handlers.get("agent_end")!({ runId: params.runId, messages: transcript, success: false }, ctx());
    if (!retainHistory) transcript = [];
    started = 0;
    start();
  };
  const revise = (edit = (_messages: any[]) => {}, context = ctx()) => {
    endRound("stop");
    edit(transcript);
    handlers.get("before_agent_finalize")!({ runId: params.runId, sessionId: params.sessionId, messages: transcript }, context);
    transcript = [];
    started = 0;
    start();
  };
  const invoke = async (name: string, args: Record<string, unknown>, observe = true) => {
    const id = `tool-${toolsInRound.length}-${calls}`;
    const toolName = name === "web_fetch" ? name : `kars_mission_${name}`;
    toolsInRound.push({ type: "toolCall", id, name: toolName, arguments: args });
    if (!nativeState.routerEvents.some(event => event.kind === "round" && event.round === calls + 1)) {
      nativeState.routerEvents.push({ kind: "round", scope_id: scope, seq: ++nativeState.sequence, round: calls + 1,
        provider: "azure-openai", model, accepted: true, source: "router-upstream", http_status: 200,
        outcome: "response_observed", usage_state: "present", partial_observation: false, ms: 1, tool_calls_observed: 1 });
    }
    nativeState.routerEvents.push({ kind: "tool_proposed", scope_id: scope, seq: ++nativeState.sequence, round: calls + 1,
      call_id: id, name: toolName, source: "model-proposed" });
    if (observe) params.onAgentEvent({ stream: "tool", data: { phase: "start", name: toolName, toolCallId: id, args } });
    if (name === "web_fetch") {
      const event = { runId: params.runId, toolCallId: id, toolName, params: args };
      const context = { ...ctx(), toolCallId: id, toolName, abortSignal: params.abortSignal };
      const decision = await handlers.get("before_tool_call")!(event, context);
      if (decision?.block) throw new Error(decision.blockReason);
      try {
        const result = await nativeFetch(args);
        await handlers.get("after_tool_call")!({ ...event, result }, context);
        return result;
      } catch (error) {
        await handlers.get("after_tool_call")!({ ...event, error: String(error) }, context);
        throw error;
      }
    }
    return tools.find(tool => tool.name === toolName)!.execute(id, args, params.abortSignal);
  };
  const api: NativeMissionApi = {
    id: "kars", registerWebFetchProvider: vi.fn(),
    runtime: { version: "2026.9.8", config: { current: () => ({ tools: { toolSearch: false }, models: { providers: { "azure-openai": {
      baseUrl: `${process.env.KARS_ROUTER_URL}/v1`, models: [{ id: model, contextWindow: 1_050_000, contextTokens: 1_050_000, maxTokens: 128_000 }],
    } } } }) }, agent: {
      resolveAgentDir: () => "/sandbox/agent", resolveAgentWorkspaceDir: () => "/sandbox/workspace",
      session: { getSessionEntry: vi.fn(() => undefined) },
      runEmbeddedAgent: vi.fn(async p => {
        params = p; transcript = []; toolsInRound = []; calls = 0; started = 0;
        tools = factory.create({ ...ctx(), toolBindings: p.toolBindings, activeModel: { provider: "azure-openai", modelId: model }, assertInvocationCurrent: current })!;
        start();
        await program(p);
        endRound("stop");
        editMessages(transcript);
        handlers.get("agent_end")!({ runId: p.runId, messages: transcript, success: true }, ctx());
        const result: NativeRunResult = { meta: { agentMeta: { sessionId: p.sessionId, provider: "azure-openai", model, assistantTurns: calls } }, payloads: [{ text: "Report delivered" }] };
        editResult(result);
        return result;
      }),
    } },
    registerTool: vi.fn((value: NativeToolFactory) => { factory = value; }),
    on: ((name: string, handler: any) => { handlers.set(name, handler); }) as NativeMissionApi["on"],
  };
  const execute = createNativeMissionExecutor(api);
  return { api, execute, invoke, nativeFetch, start, endRound, retry, revise, current, handlers, ctx, get factory() { return factory; }, get tools() { return tools; }, get params() { return params; },
    setProgram: (value: typeof program) => { program = value; }, setResult: (value: typeof editResult) => { editResult = value; },
    setMessages: (value: typeof editMessages) => { editMessages = value; } };
}

beforeEach(async () => {
  vi.clearAllMocks();
  resetMcpBridgeForTests();
  nativeState.mcpFailed = false;
  nativeState.memoryResponse = { memories: [{ id: "memory-1", text: "Observe-only first" }] };
  nativeState.memoryHttpStatus = 200;
  nativeState.memoryError = false;
  nativeState.memoryRequestStarted = undefined;
  vi.stubEnv("OPENCLAW_MODEL", model);
  vi.stubEnv("HTTPS_PROXY", "http://127.0.0.1:8444");
  vi.stubEnv("HTTP_PROXY", "http://127.0.0.1:8444");
  vi.stubEnv("NO_PROXY", "127.0.0.1,localhost");
  vi.stubEnv("KARS_SKILL_CATALOG_DIGEST", undefined);
  vi.mocked(authorizeTaskAction).mockResolvedValue(true);
  vi.mocked(readTaskFile).mockResolvedValue({ path: "/sandbox/input", bytes: 17, returned_bytes: 17, content: "Actual file bytes", truncated: false });
  vi.mocked(writeTaskFile).mockImplementation(async (_path, content) => Buffer.byteLength(content));
  nativeState.routerEvents = []; nativeState.routerRequests = []; nativeState.sequence = 0; nativeState.cursorResponse = undefined;
  nativeState.changeTrace = trace => trace;
  nativeState.outcomeReceipt = { recorded: true, source: "harness-reported", enforcement_changed: false };
  nativeState.fetchResponse = { status: 200, headers: {}, body: "Actual upstream text", truncated: false };
  nativeState.fetchStatus = 200;
  nativeState.accessDecision = { scope_id: scope, request_id: "request-1", kind: "clarification", status: "approved", answer: "Use the internal report." };
  nativeState.accessStatus = 200;
  nativeState.modelMetadataStatus = 200;
  nativeState.modelMetadataRaw = undefined;
  nativeState.modelMetadata = { object: "list", data: [{ id: model, context_window: 1_050_000, max_output_tokens: 128_000 }] };
  nativeState.server = createServer((req, res) => {
    let text = "";
    req.on("data", chunk => { text += chunk; });
    req.on("end", () => {
      nativeState.routerRequests.push({ url: req.url, method: req.method, headers: req.headers, ...(text ? { body: JSON.parse(text) } : {}) });
      const url = new URL(req.url!, "http://router.invalid");
      if (url.pathname === "/v1/access-request") {
        res.writeHead(202, { "content-type": "application/json" });
        res.end(JSON.stringify({ status: "queued", request: { ...JSON.parse(text), request_id: "request-1", status: "pending" }, enforcement_changed: false }));
        return;
      }
      if (url.pathname === "/v1/access-requests/request-1/wait") {
        res.writeHead(nativeState.accessStatus, { "content-type": "application/json" });
        res.end(JSON.stringify(nativeState.accessDecision));
        return;
      }
      if (url.pathname === "/v1/models") {
        res.writeHead(nativeState.modelMetadataStatus, { "content-type": "application/json" });
        res.end(nativeState.modelMetadataRaw ?? JSON.stringify(nativeState.modelMetadata));
        return;
      }
      if (url.pathname === "/platform/mcp") {
        if (nativeState.memoryRequestStarted) { nativeState.memoryRequestStarted(); return; }
        const body = JSON.parse(text);
        res.writeHead(nativeState.memoryHttpStatus, { "content-type": "application/json" });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: body.id,
          result: { content: [{ type: "text", text: JSON.stringify(nativeState.memoryResponse) }], isError: nativeState.memoryError } }));
        return;
      }
      if (url.pathname === "/mcp") {
        const body = JSON.parse(text);
        if (body.method === "notifications/initialized") { res.writeHead(202).end(); return; }
        const result = body.method === "initialize" ? { protocolVersion: "2025-06-18", capabilities: {} }
          : body.method === "tools/list" ? { tools: [{ name: "everything.echo" }] }
          : { content: [{ type: "text", text: "Actual MCP result" }], isError: nativeState.mcpFailed };
        res.writeHead(200, { "content-type": "application/json", "mcp-session-id": "native-mcp" });
        res.end(JSON.stringify({ jsonrpc: "2.0", id: body.id, result }));
        return;
      }
      if (url.pathname === "/telemetry/trace" && req.headers["x-kars-service-scope"] !== scope) {
        res.writeHead(409, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "stale_scope" }));
        return;
      }
      if (url.pathname === "/telemetry/tool" && nativeState.outcomeReceipt.recorded === true) {
        const body = JSON.parse(text);
        const proposal = nativeState.routerEvents.find(event => event.kind === "tool_proposed" && event.call_id === body.call_id);
        if (proposal) nativeState.routerEvents.push({ kind: "tool_result", scope_id: scope, seq: ++nativeState.sequence, round: proposal.round,
          source: "harness-reported", call_id: body.call_id, name: proposal.name, ok: body.ok,
          ms: body.latency_ms, ...(body.result_summary ? { result_summary: body.result_summary } : {}) });
      }
      const response = url.pathname === "/telemetry/cursor" ? nativeState.cursorResponse ?? { scope_id: scope, cursor: nativeState.sequence, durable: false }
        : url.pathname === "/telemetry/trace" ? nativeState.changeTrace({ scope_id: scope, durable: false, has_more: false,
          cursor: nativeState.sequence, high_water: nativeState.sequence, coverage: "router-observed-only", dropped_events: 0,
          events: nativeState.routerEvents.filter(event => (event.seq as number) > Number(url.searchParams.get("since"))) })
        : url.pathname === "/telemetry/tool" ? nativeState.outcomeReceipt
        : url.pathname === "/agt/evaluate" ? { allowed: true }
        : nativeState.fetchResponse;
      res.writeHead(url.pathname === "/egress/fetch" ? nativeState.fetchStatus : 200, { "content-type": "application/json" });
      res.end(JSON.stringify(response));
    });
  });
  await new Promise<void>(resolve => nativeState.server.listen(0, "127.0.0.1", resolve));
  const address = nativeState.server.address();
  if (!address || typeof address === "string") throw new Error("Missing test listener");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
});

afterEach(async () => {
  nativeState.server.closeAllConnections();
  await new Promise<void>((resolve, reject) => nativeState.server.close(error => error ? reject(error) : resolve()));
  vi.unstubAllEnvs();
});

export const peerDid = `did:mesh:${"c".repeat(32)}`;

export function peerFixture() {
    const deps: MissionPeerDependencies = {
      client: { supportsScopedOperations: true, send: vi.fn(async () => {}), isPlaintextPeer: () => false },
      sandboxName: "writer", registry: { provider: "agt", search: vi.fn(async () => []), lookup: vi.fn(async () => ({ amid: peerDid, last_seen: new Date().toISOString() })) },
      inbox: [], partition: new WeakSet(), assertCurrent: vi.fn(), waitForInbox: vi.fn(async () => false), markRead: vi.fn(),
    };
    return { deps, peers: createMissionPeers(deps) };
  }

export const webAssignment = (overrides = {}) => assignment({ requiredOutputs: [],
    reviewedPhase: phase({ capabilities: ["network"], ...overrides }) });

export const nativeState = {} as {
  server: Server;
  routerEvents: Record<string, unknown>[];
  routerRequests: { url?: string; method?: string; headers: IncomingHttpHeaders; body?: any }[];
  cursorResponse: Record<string, unknown> | undefined;
  outcomeReceipt: Record<string, unknown>;
  fetchResponse: Record<string, unknown>;
  fetchStatus: number;
  accessDecision: Record<string, unknown>;
  accessStatus: number;
  modelMetadata: unknown;
  modelMetadataRaw: string | undefined;
  modelMetadataStatus: number;
  mcpFailed: boolean;
  memoryResponse: unknown;
  memoryHttpStatus: number;
  memoryError: boolean;
  memoryRequestStarted: (() => void) | undefined;
  changeTrace: (trace: Record<string, any>) => Record<string, any>;
  sequence: number;
};
