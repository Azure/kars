// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type Server, type RequestListener } from "node:http";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getMeshRegistry, type IMeshRegistry } from "./mesh-registry.js";

const did = `did:mesh:${"a".repeat(32)}`;
let server: Server;
let registry: IMeshRegistry;
let handler: RequestListener;
let requests: string[];
const operation = (assertCurrent = () => {}, signal = new AbortController().signal) => ({ signal, assertCurrent, deadline: Date.now() + 2000 });
beforeEach(async () => {
  vi.stubEnv("KARS_MESH_PROVIDER", "agt"); requests = [];
  handler = (_req, res) => res.end(JSON.stringify({ did, metadata: { display_name: "reviewer" }, last_seen: new Date().toISOString() }));
  server = createServer((req, res) => { requests.push(req.url!); handler(req, res); });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("Missing test listener");
  registry = getMeshRegistry(path => `http://127.0.0.1:${address.port}${path}`);
});
afterEach(async () => {
  server.closeAllConnections();
  await new Promise<void>(resolve => server.close(() => resolve()));
  vi.unstubAllEnvs();
});

describe("existing registry with native operation custody", () => {
  it.each(["stale", "aborted", "expired", "infinite"])("issues no request for %s authority", async cause => {
    const abort = new AbortController();
    const op = operation(() => { if (cause === "stale") throw new Error("stale"); }, abort.signal);
    if (cause === "aborted") abort.abort();
    if (cause === "expired") op.deadline = Date.now() - 1;
    if (cause === "infinite") op.deadline = Infinity;
    await expect(registry.lookup(did, { operation: op })).rejects.toThrow(); expect(requests).toEqual([]);
  });
  it("bounds hydration to sixteen actual lookups even when registry ignores limit", async () => {
    handler = (req, res) => {
      if (req.url!.includes("/discover")) res.end(JSON.stringify({ results: Array.from({ length: 40 }, (_, i) => ({ did: `did:mesh:${i.toString(16).padStart(32, "0")}` })) }));
      else res.end(JSON.stringify({ did: decodeURIComponent(req.url!.split("/").at(-1)!), metadata: { display_name: "reviewer" }, last_seen: new Date().toISOString() }));
    };
    expect(await registry.search("reviewer", { operation: operation(), limit: 500 })).toHaveLength(16);
    expect(requests).toHaveLength(17); expect(requests[0]).toContain("limit=16");
  });
  it.each(["oversize", "http", "identity", "json"])("propagates %s failure once, while preserving generic lookup behavior", async failure => {
    handler = (_req, res) => {
      if (failure === "http") res.statusCode = 503;
      res.end(failure === "oversize" ? "x".repeat(160 * 1024 + 1) : failure === "json" ? "bad-json" : JSON.stringify({ did: "other" }));
    };
    await expect(registry.lookup(did, { operation: operation() })).rejects.toThrow(); expect(requests).toHaveLength(1);
    if (failure !== "identity") await expect(registry.lookup(did)).resolves.toBeNull();
  });
  it("cancels a stalled response at the absolute deadline, not an inactivity-only deadline", async () => {
    handler = (_req, res) => { res.writeHead(200); res.write("{"); };
    await expect(registry.lookup(did, { operation: { ...operation(), deadline: Date.now() + 30 }, timeoutMs: 1000 })).rejects.toThrow();
    expect(requests).toHaveLength(1);
  });
  it("aborts an in-flight lookup without retry", async () => {
    const abort = new AbortController();
    handler = (_req, res) => { res.write("{"); abort.abort(); };
    await expect(registry.lookup(did, { operation: operation(() => {}, abort.signal) })).rejects.toThrow();
    expect(requests).toHaveLength(1);
  });
  it("rechecks authority before returning response data", async () => {
    let current = true;
    handler = (_req, res) => { current = false; res.end(JSON.stringify({ did })); };
    await expect(registry.lookup(did, { operation: operation(() => { if (!current) throw new Error("stale"); }) })).rejects.toThrow("stale");
  });
});
