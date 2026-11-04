// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { request } from "node:http";
import { MAX_MISSION_ACTIVITY_BYTES, MAX_ROUTER_OBSERVATION_EVENTS, MAX_ROUTER_OBSERVATION_PAGE_BYTES, MAX_ROUTER_OBSERVATION_ROUNDS } from "@kars/mesh/dist/mission-protocol.js";
import { routerUrl } from "./router-client.js";
import { ROUTER_OBSERVATIONS_ARTIFACT, sanitizeRouterEvent, type ToolResultSummary } from "./router-observations.js";

const object = (value: unknown): Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value)
  ? value as Record<string, unknown> : {};

async function exchange(path: string, signal: AbortSignal, scope?: string, payload?: unknown): Promise<Record<string, unknown>> {
  const body = payload === undefined ? undefined : JSON.stringify(payload);
  return new Promise((resolve, reject) => {
    const req = request(routerUrl(path), { method: body === undefined ? "GET" : "POST",
      signal: AbortSignal.any([signal, AbortSignal.timeout(2000)]), headers: {
        ...(scope ? { "x-kars-service-scope": scope } : {}),
        ...(body === undefined ? {} : { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) }),
      } }, res => {
      const chunks: Buffer[] = [];
      let bytes = 0;
      res.on("data", (chunk: Buffer) => {
        bytes += chunk.length;
        if (bytes > (path.startsWith("/telemetry/trace?") ? MAX_ROUTER_OBSERVATION_PAGE_BYTES : 4096)) res.destroy(new Error("Native telemetry response exceeds limit"));
        else chunks.push(chunk);
      });
      res.on("error", reject);
      res.on("aborted", () => reject(new Error("Native telemetry response aborted")));
      res.on("end", () => {
        try {
          if (res.statusCode !== 200) throw new Error(`Native telemetry HTTP ${res.statusCode}`);
          resolve(object(JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks)))));
        } catch (error) { reject(error); }
      });
    });
    req.on("error", reject);
    req.end(body);
  });
}

/** A cursor is a search boundary, never proof that every subsequent round belongs to this run. */
export class NativeToolObservations {
  private readonly matched = new Map<string, string>();
  private readonly reported = new Set<string>();
  private readonly correlations = new Map<string, { round: number; proposal: number; model: string; provider: string }>();

  private matchCursor: number;

  private constructor(readonly scopeId: string, private readonly cursor: number, private readonly signal: AbortSignal) {
    this.matchCursor = cursor;
  }

  private async matchWindow(cursor: number, signal = this.signal, complete = false): Promise<Record<string, unknown>[]> {
    const start = cursor;
    let highWater: number | undefined;
    const events: Record<string, unknown>[] = [];
    do {
      const trace = await exchange(`/telemetry/trace?since=${cursor}`, signal, this.scopeId);
      if (trace.scope_id !== this.scopeId || trace.durable !== false || trace.coverage !== "router-observed-only"
        || !Number.isSafeInteger(trace.cursor) || (trace.cursor as number) < cursor
        || !Number.isSafeInteger(trace.high_water) || (trace.high_water as number) < (trace.cursor as number)
        || trace.has_more !== ((trace.cursor as number) < (trace.high_water as number))
        || !Number.isSafeInteger(trace.dropped_events) || (trace.dropped_events as number) < 0
        || (complete && trace.dropped_events !== 0)
        || (highWater !== undefined && (trace.high_water as number) < highWater)
        || !Array.isArray(trace.events) || trace.events.length > 256) {
        throw new Error("Native tool correlation is unavailable, truncated or belongs to another router scope");
      }
      highWater ??= trace.high_water as number;
      if (highWater - start > MAX_ROUTER_OBSERVATION_EVENTS) throw new Error("Native telemetry window exceeds retention");
      for (const item of trace.events) {
        const event = object(item);
        if (event.scope_id !== this.scopeId || event.seq !== cursor + 1
          || (event.seq as number) > (trace.cursor as number)) {
          throw new Error("Native tool correlation contains a gap or invalid event sequence");
        }
        cursor = event.seq as number;
        if (cursor <= highWater) events.push(event);
      }
      if (cursor !== trace.cursor || (cursor < highWater && !trace.events.length)) {
        throw new Error("Native tool correlation did not advance to its reported cursor");
      }
    } while (cursor < highWater);
    return events;
  }

  static async open(signal: AbortSignal): Promise<NativeToolObservations> {
    const data = await exchange("/telemetry/cursor", signal);
    if (typeof data.scope_id !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/.test(data.scope_id)
      || !Number.isSafeInteger(data.cursor) || (data.cursor as number) < 0 || data.durable !== false) {
      throw new Error("Router did not issue a valid ephemeral telemetry cursor");
    }
    return new NativeToolObservations(data.scope_id, data.cursor as number, signal);
  }

  async match(callId: string, name: string, runtimeProvider: string, model: string): Promise<string> {
    if (runtimeProvider !== "azure-openai") throw new Error("Unsupported native router transport provider");
    if (!callId || this.matched.has(callId)) throw new Error("Repeated native telemetry call correlation");
    const cursor = this.matchCursor;
    const events = await this.matchWindow(cursor);
    if (this.matched.has(callId)) throw new Error("Repeated native telemetry call correlation");
    const proposals = events.filter(event => event.kind === "tool_proposed" && event.call_id === callId);
    const proposal = proposals[0];
    const inWindow = (event: Record<string, unknown>) => event.scope_id === this.scopeId
      && Number.isSafeInteger(event.seq) && (event.seq as number) > cursor
      && Number.isSafeInteger(event.round) && (event.round as number) > 0;
    const rounds = proposal ? events.filter(event => event.kind === "round" && event.round === proposal.round) : [];
    // OpenClaw's local provider names the router transport, not the policy-selected upstream.
    const routerProvider = rounds[0]?.provider;
    if (proposals.length !== 1 || proposal.name !== name || !inWindow(proposal) || rounds.length !== 1
      || !inWindow(rounds[0]) || typeof routerProvider !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/.test(routerProvider)
      || rounds[0].model !== model || rounds[0].accepted !== true) {
      throw new Error("Native tool lacks an unambiguous router proposal with matching call ID, name and model");
    }
    // Retain the proposing round for sibling calls, not the entire run's telemetry window.
    this.matchCursor = Math.max(this.matchCursor, (rounds[0].seq as number) - 1);
    this.matched.set(callId, name);
    this.correlations.set(callId, { round: proposal.round as number, proposal: proposal.seq as number, model, provider: routerProvider });
    return routerProvider;
  }

  async artifact(): Promise<string | undefined> {
    if (!this.correlations.size) return undefined;
    const correlations = new Map(this.correlations);
    const matched = new Map(this.matched);
    const reported = new Set(this.reported);
    const rounds = [...new Set([...correlations.values()].map(value => value.round))].sort((a, b) => a - b);
    const base = { version: 1, source: "runtime-forwarded-router-observations", coverage: "native-tool-correlated-only",
      correlated_tools: correlations.size, scope_id: this.scopeId, rounds };
    const unavailable = () => JSON.stringify({ ...base, rounds: rounds.length > MAX_ROUTER_OBSERVATION_ROUNDS ? [] : rounds, state: "unavailable",
      reason: "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)" });
    try {
      if (rounds.length > MAX_ROUTER_OBSERVATION_ROUNDS) return unavailable();
      const events = await this.matchWindow(this.cursor, AbortSignal.timeout(8000), true);
      for (const [id, correlation] of correlations) {
        const proposals = events.filter(event => event.kind === "tool_proposed" && event.call_id === id);
        const model = events.filter(event => event.kind === "round" && event.round === correlation.round);
        const results = events.filter(event => event.kind === "tool_result" && event.call_id === id && event.reported_in_round === undefined);
        if (proposals.length !== 1 || proposals[0].seq !== correlation.proposal
          || proposals[0].round !== correlation.round || proposals[0].name !== matched.get(id)
          || proposals[0].source !== "model-proposed" || model.length !== 1 || model[0].model !== correlation.model
          || model[0].source !== "router-upstream" || model[0].provider !== correlation.provider || model[0].accepted !== true
          || results.length > 1 || results.some(result => result.round !== correlation.round || result.name !== matched.get(id)
            || result.source !== "harness-reported" || (result.seq as number) <= correlation.proposal)) return unavailable();
      }
      // Export only individually matched calls and their proposing rounds, never the cursor's whole window.
      // Immediate completion reports have no reported_in_round; later request echoes are not additional outcomes.
      const selected = events.filter(event => {
        if (event.kind === "round") return rounds.includes(event.round as number);
        const correlation = correlations.get(event.call_id as string);
        return correlation && correlation.round === event.round && matched.get(event.call_id as string) === event.name
          && (event.kind === "tool_proposed" || (event.kind === "router_operation"
            && (event.seq as number) > correlation.proposal) || (event.kind === "tool_result"
            && event.reported_in_round === undefined && reported.has(event.call_id as string)));
      }).map(sanitizeRouterEvent);
      const content = JSON.stringify({ ...base, state: "observed", trace: {
        scope_id: this.scopeId, rounds, coverage: base.coverage, durable: false,
        missing_rounds: [], dropped_events: 0, truncated: false, events: selected,
      } });
      return Buffer.byteLength(JSON.stringify({ [ROUTER_OBSERVATIONS_ARTIFACT]: content })) <= MAX_MISSION_ACTIVITY_BYTES ? content : unavailable();
    } catch { return unavailable(); }
  }

  async reportAuthorized(callId: string, name: string, ok: boolean, latencyMs: number, summary?: ToolResultSummary): Promise<void> {
    if (this.matched.get(callId) !== name || this.reported.has(callId)) throw new Error("Native outcome lacks unique matched tool correlation");
    this.reported.add(callId);
    // Report an already-authorized outcome even when the native invocation was cancelled during its effect.
    const receipt = await exchange("/telemetry/tool", AbortSignal.timeout(2000), this.scopeId, {
      scope_id: this.scopeId, call_id: callId, ok,
      latency_ms: Math.min(3_600_000, Math.max(0, Math.round(latencyMs))),
      ...(summary ? { result_summary: summary } : {}),
    });
    if (receipt.recorded !== true || receipt.source !== "harness-reported" || receipt.enforcement_changed !== false) {
      throw new Error("Router refused native tool completion report");
    }
  }
}
