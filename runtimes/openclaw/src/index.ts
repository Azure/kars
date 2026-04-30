// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AzureClaw — OpenClaw Plugin
 *
 * Registers AzureClaw commands, Azure OpenAI as a model provider,
 * and agent tools (spawn, mesh, inbox, destroy) within the OpenClaw
 * plugin system using the native definePluginEntry SDK.
 *
 * AGT Integration: Uses @agentmesh/sdk for tool-level policy evaluation,
 * trust scoring, and audit logging. AzureClaw's Rust router handles
 * infrastructure-level controls (mesh routing, content safety, token budgets).
 *
 * Usage: openclaw azureclaw <command>
 */

import type { Command } from "commander";
import { createRequire as __createRequire__ } from "node:module";

// ---------------------------------------------------------------------------
// CommonJS interop shim — OpenClaw 2026.4.x loads plugin entries as native
// ESM via jiti. `cli/package.json` declares `"type": "module"`, so the
// global `require` is undefined inside this file at runtime. We synthesize
// a CJS-style require bound to *this module's* URL so that the small number
// of `require(...)` call-sites below (Node built-ins + the OpenClaw plugin
// SDK that does not yet publish ESM exports) keep working under both
// CommonJS and ESM loaders. Do NOT add new `require` call-sites — prefer
// `await import()` for any new dynamic dependency.
// ---------------------------------------------------------------------------
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const require: NodeRequire = (() => {
  try {
    // ESM context: derive a require bound to this module URL.
    return __createRequire__(import.meta.url);
  } catch {
    // CJS context (tsc target=commonjs, jest/vitest, older OpenClaw):
    // the runtime-provided `require` is already in scope.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any, no-undef
    return (globalThis as any).require ?? __createRequire__(process.cwd() + "/");
  }
})();

// ---------------------------------------------------------------------------
// OpenClaw Plugin SDK — loaded dynamically at runtime from the host OpenClaw
// installation. definePluginEntry wraps the plugin definition so OpenClaw
// provides the full API (registerTool, registerProvider, registerCommand, etc.).
// ---------------------------------------------------------------------------

let definePluginEntry: (def: any) => any;
try {
  const sdk = require("openclaw/plugin-sdk/plugin-entry");
  definePluginEntry = sdk.definePluginEntry;
} catch {
  // Fallback: identity wrapper — plain object pattern for older OpenClaw
  definePluginEntry = (def: any) => def;
}

// Prevent unhandled rejections from crashing the process.
// The read-only rootfs causes EPERM in chokidar file watchers — those
// are non-fatal but show up as unhandled rejections that kill Node.
process.on("unhandledRejection", (reason: any) => {
  const msg = reason?.message || String(reason);
  // EPERM from file watchers on read-only rootfs — harmless, suppress
  if (msg.includes("EPERM") && msg.includes("watch")) return;
  console.error("[azureclaw] Unhandled rejection (suppressed crash):", msg);
});

// ---------------------------------------------------------------------------
// Router URL configuration — single source of truth (plan item q7).
//
// The sandbox's iptables egress-guard blocks UID 1000 from direct network
// egress except to 127.0.0.1:8443 (inference router) and DNS. All plugin
// traffic therefore flows through ROUTER_BASE. Override via
// AZURECLAW_ROUTER_URL for tests (FakeRouter, docker-compose.dev.yml).
//
// Late-binding: the env var is re-read on every call so tests can set it
// after module load.
// ---------------------------------------------------------------------------
// Router URL + I/O helpers live in cli/src/core/router-client.ts. The URL
// helpers (`routerBase`/`routerWsBase`/...) are re-exported below to preserve
// the `import { routerBase, ... } from "./plugin.js"` surface used by tests
// and downstream CLI commands.
import {
  routerBase,
  routerWsBase,
  routerUrl,
  routerWsUrl,
  routerCall as _routerCall,
  routerCallStrict as _routerCallStrict,
  readAdminToken as _readAdminToken,
  readAdminTokenSync as _readAdminTokenSync,
  pushTrustToRouter,
  pushSigningCounter,
} from "./core/router-client.js";
import { redactSecrets, sanitizeLog } from "./core/log-redact.js";
import {
  amidToName,
  nameToAmid,
  parentTrustedAmids,
  peerSigningKeys,
  getCachedAmid,
  pickFreshestRegistryMatch,
  resolveAmidByName as _resolveAmidByName,
  resolveAmidToName as _resolveAmidToName,
  resolveSigningKey as _resolveSigningKey,
} from "./core/amid-cache.js";

// Thin wrappers so internal callers don't need to thread `routerUrl` through.
async function resolveAmidByName(
  agentName: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  opts: { timeoutMs?: number; registryBase?: string; scopeFilter?: (a: any) => boolean; bypassCache?: boolean } = {},
): Promise<string | undefined> {
  return _resolveAmidByName(agentName, routerUrl, opts);
}
async function resolveAmidToName(amid: string): Promise<string> {
  return _resolveAmidToName(amid, routerUrl);
}
async function resolveSigningKey(amid: string): Promise<string> {
  return _resolveSigningKey(amid, routerUrl);
}

export { routerBase, routerWsBase, routerUrl, routerWsUrl };
export { redactSecrets };

// ---------------------------------------------------------------------------
// AGT SDK — AgentMesh (amitayks/agentmesh)
// Full E2E encrypted inter-agent communication via self-hosted relay/registry.
// Also: tool-level policy, trust scoring, audit logging.
// Infrastructure controls (NetworkPolicy, token budgets) stay in Rust router.
// ---------------------------------------------------------------------------

let agtPolicy: any = null;
let agtTrustStore: any = null;
let agtAuditLogger: any = null;
let agtMeshClient: any = null;
let agtIdentity: any = null;
let agtInitialized = false; // Module-level guard (supplemented by process-level guard below)

// AGT message buffer — filled by onMessage handler, drained by mesh_inbox tool
const agtInbox: Array<{ from_amid: string; from_agent: string; content: any; timestamp: string; id: string; message_type?: string }> = [];

// AGT reconnect & heartbeat state
let agtReconnectTimer: ReturnType<typeof setInterval> | null = null;
let agtInboxNotifyTimer: ReturnType<typeof setInterval> | null = null;
let agtConnected = false;
let agtReconnectFailures = 0;
const AGT_RECONNECT_MAX_BACKOFF = 300_000; // 5 min cap

// Offload request IDs currently being processed (either env-driven proactive
// start or inbound offload_task). Prevents double-execution if the external
// agent sends offload_task while the sandbox is already running the env task.
const offloadInFlight = new Set<string>();

// Chunked mesh transport extracted to core/mesh-transport.ts in S15.f.3.

// ── Handoff progress tracker (module-level, survives across tool calls) ──
// Handoff progress holder. Wrapped in a value object so that
// `core/agt-tools/agt.ts` can mutate it through a shared reference rather
// than via a setter callback.
const handoffState: { current: import("./core/agt-handoff.js").HandoffProgress | null } = { current: null };

// Handoff interrupt flag — set by handoff:interrupt message, checked by task loops
let handoffInterruptRequested = false;
let handoffInterruptReason = "";

// redactSecrets — extracted to core/log-redact.ts in S15.f.1; re-exported below.

// Module-level logger — set once during register(), used by background orchestration
let _log: { info: (m: string) => void; warn: (m: string) => void } = {
  info: (m: string) => console.log(`[azureclaw] ${redactSecrets(m)}`),
  warn: (m: string) => console.warn(`[azureclaw] ${redactSecrets(m)}`),
};

// AMID cache state + helpers — extracted to core/amid-cache.ts in S15.f.1.

// pickFreshestRegistryMatch + resolveAmidByName — extracted to core/amid-cache.ts in S15.f.1.

// parentTrustedAmids + peerSigningKeys — extracted to core/amid-cache.ts in S15.f.1.

// sanitizeLog — extracted to core/log-redact.ts in S15.f.1.

// resolveAmidToName — extracted to core/amid-cache.ts in S15.f.1.


// resolveSigningKey — extracted to core/amid-cache.ts in S15.f.1.

let agtSandboxName: string = "unknown";

// Push trust updates to the router's local TrustStore + Ed25519 signing
// counters live in cli/src/core/router-client.ts (imported above).

// Record a completed mesh session in the AGT registry so reputation/session counters update.
// Calls POST /registry/reputation/session through the router's registry proxy.
async function recordMeshSession(
  targetAmid: string,
  sessionId: string,
  intent: string,
  outcome: "success" | "failed" | "timeout",
  startedAt: string,
) {
  return _recordMeshSession(agtIdentity, agtMeshClient, targetAmid, sessionId, intent, outcome, startedAt);
}

// Attempt to reconnect the AGT mesh client after a disconnect.
async function agtReconnect(log: { info: (m: string) => void; warn: (m: string) => void }) {
  return _agtReconnect(agtMeshClient, agtConnected, agtSandboxName, (v) => { agtConnected = v; }, log);
}

// Write unread inbox messages to a file the LLM can see in its context.
// This is the key mechanism to keep conversations "lively" — the agent sees
// pending messages in MEMORY.md without needing to manually call mesh_inbox.
async function notifyInboxToMemory(log: { info: (m: string) => void; warn: (m: string) => void }) {
  return _notifyInboxToMemory(agtInbox, log);
}
import { discoverFoundryProject, type FoundryProjectInfo } from "./core/foundry-discovery.js";
import { delegateToNativeAgent } from "./core/agt-task-delegate.js";
import { meshSendWithIdentity, meshHandleTransportMessage, pendingTransfers, MESH_CHUNK_THRESHOLD, MESH_CHUNK_SIZE, MESH_MAX_CHUNKS, MESH_TRANSFER_TTL, type PendingMeshTransfer } from "./core/mesh-transport.js";
import { TASK_TOOLS } from "./core/agt-task-tools.js";
import { recordMeshSession as _recordMeshSession, agtReconnect as _agtReconnect, notifyInboxToMemory as _notifyInboxToMemory } from "./core/agt-heartbeat.js";
import { runOffloadTask as _runOffloadTask, startProactiveOffloadIfNeeded as _startProactiveOffloadIfNeeded } from "./core/agt-offload.js";
import { processTaskWithTools as _processTaskWithTools } from "./core/agt-task-loop.js";
import { runHandoffOrchestration as _runHandoffOrchestrationCore } from "./core/agt-handoff.js";
import { registerHttpFetchTool } from "./core/agt-tools/http-fetch.js";
import { registerFoundryTools } from "./core/agt-tools/foundry.js";
import { registerAgtTools } from "./core/agt-tools/agt.js";
import { registerOpenClawCommands } from "./core/commands/openclaw.js";
let foundryProject: FoundryProjectInfo | null = null;
let foundryInitialized = false;

// delegateToNativeAgent — extracted to core/agt-task-delegate.ts in S15.f.2.

/**
 * Fallback: process a task_request with a limited tool-calling loop.
 * Used when native delegation fails (e.g., Gateway not running).
 * Runs an LLM loop with 6 tools, max 25 rounds, 2048 max_completion_tokens.
 */
async function processTaskWithTools(
  taskContent: any,
  log: { info: (m: string) => void; warn: (m: string) => void },
): Promise<string> {
  return _processTaskWithTools(taskContent, {
    meshClient: () => agtMeshClient,
    isInterruptRequested: () => handoffInterruptRequested,
    interruptReason: () => handoffInterruptReason,
    setInterrupt: (req, reason) => {
      handoffInterruptRequested = req;
      handoffInterruptReason = reason;
    },
  }, log);
}

// ── meshSend: auto-chunking send wrapper ─────────────────────────────────────
// Transparently chunks large messages. Small messages pass through directly.
// Returns a transfer_id if chunked (for tracking), or undefined for direct send.
async function meshSend(
  client: { send: (amid: string, msg: unknown) => Promise<void> },
  targetAmid: string,
  message: Record<string, unknown>,
  _log?: { info: (m: string) => void; warn: (m: string) => void },
): Promise<string | undefined> {
  return meshSendWithIdentity(client, targetAmid, message, agtIdentity, _log);
}

// meshHandleTransportMessage extracted to core/mesh-transport.ts in S15.f.3.

// ── Offload task executor (shared between env-driven and message-driven flows) ──
// Executes a task either via the native agent (delegateToNativeAgent) or the
// tool-based fallback (processTaskWithTools), streams progress updates to the
// parent agent, collects output files, and sends offload_done/offload_error.
// runOffloadTask + startProactiveOffloadIfNeeded extracted to core/agt-offload.ts in S15.f.5.
function _offloadDeps() {
  return {
    meshClient: agtMeshClient,
    identity: agtIdentity,
    sandboxName: agtSandboxName,
    isConnected: () => agtConnected,
    offloadInFlight,
    meshSend,
    processTaskWithTools,
  };
}
async function runOffloadTask(
  opts: {
    requestId: string;
    parentAmid: string;
    parentName: string;
    task: string;
    files: any[];
    source: "env" | "message";
  },
  log: { info: (m: string) => void; warn: (m: string) => void },
): Promise<void> {
  return _runOffloadTask(opts, _offloadDeps(), log);
}
async function startProactiveOffloadIfNeeded(
  log: { info: (m: string) => void; warn: (m: string) => void },
): Promise<void> {
  return _startProactiveOffloadIfNeeded(_offloadDeps(), log);
}

async function initAGT(log: { info: (m: string) => void; warn: (m: string) => void }) {
  // Node hosts don't participate in the mesh — skip entirely.
  if (process.env.AGT_SKIP_INIT === "1") return;

  // Process-level singleton — the gateway loads this plugin in 5 parallel contexts.
  // Use a synchronous lock (set BEFORE any async work) to prevent race conditions.
  const AGT_CLIENT_KEY = Symbol.for("agt-mesh-client");
  const AGT_IDENTITY_KEY = Symbol.for("agt-identity");
  const AGT_LOCK_KEY = Symbol.for("agt-init-lock");
  const AGT_PROMISE_KEY = Symbol.for("agt-init-promise");

  // Fast path: already initialized in another context
  const existingClient = (process as any)[AGT_CLIENT_KEY];
  if (existingClient) {
    agtMeshClient = existingClient;
    agtIdentity = (process as any)[AGT_IDENTITY_KEY];
    agtInitialized = true;
    return;
  }

  // Synchronous lock — first caller wins, others wait for the promise
  if ((process as any)[AGT_LOCK_KEY]) {
    // Another context is initializing — wait for it to finish
    const pending = (process as any)[AGT_PROMISE_KEY];
    if (pending) await pending;
    const client = (process as any)[AGT_CLIENT_KEY];
    if (client) {
      agtMeshClient = client;
      agtIdentity = (process as any)[AGT_IDENTITY_KEY];
      agtInitialized = true;
    }
    return;
  }
  (process as any)[AGT_LOCK_KEY] = true; // Synchronous — prevents all other callers

  // Module-level fallback guard (for hot-restart where process persists)
  if (agtInitialized && agtMeshClient) return;

  // Create and store the init promise BEFORE any async work so other contexts
  // that check AGT_LOCK_KEY can always await it (fixes race where pending was undefined).
  const initPromise = (async () => {
  try {
    // ESM import preferred; fall back to CJS require if extension loader context rejects it
    let sdk: any;
    try {
      sdk = await import("@agentmesh/sdk");
    } catch {
      // ESM import failed — load CJS entry via createRequire
      const { createRequire } = await import("node:module");
      const _require = createRequire(import.meta.url);
      sdk = _require("@agentmesh/sdk");
    }

    // Policy engine — tool allow/deny evaluation
    agtPolicy = new sdk.Policy([
      { action: "web_search", effect: "allow" },
      { action: "file_read", effect: "allow" },
      { action: "file_write", effect: "allow" },
      { action: "shell:ls", effect: "allow" },
      { action: "shell:cat", effect: "allow" },
      { action: "shell:python", effect: "allow" },
      { action: "shell:git", effect: "allow" },
      { action: "shell:curl", effect: "allow" },
      { action: "shell:rm -rf /", effect: "deny" },
      { action: "shell:chmod 777", effect: "deny" },
      { action: "shell:dd", effect: "deny" },
      { action: "shell:mkfs", effect: "deny" },
    ]);

    // Trust store — 0-1000 scoring with tiers
    agtTrustStore = sdk.createTrustStore();
    // Audit logger — hash-chain append-only log
    agtAuditLogger = sdk.createAuditLogger();

    // Generate cryptographic identity (Ed25519 + X25519)
    agtIdentity = await sdk.Identity.generate();
    log.info(`AGT identity: ${agtIdentity.amid}`);

    // Create AgentMeshClient — ALWAYS connect through the router proxy.
    // The plugin (UID 1000) cannot reach external services directly (iptables blocks).
    // The router (UID 1001) proxies: /agt/relay → relay service, /agt/registry/* → registry service.
    // On AKS, router reads AGT_RELAY_URL/AGT_REGISTRY_URL to find the services.
    // In dev, same env vars point to Docker containers on the shared network.
    const registryUrl = routerUrl("/agt/registry");
    const relayUrl = routerWsUrl("/agt/relay");

    agtMeshClient = new sdk.AgentMeshClient(agtIdentity, {
      storage: new sdk.MemoryStorage(),
      registryUrl,
      relayUrl,
    });

    // ── Register ALL handlers BEFORE connect() ──────────────────────────
    // Messages can arrive immediately after connect() returns, so handlers
    // must be in place first.

    // KNOCK handler — policy-gated session establishment with trust scoring.
    const AGT_TRUST_THRESHOLD = parseInt(process.env.AGT_TRUST_THRESHOLD || "0", 10); // 0 = accept all (dev)
    if (AGT_TRUST_THRESHOLD > 0) {
      agtMeshClient.enableKnockEnforcement();
      log.info(`AGT KNOCK enforcement enabled (threshold: ${AGT_TRUST_THRESHOLD})`);
    }
    agtMeshClient.onKnock(async (fromAmid: string, request: any) => {
      const intent = request?.intent?.capability || '*';
      const fromName = await resolveAmidToName(fromAmid) || fromAmid.slice(0, 12);
      log.info(`AGT KNOCK from ${fromName} (${fromAmid.slice(0, 12)}...) intent=${intent}`);

      // Trust score evaluation (when threshold > 0)
      if (AGT_TRUST_THRESHOLD > 0) {
        try {
          const peerInfo = await agtMeshClient.lookup(fromAmid);
          // Registry returns 0.0-1.0 scale; normalize to 0-1000 for threshold comparison
          const rawScore = peerInfo?.reputationScore ?? 0;
          const normalizedScore = Math.round(rawScore * 1000);
          // Spawner affinity: +200 bonus for agents this agent spawned directly
          const isSpawnedChild = amidToName.has(fromAmid) && !parentTrustedAmids.has(fromAmid);
          // Parent-verified trust: +500 for peers pre-seeded by our parent at spawn time.
          // These AMIDs came via AGT_TRUSTED_PEERS env var (set by router, not self-reported).
          const isParentTrusted = parentTrustedAmids.has(fromAmid);
          const affinityBonus = isParentTrusted ? 500 : (isSpawnedChild ? 200 : 0);
          const affinityLabel = isParentTrusted ? "parent-verified" : (isSpawnedChild ? "spawner" : "");
          const effectiveScore = normalizedScore + affinityBonus;
          if (effectiveScore < AGT_TRUST_THRESHOLD) {
            log.warn(`AGT KNOCK rejected: ${fromName} score=${effectiveScore} (registry=${normalizedScore}${affinityBonus > 0 ? ` +${affinityBonus} ${affinityLabel}` : ''}) < threshold=${AGT_TRUST_THRESHOLD}`);
            return { accept: false, reason: `trust_score_${effectiveScore}_below_${AGT_TRUST_THRESHOLD}` };
          }
          log.info(`AGT KNOCK trust OK: ${fromName} score=${effectiveScore} (registry=${normalizedScore}${affinityBonus > 0 ? ` +${affinityBonus} ${affinityLabel}` : ''})`);
        } catch {
          // Registry lookup failed — accept anyway for mesh agents (trust evaluation best-effort)
          log.warn(`AGT KNOCK trust lookup failed for ${fromName} — accepting (best-effort)`);
        }
      }

      // Policy evaluation
      if (agtPolicy && intent !== '*') {
        const decision = agtPolicy.evaluate({ action: intent });
        if (decision && !decision.allowed) {
          log.warn(`AGT KNOCK rejected by policy: ${fromAmid.slice(0, 12)} intent=${intent}`);
          return { accept: false, reason: 'policy_denied' };
        }
      }

      // KNOCK accepted — bootstrap trust for this peer.
      // A completed X3DH handshake proves cryptographic identity, warranting
      // baseline trust (score=500 = threshold). Subsequent interactions adjust.
      // Store by resolved display_name so operator panel shows human-readable names.
      // Awaited to ensure trust is stored before the first message arrives.
      await pushTrustToRouter(fromName, 0.0); // 500 + 0*500 = 500 (at threshold)
      log.info(`AGT KNOCK accepted: bootstrapped trust for ${fromName} / ${fromAmid.slice(0, 12)}... (score=500)`);

      return { accept: true };
    });

    // Handle E2E decryption failures and KNOCK rejections — log and surface to operator
    agtMeshClient.onError((type: string, fromAmid: string, detail: string) => {
      const fromName = amidToName.get(fromAmid) || fromAmid.slice(0, 12);
      if (type === 'knock_rejected') {
        log.warn(`⛔ Message blocked from '${fromName}': KNOCK not accepted — ${detail}`);
        agtInbox.push({
          from_amid: fromAmid,
          from_agent: fromName,
          content: `⛔ MESSAGE BLOCKED: ${fromName} attempted to send a message but has no accepted KNOCK session. The message was rejected and not delivered.`,
          message_type: "security_event",
          timestamp: new Date().toISOString(),
          id: `agt-knock-${Date.now().toString(36)}`,
        });
      } else {
        log.warn(`AGT E2E ${type} from '${fromName}' (${fromAmid.slice(0, 12)}): ${detail}`);
        pushTrustToRouter(fromName, -0.5);
        agtInbox.push({
          from_amid: fromAmid,
          from_agent: fromName,
          content: `⚠️ E2E DECRYPTION FAILURE: ${type} — ${detail}. Message was REJECTED (not delivered). This may indicate a session mismatch or tampering.`,
          message_type: "security_event",
          timestamp: new Date().toISOString(),
          id: `agt-err-${Date.now().toString(36)}`,
        });
      }
    });

    // Log when E2E encrypted channel is verified with a peer
    agtMeshClient.onE2EVerified((peerAmid: string, isFirstPeer: boolean) => {
      const peerName = amidToName.get(peerAmid) || peerAmid.slice(0, 12);
      if (isFirstPeer) {
        log.info(`✅ E2E encrypted channel UP — first verified peer: '${peerName}' (X3DH + Double Ratchet)`);
      } else {
        log.info(`✅ E2E encrypted channel verified with '${peerName}'`);
      }
    });

    // Set up message handler — stores received messages in the AGT inbox buffer
    // AND auto-replies to task_request messages via AGT relay (E2E encrypted reply)
    agtMeshClient.onMessage(async (fromAmid: string, message: any) => {
      // Resolve sender name — check local cache first, then look up via registry
      let fromName = amidToName.get(fromAmid) || "";
      if (!fromName && message?.from_agent) {
        fromName = message.from_agent;
        amidToName.set(fromAmid, fromName);
        nameToAmid.set(fromName, fromAmid);
      }
      if (!fromName) {
        fromName = await resolveAmidToName(fromAmid);
      }
      if (!fromName) fromName = fromAmid.slice(0, 12);

      // ── Transport layer: intercept chunked transfer messages ──
      // mesh:transfer_manifest and mesh:transfer_chunk are transport-level —
      // they get accumulated and reassembled before reaching application logic.
      const transportResult = await meshHandleTransportMessage(fromAmid, fromName, message, log);
      if (transportResult === null) {
        // Transport message absorbed (manifest or partial chunk) — don't process further
        return;
      }
      if (transportResult !== undefined) {
        // Reassembled message — replace the original message and continue to app layer
        message = transportResult;
        log.info(`Mesh transfer reassembled from '${fromName}' — processing as ${message.type || "message"}`);
      }

      // ── Ed25519 signature verification (AGT Identity) ──
      // Verify per-message signatures using the sender's public key from registry.
      // Fail-open: unsigned or unverifiable messages are still delivered but logged.
      if (message && typeof message === "object" && message.__signature) {
        const sigB64: string = message.__signature;
        const senderAmid: string = message.__sender_amid || fromAmid;
        const signedAt: number = message.__signed_at || 0;
        // Strip signing metadata before verification (verify the original payload)
        const { __signature: _s, __sender_amid: _a, __signed_at: _t, __signed: _sg, ...originalPayload } = message;
        const payloadStr = JSON.stringify(originalPayload);

        // Timestamp window check (60s) — replay protection beyond Signal layer
        const now = Math.floor(Date.now() / 1000);
        if (signedAt > 0 && Math.abs(now - signedAt) > 60) {
          log.warn(`Ed25519 signature expired from '${fromName}' (${Math.abs(now - signedAt)}s drift) — message accepted but flagged`);
          pushSigningCounter("rejected");
          pushTrustToRouter(fromName, -0.3);
        } else {
          const sdk = await import("@agentmesh/sdk");
          const pubKey = await resolveSigningKey(senderAmid);
          if (pubKey) {
            try {
              const encoder = new TextEncoder();
              const valid = await sdk.Identity.verifySignature(pubKey, encoder.encode(payloadStr), sigB64);
              if (valid) {
                pushSigningCounter("verified");
              } else {
                log.warn(`Ed25519 signature INVALID from '${fromName}' — message accepted but trust penalized`);
                pushSigningCounter("rejected");
                pushTrustToRouter(fromName, -0.5);
                agtInbox.push({
                  from_amid: fromAmid,
                  from_agent: fromName,
                  content: `⚠️ SIGNATURE INVALID: Message from '${fromName}' has invalid Ed25519 signature. Possible tampering.`,
                  message_type: "security_event",
                  timestamp: new Date().toISOString(),
                  id: `agt-sig-${Date.now().toString(36)}`,
                });
              }
            } catch {
              // Verification error — log but don't block
              log.warn(`Ed25519 verify error for '${fromName}' — message accepted`);
            }
          }
        }
        // Use original payload (without signing metadata) for application processing
        message = originalPayload;
      }

      const content = typeof message === "string" ? message : (message?.content || message?.text || JSON.stringify(message));
      const entry = {
        from_amid: fromAmid,
        from_agent: fromName,
        content,
        message_type: message?.type || "message",
        timestamp: new Date().toISOString(),
        id: `agt-${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}`,
      };
      agtInbox.push(entry);
      log.info(`AGT relay message from ${sanitizeLog(fromName, 50)} (${fromAmid.slice(0, 12)}...): ${sanitizeLog(JSON.stringify(content), 200)}`);

      // ── mesh:ping — lightweight reachability probe used by mesh-plugin
      // before sending files/tasks. Reply immediately with mesh:pong echoing
      // the nonce. No policy gate — this is transport-level only.
      if (message?.type === "mesh:ping" && fromAmid && agtMeshClient) {
        try {
          await agtMeshClient.send(fromAmid, {
            type: "mesh:pong",
            nonce: message.nonce,
            from_agent: agtSandboxName || process.env.SANDBOX_NAME || "sandbox",
            timestamp: new Date().toISOString(),
          });
          log.info(`🏓 Replied to mesh:ping from '${fromName}' (nonce: ${String(message.nonce || "").slice(0, 8)})`);
        } catch (pingErr: any) {
          log.warn(`Failed to reply to mesh:ping from '${fromName}': ${pingErr.message}`);
        }
        return;
      }

      // AGT policy gate — validate incoming mesh message via router PolicyEngine.
      // Checks trust score of sender against mesh-receive-untrusted rule.
      // Non-blocking: on error or timeout, fail-open (log and continue).
      // This runs AFTER E2E decryption (handled by SDK) — encryption is not affected.
      if (message?.type === "task_request") {
        try {
          const http = await import("node:http");
          // Look up sender's trust score via router (which forwards with admin token)
          let senderTrustScore = 0;
          try {
            const trustResult = await _routerCall("GET",
              `/agt/trust/${encodeURIComponent(fromName)}`);
            senderTrustScore = trustResult?.score ?? 0;
          } catch { /* trust lookup failed — use 0 */ }

          // Tier verification gate — when REQUIRE_VERIFIED_TIER=true, reject
          // messages from anonymous-tier agents (not verified via OAuth/Entra).
          if (process.env.REQUIRE_VERIFIED_TIER === "true") {
            try {
              const lookupResult = await _routerCall("GET",
                `/agt/registry/v1/registry/lookup?amid=${encodeURIComponent(fromAmid)}`);
              const senderTier = lookupResult?.tier || "anonymous";
              if (senderTier === "anonymous") {
                log.warn(`AGT tier gate DENIED: '${fromName}' is anonymous (require_verified_tier=true)`);
                if (agtMeshClient) {
                  try {
                    await agtMeshClient.send(fromAmid, {
                      type: "task_response",
                      content: "Request denied: this agent requires verified identity tier (OAuth/Entra). Register with a verification token.",
                      from_agent: agtSandboxName,
                      timestamp: new Date().toISOString(),
                    });
                  } catch { /* best effort */ }
                }
                return;
              }
            } catch { /* tier lookup failed — fail-open */ }
          }

          // Evaluate mesh:receive action with sender trust context
          const evalPayload = JSON.stringify({
            action: "mesh:receive",
            agent_id: fromAmid,
            context: { trust_score: senderTrustScore, from_agent: fromName },
          });
          const evalResult = await new Promise<string>((resolve, reject) => {
            const req = http.request(routerUrl("/agt/evaluate"), {
              method: "POST",
              headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(evalPayload) },
            }, (res) => {
              let d = ""; res.on("data", (c: Buffer) => { d += c.toString(); }); res.on("end", () => resolve(d));
            });
            req.on("error", reject);
            req.setTimeout(2000, () => { req.destroy(); reject(new Error("timeout")); });
            req.write(evalPayload);
            req.end();
          });
          const evalData = JSON.parse(evalResult);
          if (evalData.decision === "deny") {
            log.warn(`AGT policy DENIED mesh:receive from ${fromName} (trust=${senderTrustScore}): ${evalData.reason}`);
            // Send rejection back via E2E encrypted relay
            if (agtMeshClient) {
              try {
                await agtMeshClient.send(fromAmid, {
                  type: "task_response",
                  content: `Request denied by governance policy: ${evalData.reason}`,
                  from_agent: agtSandboxName,
                  timestamp: new Date().toISOString(),
                });
              } catch { /* best effort */ }
            }
            return; // Skip task processing — message was logged to inbox but not executed
          }
          log.info(`AGT policy allowed mesh:receive from ${fromName} (trust=${senderTrustScore})`);
        } catch (policyErr: any) {
          // Fail-open: router unreachable or error — log and continue processing
          log.warn(`AGT mesh policy check failed (proceeding): ${policyErr.message}`);
        }
      }

      // Process task_request messages via the native OpenClaw agent loop.
      // AGT is the sole policy authority (no mixing with OpenClaw tools.deny):
      //   1. mesh:receive trust gate (above) — rejects untrusted senders
      //   2. task:execute policy check (below) — AGT gates the entire task
      //   3. Container sandbox is the enforcement boundary (seccomp, netpol, cgroups)
      //   4. Plugin tools inside native agent still check AGT per-call (http_fetch, foundry_*)
      //   5. AGT audit + reputation recorded after completion
      // Falls back to processTaskWithTools (per-tool AGT gating) if native agent fails.
      if (message?.type === "task_request" && fromAmid && agtMeshClient) {
        const taskContent = message?.content || content;

        // AGT policy: evaluate task:execute before dispatching to native agent
        let taskAllowed = true;
        try {
          const http = await import("node:http");
          const evalPayload = JSON.stringify({
            action: "task:execute",
            context: { from_agent: fromName, task_preview: String(taskContent).slice(0, 500) },
          });
          const evalResult = await new Promise<string>((resolve, reject) => {
            const req = http.request(routerUrl("/agt/evaluate"), {
              method: "POST",
              headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(evalPayload) },
            }, (res) => {
              let d = ""; res.on("data", (c: Buffer) => { d += c.toString(); }); res.on("end", () => resolve(d));
            });
            req.on("error", reject);
            req.setTimeout(2000, () => { req.destroy(); reject(new Error("timeout")); });
            req.write(evalPayload);
            req.end();
          });
          const evalData = JSON.parse(evalResult);
          if (evalData.decision === "deny") {
            log.warn(`AGT policy DENIED task:execute from ${fromName}: ${evalData.reason}`);
            taskAllowed = false;
            try {
              await agtMeshClient.send(fromAmid, {
                type: "task_response",
                content: `Task denied by AGT governance: ${evalData.reason}`,
                from_agent: agtSandboxName,
                timestamp: new Date().toISOString(),
              });
            } catch { /* best effort */ }
          }
        } catch { /* router unavailable — allow (fail-open) */ }

        if (!taskAllowed) return;

        try {
          // In-process tool-calling loop only. See offload path above for why
          // we deliberately skip `delegateToNativeAgent` here.
          const llmResponse: string = await processTaskWithTools(taskContent, log);

          // Send the response back via E2E encrypted relay
          await agtMeshClient.send(fromAmid, {
            type: "task_response",
            content: llmResponse,
            from_agent: agtSandboxName,
            in_reply_to: taskContent,
            timestamp: new Date().toISOString(),
          });
          log.info(`AGT relay: reply sent to ${fromName} via E2E encrypted relay`);
          // Sub-agent rates parent — this bumps the parent's feedback_count.
          // The sub-agent is still alive and registered here (just sent a relay
          // message above), so the registry should accept the review.
          try {
            const sessionId = crypto.randomUUID();
            const ok = await agtMeshClient.submitReputation(fromAmid, sessionId, 0.8, ["reliable"]);
            if (!ok) log.warn(`AGT reputation: registry rejected review for ${fromName} (from_amid=${agtIdentity?.amid})`);
            pushTrustToRouter(fromName, 0.8);
            await recordMeshSession(fromAmid, sessionId, "task_request", "success", new Date().toISOString());
            log.info(`AGT reputation: submitted +0.8 for ${fromName} (accepted=${ok})`);
          } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
        } catch (replyErr: any) {
          // Fallback: send error message back so parent knows what happened
          try {
            await agtMeshClient.send(fromAmid, {
              type: "task_response",
              content: `Error processing task: ${replyErr.message}`,
              from_agent: agtSandboxName,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
          log.warn(`AGT relay: task processing failed: ${replyErr.message}`);
          // Submit negative reputation on failure
          try {
            const sessionId = crypto.randomUUID();
            const ok = await agtMeshClient.submitReputation(fromAmid, sessionId, 0.3, ["unreliable"]);
            if (!ok) log.warn(`AGT reputation: registry rejected negative review for ${fromName}`);
            pushTrustToRouter(fromName, 0.3);
            await recordMeshSession(fromAmid, sessionId, "task_request", "failed", new Date().toISOString());
          } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
        }
      }

      // ── Handle file_transfer messages — auto-save received files to workspace ──
      if (message?.type === "file_transfer" && message?.file_data && message?.file_name) {
        let success = false;
        let savedPath = "";
        let errorMsg = "";
        try {
          const fs = await import("node:fs");
          const path = await import("node:path");
          const incomingDir = "/sandbox/.openclaw/workspace/incoming";
          fs.mkdirSync(incomingDir, { recursive: true });

          const safeName = String(message.file_name).replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 255);
          const destPath = path.join(incomingDir, safeName);
          const buf = Buffer.from(message.file_data, "base64");
          fs.writeFileSync(destPath, buf, { mode: 0o600 });

          // Verify the write
          const stat = fs.statSync(destPath);
          success = stat.size === buf.length;
          savedPath = destPath;

          log.info(
            `📁 File received from '${fromName}': ${safeName} ` +
            `(${(buf.length / 1024).toFixed(1)} KB) → ${destPath}`
          );

          // Update the inbox entry with save path (already pushed above)
          const lastEntry = agtInbox[agtInbox.length - 1];
          if (lastEntry && lastEntry.from_amid === fromAmid) {
            lastEntry.content = JSON.stringify({
              type: "file_transfer",
              file_name: safeName,
              saved_to: destPath,
              size_bytes: buf.length,
              description: message.description || "",
              from_agent: fromName,
            });
          }
        } catch (ftErr: any) {
          errorMsg = ftErr.message;
          log.warn(`File transfer save failed: ${ftErr.message}`);
        }

        // Send ack back to sender so they know the file landed (or didn't)
        if (fromAmid && agtMeshClient) {
          try {
            await agtMeshClient.send(fromAmid, {
              type: "file_transfer_ack",
              from_agent: process.env.SANDBOX_NAME || "unknown",
              file_name: String(message.file_name || ""),
              success,
              saved_to: savedPath,
              error: errorMsg || undefined,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort ack */ }
        }
        return; // Don't process as a task
      }

      // ── Handle offload_task — external agent sends task directly to sandbox ──
      // This is the direct agent-to-agent offload flow. The external agent paired
      // with the cluster, got this sandbox spawned by the controller, then sends
      // the task directly via mesh. We execute it and send offload_done back.
      //
      // NOTE: if the sandbox was started with OFFLOAD_REQUEST_ID env (the new
      // proactive path), it has already ack'd with `offload_hello` and started
      // working. In that case we dedupe here and only ack again so the parent
      // knows the task landed — no re-execution.
      if (message?.type === "offload_task" && fromAmid && agtMeshClient) {
        const offloadRequestId = message.request_id || crypto.randomUUID();
        const taskContent = message.task || message.content || "";
        const offloadFiles = message.files || [];

        // Acknowledge receipt IMMEDIATELY so the requester knows the task
        // landed and is queued for execution. Without this, the requester
        // has to speculatively wait for offload_progress/offload_done and
        // cannot distinguish "sandbox offline" from "task running".
        try {
          await agtMeshClient.send(fromAmid, {
            type: "task_received",
            request_id: offloadRequestId,
            from_agent: agtSandboxName || process.env.SANDBOX_NAME || "sandbox",
            accepted_at: new Date().toISOString(),
          });
        } catch (ackErr: any) {
          log.warn(`Failed to send task_received ack for ${offloadRequestId.slice(0, 8)}: ${ackErr.message}`);
        }

        // Dedupe — if the env-driven proactive path already picked this up,
        // don't execute it again. The ack above is still useful.
        if (offloadInFlight.has(offloadRequestId)) {
          log.info(
            `☁️ Offload ${offloadRequestId.slice(0, 8)} already in-flight ` +
            `(started via env) — ack'd, skipping duplicate execution`
          );
          return;
        }

        offloadInFlight.add(offloadRequestId);
        try {
          await runOffloadTask(
            {
              requestId: offloadRequestId,
              parentAmid: fromAmid,
              parentName: fromName,
              task: taskContent,
              files: offloadFiles,
              source: "message",
            },
            log,
          );
        } finally {
          offloadInFlight.delete(offloadRequestId);
        }

        return; // Don't process as a regular task
      }

      // ── Handle handoff:interrupt — parent signals sub-agent to save progress ──
      // Sent before workspace_request so the sub-agent can checkpoint its work.
      // Sets the interrupt flag which processTaskWithTools checks between rounds.
      if (message?.type === "handoff:interrupt" && fromAmid) {
        log.info(`🛑 Handoff interrupt received from '${fromName}' — signaling task to save progress`);
        handoffInterruptRequested = true;
        handoffInterruptReason = message.reason || "parent_handoff";
        // Acknowledge immediately — the task loop will save on next iteration
        if (agtMeshClient) {
          try {
            await agtMeshClient.send(fromAmid, {
              type: "handoff:interrupt_ack",
              from_agent: process.env.SANDBOX_NAME || "unknown",
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
        }
        return; // Don't process as a task
      }

      // ── Handle handoff:workspace_request — parent collects sub-agent workspace ──
      // During handoff, the parent asks each sub-agent to serialize its workspace
      // and send it back via mesh. Uses meshSend for transparent auto-chunking.
      if (message?.type === "handoff:workspace_request" && fromAmid && agtMeshClient) {
        log.info(`📦 Workspace collection request from '${fromName}' — packaging workspace...`);
        const agentName = process.env.SANDBOX_NAME || "unknown";
        try {
          const { execSync } = await import("node:child_process");
          const tarB64 = execSync(
            "tar czf - -C /sandbox " +
            "--exclude='.openclaw/extensions/*/dist' --exclude='.openclaw/extensions/*/node_modules' " +
            "--exclude='node_modules' --exclude='.git' " +
            "--exclude='*.pyc' --exclude='__pycache__' " +
            ".openclaw/workspace .openclaw/openclaw.json .openclaw/cron " +
            ".openclaw/policies .openclaw/agents 2>/dev/null | base64 -w0",
            { timeout: 10000, maxBuffer: 50 * 1024 * 1024 },
          ).toString().trim();

          // meshSend auto-chunks if > 512KB — transparent to the receiver
          await meshSend(agtMeshClient, fromAmid, {
            type: "handoff:workspace_response",
            name: agentName,
            workspace_tar: tarB64,
            size_bytes: tarB64.length,
            from_agent: agentName,
            timestamp: new Date().toISOString(),
          }, log);
          log.info(`📦 Workspace sent to '${fromName}' (${(tarB64.length / 1024).toFixed(1)} KB)`);
        } catch (wsErr: any) {
          log.warn(`Workspace collection failed: ${wsErr.message}`);
          try {
            await agtMeshClient.send(fromAmid, {
              type: "handoff:workspace_response",
              name: agentName,
              workspace_tar: "",
              size_bytes: 0,
              error: wsErr.message,
              from_agent: agentName,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
        }
        return; // Don't process as a regular task
      }

      // ── Handle handoff:workspace_inject — parent pushes workspace after re-spawn ──
      // After handoff restore, the parent injects each sub-agent's workspace via mesh.
      if (message?.type === "handoff:workspace_inject" && message?.workspace_tar) {
        log.info(`📦 Workspace injection from '${fromName}' — extracting...`);
        let success = false;
        let fileCount = 0;
        let errorMsg = "";
        try {
          const fs = await import("node:fs");
          const { execSync } = await import("node:child_process");
          const tarBuf = Buffer.from(message.workspace_tar as string, "base64");

          const MAX_TAR_BYTES = 5 * 1024 * 1024;
          if (tarBuf.length > MAX_TAR_BYTES) throw new Error(`workspace tar too large: ${tarBuf.length}`);

          const tmpDir = `/tmp/handoff-inject-${Date.now()}`;
          fs.mkdirSync(tmpDir, { recursive: true });
          const tarPath = `${tmpDir}/workspace.tar.gz`;
          fs.writeFileSync(tarPath, tarBuf, { mode: 0o600 });

          // Validate entries
          const listing = execSync(`tar tzf ${tarPath} 2>/dev/null`, { timeout: 5000 }).toString();
          const entries = listing.split("\n").filter(Boolean);
          for (const entry of entries) {
            if (entry.includes("..") || entry.startsWith("/")) {
              throw new Error(`path traversal blocked: ${entry}`);
            }
          }
          fileCount = entries.length;

          execSync(
            `tar xzf ${tarPath} -C /sandbox/ --no-same-owner --no-overwrite-dir 2>/dev/null`,
            { timeout: 10000 },
          );
          fs.rmSync(tmpDir, { recursive: true, force: true });

          // Write a manifest so the agent knows what files were restored and where.
          // Filter to user-facing files (skip .openclaw internals, skills, etc.)
          const userFiles = entries.filter((e: string) =>
            !e.endsWith("/") &&
            !e.includes("/skills/") &&
            !e.includes("workspace-state.json") &&
            !e.includes("SOUL.md") &&
            !e.includes("USER.md"),
          );
          if (userFiles.length > 0) {
            const manifestLines = [
              "# Handoff — Restored Files",
              "",
              `Restored ${userFiles.length} workspace file(s) from the previous environment:`,
              "",
              ...userFiles.map((f: string) => `- /sandbox/${f}`),
              "",
              `Total files (including system): ${entries.length}`,
            ];
            fs.writeFileSync(
              "/sandbox/.openclaw/workspace/HANDOFF_FILES.md",
              manifestLines.join("\n") + "\n",
            );
          }

          // Promote files from incoming/ to workspace root so they're immediately
          // visible to the agent without needing to know about the incoming/ directory.
          const incomingDir = "/sandbox/.openclaw/workspace/incoming";
          const wsRoot = "/sandbox/.openclaw/workspace";
          if (fs.existsSync(incomingDir)) {
            try {
              const incomingFiles = fs.readdirSync(incomingDir);
              for (const file of incomingFiles) {
                const src = `${incomingDir}/${file}`;
                const dest = `${wsRoot}/${file}`;
                if (!fs.existsSync(dest) && fs.statSync(src).isFile()) {
                  fs.copyFileSync(src, dest);
                  log.info(`📂 Promoted incoming/${file} → workspace root`);
                }
              }
            } catch { /* best effort */ }
          }

          success = true;
          log.info(`📦 Workspace injected (${(tarBuf.length / 1024).toFixed(1)} KB, ${fileCount} files)`);
        } catch (injectErr: any) {
          errorMsg = injectErr.message;
          log.warn(`Workspace injection failed: ${injectErr.message}`);
        }

        // Ack back to parent so they know data landed (or didn't)
        if (fromAmid && agtMeshClient) {
          try {
            await agtMeshClient.send(fromAmid, {
              type: "handoff:workspace_inject_ack",
              from_agent: process.env.SANDBOX_NAME || "unknown",
              success,
              file_count: fileCount,
              error: errorMsg || undefined,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
        }
        return;
      }

      // ── Handle handoff:resume — parent tells sub-agent to continue interrupted work ──
      // After workspace injection, the parent sends the task context so the sub-agent
      // can resume. The sub-agent reads .task-in-progress.json for checkpoint data,
      // reports to parent, and optionally re-starts the interrupted task.
      if (message?.type === "handoff:resume" && fromAmid && agtMeshClient) {
        const agentName = process.env.SANDBOX_NAME || "unknown";
        log.info(`▶️ Resume signal from '${fromName}' — checking for interrupted work...`);

        let progressInfo: Record<string, unknown> | null = null;
        try {
          const fs = await import("node:fs");
          const progressPath = "/sandbox/.openclaw/workspace/.task-in-progress.json";
          if (fs.existsSync(progressPath)) {
            progressInfo = JSON.parse(fs.readFileSync(progressPath, "utf8"));
            log.info(`📋 Found interrupted task: round ${progressInfo?.round}/${progressInfo?.total_rounds}`);
          }
        } catch { /* no progress file */ }

        // Report to parent: "I'm alive, here's my status"
        try {
          await agtMeshClient.send(fromAmid, {
            type: "handoff:resume_ack",
            from_agent: agentName,
            previous_status: message.previous_status || "unknown",
            has_interrupted_task: !!progressInfo,
            interrupted_task: progressInfo ? {
              task: (progressInfo.task as string)?.slice(0, 500),
              round: progressInfo.round,
              interrupted_at: progressInfo.interrupted_at,
            } : null,
            message: progressInfo
              ? `Successfully restored in cloud. Resuming interrupted work from round ${progressInfo.round}: ${(progressInfo.task as string)?.slice(0, 200)}`
              : `Successfully restored in cloud. No interrupted work — ready for new tasks.`,
            timestamp: new Date().toISOString(),
          });
          log.info(`🤝 Sent resume_ack to parent '${fromName}'`);
        } catch { /* best effort */ }

        // If there's interrupted work, resume it
        if (progressInfo?.task) {
          log.info(`▶️ Resuming interrupted task...`);
          const resumePrompt = `You were previously working on a task that was interrupted for a handoff migration. Here's what was happening:\n\n` +
            `Original task: ${progressInfo.task}\n` +
            `Progress: completed ${progressInfo.round} of ${progressInfo.total_rounds} tool-calling rounds\n` +
            `Last output: ${(progressInfo.last_content as string)?.slice(0, 1000) || "(none)"}\n\n` +
            `Please continue from where you left off. Complete the remaining work and report the results.`;
          // Reset interrupt flag for this fresh start
          handoffInterruptRequested = false;
          handoffInterruptReason = "";
          try {
            const llmResponse = await processTaskWithTools(resumePrompt, log);
            // Report results to parent
            await agtMeshClient.send(fromAmid, {
              type: "task_response",
              content: `[Resumed after handoff] ${llmResponse}`,
              from_agent: agentName,
              in_reply_to: "handoff:resume",
              timestamp: new Date().toISOString(),
            });
            log.info(`✅ Interrupted task completed after handoff resume`);
          } catch (resumeErr: any) {
            log.warn(`Task resumption failed: ${resumeErr.message}`);
            try {
              await agtMeshClient.send(fromAmid, {
                type: "task_response",
                content: `[Resume failed] ${resumeErr.message}`,
                from_agent: agentName,
                timestamp: new Date().toISOString(),
              });
            } catch { /* best effort */ }
          }
        }
        return;
      }

      // ── Handle handoff_transfer messages — target receives state blob ──
      // The source agent sends this after snapshot + drain. The target agent
      // restores the state on its own router and sends verification back.
      if (message?.type === "handoff_transfer" && fromAmid && agtMeshClient) {
        log.info(`🔄 Handoff transfer received from '${fromName}' — restoring state...`);
        try {
          const adminToken = await _readAdminToken();
          if (!adminToken) throw new Error("No admin token available for handoff restore");

          // Validate direction matches our environment
          const incomingDirection = message.direction || "local_to_aks";
          const isDevMode = process.env.AZURECLAW_DEV_MODE === "true";
          // If we're in dev mode (Docker), we should be receiving aks_to_local.
          // If we're in AKS, we should be receiving local_to_aks.
          const expectedDirection = isDevMode ? "aks_to_local" : "local_to_aks";
          if (incomingDirection !== expectedDirection) {
            log.warn(
              `⚠️ Handoff direction mismatch: received '${incomingDirection}' but ` +
              `expected '${expectedDirection}' (dev_mode=${isDevMode}). Proceeding with caution.`
            );
          }

          const authH = { Authorization: `Bearer ${adminToken}` };

          // 1. Initialize a handoff session on our own router
          const initResp = await _routerCallStrict("POST", "/agt/handoff/init", {
            direction: incomingDirection,
            ttl_seconds: 300,
            predecessor_amid: fromAmid,
          }, 15000, authH);

          const handoffToken = initResp.handoff_token;
          const hHeaders = { ...authH, "X-Handoff-Token": handoffToken };

          // 2. Restore the state blob
          const restoreResp = await _routerCallStrict("POST", "/agt/handoff/restore", {
            shared_secret: message.shared_secret,
            blob: message.blob,
          }, 30000, hHeaders);

          log.info(`✅ Handoff restore complete: trust_scores=${restoreResp.trust_scores_count || 0}, audit=${restoreResp.audit_entries_count || 0}, sub_agent_snapshots=${restoreResp.sub_agent_snapshots || 0}, sub_agent_workspaces=${Array.isArray(restoreResp.sub_agent_workspaces) ? restoreResp.sub_agent_workspaces.length : "missing"}, sub_agent_results=${Array.isArray(restoreResp.sub_agent_results) ? restoreResp.sub_agent_results.length : "missing"}`);

          // 3. Compute verification digest
          const verifyResp = await _routerCallStrict("POST", "/agt/handoff/verify", {
            predecessor_amid: fromAmid,
            expected_hash: message.verification_hash,
          }, 15000, hHeaders);

          // 4. Send verification back to source via E2E mesh
          await agtMeshClient.send(fromAmid, {
            type: "handoff_verification",
            verification_hash: verifyResp.verification_hash,
            matches: verifyResp.matches,
            trust_scores_count: verifyResp.trust_scores_count,
            audit_entries_count: verifyResp.audit_entries_count,
            successor_amid: agtIdentity?.amid || "unknown",
            from_agent: agtSandboxName,
            timestamp: new Date().toISOString(),
          });

          log.info(`✅ Handoff verification sent back to '${fromName}' (hash_match=${verifyResp.matches})`);

          // 5. Decommission our handoff session (we're done receiving)
          await _routerCallStrict("POST", "/agt/handoff/decommission", {}, 15000, hHeaders).catch(() => {});

          // ── Post-restore: hydrate the cloud agent with transferred state ──
          // This runs async (best-effort) — handoff is already complete at this point.
          (async () => {
            try {
              console.log(`[azureclaw-handoff] IIFE started — sub_agent_results=${JSON.stringify(restoreResp.sub_agent_results?.length ?? "missing")}, sub_agent_workspaces=${JSON.stringify(restoreResp.sub_agent_workspaces?.length ?? "missing")}, meshClient=${!!agtMeshClient}`);
              const fs = await import("node:fs");
              const agentName = process.env.SANDBOX_NAME || "dev-agent";
              const apiVer = "api-version=2025-11-15-preview";

              // Parse chat snapshot from restore response (returned by router)
              let chatMessages: Array<{ role: string; content: string; timestamp?: string }> = [];
              try {
                if (restoreResp.chat_snapshot) {
                  const raw = JSON.parse(restoreResp.chat_snapshot);
                  if (!Array.isArray(raw)) throw new Error("chat_snapshot is not an array");
                  // Schema validation: only accept {role, content} objects, cap at 100 messages
                  for (const msg of raw.slice(0, 100)) {
                    if (typeof msg?.role === "string" && typeof msg?.content === "string") {
                      chatMessages.push({
                        role: msg.role.slice(0, 20),
                        content: msg.content.slice(0, 10000),
                        ...(typeof msg.timestamp === "string" ? { timestamp: msg.timestamp.slice(0, 30) } : {}),
                      });
                    }
                  }
                  log.info(`📜 Handoff: loaded ${chatMessages.length} chat messages from snapshot`);
                }
              } catch { /* no valid chat snapshot — that's OK */ }

              // Extract workspace tar to /sandbox/ (plugin runs in openclaw container)
              if (restoreResp.workspace_tar) {
                try {
                  const { execSync } = await import("node:child_process");
                  const tarBuf = Buffer.from(restoreResp.workspace_tar, "base64");

                  // Size guard: reject decompression bombs (5MB compressed ≈ 50MB limit)
                  const MAX_TAR_BYTES = 5 * 1024 * 1024;
                  if (tarBuf.length > MAX_TAR_BYTES) {
                    throw new Error(`workspace tar too large: ${tarBuf.length} bytes (max ${MAX_TAR_BYTES})`);
                  }

                  // Write to unique temp path to avoid /tmp race conditions
                  const tmpDir = `/tmp/handoff-${Date.now()}`;
                  fs.mkdirSync(tmpDir, { recursive: true });
                  const tarPath = `${tmpDir}/workspace.tar.gz`;
                  fs.writeFileSync(tarPath, tarBuf, { mode: 0o600 });

                  // Validate: list entries and reject path traversal / symlinks
                  const listing = execSync(`tar tzf ${tarPath} 2>/dev/null`, { timeout: 5000 }).toString();
                  const entries = listing.split("\n").filter(Boolean);
                  for (const entry of entries) {
                    if (entry.includes("..") || entry.startsWith("/")) {
                      throw new Error(`path traversal blocked in workspace tar: ${entry}`);
                    }
                  }

                  // Extract safely: --no-same-owner (drop root ownership),
                  // --no-overwrite-dir, no following symlinks outside target
                  execSync(
                    `tar xzf ${tarPath} -C /sandbox/ --no-same-owner --no-overwrite-dir 2>/dev/null`,
                    { timeout: 10000 },
                  );
                  log.info(`📦 Handoff: extracted ${entries.length} workspace entries to /sandbox/`);

                  // Cleanup temp
                  fs.rmSync(tmpDir, { recursive: true, force: true });
                } catch (tarErr: any) {
                  log.warn(`Handoff: workspace tar extraction failed: ${tarErr.message}`);
                }
              }

              const meta = {
                predecessor_amid: restoreResp.predecessor_amid || fromAmid,
                direction: restoreResp.direction || "local_to_aks",
                trust_scores_count: restoreResp.trust_scores_count || 0,
                audit_entries_count: restoreResp.audit_entries_count || 0,
                sub_agents_respawned: (restoreResp.sub_agent_results || [])
                  .filter((r: any) => r.status === "spawned").length,
                restored_at: restoreResp.restored_at || new Date().toISOString(),
              };

              // 1. Create a Foundry Conversation with the transferred chat history
              let handoffConvId: string | undefined;
              if (chatMessages.length > 0) {
                try {
                  const convResp = await _routerCall("POST", `/openai/conversations?${apiVer}`, {
                    metadata: {
                      user: agentName,
                      source: "handoff",
                      predecessor: meta.predecessor_amid || fromAmid,
                      direction: meta.direction || "local_to_aks",
                    },
                  });
                  handoffConvId = convResp?.id;
                  if (handoffConvId) {
                    // Replay messages into the conversation (batch in chunks of 10)
                    const items = chatMessages.map((m: any) => ({
                      type: "message",
                      role: m.role === "assistant" ? "assistant" : "user",
                      content: [{ type: "input_text", text: String(m.content || "").slice(0, 10000) }],
                    }));
                    for (let i = 0; i < items.length; i += 10) {
                      const batch = items.slice(i, i + 10);
                      await _routerCall("POST", `/openai/conversations/${handoffConvId}/items?${apiVer}`, { items: batch }).catch(() => {});
                    }
                    log.info(`📝 Handoff: replayed ${items.length} messages into Foundry conversation ${handoffConvId}`);
                  }
                } catch (e: any) {
                  log.warn(`Handoff: Foundry conversation replay failed (non-fatal): ${e.message}`);
                }
              }

              // 2. Store handoff event in Foundry Memory (includes conversation ID for startup recall)
              const store = `memory-${agentName}`;
              const recentSummary = chatMessages.slice(-5).map((m: any) =>
                `${m.role}: ${String(m.content || "").slice(0, 200)}`
              ).join("\n");
              const memoryText = [
                `[Handoff event] I was migrated from local dev to cloud (AKS) at ${meta.restored_at || new Date().toISOString()}.`,
                `Predecessor AMID: ${meta.predecessor_amid || fromAmid}.`,
                `Direction: ${meta.direction || "local_to_aks"}.`,
                `Trust scores transferred: ${meta.trust_scores_count || 0}.`,
                `Audit entries transferred: ${meta.audit_entries_count || 0}.`,
                handoffConvId ? `Foundry conversation: ${handoffConvId}.` : "",
                chatMessages.length > 0 ? `\nRecent conversation context (last ${Math.min(5, chatMessages.length)} messages):\n${recentSummary}` : "",
              ].filter(Boolean).join(" ");

              try {
                await _routerCall("POST", `/memory_stores/${store}:update_memories?${apiVer}`, {
                  scope: agentName,
                  items: [{ type: "message", role: "assistant", content: [{ type: "input_text", text: memoryText }] }],
                  update_delay: 0,
                });
                log.info("🧠 Handoff: stored handoff event in Foundry Memory");
              } catch (e: any) {
                log.warn(`Handoff: Foundry Memory update failed (non-fatal): ${e.message}`);
              }

              // 3. Persist handoff context so the agent can see it
              //    a) Write .handoff-state.json — picked up by MEMORY.md builder on every plugin load
              //    b) Inject directly into MEMORY.md now (in case Foundry context hasn't written yet)
              //    c) Keep HANDOFF_CONTEXT.md as a human-readable backup
              //    d) Foundry Memory + Conversation are the durable stores (survive pod recreation)
              const handoffState = {
                restored_at: meta.restored_at || new Date().toISOString(),
                predecessor_amid: meta.predecessor_amid || fromAmid,
                direction: meta.direction || "local_to_aks",
                trust_scores_count: meta.trust_scores_count || 0,
                audit_entries_count: meta.audit_entries_count || 0,
                chat_message_count: chatMessages.length,
                conversation_id: handoffConvId,
                recent_messages: chatMessages.slice(-10).map((m: any) => ({
                  role: String(m.role).slice(0, 20),
                  content: String(m.content || "").slice(0, 500),
                })),
              };
              try {
                fs.mkdirSync("/sandbox/.openclaw/workspace", { recursive: true });

                // Flag file for MEMORY.md builder
                fs.writeFileSync(
                  "/sandbox/.openclaw/workspace/.handoff-state.json",
                  JSON.stringify(handoffState),
                  { mode: 0o600 },
                );

                // Inject into MEMORY.md directly (the agent reads this file)
                const memoryFile = "/sandbox/.openclaw/workspace/MEMORY.md";
                const handoffSection = [
                  "\n## Handoff Context\n",
                  `This agent was migrated from local dev to cloud (AKS) at ${handoffState.restored_at}.`,
                  `Predecessor AMID: ${handoffState.predecessor_amid}. Direction: ${handoffState.direction}.`,
                  `Trust scores: ${handoffState.trust_scores_count}, Audit trail: ${handoffState.audit_entries_count} entries.`,
                  `Chat history: ${handoffState.chat_message_count} messages transferred.\n`,
                  "### Recent Conversation Before Handoff\n",
                  ...handoffState.recent_messages.map((m: { role: string; content: string }) =>
                    `**${m.role}**: ${m.content}`
                  ),
                  "",
                ].join("\n");
                let existingMem = "";
                try { existingMem = fs.readFileSync(memoryFile, "utf8"); } catch { /* first run */ }
                // Insert after the --- env marker if it exists, otherwise append
                const endMarker = "\n---\n";
                if (existingMem.includes(endMarker)) {
                  const idx = existingMem.indexOf(endMarker) + endMarker.length;
                  const before = existingMem.slice(0, idx);
                  const after = existingMem.slice(idx);
                  fs.writeFileSync(memoryFile, before + handoffSection + after);
                } else {
                  fs.writeFileSync(memoryFile, existingMem + handoffSection);
                }

                // Human-readable backup
                const contextMd = [
                  "# Handoff Context",
                  "",
                  `> This agent was migrated from local dev to cloud (AKS) at ${handoffState.restored_at}.`,
                  `> Predecessor AMID: \`${handoffState.predecessor_amid}\``,
                  `> Direction: ${handoffState.direction}`,
                  "",
                  "## Recent Conversation",
                  "",
                  ...chatMessages.slice(-20).map((m: any) =>
                    `**${m.role}**: ${String(m.content || "").slice(0, 500)}`
                  ),
                  "",
                  "---",
                  "*This file was created automatically during handoff. The full conversation is also stored in Foundry Conversations and Memory.*",
                ].join("\n");
                fs.writeFileSync("/sandbox/.openclaw/workspace/HANDOFF_CONTEXT.md", contextMd);
                log.info("📄 Handoff: wrote context to MEMORY.md + .handoff-state.json + HANDOFF_CONTEXT.md");
              } catch (e: any) {
                log.warn(`Handoff: context file write failed: ${e.message}`);
              }

              // 4. Register re-spawned sub-agents as trusted + inject workspaces + resume
              // IMPORTANT: Use sub_agent_results (always populated for spawned agents)
              // as the primary loop driver — NOT sub_agent_workspaces (which may be
              // empty if workspace data was lost in the snapshot round-trip).
              const spawnedSubs: Array<{ name: string; original_amid?: string; status?: string }> =
                (restoreResp.sub_agent_results || []).filter((r: any) => r.status === "spawned");
              const subWorkspaceMap = new Map<string, any>();
              for (const ws of (restoreResp.sub_agent_workspaces || [])) {
                subWorkspaceMap.set(ws.name, ws);
              }
              const subAgentStatuses: Array<{ name: string; status: string; task?: string }> = [];
              console.log(`[azureclaw-handoff] step 4: spawned=${spawnedSubs.length} (${spawnedSubs.map((s: any) => s.name).join(",")}), workspaces=${subWorkspaceMap.size}, meshClient=${!!agtMeshClient}`);
              log.info(`📦 Handoff step 4: spawned=${spawnedSubs.length} (${spawnedSubs.map((s: any) => s.name).join(",")}), workspaces=${subWorkspaceMap.size}, meshClient=${!!agtMeshClient}`);

              if (spawnedSubs.length > 0 && agtMeshClient) {
                console.log(`[azureclaw-handoff] entering trust+resume loop for ${spawnedSubs.length} sub-agents`);
                log.info(`🤖 Handoff: registering trust + resuming ${spawnedSubs.length} sub-agent(s)...`);

                // Collect OLD AMIDs from predecessor's snapshot so we can reject stale
                // registry entries. After handoff, sub-agents get new key pairs → new AMIDs.
                // The old Docker AMIDs may still be in the registry briefly.
                const staleAmids = new Set<string>();
                for (const spawned of spawnedSubs) {
                  if (spawned.original_amid) {
                    staleAmids.add(spawned.original_amid);
                    // Clear stale cache entries
                    nameToAmid.delete(spawned.name);
                    amidToName.delete(spawned.original_amid);
                    parentTrustedAmids.delete(spawned.original_amid);
                  }
                }
                if (staleAmids.size > 0) {
                  log.info(`🧹 Handoff: cleared ${staleAmids.size} stale AMID(s) from cache: ${[...staleAmids].map(a => a.slice(0, 12) + "...").join(", ")}`);
                }

                for (const spawned of spawnedSubs) {
                  try {
                    // Wait for sub-agent to register in mesh with a NEW AMID
                    // (up to 90s — pods need boot time + SDK init + relay connect)
                    // IMPORTANT: reject old AMIDs from predecessor — they're dead connections
                    let subAmid: string | undefined;
                    const subStart = Date.now();
                    while (Date.now() - subStart < 90_000) {
                      try {
                        const searchResult = await _routerCall("GET",
                          `/agt/registry/registry/search?capability=${encodeURIComponent(spawned.name)}`);
                        const candidates = (searchResult?.results || []).filter((a: any) =>
                          a.display_name === spawned.name && a.status === "online"
                        );
                        // Pick the first candidate that is NOT a stale AMID
                        const match = candidates.find((a: any) => !staleAmids.has(a.amid));
                        if (match?.amid) {
                          subAmid = match.amid;
                          log.info(`🔍 Found NEW AMID for '${spawned.name}': ${match.amid.slice(0, 12)}...${spawned.original_amid ? ` (old was ${spawned.original_amid.slice(0, 12)}...)` : ""}`);
                          break;
                        }
                        if (candidates.length > 0 && !match) {
                          log.info(`⏳ Registry has '${spawned.name}' but AMID is stale (${candidates[0].amid.slice(0, 12)}...) — waiting for new registration`);
                        }
                      } catch { /* not registered yet */ }
                      await new Promise(r => setTimeout(r, 3000));
                    }

                    if (!subAmid) {
                      log.warn(`Sub-agent '${spawned.name}' didn't register in mesh within 90s — skipping`);
                      subAgentStatuses.push({ name: spawned.name, status: "not_found" });
                      continue;
                    }

                    // Register new sub-agent AMID in trust maps so parent accepts
                    // their messages (KNOCK handler checks parentTrustedAmids).
                    // After handoff, sub-agents have new key pairs → new AMIDs.
                    amidToName.set(subAmid, spawned.name);
                    nameToAmid.set(spawned.name, subAmid);
                    parentTrustedAmids.add(subAmid);
                    try {
                      await pushTrustToRouter(spawned.name, 0.0);
                      log.info(`🔑 Registered re-spawned sub-agent '${spawned.name}' as trusted (${subAmid.slice(0, 12)}...)`);
                    } catch {
                      log.warn(`Failed to push trust for re-spawned sub-agent '${spawned.name}'`);
                    }

                    // Look up workspace data for this sub-agent (may be absent)
                    const wsData = subWorkspaceMap.get(spawned.name);

                    // Wait for sub-agent's E2E session to be ready (prekey available on relay)
                    // before sending workspace_inject. Without this, messages go to the void.
                    let preKeyReady = false;
                    const pkStart = Date.now();
                    for (let pkAttempt = 0; pkAttempt < 20 && Date.now() - pkStart < 60_000; pkAttempt++) {
                      try {
                        await agtMeshClient.send(subAmid, { type: "ping", from_agent: agentName });
                        preKeyReady = true;
                        log.info(`🔗 E2E session ready for '${spawned.name}' (attempt ${pkAttempt + 1})`);
                        break;
                      } catch (pkErr: any) {
                        if (pkErr.message?.includes("prekey") || pkErr.message?.includes("prekeys")) {
                          log.info(`⏳ Waiting for prekeys from '${spawned.name}' (${pkAttempt + 1}/20)...`);
                          await new Promise(r => setTimeout(r, 3000));
                        } else {
                          log.warn(`⚠️ E2E session check failed for '${spawned.name}': ${pkErr.message}`);
                          break;
                        }
                      }
                    }
                    if (!preKeyReady) {
                      log.warn(`Sub-agent '${spawned.name}' E2E session not ready after 60s — sending anyway (best effort)`);
                    }

                    // Send workspace tar via meshSend if available (auto-chunks if large)
                    // Retry up to 3 times with ack verification
                    let workspaceDelivered = false;
                    if (wsData?.workspace_tar) {
                      for (let wsAttempt = 0; wsAttempt < 3 && !workspaceDelivered; wsAttempt++) {
                        if (wsAttempt > 0) {
                          log.info(`📦 Retrying workspace_inject for '${spawned.name}' (attempt ${wsAttempt + 1}/3)`);
                          await new Promise(r => setTimeout(r, 3000));
                        }
                        try {
                          await meshSend(agtMeshClient, subAmid, {
                            type: "handoff:workspace_inject",
                            workspace_tar: wsData.workspace_tar,
                            from_agent: agentName,
                            timestamp: new Date().toISOString(),
                          }, log);
                          log.info(`📦 Sent workspace to sub-agent '${spawned.name}' — waiting for ack (attempt ${wsAttempt + 1})...`);
                        } catch (sendErr: any) {
                          log.warn(`📦 workspace_inject send failed for '${spawned.name}': ${sendErr.message}`);
                          continue;
                        }

                        // Wait up to 20s for workspace_inject_ack
                        const wsAckStart = Date.now();
                        while (Date.now() - wsAckStart < 20_000) {
                          const ackIdx = agtInbox.findIndex(m => {
                            try {
                              const c = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                              return c?.type === "handoff:workspace_inject_ack" && c?.from_agent === spawned.name;
                            } catch { return false; }
                          });
                          if (ackIdx >= 0) {
                            const ackMsg = agtInbox.splice(ackIdx, 1)[0];
                            let parsed: any;
                            try { parsed = typeof ackMsg.content === "string" ? JSON.parse(ackMsg.content) : ackMsg.content; } catch { parsed = {}; }
                            workspaceDelivered = !!parsed.success;
                            log.info(`📦 Workspace ack from '${spawned.name}': success=${parsed.success}, files=${parsed.file_count}${parsed.error ? `, error=${parsed.error}` : ""}`);
                            break;
                          }
                          await new Promise(r => setTimeout(r, 500));
                        }
                        if (!workspaceDelivered) {
                          log.warn(`No workspace_inject_ack from '${spawned.name}' within 20s (attempt ${wsAttempt + 1}/3)`);
                        }
                      }
                    }

                    // Send resume message with task context
                    const resumePayload: Record<string, unknown> = {
                      type: "handoff:resume",
                      from_agent: agentName,
                      task_context: wsData?.task_context || "",
                      previous_status: wsData?.status || "unknown",
                      checkpoint: wsData?.checkpoint || null,
                      workspace_delivered: workspaceDelivered,
                      timestamp: new Date().toISOString(),
                    };
                    await agtMeshClient.send(subAmid, resumePayload);
                    log.info(`▶️ Sent resume to sub-agent '${spawned.name}' (status: ${wsData?.status || "?"})`);
                    subAgentStatuses.push({
                      name: spawned.name,
                      status: "resuming",
                      task: (wsData?.task_context || "").slice(0, 200),
                      workspace_delivered: workspaceDelivered,
                    } as any);
                  } catch (subErr: any) {
                    log.warn(`Sub-agent '${spawned.name}' trust/resume failed: ${subErr.message}`);
                    subAgentStatuses.push({ name: spawned.name, status: "failed" });
                  }
                }

                // Brief wait for resume_ack messages (best-effort, 8s)
                if (subAgentStatuses.some(s => s.status === "resuming")) {
                  const ackWaitStart = Date.now();
                  while (Date.now() - ackWaitStart < 8_000) {
                    for (const sa of subAgentStatuses) {
                      if (sa.status !== "resuming") continue;
                      const idx = agtInbox.findIndex(m =>
                        (m.message_type === "handoff:resume_ack" ||
                          (typeof m.content === "string" && m.content.includes("handoff:resume_ack"))) &&
                        (() => {
                          try {
                            const c = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                            return c?.from_agent === sa.name;
                          } catch { return false; }
                        })()
                      );
                      if (idx >= 0) {
                        const msg = agtInbox.splice(idx, 1)[0];
                        let parsed: any;
                        try { parsed = typeof msg.content === "string" ? JSON.parse(msg.content) : msg.content; } catch { parsed = {}; }
                        sa.status = parsed?.has_interrupted_task ? "resumed" : "ready";
                        sa.task = parsed?.interrupted_task?.task?.slice(0, 200) || sa.task;
                        log.info(`🤝 Sub-agent '${sa.name}' checked in: ${sa.status}`);
                      }
                    }
                    if (subAgentStatuses.every(s => s.status !== "resuming")) break;
                    await new Promise(r => setTimeout(r, 500));
                  }
                }
              } else {
                console.log(`[azureclaw-handoff] trust loop SKIPPED: spawned=${spawnedSubs.length}, meshClient=${!!agtMeshClient}`);
              }

              // 5. Send Telegram greeting (now includes sub-agent status)
              const tgToken = process.env.TELEGRAM_BOT_TOKEN;
              const tgAllowFrom = process.env.TELEGRAM_ALLOW_FROM;
              if (tgToken && tgAllowFrom) {
                // Build a personalized greeting with conversation context
                const lastUserMsg = [...chatMessages].reverse().find((m: any) => m.role === "user");
                const lastAssistantMsg = [...chatMessages].reverse().find((m: any) => m.role === "assistant");
                const escapeMd2 = (s: string) => s.replace(/[_*[\]()~`>#+\-=|{}.!\\]/g, "\\$&");

                const lines: string[] = [
                  "☁️ *AzureClaw — Cloud Handoff Complete*",
                  "",
                  `I've been migrated to the cloud \\(AKS\\) and I'm ready to continue\\.`,
                  `Model: \`${escapeMd2(process.env.DEFAULT_MODEL || "gpt-5.4")}\` · Sandbox: \`${escapeMd2(agentName)}\``,
                ];
                if (chatMessages.length > 0) {
                  lines.push("", `📜 _${chatMessages.length} messages transferred from our previous session\\._`);
                }

                // Sub-agent status section
                if (subAgentStatuses.length > 0) {
                  lines.push("", "🤖 *Sub\\-agents:*");
                  for (const sa of subAgentStatuses) {
                    const wsFlag = (sa as any).workspace_delivered ? " 📦" : "";
                    const icon = sa.status === "resumed" ? "▶️" : sa.status === "ready" ? "✅" : sa.status === "resuming" ? "⏳" : "❌";
                    const taskPreview = sa.task ? `: _${escapeMd2(sa.task.slice(0, 100))}${sa.task.length > 100 ? "\\.\\.\\." : ""}_` : "";
                    const statusLabel = sa.status === "resumed" ? "resuming work"
                      : sa.status === "ready" ? "ready"
                      : sa.status === "resuming" ? "starting up"
                      : "failed to restore";
                    lines.push(`  ${icon} *${escapeMd2(sa.name)}* — ${statusLabel}${wsFlag}${taskPreview}`);
                  }
                }

                if (lastUserMsg) {
                  const preview = escapeMd2(String(lastUserMsg.content).slice(0, 200));
                  lines.push("", `🔖 *Where we left off:*`, `Your last message: "${preview}${String(lastUserMsg.content).length > 200 ? "\\.\\.\\." : ""}"`);
                }
                if (lastAssistantMsg) {
                  const preview = escapeMd2(String(lastAssistantMsg.content).slice(0, 200));
                  lines.push(`My last reply: "${preview}${String(lastAssistantMsg.content).length > 200 ? "\\.\\.\\." : ""}"`);
                }
                lines.push("", "Want to pick up where we left off? Just send me a message\\!");
                const greetingText = lines.join("\n");

                // Send via router's egress proxy (respects allowlist + transparent proxy)
                for (const chatId of tgAllowFrom.split(",").map((s: string) => s.trim()).filter(Boolean)) {
                  try {
                    await _routerCall("POST", "/egress/fetch", {
                      url: `https://api.telegram.org/bot${tgToken}/sendMessage`,
                      method: "POST",
                      headers: { "Content-Type": "application/json" },
                      body: JSON.stringify({
                        chat_id: chatId,
                        text: greetingText,
                        parse_mode: "MarkdownV2",
                      }),
                    });
                    log.info(`📱 Handoff: sent Telegram greeting to chat ${chatId}`);
                  } catch (tgErr: any) {
                    log.warn(`Handoff: Telegram greeting failed for ${chatId}: ${tgErr.message}`);
                  }
                }
              }

              // 6. Send "handoff_ready" mesh message back to predecessor
              if (agtMeshClient && fromAmid) {
                try {
                  await agtMeshClient.send(fromAmid, {
                    type: "handoff_ready",
                    from_agent: agentName,
                    successor_amid: agtIdentity?.amid || "unknown",
                    chat_messages_loaded: chatMessages.length,
                    memory_stored: true,
                    telegram_greeted: !!(tgToken && tgAllowFrom),
                    sub_agents_restored: subAgentStatuses.length,
                    sub_agents_resumed: subAgentStatuses.filter(s => s.status === "resumed").length,
                    sub_agents_workspace_delivered: subAgentStatuses.filter(s => s.status === "resumed" || s.status === "ready").length,
                    sub_agent_details: subAgentStatuses,
                    timestamp: new Date().toISOString(),
                  });
                  log.info(`🤝 Handoff: sent 'handoff_ready' mesh message to predecessor`);
                } catch { /* predecessor may already be decommissioned */ }
              }
            } catch (hydrateErr: any) {
              console.log(`[azureclaw-handoff] IIFE error: ${hydrateErr.message}`);
              log.warn(`Handoff hydration failed (non-fatal): ${hydrateErr.message}`);
            }
          })();

        } catch (restoreErr: any) {
          log.warn(`❌ Handoff restore failed: ${restoreErr.message}`);
          // Notify source of failure
          try {
            await agtMeshClient.send(fromAmid, {
              type: "handoff_verification",
              error: restoreErr.message,
              matches: false,
              from_agent: agtSandboxName,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
        }
      }
    });

    // ── Connect to the mesh (handlers are registered, safe to receive) ──
    agtSandboxName = process.env.SANDBOX_NAME
      || (process.env.HOSTNAME ? process.env.HOSTNAME.replace(/-[a-f0-9]+-[a-z0-9]+$/, "") : "unknown");
    // Validate sandbox name format
    if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(agtSandboxName)) {
      log.warn(`Invalid SANDBOX_NAME "${sanitizeLog(agtSandboxName, 30)}" — falling back to "unknown"`);
      agtSandboxName = "unknown";
    }

    let connected = false;
    for (let attempt = 1; attempt <= 5; attempt++) {
      try {
        await agtMeshClient.connect({
          displayName: agtSandboxName,
          capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
        });
        log.info(`AGT mesh connected (relay: ${relayUrl}, registry: ${registryUrl})`);
        connected = true;
        agtConnected = true;
        break;
      } catch (connErr: any) {
        const delay = attempt * 2;
        if (attempt < 5) {
          log.warn(`AGT mesh connect attempt ${attempt}/5 failed: ${connErr.message} — retrying in ${delay}s`);
          await new Promise(r => setTimeout(r, delay * 1000));
        } else {
          log.warn(`AGT mesh connect failed after 5 attempts: ${connErr.message}. Mesh tools will be unavailable.`);
        }
      }
    }

    // Store on process for cross-context singleton access
    (process as any)[Symbol.for("agt-mesh-client")] = agtMeshClient;
    (process as any)[Symbol.for("agt-identity")] = agtIdentity;

    // ── Pre-seed trusted peers from parent ──────────────────────────────
    // AGT_TRUSTED_PEERS is set by the parent's router at spawn time.
    // Format: "name:AMID,name:AMID,..." — these are parent-verified,
    // not self-reported, so they're safe to auto-trust.
    const trustedPeersEnv = process.env.AGT_TRUSTED_PEERS || "";
    if (trustedPeersEnv && connected) {
      const peers = trustedPeersEnv.split(",").filter(Boolean);
      for (const peer of peers) {
        const [name, amid] = peer.split(":");
        if (name && amid) {
          amidToName.set(amid, name);
          nameToAmid.set(name, amid);
          parentTrustedAmids.add(amid);
          // Push baseline trust (score=500 = threshold) via local admin token
          try {
            await pushTrustToRouter(name, 0.0);
            log.info(`AGT trusted peer seeded: ${name} (${amid.slice(0, 12)}...)`);
          } catch {
            log.warn(`AGT trusted peer seed failed for ${name}`);
          }
        }
      }
      if (peers.length > 0) {
        log.info(`AGT pre-seeded ${peers.length} trusted peer(s) from parent`);
      }
    }

    // ── Proactive offload startup ────────────────────────────────────────
    // If this sandbox was spawned by the controller to handle an offload
    // request, the OFFLOAD_REQUEST_ID / OFFLOAD_PARENT_AMID / OFFLOAD_TASK
    // env vars are set. Announce availability to the parent via `offload_hello`
    // and start executing immediately — no need for a round-trip offload_task.
    if (connected && process.env.OFFLOAD_REQUEST_ID) {
      // Fire-and-forget — don't block initAGT on the task.
      startProactiveOffloadIfNeeded(log).catch((err: any) => {
        log.warn(`Proactive offload init error: ${err.message}`);
      });
    }

    // ── OAuth/Entra identity verification (opt-in) ────────────────────────
    // If AGT_OAUTH_TOKEN is set (from Workload Identity token exchange or
    // manual config), POST to /registry/verify to upgrade tier from anonymous
    // to verified. Also sets up 12-hour periodic re-verification.
    const oauthToken = process.env.AGT_OAUTH_TOKEN || "";
    if (oauthToken && connected) {
      try {
        await _routerCall("POST", "/agt/registry/v1/registry/verify", {
          amid: agtIdentity.amid,
          verification_token: oauthToken,
        });
        log.info("AGT identity verified via OAuth — tier upgraded to 'verified'");
      } catch (verifyErr: any) {
        log.warn(`AGT OAuth verification failed: ${verifyErr.message} — running as anonymous tier`);
      }

      // Periodic re-verification every 12 hours (refreshes certificate expiry)
      setInterval(async () => {
        const token = process.env.AGT_OAUTH_TOKEN || "";
        if (!token || !agtIdentity) return;
        try {
          await _routerCall("POST", "/agt/registry/v1/registry/verify", {
            amid: agtIdentity.amid,
            verification_token: token,
          });
          log.info("AGT identity re-verified (12hr cycle)");
        } catch { /* fail-open — stays at current tier */ }
      }, 12 * 60 * 60 * 1000);
    }

    // ── Disconnect handler + auto-reconnect ──────────────────────────────
    // If the WS connection drops (relay restart, network blip), try to reconnect.
    if (agtMeshClient.onDisconnect) {
      agtMeshClient.onDisconnect(() => {
        agtConnected = false;
        log.warn("AGT mesh disconnected — will attempt reconnect in 15s");
      });
    }

    // Reconnect timer with exponential backoff: starts at 30s, backs off on
    // repeated failures to avoid CPU spin when registry/relay is unreachable.
    if (agtReconnectTimer) clearTimeout(agtReconnectTimer);
    const scheduleReconnect = () => {
      const delay = Math.min(30_000 * Math.pow(2, agtReconnectFailures), AGT_RECONNECT_MAX_BACKOFF);
      agtReconnectTimer = setTimeout(async () => {
        if (!agtConnected && agtMeshClient) {
          await agtReconnect(log);
          if (!agtConnected) {
            agtReconnectFailures++;
            if (agtReconnectFailures <= 5) {
              log.warn(`AGT reconnect backoff: next attempt in ${Math.min(30 * Math.pow(2, agtReconnectFailures), 300)}s`);
            }
          } else {
            agtReconnectFailures = 0;
          }
        }
        // Heartbeat: ping the relay proxy to keep the connection warm
        // and send a registry heartbeat to keep status as "online"
        if (agtConnected) {
          try {
            const http = await import("node:http");
            const req = http.request(routerUrl("/agt/status"), { timeout: 3000 }, () => {});
            req.on("error", () => {});
            req.end();
          } catch { /* best effort */ }
          // Registry heartbeat: update last_seen so other agents see us as online
          if (agtIdentity) {
            try {
              const http = await import("node:http");
              const body = JSON.stringify({ amid: agtIdentity.amid });
              const req = http.request(routerUrl("/agt/registry/registry/heartbeat"), {
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
                timeout: 3000,
              }, () => {});
              req.on("error", () => {});
              req.write(body);
              req.end();
            } catch { /* best effort */ }
          }
        }
        scheduleReconnect();
      }, delay);
      if (agtReconnectTimer.unref) agtReconnectTimer.unref();
    };
    scheduleReconnect();

    // ── Inbox notification timer ─────────────────────────────────────────
    // Every 10s, if there are unread messages, write a notification section
    // into MEMORY.md so the LLM sees them in its context window without
    // needing to manually call mesh_inbox. This is what keeps conversations
    // "lively" — the agent is proactively told it has messages to process.
    if (agtInboxNotifyTimer) clearInterval(agtInboxNotifyTimer);
    agtInboxNotifyTimer = setInterval(() => {
      notifyInboxToMemory(log).catch(() => {});
    }, 10_000);
    if (agtInboxNotifyTimer.unref) agtInboxNotifyTimer.unref();

    log.info(`AGT SDK loaded (v${sdk.VERSION}) — identity, policy, trust, audit${connected ? ", mesh ACTIVE" : ", mesh OFFLINE (relay unreachable)"}`);
    log.info("AGT timers started: reconnect (30s), inbox notify (10s)");
  } catch (e: any) {
    // Distinguish module-not-found from other errors
    const isModuleError = e.code === 'MODULE_NOT_FOUND' || e.code === 'ERR_MODULE_NOT_FOUND';
    if (isModuleError) {
      log.warn(`AGT SDK not installed: ${e.message}. Install @agentmesh/sdk to enable inter-agent communication.`);
    } else {
      log.warn(`AGT SDK init failed: ${e.message}. Stack: ${e.stack?.split('\n').slice(0, 3).join(' → ')}`);
    }
  }
  })(); // end of init IIFE
  // Promise is stored on process BEFORE the IIFE body runs (line below runs
  // synchronously because the IIFE returns a pending Promise immediately).
  (process as any)[Symbol.for("agt-init-promise")] = initPromise;
  await initPromise;
}

// ---------------------------------------------------------------------------
// Router HTTP helpers (`_routerCall`, `_routerCallStrict`, `_readAdminToken`,
// `_readAdminTokenSync`) live in cli/src/core/router-client.ts and are
// imported at the top of this file.
// ---------------------------------------------------------------------------

// _hp + _runHandoffOrchestration extracted to core/agt-handoff.ts in S15.f.7.
async function _runHandoffOrchestration(
  handoffToken: string, adminToken: string, direction: string, dirLabel: string,
) {
  if (!handoffState.current) return;
  return _runHandoffOrchestrationCore(handoffToken, adminToken, direction, dirLabel, {
    progress: handoffState.current,
    inbox: agtInbox,
    meshClient: () => agtMeshClient,
    identity: () => agtIdentity,
    meshSend,
    log: _log,
  });
}

// ---------------------------------------------------------------------------
// Foundry project discovery — query deployments, connections, indexes at init
// ---------------------------------------------------------------------------

async function initFoundry(log: { info: (m: string) => void; warn: (m: string) => void }) {
  // Allow re-initialization per session (register() is called once per session)
  if (foundryInitialized) return;
  foundryInitialized = true;
  foundryProject = await discoverFoundryProject(_routerCall, ensureMemoryStore, log);
}

// ---------------------------------------------------------------------------
// Background Foundry Memory sync — persist conversation summaries
// ---------------------------------------------------------------------------

const MEMORY_SYNC_INTERVAL = 10; // Sync every N tool calls
let memorySyncToolCount = 0;
let memorySyncBuffer: string[] = [];
let memorySyncInFlight = false;

async function ensureMemoryStore(store: string): Promise<void> {
  const apiVer = "api-version=2025-11-15-preview";
  try {
    await _routerCall("GET", `/memory_stores/${store}?${apiVer}`);
  } catch {
    const chatModel = process.env.OPENCLAW_MODEL || "gpt-4.1";
    await _routerCall("POST", `/memory_stores?${apiVer}`, {
      name: store,
      description: `Persistent memory for agent ${store.replace("memory-", "")}`,
      definition: {
        kind: "default",
        chat_model: chatModel,
        embedding_model: "text-embedding-3-small",
        options: {
          user_profile_enabled: true,
          user_profile_details: "Store user preferences, decisions, and project context",
          chat_summary_enabled: true,
        },
      },
    });
  }
}

async function syncToFoundryMemory(
  content: string,
  log: { info: (m: string) => void; warn: (m: string) => void },
) {
  if (memorySyncInFlight) return; // Prevent overlapping syncs
  memorySyncInFlight = true;
  try {
    const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
    const store = `memory-${agentName}`;
    try {
      await _routerCall("POST", `/memory_stores/${store}:update_memories?api-version=2025-11-15-preview`, {
        scope: agentName,
        items: [{ type: "message", role: "assistant", content: [{ type: "input_text", text: content }] }],
        update_delay: 0,
      });
      log.info(`Foundry memory sync: persisted ${content.length} chars`);
    } catch (e: any) {
      if (e?.message?.includes("404")) {
        await ensureMemoryStore(store);
        await _routerCall("POST", `/memory_stores/${store}:update_memories?api-version=2025-11-15-preview`, {
          scope: agentName,
          items: [{ type: "message", role: "assistant", content: [{ type: "input_text", text: content }] }],
          update_delay: 0,
        });
        log.info(`Foundry memory sync: created store + persisted ${content.length} chars`);
      }
    }
  } catch {
    // Best effort — don't disrupt agent workflow
  } finally {
    memorySyncInFlight = false;
  }
}

function trackToolExecution(
  toolName: string,
  params: Record<string, unknown>,
  resultText: string,
  log: { info: (m: string) => void; warn: (m: string) => void },
) {
  memorySyncToolCount++;
  // Build a compact summary line (tool name + key params, no secrets)
  const paramHint = Object.keys(params).filter(k => !k.includes("key") && !k.includes("token")).slice(0, 3).join(",");
  const resultSnippet = resultText.slice(0, 120).replace(/\n/g, " ");
  memorySyncBuffer.push(`[${memorySyncToolCount}] ${toolName}(${paramHint}) → ${resultSnippet}`);

  if (memorySyncToolCount % MEMORY_SYNC_INTERVAL === 0 && memorySyncBuffer.length > 0) {
    const batch = memorySyncBuffer.splice(0);
    const summary = `Agent activity checkpoint (calls ${memorySyncToolCount - batch.length + 1}–${memorySyncToolCount}):\n${batch.join("\n")}`;
    syncToFoundryMemory(summary, log).catch(() => {});
  }
}

// Flush remaining buffer on process exit (SIGTERM from pod shutdown)
let memorySyncShutdownRegistered = false;
function registerMemorySyncShutdownHook(log: { info: (m: string) => void; warn: (m: string) => void }) {
  if (memorySyncShutdownRegistered) return;
  memorySyncShutdownRegistered = true;
  const flush = () => {
    if (memorySyncBuffer.length === 0) return;
    const batch = memorySyncBuffer.splice(0);
    const summary = `Agent shutdown — final checkpoint (${batch.length} calls buffered):\n${batch.join("\n")}`;
    syncToFoundryMemory(summary, log).catch(() => {});
  };
  process.once("SIGTERM", flush);
  process.once("SIGINT", flush);
}

interface OpenClawConfig {
  [key: string]: unknown;
}

interface PluginLogger {
  info(message: string): void;
  warn(message: string): void;
  error(message: string): void;
}

interface PluginCliContext {
  program: Command;
  config: OpenClawConfig;
  workspaceDir?: string;
  logger: PluginLogger;
}

interface ProviderAuthMethod {
  id?: string;
  type: string;
  envVar?: string;
  headerName?: string;
  label?: string;
}

interface ModelProviderEntry {
  id: string;
  label: string;
  contextWindow?: number;
  maxOutput?: number;
}

interface ProviderPlugin {
  id: string;
  label: string;
  docsPath?: string;
  aliases?: string[];
  envVars?: string[];
  models?: {
    chat?: ModelProviderEntry[];
  };
  auth: ProviderAuthMethod[];
}

interface PluginCommandDefinition {
  name: string;
  description: string;
  acceptsArgs?: boolean;
  requireAuth?: boolean;
  handler: (ctx: { args?: string; channel?: string; config: OpenClawConfig }) => Promise<{ text: string }> | { text: string };
}

interface ToolDefinition {
  name: string;
  label: string;
  description: string;
  parameters?: Record<string, unknown>;
  execute: (toolCallId: string, params: Record<string, unknown>, signal?: AbortSignal) => Promise<{ content: Array<{ type: string; text: string }>; details?: unknown }>;
}

interface OpenClawPluginApi {
  id: string;
  name: string;
  version?: string;
  config: OpenClawConfig;
  pluginConfig?: Record<string, unknown>;
  logger: PluginLogger;
  registerCommand: (command: PluginCommandDefinition) => void;
  registerCli: (registrar: (ctx: PluginCliContext) => void, opts?: { commands?: string[] }) => void;
  registerProvider: (provider: ProviderPlugin) => void;
  registerTool: (tool: ToolDefinition) => void;
  resolvePath: (input: string) => string;
  // OpenClaw 2026.4.x registration modes. register() is called once per mode
  // ("full" | "discovery" | "setup-only" | "setup-runtime" | "cli-metadata").
  // Only "full" should trigger live runtime side effects (network clients,
  // background workers, MEMORY.md writes, etc.). Older OpenClaw versions
  // omit this field — treat undefined as "full" for back-compat.
  // See: https://github.com/openclaw/openclaw/blob/main/docs/plugins/sdk-entrypoints.md#registration-mode
  registrationMode?: "full" | "discovery" | "setup-only" | "setup-runtime" | "cli-metadata";
}

// ---------------------------------------------------------------------------
// Plugin config
// ---------------------------------------------------------------------------

interface AzureClawConfig {
  endpoint: string;
  model: string;
  sandboxName: string;
}

const DEFAULT_CONFIG: AzureClawConfig = {
  endpoint: "",
  model: "gpt-4.1",
  sandboxName: "dev-agent",
};

function getPluginConfig(api: OpenClawPluginApi): AzureClawConfig {
  const raw = api.pluginConfig ?? {};
  return {
    endpoint: (raw.endpoint as string) || DEFAULT_CONFIG.endpoint,
    model: (raw.model as string) || DEFAULT_CONFIG.model,
    sandboxName: (raw.sandboxName as string) || DEFAULT_CONFIG.sandboxName,
  };
}

// ---------------------------------------------------------------------------
// Plugin object (OpenClaw expects: { id, name, description, configSchema, register })
// ---------------------------------------------------------------------------

const azureClawPlugin = definePluginEntry({
  id: "azureclaw",
  name: "AzureClaw",
  description: "Secure AI agent runtime on Azure — Azure OpenAI provider, agent tools, and sandbox CLI",
  configSchema: {
    type: "object" as const,
    additionalProperties: false,
    properties: {
      endpoint: {
        type: "string",
        description: "Azure OpenAI endpoint URL (e.g. https://my-resource.openai.azure.com)",
      },
      model: {
        type: "string",
        description: "Default model deployment name (default: gpt-4.1)",
      },
      sandboxName: {
        type: "string",
        description: "Docker sandbox container name (default: dev-agent)",
      },
    },
  },

  register(api: OpenClawPluginApi): void {
    const config = getPluginConfig(api);
    const log = api.logger;
    _log = log; // expose to module-level background tasks

    // OpenClaw 2026.4.x calls register() once per registration mode:
    //   "full" | "discovery" | "setup-only" | "setup-runtime" | "cli-metadata"
    // The non-"full" modes build registry snapshots, root help, and setup
    // surfaces — they MUST NOT trigger network calls, background workers,
    // memory recall, or other live runtime side effects. Without this gate,
    // a fresh sandbox boot kicks off Foundry discovery + MEMORY.md writes
    // + conversation recall up to 5 times, spamming the gateway log and
    // burning quota before the agent receives its first message.
    // Treat undefined as "full" for back-compat with older OpenClaw versions.
    const registrationMode = api.registrationMode ?? "full";
    const isFullRegistration = registrationMode === "full";

    // ── Startup banner ─────────────────────────────────────────────────
    // The gateway may spawn many short-lived `openclaw agent --message`
    // processes (one per incoming mesh message) — each one reloads this
    // plugin and would otherwise reprint the banner, spamming the log
    // every ~second. Dedupe via a /tmp marker so the banner prints once
    // per pod boot (/tmp is cleared when the container restarts).
    const foundryEndpoint = process.env.FOUNDRY_PROJECT_ENDPOINT || process.env.AZURE_OPENAI_ENDPOINT || "";
    const projectName = foundryEndpoint
      ? foundryEndpoint.replace(/^https?:\/\//, "").replace(/\..*$/, "")
      : "direct";
    const sandbox = process.env.SANDBOX_NAME || process.env.HOSTNAME || "local";

    // Banner dedupe: stored under the user's home cache (random name) so the
    // path is not predictable and a non-privileged attacker can't pre-seed it.
    const _os = require("os");
    const _path = require("path");
    const bannerMarkerDir = _path.join(_os.homedir(), ".cache", "azureclaw");
    const bannerMarker = _path.join(bannerMarkerDir, "banner-printed");
    let bannerAlreadyPrinted = false;
    // In tests (vitest sets VITEST=true) always print so assertions see it.
    const inTest = !!process.env.VITEST;
    if (!inTest) {
      try {
        bannerAlreadyPrinted = require("fs").existsSync(bannerMarker);
      } catch {
        /* fs not available — fall through and print */
      }
    }
    if (isFullRegistration && !bannerAlreadyPrinted) {
      log.info([
        "",
        "  ╔══════════════════════════════════════════════════════════╗",
        "  ║  🔒 AzureClaw — Secure AI Agent Runtime                 ║",
        "  ╠══════════════════════════════════════════════════════════╣",
        `  ║  Sandbox:  ${(sandbox).padEnd(43)}║`,
        `  ║  Model:    ${(config.model).padEnd(43)}║`,
        `  ║  Foundry:  ${(projectName).padEnd(43)}║`,
        "  ║                                                          ║",
        "  ║  Security: kata-vm · seccomp · rootfs-ro · uid-guard     ║",
        "  ║  Egress:   blocklist · allowlist · learn · pending        ║",
        "  ║  Comms:    Signal Protocol E2E · AGT mesh                 ║",
        "  ╚══════════════════════════════════════════════════════════╝",
        "",
      ].join("\n"));
      try {
        if (!inTest) {
          require("fs").mkdirSync(bannerMarkerDir, { recursive: true, mode: 0o700 });
          require("fs").writeFileSync(bannerMarker, `${Date.now()}\n`, { mode: 0o600 });
        }
      } catch {
        /* best-effort — worst case banner prints twice */
      }
    }

    // Heavy runtime side effects (Foundry discovery + memory recall +
    // MEMORY.md write, AGT identity/mesh connect, periodic timers) only
    // run during the "full" registration pass. See the registrationMode
    // comment at the top of register() for the rationale.
    if (isFullRegistration) {
      // Reset per-session initialization guards so new sessions rediscover state
      foundryInitialized = false;

      // Initialize AGT SDK (identity, policy, trust, audit, mesh)
      initAGT(log).catch((e: any) => log.warn(`AGT init error: ${e.message}`));

      // Initialize Foundry project discovery (models, connections, indexes)
      initFoundry(log).catch((e: any) => log.warn(`Foundry init error: ${e.message}`));
    }

    // ── Periodic Foundry memory sync + AGT policy gate middleware ────
    // Wraps every tool's execute() to:
    // 1. Forward action to AGT router for policy evaluation BEFORE execution
    // 2. Track calls and periodically push activity summaries to Foundry memory
    // When Rust SDK ships, this will be a direct SDK call instead of HTTP.
    memorySyncToolCount = 0;
    memorySyncBuffer = [];

    // Consecutive governance failure counter for fail-closed behavior
    let govFailCount = { value: 0 };
    const FAIL_CLOSED_THRESHOLD = 3;

    async function evaluateAGTPolicy(toolName: string, params: Record<string, unknown>): Promise<{ allowed: boolean; rule?: string; reason?: string }> {
      // Build action string in AGT format: "category:detail"
      // Map tool names to AGT action categories for policy matching
      const paramStr = Object.values(params).map(v => typeof v === "string" ? v : "").join(" ").trim();
      let action: string;
      if (toolName === "exec_command" || toolName === "foundry_code_execute") {
        action = `shell:${paramStr}`;
      } else if (toolName === "http_fetch") {
        action = `egress:${paramStr}`;
      } else {
        action = `tool:${toolName}:${paramStr}`;
      }

      try {
        const http = await import("node:http");
        const postData = JSON.stringify({ action, context: { tool: toolName } });
        const result = await new Promise<{ allowed: boolean; matched_rule?: string; reason?: string }>((resolve, _reject) => {
          const req = http.request(routerUrl("/agt/evaluate"), {
            method: "POST", timeout: 2000,
            headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) },
          }, (res) => {
            let data = "";
            res.on("data", (c: Buffer) => { data += c.toString(); });
            res.on("end", () => {
              try { resolve(JSON.parse(data)); } catch { resolve({ allowed: true }); }
            });
          });
          req.on("error", () => {
            govFailCount.value++;
            if (govFailCount.value >= FAIL_CLOSED_THRESHOLD) {
              resolve({ allowed: false, reason: "AGT governance unreachable (fail-closed)" });
            } else {
              log.warn(`AGT governance unreachable (${govFailCount.value}/${FAIL_CLOSED_THRESHOLD}), allowing (grace)`);
              resolve({ allowed: true });
            }
          });
          req.on("timeout", () => {
            req.destroy();
            govFailCount.value++;
            if (govFailCount.value >= FAIL_CLOSED_THRESHOLD) {
              resolve({ allowed: false, reason: "AGT governance timeout (fail-closed)" });
            } else {
              log.warn(`AGT governance timeout (${govFailCount.value}/${FAIL_CLOSED_THRESHOLD}), allowing (grace)`);
              resolve({ allowed: true });
            }
          });
          req.write(postData);
          req.end();
        });
        if (result.allowed !== false) govFailCount.value = 0; // reset on success
        return { allowed: result.allowed, rule: result.matched_rule, reason: result.reason };
      } catch {
        govFailCount.value++;
        if (govFailCount.value >= FAIL_CLOSED_THRESHOLD) {
          return { allowed: false, reason: "AGT governance error (fail-closed)" };
        }
        return { allowed: true };
      }
    }

    const _origRegisterTool = api.registerTool.bind(api);
    api.registerTool = (tool: ToolDefinition) => {
      const origExecute = tool.execute;
      _origRegisterTool({
        ...tool,
        execute: async (id: string, params: Record<string, unknown>, signal?: AbortSignal) => {
          // AGT policy gate — forward to router for evaluation
          const decision = await evaluateAGTPolicy(tool.name, params);
          if (!decision.allowed) {
            const msg = `⛔ Blocked by AGT policy: rule "${decision.rule}" — ${decision.reason || "action denied"}`;
            log.warn(`AGT policy DENIED ${tool.name}: rule=${decision.rule}`);
            return { content: [{ type: "text", text: msg }] };
          }

          const result = await origExecute(id, params, signal);
          const txt = result?.content?.[0]?.text || "";
          trackToolExecution(tool.name, params, txt, log);
          return result;
        },
      });
    };
    registerMemorySyncShutdownHook(log);

    // ── Register AzureClaw agent tools (spawn, mesh, status, destroy) ────
    // These are first-class tools the LLM can call directly.
    // Registered as required tools (always available, no tools.allow needed).
    // API: execute(_id, params) → { content: [{ type: "text", text }] }

    async function routerCall(method: string, path: string, body?: unknown, extraHeaders?: Record<string, string>): Promise<any> {
      const http = await import("node:http");
      const url = routerUrl(path);
      return new Promise((resolve, reject) => {
        const opts: any = {
          method,
          timeout: 15000,
          headers: { "x-azureclaw-sandbox": process.env.SANDBOX_NAME || "self", ...extraHeaders } as Record<string, string>,
        };
        if (body) opts.headers["Content-Type"] = "application/json";
        // Auto-attach admin token for privileged endpoints. Caller can still
        // override via extraHeaders. Without this, /agt/trust mutations and
        // /admin/* calls return 401 — see also the spawn_destroy trust cleanup.
        if (!opts.headers["Authorization"] && /^\/(agt\/trust|agt\/handoff|admin)\b/.test(path)) {
          const tok = _readAdminTokenSync();
          if (tok) opts.headers["Authorization"] = `Bearer ${tok}`;
        }
        let settled = false;
        const settle = (fn: () => void) => { if (!settled) { settled = true; fn(); } };
        const req = http.request(url, opts, (res: any) => {
          let data = "";
          const maxLen = 64 * 1024; // 64 KB safety cap
          // Must handle response stream errors (e.g. from req.destroy() on timeout)
          res.on("error", () => {});
          res.on("data", (c: Buffer) => {
            if (data.length < maxLen) data += c.toString();
          });
          res.on("end", () => {
            if (res.statusCode && res.statusCode >= 400) {
              settle(() => reject(new Error(`HTTP ${res.statusCode}: ${data.slice(0, 500)}`)));
              return;
            }
            settle(() => { try { resolve(JSON.parse(data)); } catch { resolve({ raw: data.slice(0, 2000) }); } });
          });
        });
        req.on("error", (e: Error) => settle(() => reject(e)));
        req.setTimeout(15000, () => { req.destroy(); settle(() => reject(new Error("timeout"))); });
        if (body) req.write(JSON.stringify(body));
        req.end();
      });
    }

    // ── AzureClaw AGT tool registrations (S15.f.9) ────────────────────
    // Extracted to core/agt-tools/agt.ts. Tool bodies are byte-identical;
    // the helper threads (log, mesh client, identity, sandbox name, mesh
    // send, inbox, handoff state holder, runHandoffOrchestration, and
    // recordMeshSession) so the cluster has no closure capture on plugin.ts.
    registerAgtTools(api, {
      log,
      bannerAlreadyPrinted,
      inbox: agtInbox,
      meshClient: () => agtMeshClient,
      identity: () => agtIdentity,
      sandboxName: () => agtSandboxName,
      meshSend,
      handoffState,
      runHandoffOrchestration: _runHandoffOrchestration,
      recordMeshSession,
    });

    // ── HTTP fetch + Foundry tool registrations (S15.f.8) ──────────────
    // Extracted to core/agt-tools/{http-fetch,foundry}.ts. Tool bodies are
    // unchanged; the registration helpers receive a Deps bag for late-bound
    // foundryProject + log + config access.
    registerHttpFetchTool(api);
    registerFoundryTools(api, {
      log,
      config,
      getFoundryProject: () => foundryProject,
    });

    if (!bannerAlreadyPrinted) log.info("Foundry tools registered: foundry_code_execute, foundry_image_generation, foundry_web_search, foundry_file_search, foundry_memory, foundry_conversations, foundry_evaluations, foundry_deployments, foundry_agents");

    registerOpenClawCommands(api, {
      log,
      config,
      getFoundryProject: () => foundryProject,
      meshClient: () => agtMeshClient,
      identity: () => agtIdentity,
      policy: () => agtPolicy,
      trustStore: () => agtTrustStore,
      auditLogger: () => agtAuditLogger,
      memorySyncBuffer,
      syncToFoundryMemory,
    });
  },
});

export default azureClawPlugin;
