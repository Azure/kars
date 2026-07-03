// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Router-sourced per-task execution trace.
//
// The inference router observes every model call an agent makes and records a
// per-task event log (round + tool events) under /telemetry. This is the
// honest, harness-neutral source of a mission's activity + token telemetry —
// it replaces the retired in-process loop's self-reported `onTrace`. Because
// the REAL OpenClaw agent's calls all flow through the same router, delegating
// a task to the native agent and then reading this trace yields a faithful
// record without any hand-rolled instrumentation.

import { routerUrl } from "./router-client.js";

/** One router-emitted trace event (round or tool). Mirrors the trace.json shape
 *  the controller persists to `kars-mission-trace-<task>` and the Bridge renders. */
export interface RouterTraceEvent {
  kind: "round" | "tool";
  round: number;
  // round fields
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  finish_reason?: string;
  tool_calls?: number;
  // tool fields
  name?: string;
  args_preview?: string;
  result_preview?: string;
  ok?: boolean;
  ms?: number;
  ts?: string;
  seq?: number;
}

type Logger = { info: (m: string) => void; warn: (m: string) => void };

async function getJson(path: string, timeoutMs = 4000): Promise<unknown> {
  const http = await import("node:http");
  return new Promise((resolve, reject) => {
    const req = http.request(routerUrl(path), { method: "GET", timeout: timeoutMs }, (res) => {
      let body = "";
      res.on("data", (c: Buffer) => { body += c.toString(); });
      res.on("end", () => {
        try { resolve(JSON.parse(body)); } catch (e) { reject(e); }
      });
    });
    req.on("error", reject);
    req.on("timeout", () => { req.destroy(); reject(new Error("router telemetry timeout")); });
    req.end();
  });
}

/** Current telemetry high-water cursor. Record this BEFORE running a task; pass
 *  it to {@link fetchTaskTrace} afterwards to get exactly that task's events.
 *  Returns 0 on any error (so the worst case is a slightly wider trace window). */
export async function fetchTelemetryCursor(log: Logger): Promise<number> {
  try {
    const data = await getJson("/telemetry/cursor") as { cursor?: number };
    return typeof data?.cursor === "number" ? data.cursor : 0;
  } catch (e) {
    log.warn(`router telemetry cursor unavailable (continuing): ${(e as Error).message}`);
    return 0;
  }
}

/** Events with `seq > sinceCursor` — the executed task's router-sourced trace.
 *  Returns [] on any error; the caller still ships the deliverable + artifacts. */
export async function fetchTaskTrace(sinceCursor: number, log: Logger): Promise<RouterTraceEvent[]> {
  try {
    const data = await getJson(`/telemetry/trace?since=${sinceCursor}`) as { events?: RouterTraceEvent[] };
    return Array.isArray(data?.events) ? data.events : [];
  } catch (e) {
    log.warn(`router telemetry trace unavailable (continuing): ${(e as Error).message}`);
    return [];
  }
}
