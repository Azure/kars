// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { createServer, type IncomingHttpHeaders, type Server, type ServerResponse } from "node:http";
import { ReturnedRouterResponses, ROUTER_OBSERVATIONS_ARTIFACT, sanitizeRouterEvent } from "./router-observations.js";
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
    const selected = body as { events?: { seq: number }[] };
    const cursor = selected?.events?.at(-1)?.seq ?? 0;
    res.end(typeof body === "string" ? body : JSON.stringify(selected?.events
      ? { cursor, high_water: cursor, has_more: false, ...selected } : body));
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
  it.each([null, "call_id", "name", "round", "null", "unknown_outcome", "missing_proposal"])(
    "retains only proposal-correlated immediate outcomes (%s)", async corruption => {
      const source = trace();
      const proposal = { kind: "tool_proposed", scope_id: "scope-a", round: 1, seq: 2,
        source: "model-proposed", call_id: "call-1", name: "read_file", ok: null };
      const result: Record<string, unknown> = { ...proposal, kind: "tool_result", source: "harness-reported", seq: 3, ok: true, ms: 7 };
      if (corruption === "call_id" || corruption === "name") result[corruption] = "unrelated";
      if (corruption === "round") result.round = 2;
      if (corruption === "null") result.reported_in_round = null;
      if (corruption === "unknown_outcome") result.ok = null;
      await serve({ ...source, rounds: [1, 2], events: [...source.events,
        ...(corruption === "missing_proposal" ? [] : [proposal]), result] });
      const selected = new ReturnedRouterResponses(); selected.capture(headers("1")); selected.capture(headers("2"));
      const artifact = JSON.parse((await selected.artifact())!);
      expect(artifact.state).toBe(corruption ? "unavailable" : "observed");
      if (!corruption) expect(artifact.trace.events.at(-1)).toMatchObject({ kind: "tool_result", ok: true });
    });
  it("forwards bounded router argument previews and rejects malformed preview payloads", () => {
    const event = { kind: "tool_proposed", name: "write_file", call_id: "call-1", argument_preview: {
      text: '{"path":"/sandbox/report.md","content":"Useful evidence"}', redacted: false, truncated: false,
    }, raw_arguments: "must not be forwarded" };
    expect(sanitizeRouterEvent(event).argument_preview).toEqual(event.argument_preview);
    expect(sanitizeRouterEvent(event)).not.toHaveProperty("raw_arguments");
    for (const preview of [null, { ...event.argument_preview, raw: "hidden" },
      { ...event.argument_preview, text: "🦀".repeat(513) }, { ...event.argument_preview, redacted: "true" }]) {
      expect(() => sanitizeRouterEvent({ ...event, argument_preview: preview })).toThrow("Invalid tool argument preview");
    }
  });
  it.each([false, true])("exports an ordinary 128-tool phase over four pages (argument previews: %s)", async previews => {
    const cycle = JSON.parse(readFileSync(new URL("../../../../inference-router/tests/fixtures/router-observation-tool-cycle.json", import.meta.url), "utf8"));
    const events = Array.from({ length: 128 }, (_, i) => cycle.filter((e: any) => e.kind !== "tool_result" || e.reported_in_round).map((e: any) => ({
      ...e, round: i + 1, seq: i * 7 + e.seq, ...(e.call_id ? { call_id: `call-${i + 1}` } : {}),
      ...(e.reported_in_round ? { reported_in_round: i + 2 } : {}),
      ...(previews && e.kind === "tool_proposed" ? { argument_preview: { text: "Useful details ".repeat(28), truncated: false, redacted: false } } : {}),
    }))).flat();
    events.push({ ...cycle[0], round: 129, seq: 897, tool_calls_observed: 0, finish_reason: "stop" });
    const rounds = Array.from({ length: 129 }, (_, i) => i + 1);
    await serve((res: ServerResponse) => {
      const url = new URL(calls.at(-1)!.url!, "http://router");
      expect(url.searchParams.get("rounds")).toBe(rounds.join(","));
      const after = Number(url.searchParams.get("after") ?? 0);
      const page = events.filter((e: any) => e.seq > after).slice(0, 256);
      const cursor = page.at(-1).seq;
      res.end(JSON.stringify({ scope_id: cycle[0].scope_id, rounds, coverage: "returned-responses-only", durable: false,
        missing_rounds: [], dropped_events: 0, truncated: false, events: page, cursor, high_water: 897, has_more: cursor < 897 }));
    });
    const selected = new ReturnedRouterResponses();
    for (const round of rounds) selected.capture(headers(String(round), cycle[0].scope_id));
    const content = (await selected.artifact())!;
    const result = JSON.parse(content);
    expect(result.state).toBe("observed");
    expect(result.trace.events).toHaveLength(769);
    expect(result.trace.events.filter((e: any) => e.kind === "tool_result")).toHaveLength(128);
    expect(result.trace.events.filter((e: any) => e.kind === "tool_result").every((e: any) => e.reported_in_round === e.round + 1)).toBe(true);
    expect(calls).toHaveLength(4);
    expect(Buffer.byteLength(content)).toBeGreaterThan(128 * 1024);
    expect(Buffer.byteLength(content)).toBeLessThan(384 * 1024);
    expect(result.trace.events.filter((e: any) => e.argument_preview)).toHaveLength(previews ? 128 : 0);
  });
  it("retains observation time and allow-listed completion metadata, never raw payloads", async () => {
    const source = trace();
    const at = "2026-10-08T20:00:00.123Z";
    await serve({ ...source, rounds: [1, 2], events: [
      { ...source.events[0], observed_at: at },
      { kind: "tool_result", scope_id: "scope-a", round: 1, reported_in_round: 2,
        seq: 2, source: "harness-reported", name: "file_write", call_id: "call-1", ok: true,
        observed_at: at, ms: 32, result_summary: {
          kind: "file_write", bytes: 12, artifact_name: "briefing.md", private_content: "secret-marker",
        } },
    ] });
    const selected = new ReturnedRouterResponses();
    selected.capture(headers("1")); selected.capture(headers("2"));
    const content = (await selected.artifact())!;
    const events = JSON.parse(content).trace.events;
    expect(events.every((event: { observed_at: string }) => event.observed_at === at)).toBe(true);
    expect(events[1].result_summary).toEqual({ kind: "file_write", bytes: 12, artifact_name: "briefing.md" });
    expect(events[1].ms).toBe(32);
    expect(content).not.toContain("secret-marker");
  });

  it("retains selected router flags without requiring a later harness reporting round", async () => {
    const source = trace();
    await serve({ ...source, events: [...source.events,
      { kind: "tool_proposed", scope_id: "scope-a", round: 1, seq: 2, source: "model-proposed", name: "http_fetch", call_id: "call-1", ok: null },
      { kind: "router_operation", scope_id: "scope-a", round: 1, seq: 3, source: "router-policy", name: "http_fetch", call_id: "call-1",
        operation: "egress_policy", flag: "private_target", allowed: false, http_status: null, complete: null,
        destination_host: "localhost", destination_port: 443, ms: 0, url: "https://localhost/private", headers: { Authorization: "secret-marker" } },
    ] });
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    const content = (await selected.artifact())!;
    const value = JSON.parse(content);
    expect(value.state).toBe("observed");
    expect(value.trace.events[2]).toMatchObject({ kind: "router_operation", flag: "private_target", allowed: false });
    expect(content).not.toMatch(/secret-marker|https:|Authorization/);
  });

  it("reports completion only with valid returned scope and a positive bounded router receipt", async () => {
    await serve({ recorded: true, source: "harness-reported", enforcement_changed: false });
    const selected = new ReturnedRouterResponses();
    await expect(selected.reportTool("call-1", true, 4)).rejects.toThrow("scope");
    expect(calls).toHaveLength(0);
    selected.capture(headers());
    await selected.reportTool("call-1", true, 4, { kind: "file_write", bytes: 12 });
    expect(calls[0].url).toBe("/telemetry/tool");
    selected.capture(headers("2", "another-scope"));
    await expect(selected.reportTool("call-2", true, 4)).rejects.toThrow("scope");
    expect(calls).toHaveLength(1);
  });

  it.each([
    [{ recorded: false }, 200], [{ recorded: true, source: "router", enforcement_changed: false }, 200],
    ["invalid", 200], [{ recorded: true, source: "harness-reported", enforcement_changed: false }, 409],
    ["x".repeat(5000), 200],
  ])("surfaces rejected or invalid completion receipts (%#)", async (body, status) => {
    await serve(body, status as number);
    const selected = new ReturnedRouterResponses(); selected.capture(headers());
    await expect(selected.reportTool("call-1", false, 2)).rejects.toThrow();
  });

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
    if (scenario === "too many") for (let n = 2; n <= 131; n++) selected.capture(headers(String(n)));
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
