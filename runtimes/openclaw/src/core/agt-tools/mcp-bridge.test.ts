// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as http from "node:http";
import { once } from "node:events";
import { readFileSync } from "node:fs";

const target = vi.hoisted(() => ({ port: 0 }));
vi.mock("node:http", async () => {
  const actual = await vi.importActual<typeof import("node:http")>("node:http");
  type Callback = (response: http.IncomingMessage) => void;
  return { ...actual, request: (options: http.RequestOptions | string | URL, second?: http.RequestOptions | Callback, third?: Callback) => {
    if (typeof options === "string" || options instanceof URL) {
      return actual.request(options, typeof second === "object" ? second : {}, typeof second === "function" ? second : third);
    }
    expect(options.hostname).toBe("127.0.0.1");
    expect(options.port).toBe(8443);
    expect(options.path).toBe("/mcp");
    return actual.request({ ...options, port: target.port }, typeof second === "function" ? second : third);
  } };
});
import { invokeMcp, MCP_BRIDGE_TOOLS, registerMcpBridgeTools, resetMcpBridgeForTests } from "./mcp-bridge.js";
import { getTaskTools } from "../agt-task-tools.js";
import { executeTaskWithEvidence } from "../agt-task-loop.js";

const deps = { meshClient: () => null, isInterruptRequested: () => false,
  interruptReason: () => "", setInterrupt: () => {} };
const log = { info: vi.fn(), warn: vi.fn() };
const completion = (name?: string, args = {}) => ({
  choices: [{ finish_reason: name ? "tool_calls" : "stop", message: { role: "assistant",
    content: name ? null : "Recorded the real MCP response.",
    ...(name ? { tool_calls: [{ id: `call-${name}`, type: "function",
      function: { name, arguments: JSON.stringify(args) } }] } : {}) } }],
  usage: { prompt_tokens: 7, completion_tokens: 4, total_tokens: 11 },
});

describe("real HTTP MCP bridge", () => {
  let server: http.Server;
  let calls: Array<{ body: any; headers: http.IncomingHttpHeaders }>;
  let result: Record<string, unknown>;
  let rpcError: unknown;
  let status: number;
  let rawResponse: { method: string; status: number; body: string } | undefined;
  let modelResponses: ReturnType<typeof completion>[];
  let modelRequests: Array<{ tools: Array<{ function: { name: string } }>; messages: Array<{ role: string; content: string }> }>;
  let policyAllowed: boolean;
  let policyActions: string[];
  beforeEach(async () => {
    resetMcpBridgeForTests();
    calls = [];
    result = { content: [{ type: "text", text: "ok" }] };
    rpcError = undefined;
    status = 200;
    rawResponse = undefined;
    modelResponses = [];
    modelRequests = [];
    policyAllowed = true;
    policyActions = [];
    server = http.createServer(async (request, response) => {
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(chunks).toString());
      response.setHeader("content-type", "application/json");
      if (request.url === "/agt/evaluate") {
        policyActions.push(body.action);
        response.end(JSON.stringify({ allowed: policyAllowed }));
        return;
      }
      if (request.url?.includes("/chat/completions")) {
        modelRequests.push(body);
        response.end(JSON.stringify(modelResponses.shift() ?? completion()));
        return;
      }
      expect(request.url).toBe("/mcp");
      calls.push({ body, headers: request.headers });
      if (rawResponse && rawResponse.method === body.method) {
        response.writeHead(rawResponse.status, { "content-type": "text/plain" });
        response.end(rawResponse.body);
        return;
      }
      if (body.method === "initialize") {
        response.setHeader("mcp-session-id", "session-one");
        response.end(JSON.stringify({ jsonrpc: "2.0", id: body.id, result: { protocolVersion: "2025-06-18", capabilities: {} } }));
      } else if (body.method === "notifications/initialized") {
        response.writeHead(202).end();
      } else {
        response.writeHead(status);
        response.end(JSON.stringify({ jsonrpc: "2.0", id: body.id,
          ...(rpcError ? { error: rpcError } : { result: body.method === "tools/list" ? { tools: [{ name: "everything.echo" }] } : result }) }));
      }
    }).listen(0, "127.0.0.1");
    await once(server, "listening");
    target.port = (server.address() as { port: number }).port;
    vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${target.port}`);
    vi.stubEnv("OPENCLAW_MODEL", "gpt-5.4-mini");
  });
  afterEach(async () => {
    vi.unstubAllEnvs();
    await new Promise<void>(resolve => server.close(() => resolve()));
  });

  it.each(["azure-foundry", "github-models", "github-copilot"])("exposes the shared MCP tools to %s Missions and the host manifest", provider => {
    vi.stubEnv("KARS_PROVIDER", provider);
    vi.stubEnv("KARS_STRICT_TOOLS", "1");
    const manifest = JSON.parse(readFileSync(new URL("../../../openclaw.plugin.json", import.meta.url), "utf8"));
    for (const tool of MCP_BRIDGE_TOOLS) {
      const exposed = getTaskTools().find(candidate => candidate.function.name === tool.name);
      expect(exposed.function).toEqual({ name: tool.name, description: tool.description, parameters: tool.parameters });
      expect(manifest.contracts.tools).toContain(tool.name);
    }
  });

  it("executes discovery and invocation through the actual Mission loop and shared HTTP bridge", async () => {
    modelResponses = [completion("kars_mcp_list", { server: "everything" }),
      completion("kars_mcp_call", { name: "everything.echo", arguments: { text: "actual echo" }, server: "everything" }),
      completion()];
    result = { content: [{ type: "text", text: "actual echo" }] };
    const evidence = await executeTaskWithEvidence("Verify the MCP response", deps, log);
    expect(evidence.rounds).toBe(3);
    expect(modelRequests[0].tools.map(tool => tool.function.name)).toEqual(expect.arrayContaining(["kars_mcp_list", "kars_mcp_call"]));
    expect(policyActions).toEqual(["tool:kars_mcp_list", "tool:kars_mcp_call"]);
    expect(calls.map(call => call.body.method)).toEqual(["initialize", "notifications/initialized", "tools/list", "tools/call"]);
    expect(calls.at(-1)?.body.params).toEqual({ name: "everything.echo", arguments: { text: "actual echo" } });
    expect(JSON.parse(modelRequests[2].messages.at(-1)!.content)).toEqual(result);
    expect(calls.every(call => call.headers["x-kars-mcp-server"] === "everything")).toBe(true);
  });

  it("does not contact MCP when Mission policy denies the bridge tool", async () => {
    policyAllowed = false;
    modelResponses = [completion("kars_mcp_call", { name: "everything.echo" }), completion()];
    await executeTaskWithEvidence("Try the denied tool", deps, log);
    expect(policyActions).toEqual(["tool:kars_mcp_call"]);
    expect(calls).toHaveLength(0);
    expect(modelRequests[1].messages.at(-1)?.content).toContain("Blocked by policy");
  });

  it("keeps MCP unavailable to filesystem-only reviewed phases", async () => {
    modelResponses = [completion("kars_mcp_call", { name: "everything.echo" }), completion()];
    await expect(executeTaskWithEvidence("Do not escape the phase", deps, log, false, {
      name: "read", objective: "Read the reviewed file", capabilities: ["filesystem-read"],
      minToolCalls: 1, maxToolCalls: 2, requiredToolCalls: [], freshContext: true,
    })).rejects.toThrow();
    expect(modelRequests[0].tools.map(tool => tool.function.name)).not.toContain("kars_mcp_call");
    expect(calls).toHaveLength(0);
  });

  it("retains semantic MCP failure in the Mission response without replay or shell fallback", async () => {
    modelResponses = [completion("kars_mcp_call", { name: "everything.echo" }), completion()];
    result = { isError: true, content: [{ type: "text", text: "fixture tool failure" }] };
    await executeTaskWithEvidence("Report the actual failure", deps, log);
    expect(JSON.parse(modelRequests[1].messages.at(-1)!.content)).toEqual(result);
    expect(calls.filter(call => call.body.method === "tools/call")).toHaveLength(1);
    expect(policyActions).toEqual(["tool:kars_mcp_call"]);
  });

  it("initializes once, scopes calls, and sends negotiated session headers without credentials", async () => {
    await invokeMcp("tools/list", {}, "everything");
    await invokeMcp("tools/call", { name: "everything.echo", arguments: { value: 1 } }, "everything");
    expect(calls.map(call => call.body.method)).toEqual(["initialize", "notifications/initialized", "tools/list", "tools/call"]);
    expect(calls.at(-1)?.headers["mcp-session-id"]).toBe("session-one");
    expect(calls.at(-1)?.headers["mcp-protocol-version"]).toBe("2025-06-18");
    expect(calls.every(call => call.headers["x-kars-mcp-server"] === "everything")).toBe(true);
    expect(calls.every(call => call.headers.authorization === undefined)).toBe(true);
  });

  it("never replays accepted RPC failures or leaks their error bodies", async () => {
    rpcError = { code: -32000, message: "session expired PRIVATE_SENTINEL" };
    await expect(invokeMcp("tools/call", { name: "everything.echo" })).rejects.toThrow("MCP RPC failed");
    expect(calls.filter(call => call.body.method === "tools/call")).toHaveLength(1);
    expect(calls.filter(call => call.body.method === "initialize")).toHaveLength(1);
  });

  it("preserves semantic isError and rejects malformed arguments without invocation", async () => {
    const tools: any[] = [];
    registerMcpBridgeTools({ registerTool: tool => tools.push(tool) });
    result = { content: [{ type: "text", text: "tool failed" }], isError: true };
    const output = await tools[1].execute("id", { name: "everything.echo", arguments: {} });
    expect(output.isError).toBe(true);
    expect(JSON.parse(output.content[0].text).isError).toBe(true);
    const count = calls.length;
    expect((await tools[1].execute("id", { name: "everything.echo", arguments: [] })).isError).toBe(true);
    expect(calls).toHaveLength(count);
  });

  it("does not follow redirects or retry a failed HTTP call", async () => {
    status = 302;
    await expect(invokeMcp("tools/call", { name: "everything.echo" })).rejects.toThrow("MCP HTTP 302");
    expect(calls.filter(call => call.body.method === "tools/call")).toHaveLength(1);
  });

  it.each(
    ["initialize", "notifications/initialized", "tools/call"].flatMap(method =>
      [302, 403, 404, 503].map(status => ({ method, status }))),
  )("preserves HTTP $status during $method without parsing or exposing its body", async ({ method, status }) => {
    rawResponse = { method, status, body: "Router error PRIVATE_SENTINEL" };
    const prefix = method === "notifications/initialized" ? "MCP initialization HTTP" : "MCP HTTP";
    await expect(invokeMcp("tools/call", { name: "everything.echo" }, "everything"))
      .rejects.toMatchObject({ message: `${prefix} ${status}` });
    expect(calls.filter(call => call.body.method === method)).toHaveLength(1);
    expect(calls.filter(call => call.body.method === "tools/call"))
      .toHaveLength(method === "tools/call" ? 1 : 0);
  });

  it("reinitializes only on a later explicit call after a plain-text 404", async () => {
    rawResponse = { method: "tools/call", status: 404, body: "Unknown or unqualified MCP server scope" };
    await expect(invokeMcp("tools/call", { name: "everything.echo" }, "everything"))
      .rejects.toMatchObject({ message: "MCP HTTP 404" });
    expect(calls.filter(call => call.body.method === "initialize")).toHaveLength(1);
    rawResponse = undefined;
    await invokeMcp("tools/list", {}, "everything");
    expect(calls.filter(call => call.body.method === "initialize")).toHaveLength(2);
    expect(calls.filter(call => call.body.method === "tools/call")).toHaveLength(1);
  });

  it("still rejects a non-JSON successful response without exposing its body", async () => {
    rawResponse = { method: "tools/list", status: 200, body: "PRIVATE_SENTINEL" };
    await expect(invokeMcp("tools/list", {}))
      .rejects.toMatchObject({ message: "MCP router response is not valid JSON" });
  });

  it("rejects non-string or malformed scopes before any request", async () => {
    await expect(invokeMcp("tools/list", {}, ["everything"] as unknown as string)).rejects.toThrow("Invalid MCP");
    await expect(invokeMcp("tools/list", {}, "../other")).rejects.toThrow("Invalid MCP");
    expect(calls).toHaveLength(0);
  });
});
