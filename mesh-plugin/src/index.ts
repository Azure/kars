// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * @kars/mesh — OpenClaw plugin for mesh federation.
 *
 * Enables any OpenClaw agent to:
 * 1. Pair with a trusted kars cluster (one-time)
 * 2. Offload tasks to governed cloud sandboxes
 * 3. Communicate with other mesh agents (send/inbox/discover)
 *
 * Dependencies: ws (WebSocket client)
 * No Docker, no Rust, no kars CLI required.
 */

import { loadOrCreateIdentity, getIdentityPath, type MeshIdentity } from "./identity.js";
import {
  decodeToken,
  savePairing,
  getDefaultPairing,
  type StoredPairing,
} from "./pairing.js";
import {
  createMeshTransport,
  type MeshTransportConfig,
} from "./transport-factory.js";
import type { IMeshTransport } from "./transport-interface.js";
import { TIMEOUTS, RETRIES } from "./timers.js";
import type {
  PairRequestMessage,
  PairResponseMessage,
  OffloadRequestMessage,
  OffloadStatusMessage,
  OffloadProgressMessage,
  OffloadDoneMessage,
  OffloadErrorMessage,
} from "./types.js";
import * as fs from "node:fs";
import * as path from "node:path";
import * as crypto from "node:crypto";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Plugin state — singleton via process-keyed Symbol.
//
// OpenClaw's plugin loader runs through this module twice in some setups
// (tool-registry pass + agent-session pass), and a hot-reload during dev
// re-imports it again. Without a singleton, each pass would build its own
// transport, upload its own prekeys, and open its own WebSocket — exactly
// the duplicate-message / "session already exists" bug pattern that the
// vendored fork's patch #10 originally fixed (preserved here at the
// adapter boundary).
//
// `Symbol.for(...)` lookups are process-global so all imports share the
// same state object. Module-level `let`s mirror the singleton for the
// common read path; `ensureInitialized()` is the only writer and syncs
// both sides.
// ---------------------------------------------------------------------------

interface MeshPluginState {
  connection: IMeshTransport | null;
  meshIdentity: MeshIdentity | null;
  activePairing: StoredPairing | null;
  initialized: boolean;
  /** In-flight init promise — prevents racey concurrent ensureInitialized calls. */
  initPromise: Promise<string | null> | null;
}

const STATE_KEY = Symbol.for("kars.mesh-plugin.state");
const state: MeshPluginState = (() => {
  const proc = process as unknown as Record<symbol, MeshPluginState | undefined>;
  let s = proc[STATE_KEY];
  if (!s) {
    s = {
      connection: null,
      meshIdentity: null,
      activePairing: null,
      initialized: false,
      initPromise: null,
    };
    proc[STATE_KEY] = s;
  }
  return s;
})();

// Local view onto the singleton. Reads only — writes go through `state.*`
// inside ensureInitialized (and are mirrored back here so legacy reads
// see the same connection on subsequent invocations).
let connection: IMeshTransport | null = state.connection;
let meshIdentity: MeshIdentity | null = state.meshIdentity;
let activePairing: StoredPairing | null = state.activePairing;
let initialized: boolean = state.initialized;

/**
 * Offload state. Updated by the background orchestrator; read by
 * offload_status. Keeps the LLM round free of long waits.
 */
type OffloadPhase =
  | "submitted"
  | "validating"
  | "spawning"
  | "scheduled"
  | "ready"
  | "connecting"
  | "acknowledged"
  | "verifying"
  | "uploading"
  | "dispatching"
  | "running"
  | "returning"
  | "done"
  | "error";

interface OffloadStageEvent {
  at: string;
  phase: OffloadPhase;
  message: string;
}

interface ActiveOffload {
  requestId: string;
  startedAt: number;
  sandboxName?: string;
  sandboxAmid?: string;
  filesTotal: number;
  filesSent: number;
  phase: OffloadPhase;
  lastMessage: string;
  events: OffloadStageEvent[];
  error?: string;
  done?: boolean;
  doneSummary?: string;
  doneFiles?: string[];
  tokensUsed?: { prompt: number; completion: number };
  durationSeconds?: number;
  receivedFiles: Array<{ fileName: string; savedPath: string; sizeBytes: number }>;
  pingRttMs?: number;
  taskAckedAt?: number;
  helloReceivedAt?: number; // set when sandbox proactively announces itself
  // Long-poll / change-detection bookkeeping (not persisted)
  lastPolledAt?: number;
  lastPolledEventCount?: number;
}

let activeOffload: ActiveOffload | null = null;

// Recent offloads (completed or in-flight) — used to deduplicate accidental
// LLM re-emissions of the same `cloud_offload` call from stale/repeating
// context.  Kept in memory only; a fresh agent session starts clean.
interface RecentOffloadEntry {
  requestId: string;
  taskNormalized: string;
  submittedAt: number;
  done: boolean;
  status: string;
}
const RECENT_OFFLOADS: RecentOffloadEntry[] = [];
const RECENT_OFFLOAD_WINDOW_MS = 10 * 60 * 1000; // 10 minutes
const RECENT_OFFLOADS_MAX = 20;

function normalizeTask(task: string): string {
  return task.trim().toLowerCase().replace(/\s+/g, " ");
}

function findRecentDuplicateOffload(task: string): RecentOffloadEntry | null {
  const norm = normalizeTask(task);
  const now = Date.now();
  // Prune stale entries while we're here.
  for (let i = RECENT_OFFLOADS.length - 1; i >= 0; i--) {
    if (now - RECENT_OFFLOADS[i].submittedAt > RECENT_OFFLOAD_WINDOW_MS) {
      RECENT_OFFLOADS.splice(i, 1);
    }
  }
  return RECENT_OFFLOADS.find((e) => e.taskNormalized === norm) ?? null;
}

function recordRecentOffload(requestId: string, task: string): void {
  RECENT_OFFLOADS.push({
    requestId,
    taskNormalized: normalizeTask(task),
    submittedAt: Date.now(),
    done: false,
    status: "submitted",
  });
  if (RECENT_OFFLOADS.length > RECENT_OFFLOADS_MAX) {
    RECENT_OFFLOADS.shift();
  }
}

// ─── Ambient user-notification queue ────────────────────────────────────────
// Populated whenever a state change is observed for an active offload (new
// phase, progress tick, file received, done/error). Drained by:
//   • the `message_sending` hook (prepends to outgoing assistant/channel msg)
//   • the `/offload` slash command (renders + clears)
//   • stderr mirror (always-on, for TUIs that tail plugin logs)
// This is intentionally out-of-band from the LLM tool-call loop so progress
// reaches the user without the agent having to poll `offload_status`.
interface PendingNotification {
  requestId: string;
  at: number;
  line: string;        // rendered, user-visible
  kind: "progress" | "done" | "error" | "file" | "phase";
}
const PENDING_NOTIFICATIONS: PendingNotification[] = [];
const PENDING_NOTIF_MAX = 10;
const PENDING_NOTIF_COALESCE_KINDS = new Set<PendingNotification["kind"]>(["progress", "phase"]);

function pushNotification(n: Omit<PendingNotification, "at">): void {
  // Coalesce successive progress/phase lines for the same request — the user
  // only needs the latest ambient tick, not every 20 s sample.
  if (PENDING_NOTIF_COALESCE_KINDS.has(n.kind)) {
    for (let i = PENDING_NOTIFICATIONS.length - 1; i >= 0; i--) {
      const prev = PENDING_NOTIFICATIONS[i];
      if (prev.requestId === n.requestId && PENDING_NOTIF_COALESCE_KINDS.has(prev.kind)) {
        PENDING_NOTIFICATIONS.splice(i, 1);
        break;
      }
    }
  }
  PENDING_NOTIFICATIONS.push({ ...n, at: Date.now() });
  while (PENDING_NOTIFICATIONS.length > PENDING_NOTIF_MAX) {
    PENDING_NOTIFICATIONS.shift();
  }
  // Always also mirror to stderr so TUIs that tail plugin logs render it.
  try {
    process.stderr.write(`[kars-mesh] ${n.line}\n`);
  } catch {
    /* best effort */
  }
}

function drainPendingNotifications(): string[] {
  const out = PENDING_NOTIFICATIONS.map((n) => n.line);
  PENDING_NOTIFICATIONS.length = 0;
  return out;
}

function peekPendingNotifications(): string[] {
  return PENDING_NOTIFICATIONS.map((n) => n.line);
}

// Directory to save incoming files from sandboxes
const INCOMING_DIR = path.join(
  process.env.HOME || process.env.USERPROFILE || "/tmp",
  ".kars-mesh",
  "incoming",
);

// ---------------------------------------------------------------------------
// Plugin entry point
// ---------------------------------------------------------------------------

/**
 * OpenClaw plugin entry. Called by OpenClaw when the plugin is loaded.
 * Registers 6 tools: mesh_pair, cloud_offload, offload_status,
 * mesh_send, mesh_inbox, discover.
 */
export function definePluginEntry() {
  return {
    id: "kars-mesh",
    name: "kars Mesh",
    register(api: any) {
      // ── mesh_pair ──
      api.registerTool({
        name: "mesh_pair",
        description:
          "One-time pairing with an kars cluster using an admin-provided token. " +
          "After pairing, you can use cloud_offload to delegate tasks to governed cloud sandboxes.",
        parameters: {
          type: "object",
          properties: {
            token: {
              type: "string",
              description: "Pairing token (starts with azcp_1_)",
            },
          },
          required: ["token"],
        },
        handler: meshPairHandler,
        execute: meshPairHandler,
      });

      // ── cloud_offload ──
      api.registerTool({
        name: "cloud_offload",
        description:
          "Delegate a task to a governed kars cloud sandbox. " +
          "Optionally send workspace files — they are transferred directly to the sandbox via E2E encrypted mesh (up to 30MB each). " +
          "The sandbox runs with full GPU/inference capabilities and AGT governance. " +
          "Results and output files are returned directly via mesh. Requires prior pairing.",
        parameters: {
          type: "object",
          properties: {
            task: {
              type: "string",
              description: "The task to execute in the cloud sandbox",
            },
            files: {
              type: "array",
              items: { type: "string" },
              description: "Workspace files to send to the sandbox (paths, up to 30MB each)",
            },
            model: {
              type: "string",
              description: "Inference model (default: from cluster config)",
            },
            timeout_minutes: {
              type: "number",
              description: "Maximum runtime in minutes (default: 30)",
            },
            force: {
              type: "boolean",
              description:
                "Allow submitting this task even if an identical one was offloaded in the last 10 minutes. " +
                "Use only when the user EXPLICITLY asks to re-run the task. " +
                "If omitted/false, a duplicate task returns the existing request's status instead of spawning a new sandbox.",
            },
          },
          required: ["task"],
        },
        handler: cloudOffloadHandler,
        execute: cloudOffloadHandler,
      });

      // ── offload_status ──
      // Registered as OPTIONAL (not auto-allowed) so the LLM does not see it
      // by default. Users can expose it via config if they want LLM-driven
      // polling; otherwise ambient push notifications + the /offload command
      // cover the status-query UX without burning tokens on tight poll loops.
      api.registerTool(
        {
          name: "offload_status",
          description:
            "Return a snapshot of the currently active cloud offload (state, " +
            "request id, start time, last update). Non-blocking — returns " +
            "immediately. Call this tool ONCE when the user asks about the " +
            "status, progress, or state of a cloud offload (e.g. 'how is " +
            "the offload going?', 'offload status', 'what's happening with " +
            "the cloud task?'). Do NOT answer those questions from memory " +
            "and do NOT tell the user to type a slash command — invoke this " +
            "tool. Strict rule: call AT MOST ONCE per user message. Do NOT " +
            "call it again in the same turn or in a loop — if the handler " +
            "returns 'no change', stop polling and wait for the next user " +
            "message or a completion event.",
          parameters: {
            type: "object",
            properties: {},
          },
          handler: offloadStatusHandler,
          execute: offloadStatusHandler,
        },
      );

      // ── offload_cancel ──
      // Call only when the user EXPLICITLY asks to cancel/abort the current
      // cloud offload. This sends a cleanup signal to the controller which
      // deletes the cloud sandbox + tears down the namespace, and clears
      // local offload state so a new `cloud_offload` can be started.
      const offloadCancelHandler = async () => {
        if (!activeOffload) {
          return {
            content: [{ type: "text", text: "No active cloud offload to cancel." }],
          };
        }
        const rid = activeOffload.requestId;
        try {
          void sendOffloadCleanup(rid, "cancelled");
        } catch {
          /* best effort — cleanup is async */
        }
        failOffload("Cancelled by user via offload_cancel");
        return {
          content: [
            {
              type: "text",
              text: `🛑 Offload ${rid.slice(0, 8)} cancelled. Cluster CRD cleanup signal sent; the sandbox will be torn down shortly.`,
            },
          ],
        };
      };
      api.registerTool({
        name: "offload_cancel",
        description:
          "Cancel the currently active cloud offload. " +
          "Sends a cleanup signal to the controller (deletes the cloud sandbox) " +
          "and clears local offload state so a new offload can be started. " +
          "Use ONLY when the user explicitly asks to cancel/abort. No-op if no offload is active.",
        parameters: {
          type: "object",
          properties: {},
        },
        handler: offloadCancelHandler,
        execute: offloadCancelHandler,
      });

      // ── mesh_send ──
      api.registerTool({
        name: "mesh_send",
        description:
          "Send an E2E encrypted message to another agent on the mesh. " +
          "Messages use Signal Protocol (Double Ratchet) for forward secrecy.",
        parameters: {
          type: "object",
          properties: {
            to: {
              type: "string",
              description: "Target agent name or AMID",
            },
            message: {
              type: "string",
              description: "Message content (will be E2E encrypted)",
            },
          },
          required: ["to", "message"],
        },
        handler: meshSendHandler,
        execute: meshSendHandler,
      });

      // ── mesh_inbox ──
      api.registerTool({
        name: "mesh_inbox",
        description:
          "Read messages received via the E2E encrypted AGT mesh. ALWAYS call " +
          "this tool FIRST whenever your task description says a peer agent " +
          "has sent you data, output, or a reply — peer messages always " +
          "arrive before you process them, so checking the inbox is your " +
          "default opening move whenever you're told to consume something " +
          "from another agent. Do NOT rely on what you remember from earlier " +
          "turns, because new messages may have arrived since then. Default " +
          "behaviour is *peek-only* and shows only entries you haven't read " +
          "yet; messages stay in the inbox so you can re-read them. Pass " +
          "mark_read=true once you have acted on the contents to flag them " +
          "as seen, or unread_only=false to also see entries from previous " +
          "turns. **Server-side blocking:** pass `block_until_message=true` " +
          "(with optional `timeout_seconds`, default 120, max 300) to wait " +
          "until at least one new message arrives instead of polling. The " +
          "response includes a `diagnostics` block with lifecycle counters " +
          "so you can tell apart 'never received' from 'already consumed by " +
          "an offload waiter'.",
        parameters: {
          type: "object",
          properties: {
            limit: { type: "number", description: "Max messages to return (default: 10)" },
            mark_read: { type: "boolean", description: "When true, flag returned entries as read." },
            unread_only: { type: "boolean", description: "Default true. Set false to include entries you already read." },
            block_until_message: { type: "boolean", description: "When true, block server-side until at least one matching message arrives or `timeout_seconds` elapses. Use this instead of polling." },
            timeout_seconds: { type: "number", description: "Maximum wait when block_until_message=true (default 120, max 300)." },
          },
        },
        handler: meshInboxHandler,
        execute: meshInboxHandler,
      });

      // ── mesh_await ──
      api.registerTool({
        name: "mesh_await",
        description:
          "Block server-side until ALL named peer agents have delivered at " +
          "least one content message (or until timeout). Use this BEFORE " +
          "assembly steps that depend on multiple sibling outputs (e.g. a " +
          "writer waiting on both an analyst and a viz). The tool blocks " +
          "inside a single tool call — you do NOT need to poll mesh_inbox in " +
          "a loop. Returns once every requested sender has arrived, or with " +
          "status 'partial_timeout' listing the missing senders. Combine with " +
          "mark_read=true to flush matched entries and then call mesh_inbox " +
          "to fetch the actual content.",
        parameters: {
          type: "object",
          properties: {
            senders: { type: "array", items: { type: "string" }, description: "Names (or amids) of peer agents you must hear from. Match is case-insensitive on the inbox `from` field." },
            timeout_seconds: { type: "number", description: "Max wait in seconds (default 180, max 600)." },
            mark_read: { type: "boolean", description: "When true, mark matched messages as read on resolve. Default false." },
          },
          required: ["senders"],
        },
        handler: meshAwaitHandler,
        execute: meshAwaitHandler,
      });

      // ── discover ──
      api.registerTool({
        name: "discover",
        description:
          "Find agents on the mesh by capability or name.",
        parameters: {
          type: "object",
          properties: {
            capability: {
              type: "string",
              description: "Filter by capability (e.g., 'analysis', 'review')",
            },
            limit: {
              type: "number",
              description: "Max results (default: 20)",
            },
          },
        },
        handler: discoverHandler,
        execute: discoverHandler,
      });

      // ── /offload slash command (bypasses the LLM) ──────────────────────
      // User types `/offload` or `/offload status` to get an instant,
      // zero-token status snapshot without burning an LLM turn.
      try {
        api.registerCommand?.({
          name: "offload",
          description: "Show active cloud-offload status (no LLM invocation).",
          async handler(_ctx: unknown, args: string[] = []) {
            const sub = (args[0] || "status").toLowerCase();
            if (sub === "cancel") {
              if (!activeOffload) return { reply: "No active offload to cancel." };
              const rid = activeOffload.requestId;
              try { void sendOffloadCleanup(rid, "cancelled"); } catch { /* best effort */ }
              failOffload("Cancelled by user via /offload cancel");
              return { reply: `🛑 Offload ${rid.slice(0, 8)} cancel request sent.` };
            }
            // Default: status
            await drainOffloadInbox().catch(() => { /* best effort */ });
            if (!activeOffload) {
              // Surface any pending notifications for a just-completed offload.
              const pending = drainPendingNotifications();
              if (pending.length > 0) return { reply: pending.join("\n") };
              return { reply: "No active offload. Use cloud_offload to start one." };
            }
            const elapsed = Math.round((Date.now() - activeOffload.startedAt) / 1000);
            const pending = drainPendingNotifications();
            const snap = renderOffloadStatus(elapsed);
            return {
              reply: pending.length > 0 ? pending.join("\n") + "\n\n" + snap : snap,
            };
          },
        });
      } catch { /* older OpenClaw: command API not present */ }

      // ── message_sending hook: ambient push of offload progress ─────────
      // On every outgoing assistant / channel message, prepend queued
      // offload notifications (progress, phase, file, done, error) and
      // clear the queue. No LLM round-trip needed; the user sees live
      // progress piggybacked on any reply the agent produces.
      try {
        api.registerHook?.(
          "message_sending",
          async (event: { content: string }, _ctx: unknown) => {
            const lines = drainPendingNotifications();
            if (lines.length === 0) return;
            const banner = lines.join("\n");
            return { content: `${banner}\n\n${event.content ?? ""}` };
          },
        );
      } catch { /* older OpenClaw: hook API not present */ }

      // ── background inbox-drain service ─────────────────────────────────
      // Keeps offload state advancing (and therefore notifications flowing)
      // even when the agent is idle / the user is typing. Short interval
      // because the cost is a single decrypt-and-consume pass over the SDK
      // inbox queue — no network call.
      try {
        api.registerService?.({
          id: "kars-mesh-offload-drain",
          description: "Drains offload mesh inbox while an offload is active.",
          async start() {
            const INTERVAL_MS = 2000;
            const tick = async () => {
              try {
                if (activeOffload && connection && initialized) {
                  await drainOffloadInbox();
                }
              } catch { /* best effort */ }
            };
            const handle = setInterval(tick, INTERVAL_MS);
            (handle as any).unref?.();
            return {
              async stop() {
                clearInterval(handle);
              },
            };
          },
        });
      } catch { /* older OpenClaw: service API not present */ }
    },
  };
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

async function ensureInitialized(): Promise<string | null> {
  // Fast path — singleton already initialized by a prior plugin load.
  if (state.initialized) {
    if (!initialized) {
      // This module copy hasn't synced yet (re-import after hot-reload or
      // second plugin pass). Mirror the canonical state into our locals.
      connection = state.connection;
      meshIdentity = state.meshIdentity;
      activePairing = state.activePairing;
      initialized = true;
    }
    return null;
  }

  // Coalesce concurrent callers onto a single in-flight init.
  if (state.initPromise) return state.initPromise;

  state.initPromise = (async (): Promise<string | null> => {
    try {
      const id = await loadOrCreateIdentity();
      const pairing = getDefaultPairing();
      let conn: IMeshTransport | null = null;

      if (pairing) {
        try {
          conn = await createMeshTransport({
            relayUrl: pairing.relayUrl,
            registryUrl: pairing.registryUrl,
            identity: id,
            // The controller speaks legacy base64(JSON), not Signal E2E — route
            // its traffic through the plaintext-compat bypass in the SDK.
            plaintextPeers: pairing.controllerAmid
              ? [pairing.controllerAmid]
              : undefined,
          });
          await conn.connect();
        } catch (err: any) {
          // Reset the gate so a later retry can try again instead of
          // permanently caching a failed init promise.
          state.initPromise = null;
          return `Failed to connect to mesh: ${err.message}`;
        }
      }

      // Commit to the singleton AND mirror locally.
      state.meshIdentity = id;
      state.activePairing = pairing;
      state.connection = conn;
      state.initialized = true;
      meshIdentity = id;
      activePairing = pairing;
      connection = conn;
      initialized = true;
      return null;
    } finally {
      // Drop the promise reference once resolved so reads don't hold it
      // forever. A failed init already cleared it above before returning.
      if (state.initialized) state.initPromise = null;
    }
  })();

  return state.initPromise;
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

/** Recursively scan an object for a string value starting with azcp_1_ */
function findTokenInObject(obj: any, depth = 0): string | undefined {
  if (depth > 5) return undefined;
  if (typeof obj === "string") {
    // Check if the string contains a token (LLM may embed it in prose)
    const match = obj.match(/azcp_1_[A-Za-z0-9_\-+=\/]+/);
    if (match) return match[0];
    return undefined;
  }
  if (Array.isArray(obj)) {
    for (const item of obj) {
      const found = findTokenInObject(item, depth + 1);
      if (found) return found;
    }
  } else if (obj && typeof obj === "object") {
    for (const val of Object.values(obj)) {
      const found = findTokenInObject(val, depth + 1);
      if (found) return found;
    }
  }
  return undefined;
}

async function meshPairHandler(...args: any[]): Promise<string> {
  // OpenClaw gateway passes (tool_call_id, params) — the first arg is often
  // just an OpenAI tool_call ID string like "chatcmpl-tool-xxx".
  // Scan ALL arguments to find the real token.
  let token: string | undefined;
  for (const arg of args) {
    // Direct property access on object-shaped args
    const candidate =
      arg?.token ?? arg?.arguments?.token ?? arg?.params?.token;
    if (candidate && typeof candidate === "string" && candidate.startsWith("azcp_1_")) {
      token = candidate;
      break;
    }
    // Deep scan for azcp_1_ in any shape
    const found = findTokenInObject(arg);
    if (found) {
      token = found;
      break;
    }
  }

  if (!token) {
    const dump = args.map((a: any) => JSON.stringify(a)).join(" | ").slice(0, 300);
    return `❌ No token provided. Pass a pairing token starting with azcp_1_. (received args[${args.length}]: ${dump})`;
  }

  const payload = decodeToken(token);
  if (!payload) {
    return `❌ Invalid pairing token. Must start with azcp_1_ and contain valid data. (token length: ${token.length}, starts: ${token.slice(0, 20)}...)`;
  }

  meshIdentity = await loadOrCreateIdentity();
  const identityPath = getIdentityPath();

  // If we already have a live connection to the same relay, reuse it.
  // This avoids double-connect churn when the user re-pairs after the
  // plugin already auto-connected via an existing stored pairing.
  const connectStart = Date.now();
  const reuseExisting =
    connection?.isConnected &&
    (connection as any)["config"]?.relayUrl === payload.relay_url;

  if (!reuseExisting) {
    try { await connection?.disconnect(); } catch { /* noop */ }
    connection = await createMeshTransport({
      relayUrl: payload.relay_url,
      registryUrl: payload.registry_url,
      identity: meshIdentity,
      // The Rust controller still uses the legacy base64(JSON) wire format
      // instead of full Signal E2E. Register it as a plaintext-compat peer
      // so our SDK bypasses X3DH/Ratchet for messages to the controller.
      plaintextPeers: [payload.controller_amid],
    });
    try {
      await connection.connect();
    } catch (err: any) {
      // Differentiate common failure modes so the user can act.
      const msg = String(err?.message || err);
      if (msg.includes("ECONNREFUSED") || msg.includes("EHOSTUNREACH")) {
        return `❌ Cannot reach relay at ${payload.relay_url}. The kars cluster may be offline or the relay address is wrong.`;
      }
      if (msg.includes("timed out")) {
        return `❌ Relay connection timed out (${TIMEOUTS.RELAY_CONNECT / 1000}s). Check your network/proxy to ${payload.relay_url}.`;
      }
      if (msg.includes("certificate") || msg.includes("CERT_")) {
        return `❌ TLS error connecting to relay: ${msg}. Check the relay's certificate.`;
      }
      return `❌ Failed to connect to mesh relay: ${msg}`;
    }
  } else {
    // Live connection reused — make sure the controller is registered as a
    // plaintext-compat peer on this existing client.
    connection?.addPlaintextPeer(payload.controller_amid);
  }

  const connectMs = Date.now() - connectStart;
  if (!connection?.isConnected) {
    return "❌ Connection reported success but isConnected=false. Please try again.";
  }

  // Send pair_request
  const pairRequest: PairRequestMessage = {
    type: "pair_request",
    secret: payload.secret,
    pubkey_ed25519: meshIdentity.signingPublicKey.toString("base64"),
    display_name: `external-${meshIdentity.amid.slice(0, 8)}`,
    capabilities_requested: ["offload", "handoff"],
  };

  try {
    await connection.send(payload.controller_amid, pairRequest);
  } catch (err: any) {
    return `❌ Failed to send pair request: ${err.message}`;
  }

  // Wait for pair_response. Short timeout: the controller's pair handler
  // runs in <1s on a healthy cluster, so longer timeouts just stall the LLM.
  // If the controller isn't there (mesh peer disabled or offline), failing
  // fast is better than waiting.
  const PAIR_TIMEOUT_MS = TIMEOUTS.PAIR_HANDSHAKE;
  let response: PairResponseMessage | null = null;
  try {
    response = await connection.waitForMessage<PairResponseMessage>(
      (content, from) => {
        const msg = content as Record<string, unknown>;
        if (msg?.type !== "pair_response") return null;
        // Only accept from the controller we paired with (prevents spoofing).
        if (from !== payload.controller_amid) return null;
        return msg as unknown as PairResponseMessage;
      },
      PAIR_TIMEOUT_MS,
    );
  } catch {
    // Distinguish "relay dead" from "controller silent"
    const stillConnected = connection.isConnected;
    if (!stillConnected) {
      return `❌ Pairing lost relay connection mid-handshake (connect took ${connectMs}ms). The cluster or relay may be unhealthy.`;
    }
    return [
      `❌ Pairing timed out after ${PAIR_TIMEOUT_MS / 1000}s — the relay is reachable but the cluster controller (${payload.controller_amid.slice(0, 16)}...) did not respond.`,
      `  Common causes (in order of likelihood):`,
      `    1. Mesh peer not enabled on the cluster — run \`kars up\` on the cluster host (federation is on by default in current builds)`,
      `    2. Controller pod is not Running — check \`kubectl get pods -n kars-system\``,
      `    3. Controller is not the leader yet — retry in 15s`,
      `    4. Token belongs to a different cluster/relay — confirm the token is fresh`,
    ].join("\n");
  }

  if (!response.success) {
    return `❌ Pairing rejected by cluster: ${response.error || "Unknown error"}`;
  }

  // Persist pairing
  const clusterName = response.cluster_name || "unknown";
  savePairing(clusterName, {
    controllerAmid: response.controller_amid || payload.controller_amid,
    relayUrl: payload.relay_url,
    registryUrl: payload.registry_url,
    clusterName,
    capabilities: response.capabilities_granted || [],
    slots: response.slots || 1,
    tokenBudget: response.token_budget || 500000,
    expiresAt: response.expires_at || "",
    pairedAt: new Date().toISOString(),
  });

  activePairing = getDefaultPairing();
  initialized = true;

  const budgetStr = (response.token_budget || 500000).toLocaleString();
  return [
    `✅ Paired successfully with kars cluster "${clusterName}"`,
    "",
    `  Your AMID:     ${meshIdentity.amid}`,
    `  Identity:      ${identityPath}`,
    `  Budget:        ${budgetStr} tokens`,
    `  Capabilities:  ${(response.capabilities_granted || []).join(", ")}`,
    `  Expires:       ${response.expires_at || "—"}`,
    `  Relay RTT:     ${connectMs}ms (handshake)`,
    "",
    "You can now use cloud_offload to delegate tasks to the cloud.",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Offload helpers — stage tracking & background orchestrator
// ---------------------------------------------------------------------------

/**
 * Tell the cluster controller to tear down the offload KarsSandbox CRD so
 * the reconciler stops keeping the pod alive. Best-effort: if the mesh is
 * down or the controller rejects the message, we log and continue — the
 * controller also has an idle-timeout fallback.
 */
async function sendOffloadCleanup(
  requestId: string,
  reason: "done" | "error" | "cancelled" | "discovery_timeout",
): Promise<void> {
  if (!connection || !activePairing?.controllerAmid) return;
  try {
    await connection.send(activePairing.controllerAmid, {
      type: "offload_cleanup",
      request_id: requestId,
      reason,
      timestamp: new Date().toISOString(),
    });
    console.log(`[mesh] offload[${requestId.slice(0, 8)}] cleanup requested (${reason})`);
  } catch (err: any) {
    console.warn(`[mesh] offload cleanup send failed: ${err?.message || err}`);
  }
}

function recordStage(phase: OffloadPhase, message: string): void {
  if (!activeOffload) return;
  const prevPhase = activeOffload.phase;
  activeOffload.phase = phase;
  activeOffload.lastMessage = message;
  activeOffload.events.push({
    at: new Date().toISOString(),
    phase,
    message,
  });
  // Cap at last 40 events to keep memory bounded
  if (activeOffload.events.length > 40) {
    activeOffload.events.splice(0, activeOffload.events.length - 40);
  }
  // Mirror into the recent-offload ledger so dedup responses reflect current state.
  const rec = RECENT_OFFLOADS.find((e) => e.requestId === activeOffload!.requestId);
  if (rec) {
    rec.status = phase;
    rec.done = activeOffload.done === true;
  }
  console.log(`[mesh] offload[${activeOffload.requestId.slice(0, 8)}] ${phase}: ${message}`);

  // Ambient push: progress ticks coalesce, phase transitions + file events
  // queue distinct lines so the next outgoing message surfaces them all.
  const shortId = activeOffload.requestId.slice(0, 8);
  if (phase === "returning" && /Received output file/.test(message)) {
    pushNotification({ requestId: activeOffload.requestId, kind: "file", line: `📥 [offload ${shortId}] ${message}` });
  } else if (phase !== prevPhase) {
    pushNotification({ requestId: activeOffload.requestId, kind: "phase", line: `ℹ️ [offload ${shortId}] phase → ${phase}: ${message}` });
  } else {
    pushNotification({ requestId: activeOffload.requestId, kind: "progress", line: `⚙️ [offload ${shortId}] ${message}` });
  }
}

function failOffload(err: string): void {
  if (!activeOffload) return;
  activeOffload.phase = "error";
  activeOffload.lastMessage = err;
  activeOffload.error = err;
  activeOffload.done = true;
  activeOffload.events.push({
    at: new Date().toISOString(),
    phase: "error",
    message: err,
  });
  console.log(`[mesh] offload[${activeOffload.requestId.slice(0, 8)}] FAILED: ${err}`);
  pushNotification({
    requestId: activeOffload.requestId,
    kind: "error",
    line: `❌ [offload ${activeOffload.requestId.slice(0, 8)}] failed: ${err}`,
  });
}

/**
 * Background orchestrator for cloud_offload. Drives the state machine:
 * submitted → validating → spawning → scheduled → ready → connecting →
 * verifying (sandbox ping) → uploading → dispatching → running → done.
 *
 * Runs detached; the LLM gets immediate feedback from cloud_offload and
 * polls progress via offload_status.
 */
async function runOffloadOrchestrator(
  conn: IMeshTransport,
  controllerAmid: string,
  request: OffloadRequestMessage,
  validFiles: string[],
): Promise<void> {
  const requestId = request.request_id;

  // Phase 1: Send offload request to controller (matchmaker)
  try {
    await conn.send(controllerAmid, request);
    recordStage("submitted", "Offload request sent to cluster controller");
  } catch (err: any) {
    failOffload(`Failed to send offload request: ${err.message}`);
    return;
  }

  // Phase 2: Wait for controller to report "ready" with sandbox_name.
  // Also absorb intermediate phases (validating/spawning/scheduled) for UX.
  const readyTimeoutMs = 5 * 60 * 1000;
  let sandboxName: string | null = null;

  try {
    const readyStatus = await conn.waitForMessage<OffloadStatusMessage>(
      (content, from) => {
        if (from !== controllerAmid) return null;
        const msg = content as Record<string, unknown>;
        if (msg?.request_id !== requestId) return null;

        if (msg?.type === "offload_error") {
          throw new Error(`${msg.error} (phase: ${msg.phase})`);
        }

        if (msg?.type === "offload_status") {
          const phase = String(msg.phase) as OffloadPhase;
          const statusMsg = String(msg.message ?? "");
          // Stream intermediate phases into our tracker.
          if (phase === "validating" || phase === "spawning" || phase === "scheduled") {
            recordStage(phase, statusMsg || `Controller: ${phase}`);
            return null; // keep waiting
          }
          if (phase === "ready" && msg.sandbox_name) {
            return msg as unknown as OffloadStatusMessage;
          }
        }
        return null;
      },
      readyTimeoutMs,
    );
    sandboxName = readyStatus.sandbox_name || null;
  } catch (err: any) {
    failOffload(`Controller did not deliver sandbox: ${err.message}`);
    return;
  }

  if (!sandboxName || !activeOffload) {
    failOffload("Controller reported ready but no sandbox name — cannot proceed.");
    return;
  }

  activeOffload.sandboxName = sandboxName;
  recordStage("ready", `Sandbox '${sandboxName}' is running`);

  // Phase 3: Discover sandbox AMID via registry (sandbox auto-registers on boot).
  // Discovery must span the full cold-start budget: pod Ready ≠ mesh-Ready. The
  // sandbox still has to finish entrypoint (incl. up to 120s Entra token retry),
  // load openclaw, load the plugin, dial the relay, and register with the
  // registry. On Kata/confidential pods this can reach 90–120s.
  recordStage("connecting", `Discovering sandbox '${sandboxName}' on the mesh`);
  let sandboxAmid: string | null = null;
  const discoveryIntervalMs = 2000;
  const discoveryAttempts = Math.max(
    15,
    Math.ceil(TIMEOUTS.COLD_START / discoveryIntervalMs),
  );
  for (let i = 0; i < discoveryAttempts; i++) {
    try {
      sandboxAmid = await conn.resolveAmid(sandboxName);
    } catch { /* registry hiccup, retry */ }
    if (sandboxAmid) break;
    if (i > 0 && i % 15 === 0) {
      recordStage(
        "connecting",
        `Still discovering sandbox '${sandboxName}' (attempt ${i}/${discoveryAttempts}) — cold start in progress`,
      );
    }
    await new Promise((r) => setTimeout(r, discoveryIntervalMs));
  }

  if (!sandboxAmid) {
    failOffload(
      `Could not discover sandbox '${sandboxName}' on the mesh after ${discoveryAttempts} attempts (${(discoveryAttempts * discoveryIntervalMs / 1000).toFixed(0)}s). The sandbox may have failed to register.`,
    );
    // Best effort: tell the controller to tear the sandbox down so we don't
    // leave a zombie pod producing orphan messages into mesh_inbox.
    try { void sendOffloadCleanup(requestId, "discovery_timeout"); } catch { /* best effort */ }
    return;
  }

  activeOffload.sandboxAmid = sandboxAmid;

  // Phase 3b: Race for proactive sandbox hello. The sandbox reads OFFLOAD_*
  // env vars set by the controller and auto-sends `offload_hello` the moment
  // its mesh is up. If we receive hello within the window, skip the ping +
  // upload + dispatch round-trip entirely — the sandbox already has the task
  // and is running it. If no hello (older sandbox image, or env path failed),
  // fall back to the legacy ping/upload/dispatch flow for backward compat.
  //
  // Window sized for cold-start cost: controller signals `ready` when the pod
  // is Running (2/2 containers live), but the openclaw plugin still needs to
  // finish Node startup + plugin load + AGT relay connect before it can send
  // `offload_hello`. Typical 25–40s on AKS cold starts; Kata/confidential
  // sandboxes can hit 90–120s, plus the 120s Entra token-retry budget in
  // the sandbox entrypoint. TIMEOUTS.COLD_START (default 180s) spans both.
  const HELLO_WAIT_MS = TIMEOUTS.COLD_START;
  let helloReceived = false;
  try {
    const hello = await conn.waitForMessage(
      (content, from) => {
        if (from !== sandboxAmid) return null;
        const m = content as Record<string, unknown>;
        if (m?.type === "offload_hello" && m?.request_id === requestId) {
          return m;
        }
        return null;
      },
      HELLO_WAIT_MS,
    );
    if (hello) {
      helloReceived = true;
      if (activeOffload) activeOffload.helloReceivedAt = Date.now();
      const preview = String((hello as any).task_preview || "").slice(0, 80);
      recordStage(
        "acknowledged",
        `Sandbox announced itself (proactive) — task received and executing${preview ? ": " + preview : ""}`,
      );
      recordStage("running", "Sandbox is executing the task — progress will stream via mesh");
    }
  } catch { /* waitForMessage may throw on timeout in some impls — fall through */ }

  if (helloReceived) {
    // Sandbox is self-driving via OFFLOAD_* env. Files (if any) were passed
    // via env as well by the controller for this path. Progress, file_transfer
    // outputs, and offload_done will arrive via inbox and be consumed by
    // offload_status. Orchestrator work is complete.
    if (validFiles.length > 0) {
      // Upload files after hello so the sandbox has them available during
      // execution. This is a best-effort path — sandbox runs even without.
      recordStage("uploading", `Uploading ${validFiles.length} file(s) to self-driving sandbox`);
      for (const filePath of validFiles) {
        const fileName = path.basename(filePath);
        try {
          const ack = await conn.sendFile(sandboxAmid, filePath, {
            description: `Offload file for request ${requestId}`,
            timeoutMs: TIMEOUTS.PROGRESS,
            retries: RETRIES.PROGRESS.count,
          });
          if (ack.success) {
            activeOffload.filesSent++;
            recordStage("uploading", `Uploaded ${activeOffload.filesSent}/${validFiles.length}: ${fileName}`);
          }
        } catch { /* best-effort, sandbox may have inline copy */ }
      }
      recordStage("running", "Files uploaded — sandbox continuing execution");
    }
    return;
  }

  // ── Legacy fallback path (sandbox did NOT announce via offload_hello) ──
  // Older sandbox images don't set up proactive offload; use round-trip flow.

  // Phase 4: Sandbox ping verification — confirm the sandbox is actually
  // reachable on the mesh (not just registered) BEFORE sending files/task.
  recordStage("verifying", `Pinging sandbox at ${sandboxAmid.slice(0, 16)}... to verify E2E reachability`);
  try {
    const { rttMs } = await conn.pingPeer(sandboxAmid, {
      timeoutMs: TIMEOUTS.PING,
      retries: RETRIES.PING.count,
    });
    activeOffload.pingRttMs = rttMs;
    recordStage("verifying", `Sandbox acknowledged ping (rtt ${rttMs}ms) — ready for files/task`);
  } catch (err: any) {
    failOffload(`Sandbox '${sandboxName}' is registered but not responding to mesh pings: ${err.message}`);
    return;
  }

  // Phase 5: Upload files (if any) — per-file ACKs already built into sendFile
  if (validFiles.length > 0) {
    recordStage("uploading", `Uploading ${validFiles.length} file(s) to sandbox`);
    for (const filePath of validFiles) {
      const fileName = path.basename(filePath);
      try {
        const ack = await conn.sendFile(sandboxAmid, filePath, {
          description: `Offload file for request ${requestId}`,
          timeoutMs: TIMEOUTS.PROGRESS,
          retries: RETRIES.PROGRESS.count,
        });
        if (!ack.success) {
          failOffload(`File transfer failed for '${fileName}': ${ack.error || "no ACK"}`);
          return;
        }
        activeOffload.filesSent++;
        recordStage("uploading", `Uploaded ${activeOffload.filesSent}/${validFiles.length}: ${fileName}`);
      } catch (err: any) {
        failOffload(`File transfer error for '${fileName}': ${err.message}`);
        return;
      }
    }
  }

  // Phase 6: Dispatch the task and require an ack from the sandbox.
  // Sandbox responds with { type: "task_received", request_id } on receipt.
  recordStage("dispatching", "Sending task to sandbox and awaiting acknowledgment");
  try {
    await conn.sendWithAck(
      sandboxAmid,
      {
        type: "offload_task",
        request_id: requestId,
        task: request.task,
        files: request.files,
        from_agent: conn.amid,
        timestamp: new Date().toISOString(),
      },
      (content, from) => {
        if (from !== sandboxAmid) return null;
        const m = content as Record<string, unknown>;
        if (m?.type === "task_received" && m?.request_id === requestId) {
          return m;
        }
        return null;
      },
      { timeoutMs: TIMEOUTS.ACK, retries: RETRIES.ACK.count, retryDelayMs: RETRIES.ACK.delayMs },
    );
    if (activeOffload) activeOffload.taskAckedAt = Date.now();
    recordStage("running", "Sandbox acknowledged task — execution in progress");
  } catch (err: any) {
    // Task may still have been received; surface a clear warning but mark failed.
    failOffload(`Sandbox did not acknowledge task receipt: ${err.message}`);
    return;
  }
}

// ---------------------------------------------------------------------------
// cloud_offload — immediate return, background orchestrator drives stages
// ---------------------------------------------------------------------------

async function cloudOffloadHandler(...args: any[]): Promise<string> {
  const raw = extractParams(args);

  // Accept param aliases — LLMs frequently pass `prompt`, `description`,
  // `request`, or `content` instead of the declared `task`. Normalize here.
  const params: {
    task: string;
    files?: string[];
    model?: string;
    timeout_minutes?: number;
    force?: boolean;
  } = {
    task: String(
      (raw as any).task ??
      (raw as any).prompt ??
      (raw as any).description ??
      (raw as any).request ??
      (raw as any).content ??
      (raw as any).instruction ??
      (raw as any).query ??
      "",
    ),
    files: Array.isArray((raw as any).files)
      ? ((raw as any).files as string[])
      : Array.isArray((raw as any).file_paths)
        ? ((raw as any).file_paths as string[])
        : undefined,
    model: typeof (raw as any).model === "string" ? (raw as any).model : undefined,
    timeout_minutes: typeof (raw as any).timeout_minutes === "number"
      ? (raw as any).timeout_minutes
      : typeof (raw as any).timeout === "number"
        ? (raw as any).timeout
        : undefined,
    force: (raw as any).force === true || (raw as any).force === "true",
  };

  if (!params.task || !params.task.trim()) {
    return "❌ cloud_offload requires a `task` parameter describing what the sandbox should do. " +
           "Aliases accepted: task, prompt, description, request, content, instruction, query.";
  }

  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!activePairing) return "❌ Not paired with any kars cluster. Use mesh_pair first.";
  if (!connection?.isConnected) {
    // Attempt a one-shot reconnect before failing — avoids hard failure on
    // transient WebSocket flaps. If the connection is still broken, bail with
    // a clear message.
    try { await connection?.connect(); } catch { /* noop */ }
    if (!connection?.isConnected) {
      return "❌ Mesh connection is not live. Wait for automatic reconnect or call mesh_pair again.";
    }
  }
  if (activeOffload && !activeOffload.done) {
    return `❌ Offload already in progress (${activeOffload.requestId}). Use \`/offload\` for status or \`/offload cancel\` to abort.`;
  }

  // Dedup: LLMs at high context saturation often re-emit the same tool call
  // from stale history. Unless the user explicitly asks to re-run (force=true),
  // return the existing offload's status instead of spawning a duplicate
  // sandbox on the cluster.
  if (!params.force) {
    const dup = findRecentDuplicateOffload(params.task);
    if (dup) {
      const ageSec = Math.round((Date.now() - dup.submittedAt) / 1000);
      return [
        `⚠️  This task was already offloaded ${ageSec}s ago — not starting a new sandbox.`,
        "",
        `  Existing request ID: ${dup.requestId}`,
        `  Status:              ${dup.status}${dup.done ? " (done)" : " (in progress)"}`,
        "",
        "Use `offload_status` only if the user explicitly asks you to.",
        "Otherwise ambient progress notifications will arrive automatically on",
        "your next reply — do NOT poll `mesh_inbox` or `offload_status`.",
        "To run this task again anyway (e.g., user explicitly requested a re-run),",
        "pass `force: true` to cloud_offload.",
      ].join("\n");
    }
  }

  const requestId = crypto.randomUUID();

  // Validate files upfront (before sending request to controller)
  const filePaths = params.files || [];
  let totalBytes = 0;
  const validFiles: string[] = [];
  for (const f of filePaths) {
    try {
      const stat = fs.statSync(f);
      if (stat.size > 30 * 1024 * 1024) {
        return `❌ File too large: ${f} (${(stat.size / 1024 / 1024).toFixed(1)} MB, max 30MB)`;
      }
      totalBytes += stat.size;
      validFiles.push(f);
    } catch {
      return `❌ Cannot read file: ${f}`;
    }
  }

  const request: OffloadRequestMessage = {
    type: "offload_request",
    task: params.task,
    files: validFiles.map((f) => path.basename(f)),
    file_count: validFiles.length,
    total_bytes: totalBytes,
    preferences: {
      model: params.model,
      timeout_minutes: params.timeout_minutes || 30,
    },
    request_id: requestId,
    timestamp: new Date().toISOString(),
  };

  // Initialize tracker BEFORE launching orchestrator so the first
  // recordStage() call has state to write into.
  activeOffload = {
    requestId,
    startedAt: Date.now(),
    filesTotal: validFiles.length,
    filesSent: 0,
    phase: "submitted",
    lastMessage: "Dispatching offload request",
    events: [],
    receivedFiles: [],
  };
  recordStage("submitted", "Validating offload request locally");
  recordRecentOffload(requestId, params.task);

  // Background orchestrator — does NOT block the LLM round.
  // Errors are captured into activeOffload via failOffload(); the LLM polls
  // via offload_status to observe progress.
  setImmediate(() => {
    runOffloadOrchestrator(connection!, activePairing!.controllerAmid, request, validFiles)
      .catch((err) => {
        failOffload(`Orchestrator crashed: ${err?.message || String(err)}`);
      });
  });

  const filesInfo = validFiles.length > 0
    ? `${validFiles.length} file(s) (${(totalBytes / 1024).toFixed(1)} KB) queued`
    : "no files";

  return [
    `☁️ Offload submitted — orchestrating in background`,
    "",
    `  Request ID:    ${requestId}`,
    `  Model:         ${params.model || "default"}`,
    `  Task:          ${params.task.slice(0, 120)}${params.task.length > 120 ? "..." : ""}`,
    `  Files:         ${filesInfo}`,
    `  Timeout:       ${params.timeout_minutes || 30}m`,
    "",
    "The sandbox is orchestrating in background. DO NOT call any tools to",
    "check on it — offload_status, mesh_inbox, exec_command, etc. will only",
    "waste tokens. Progress, file deliveries, and completion are pushed to",
    "the user automatically as ambient notifications prepended to your next",
    "reply. The user can also type `/offload` for an instant status line.",
    "",
    "Your next action: send the user a brief acknowledgement of the",
    "submission (1–2 sentences) and then STOP tool-calling. The system",
    "will surface progress on the next turn without any polling from you.",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// offload_status — polls tracker + inbox; no blocking waits
// ---------------------------------------------------------------------------

// Extract the first object-shaped argument from a (tool_call_id, params) call.
// OpenClaw gateway invokes non-bundled plugin handlers with this signature.
function extractParams(args: any[]): Record<string, unknown> {
  for (const a of args) {
    if (a && typeof a === "object" && !Array.isArray(a)) {
      return a as Record<string, unknown>;
    }
  }
  return {};
}

async function offloadStatusHandler(..._args: any[]): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";
  if (!activeOffload) return "No active offload. Use cloud_offload to start one.";

  // Anti-poll throttle: if called within 15s of the previous call AND no new
  // events arrived in the meantime, return a short "no change" response so the
  // LLM doesn't waste tokens burst-polling. A non-zero delta always returns
  // the full snapshot.
  const now = Date.now();
  const lastAt = activeOffload.lastPolledAt ?? 0;
  const lastCount = activeOffload.lastPolledEventCount ?? 0;
  const sinceLastMs = now - lastAt;

  // Snapshot-only: drain inbox once, return immediately. Ambient updates are
  // delivered out-of-band via the message_sending hook, /offload slash command,
  // and a background drain service — so we never block the LLM turn on polling.
  await drainOffloadInbox();

  const elapsed = Math.round((now - activeOffload.startedAt) / 1000);
  const newEvents = activeOffload.events.length - lastCount;
  activeOffload.lastPolledAt = Date.now();
  activeOffload.lastPolledEventCount = activeOffload.events.length;

  if (lastAt > 0 && sinceLastMs < 15_000 && newEvents <= 0) {
    const secs = Math.round(sinceLastMs / 1000);
    return (
      `⏳ No change since last check ${secs}s ago (offload still running, ` +
      `elapsed ${elapsed}s). Stop polling — wait for the user to ask again, ` +
      `or wait for the next completion event.`
    );
  }

  return renderOffloadStatus(elapsed);
}

/** Shared inbox-draining logic used by both the initial pass and long-poll ticks. */
async function drainOffloadInbox(): Promise<void> {
  if (!activeOffload || !connection) return;
  const requestId = activeOffload.requestId;
  const sandboxAmid = activeOffload.sandboxAmid;

  const claimed = connection.consumeInbox((m) => {
    const c = m.content as Record<string, unknown> | null;
    if (!c) return false;
    if (c.request_id === requestId) return true;
    if (sandboxAmid && m.from === sandboxAmid) return true;
    return false;
  });

  let latestProgress: OffloadProgressMessage | null = null;
  let doneMsg: OffloadDoneMessage | null = null;
  let errorMsg: OffloadErrorMessage | null = null;

  for (const msg of claimed) {
    const content = msg.content as Record<string, unknown>;
    switch (content.type) {
      case "offload_hello":
        if (sandboxAmid && msg.from === sandboxAmid && content.request_id === activeOffload.requestId) {
          if (!activeOffload.helloReceivedAt) {
            activeOffload.helloReceivedAt = Date.now();
            const preview = String(content.task_preview || "").slice(0, 80);
            recordStage(
              "acknowledged",
              `Sandbox announced itself (late)${preview ? ": " + preview : ""}`,
            );
          }
        }
        break;
      case "offload_progress":
        latestProgress = content as unknown as OffloadProgressMessage;
        break;
      case "offload_done":
        doneMsg = content as unknown as OffloadDoneMessage;
        break;
      case "offload_error":
        errorMsg = content as unknown as OffloadErrorMessage;
        break;
      case "file_transfer":
        if (sandboxAmid && msg.from === sandboxAmid && content.file_name) {
          try {
            const result = await connection.handleFileTransfer(
              msg.from,
              content,
              INCOMING_DIR,
            );
            if (result) {
              activeOffload.receivedFiles.push(result);
              recordStage(
                "returning",
                `Received output file: ${result.fileName} (${(result.sizeBytes / 1024).toFixed(1)} KB)`,
              );
            }
          } catch { /* best effort */ }
        }
        break;
    }
  }

  if (errorMsg && activeOffload.phase !== "error") {
    failOffload(`Sandbox reported error (phase: ${errorMsg.phase}): ${errorMsg.error}`);
    try {
      void sendOffloadCleanup(activeOffload.requestId, "error");
    } catch { /* best effort */ }
  }

  if (latestProgress && !activeOffload.done) {
    recordStage(
      "running",
      `${latestProgress.stage} ${latestProgress.pct}% — ${latestProgress.message}`,
    );
  }

  if (doneMsg && !activeOffload.done) {
    activeOffload.phase = "done";
    activeOffload.done = true;
    activeOffload.doneSummary = doneMsg.summary;
    activeOffload.doneFiles = doneMsg.output_files || [];
    activeOffload.tokensUsed = doneMsg.tokens_used;
    activeOffload.durationSeconds = doneMsg.duration_seconds;
    activeOffload.lastMessage = "Task complete";
    const shortId = activeOffload.requestId.slice(0, 8);
    const outFiles = doneMsg.output_files ?? [];
    const fileSuffix = outFiles.length > 0 ? ` — files: ${outFiles.join(", ")}` : "";
    const tokenSuffix =
      typeof doneMsg.tokens_used === "number" && doneMsg.tokens_used > 0
        ? ` · ${doneMsg.tokens_used} tokens`
        : "";
    pushNotification({
      requestId: activeOffload.requestId,
      kind: "done",
      line: `✅ [offload ${shortId}] complete (${doneMsg.duration_seconds ?? "?"}s${tokenSuffix})${fileSuffix}`,
    });
    // Fire-and-forget cleanup request to the controller so the KarsSandbox CRD
    // is torn down (and the reconciler stops keeping the offload pod alive).
    try {
      void sendOffloadCleanup(activeOffload.requestId, "done");
    } catch { /* best effort */ }
  }
}

function renderOffloadStatus(elapsed: number): string {
  if (!activeOffload) return "No active offload.";

  if (activeOffload.phase === "done" && activeOffload.doneSummary !== undefined) {
    const total = (activeOffload.tokensUsed?.prompt ?? 0) + (activeOffload.tokensUsed?.completion ?? 0);
    const lines = [
      `✅ Offload complete`,
      "",
      `  Request ID:    ${activeOffload.requestId}`,
      `  Duration:      ${activeOffload.durationSeconds ?? elapsed}s`,
      `  Tokens used:   ${total.toLocaleString()}`,
    ];
    if ((activeOffload.doneFiles ?? []).length > 0) {
      lines.push(`  Output files:  ${(activeOffload.doneFiles ?? []).join(", ")}`);
    }
    if (activeOffload.receivedFiles.length > 0) {
      lines.push(`  Saved files:   ${activeOffload.receivedFiles.length}`);
      for (const f of activeOffload.receivedFiles) {
        lines.push(`    • ${f.fileName} (${(f.sizeBytes / 1024).toFixed(1)} KB) → ${f.savedPath}`);
      }
    }
    lines.push("", activeOffload.doneSummary);
    // Clear after delivery so next cloud_offload can run
    const out = lines.join("\n");
    activeOffload = null;
    return out;
  }

  if (activeOffload.phase === "error") {
    const out = [
      `❌ Offload failed [${elapsed}s]`,
      "",
      `  Request ID:    ${activeOffload.requestId}`,
      `  Phase:         ${activeOffload.events[activeOffload.events.length - 2]?.phase || "?"}`,
      `  Error:         ${activeOffload.error || activeOffload.lastMessage}`,
    ].join("\n");
    activeOffload = null;
    return out;
  }

  const phaseIcon: Record<OffloadPhase, string> = {
    submitted: "📤",
    validating: "🔎",
    spawning: "🚀",
    scheduled: "📅",
    ready: "🟢",
    connecting: "🔗",
    acknowledged: "👋",
    verifying: "🏓",
    uploading: "📦",
    dispatching: "📬",
    running: "⚙️",
    returning: "📥",
    done: "✅",
    error: "❌",
  };

  const lines: string[] = [
    `☁️ Offload in progress [${elapsed}s]`,
    "",
    `  Request ID:    ${activeOffload.requestId}`,
    `  Sandbox:       ${activeOffload.sandboxName || "— (awaiting controller)"}`,
    `  Phase:         ${phaseIcon[activeOffload.phase] || "•"} ${activeOffload.phase}`,
    `  Current:       ${activeOffload.lastMessage}`,
  ];
  if (activeOffload.pingRttMs !== undefined) {
    lines.push(`  Sandbox ping:  ${activeOffload.pingRttMs}ms RTT`);
  }
  if (activeOffload.filesTotal > 0) {
    lines.push(`  Files:         ${activeOffload.filesSent}/${activeOffload.filesTotal} uploaded`);
  }
  if (activeOffload.taskAckedAt) {
    const ackAge = Math.round((Date.now() - activeOffload.taskAckedAt) / 1000);
    lines.push(`  Task ack:      ${ackAge}s ago`);
  }

  // Recent events (last 5) for trail
  const recent = activeOffload.events.slice(-5);
  if (recent.length > 1) {
    lines.push("", "  Recent stages:");
    for (const e of recent) {
      lines.push(`    • ${e.phase}: ${e.message}`);
    }
  }

  lines.push("", "Results and output files will arrive automatically via mesh.");
  return lines.join("\n");
}

async function meshSendHandler(...args: any[]): Promise<string> {
  const params = extractParams(args) as {
    to?: string; to_amid?: string; to_agent?: string; target?: string; amid?: string; recipient?: string;
    message?: string; content?: string; body?: string; text?: string;
  };
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection?.isConnected) return "❌ Mesh client not connected. Pair first with mesh_pair.";

  // LLMs frequently use parameter aliases — accept common variants.
  const to = params.to ?? params.to_amid ?? params.to_agent ?? params.target ?? params.amid ?? params.recipient;
  const message = params.message ?? params.content ?? params.body ?? params.text;
  if (!to || typeof to !== "string") {
    return "❌ mesh_send requires `to` (AMID or agent name). Use `discover` to list known agents.";
  }
  if (!message || typeof message !== "string") {
    return "❌ mesh_send requires `message` content.";
  }

  // Resolve a display-name to an AMID if needed (AMIDs are ~27+ char base58).
  let targetAmid = to;
  if (!/^[A-Za-z0-9]{20,}$/.test(to) && connection) {
    try {
      const resolved = await connection.resolveAmid(to);
      if (resolved) targetAmid = resolved;
    } catch {
      // fall through; registry resolution is best-effort
    }
  }

  try {
    await connection.send(targetAmid, { type: "message", content: message });
    const shown = targetAmid === to ? targetAmid : `${to} (${targetAmid.slice(0, 16)}...)`;
    return `✓ Message sent to ${shown} (E2E encrypted)`;
  } catch (err: any) {
    return `❌ Send failed: ${err.message}`;
  }
}

async function meshInboxHandler(...args: any[]): Promise<string> {
  const params = extractParams(args) as {
    limit?: number;
    mark_read?: boolean;
    unread_only?: boolean;
    block_until_message?: boolean;
    timeout_seconds?: number;
  };
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";

  const markRead = params.mark_read === true;
  const unreadOnly = params.unread_only !== false; // default true
  const limit = typeof params.limit === "number" && params.limit > 0
    ? Math.floor(params.limit)
    : 10;
  const blockUntilMessage = params.block_until_message === true;
  const timeoutSeconds = typeof params.timeout_seconds === "number" && params.timeout_seconds > 0
    ? Math.min(Math.floor(params.timeout_seconds), 300)
    : 120;

  const computeVisible = (): ReturnType<IMeshTransport["getInbox"]> => {
    const all = connection!.getInbox();
    return unreadOnly ? all.filter((m) => !m.read_at) : all;
  };

  let visible = computeVisible();
  // Server-side blocking — replaces the LLM polling pattern.
  if (blockUntilMessage && visible.length === 0) {
    const deadline = Date.now() + timeoutSeconds * 1000;
    while (visible.length === 0 && Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const woke = await connection.waitForInbox(remaining);
      visible = computeVisible();
      if (!woke) break;
    }
  }

  const slice = visible.slice(-limit);

  if (markRead && slice.length > 0) {
    connection.markRead(slice.map((m) => m.id));
  }

  const diag = connection.getDiagnostics();
  const unreadCount = connection.getUnreadCount();
  const totalInbox = connection.getInbox().length;

  if (slice.length === 0) {
    // Empty inbox → still return diagnostics so operators / the LLM can
    // distinguish "never received" from "already consumed by an offload
    // waiter / mesh_send reply waiter / earlier mark_read".
    const summary = unreadOnly && unreadCount === 0 && totalInbox > 0
      ? `📭 No unread messages (${totalInbox} already read; pass unread_only=false to re-read).`
      : blockUntilMessage
        ? `📭 Inbox empty (waited ${timeoutSeconds}s blocking, no message arrived).`
        : "📭 Inbox empty.";
    return `${summary}\n\nDiagnostics: ${JSON.stringify(diag)}`;
  }

  const lines = slice.map((m, i) => {
    const content =
      typeof m.content === "string"
        ? m.content.slice(0, 200)
        : JSON.stringify(m.content).slice(0, 200);
    const tag = m.read_at ? "·read" : "·new";
    return `  ${i + 1}. [${m.from}] (${tag}) ${content}`;
  });

  const header = `📬 ${slice.length} of ${totalInbox} message(s) (${unreadCount} unread${markRead ? `, ${slice.length} now marked read` : ""}):`;
  return `${header}\n${lines.join("\n")}\n\nDiagnostics: ${JSON.stringify(diag)}`;
}

async function meshAwaitHandler(...args: any[]): Promise<string> {
  const params = extractParams(args) as {
    senders?: unknown;
    timeout_seconds?: number;
    mark_read?: boolean;
  };
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";

  const sendersRaw = params.senders;
  if (!Array.isArray(sendersRaw) || sendersRaw.length === 0) {
    return "❌ mesh_await: `senders` must be a non-empty array of agent names.";
  }
  const wantedSenders = (sendersRaw as unknown[])
    .filter((s): s is string => typeof s === "string" && s.trim().length > 0)
    .map((s) => s.trim());
  if (wantedSenders.length === 0) {
    return "❌ mesh_await: `senders` must contain at least one non-empty agent name.";
  }
  const wantedSet = new Set(wantedSenders.map((s) => s.toLowerCase()));
  const timeoutSeconds = typeof params.timeout_seconds === "number" && params.timeout_seconds > 0
    ? Math.min(Math.floor(params.timeout_seconds), 600)
    : 180;
  const markReadOnResolve = params.mark_read === true;

  const computeMatches = (): Map<string, string[]> => {
    const out = new Map<string, string[]>();
    for (const m of connection!.getInbox()) {
      if (m.read_at) continue;
      const fromAmid = (m.from || "").toLowerCase();
      const contentObj = (m.content && typeof m.content === "object")
        ? (m.content as Record<string, unknown>)
        : null;
      const fromAgent = (contentObj && typeof contentObj.from_agent === "string")
        ? (contentObj.from_agent as string).toLowerCase()
        : "";
      let matchedKey: string | null = null;
      if (wantedSet.has(fromAmid)) matchedKey = fromAmid;
      else if (fromAgent && wantedSet.has(fromAgent)) matchedKey = fromAgent;
      if (!matchedKey) continue;
      const list = out.get(matchedKey) ?? [];
      list.push(m.id);
      out.set(matchedKey, list);
    }
    return out;
  };

  let matches = computeMatches();
  const startedAt = Date.now();
  if (matches.size < wantedSet.size) {
    const deadline = startedAt + timeoutSeconds * 1000;
    while (matches.size < wantedSet.size && Date.now() < deadline) {
      const remaining = Math.max(1, deadline - Date.now());
      const woke = await connection.waitForInbox(remaining);
      matches = computeMatches();
      if (!woke) break;
    }
  }

  const missing: string[] = [];
  for (const wanted of wantedSet) if (!matches.has(wanted)) missing.push(wanted);

  let markedRead = 0;
  if (markReadOnResolve) {
    const allMatchedIds: string[] = [];
    for (const ids of matches.values()) for (const id of ids) allMatchedIds.push(id);
    if (allMatchedIds.length > 0) {
      markedRead = connection.markRead(allMatchedIds);
    }
  }

  const matchedSummary: Record<string, string[]> = {};
  for (const [sender, ids] of matches) matchedSummary[sender] = ids;

  const status = missing.length === 0 ? "all_received" : "partial_timeout";
  const note = missing.length === 0
    ? "All requested senders delivered. Call mesh_inbox to read message contents."
    : `Timeout: missing ${missing.join(", ")}. Call mesh_inbox to inspect what arrived; retry mesh_await for the missing senders or proceed with partial input.`;

  return JSON.stringify({
    status,
    requested_senders: wantedSenders,
    matched: matchedSummary,
    missing,
    mark_read: markReadOnResolve,
    marked_read_count: markedRead,
    waited_seconds: Math.round((Date.now() - startedAt) / 1000),
    timeout_seconds: timeoutSeconds,
    note,
  });
}

async function discoverHandler(...args: any[]): Promise<string> {
  const params = extractParams(args) as { capability?: string; limit?: number };
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";

  try {
    const results = await connection.discover({
      capability: params.capability,
      limit: params.limit || 20,
    });

    if (results.length === 0) return "No agents found on the mesh.";

    const lines = results.map((a) =>
      `  ${a.amid?.slice(0, 16)}...  ${a.displayName || "—"}  [${(a.capabilities || []).join(", ")}]`
    );
    return `🌐 ${results.length} agent(s):\n${lines.join("\n")}`;
  } catch (err: any) {
    return `❌ Discovery failed: ${err.message}`;
  }
}

// Gateway-compatible named export: OpenClaw's runtime plugin loader
// looks for a top-level `register` or `activate` export on non-bundled
// extensions discovered from ~/.openclaw-data/extensions/.
export function register(api: any) {
  const entry = definePluginEntry();
  entry.register(api);
}

// Also expose as `activate` (alternate name accepted by the gateway).
export const activate = register;

// Re-export for consumers
export { decodeToken } from "./pairing.js";
export { createMeshTransport } from "./transport-factory.js";
export {
  generateIdentity,
  loadIdentity,
  loadOrCreateIdentity,
  verifyEd25519Signature,
  getIdentityPath,
} from "./identity.js";
export type { MeshIdentity } from "./identity.js";
export type * from "./types.js";
