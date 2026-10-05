// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { createServer, type IncomingHttpHeaders, type Server, type ServerResponse } from "node:http";
import { ReturnedRouterResponses, ROUTER_OBSERVATIONS_ARTIFACT } from "./router-observations.js";
import { TaskCompletionLedger } from "./task-completion.js";

let server: Server | undefined;
const calls: { url?: string; headers: IncomingHttpHeaders }[] = [];
const headers = (round = "1", scope = "scope-a") => ({ "x-kars-telemetry-scope": scope, "x-kars-telemetry-round": round });
const trace = () => ({
  scope_id: "scope-a", rounds: [1], missing_rounds: [], dropped_events: 0, truncated: false,
  coverage: "returned-responses-only", durable: false,
  events: [{ kind: "round", scope_id: "scope-a", round: 1, seq: 1, source: "router-upstream",
    model: "mini", provider: "foundry", usage: { prompt_tokens: null, completion_tokens: null, total_tokens: null },
    http_status: 200, accepted: true, outcome: "complete", usage_state: "missing", finish_reason: "stop",
    partial_observation: false, ms: 23, tool_calls_observed: 0 }],
});
async function serve(body: unknown, status = 200) {
  server = createServer((req, res) => {
    calls.push({ url: req.url, headers: req.headers });
    if (typeof body === "function") { body(res); return; }
    res.writeHead(status, { "content-type": "application/json" });
    res.end(typeof body === "string" ? body : JSON.stringify(body));
  });
  await new Promise<void>(resolve => server!.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing test port");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
}
afterEach(async () => {
  server?.closeAllConnections();
  if (server) await new Promise<void>(resolve => server!.close(() => resolve()));
  server = undefined;
  calls.length = 0;
  vi.unstubAllEnvs();
});

describe("returned router response selection", () => {
  it("produces the shared router-to-Bridge contract through the real collector", async () => {
    const contract = JSON.parse(readFileSync(new URL("../../../../inference-router/tests/fixtures/router-observations-v1.json", import.meta.url), "utf8"));
    const routerTrace = structuredClone(contract.trace);
    for (const event of routerTrace.events) {
      if (event.kind === "round") event.usage.cached_tokens = null;
      event.private_payload = "must-not-be-forwarded";
    }
    await serve(routerTrace);
    const selected = new ReturnedRouterResponses();
    selected.capture({});
    selected.capture(headers("2"));
    selected.capture(headers("1"));
    const content = (await selected.artifact())!;
    expect(JSON.parse(content)).toEqual(contract);
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/telemetry/trace?rounds=1,2");
    expect(calls[0].headers["x-kars-service-scope"]).toBe(contract.scope_id);
    expect(calls[0].headers.authorization).toBeUndefined();
    const ledger = new TaskCompletionLedger("model", true);
    ledger.commitArtifacts({ "briefing.md": "Useful briefing\r\n✓" });
    expect(ledger.attachRouterObservations(content)).toBe(true);
  });
  it("does not request or invent evidence when response headers are absent", async () => {
    await serve(trace());
    const selected = new ReturnedRouterResponses();
    selected.capture({});
    expect(await selected.artifact()).toBeUndefined();
    expect(calls).toEqual([]);
  });
  it("selects exact returned IDs and strips unknown payload fields", async () => {
    const data = trace();
    await serve({ ...data, private_prompt: "secret-marker", events: [{ ...data.events[0], private_body: "secret-marker",
      usage: { ...data.events[0].usage, private_field: "secret-marker" } }] });
    const selected = new ReturnedRouterResponses();
    selected.capture({});
    selected.capture(headers());
    const content = (await selected.artifact())!;
    const artifact = JSON.parse(content);
    expect(artifact).toMatchObject({ state: "observed", responses: 2, rounds: [1], trace: data });
    expect(content).not.toContain("secret-marker");
    expect(calls[0].url).toBe("/telemetry/trace?rounds=1");
    expect(calls[0].headers["x-kars-service-scope"]).toBe("scope-a");
    expect(calls[0].headers.authorization).toBeUndefined();
  });
  it.each([
    headers("0"), headers("01"), headers("9007199254740992"), headers("1", "bad scope"),
    { "x-kars-telemetry-scope": ["scope-a"], "x-kars-telemetry-round": "1" },
    { "x-kars-telemetry-round": "1" },
  ])("rejects malformed returned correlation without a read (%#)", async malformed => {
    await serve(trace());
    const selected = new ReturnedRouterResponses();
    selected.capture(malformed);
    expect(JSON.parse((await selected.artifact())!).state).toBe("unavailable");
    expect(calls).toEqual([]);
  });
  it.each(["duplicate", "changed scope", "too many"])("fails closed for %s", async scenario => {
    await serve(trace());
    const selected = new ReturnedRouterResponses();
    selected.capture(headers());
    if (scenario === "too many") for (let n = 2; n <= 33; n++) selected.capture(headers(String(n)));
    else selected.capture(headers(scenario === "duplicate" ? "1" : "2", scenario === "duplicate" ? "scope-a" : "scope-b"));
    expect(JSON.parse((await selected.artifact())!).state).toBe("unavailable");
    expect(calls).toEqual([]);
  });
  it.each([
    { scope_id: "scope-b" }, { rounds: [2] }, { durable: true }, { missing_rounds: [2] },
    { events: [{ kind: "tool_result", scope_id: "scope-a", round: 1, reported_in_round: 2 }] },
    { events: [{ kind: "round", scope_id: "scope-a", round: 2 }] },
  ])("rejects mismatched selections (%#)", async change => {
    await serve({ ...trace(), ...change });
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    expect(JSON.parse((await selected.artifact())!).state).toBe("unavailable");
  });
  it("preserves explicit missing, dropped and truncated coverage", async () => {
    await serve({ ...trace(), events: [], missing_rounds: [1], dropped_events: 42, truncated: true });
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    expect(JSON.parse((await selected.artifact())!).trace).toMatchObject({ events: [], missing_rounds: [1], dropped_events: 42, truncated: true });
  });
  it.each([["not json", 200], [trace(), 409], ["x".repeat(49 * 1024), 200]])("reports failed reads without failing execution (%#)", async (body, status) => {
    await serve(body, status as number);
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    expect(JSON.parse((await selected.artifact())!).state).toBe("unavailable");
  });
  it("enforces an absolute deadline even while the router keeps sending bytes", async () => {
    await serve((res: ServerResponse) => {
      res.writeHead(200); res.write("{");
      const timer = setInterval(() => res.write(" "), 50);
      res.on("close", () => clearInterval(timer));
    });
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    const start = performance.now();
    expect(JSON.parse((await selected.artifact())!).state).toBe("unavailable");
    expect(performance.now() - start).toBeLessThan(3000);
  });
  it("reserves the filename and preserves all useful files when capacity is exhausted", () => {
    const ledger = new TaskCompletionLedger("mini", true);
    expect(() => ledger.prepareArtifact(ROUTER_OBSERVATIONS_ARTIFACT, "spoof")).toThrow("reserved");
    expect(() => ledger.commitArtifacts({ [ROUTER_OBSERVATIONS_ARTIFACT]: "spoof" })).toThrow();
    const files = Object.fromEntries(Array.from({ length: 16 }, (_, n) => [`file-${n}.md`, "Useful output"]));
    ledger.commitArtifacts(files);
    expect(ledger.attachRouterObservations("{}" )).toBe(false);
    expect(ledger.artifactSnapshot().artifacts).toEqual(files);
    expect(new TaskCompletionLedger("mini").attachRouterObservations("{}")).toBe(false);
  });
});
