// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type Server } from "node:http";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { registerHttpFetchTool } from "./http-fetch.js";
import { routerCall } from "../router-client.js";

let server: Server;
let oldRouter: string | undefined;
let requests: { path: string; body?: Record<string, unknown> }[];
let status: number;
let response: unknown;
let tool: any;

beforeEach(async () => {
  oldRouter = process.env.KARS_ROUTER_URL;
  requests = []; status = 403;
  response = { error: "approval_pending", request: { scope_id: "scope-1", request_id: "egress-1", status: "approved" }, enforcement_changed: false };
  registerHttpFetchTool({ registerTool: (registered: any) => { tool = registered; } });
  server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk);
    requests.push({ path: req.url!, ...(chunks.length ? { body: JSON.parse(Buffer.concat(chunks).toString()) } : {}) });
    if (req.url === "/v1/access-requests") { res.end(JSON.stringify({ scope: { id: "scope-1" } })); return; }
    res.writeHead(status);
    res.end(typeof response === "string" ? response : JSON.stringify(response));
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  process.env.KARS_ROUTER_URL = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
});
afterEach(async () => {
  await new Promise<void>(resolve => server.close(() => resolve()));
  if (oldRouter === undefined) delete process.env.KARS_ROUTER_URL;
  else process.env.KARS_ROUTER_URL = oldRouter;
});

describe("governed fetch approval waits", () => {
  it("passes the current scope to actual policy wait, not queue approval", async () => {
    const result = await tool.execute("call-1", { url: "https://example.com/report", wait_for_approval_ms: 300000 });
    expect(result.isError).toBe(true);
    expect(JSON.parse(result.content[0].text)).toEqual({ ...(response as object), http_status: 403 });
    expect(requests).toEqual([{ path: "/v1/access-requests" }, { path: "/egress/fetch", body: {
      url: "https://example.com/report", method: "GET", headers: {}, wait_for_approval_ms: 300000, scope_id: "scope-1",
    } }]);
  });
  it("preserves immediate denied fetches without a separate scope request", async () => {
    await tool.execute("call-1", { url: "https://example.com/report" });
    expect(requests).toHaveLength(1);
    expect(requests[0].body).not.toHaveProperty("scope_id");
  });
  it("reports non-JSON refusals as errors with HTTP status", async () => {
    status = 503; response = "Unavailable";
    const result = await tool.execute("call-1", { url: "https://example.com/report" });
    expect(result.isError).toBe(true);
    expect(JSON.parse(result.content[0].text)).toMatchObject({ http_status: 503, error: expect.any(String) });
  });
  it("returns actual successful fetch data", async () => {
    status = 200; response = { status: 200, body: "Evidence", headers: {}, truncated: false };
    const result = await tool.execute("call-1", { url: "https://example.com/report", wait_for_approval_ms: 50 });
    expect(result.isError).toBe(false);
    expect(JSON.parse(result.content[0].text)).toEqual(response);
  });
  it("rejects invalid wait before dispatch", async () => {
    expect((await tool.execute("call-1", { url: "https://example.com", wait_for_approval_ms: 300001 })).isError).toBe(true);
    expect(requests).toHaveLength(0);
  });
  it("honors an already cancelled invocation", async () => {
    const controller = new AbortController(); controller.abort();
    expect((await tool.execute("call-1", { url: "https://example.com" }, controller.signal)).isError).toBe(true);
    expect(requests).toHaveLength(0);
  });
  it("bounds router envelopes without losing default non-strict compatibility", async () => {
    status = 400; response = "Legacy response";
    expect(await routerCall("GET", "/legacy")).toBe(response);
    await expect(routerCall("GET", "/bounded", undefined, 1000, undefined, undefined, { maxBytes: 4 })).rejects.toThrow();
  });
});
