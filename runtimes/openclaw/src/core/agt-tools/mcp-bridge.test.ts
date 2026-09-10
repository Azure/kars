// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as http from "node:http";
import { once } from "node:events";

const target = vi.hoisted(() => ({ port: 0 }));
vi.mock("node:http", async () => {
  const actual = await vi.importActual<typeof import("node:http")>("node:http");
  return { ...actual, request: (options: http.RequestOptions, callback: (response: http.IncomingMessage) => void) => {
    expect(options.hostname).toBe("127.0.0.1");
    expect(options.port).toBe(8443);
    expect(options.path).toBe("/mcp");
    return actual.request({ ...options, port: target.port }, callback);
  } };
});
import { invokeMcp, registerMcpBridgeTools, resetMcpBridgeForTests } from "./mcp-bridge.js";

describe("real HTTP MCP bridge", () => {
  let server: http.Server;
  let calls: Array<{ body: any; headers: http.IncomingHttpHeaders }>;
  let result: Record<string, unknown>;
  let rpcError: unknown;
  let status: number;
  beforeEach(async () => {
    resetMcpBridgeForTests();
    calls = [];
    result = { content: [{ type: "text", text: "ok" }] };
    rpcError = undefined;
    status = 200;
    server = http.createServer(async (request, response) => {
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(chunks).toString());
      calls.push({ body, headers: request.headers });
      response.setHeader("content-type", "application/json");
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
  });
  afterEach(async () => { await new Promise<void>(resolve => server.close(() => resolve())); });

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

  it("rejects non-string or malformed scopes before any request", async () => {
    await expect(invokeMcp("tools/list", {}, ["everything"] as unknown as string)).rejects.toThrow("Invalid MCP");
    await expect(invokeMcp("tools/list", {}, "../other")).rejects.toThrow("Invalid MCP");
    expect(calls).toHaveLength(0);
  });
});
