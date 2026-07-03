// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Native-agent task delegation — extracted from plugin.ts in S15.f.2
// to give plugin.ts headroom under the §4.2 800-LOC cap.
//
// Pure stdlib (`node:child_process`, `node:fs`) — does not touch any
// AGT singleton. Owned by the AGT mesh task path which calls it when a
// peer issues a `task_request` and the local sandbox's openclaw.json
// permits delegation to the native OpenClaw agent.

interface TaskLogger {
  info(m: string): void;
  warn(m: string): void;
}

/**
 * Delegate a task to the native OpenClaw agent loop running in the Gateway.
 * This gives the sub-agent access to ALL OpenClaw tools (exec, process, web_search,
 * web_fetch, browser, cron, read/write, etc.) plus all kars plugin skills
 * (foundry_memory, foundry_web_search, foundry_code, etc.).
 *
 * The task is sent via `openclaw agent --message` which goes through the Gateway's
 * full agent pipeline (AGENTS.md, SOUL.md, TOOLS.md, skills, tool policy, etc.).
 */
export async function delegateToNativeAgent(
  taskContent: string,
  fromAgent: string,
  log: TaskLogger,
): Promise<string> {
  const { spawn } = await import("node:child_process");

  // Stable session ID per sender → maintains conversation context across tasks
  const sessionId = `agt-task-${fromAgent}`;
  const taskText = typeof taskContent === "string" ? taskContent : JSON.stringify(taskContent);

  log.info(`Delegating task to native OpenClaw agent (session: ${sessionId})`);

  const fs = await import("node:fs");
  try { fs.mkdirSync("/tmp/agt-delegate-home", { recursive: true }); } catch {}

  return new Promise<string>((resolve, reject) => {
    const child = spawn("openclaw", [
      "agent",
      "--message", taskText,
      "--session-id", sessionId,
      "--timeout", "1500",
      "--json",
    ], {
      env: {
        ...process.env,
        // Separate HOME so the agent gets its own device fingerprint and doesn't
        // conflict with the node host's "node" role pairing.
        HOME: "/tmp/agt-delegate-home",
        AGT_SKIP_INIT: "1",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    const chunks: Buffer[] = [];
    // OpenClaw writes all output (plugin logs + JSON result) to stderr
    child.stderr.on("data", (chunk: Buffer) => chunks.push(chunk));
    child.stdout.on("data", (chunk: Buffer) => chunks.push(chunk));

    // External watchdog above the agent's own `--timeout` so a wedged child is
    // still reaped, but generous enough for real multi-round missions (the
    // controller's mesh delivery has its own absolute ceiling above this).
    const timer = setTimeout(() => { child.kill("SIGTERM"); }, 1_530_000);

    child.on("close", (code: number | null, signal: NodeJS.Signals | null) => {
      clearTimeout(timer);
      const output = Buffer.concat(chunks).toString("utf-8");

      // A parsed JSON result with real text is authoritative success even if the
      // process then exited non-zero on teardown. Extract the last top-level
      // { ... } block.
      const jsonMatch = output.match(/\n(\{[\s\S]*\})\s*$/);
      if (jsonMatch) {
        try {
          const result = JSON.parse(jsonMatch[1]);
          const text = result?.reply?.text || result?.text || "";
          if (text) {
            log.info(`Native agent responded (${text.length} chars, session: ${sessionId})`);
            return resolve(text);
          }
        } catch { /* fall through */ }
      }

      // No valid JSON result. If the process failed (non-zero exit or killed by
      // signal — e.g. the watchdog SIGTERM, OOM, or a runtime crash), this is an
      // ERROR, not a deliverable: rejecting makes the caller stamp the run
      // `ok:false` instead of shipping scraped log noise as a successful mission.
      if (code !== 0 || signal) {
        const tail = output.slice(-800).trim();
        log.warn(`Native agent exited abnormally (code=${code}, signal=${signal}); ${output.length} bytes captured`);
        return reject(new Error(`native agent failed (code=${code}, signal=${signal}): ${tail || "no output"}`));
      }

      // Clean exit but no JSON envelope: fall back to the stripped textual
      // output (older agent output shapes). Only treat non-empty text as success.
      const lines = output.split("\n").filter((l: string) =>
        !l.startsWith("[plugins]") && !l.startsWith("[") && l.trim());
      const response = lines.join("\n").trim();
      if (response) {
        log.info(`Native agent responded (${response.length} chars, session: ${sessionId})`);
        return resolve(response);
      }

      log.warn(`Native agent returned empty response (${output.length} bytes captured)`);
      reject(new Error("Native agent returned empty response"));
    });

    child.on("error", (err: Error) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

/**
 * The native agent's `--json` reply is a structured envelope
 * (`{ runId, status, summary, result: { payloads: [ { text } ] } }`). Extract
 * the human deliverable text so callers ship clean prose, not raw escaped JSON
 * — which otherwise becomes both an ugly deliverable AND a control-char-laden
 * "binary" fallback artifact. Plain-text replies (no envelope) pass through.
 */
export function extractNativeDeliverable(raw: string): string {
  const t = (raw ?? "").trim();
  if (!t.startsWith("{") && !t.startsWith("[")) return raw;
  try {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const v: any = JSON.parse(t);
    const payloads = v?.result?.payloads;
    if (Array.isArray(payloads)) {
      const joined = payloads
        .map((p: { text?: unknown }) => (typeof p?.text === "string" ? p.text : ""))
        .filter(Boolean)
        .join("\n\n");
      if (joined.trim()) return joined;
    }
    for (const [a, b] of [["reply", "text"], ["result", "text"]] as const) {
      const x = v?.[a]?.[b];
      if (typeof x === "string" && x.trim()) return x;
    }
    for (const k of ["text", "output", "summary"]) {
      if (typeof v?.[k] === "string" && v[k].trim()) return v[k];
    }
  } catch {
    /* not JSON — a plain-text reply */
  }
  return raw;
}
