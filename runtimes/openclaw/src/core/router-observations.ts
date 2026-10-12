// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import http, { type IncomingHttpHeaders } from "node:http";
import { routerUrl } from "./router-client.js";

export const ROUTER_OBSERVATIONS_ARTIFACT = "kars-router-observations.json";
const MAX_ARTIFACT_BYTES = 32 * 1024;

export type ToolResultSummary =
  | { kind: "file_read"; bytes: number; returned_bytes: number; truncated: boolean }
  | { kind: "file_write"; bytes: number; artifact_name?: string }
  | { kind: "failure"; reason: "filesystem_operation_failed" | "invalid_arguments" | "policy_denied" | "reviewed_phase_denied" | "tool_execution_failed" };

/** Select and report to the existing router recorder using returned response IDs. */
export class ReturnedRouterResponses {
  private scope?: string;
  private rounds = new Set<number>();
  private responses = 0;
  private advertised = false;
  private invalid = false;

  scopeId(): string | undefined {
    return this.invalid ? undefined : this.scope;
  }

  async reportTool(callId: string, ok: boolean, latencyMs: number, summary?: ToolResultSummary): Promise<void> {
    const scope = this.scopeId();
    if (!scope) throw new Error("No valid returned router scope");
    const body = JSON.stringify({ scope_id: scope, call_id: callId, ok,
      latency_ms: Math.min(3_600_000, Math.max(0, Math.round(latencyMs))),
      ...(summary ? { result_summary: summary } : {}) });
    await new Promise<void>((resolve, reject) => {
      const request = http.request(routerUrl("/telemetry/tool"), {
        method: "POST", signal: AbortSignal.timeout(2000),
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
      }, response => {
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > 4096) { response.destroy(new Error("Tool receipt exceeds limit")); return; }
          chunks.push(chunk);
        });
        response.on("end", () => {
          try {
            const receipt = JSON.parse(Buffer.concat(chunks).toString("utf8"));
            if (response.statusCode !== 200 || receipt.recorded !== true
              || receipt.source !== "harness-reported" || receipt.enforcement_changed !== false) {
              throw new Error("Router refused tool completion report");
            }
            resolve();
          } catch (error) { reject(error); }
        });
        response.on("error", reject);
        response.on("aborted", () => reject(new Error("Tool receipt aborted")));
      });
      request.on("error", reject);
      request.end(body);
    });
  }

  capture(headers: IncomingHttpHeaders): void {
    this.responses++;
    const scope = headers["x-kars-telemetry-scope"];
    const rawRound = headers["x-kars-telemetry-round"];
    if (scope === undefined && rawRound === undefined) return;
    this.advertised = true;
    const round = Number(rawRound);
    if (typeof scope !== "string" || !/^[A-Za-z0-9._:-]{1,128}$/.test(scope)
      || typeof rawRound !== "string" || !/^[1-9][0-9]*$/.test(rawRound)
      || !Number.isSafeInteger(round) || this.rounds.size >= 32
      || this.rounds.has(round) || (this.scope !== undefined && this.scope !== scope)) {
      this.invalid = true;
      return;
    }
    this.scope = scope;
    this.rounds.add(round);
  }

  async artifact(): Promise<string | undefined> {
    if (!this.advertised) return undefined;
    const rounds = [...this.rounds].sort((a, b) => a - b);
    const base = {
      version: 1, source: "runtime-forwarded-router-observations",
      coverage: "returned-responses-only", responses: this.responses,
      scope_id: this.scope ?? null, rounds,
    };
    const unavailable = (reason: string) => JSON.stringify({ ...base, state: "unavailable", reason });
    if (this.invalid || !this.scope) return unavailable("Response correlation changed or was invalid");
    try {
      const trace = await readSelection(this.scope, rounds);
      if (trace.scope_id !== this.scope || trace.coverage !== "returned-responses-only"
        || trace.durable !== false || JSON.stringify(trace.rounds) !== JSON.stringify(rounds)
        || !Array.isArray(trace.events) || trace.events.length > 256
        || !Array.isArray(trace.missing_rounds) || !trace.missing_rounds.every((n: unknown) => typeof n === "number" && rounds.includes(n))
        || typeof trace.dropped_events !== "number" || !Number.isSafeInteger(trace.dropped_events) || trace.dropped_events < 0
        || typeof trace.truncated !== "boolean"
        || !trace.events.every((event: Record<string, unknown>) => event && typeof event === "object"
          && event.scope_id === this.scope && typeof event.round === "number" && rounds.includes(event.round)
          && (event.kind === "round" || event.kind === "tool_proposed"
            || (event.kind === "tool_result" && typeof event.reported_in_round === "number" && rounds.includes(event.reported_in_round))))) {
        return unavailable("Router selection did not match returned response IDs");
      }
      const events = trace.events.map((event: Record<string, unknown>) => {
        const fields = event.kind === "round"
          ? ["provider", "model", "http_status", "accepted", "outcome", "usage_state", "finish_reason", "partial_observation", "ms", "tool_calls_observed"]
          : ["call_id", "name", "ok", "reported_in_round", "ms"];
        const selected = Object.fromEntries(["kind", "round", "scope_id", "seq", "source", "observed_at", ...fields]
          .filter(key => Object.hasOwn(event, key)).map(key => [key, event[key]]));
        if (event.kind === "round" && event.usage && typeof event.usage === "object") {
          const usage = event.usage as Record<string, unknown>;
          selected.usage = Object.fromEntries(["prompt_tokens", "completion_tokens", "total_tokens"]
            .map(key => [key, usage[key] ?? null]));
        }
        if (event.kind === "tool_result" && event.result_summary && typeof event.result_summary === "object") {
          const summary = event.result_summary as Record<string, unknown>;
          const keys = summary.kind === "file_read" ? ["kind", "bytes", "returned_bytes", "truncated"]
            : summary.kind === "file_write" ? ["kind", "bytes", "artifact_name"]
              : summary.kind === "failure" ? ["kind", "reason"] : [];
          if (!keys.length) throw new Error("Unknown tool summary");
          selected.result_summary = Object.fromEntries(keys.filter(key => Object.hasOwn(summary, key))
            .map(key => [key, summary[key]]));
        }
        return selected;
      });
      const content = JSON.stringify({ ...base, state: "observed", trace: {
        scope_id: trace.scope_id, rounds: trace.rounds, coverage: trace.coverage, durable: false,
        missing_rounds: trace.missing_rounds, dropped_events: trace.dropped_events,
        truncated: trace.truncated, events,
      } });
      if (Buffer.byteLength(JSON.stringify({ [ROUTER_OBSERVATIONS_ARTIFACT]: content }), "utf8") > MAX_ARTIFACT_BYTES) {
        return unavailable("Router selection exceeded the observation artifact limit");
      }
      return content;
    } catch {
      return unavailable("Router observations unavailable after execution (reset, expiry or read failure)");
    }
  }
}

function readSelection(scope: string, rounds: number[]): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const url = routerUrl(`/telemetry/trace?rounds=${rounds.join(",")}`);
    const request = http.get(url, { headers: { "x-kars-service-scope": scope }, signal: AbortSignal.timeout(2000) }, response => {
      const chunks: Buffer[] = [];
      let size = 0;
      response.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > 48 * 1024) { response.destroy(new Error("Observation response too large")); return; }
        chunks.push(chunk);
      });
      response.on("end", () => {
        if (response.statusCode !== 200) { reject(new Error("Observation read failed")); return; }
        try { resolve(JSON.parse(Buffer.concat(chunks).toString("utf8"))); }
        catch (error) { reject(error); }
      });
      response.on("error", reject);
      response.on("aborted", () => reject(new Error("Observation read aborted")));
    });
    request.on("error", reject);
  });
}
