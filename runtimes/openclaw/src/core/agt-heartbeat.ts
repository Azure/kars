// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// AGT mesh heartbeat & memory-notify helpers — extracted from plugin.ts in S15.f.5.
//
// These three helpers all read AGT singleton state owned by plugin.ts. To
// avoid threading the entire singleton bag through call chains, callers pass
// the state references explicitly. plugin.ts wraps each helper with a thin
// no-arg/log-only wrapper that captures its module-scope `let` vars.
//
// - recordMeshSession: posts a session reputation update to the registry via
//   the router proxy. Read-only on identity, fail-soft on errors.
// - agtReconnect: re-establishes a dropped mesh client connection. Mutates
//   the connected flag via setConnected().
// - notifyInboxToMemory: rewrites the AGT_INBOX section of MEMORY.md so the
//   wrapped LLM sees pending peer messages in its next round.

import { routerUrl } from "./router-client.js";

interface MeshIdentity {
  amid: string;
  signTimestamp: () => Promise<[string, string]>;
}
interface MeshClient {
  disconnect: () => Promise<void>;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  connect: (opts: { displayName: string; capabilities: string[] }) => Promise<any>;
}
interface InboxEntry {
  from_amid: string;
  from_agent: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  content: any;
  timestamp: string;
  id: string;
  message_type?: string;
}
type MeshLogger = { info: (m: string) => void; warn: (m: string) => void };

/**
 * Post a completed mesh session record to the AGT registry so reputation /
 * session counters update for the responder. All errors are logged but
 * swallowed — this is best-effort telemetry.
 */
export async function recordMeshSession(
  identity: MeshIdentity | null,
  meshClient: MeshClient | null,
  targetAmid: string,
  sessionId: string,
  intent: string,
  outcome: "success" | "failed" | "timeout",
  startedAt: string,
): Promise<void> {
  if (!identity || !meshClient) return;
  // AGT mode: registry has no `/v1/registry/reputation/session` endpoint.
  // Per-agent reputation is already submitted via MeshClient.submitReputation
  // (see mesh-plugin agt-transport.ts) — this session-counter call is a
  // vendored-only artifact. Skipping avoids the 404 spam (once per mesh
  // reply, ~2/sec under load) in registry logs.
  const provider = (process.env.KARS_MESH_PROVIDER ?? "agt")
    .trim()
    .toLowerCase();
  if (provider === "agt") return;
  try {
    const [timestamp, signature] = await identity.signTimestamp();
    const http = await import("node:http");
    const body = JSON.stringify({
      session_id: sessionId,
      initiator_amid: identity.amid,
      receiver_amid: targetAmid,
      intent,
      outcome,
      started_at: startedAt,
      reporter_amid: identity.amid,
      timestamp,
      signature,
    });
    await new Promise<void>((resolve, reject) => {
      const req = http.request(routerUrl("/agt/registry/registry/reputation/session"), {
        method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
        timeout: 5000,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
      }, (res: any) => {
        res.resume();
        res.on("end", () => resolve());
      });
      req.on("error", reject);
      req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
      req.write(body);
      req.end();
    });
    console.log(`[kars] recordMeshSession: ${outcome} for ${sessionId}`);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } catch (e: any) {
    console.error(`[kars] recordMeshSession failed: ${e.message}`);
  }
}

/**
 * Re-establish a dropped mesh client connection. The SDK sets
 * `client.connected = true` even on transport failure, so we explicitly
 * disconnect first to reset stale state. Caller must update its
 * `connected` flag via `setConnected` on success.
 */
export async function agtReconnect(
  meshClient: MeshClient | null,
  isConnected: boolean,
  sandboxName: string,
  setConnected: (v: boolean) => void,
  log: MeshLogger,
): Promise<void> {
  if (!meshClient || isConnected) return;
  try {
    try { await meshClient.disconnect(); } catch { /* ignore */ }
    await meshClient.connect({
      displayName: sandboxName,
      capabilities: ["kars-agent", "task-execution", sandboxName],
    });
    setConnected(true);
    log.info("AGT mesh reconnected successfully");
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } catch (e: any) {
    log.warn(`AGT mesh reconnect failed: ${e.message}`);
  }
}

/**
 * Start a periodic `task_progress` heartbeat to `originatorAmid` while a
 * `task_request` is being processed by the in-process LLM tool-call loop.
 *
 * Why: the in-process task-loop path (sub-agent receives `task_request`,
 * runs `processTaskWithTools`, replies with `task_response`) used to leave
 * the parent silently waiting on a fixed 60s timeout. Long-running tool
 * sequences (e.g. multiple `foundry_web_search` calls) routinely exceeded
 * this and the parent gave up *before* the actual reply arrived. The
 * offload path (`agt-offload.ts`) already solved this with periodic
 * `offload_progress` pings; this is the in-process equivalent so the
 * parent's wait loop can extend the idle clock as long as the sub-agent
 * keeps signalling progress.
 *
 * Returns a cancel function that MUST be called from a `finally` block
 * when the task completes (success, failure, or interrupt). All sends are
 * fire-and-forget — a transient ratchet failure on the heartbeat must
 * never crash the wrapper task.
 */
export function startTaskProgressHeartbeat(
  originatorAmid: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  meshClient: { send: (amid: string, msg: any) => Promise<unknown> } | null,
  fromAgent: string,
  log: MeshLogger,
  intervalMs: number = 20_000,
): () => void {
  const startedAt = Date.now();
  let tick = 0;
  let cancelled = false;

  const fire = (stage: "started" | "executing"): void => {
    if (cancelled || !meshClient) return;
    const elapsedSec = Math.round((Date.now() - startedAt) / 1000);
    try {
      meshClient.send(originatorAmid, {
        type: "task_progress",
        stage,
        tick,
        elapsed_seconds: elapsedSec,
        from_agent: fromAgent,
        timestamp: new Date().toISOString(),
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
      }).catch((e: any) => {
        // Best-effort heartbeat — log once at debug-equivalent then swallow.
        log.warn(`AGT relay: task_progress heartbeat send failed (tick=${tick}): ${e?.message || e}`);
      });
    } catch (e: any) {
      log.warn(`AGT relay: task_progress heartbeat threw (tick=${tick}): ${e?.message || e}`);
    }
  };

  // Initial ping — tells the parent "I started" so its wait loop sees a
  // progress hit even for fast tasks (avoids the race where the reply
  // arrives between the "started" tick and the first interval fire).
  fire("started");

  const timer = setInterval(() => {
    tick += 1;
    fire("executing");
  }, intervalMs);
  // Don't keep the event loop alive solely for this interval — when the
  // wrapper task finishes its finally block runs the cancel returned below.
  if (typeof timer.unref === "function") timer.unref();

  return () => {
    if (cancelled) return;
    cancelled = true;
    clearInterval(timer);
  };
}

/**
 * Rewrite the AGT_INBOX_START..AGT_INBOX_END section of MEMORY.md so the
 * wrapped LLM sees pending peer messages without an explicit mesh_inbox
 * call. Best-effort — silently no-ops if the file isn't present.
 */
export async function notifyInboxToMemory(
  inbox: ReadonlyArray<InboxEntry>,
  log: MeshLogger,
): Promise<void> {
  if (inbox.length === 0) return;
  try {
    const fs = await import("node:fs/promises");
    const memPath = process.env.MEMORY_FILE_PATH || "/home/user/MEMORY.md";
    const INBOX_MARKER = "<!-- AGT_INBOX_START -->";
    const INBOX_END = "<!-- AGT_INBOX_END -->";

    let existing = "";
    try { existing = await fs.readFile(memPath, "utf-8"); } catch { return; }

    const preview = inbox.slice(0, 10).map((m, i) =>
      `${i + 1}. **${m.from_agent}** (${m.timestamp}): ${String(m.content).slice(0, 300)}`
    ).join("\n");
    const section = [
      INBOX_MARKER,
      "",
      `## 📬 Unread Mesh Messages (${inbox.length})`,
      "",
      `> You have ${inbox.length} unread message(s) from sub-agents. Call \`kars_mesh_inbox\` to read and respond.`,
      "",
      preview,
      "",
      INBOX_END,
    ].join("\n");

    if (existing.includes(INBOX_MARKER)) {
      const re = new RegExp(`${INBOX_MARKER}[\\s\\S]*?${INBOX_END}`, "m");
      existing = existing.replace(re, section);
    } else {
      existing = existing + "\n\n" + section;
    }
    await fs.writeFile(memPath, existing, "utf-8");
    log.info(`AGT inbox: wrote ${inbox.length} pending message(s) to MEMORY.md`);
  } catch { /* best effort */ }
}
