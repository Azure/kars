// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createServer, type Server, type IncomingHttpHeaders } from "node:http";
import { NativeToolObservations } from "./native-tool-observations.js";
import { readFileSync } from "node:fs";
import { MAX_MISSION_ACTIVITY_BYTES, parseMissionMessage } from "@kars/mesh/dist/mission-protocol.js";

let server: Server | undefined;
const requests: { url?: string; headers: IncomingHttpHeaders; body: unknown }[] = [];
const name = "kars_mission_file_read";
const scope = "issued-scope";
const trace = () => ({ scope_id: scope, durable: false, has_more: false, cursor: 12, high_water: 12,
  coverage: "router-observed-only", dropped_events: 0, events: [
  { kind: "round", scope_id: scope, seq: 11, round: 3, provider: "azure-openai", model: "reviewed", accepted: true,
    source: "router-upstream", http_status: 200, outcome: "response_observed", usage_state: "present",
    usage: { prompt_tokens: 12, completion_tokens: 3, total_tokens: 15 }, partial_observation: false, ms: 5, tool_calls_observed: 1 },
  { kind: "tool_proposed", scope_id: scope, seq: 12, round: 3, call_id: "call-1", name, source: "model-proposed" },
] });
async function serve(response: (path: string) => { body: unknown; status?: number }) {
  server = createServer((req, res) => {
    let text = "";
    req.on("data", chunk => { text += chunk; });
    req.on("end", () => {
      requests.push({ url: req.url, headers: req.headers, body: text ? JSON.parse(text) : undefined });
      const result = req.url?.startsWith("/telemetry/trace?") && req.headers["x-kars-service-scope"] !== scope
        ? { body: { error: "stale_scope" }, status: 409 } : response(req.url!);
      res.writeHead(result.status ?? 200, { "content-type": "application/json" });
      res.end(typeof result.body === "string" ? result.body : JSON.stringify(result.body));
    });
  });
  await new Promise<void>(resolve => server!.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No local test listener");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
}
const ordinaryResponse = (path: string) => ({ body: path === "/telemetry/cursor" ? { scope_id: scope, cursor: 10, durable: false }
  : path.startsWith("/telemetry/trace?") ? trace() : { recorded: true, source: "harness-reported", enforcement_changed: false } });
afterEach(async () => {
  server?.closeAllConnections();
  if (server) await new Promise<void>((resolve, reject) => server!.close(error => error ? reject(error) : resolve()));
  server = undefined;
  requests.length = 0;
  vi.unstubAllEnvs();
});

describe("native router-issued tool correlation transport", () => {
  it("retains ordinary 128-tool Activity across four pages, with exact provider correlation and fail-closed later pages", async () => {
    const cycle = JSON.parse(readFileSync(new URL("../../../../inference-router/tests/fixtures/router-observation-tool-cycle.json", import.meta.url), "utf8"));
    const events: any[] = [];
    let mutate: ((page: any) => void) | undefined;
    await serve(path => {
      if (!path.startsWith("/telemetry/trace?")) return ordinaryResponse(path);
      const since = Number(new URL(path, "http://router.invalid").searchParams.get("since"));
      const page = events.filter(event => event.seq > since).slice(0, 256);
      const cursor = page.at(-1)?.seq ?? 10;
      const body = { ...trace(), events: structuredClone(page), cursor, high_water: events.at(-1)?.seq ?? 10,
        has_more: cursor < (events.at(-1)?.seq ?? 10) };
      if (since > 10) mutate?.(body);
      return { body };
    });
    const observations = await NativeToolObservations.open(new AbortController().signal);
    for (let round = 1; round <= 128; round++) {
      events.push(...cycle.map((event: any) => ({ ...event, scope_id: scope, round, seq: 10 + (round - 1) * 7 + event.seq,
        ...(event.call_id ? { call_id: `call-${round}` } : {}), ...(event.reported_in_round ? { reported_in_round: round + 1 } : {}) })));
      await expect(observations.match(`call-${round}`, "web_fetch", "azure-openai", "reviewed")).resolves.toBe("foundry");
      await observations.reportAuthorized(`call-${round}`, "web_fetch", true, 80);
      if (![39, 68, 128].includes(round)) continue;
      const before = requests.length;
      const content = (await observations.artifact())!;
      const value = JSON.parse(content);
      expect(value).toMatchObject({ state: "observed", correlated_tools: round });
      expect(value.trace.events).toHaveLength(round * 6);
      expect(value.trace.events.filter((event: any) => event.kind === "tool_result")).toHaveLength(round);
      expect(value.trace.events.some((event: any) => event.reported_in_round)).toBe(false);
      expect(Buffer.byteLength(JSON.stringify({ "kars-router-observations.json": content }))).toBeLessThan(MAX_MISSION_ACTIVITY_BYTES);
      expect(requests.length - before).toBe(Math.ceil(round * 7 / 256));
      expect(parseMissionMessage({ type: "mission:reply", version: 1, taskName: "research", taskUid: "task-uid",
        sandboxUid: "sandbox-uid", podUid: "pod-uid", runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`,
        dispatcherDid: `did:mesh:${"b".repeat(32)}`, bootId: "boot-1", assignmentId: "assignment-1", status: "running",
        activityFormat: "router-observations-v1", routerObservations: content })).not.toBeNull();
    }
    for (const change of [
      (page: any) => { page.scope_id = "reset"; },
      (page: any) => { page.dropped_events = 1; },
      (page: any) => { page.high_water--; },
      (page: any) => { page.events.shift(); },
      (page: any) => { page.events[0].seq++; },
      (page: any) => { page.cursor = 10; },
      (page: any) => { const proposal = page.events.find((e: any) => e.kind === "tool_proposed"); proposal.name = "wrong"; },
    ]) {
      mutate = change;
      expect(JSON.parse((await observations.artifact())!).state).toBe("unavailable");
    }
  });
  it("transports a correlated GitHub-service denial through the shared mission wire validator", async () => {
    const cycle = JSON.parse(readFileSync(new URL("../../../../inference-router/tests/fixtures/router-observation-tool-cycle.json", import.meta.url), "utf8"));
    const events = cycle.filter((event: any) => event.operation !== "egress_fetch" && !event.reported_in_round)
      .map((event: any, index: number) => ({ ...event, scope_id: scope, seq: 11 + index,
        ...(event.name ? { name: "kars_mission_http_fetch" } : {}),
        ...(event.operation === "egress_policy" ? { flag: "github_service", allowed: false, destination_host: "api.github.com" } : {}),
        ...(event.kind === "tool_result" ? { ok: false } : {}) }));
    await serve(path => path.startsWith("/telemetry/trace?")
      ? { body: { ...trace(), events, cursor: events.at(-1).seq, high_water: events.at(-1).seq } }
      : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", "kars_mission_http_fetch", "azure-openai", "reviewed");
    await observations.reportAuthorized("call-1", "kars_mission_http_fetch", false, 80);
    const content = (await observations.artifact())!;
    expect(JSON.parse(content).trace.events).toContainEqual(expect.objectContaining({
      operation: "egress_policy", flag: "github_service", allowed: false, destination_host: "api.github.com",
    }));
    expect(parseMissionMessage({ type: "mission:reply", version: 1, taskName: "research", taskUid: "task-uid",
      sandboxUid: "sandbox-uid", podUid: "pod-uid", runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`,
      dispatcherDid: `did:mesh:${"b".repeat(32)}`, bootId: "boot-1", assignmentId: "assignment-1", status: "running",
      activityFormat: "router-observations-v1", routerObservations: content })).not.toBeNull();
  });
  it("paginates a contiguous bounded window and retains the proposing round for sibling calls", async () => {
    const events: Record<string, unknown>[] = Array.from({ length: 256 }, (_, i) => ({
      kind: "round", scope_id: scope, seq: 11 + i, round: i + 1, provider: "unrelated", model: "unrelated",
    }));
    events.push({ ...trace().events[0], seq: 267, round: 500 }, { ...trace().events[1], seq: 268, round: 500 });
    await serve(path => {
      if (!path.startsWith("/telemetry/trace?")) return ordinaryResponse(path);
      const since = Number(new URL(path, "http://router.invalid").searchParams.get("since"));
      const page = events.filter(event => Number(event.seq) > since).slice(0, 256);
      const cursor = Number(page.at(-1)?.seq ?? events.at(-1)!.seq);
      return { body: { ...trace(), events: page, cursor, high_water: events.at(-1)!.seq,
        has_more: cursor < Number(events.at(-1)!.seq) } };
    });
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).resolves.toBe("azure-openai");
    events.push({ ...trace().events[1], seq: 269, round: 500, call_id: "call-2" });
    await expect(observations.match("call-2", name, "azure-openai", "reviewed")).resolves.toBe("azure-openai");
    expect(requests.filter(request => request.url?.startsWith("/telemetry/trace")).map(request => request.url))
      .toEqual(["/telemetry/trace?since=10", "/telemetry/trace?since=266", "/telemetry/trace?since=266"]);
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).rejects.toThrow("Repeated native telemetry call correlation");
    expect(JSON.parse((await observations.artifact())!)).toMatchObject({ state: "observed", correlated_tools: 2 });
  });

  it("advances current correlation through recorder rollover without claiming a complete Activity export", async () => {
    let events: Record<string, unknown>[] = [];
    let highWater = 10;
    let dropped = 0;
    await serve(path => {
      if (!path.startsWith("/telemetry/trace?")) return ordinaryResponse(path);
      const since = Number(new URL(path, "http://router.invalid").searchParams.get("since"));
      const page = events.filter(event => Number(event.seq) > since).slice(0, 256);
      const cursor = Number(page.at(-1)?.seq ?? highWater);
      return { body: { ...trace(), events: page, cursor, high_water: highWater,
        has_more: cursor < highWater, dropped_events: dropped } };
    });
    const observations = await NativeToolObservations.open(new AbortController().signal);
    for (let round = 1; round <= 600; round++) {
      events.push({ ...trace().events[0], seq: ++highWater, round },
        { ...trace().events[1], seq: ++highWater, round, call_id: `call-${round}` });
      if (events.length > 1024) { dropped += events.length - 1024; events = events.slice(-1024); }
      await expect(observations.match(`call-${round}`, name, "azure-openai", "reviewed")).resolves.toBe("azure-openai");
    }
    expect(dropped).toBeGreaterThan(0);
    expect(JSON.parse((await observations.artifact())!)).toMatchObject({ state: "unavailable", correlated_tools: 600, rounds: [] });
  });

  it("keeps concurrent round matches on their captured windows and refuses duplicate claims", async () => {
    await serve(path => path.startsWith("/telemetry/trace?") ? { body: { ...trace(), cursor: 14, high_water: 14,
      events: [...trace().events, { ...trace().events[0], seq: 13, round: 4 },
        { ...trace().events[1], seq: 14, round: 4, call_id: "call-2" }] } } : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(Promise.all([
      observations.match("call-2", name, "azure-openai", "reviewed"),
      observations.match("call-1", name, "azure-openai", "reviewed"),
    ])).resolves.toEqual(["azure-openai", "azure-openai"]);
    const other = await NativeToolObservations.open(new AbortController().signal);
    const results = await Promise.allSettled([
      other.match("call-1", name, "azure-openai", "reviewed"),
      other.match("call-1", name, "azure-openai", "reviewed"),
    ]);
    expect(results.filter(result => result.status === "fulfilled")).toHaveLength(1);
    expect(results.filter(result => result.status === "rejected")).toHaveLength(1);
  });

  it("does not invent coverage when no native tool is correlated", async () => {
    await serve(ordinaryResponse);
    const observations = await NativeToolObservations.open(new AbortController().signal);
    expect(await observations.artifact()).toBeUndefined();
    expect(requests).toHaveLength(1);
  });
  it("exports only correlated proposals, proposing rounds and genuine immediate reports without private payloads", async () => {
    let exportTrace = false;
    await serve(path => {
      if (!exportTrace || !path.startsWith("/telemetry/trace?")) return ordinaryResponse(path);
      const events = [...trace().events,
        { kind: "tool_proposed", source: "model-proposed", scope_id: scope, seq: 13, round: 3, call_id: "unrelated", name },
        { kind: "tool_result", source: "harness-reported", scope_id: scope, seq: 14, round: 3, call_id: "call-1", name,
          ok: true, ms: 2, result_summary: { kind: "file_read", bytes: 8, returned_bytes: 8, truncated: false, content: "private" },
          arguments: { path: "/private" }, result: "private", url: "https://private.example" },
        { ...trace().events[0], seq: 15, round: 4 },
        { kind: "tool_result", source: "harness-reported", scope_id: scope, seq: 16, round: 3,
          call_id: "call-1", name, ok: true, reported_in_round: 4 }];
      return { body: { ...trace(), cursor: 16, high_water: 16, events } };
    });
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    await observations.reportAuthorized("call-1", name, true, 2);
    exportTrace = true;
    const content = (await observations.artifact())!;
    const result = JSON.parse(content);
    expect(result).toMatchObject({ state: "observed", coverage: "native-tool-correlated-only", correlated_tools: 1, rounds: [3] });
    expect(result).not.toHaveProperty("responses");
    expect(result.trace.events.map((event: { seq: number }) => event.seq)).toEqual([11, 12, 14]);
    expect(result.trace.events[2]).not.toHaveProperty("reported_in_round");
    expect(result.trace.events[2].result_summary).toEqual({ kind: "file_read", bytes: 8, returned_bytes: 8, truncated: false });
    expect(content).not.toContain("private");
    expect(content).not.toContain("unrelated");
  });
  it.each([
    (value: any) => ({ ...value, scope_id: "reset" }),
    (value: any) => ({ ...value, has_more: true }),
    (value: any) => ({ ...value, dropped_events: 1 }),
    (value: any) => ({ ...value, events: [] }),
    (value: any) => ({ ...value, events: [...value.events].reverse() }),
    (value: any) => ({ ...value, events: value.events.map((event: any) => ({ ...event, source: "invented" })) }),
    (value: any) => ({ ...value, events: value.events.map((event: any) => ({ ...event, round: 7 })) }),
    (value: any) => ({ ...value, events: value.events.map((event: any) => event.kind === "round" ? { ...event, model: "other" } : event) }),
  ])("retains explicit unavailable coverage on failed export validation (%#)", async mutate => {
    let exporting = false;
    await serve(path => exporting && path.startsWith("/telemetry/trace?") ? { body: mutate(trace()) } : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    exporting = true;
    expect(JSON.parse((await observations.artifact())!)).toMatchObject({ state: "unavailable", correlated_tools: 1, rounds: [3] });
  });
  it("retains only individually correlated router policy and fetch records, independently of harness reporting", async () => {
    const webName = "web_fetch";
    const base = trace();
    base.events[1].name = webName;
    const proposal = { ...base.events[1], seq: 13, call_id: "call-2" };
    let exporting = false;
    const operation = (seq: number, call_id: string, extra: Record<string, unknown> = {}) => ({
      kind: "router_operation", source: "router-policy", scope_id: scope, seq, round: 3, call_id, name: webName,
      operation: "egress_policy", flag: "egress_check", allowed: true, http_status: null, complete: null,
      destination_host: "source.example", destination_port: 443, ms: 2, ...extra,
    });
    await serve(path => path.startsWith("/telemetry/trace?") ? { body: {
      ...base, cursor: exporting ? 20 : 13, high_water: exporting ? 20 : 13,
      events: [...base.events, proposal, ...(exporting ? [
        operation(14, "call-2", { destination_host: "second.example" }), operation(15, "unrelated"),
        operation(16, "call-1", { url: "https://source.example/secret?token=private", headers: { Authorization: "private" } }),
        operation(17, "call-1", { source: "router", operation: "egress_fetch", flag: "response", allowed: null, http_status: 403, complete: true }),
        operation(18, "call-1", { destination_host: "redirect.example" }),
        operation(19, "call-1", { round: 8 }), operation(20, "call-1", { name: "wrong" }),
      ] : [])],
    } } : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", webName, "azure-openai", "reviewed");
    await observations.match("call-2", webName, "azure-openai", "reviewed");
    exporting = true;
    const content = (await observations.artifact())!;
    const result = JSON.parse(content);
    expect(result).toMatchObject({ state: "observed", correlated_tools: 2 });
    expect(result.trace.events.map((event: any) => event.seq)).toEqual([11, 12, 13, 14, 16, 17, 18]);
    expect(result.trace.events.some((event: any) => event.kind === "tool_result")).toBe(false);
    expect(result.trace.events[5]).toMatchObject({ source: "router", http_status: 403, allowed: null });
    expect(content).not.toMatch(/private|secret|Authorization|unrelated/);
  });
  it("does not export a completion never reported by this invocation", async () => {
    await serve(path => path.startsWith("/telemetry/trace?") ? { body: { ...trace(), cursor: 13, high_water: 13,
      events: [...trace().events, { kind: "tool_result", source: "harness-reported", scope_id: scope,
        seq: 13, round: 3, call_id: "call-1", name, ok: true }] } } : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    expect(JSON.parse((await observations.artifact())!).trace.events.map((event: any) => event.kind)).toEqual(["round", "tool_proposed"]);
  });
  it("exports after cancellation and converts refused telemetry reads into unavailable evidence", async () => {
    let refused = false;
    await serve(path => refused && path.startsWith("/telemetry/trace?") ? { body: {}, status: 409 } : ordinaryResponse(path));
    const controller = new AbortController();
    const observations = await NativeToolObservations.open(controller.signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    controller.abort();
    expect(JSON.parse((await observations.artifact())!).state).toBe("observed");
    refused = true;
    expect(JSON.parse((await observations.artifact())!).state).toBe("unavailable");
  });
  it("uses the actual nonzero cursor and scope without inventing inference round ownership", async () => {
    await serve(ordinaryResponse);
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    expect(requests.map(req => req.url)).toEqual(["/telemetry/cursor", "/telemetry/trace?since=10"]);
    expect(requests[1].headers["x-kars-service-scope"]).toBe(scope);
    expect(requests[1].headers["x-kars-telemetry-scope"]).toBeUndefined();
    await expect(observations.reportAuthorized("other-call", name, true, 0)).rejects.toThrow("correlation");
    await expect(observations.reportAuthorized("call-1", "file_read", true, 0)).rejects.toThrow("correlation");
    expect(requests).toHaveLength(2);
    await observations.reportAuthorized("call-1", name, true, 2.7, { kind: "file_read", bytes: 8, returned_bytes: 4, truncated: true });
    expect(requests[2].body).toEqual({ scope_id: scope, call_id: "call-1", ok: true, latency_ms: 3,
      result_summary: { kind: "file_read", bytes: 8, returned_bytes: 4, truncated: true } });
    await expect(observations.reportAuthorized("call-1", name, true, 0)).rejects.toThrow("correlation");
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).rejects.toThrow("Repeated");
    expect(requests).toHaveLength(3);
  });
  it.each(["azure-openai", "azure-foundry", "github-copilot"])("retains policy-selected upstream %s separately from the local SDK provider", async provider => {
    await serve(path => {
      const response = ordinaryResponse(path);
      if (path.startsWith("/telemetry/trace?")) {
        const data = trace();
        data.events[0].provider = provider;
        return { body: data };
      }
      return response;
    });
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).resolves.toBe(provider);
    await observations.reportAuthorized("call-1", name, true, 1);
    expect(requests).toHaveLength(3);
  });
  it.each([undefined, "", "bad provider"])("rejects missing or malformed upstream identity (%#)", async provider => {
    await serve(path => path.startsWith("/telemetry/trace?")
      ? { body: { ...trace(), events: [{ ...trace().events[0], provider }, trace().events[1]] } }
      : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).rejects.toThrow("unambiguous");
    await expect(observations.reportAuthorized("call-1", name, true, 0)).rejects.toThrow("correlation");
  });
  it("rejects a different local SDK transport without inferring provider aliases", async () => {
    await serve(ordinaryResponse);
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(observations.match("call-1", name, "anthropic", "reviewed")).rejects.toThrow("transport provider");
    expect(requests).toHaveLength(1);
  });
  it.each([
    { body: "not JSON" }, { body: "x".repeat(4097) }, { body: {}, status: 409 }, { body: null },
  ])("rejects malformed or refused cursor discovery (%#)", async reply => {
    await serve(() => reply);
    await expect(NativeToolObservations.open(new AbortController().signal)).rejects.toThrow();
    expect(requests).toHaveLength(1);
  });
  it.each([
    { body: "not JSON" }, { body: "x".repeat(48 * 1024 + 1) }, { body: {}, status: 409 },
    { body: { ...trace(), events: [null] } },
    { body: { ...trace(), has_more: true } },
    { body: { ...trace(), has_more: undefined, truncated: false } },
    { body: { ...trace(), cursor: 11 } },
    { body: { ...trace(), cursor: 11, high_water: 11 } },
    { body: { ...trace(), coverage: "returned-responses-only" } },
    { body: { ...trace(), events: [...trace().events, { ...trace().events[0] }] } },
  ])("does not manufacture a match from a malformed, refused or ambiguous trace (%#)", async reply => {
    await serve(path => path.startsWith("/telemetry/trace?") ? reply : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await expect(observations.match("call-1", name, "azure-openai", "reviewed")).rejects.toThrow();
    await expect(observations.reportAuthorized("call-1", name, true, 0)).rejects.toThrow("correlation");
    expect(requests).toHaveLength(2);
  });
  it.each([
    { body: { recorded: false } }, { body: { recorded: true, source: "router", enforcement_changed: false } },
    { body: { recorded: true, source: "harness-reported", enforcement_changed: true } },
    { body: "invalid" }, { body: "x".repeat(4097) }, { body: {}, status: 409 },
  ])("rejects unconfirmed outcome receipts without retrying (%#)", async reply => {
    await serve(path => path === "/telemetry/tool" ? reply : ordinaryResponse(path));
    const observations = await NativeToolObservations.open(new AbortController().signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    await expect(observations.reportAuthorized("call-1", name, false, 3)).rejects.toThrow();
    await expect(observations.reportAuthorized("call-1", name, false, 3)).rejects.toThrow("correlation");
    expect(requests).toHaveLength(3);
  });
  it("honors cancellation for discovery while allowing a bounded already-authorized failure report", async () => {
    await serve(ordinaryResponse);
    const controller = new AbortController();
    const observations = await NativeToolObservations.open(controller.signal);
    await observations.match("call-1", name, "azure-openai", "reviewed");
    controller.abort();
    await expect(observations.match("another-call", name, "azure-openai", "reviewed")).rejects.toThrow();
    await observations.reportAuthorized("call-1", name, false, 1, { kind: "failure", reason: "tool_execution_failed" });
    expect(requests).toHaveLength(3);
  });
});
