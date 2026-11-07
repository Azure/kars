// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Regression test for the Foundry memory bug: the runtime calls the router's
// MCP Streamable-HTTP endpoint `/platform/mcp`, which REQUIRES the Accept
// header to advertise both `application/json` and `text/event-stream`. The
// thin client must send it, else the router replies 406 "Accept must include
// both application/json and text/event-stream" and memory is broken.
//
// This exercises `callPlatformTool` -> `routerCall` against a REAL loopback
// HTTP server (no mocking of the transport), so it would have caught the
// missing-header regression.

import { describe, it, expect, afterEach } from "vitest";
import { createServer, type Server } from "node:http";
import { callPlatformTool } from "./core/router-client.js";
import { callFoundryMemory, memoryUpdateSummary } from "./core/foundry-memory.js";

let server: Server | undefined;

afterEach(async () => {
  if (server) {
    await new Promise<void>((r) => server!.close(() => r()));
    server = undefined;
  }
  delete process.env.KARS_ROUTER_URL;
});

/** Start a fake MCP server that enforces the same Accept negotiation the real
 *  router does, captures the request, and returns a JSON-RPC tools/call
 *  result. Resolves with the captured request once it's been hit. */
function startFakeRouter(text = '{"memories":[]}', isError = false): Promise<{ port: number; captured: () => { accept?: string; body: any } }> {
  let capturedAccept: string | undefined;
  let capturedBody: any;
  return new Promise((resolve) => {
    server = createServer((req, res) => {
      capturedAccept = req.headers["accept"];
      let data = "";
      req.on("data", (c) => (data += c));
      req.on("end", () => {
        capturedBody = JSON.parse(data || "{}");
        const accept = (req.headers["accept"] || "") as string;
        // Mirror the router pipeline: POST requires BOTH json + event-stream.
        if (!(accept.includes("application/json") && accept.includes("text/event-stream"))) {
          res.writeHead(406, { "content-type": "text/plain" });
          res.end("Accept must include both application/json and text/event-stream");
          return;
        }
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({
          jsonrpc: "2.0",
          id: capturedBody.id,
          result: { content: [{ type: "text", text }], isError },
        }));
      });
    });
    server.listen(0, "127.0.0.1", () => {
      const addr = server!.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      resolve({ port, captured: () => ({ accept: capturedAccept, body: capturedBody }) });
    });
  });
}

describe("shared Foundry memory client", () => {
  it.each(["foundry", "builtin"])("preserves the router-selected %s backend without a local substitute", async (backend) => {
    const completed = { backend, status: "completed", update_id: "durable-id", approved_knowledge: false };
    const { port, captured } = await startFakeRouter(JSON.stringify(completed));
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    expect(await callFoundryMemory({ operation: "update", text: "retained preference", scope: null })).toEqual(completed);
    expect(captured().body.params.arguments).toEqual({ operation: "update", text: "retained preference" });
    expect(memoryUpdateSummary(completed)).toBe("completed");
  });

  it("preserves lexical recall and private scope from the built-in backend", async () => {
    const result = { backend: "builtin", retrieval: "lexical", scope: "private_owner_role", memories: [{ text: "Helsinki" }] };
    const { port } = await startFakeRouter(JSON.stringify(result));
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    expect(await callFoundryMemory({ operation: "search", text: "Helsinki" })).toEqual(result);
  });

  it("forwards static reads without inventing an agent-side store or scope", async () => {
    const { port, captured } = await startFakeRouter();
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    expect(await callFoundryMemory({ operation: "search", top_k: 20 })).toEqual({ memories: [] });
    expect(captured().body.params).toEqual({ name: "foundry.memory", arguments: { operation: "search", top_k: 20 } });
  });

  it("preserves an explicit partition, assistant role and terminal result", async () => {
    const completed = { status: "completed", update_id: "u-1" };
    const { port, captured } = await startFakeRouter(JSON.stringify(completed));
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    const args = { operation: "update", text: "handoff context", role: "assistant", scope: "custom_partition" };
    expect(await callFoundryMemory(args)).toEqual(completed);
    expect(captured().body.params.arguments).toEqual(args);
    expect(memoryUpdateSummary(completed)).toBe("completed");
  });

  it("surfaces a failed asynchronous operation rather than claiming persistence", async () => {
    const failed = { status: "failed", update_id: "u-1", error: { code: "model_failure" } };
    const { port } = await startFakeRouter(JSON.stringify(failed), true);
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    await expect(callFoundryMemory({ operation: "update", text: "x" })).rejects.toThrow("model_failure");
  });

  it.each(["null", "[]", "not JSON"])("rejects malformed MCP memory content: %s", async (text) => {
    const { port } = await startFakeRouter(text);
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;
    await expect(callFoundryMemory({ operation: "search" })).rejects.toThrow();
  });

  it.each(["queued", "in_progress"])("reports %s as unconfirmed", (status) => {
    expect(memoryUpdateSummary({ status, update_id: "u-1" })).toBe(`${status} (update u-1; persistence not yet confirmed)`);
  });

  it.each([{}, { status: "failed" }, { status: "unknown" }])("rejects unconfirmed success: %j", (value) => {
    expect(() => memoryUpdateSummary(value)).toThrow("did not confirm");
  });
});

describe("callPlatformTool — MCP Accept negotiation (foundry memory regression)", () => {
  it("sends Accept: application/json + text/event-stream and parses the result", async () => {
    const { port, captured } = await startFakeRouter();
    process.env.KARS_ROUTER_URL = `http://127.0.0.1:${port}`;

    const { text, isError } = await callPlatformTool("foundry.memory", {
      operation: "search",
      query: "coffee?",
    });

    const cap = captured();
    expect(cap.accept).toBeDefined();
    expect(cap.accept).toContain("application/json");
    expect(cap.accept).toContain("text/event-stream");
    // The JSON-RPC envelope was sent correctly.
    expect(cap.body.method).toBe("tools/call");
    expect(cap.body.params.name).toBe("foundry.memory");
    expect(cap.body.params.arguments).toEqual({ operation: "search", query: "coffee?" });
    // The router's text result is flattened back, not an error.
    expect(isError).toBe(false);
    expect(text).toBe('{"memories":[]}');
  });

  it("would surface the 406 as an error if the Accept header were missing", async () => {
    // Sanity: prove the fake server actually rejects a missing Accept, so the
    // positive test above is meaningful (not a no-op server).
    const { port } = await startFakeRouter();
    const res = await fetch(`http://127.0.0.1:${port}/platform/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json" }, // no Accept
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "x", arguments: {} } }),
    });
    expect(res.status).toBe(406);
    expect(await res.text()).toContain("Accept must include both");
  });
});
