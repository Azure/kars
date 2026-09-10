// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import * as http from "node:http";

const MAX_BYTES = 2 * 1024 * 1024;
const MAX_QUEUED = 8;
const PROTOCOL = "2025-06-18";
interface State { initialized: boolean; session?: string; protocol: string; nextId: number; queued: number; tail: Promise<void> }
const KEY = Symbol.for("kars-mcp-bridge");
const globals = globalThis as typeof globalThis & { [KEY]?: State };
const state = globals[KEY] ??= { initialized: false, protocol: PROTOCOL, nextId: 1, queued: 0, tail: Promise.resolve() };

interface Response { status: number; headers: http.IncomingHttpHeaders; body: unknown }
interface Result { content: Array<{ type: "text"; text: string }>; isError?: boolean }
interface Tool { name: string; label: string; description: string; parameters: Record<string, unknown>;
  execute(id: string, args?: Record<string, unknown>): Promise<Result> }

function request(payload: Record<string, unknown>, server?: string): Promise<Response> {
  const body = JSON.stringify(payload);
  if (Buffer.byteLength(body) > MAX_BYTES) return Promise.reject(new Error("MCP request exceeds its byte limit"));
  return new Promise((resolve, reject) => {
    const req = http.request({
      hostname: "127.0.0.1", port: 8443, path: "/mcp", method: "POST",
      headers: { accept: "application/json, text/event-stream", "content-type": "application/json",
        "content-length": Buffer.byteLength(body), "mcp-protocol-version": state.protocol,
        ...(state.session ? { "mcp-session-id": state.session } : {}),
        ...(server ? { "x-kars-mcp-server": server } : {}) },
    }, response => {
      const chunks: Buffer[] = [];
      let size = 0;
      response.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > MAX_BYTES) { req.destroy(new Error("MCP response exceeds its byte limit")); return; }
        chunks.push(chunk);
      });
      response.on("error", () => reject(new Error("MCP response transport failed")));
      response.on("end", () => {
        try {
          const bytes = Buffer.concat(chunks).toString("utf8");
          resolve({ status: response.statusCode ?? 500, headers: response.headers,
            body: bytes ? JSON.parse(bytes) : undefined });
        } catch { reject(new Error("MCP router response is not valid JSON")); }
      });
    });
    req.on("error", error => reject(new Error(error.message.startsWith("MCP ") ? error.message : "MCP transport failed")));
    req.setTimeout(30_000, () => req.destroy(new Error("MCP request timed out")));
    req.end(body);
  });
}

function rpc(response: Response, id: number): Record<string, unknown> {
  if (response.status < 200 || response.status >= 300) throw new Error(`MCP HTTP ${response.status}`);
  const body = response.body as { jsonrpc?: unknown; id?: unknown; error?: unknown; result?: unknown } | undefined;
  if (body?.jsonrpc !== "2.0" || body.id !== id || body.error
    || !body.result || typeof body.result !== "object" || Array.isArray(body.result)) {
    throw new Error("MCP RPC failed or omitted its matching result");
  }
  return body.result as Record<string, unknown>;
}

async function initialize(server?: string): Promise<void> {
  state.session = undefined;
  state.protocol = PROTOCOL;
  const id = state.nextId++;
  const response = await request({ jsonrpc: "2.0", id, method: "initialize",
    params: { protocolVersion: PROTOCOL, capabilities: {}, clientInfo: { name: "kars-runtime-openclaw", version: "0.1.0" } } }, server);
  const result = rpc(response, id);
  if (typeof result.protocolVersion !== "string"
    || !["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].includes(result.protocolVersion)) {
    throw new Error("MCP negotiated an unsupported protocol");
  }
  const session = response.headers["mcp-session-id"];
  if (session !== undefined && (typeof session !== "string" || !session.length
    || session.length > 1024 || /[^\x21-\x7e]/.test(session))) throw new Error("MCP session identifier is invalid");
  state.session = session;
  state.protocol = result.protocolVersion;
  const notification = await request({ jsonrpc: "2.0", method: "notifications/initialized", params: {} }, server);
  if (notification.status < 200 || notification.status >= 300) throw new Error(`MCP initialization HTTP ${notification.status}`);
  state.initialized = true;
}

export async function invokeMcp(method: "tools/list" | "tools/call", params: Record<string, unknown>, server?: string): Promise<Record<string, unknown>> {
  if (server !== undefined && (typeof server !== "string" || !server.length || server.length > 253 || !/^[a-z0-9.-]+$/.test(server))) {
    throw new Error("Invalid MCP server scope");
  }
  if (state.queued >= MAX_QUEUED) throw new Error("MCP bridge queue is full");
  state.queued++;
  const deadline = Date.now() + 60_000;
  const run = async () => {
    try {
      if (Date.now() >= deadline) throw new Error("MCP bridge queue timed out");
      if (!state.initialized) await initialize(server);
      const id = state.nextId++;
      const response = await request({ jsonrpc: "2.0", id, method, params }, server);
      // A failed call is never replayed. A later explicit caller may open a
      // new session after a definite transport-level session rejection.
      if (response.status === 404 || response.status === 400) state.initialized = false;
      return rpc(response, id);
    } finally { state.queued--; }
  };
  const result = state.tail.then(run, run);
  state.tail = result.then(() => undefined, () => undefined);
  return result;
}

function text(value: unknown, failed = false): Result {
  return { content: [{ type: "text", text: JSON.stringify(value) }],
    ...(failed || (value as { isError?: unknown } | undefined)?.isError === true ? { isError: true } : {}) };
}

export function registerMcpBridgeTools(api: { registerTool(tool: Tool): void }): void {
  api.registerTool({
    name: "kars_mcp_list", label: "Kars MCP Tool List",
    description: "List qualified MCP tools mounted in this sandbox. Optional server scope narrows the catalog.",
    parameters: { type: "object", properties: { server: { type: "string" } } },
    async execute(_id, args = {}) {
      try {
        const result = await invokeMcp("tools/list", {}, args.server as string | undefined);
        if (!Array.isArray(result.tools)) throw new Error("MCP catalog omitted tools");
        return text(result);
      } catch (error) { return text({ error: error instanceof Error ? error.message : "MCP discovery failed" }, true); }
    },
  });
  api.registerTool({
    name: "kars_mcp_call", label: "Kars MCP Tool Call",
    description: "Invoke the exact MCP tool name returned by kars_mcp_list. Calls are never automatically replayed.",
    parameters: { type: "object", properties: { name: { type: "string" }, arguments: { type: "object" }, server: { type: "string" } }, required: ["name"] },
    async execute(_id, args = {}) {
      if (typeof args.name !== "string" || !args.name.trim()) return text({ error: "name is required" }, true);
      const argumentsValue = args.arguments ?? {};
      if (typeof argumentsValue !== "object" || argumentsValue === null || Array.isArray(argumentsValue)) {
        return text({ error: "arguments must be an object" }, true);
      }
      try { return text(await invokeMcp("tools/call", { name: args.name, arguments: argumentsValue }, args.server as string | undefined)); }
      catch (error) { return text({ error: error instanceof Error ? error.message : "MCP invocation failed" }, true); }
    },
  });
}

export function resetMcpBridgeForTests(): void {
  state.initialized = false;
  state.session = undefined;
  state.protocol = PROTOCOL;
  state.nextId = 1;
  state.queued = 0;
  state.tail = Promise.resolve();
}
