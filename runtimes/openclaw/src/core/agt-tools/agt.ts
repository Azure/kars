// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
// AzureClaw AGT tool registrations — extracted from plugin.ts in S15.f.9.
//
// Stateful tools that interact with the AGT mesh (Signal Protocol session
// over WebSocket relay), sandbox lifecycle (spawn/destroy/list/status), and
// agent-to-agent handoff (live migration). Tool bodies are byte-identical
// to the previous inline registrations — only closure-capture is replaced
// with explicit Deps threading, and the `handoffProgress` mutable was
// promoted to a shared holder object (`HandoffStateHolder`) so that
// plugin.ts and this module share the same reference.
//
// Tools registered:
//   azureclaw_spawn               azureclaw_spawn_status
//   azureclaw_mesh_send           azureclaw_mesh_inbox
//   azureclaw_mesh_transfer_file  azureclaw_spawn_destroy
//   azureclaw_spawn_list          azureclaw_discover
//   azureclaw_handoff_status      azureclaw_handoff_request
//   azureclaw_handoff_confirm
//
// The handoff_request + handoff_confirm tools are only registered when
// AGT_REGISTRY_MODE === "global" (mirrors the previous inline conditional).

import {
  routerCall,
  routerCallStrict,
  routerUrl,
  readAdminToken as _readAdminToken,
  pushTrustToRouter,
} from "../router-client.js";
import {
  amidToName,
  nameToAmid,
  parentTrustedAmids,
  getCachedAmid,
  resolveAmidByName as _resolveAmidByName,
} from "../amid-cache.js";
import { safeJson } from "../safe-json.js";
import { validateMeshPayload } from "../mesh-payload-guard.js";
import type { HandoffProgress, AgtInboxEntry } from "../agt-handoff.js";

// Re-suppress unused warnings for imports retained for symmetry with plugin.ts.
void routerCallStrict; void parentTrustedAmids; void getCachedAmid;

// 2-arg wrapper around the canonical resolveAmidByName(name, routerUrl, opts?).
// Kept local so existing tool bodies don't have to thread routerUrl.
async function resolveAmidByName(
  agentName: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  opts: { timeoutMs?: number; registryBase?: string; scopeFilter?: (a: any) => boolean; bypassCache?: boolean } = {},
): Promise<string | undefined> {
  return _resolveAmidByName(agentName, routerUrl, opts);
}

// Pod phases that mean the sub-agent is permanently gone.
const POD_DEAD_PHASES = new Set(["Failed", "Terminating", "Exited"]);

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyApi = any;

export interface HandoffStateHolder {
  current: HandoffProgress | null;
}

export interface AgtToolsDeps {
  log: { info: (m: string) => void; warn: (m: string) => void };
  bannerAlreadyPrinted: boolean;
  inbox: AgtInboxEntry[];
  /**
   * Inbox + gateway diagnostics. Surfaced inside `azureclaw_mesh_inbox`
   * responses so the LLM (and operators triaging "inbox empty"-style
   * reports) can distinguish a never-arrived message from one that the
   * agent already received and consumed on a prior turn, vs a gateway
   * restart that wiped the in-memory buffer. Mutated by sites that
   * remove entries directly from `inbox` (mesh_send reply waiter,
   * protocol-message scrubber); read-only here.
   */
  diagnostics?: () => {
    gateway_instance_id: string;
    gateway_started_at: string;
    received_total: number;
    consumed_by_send_wait: number;
    consumed_by_protocol_drain: number;
    read_total: number;
    last_received_at: string | null;
    last_read_at: string | null;
  };
  /** Mark all currently-unread inbox entries as read, bumping read_total. */
  markRead?: (ids: string[]) => void;
  /**
   * Notify diagnostics that entries were removed from the inbox by an
   * internal consumer (mesh_send reply waiter, protocol scrubber, file
   * transfer ack handler). Index sites that do `agtInbox.splice(...)`
   * bypass `pushInbox` so counters would otherwise drift.
   */
  notifyConsumed?: (kind: "send_wait" | "protocol_drain" | "progress_drain", count: number) => void;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  meshClient: () => any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  identity: () => any;
  sandboxName: () => string;
  // Auto-chunking mesh send wrapper (matches plugin.ts `meshSend`).
  meshSend: (
    client: { send: (amid: string, msg: unknown) => Promise<void> },
    targetAmid: string,
    message: Record<string, unknown>,
    log?: { info: (m: string) => void; warn: (m: string) => void },
  ) => Promise<string | undefined>;
  handoffState: HandoffStateHolder;
  runHandoffOrchestration: (
    handoffToken: string, adminToken: string, direction: string, dirLabel: string,
  ) => Promise<void>;
  recordMeshSession: (
    targetAmid: string,
    sessionId: string,
    intent: string,
    outcome: "success" | "failed" | "timeout",
    startedAt: string,
  ) => Promise<void>;
}

export function registerAgtTools(api: AnyApi, deps: AgtToolsDeps): void {
  const { log, bannerAlreadyPrinted } = deps;
  const agtInbox = deps.inbox;
  const meshSend = deps.meshSend;
  const recordMeshSession = deps.recordMeshSession;
  const _runHandoffOrchestration = deps.runHandoffOrchestration;
  const handoffState = deps.handoffState;

  // Probe sub-agent pod phase. Used by azureclaw_spawn_status.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async function probeSubAgentAlive(
    name: string,
  ): Promise<{ alive: boolean; phase?: string; reason?: string } | null> {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const status: any = await routerCall("GET", `/sandbox/${encodeURIComponent(name)}/status`);
      const phase: string = status?.phase || "Unknown";
      if (POD_DEAD_PHASES.has(phase)) {
        return { alive: false, phase, reason: `sub-agent phase is ${phase}` };
      }
      return { alive: true, phase };
    } catch (e: any) {
      const msg = (e && e.message) || "";
      if (msg.startsWith("HTTP 404")) {
        return { alive: false, reason: "sub-agent sandbox not found (CRD deleted)" };
      }
      return null;
    }
  }
  void probeSubAgentAlive;

  // ── Register AzureClaw agent tools (spawn, mesh, status, destroy) ────
  // These are first-class tools the LLM can call directly.
  // Registered as required tools (always available, no tools.allow needed).
  // API: execute(_id, params) → { content: [{ type: "text", text }] }

  // Spawn tools are always registered — AGT policy profile decides whether
  // the sandbox may actually invoke them. Offload sandboxes use the "offload"
  // policy profile which denies spawn:* + tool:azureclaw_spawn_* actions.
  // Normal interactive sandboxes use "default" and retain full spawn capability.

  api.registerTool({
    name: "azureclaw_spawn",
    label: "Spawn Sub-Agent",
    description: "Spawn a secure isolated sub-agent on AKS with E2E encrypted mesh communication (Signal Protocol). The sub-agent runs in its own container with a SEPARATE filesystem — it CANNOT see your files. Exchange data via azureclaw_mesh_send (include content in the message body). Sub-agents can also message EACH OTHER directly via mesh — you can instruct one sub-agent to forward its results to another sub-agent by name (e.g. 'send your analysis to the writer agent'). You don't need to relay everything yourself.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "DNS-safe name for the sub-agent (lowercase alphanumeric + hyphens, e.g. 'auditor', 'analyst')" },
        model: { type: "string", description: "AI model deployment (default: gpt-4.1)" },
        governance: { type: "boolean", description: "Enable AGT governance + mesh communication (default: true)" },
      },
      required: ["name"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      // Hard short-circuit in offload-mode sandboxes: the AGT policy profile
      // already denies spawn:* but the denial message is opaque ("blocked by
      // policy") and the LLM then asks the parent for confirmation instead
      // of executing the task itself. Returning a clear, actionable error
      // here prevents the loop and tells the model to just do the work.
      if (process.env.OFFLOAD_REQUEST_ID) {
        return {
          content: [{
            type: "text",
            text:
              "❌ azureclaw_spawn is DISABLED in offload sandboxes. You ARE the " +
              "offload executor — do NOT try to delegate this task to another " +
              "sandbox. Execute the task directly here. Write output files to " +
              "/sandbox/.openclaw/workspace/ (use exec_command) so they are " +
              "shipped back to the parent automatically when you finish.",
          }],
        };
      }
      try {
        // Build trusted peers list: parent's AMID + all existing siblings
        // These are parent-verified (from registry lookups), not self-reported
        const trustedPeers: string[] = [];
        const myAmid = deps.meshClient()?.getAmid?.() || deps.identity()?.amid;
        const myName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "parent";
        if (myAmid) trustedPeers.push(`${myName}:${myAmid}`);
        for (const [amid, name] of amidToName.entries()) {
          if (amid !== myAmid) trustedPeers.push(`${name}:${amid}`);
        }

        const result = await routerCall("POST", "/sandbox/spawn", {
          agent_id: params.name,
          model: params.model || "gpt-4.1",
          governance: params.governance !== false,
          trust_threshold: 500,
          trusted_peers: trustedPeers.length > 0 ? trustedPeers.join(",") : undefined,
        });

        // Poll until sub-agent is Running AND registered on mesh (in parallel)
        const agentName = params.name as string;
        log.info(`Waiting for sub-agent '${agentName}' to be Running + registered...`);

        let phase = "Pending";
        let amid: string | undefined;
        const startWait = Date.now();
        const maxWaitMs = 45_000;

        // Parallel poll: check status AND registry simultaneously
        while (Date.now() - startWait < maxWaitMs) {
          await new Promise(r => setTimeout(r, 1000));

          // Status check
          if (phase !== "Running") {
            try {
              const status = await routerCall("GET", `/sandbox/${encodeURIComponent(agentName)}/status`);
              phase = status?.phase || "Pending";
            } catch { /* not ready yet */ }
          }

          // Registry check (start early — sub-agent may register before status reports Running).
          // Use bypassCache: pre-discovery only confirms availability; it must NOT pin
          // an AMID into the cache, because the sub-agent's pod may still be rolling
          // and a fresh AMID could replace it before the parent's first send. The
          // actual send path resolves through resolveAmidByName which will hit the
          // registry once status flips Running, picking the freshest live entry.
          if (!amid && deps.meshClient()) {
            const resolved = await resolveAmidByName(agentName, { bypassCache: true });
            if (resolved) {
              amid = resolved;
              log.info(`AGT pre-discovery: '${agentName}' registered (${resolved.slice(0, 12)}..., not cached — send will re-resolve)`);
            }
          }

          // Both ready — exit early
          if (phase === "Running" && (amid || !deps.meshClient())) break;

          const elapsed = Math.round((Date.now() - startWait) / 1000);
          if (elapsed % 5 === 0) {
            log.info(`Sub-agent '${agentName}': phase=${phase}, mesh=${amid ? "registered" : "waiting"} (${elapsed}s)`);
          }
        }

        if (phase !== "Running") {
          return { content: [{ type: "text", text: JSON.stringify({
            ...result,
            warning: `Sub-agent created but not yet Running (phase: ${phase}). It may still be booting. Use azureclaw_spawn_status to check.`,
          }, null, 2) }] };
        }

        if (!amid && deps.meshClient()) {
          log.info(`AGT pre-discovery: '${agentName}' not yet registered — mesh_send will retry`);
        }

        // Collect known sibling names for context
        const siblings = [...amidToName.values()].filter(n => n !== agentName && n !== (process.env.SANDBOX_NAME || ""));

        return { content: [{ type: "text", text: JSON.stringify({
          ...result,
          phase: "Running",
          message: `Sub-agent '${agentName}' is Running and ready for mesh communication. Use azureclaw_mesh_send to send it a task.`,
          ...(siblings.length > 0 ? { mesh_peers: `This agent can communicate directly with other sub-agents: ${siblings.join(", ")}. You can instruct it to forward results to them by name.` } : {}),
        }, null, 2) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Spawn failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_spawn_status",
    label: "Sub-Agent Status",
    description: "Check the status of a spawned sub-agent. Returns phase (Pending/Running/Terminating), namespace, mesh_registered (true once the sub-agent has registered with the AGT mesh), and mesh_ready (Running AND mesh_registered). Prefer polling until mesh_ready=true before sending mesh messages — phase=Running alone is not sufficient because mesh registration happens asynchronously (~60s on AKS) after the pod becomes Ready.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Name of the sub-agent to check" },
      },
      required: ["name"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const name = params.name as string;
      try {
        const result: any = await routerCall("GET", `/sandbox/${encodeURIComponent(name)}/status`);
        // Best-effort registry probe — don't fail status on registry hiccups.
        let mesh_registered = false;
        try {
          const search: any = await routerCall(
            "GET",
            `/agt/registry/registry/search?capability=${encodeURIComponent(name)}`,
          );
          const agents = (search && Array.isArray(search.results)) ? search.results : [];
          mesh_registered = agents.some(
            (a: any) => a.display_name === name || (a.capabilities || []).includes(name),
          );
        } catch { /* registry unavailable — report as not-registered */ }
        const enriched = {
          ...result,
          mesh_registered,
          mesh_ready: result?.phase === "Running" && mesh_registered,
        };
        return { content: [{ type: "text", text: safeJson(enriched) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Status check failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_mesh_send",
    label: "Send Mesh Task",
    description: "Send a TEXT/JSON task to a sub-agent via AGT mesh (E2E encrypted relay). Sub-agents have isolated filesystems — include any data the agent needs directly in the message body. To send a FILE / IMAGE / BINARY, use `azureclaw_mesh_transfer_file` instead — peer agents cannot read your /sandbox or /tmp paths, so a hand-crafted file_transfer envelope with placeholder file_data will be rejected by the payload guard. Plain JSON metadata and free-form text are accepted. Automatically retries registry discovery and prekey exchange for as long as the sub-agent pod is alive; aborts only if the pod reaches Failed/Terminating/Exited, the sandbox is deleted, or meshSend returns a non-transient error. Then waits up to 5.5 minutes for the reply. If no reply arrives, check azureclaw_mesh_inbox later.",
    parameters: {
      type: "object",
      properties: {
        to_agent: { type: "string", description: "Name of the target sub-agent" },
        content: { type: "string", description: "Task description or message to send" },
      },
      required: ["to_agent", "content"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      let agentName = params.to_agent as string;
      const msgContent = params.content as string;

      // OFFLOAD HARDENING: native agents in offload sandboxes may call this
      // tool with their own sandbox name or an arbitrary sibling. Force
      // routing to 'parent' — the only legitimate destination in offload mode.
      if (process.env.OFFLOAD_REQUEST_ID && agentName !== "parent") {
        log.warn(`azureclaw_mesh_send: offload mode — rewriting to_agent '${agentName}' → 'parent'`);
        agentName = "parent";
      }

      // Guard: reject malformed file_transfer envelopes / cross-container
      // path references before they hit the wire. The peer agent runs in a
      // separate container and cannot read this filesystem.
      const guardErr = validateMeshPayload(msgContent, { transferToolName: "azureclaw_mesh_transfer_file" });
      if (guardErr) {
        log.warn(`azureclaw_mesh_send: payload guard rejected — ${guardErr.slice(0, 160)}`);
        return { content: [{ type: "text", text: safeJson({
          error: guardErr,
          to_agent: agentName,
        }) }] };
      }

      // ── Primary path: AGT SDK relay (E2E encrypted) ──
      if (deps.meshClient() && deps.identity()) {
        // Ensure we're connected (reconnect if initial connect was deferred)
        if (!deps.meshClient().isConnected) {
          try {
            log.info("AGT relay: reconnecting before send...");
            // Force disconnect first to clear stale "Already connected" state
            try { await deps.meshClient().disconnect(); } catch { /* ignore */ }
            await deps.meshClient().connect({
              displayName: deps.sandboxName(),
              capabilities: ["azureclaw-agent", "task-execution", deps.sandboxName()],
            });
            log.info("AGT relay: reconnected successfully");
          } catch (reconErr: any) {
            log.warn(`AGT relay: reconnect failed: ${reconErr.message}`);
          }
        }
        try {
          // Discover the target sub-agent's AMID and send — retry continuously while
          // the sub-agent's pod is alive. The only terminal conditions are:
          //   • pod reaches Failed/Terminating/Exited (or CRD is gone)
          //   • meshSend returns a non-transient error (not a prekey / stale-AMID case)
          // This removes the old hand-rolled 12-attempt discovery + 15-attempt prekey
          // windows that were too short on AKS (router-to-relay connect alone is ~60–70s).
          // Check cache first — getCachedAmid invalidates entries older than
          // AMID_CACHE_TTL_MS so a peer pod restart (which rotates identity) is
          // recovered on the next send rather than persisting forever.
          let targetAmid: string | undefined = getCachedAmid(agentName);
          if (targetAmid) {
            log.info(`AGT relay: using cached AMID for '${agentName}' (${targetAmid.slice(0, 12)}...)`);
          }

          const waitStart = Date.now();
          let nextHeartbeatAt = waitStart + 10_000;
          let sendSucceeded = false;
          let finalSendErr: Error | null = null;

          while (!sendSucceeded) {
            // (a) Is the sub-agent still alive? Bail only on terminal phases.
            const probe = await probeSubAgentAlive(agentName);
            if (probe && probe.alive === false) {
              log.warn(`AGT relay: aborting send to '${agentName}' — ${probe.reason}`);
              return { content: [{ type: "text", text: JSON.stringify({
                error: "E2E encrypted send aborted — sub-agent is no longer running",
                reason: probe.reason,
                phase: probe.phase,
                agent: agentName,
                hint: "Sub-agent was deleted, failed, or is terminating. Respawn with azureclaw_spawn before retrying.",
              }, null, 2) }] };
            }

            // (b) Discover AMID via registry search if we don't have one yet.
            if (!targetAmid) {
              targetAmid = await resolveAmidByName(agentName);
            }

            if (!targetAmid) {
              if (Date.now() >= nextHeartbeatAt) {
                const elapsed = Math.round((Date.now() - waitStart) / 1000);
                log.info(`AGT relay: still waiting for '${agentName}' to register (${elapsed}s, pod alive)...`);
                nextHeartbeatAt = Date.now() + 10_000;
              }
              await new Promise(r => setTimeout(r, 2000));
              continue;
            }

            // (c) Try to send. Bail only on non-transient errors.
            try {
              await meshSend(deps.meshClient(), targetAmid, {
                type: "task_request",
                content: msgContent,
                from_agent: process.env.SANDBOX_NAME || "unknown",
                timestamp: new Date().toISOString(),
              }, log);
              sendSucceeded = true;
              break;
            } catch (e: any) {
              const msg = (e && e.message) || "";
              if (msg.includes("prekey")) {
                // Child registered but hasn't uploaded prekeys yet — keep waiting.
                if (Date.now() >= nextHeartbeatAt) {
                  const elapsed = Math.round((Date.now() - waitStart) / 1000);
                  log.info(`AGT relay: waiting for prekeys from '${agentName}' (${elapsed}s, pod alive)...`);
                  nextHeartbeatAt = Date.now() + 10_000;
                }
                await new Promise(r => setTimeout(r, 3000));
                continue;
              }
              if (msg.includes("not found") || msg.includes("closed") || msg.includes("AGENT_NOT_FOUND")) {
                // Cached/registered AMID is stale (sub-agent was recycled) — invalidate and re-discover.
                log.warn(`AGT relay: AMID ${targetAmid!.slice(0, 12)}... stale for '${agentName}', re-discovering`);
                nameToAmid.delete(agentName);
                amidToName.delete(targetAmid!);
                targetAmid = undefined;
                await new Promise(r => setTimeout(r, 2000));
                continue;
              }
              // Non-transient failure — give up.
              finalSendErr = e;
              break;
            }
          }

          if (!sendSucceeded) {
            log.warn(`AGT relay send failed: ${finalSendErr?.message}`);
            return { content: [{ type: "text", text: JSON.stringify({
              error: "E2E encrypted send failed — message NOT delivered",
              reason: finalSendErr?.message || "unknown",
              agent: agentName,
              hint: "Non-transient send error — verify the sub-agent is healthy with azureclaw_spawn_status.",
            }, null, 2) }] };
          }

          log.info(`AGT relay: sent to ${agentName} (${targetAmid!.slice(0, 12)}...) via E2E encrypted relay`);
          // Surface this peer in the parent's operator-panel trust view immediately
          // on first successful send — the operator dashboard reads /agt/status which
          // ultimately exposes the router's trust_states. Without this, the parent
          // would show "no peer agents yet" until a reply arrives (and never at all
          // for fire-and-forget sends).
          try { await pushTrustToRouter(agentName, 0.0); } catch { /* best-effort */ }
          const messageId = crypto.randomUUID();
          const sendStart = new Date().toISOString();

          // Auto-wait for reply: poll agtInbox for a response from this agent.
          // The relay layer does NOT surface "agent identity is dead" — it happily
          // queues messages for any AMID, including ones whose pod was recycled.
          // To recover from rolling deploys (parent's cached/registered AMID is for
          // the previous pod incarnation), we retry ONCE on reply timeout: clear
          // the cache, re-resolve, and if the AMID actually changed, resend.
          //
          // Heartbeat-aware timeout (added 2026-04-30 fix-pass F6): the
          // sub-agent emits periodic `task_progress` pings while running its
          // in-process LLM tool-loop (see `index.ts` task_request handler +
          // `core/agt-heartbeat.ts::startTaskProgressHeartbeat`). Each
          // progress drain resets the idle clock so long-running tool
          // sequences no longer time out at a fixed 60s. A hard ceiling
          // bounds the total wait absolutely — even continuous heartbeats
          // cannot keep a stuck tool call running forever.
          const idleTimeoutMs = 60_000;       // reset on each task_progress
          const hardCeilingMs = 600_000;      // absolute upper bound (10 min)
          const pollIntervalMs = 500;
          let replyContent: string | null = null;
          let retriedAfterTimeout = false;
          const overallStart = Date.now();

          // eslint-disable-next-line no-constant-condition
          while (true) {
            let replyWaitStart = Date.now();
            log.info(`AGT relay: waiting up to ${idleTimeoutMs / 1000}s idle / ${hardCeilingMs / 1000}s total for reply from '${agentName}'...`);

            while (
              Date.now() - replyWaitStart < idleTimeoutMs &&
              Date.now() - overallStart < hardCeilingMs
            ) {
              // Check inbox for a reply from this target, skipping protocol messages
              const replyIdx = agtInbox.findIndex((m) => {
                if (m.from_amid !== targetAmid && m.from_agent !== agentName) return false;
                // Skip Signal Protocol handshake + heartbeat messages
                const mt = m.message_type || "";
                if (mt === "ACCEPT" || mt === "KNOCK" || mt === "KEY_EXCHANGE" || mt === "task_progress") return false;
                // Also check content for JSON protocol messages
                if (typeof m.content === "string") {
                  try {
                    const parsed = JSON.parse(m.content);
                    if (parsed.type === "ACCEPT" || parsed.type === "KNOCK" || parsed.type === "KEY_EXCHANGE" || parsed.type === "task_progress") return false;
                  } catch { /* not JSON, treat as real content */ }
                }
                return true;
              });
              if (replyIdx >= 0) {
                const reply = agtInbox.splice(replyIdx, 1)[0];
                deps.notifyConsumed?.("send_wait", 1);
                replyContent = typeof reply.content === "string"
                  ? reply.content
                  : JSON.stringify(reply.content);
                log.info(`AGT relay: got reply from '${agentName}' after ${((Date.now() - overallStart) / 1000).toFixed(1)}s`);
                break;
              }
              // Drain protocol messages to keep inbox clean
              let drained = 0;
              for (let i = agtInbox.length - 1; i >= 0; i--) {
                const m = agtInbox[i];
                if ((m.from_amid === targetAmid || m.from_agent === agentName) &&
                    (m.message_type === "ACCEPT" || m.message_type === "KNOCK" || m.message_type === "KEY_EXCHANGE")) {
                  agtInbox.splice(i, 1);
                  drained++;
                }
              }
              if (drained > 0) deps.notifyConsumed?.("protocol_drain", drained);

              // Drain task_progress heartbeats and reset the idle clock for
              // every one we see — sub-agent is signalling "still working".
              let progressDrained = 0;
              let lastProgressStage: string | undefined;
              let lastProgressElapsed: number | undefined;
              for (let i = agtInbox.length - 1; i >= 0; i--) {
                const m = agtInbox[i];
                if (m.from_amid !== targetAmid && m.from_agent !== agentName) continue;
                let isProgress = m.message_type === "task_progress";
                let parsed: any = null;
                if (typeof m.content === "string") {
                  try {
                    parsed = JSON.parse(m.content);
                    if (parsed?.type === "task_progress") isProgress = true;
                  } catch { /* not JSON */ }
                } else if (typeof m.content === "object" && m.content !== null) {
                  parsed = m.content;
                  if (parsed?.type === "task_progress") isProgress = true;
                }
                if (!isProgress) continue;
                if (parsed) {
                  lastProgressStage = String(parsed.stage ?? "");
                  if (typeof parsed.elapsed_seconds === "number") {
                    lastProgressElapsed = parsed.elapsed_seconds;
                  }
                }
                agtInbox.splice(i, 1);
                progressDrained++;
              }
              if (progressDrained > 0) {
                deps.notifyConsumed?.("progress_drain", progressDrained);
                replyWaitStart = Date.now();
                log.info(
                  `AGT relay: '${agentName}' still working ` +
                  `(stage=${lastProgressStage ?? "?"}, ` +
                  `elapsed=${lastProgressElapsed ?? "?"}s, ` +
                  `progress_pings=${progressDrained}) — extending idle wait`,
                );
              }
              await new Promise((r) => setTimeout(r, pollIntervalMs));
            }

            if (replyContent !== null) break;
            if (retriedAfterTimeout) break;
            retriedAfterTimeout = true;

            // Reply timed out — the registered AMID may belong to a recycled pod.
            // Force-invalidate cache, re-resolve via registry, and if the AMID
            // changed, resend exactly once. This recovers from the rolling-deploy
            // race where parent's discovery happened during the gap between old
            // pod terminating and new pod re-registering its identity.
            const previousAmid = targetAmid!;
            log.warn(`AGT relay: no reply from '${agentName}' within ${idleTimeoutMs / 1000}s idle (or ${hardCeilingMs / 1000}s ceiling) — clearing cache and re-discovering (target may have been recycled during a rollout)`);
            nameToAmid.delete(agentName);
            amidToName.delete(previousAmid);
            let freshAmid: string | undefined;
            try {
              freshAmid = await resolveAmidByName(agentName);
            } catch (e: any) {
              log.warn(`AGT relay: re-discover failed: ${e?.message || e}`);
            }
            if (!freshAmid) {
              log.info(`AGT relay: re-discovery returned no AMID — giving up retry`);
              break;
            }
            if (freshAmid === previousAmid) {
              log.info(`AGT relay: re-discovery returned same AMID — peer is genuinely silent, giving up retry`);
              break;
            }
            log.info(`AGT relay: target AMID changed ${previousAmid.slice(0, 12)}... → ${freshAmid.slice(0, 12)}..., resending after rollout race`);
            targetAmid = freshAmid;
            try {
              await meshSend(deps.meshClient(), targetAmid, {
                type: "task_request",
                content: msgContent,
                from_agent: process.env.SANDBOX_NAME || "unknown",
                timestamp: new Date().toISOString(),
              }, log);
              log.info(`AGT relay: resent to '${agentName}' (${targetAmid.slice(0, 12)}...) after rollout-aware re-discover`);
            } catch (e: any) {
              log.warn(`AGT relay: resend after timeout failed: ${e?.message || e}`);
              break;
            }
            // Loop back to wait for reply on the fresh identity.
          }

          const result: any = {
            status: replyContent ? "delivered_and_replied" : "delivered_via_agt_relay",
            to_agent: agentName,
            to_amid: targetAmid,
            from_amid: deps.identity().amid,
            protocol: "AGT E2E encrypted (Signal Protocol)",
            message_id: messageId,
          };
          if (replyContent) {
            result.reply = replyContent;
            // Parent rates sub-agent — only meaningful for long-lived sub-agents
            // whose reputation will be queried again. Short-lived ones will die
            // and their score is lost, but the audit trail remains.
            try {
              const ok = await deps.meshClient().submitReputation(targetAmid!, messageId, 0.9, ["fast_response", "reliable"]);
              pushTrustToRouter(agentName, 0.9);
              await recordMeshSession(targetAmid!, messageId, "mesh_send", "success", sendStart);
              log.info(`AGT reputation: submitted +0.9 for '${agentName}' (accepted=${ok})`);
            } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
          } else {
            result.note = "No reply within timeout — use azureclaw_mesh_inbox to check later.";
          }
          return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
        } catch (agtErr: any) {
          log.warn(`AGT relay send failed: ${agtErr.message}`);
          return { content: [{ type: "text", text: JSON.stringify({
            error: "E2E encrypted send failed — message NOT delivered",
            reason: agtErr.message,
            agent: agentName,
            hint: "Retry after confirming the sub-agent is Running.",
          }, null, 2) }] };
        }
      }

      // No AGT mesh client available — cannot send without E2E encryption
      return { content: [{ type: "text", text: JSON.stringify({
        error: "AGT mesh not initialized — cannot send without E2E encryption",
        hint: "The mesh client failed to start. Check gateway logs for AGT initialization errors.",
      }, null, 2) }] };
    },
  });

  api.registerTool({
    name: "azureclaw_mesh_inbox",
    label: "Check Mesh Inbox",
    description:
      "Read messages received via the E2E encrypted AGT mesh inbox. ALWAYS " +
      "call this tool FIRST whenever your task description says a peer agent " +
      "has sent you data, output, or a reply — peer messages always arrive " +
      "before you process them, so checking the inbox is your default opening " +
      "move whenever you're told to consume something from another agent. " +
      "Also call it whenever you need to know whether a peer has sent you " +
      "anything at all — do NOT rely on what you remember from previous " +
      "turns, because new messages may have arrived since then. By default " +
      "the tool is *peek-only* and shows only unread entries; messages stay " +
      "in the inbox so you can read them again. Pass mark_read=true once you " +
      "have acted on the contents to flag them as seen, or unread_only=false " +
      "to also show entries you have already read on earlier turns. The " +
      "response includes a `diagnostics` block with the gateway instance id " +
      "and counters so you can tell apart 'no message has ever arrived' from " +
      "'I already consumed the message earlier'.",
    parameters: {
      type: "object",
      properties: {
        mark_read: {
          type: "boolean",
          description: "When true, flag the returned entries as read so the next call (with default unread_only=true) won't re-list them. The entries themselves stay in the inbox.",
        },
        unread_only: {
          type: "boolean",
          description: "Default true — only return entries with no read_at timestamp. Set false to also see entries you already read on earlier turns.",
        },
        limit: {
          type: "number",
          description: "Maximum number of entries to return. Default 50.",
        },
      },
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const markRead = params.mark_read === true;
        const unreadOnly = params.unread_only !== false; // default true
        const limit = typeof params.limit === "number" && params.limit > 0
          ? Math.floor(params.limit)
          : 50;

        // ── 1. Filter out internal protocol/handshake messages from the
        //      view. They stay in the array (the mesh_send reply waiter
        //      and handoff orchestration scrub them on their own cadence)
        //      but the LLM never needs to see them.
        const INTERNAL_TYPES = new Set([
          "handoff_transfer", "handoff_verification", "handoff_ready",
          "handoff:interrupt", "handoff:interrupt_ack",
          "handoff:workspace_request", "handoff:workspace_response",
          "handoff:workspace_inject", "handoff:workspace_inject_ack",
          "handoff:resume", "handoff:resume_ack",
          "file_transfer_ack",
          // Heartbeats — drained by mesh_send wait loop; never user-visible.
          "task_progress", "offload_progress",
        ]);

        const visible = agtInbox.filter((m) => {
          try {
            const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
            if (INTERNAL_TYPES.has(parsed?.type)) return false;
          } catch { /* not JSON — keep */ }
          if (m.message_type && INTERNAL_TYPES.has(m.message_type)) return false;
          return true;
        });

        const filtered = unreadOnly ? visible.filter((m) => !m.read_at) : visible;

        // Take the most recent `limit` entries (chronological order kept).
        const slice = filtered.slice(-limit);

        // Auto-decode file_transfer messages so the LLM sees a readable
        // representation. The original entry retains its raw content.
        // Two forms are handled:
        //   (a) Gateway-rewritten: {type, file_name, saved_to, size_bytes, ...}
        //       — the gateway has already base64-decoded and written the
        //       file to /sandbox/.openclaw/workspace/incoming/. Lift the
        //       saved path so the LLM can `cat` it; inline small text.
        //   (b) Inline file_data: legacy/raw form before gateway rewrite.
        const decoded = slice.map((m) => {
          try {
            const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
            if (parsed?.type === "file_transfer" && parsed?.saved_to && parsed?.file_name) {
              const sizeBytes = typeof parsed.size_bytes === "number" ? parsed.size_bytes : 0;
              let inlined: string | null = null;
              if (sizeBytes > 0 && sizeBytes < 100 * 1024) {
                try {
                  // eslint-disable-next-line @typescript-eslint/no-require-imports
                  const fs = require("node:fs");
                  const buf = fs.readFileSync(parsed.saved_to);
                  if (!buf.some((b: number) => b === 0)) {
                    inlined = buf.toString("utf-8");
                  }
                } catch { /* fall through */ }
              }
              return {
                id: m.id,
                from_amid: m.from_amid,
                from_agent: m.from_agent,
                timestamp: m.timestamp,
                read_at: m.read_at,
                source: "agt_relay_e2e",
                message_type: "file_transfer",
                file_name: parsed.file_name,
                saved_to: parsed.saved_to,
                file_size_bytes: sizeBytes,
                description: parsed.description || "",
                content: inlined != null
                  ? inlined
                  : `[binary or large file: ${parsed.file_name}, ${sizeBytes} bytes — read with: cat ${parsed.saved_to}]`,
              };
            }
            if (parsed?.type === "file_transfer" && parsed?.file_data && parsed?.file_name) {
              const buf = Buffer.from(parsed.file_data, "base64");
              const isText = !buf.some((b: number) => b === 0);
              return {
                id: m.id,
                from_amid: m.from_amid,
                from_agent: m.from_agent,
                timestamp: m.timestamp,
                read_at: m.read_at,
                source: "agt_relay_e2e",
                message_type: "file_transfer",
                file_name: parsed.file_name,
                file_size_bytes: buf.length,
                content: isText
                  ? buf.toString("utf-8")
                  : `[binary file: ${parsed.file_name}, ${buf.length} bytes — auto-save pending; re-check mesh_inbox for saved_to path]`,
              };
            }
          } catch { /* fall through */ }
          return {
            id: m.id,
            from_amid: m.from_amid,
            from_agent: m.from_agent,
            timestamp: m.timestamp,
            read_at: m.read_at,
            source: "agt_relay_e2e",
            message_type: m.message_type || "message",
            content: m.content,
          };
        });

        // ── 2. Optional router-level inbox merge (best effort).
        let routerMessages: any[] = [];
        try {
          const routerResult = await routerCall("GET", "/agt/mesh/inbox");
          routerMessages = routerResult.messages || [];
        } catch {
          // router inbox unavailable — AGT-only view is fine
        }

        // ── 3. Mark returned AGT entries as read (in-place). We only
        //      flip read_at for entries that were actually returned and
        //      currently have no read_at, so a subsequent unread_only
        //      query becomes empty rather than re-listing the same items.
        if (markRead) {
          const now = new Date().toISOString();
          const returnedIds = new Set(decoded.map((d) => d.id));
          const newlyRead: string[] = [];
          for (const m of agtInbox) {
            if (returnedIds.has(m.id) && !m.read_at) {
              m.read_at = now;
              newlyRead.push(m.id);
            }
          }
          if (deps.markRead && newlyRead.length > 0) {
            try { deps.markRead(newlyRead); } catch { /* best effort */ }
          }
        }

        // ── 4. Tidy MEMORY.md inbox banner once the LLM has acknowledged
        //      receipt by calling the tool. Was previously gated on a
        //      drain-everything path; now also covers peek+mark_read.
        if (markRead && decoded.length > 0) {
          try {
            const fs = await import("node:fs/promises");
            const memPath = process.env.MEMORY_FILE_PATH || "/home/user/MEMORY.md";
            const INBOX_MARKER = "<!-- AGT_INBOX_START -->";
            const INBOX_END = "<!-- AGT_INBOX_END -->";
            let mem = await fs.readFile(memPath, "utf-8");
            if (mem.includes(INBOX_MARKER)) {
              const re = new RegExp(`\\n*${INBOX_MARKER}[\\s\\S]*?${INBOX_END}\\n*`, "m");
              mem = mem.replace(re, "\n");
              await fs.writeFile(memPath, mem, "utf-8");
            }
          } catch { /* best effort */ }
        }

        // Snapshot diagnostics last so counters reflect this call too.
        const diag = deps.diagnostics ? deps.diagnostics() : undefined;

        const allMessages = [
          ...decoded,
          ...routerMessages.map((m: any) => ({ ...m, source: "router_http" })),
        ];

        const unreadCount = visible.filter((m) => !m.read_at).length;
        const totalVisible = visible.length;

        return { content: [{ type: "text", text: JSON.stringify({
          count: allMessages.length,
          unread_count: unreadCount,
          total_in_inbox: totalVisible,
          agt_relay_count: decoded.length,
          router_count: routerMessages.length,
          filter: { unread_only: unreadOnly, limit, mark_read: markRead },
          messages: allMessages,
          diagnostics: diag,
        }, null, 2) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Inbox check failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_mesh_transfer_file",
    label: "Transfer File via Mesh",
    description: "Send a file to another agent via E2E encrypted mesh. The file is read from your local workspace, base64-encoded, and sent via the chunked transfer protocol — files up to ~30MB are supported. The receiving agent gets a file_transfer message in their inbox with the file content. Great for sharing datasets, configs, code, or any file between agents.",
    parameters: {
      type: "object",
      properties: {
        to_agent: { type: "string", description: "Name of the target agent" },
        file_path: { type: "string", description: "Path to the file to send (relative to workspace or absolute)" },
        description: { type: "string", description: "Optional description of the file for the receiving agent" },
      },
      required: ["to_agent", "file_path"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      let agentName = params.to_agent as string;
      const filePath = params.file_path as string;
      const desc = (params.description as string) || "";

      // OFFLOAD HARDENING: same rule as mesh_send — offload workers may
      // only ship files back to 'parent'. Seen in the wild: agent sent
      // aks-pricing-cheatsheet.md to its own sandbox name instead of parent.
      if (process.env.OFFLOAD_REQUEST_ID && agentName !== "parent") {
        log.warn(`azureclaw_mesh_transfer_file: offload mode — rewriting to_agent '${agentName}' → 'parent'`);
        agentName = "parent";
      }

      if (!deps.meshClient() || !deps.identity()) {
        return { content: [{ type: "text", text: JSON.stringify({
          error: "AGT mesh not initialized — cannot transfer files",
        }, null, 2) }] };
      }

      try {
        const fs = await import("node:fs");
        const path = await import("node:path");

        // Resolve path relative to workspace
        const workspaceRoot = "/sandbox/.openclaw/workspace";
        const resolvedPath = path.isAbsolute(filePath) ? filePath : path.resolve(workspaceRoot, filePath);

        // Security: block path traversal outside /sandbox
        if (!resolvedPath.startsWith("/sandbox")) {
          return { content: [{ type: "text", text: JSON.stringify({
            error: "File path must be within /sandbox — path traversal blocked",
          }, null, 2) }] };
        }

        const MAX_FILE_SIZE = 30 * 1024 * 1024; // 30MB

        // Open once and do EVERYTHING (kind check, size check, read) via the
        // same fd so the file cannot be swapped between stat and read
        // (CWE-367 TOCTOU). No pre-open statSync — any path-race check
        // performed before openSync is an additional race window.
        let fd: number;
        try {
          fd = fs.openSync(resolvedPath, "r");
        } catch (e: any) {
          return { content: [{ type: "text", text: JSON.stringify({
            error: `Cannot open file ${filePath}: ${e.message}`,
          }, null, 2) }] };
        }
        let fileData: Buffer;
        let finalSize: number;
        try {
          const fstat = fs.fstatSync(fd);
          if (!fstat.isFile()) {
            return { content: [{ type: "text", text: JSON.stringify({
              error: `Not a regular file: ${filePath}`,
            }, null, 2) }] };
          }
          if (fstat.size > MAX_FILE_SIZE) {
            return { content: [{ type: "text", text: JSON.stringify({
              error: `File too large: ${(fstat.size / 1024 / 1024).toFixed(1)} MB (max 30MB)`,
            }, null, 2) }] };
          }
          finalSize = fstat.size;
          fileData = Buffer.alloc(finalSize);
          fs.readSync(fd, fileData, 0, finalSize, 0);
        } finally {
          fs.closeSync(fd);
        }
        const b64Data = fileData.toString("base64");
        const fileName = path.basename(resolvedPath);

        // Look up target AMID — TTL'd cache + freshest-match registry resolution
        // protects against stale entries from a previous peer pod incarnation.
        const targetAmid = await resolveAmidByName(agentName);

        if (!targetAmid) {
          return { content: [{ type: "text", text: JSON.stringify({
            error: `Agent '${agentName}' not found in mesh registry`,
            hint: "Ensure the agent is running and registered. Use azureclaw_discover to search.",
          }, null, 2) }] };
        }

        // Send + wait for ack with retry (up to 3 attempts)
        const fileMsg = {
          type: "file_transfer",
          file_name: fileName,
          file_path: filePath,
          file_data: b64Data,
          size_bytes: finalSize,
          description: desc,
          from_agent: process.env.SANDBOX_NAME || "unknown",
          timestamp: new Date().toISOString(),
        };

        let ackReceived = false;
        let ackSavedTo = "";
        let ackError = "";
        let transferId: string | undefined;
        const maxAttempts = 3;

        for (let attempt = 0; attempt < maxAttempts; attempt++) {
          if (attempt > 0) {
            log.info(`📁 File transfer retry ${attempt + 1}/${maxAttempts} for '${fileName}' → '${agentName}'`);
            await new Promise(r => setTimeout(r, 3000));
          }

          transferId = await meshSend(deps.meshClient(), targetAmid, fileMsg, log);

          // Wait for file_transfer_ack (up to 15s)
          const ackStart = Date.now();
          while (Date.now() - ackStart < 15_000) {
            const ackIdx = agtInbox.findIndex((m) =>
              m.from_amid === targetAmid &&
              (m.content?.includes?.("file_transfer_ack") || m.message_type === "file_transfer_ack")
            );
            if (ackIdx !== -1) {
              try {
                const ackMsg = typeof agtInbox[ackIdx].content === "string"
                  ? JSON.parse(agtInbox[ackIdx].content) : agtInbox[ackIdx].content;
                ackReceived = !!ackMsg?.success;
                ackSavedTo = ackMsg?.saved_to || "";
                ackError = ackMsg?.error || "";
              } catch { ackReceived = true; }
              agtInbox.splice(ackIdx, 1);
              deps.notifyConsumed?.("protocol_drain", 1);
              break;
            }
            await new Promise(r => setTimeout(r, 500));
          }

          if (ackReceived) break;
        }

        return { content: [{ type: "text", text: JSON.stringify({
          status: ackReceived ? "delivered" : "sent_no_ack",
          to_agent: agentName,
          file_name: fileName,
          size_bytes: finalSize,
          size_human: finalSize < 1024 ? `${finalSize}B`
            : finalSize < 1024 * 1024 ? `${(finalSize / 1024).toFixed(1)}KB`
            : `${(finalSize / 1024 / 1024).toFixed(1)}MB`,
          chunked: !!transferId,
          ...(ackReceived ? { saved_to: ackSavedTo } : {}),
          ...(ackError ? { ack_error: ackError } : {}),
          protocol: "AGT E2E encrypted (Signal Protocol)",
          note: ackReceived
            ? `File delivered and written to ${ackSavedTo} on '${agentName}'.`
            : `File sent to '${agentName}' 3 times but no write confirmation received. The target agent may not be processing messages yet.`,
        }, null, 2) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: JSON.stringify({
          error: `File transfer failed: ${e.message}`,
        }, null, 2) }] };
      }
    },
  });

  api.registerTool({
    name: "telegram_status",
    label: "Send Telegram Status",
    description: "Send a short status update to the user's Telegram chat configured for this sandbox. Use this for live progress pings during long-running multi-agent tasks (e.g. 'analyst done — 8 sources, 4 platforms scored'). Takes only the text — the chat ID is read from the sandbox's TELEGRAM_ALLOW_FROM env var (configured at `azureclaw up` time). No need to specify channel routing or chat IDs. Returns delivery status without leaking the bot token.",
    parameters: {
      type: "object",
      properties: {
        text: { type: "string", description: "The status message to send (≤4096 chars). Plain text or simple emoji are fine." },
      },
      required: ["text"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const text = String(params.text || "").trim();
      if (!text) {
        return { content: [{ type: "text", text: safeJson({ error: "telegram_status: empty text" }) }] };
      }
      const tgToken = process.env.TELEGRAM_BOT_TOKEN;
      const tgAllowFrom = process.env.TELEGRAM_ALLOW_FROM;
      if (!tgToken || !tgAllowFrom) {
        return { content: [{ type: "text", text: safeJson({
          error: "telegram_status: Telegram is not configured for this sandbox",
          hint: "Run `azureclaw credentials update <name> --telegram-token <token> --telegram-chat-id <id>` to enable.",
        }) }] };
      }
      const chatIds = tgAllowFrom.split(",").map((s: string) => s.trim()).filter(Boolean);
      if (chatIds.length === 0) {
        return { content: [{ type: "text", text: safeJson({ error: "telegram_status: TELEGRAM_ALLOW_FROM is empty" }) }] };
      }
      const truncated = text.length > 4096 ? text.slice(0, 4093) + "..." : text;
      const results: Array<{ chat_id: string; ok: boolean; status?: number; error?: string }> = [];
      for (const chatId of chatIds) {
        try {
          const tgResp = await fetch(`https://api.telegram.org/bot${tgToken}/sendMessage`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ chat_id: chatId, text: truncated }),
          });
          if (tgResp.ok) {
            results.push({ chat_id: chatId, ok: true });
          } else {
            // Sanitize: strip the token from any echoed body before surfacing.
            const body = await tgResp.text().catch(() => "");
            const sanitized = body.replace(tgToken, "[REDACTED]").slice(0, 200);
            results.push({ chat_id: chatId, ok: false, status: tgResp.status, error: sanitized });
            log.warn(`telegram_status: HTTP ${tgResp.status} for chat ${chatId}`);
          }
        } catch (e: any) {
          const msg = String(e?.message || e).replace(tgToken, "[REDACTED]");
          results.push({ chat_id: chatId, ok: false, error: msg });
          log.warn(`telegram_status: send error for chat ${chatId}: ${msg}`);
        }
      }
      const okCount = results.filter((r) => r.ok).length;
      return { content: [{ type: "text", text: safeJson({
        status: okCount === results.length ? "delivered" : okCount > 0 ? "partial" : "failed",
        delivered: okCount,
        total: results.length,
        results,
      }) }] };
    },
  });

  api.registerTool({
    name: "azureclaw_spawn_destroy",
    label: "Destroy Sub-Agent",
    description: "Destroy a spawned sub-agent sandbox. Tears down the K8s namespace, deployment, and all resources. Use this to clean up after the sub-agent has completed its task.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Name of the sub-agent to destroy" },
      },
      required: ["name"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const result = await routerCall("DELETE", `/sandbox/${encodeURIComponent(params.name as string)}`);

        // Clean up stale trust state for the destroyed agent
        try {
          await routerCall("DELETE", `/agt/trust/${encodeURIComponent(params.name as string)}`);
        } catch { /* trust cleanup is best-effort */ }

        // Clean AMID caches
        const amid = nameToAmid.get(params.name as string);
        if (amid) {
          amidToName.delete(amid);
          nameToAmid.delete(params.name as string);
        }

        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Destroy failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_spawn_list",
    label: "List Sub-Agents",
    description: "List all sub-agents spawned from this sandbox. Returns name, phase, model, and governance status for each.",
    parameters: { type: "object", properties: {} },
    async execute(_id: string, _params: Record<string, unknown>) {
      try {
        const result = await routerCall("GET", "/sandbox/list");
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `List failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_discover",
    label: "Discover Agents",
    description: "Search the AgentMesh registry for other agents by name or capability. Returns their AMID, display name, tier, capabilities, and reputation score. Use this to find agents to communicate with via azureclaw_mesh_send or azureclaw_relay.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Agent name or capability to search for. Use '*' to list all known agents." },
      },
      required: ["query"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const query = (params.query as string) || "*";
      try {
        const searchUrl = query === "*"
          ? "/agt/registry/registry/search?capability=azureclaw-agent"
          : `/agt/registry/registry/search?capability=${encodeURIComponent(query)}`;
        const result = await routerCall("GET", searchUrl);
        const agents = (result as any)?.results || [];
        const summary = agents.map((a: any) => ({
          amid: a.amid,
          name: a.display_name,
          tier: a.tier,
          capabilities: a.capabilities,
          reputation: a.reputation_score,
          status: a.status,
          last_seen: a.last_seen,
        }));
        return { content: [{ type: "text", text: safeJson({ agents: summary, count: summary.length }) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Discovery failed: ${e.message}` }] };
      }
    },
  });

  // ── Handoff tools (agent migration) ──────────────────────────────────
  // These allow the LLM to check handoff readiness and trigger migration
  // when the user asks to "continue from the cloud" or similar.

  api.registerTool({
    name: "azureclaw_handoff_status",
    label: "Handoff Status",
    description: "Check handoff (live migration) progress. Returns the current phase and NEW steps since your last poll. Pass since_step (the total_steps from your last call) to get only new updates. Relay each new step to the user immediately as a live update. Keep polling every 3-5 seconds until status is 'complete', 'error', or 'partial'.",
    parameters: {
      type: "object",
      properties: {
        since_step: {
          type: "number",
          description: "Number of steps you already received. Pass total_steps from your last handoff_status call. Omit or pass 0 on first call.",
        },
      },
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        // If there's an active/recent handoff in progress, return that
        if (handoffState.current) {
          const sinceStep = typeof params?.since_step === "number" ? params.since_step : 0;
          const allSteps = handoffState.current.steps;
          const newSteps = sinceStep > 0 ? allSteps.slice(sinceStep) : allSteps;
          return { content: [{ type: "text", text: safeJson({
            phase: handoffState.current.phase,
            status: handoffState.current.status,
            direction: handoffState.current.direction,
            active: handoffState.current.status === "running",
            total_steps: allSteps.length,
            new_steps: newSteps,
            error: handoffState.current.error,
            result: handoffState.current.result,
            instruction: handoffState.current.status === "running"
              ? `Relay ONLY these new_steps to the user right now (one message per step). Then call handoff_status again in 3-5 seconds with since_step=${allSteps.length}.`
              : handoffState.current.status === "complete"
                ? "Relay the final new_steps to the user. The handoff is complete."
                : undefined,
          }) }] };
        }
        const result = await routerCall("GET", "/agt/handoff/status");
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: safeJson({
          handoff_available: false,
          error: e.message,
        }) }] };
      }
    },
  });

  // Only register mutation handoff tools when global registry is active.
  // In local mode the LLM only sees azureclaw_handoff_status which reports
  // handoff_available: false — no point exposing tools that would 409.
  const registryMode = process.env.AGT_REGISTRY_MODE || "local";
  if (registryMode === "global") {

  api.registerTool({
    name: "azureclaw_handoff_request",
    label: "Request Handoff",
    description: "Request a live handoff (migration) of this agent to the cloud or back to local. This creates a PENDING request with a confirmation code that is sent DIRECTLY to the user's Telegram (you will NOT receive the code). The user must type the code back to you, and you pass it to azureclaw_handoff_confirm. Do NOT fabricate or guess the confirmation code. Direction: 'cloud' (local→AKS) or 'local' (AKS→local). IMPORTANT: Always call azureclaw_handoff_status first to check if handoff is available.",
    parameters: {
      type: "object",
      properties: {
        direction: { type: "string", description: "Migration direction: 'cloud' (local→AKS) or 'local' (AKS→local)" },
        reason: { type: "string", description: "Why the handoff is requested (shown in audit log and displayed to user)" },
      },
      required: ["direction"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const direction = (params.direction as string) === "local" ? "aks_to_local" : "local_to_aks";
      const reason = (params.reason as string) || "user_requested";

      // Reverse handoff (cloud→local) must be initiated from the user's laptop CLI.
      // The cloud agent cannot start Docker containers on the user's machine.
      if (direction === "aks_to_local") {
        const agentName = process.env.SANDBOX_NAME || "unknown";
        return { content: [{ type: "text", text: safeJson({
          status: "cli_required",
          direction: "aks_to_local",
          reason,
          instruction: `Reverse handoff must be initiated from the user's laptop. ` +
            `Tell the user to run this command on their terminal:`,
          command: `azureclaw handoff ${agentName} --to local`,
          display: `🔄 Ready to migrate back to local!\n\n` +
            `The handoff back to your laptop needs to be run from your terminal:\n\n` +
            `  azureclaw handoff ${agentName} --to local\n\n` +
            `This will:\n` +
            `  1. Wake your dormant local agent\n` +
            `  2. Transfer all state (chat history, trust scores, workspace)\n` +
            `  3. Restore credentials\n` +
            `  4. Decommission this cloud instance\n\n` +
            `Your local agent will be back online in ~30 seconds.`,
        }) }] };
      }

      try {
        // Stage 1 (§9.9.9): Create a pending handoff request on the router.
        // The router generates a confirmation token — the user must echo it back.
        const result = await routerCall("POST", "/agt/handoff/pending", {
          direction,
          reason,
        });

        const r = result as any;
        if (r?.status === "pending_confirmation") {
          // Send confirmation code directly to Telegram (side-channel).
          // The LLM must NOT see the code — it can only get it from user input.
          const tgToken = process.env.TELEGRAM_BOT_TOKEN;
          const tgAllowFrom = process.env.TELEGRAM_ALLOW_FROM;
          const dirLabel = direction === "local_to_aks" ? "cloud (AKS)" : "local";
          if (tgToken && tgAllowFrom) {
            const chatIds = tgAllowFrom.split(",").map((s: string) => s.trim()).filter(Boolean);
            for (const chatId of chatIds) {
              try {
                const tgResp = await fetch(`https://api.telegram.org/bot${tgToken}/sendMessage`, {
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  body: JSON.stringify({
                    chat_id: chatId,
                    text: `🔐 Handoff Confirmation Required\n\n` +
                      `A handoff to ${dirLabel} has been requested.\n` +
                      `Reason: ${reason}\n\n` +
                      `Your confirmation code:\n\n    ${r.confirmation_token}\n\n` +
                      `Reply with this code to proceed.\n` +
                      `Expires in ${r.expires_in_secs || 300}s.`,
                  }),
                });
                if (!tgResp.ok) log.warn(`Handoff: Telegram code delivery failed: ${tgResp.status}`);
              } catch (tgErr: any) {
                log.warn(`Handoff: Telegram code delivery error: ${tgErr.message}`);
              }
            }
          }
          // Also print to console for TUI users (not visible to the LLM)
          console.log(`\n🔐 Handoff confirmation code: ${r.confirmation_token}\n`);

          return { content: [{ type: "text", text: safeJson({
            status: "pending_confirmation",
            direction: r.direction,
            reason: r.reason,
            expires_in_secs: r.expires_in_secs,
            instruction: `Handoff to ${dirLabel} requested. ` +
              `A confirmation code has been sent to the user's Telegram. ` +
              `Ask the user to type the code. Do NOT guess or fabricate the code.`,
            display: `🔄 Handoff requested to ${dirLabel}\n` +
              `Reason: ${reason}\n\n` +
              `A confirmation code has been sent to your Telegram.\n` +
              `Please type the code here to confirm.`,
          }) }] };
        }

        // Non-pending response (e.g., error)
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Handoff request failed: ${e.message}` }] };
      }
    },
  });

  api.registerTool({
    name: "azureclaw_handoff_confirm",
    label: "Confirm Handoff",
    description: "Confirm a pending handoff request using the confirmation code that the USER typed. You do NOT have the code — it was sent directly to the user's Telegram. You MUST wait for the user to type it. If the user hasn't provided a code, ask them to check their Telegram. After confirming, poll azureclaw_handoff_status every 3-5 seconds and relay each new step to the user as a real-time progress update.",
    parameters: {
      type: "object",
      properties: {
        confirmation_token: { type: "string", description: "The confirmation code the user replied with" },
      },
      required: ["confirmation_token"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const token = params.confirmation_token as string;

      try {
        // ── Stage 2 (§9.9.9): Confirm the pending request ──
        const result = await routerCall("POST", "/agt/handoff/confirm", {
          confirmation_token: token,
        });

        const r = result as any;
        if (r?.status !== "confirmed") {
          return { content: [{ type: "text", text: safeJson(result) }] };
        }

        const handoffToken = r.handoff_token;
        const direction = r.direction;
        const dirLabel = direction === "local_to_aks" ? "cloud (AKS)" : "local";

        // Guard against concurrent handoff — don't overwrite in-progress tracker
        if (handoffState.current?.status === "running") {
          return { content: [{ type: "text", text: safeJson({
            status: "error",
            error: "A handoff is already in progress. Use azureclaw_handoff_status to check.",
          }) }] };
        }

        // Initialize progress tracker
        handoffState.current = {
          phase: "confirmed",
          status: "running",
          steps: [`✅ Handoff confirmed — transferring to ${dirLabel}`],
          direction,
          started_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        };

        const adminToken = await _readAdminToken();
        if (!adminToken) {
          handoffState.current.status = "error";
          handoffState.current.phase = "error";
          handoffState.current.error = "Admin token not found";
          handoffState.current.steps.push("❌ Admin token not found — cannot execute handoff");
          handoffState.current.updated_at = new Date().toISOString();
          return { content: [{ type: "text", text: safeJson(handoffState.current) }] };
        }

        // Run orchestration synchronously — return all progress when complete.
        // LLMs don't autonomously poll tools, so we block here and return
        // the full step-by-step result when the handoff finishes.
        try {
          await _runHandoffOrchestration(handoffToken, adminToken, direction, dirLabel);
        } catch (err: any) {
          log.warn(`Handoff orchestration error: ${err.message}`);
          if (handoffState.current) {
            handoffState.current.status = "error";
            handoffState.current.phase = "error";
            handoffState.current.error = err.message;
            handoffState.current.steps.push(`❌ ${err.message}`);
            handoffState.current.updated_at = new Date().toISOString();
          }
        }

        return { content: [{ type: "text", text: safeJson({
          ...handoffState.current,
          instruction: "Relay each step to the user as a live update summary.",
        }) }] };

      } catch (e: any) {
        return { content: [{ type: "text", text: safeJson({
          status: "error",
          error: e.message,
        }) }] };
      }
    },
  });

  if (!bannerAlreadyPrinted) log.info(`AzureClaw handoff tools registered (registry_mode=${registryMode}): azureclaw_handoff_request, azureclaw_handoff_confirm`);

  } else {
    if (!bannerAlreadyPrinted) log.info(`AzureClaw handoff mutation tools skipped (registry_mode=${registryMode}) — only azureclaw_handoff_status available`);
  }
}
