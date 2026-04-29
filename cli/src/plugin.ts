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
interface HandoffProgress {
  phase: string;
  status: "running" | "complete" | "error" | "partial";
  steps: string[];
  direction?: string;
  started_at: string;
  updated_at: string;
  error?: string;
  result?: Record<string, unknown>;
}
let handoffProgress: HandoffProgress | null = null;

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

// Helper: update handoff progress tracker
function _hp(phase: string, step: string) {
  if (!handoffProgress) return;
  handoffProgress.phase = phase;
  handoffProgress.steps.push(step);
  handoffProgress.updated_at = new Date().toISOString();
  _log.info(`Handoff [${phase}]: ${step}`);
}

// Background handoff orchestration — runs async after handoff_confirm returns.
// Progress is tracked in handoffProgress, polled by handoff_status tool.
async function _runHandoffOrchestration(
  handoffToken: string, adminToken: string, direction: string, dirLabel: string,
) {
 try {
  const authH: Record<string, string> = { Authorization: `Bearer ${adminToken}` };
  const handoffH: Record<string, string> = { ...authH, "X-Handoff-Token": handoffToken };

  // ── Step 1: Create encrypted state snapshot ──
  _hp("snapshot", "📦 Creating encrypted state snapshot...");
  const cryptoMod = await import("node:crypto");
  const sharedSecret = cryptoMod
    .createHash("sha256")
    .update(`${adminToken}:${handoffToken}`)
    .digest("base64");

  // Collect workspace files and recent conversation context for the snapshot
  const snapshotPayload: Record<string, unknown> = { shared_secret: sharedSecret };

  // Pack workspace files (/sandbox/) into a tar.gz for transfer
  try {
    const { execSync } = await import("node:child_process");
    // Tar key workspace files (skip large/transient dirs)
    const tarB64 = execSync(
      "tar czf - -C /sandbox " +
      "--exclude='.openclaw/extensions/*/dist' --exclude='.openclaw/extensions/*/node_modules' " +
      "--exclude='node_modules' --exclude='.git' " +
      "--exclude='*.pyc' --exclude='__pycache__' " +
      ".openclaw/workspace .openclaw/openclaw.json .openclaw/cron " +
      ".openclaw/policies .openclaw/agents 2>/dev/null | base64 -w0",
      { maxBuffer: 50 * 1024 * 1024, timeout: 10000 },
    ).toString("utf-8").trim();
    if (tarB64.length > 0 && tarB64.length < 50 * 1024 * 1024) {
      snapshotPayload.workspace_tar = tarB64;
      _log.info(`Handoff: packed workspace (${(tarB64.length / 1024).toFixed(1)} KB base64)`);
    }
  } catch { /* workspace tar is best-effort */ }

  // Collect recent Foundry Memory as conversation context
  try {
    const agentName = process.env.SANDBOX_NAME || "dev-agent";
    const store = `memory-${agentName}`;
    const apiVer = "api-version=2025-11-15-preview";
    const memResp = await _routerCall("POST", `/memory_stores/${store}:search_memories?${apiVer}`, {
      scope: agentName,
      options: { max_memories: 20 },
    }).catch(() => null);
    if (memResp?.memories?.length) {
      const chatContext = memResp.memories.map((m: any) => ({
        role: "assistant",
        content: m.memory_item?.content || m.content || m.text || JSON.stringify(m),
        timestamp: m.created_at || new Date().toISOString(),
      }));
      snapshotPayload.chat_snapshot = Buffer.from(JSON.stringify(chatContext)).toString("base64");
      _log.info(`Handoff: included ${chatContext.length} memory items as chat context`);
    }
  } catch { /* memory search is best-effort */ }

  // Include credential refs (what channels/plugins are configured, not the secrets)
  const credRefs: Array<{ name: string; env_key: string }> = [];
  for (const [envKey, label] of [
    ["TELEGRAM_BOT_TOKEN", "telegram"], ["SLACK_BOT_TOKEN", "slack"],
    ["DISCORD_BOT_TOKEN", "discord"], ["BRAVE_API_KEY", "brave"],
    ["TAVILY_API_KEY", "tavily"],
  ] as const) {
    if (process.env[envKey]) credRefs.push({ name: label, env_key: envKey });
  }
  if (credRefs.length > 0) snapshotPayload.credentials = credRefs;

  // Collect sub-agent snapshots (best-effort)
  try {
    const subResp = await _routerCall("GET", "/agt/handoff/sub-agents", undefined, 10000, authH);
    if (subResp?.count > 0 && Array.isArray(subResp.sub_agent_snapshots)) {
      const subSnaps = subResp.sub_agent_snapshots as Array<{
        name: string; workspace_tar: string; [k: string]: unknown;
      }>;
      _hp("snapshot", `🤖 Found ${subSnaps.length} sub-agent(s) — collecting state...`);

      // Request workspace from each sub-agent via E2E mesh
      // Protocol: interrupt → wait for ack/save → collect workspace
      if (agtMeshClient && agtIdentity) {
        // Phase 1: Send handoff:interrupt to ALL sub-agents concurrently
        // so they can save progress while we continue setup
        const subAmidMap = new Map<string, string>(); // name → amid
        for (const snap of subSnaps) {
          try {
            const regResp = await _routerCall("GET",
              `/agt/registry/registry/search?capability=${encodeURIComponent(snap.name)}`,
              undefined, 5000, authH);
            const candidates = (regResp?.results || []).filter(
              (a: any) => a.display_name === snap.name && a.status === "online"
            );
            if (candidates.length === 0) continue;

            const subAmid = candidates[0].amid;
            snap.original_amid = subAmid;
            subAmidMap.set(snap.name, subAmid);

            // Signal sub-agent to save in-progress work
            await agtMeshClient.send(subAmid, {
              type: "handoff:interrupt",
              reason: "parent_handoff",
              from_agent: process.env.SANDBOX_NAME || "unknown",
              timestamp: new Date().toISOString(),
            });
            _log.info(`🛑 Sent handoff:interrupt to sub-agent '${snap.name}'`);
          } catch (lookupErr: any) {
            _log.warn(`Sub-agent '${snap.name}' lookup/interrupt failed: ${lookupErr.message}`);
          }
        }

        // Brief pause for sub-agents to checkpoint (they save between LLM rounds)
        if (subAmidMap.size > 0) {
          _log.info(`⏳ Waiting for ${subAmidMap.size} sub-agent(s) to save progress...`);
          // Wait up to 10s for interrupt_ack from sub-agents (best-effort)
          const ackStart = Date.now();
          const acksReceived = new Set<string>();
          while (Date.now() - ackStart < 10_000 && acksReceived.size < subAmidMap.size) {
            for (let i = agtInbox.length - 1; i >= 0; i--) {
              const m = agtInbox[i];
              if (m.message_type === "handoff:interrupt_ack" ||
                  (typeof m.content === "string" && m.content.includes("handoff:interrupt_ack"))) {
                acksReceived.add(m.from_amid);
                agtInbox.splice(i, 1);
              }
            }
            if (acksReceived.size < subAmidMap.size) {
              await new Promise(r => setTimeout(r, 500));
            }
          }
          _hp("snapshot", `🛑 ${acksReceived.size}/${subAmidMap.size} sub-agent(s) interrupted and checkpointed`);
        }

        // Phase 2: Collect workspaces (sub-agents have now saved progress)
        let collectedCount = 0;
        for (const snap of subSnaps) {
          const subAmid = subAmidMap.get(snap.name);
          if (!subAmid) continue;

          try {
            // Send workspace request via mesh
            await agtMeshClient.send(subAmid, {
              type: "handoff:workspace_request",
              from_agent: process.env.SANDBOX_NAME || "unknown",
              timestamp: new Date().toISOString(),
            });

            // Wait for workspace response. The transport layer (meshSend + onMessage)
            // handles chunking transparently — we just wait for a single
            // handoff:workspace_response message in the inbox.
            const wsStart = Date.now();
            const WS_TIMEOUT = 60_000; // 60s — large workspaces may take time to chunk

            while (Date.now() - wsStart < WS_TIMEOUT) {
              const idx = agtInbox.findIndex((m) =>
                m.from_amid === subAmid &&
                (m.message_type === "handoff:workspace_response" ||
                 (typeof m.content === "string" && m.content.includes("handoff:workspace_response")))
              );
              if (idx >= 0) {
                const reply = agtInbox.splice(idx, 1)[0];
                let parsed: any;
                if (typeof reply.content === "string") {
                  try { parsed = JSON.parse(reply.content); } catch { parsed = reply.content; }
                } else {
                  parsed = reply.content;
                }
                if (parsed?.workspace_tar && parsed.workspace_tar.length > 0) {
                  snap.workspace_tar = parsed.workspace_tar;
                  collectedCount++;
                  _log.info(`📦 Got workspace from sub-agent '${snap.name}' (${(parsed.size_bytes / 1024).toFixed(1)} KB)`);
                } else if (parsed?.error) {
                  _log.warn(`Sub-agent '${snap.name}' workspace error: ${parsed.error}`);
                }
                break;
              }
              await new Promise(r => setTimeout(r, 500));
            }
          } catch (subWsErr: any) {
            _log.warn(`Workspace collection from sub-agent '${snap.name}' failed: ${subWsErr.message}`);
          }
        }
        if (collectedCount > 0) {
          _hp("snapshot", `📦 Collected ${collectedCount} sub-agent workspace(s)`);
        }
      }

      snapshotPayload.sub_agent_snapshots = subSnaps;
      _hp("snapshot", `🤖 ${subSnaps.length} sub-agent snapshot(s) included in handoff payload`);
    }
  } catch { /* sub-agent collection is best-effort */ }

  const snapshotResp = await _routerCallStrict("POST", "/agt/handoff/snapshot",
    snapshotPayload, 60000, handoffH);

  const snapshotSize = snapshotResp.size_bytes || 0;
  const verificationHash = snapshotResp.verification_hash;
  _hp("snapshot", `📦 Snapshot ready (${(snapshotSize / 1024).toFixed(1)} KB, AES-256-GCM encrypted)`);

  // ── Step 2: Drain ──
  _hp("drain", "⏳ Draining agent — finishing in-flight work...");
  await _routerCall("POST", "/agt/handoff/drain", {}, 30000, handoffH);
  _hp("drain", "⏳ Agent drained — no new work accepted");

  // ── Step 3: Spawn cloud target ──
  const myName = process.env.SANDBOX_NAME || "unknown";
  const myAmid = agtMeshClient?.getAmid?.() || agtIdentity?.amid;
  let targetName = myName;
  let targetAmid: string | undefined;

  if (direction === "local_to_aks") {
    _hp("spawn", "🚀 Spawning cloud target on AKS...");

    const trustedPeers: string[] = [];
    if (myAmid) trustedPeers.push(`${myName}:${myAmid}`);
    for (const [amid, name] of amidToName.entries()) {
      if (amid !== myAmid) trustedPeers.push(`${name}:${amid}`);
    }

    try {
      await _routerCall("POST", "/sandbox/spawn", {
        agent_id: targetName,
        model: process.env.DEFAULT_MODEL || "gpt-4.1",
        governance: true,
        trust_threshold: 500,
        learn_egress: process.env.EGRESS_LEARN_MODE === "true",
        trusted_peers: trustedPeers.length > 0 ? trustedPeers.join(",") : undefined,
        handoff: { mode: "restore", predecessor: myName },
      });
      _hp("spawn", "🚀 CRD created — waiting for pod to start...");
    } catch (spawnErr: any) {
      if (!spawnErr.message?.includes("already exists")) throw spawnErr;
      _hp("spawn", "🚀 Target already exists — reusing");
    }

    // Wait for target to register in mesh (up to 90s)
    _hp("mesh_wait", "🔍 Waiting for cloud target to join the mesh...");
    const spawnStart = Date.now();
    while (Date.now() - spawnStart < 90_000) {
      await new Promise(r => setTimeout(r, 2000));
      try {
        const searchResult = await _routerCall("GET",
          `/agt/registry/registry/search?capability=${encodeURIComponent(targetName)}`);
        const agents = searchResult?.results || [];
        const match = agents.find((a: any) =>
          a.amid !== myAmid && (a.display_name === targetName || a.capabilities?.includes(targetName))
        );
        if (match?.amid) {
          targetAmid = match.amid;
          nameToAmid.set(targetName, match.amid);
          amidToName.set(match.amid, targetName);
          break;
        }
      } catch { /* not registered yet */ }
      const elapsed = Math.round((Date.now() - spawnStart) / 1000);
      if (elapsed % 15 === 0) {
        _hp("mesh_wait", `🔍 Target pod starting... (${elapsed}s)`);
      }
    }

    if (!targetAmid) {
      _hp("mesh_wait", "⚠️ Target spawned but not yet registered on mesh — transfer deferred");
      await _routerCall("POST", "/agt/handoff/abort", {}, 15000, handoffH).catch(() => {});
      // Clean up orphaned CRD to avoid stale pods on AKS
      if (direction === "local_to_aks") {
        _hp("cleanup", "🧹 Cleaning up orphaned cloud target...");
        await _routerCall("DELETE", `/sandbox/${encodeURIComponent(targetName)}`, {}, 15000).catch(() => {});
      }
      if (handoffProgress) {
        handoffProgress.status = "partial";
        handoffProgress.error = "Cloud target did not register on mesh within 90s";
      }
      return;
    }

    _hp("mesh_wait", `🌐 Cloud target online (AMID: ${targetAmid.slice(0, 12)}...)`);

  } else {
    // aks_to_local: discover existing local target
    // The CLI wakes the dormant Docker container before initiating the reverse
    // handoff, so the local agent may still be starting. Retry with backoff.
    _hp("discover", "🏠 Discovering local target agent...");
    const discoverStart = Date.now();
    const DISCOVER_TIMEOUT = 60_000; // 60s — local agent may be waking up

    while (Date.now() - discoverStart < DISCOVER_TIMEOUT) {
      try {
        const searchResult = await _routerCall("GET",
          `/agt/registry/registry/search?capability=${encodeURIComponent(targetName)}`);
        const agents = searchResult?.results || [];
        const match = agents.find((a: any) =>
          a.amid !== myAmid && (a.display_name === targetName || a.capabilities?.includes(targetName))
        );
        if (match?.amid) {
          targetAmid = match.amid;
          nameToAmid.set(targetName, match.amid);
          amidToName.set(match.amid, targetName);
          break;
        }
      } catch { /* registry error */ }

      const elapsed = Math.round((Date.now() - discoverStart) / 1000);
      if (elapsed % 10 === 0 && elapsed > 0) {
        _hp("discover", `🏠 Waiting for local target to come online... (${elapsed}s)`);
      }
      await new Promise(r => setTimeout(r, 2000));
    }

    if (!targetAmid) {
      await _routerCall("POST", "/agt/handoff/abort", {}, 15000, handoffH).catch(() => {});
      if (handoffProgress) {
        handoffProgress.status = "error";
        handoffProgress.phase = "error";
        handoffProgress.error = "Local target agent not found in mesh registry after 60s. " +
          "Ensure the local agent is running: azureclaw dev <name>";
        handoffProgress.steps.push("❌ Local target not found — is the local agent running?");
      }
      return;
    }
    _hp("discover", `🏠 Local target found (AMID: ${targetAmid.slice(0, 12)}...)`);
  }

  // ── Step 4: Transfer state via E2E mesh ──
  if (!agtMeshClient || !agtIdentity) {
    await _routerCall("POST", "/agt/handoff/abort", {}, 15000, handoffH).catch(() => {});
    if (handoffProgress) {
      handoffProgress.status = "error";
      handoffProgress.phase = "error";
      handoffProgress.error = "Mesh client not connected";
      handoffProgress.steps.push("❌ Mesh client not connected — cannot transfer");
    }
    return;
  }

  _hp("transfer", "🔐 Sending encrypted state via E2E mesh (Signal Protocol)...");

  // Use meshSend for auto-chunking — transparently handles blobs of any size
  // up to ~40MB. The receiver's onMessage handler reassembles before processing.
  const handoffMessage = {
    type: "handoff_transfer",
    blob: snapshotResp.blob,
    shared_secret: sharedSecret,
    verification_hash: verificationHash,
    from_agent: myName,
    predecessor_amid: myAmid,
    direction,
    timestamp: new Date().toISOString(),
  };

  let sendSuccess = false;
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      await meshSend(agtMeshClient, targetAmid, handoffMessage, _log);
      sendSuccess = true;
      break;
    } catch (sendErr: any) {
      _log.warn(`Handoff mesh send attempt ${attempt + 1}/5 failed: ${sendErr.message}`);
      if (attempt < 4) {
        _hp("transfer", `🔐 Retrying mesh send (${attempt + 2}/5)...`);
        await new Promise(r => setTimeout(r, 2000));
      }
    }
  }

  if (!sendSuccess) {
    _hp("transfer", "❌ Mesh send failed after retries");
    await _routerCall("POST", "/agt/handoff/abort", {}, 15000, handoffH).catch(() => {});
    if (direction === "local_to_aks") {
      _hp("cleanup", "🧹 Cleaning up orphaned cloud target...");
      await _routerCall("DELETE", `/sandbox/${encodeURIComponent(targetName)}`, {}, 15000).catch(() => {});
    }
    if (handoffProgress) { handoffProgress.status = "error"; }
    return;
  }

  _hp("transfer", "📤 Encrypted state sent — waiting for target to verify...");

  // ── Step 5: Wait for verification ──
  _hp("verify", "🔍 Waiting for verification from cloud target...");
  const verifyStart = Date.now();
  let verifyResult: any = null;
  let lastResendAt = Date.now();

  while (Date.now() - verifyStart < 180_000) {
    for (const checkType of ["handoff_verification"] as const) {
      const idx = agtInbox.findIndex(m => {
        if (m.from_amid !== targetAmid || m.from_agent !== targetName) return false;
        if (m.message_type === checkType) return true;
        try {
          const c = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
          return c?.type === checkType;
        } catch { return false; }
      });
      if (idx >= 0) {
        const msg = agtInbox.splice(idx, 1)[0];
        verifyResult = typeof msg.content === "string"
          ? (() => { try { return JSON.parse(msg.content); } catch { return msg.content; } })()
          : msg.content;
        break;
      }
    }
    if (verifyResult) break;
    const elapsed = Math.round((Date.now() - verifyStart) / 1000);

    // Re-send every 30s — target's handler may not have been wired up yet
    if (Date.now() - lastResendAt >= 30_000) {
      lastResendAt = Date.now();
      _hp("verify", `🔁 Re-sending state to target... (${elapsed}s)`);
      try {
        await meshSend(agtMeshClient, targetAmid, handoffMessage, _log);
      } catch (resendErr: any) {
        _log.warn(`Handoff re-send failed: ${resendErr.message}`);
      }
    } else if (elapsed % 15 === 0 && elapsed > 0) {
      _hp("verify", `🔍 Target restoring state... (${elapsed}s)`);
    }
    await new Promise(r => setTimeout(r, 500));
  }

  if (!verifyResult) {
    _hp("verify", "⚠️ Verification timeout (180s) — target may still be restoring");
    if (handoffProgress) { handoffProgress.status = "partial"; handoffProgress.error = "Verification timeout (180s)"; }
    return;
  }

  if (verifyResult.error || verifyResult.matches === false) {
    _hp("verify", `❌ Verification failed: ${verifyResult.error || "hash mismatch"}`);
    await _routerCall("POST", "/agt/handoff/abort", {}, 15000, handoffH).catch(() => {});
    if (handoffProgress) { handoffProgress.status = "error"; handoffProgress.error = "Verification failed"; }
    return;
  }

  const successorAmid = verifyResult.successor_amid;
  _hp("verify", "✅ State verified — integrity hash match confirmed");

  // ── Step 6: Identity succession ──
  if (myAmid && successorAmid) {
    _hp("succession", "🔗 Registering identity succession...");
    try {
      // Router signs with its private key and submits to registry
      await _routerCall("POST", "/agt/handoff/succession", {
        successor_amid: successorAmid,
        reason: `handoff:${direction}`,
      }, 15000, authH);
      _hp("succession", `🔗 Identity transferred: ${myAmid.slice(0, 12)}... → ${successorAmid.slice(0, 12)}...`);
    } catch (succErr: any) {
      _hp("succession", `⚠️ Identity succession pending: ${succErr.message}`);
    }
  }

  // ── Step 7: Destroy source sub-agents + Decommission ──
  const decommLabel = direction === "local_to_aks" ? "local" : "cloud";
  _hp("decommission", `🏁 Cleaning up ${decommLabel} sub-agents and decommissioning...`);

  // Destroy source sub-agents — they've been re-spawned on the target
  try {
    const listResp = await _routerCall("GET", "/sandbox/list", undefined, 10000, authH);
    const subList = listResp?.sandboxes || [];
    if (subList.length > 0) {
      _hp("decommission", `🧹 Destroying ${subList.length} source sub-agent(s)...`);
      for (const sub of subList) {
        const subName = sub.name || sub;
        try {
          await _routerCall("DELETE", `/sandbox/${encodeURIComponent(subName)}`, {}, 10000, authH);
          _log.info(`🧹 Destroyed source sub-agent '${subName}'`);
        } catch { /* best-effort */ }
      }
      _hp("decommission", `🧹 ${subList.length} source sub-agent(s) cleaned up`);
    }
  } catch { /* sub-agent cleanup is best-effort */ }

  try {
    await _routerCall("POST", "/agt/handoff/decommission", {}, 15000, handoffH);
    _hp("decommission", direction === "local_to_aks"
      ? "🏁 Local agent decommissioned (dormant — keys preserved)"
      : "🏁 Cloud agent decommissioned");
  } catch (decommErr: any) {
    _hp("decommission", `⚠️ Decommission pending: ${decommErr.message}`);
  }

  // ── Done! ──
  _hp("complete", "");
  _hp("complete", `🎉 Handoff complete! Agent is now running on ${dirLabel}.`);
  if (!handoffProgress) return;
  const agentName = process.env.SANDBOX_NAME || "dev-agent";
  if (direction === "local_to_aks") {
    handoffProgress.steps.push("The cloud agent has your full state — chat history, trust scores, audit trail.");
    handoffProgress.steps.push("Your local keys are preserved. You can reclaim with a reverse handoff anytime.");
    handoffProgress.steps.push("");
    // Connection instructions — explicit --cloud since local container still exists
    handoffProgress.steps.push(`📡 Connect to cloud agent: azureclaw connect ${agentName} --cloud`);
    handoffProgress.steps.push(`📊 Monitor: azureclaw operator`);
    // Telegram note
    if (process.env.TELEGRAM_BOT_TOKEN) {
      handoffProgress.steps.push(`📱 Telegram: Your bot is now handled by the cloud agent. Chat continues automatically.`);
    }
    handoffProgress.steps.push(`💤 This local agent is now dormant. It will show as 'Dormant' in the operator TUI.`);
    handoffProgress.steps.push(`🗑️  Clean up local: azureclaw destroy ${agentName} --local`);
  } else {
    handoffProgress.steps.push("Your local agent has the full state — chat history, trust scores, audit trail.");
    handoffProgress.steps.push("The cloud agent has been decommissioned and scaled to zero.");
    handoffProgress.steps.push("");
    handoffProgress.steps.push(`📡 Connect to local agent: azureclaw connect ${agentName} --local`);
    handoffProgress.steps.push(`📊 Monitor: azureclaw operator`);
    if (process.env.TELEGRAM_BOT_TOKEN) {
      handoffProgress.steps.push(`📱 Telegram: Your bot is now handled by the local agent.`);
    }
    handoffProgress.steps.push(`☁️  Cloud sandbox scaled to 0 (CRD preserved — re-handoff to cloud is instant).`);
  }
  handoffProgress.status = "complete";
  const subAgentCount = (snapshotPayload.sub_agent_snapshots as unknown[] | undefined)?.length || 0;
  handoffProgress.result = {
    direction,
    snapshot_size_kb: (snapshotSize / 1024).toFixed(1),
    predecessor_amid: myAmid,
    successor_amid: successorAmid,
    verification: "passed",
    sub_agents_transferred: subAgentCount,
  };
  handoffProgress.updated_at = new Date().toISOString();

 } catch (err: any) {
    _log.warn(`Handoff orchestration failed: ${err.message}`);
    if (handoffProgress) {
      handoffProgress.status = "error";
      handoffProgress.phase = "error";
      handoffProgress.error = err.message;
      handoffProgress.steps.push(`❌ ${err.message}`);
      handoffProgress.updated_at = new Date().toISOString();
    }
  }
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

    // Terminal sub-agent pod phases — polling loops bail out when the pod reaches
    // one of these. Anything else (including Pending, Running, Unknown, or transient
    // status-endpoint errors) is treated as "still alive, keep retrying".
    const POD_DEAD_PHASES = new Set(["Failed", "Terminating", "Exited"]);

    // Probe sub-agent pod phase. Returns:
    //   { alive: true,  phase }            → keep polling
    //   { alive: false, phase?, reason }   → bail; pod is gone/dead
    //   null                               → transient error (router down, brief 5xx);
    //                                         treat as still alive, keep polling
    async function probeSubAgentAlive(
      name: string,
    ): Promise<{ alive: boolean; phase?: string; reason?: string } | null> {
      try {
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
        // transient — router busy, short network hiccup; keep retrying
        return null;
      }
    }

    // Safe JSON response for tool output — truncate to avoid blowing WebSocket frames
    function safeJson(obj: unknown, maxLen = 8000): string {
      try {
        const s = JSON.stringify(obj, null, 2);
        return s.length > maxLen ? s.slice(0, maxLen) + "\n...(truncated)" : s;
      } catch {
        return String(obj).slice(0, maxLen);
      }
    }

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
          const myAmid = agtMeshClient?.getAmid?.() || agtIdentity?.amid;
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
            if (!amid && agtMeshClient) {
              const resolved = await resolveAmidByName(agentName, { bypassCache: true });
              if (resolved) {
                amid = resolved;
                log.info(`AGT pre-discovery: '${agentName}' registered (${resolved.slice(0, 12)}..., not cached — send will re-resolve)`);
              }
            }

            // Both ready — exit early
            if (phase === "Running" && (amid || !agtMeshClient)) break;

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

          if (!amid && agtMeshClient) {
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
      description: "Send a task to a sub-agent via AGT mesh (E2E encrypted relay). Sub-agents have isolated filesystems — include any file contents the agent needs directly in the message body. Ask the agent to return its output as text in the reply. You can also instruct sub-agents to forward results to each other directly (they have peer-to-peer mesh access). Automatically retries registry discovery and prekey exchange for as long as the sub-agent pod is alive; aborts only if the pod reaches Failed/Terminating/Exited, the sandbox is deleted, or meshSend returns a non-transient error. Then waits up to 5.5 minutes for the reply. If no reply arrives, check azureclaw_mesh_inbox later.",
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

        // ── Primary path: AGT SDK relay (E2E encrypted) ──
        if (agtMeshClient && agtIdentity) {
          // Ensure we're connected (reconnect if initial connect was deferred)
          if (!agtMeshClient.isConnected) {
            try {
              log.info("AGT relay: reconnecting before send...");
              // Force disconnect first to clear stale "Already connected" state
              try { await agtMeshClient.disconnect(); } catch { /* ignore */ }
              await agtMeshClient.connect({
                displayName: agtSandboxName,
                capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
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
                await meshSend(agtMeshClient, targetAmid, {
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
            const waitMaxMs = 60_000; // 60 seconds — prevents blocking the agent loop too long
            const pollIntervalMs = 500; // 500ms — fast polling for responsive feel
            let replyContent: string | null = null;
            let retriedAfterTimeout = false;

            // eslint-disable-next-line no-constant-condition
            while (true) {
              const replyWaitStart = Date.now();
              log.info(`AGT relay: waiting up to ${waitMaxMs / 1000}s for reply from '${agentName}'...`);

              while (Date.now() - replyWaitStart < waitMaxMs) {
                // Check inbox for a reply from this target, skipping protocol messages
                const replyIdx = agtInbox.findIndex((m) => {
                  if (m.from_amid !== targetAmid && m.from_agent !== agentName) return false;
                  // Skip Signal Protocol handshake messages (ACCEPT, KNOCK, KEY_EXCHANGE)
                  const mt = m.message_type || "";
                  if (mt === "ACCEPT" || mt === "KNOCK" || mt === "KEY_EXCHANGE") return false;
                  // Also check content for JSON protocol messages
                  if (typeof m.content === "string") {
                    try {
                      const parsed = JSON.parse(m.content);
                      if (parsed.type === "ACCEPT" || parsed.type === "KNOCK" || parsed.type === "KEY_EXCHANGE") return false;
                    } catch { /* not JSON, treat as real content */ }
                  }
                  return true;
                });
                if (replyIdx >= 0) {
                  const reply = agtInbox.splice(replyIdx, 1)[0];
                  replyContent = typeof reply.content === "string"
                    ? reply.content
                    : JSON.stringify(reply.content);
                  log.info(`AGT relay: got reply from '${agentName}' after ${((Date.now() - replyWaitStart) / 1000).toFixed(1)}s`);
                  break;
                }
                // Drain protocol messages to keep inbox clean
                for (let i = agtInbox.length - 1; i >= 0; i--) {
                  const m = agtInbox[i];
                  if ((m.from_amid === targetAmid || m.from_agent === agentName) &&
                      (m.message_type === "ACCEPT" || m.message_type === "KNOCK" || m.message_type === "KEY_EXCHANGE")) {
                    agtInbox.splice(i, 1);
                  }
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
              log.warn(`AGT relay: no reply from '${agentName}' within ${waitMaxMs / 1000}s — clearing cache and re-discovering (target may have been recycled during a rollout)`);
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
                await meshSend(agtMeshClient, targetAmid, {
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
              from_amid: agtIdentity.amid,
              protocol: "AGT E2E encrypted (Signal Protocol)",
              message_id: messageId,
            };
            if (replyContent) {
              result.reply = replyContent;
              // Parent rates sub-agent — only meaningful for long-lived sub-agents
              // whose reputation will be queried again. Short-lived ones will die
              // and their score is lost, but the audit trail remains.
              try {
                const ok = await agtMeshClient.submitReputation(targetAmid!, messageId, 0.9, ["fast_response", "reliable"]);
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
      description: "Check your AGT mesh inbox for responses from sub-agents. Returns messages received via the E2E encrypted AGT relay and any router-level messages.",
      parameters: { type: "object", properties: {} },
      async execute(_id: string, _params: Record<string, unknown>) {
        try {
          // Collect messages from both sources
          const agtMessages = agtInbox.splice(0, agtInbox.length); // drain AGT buffer

          // Clear the MEMORY.md inbox notification since we've drained messages
          if (agtMessages.length > 0) {
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

          // Also get any router-level messages (fallback / auto-reply)
          let routerMessages: any[] = [];
          try {
            const routerResult = await routerCall("GET", "/agt/mesh/inbox");
            routerMessages = routerResult.messages || [];
          } catch {
            // Router inbox unavailable — use AGT only
          }

          // Merge and deduplicate (prefer AGT source)
          // Filter out internal protocol messages — only show human-readable content
          const INTERNAL_TYPES = new Set([
            "handoff_transfer", "handoff_verification", "handoff_ready",
            "handoff:interrupt", "handoff:interrupt_ack",
            "handoff:workspace_request", "handoff:workspace_response",
            "handoff:workspace_inject", "handoff:workspace_inject_ack",
            "handoff:resume", "handoff:resume_ack",
            "file_transfer_ack",
          ]);

          const userMessages = agtMessages.filter((m: any) => {
            try {
              const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
              return !INTERNAL_TYPES.has(parsed?.type);
            } catch { return true; } // If can't parse, keep it
          });

          // Auto-decode file_transfer messages so LLM sees readable content
          const decoded = userMessages.map((m: any) => {
            try {
              const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
              if (parsed?.type === "file_transfer" && parsed?.file_data && parsed?.file_name) {
                const buf = Buffer.from(parsed.file_data, "base64");
                const isText = !buf.some((b: number) => b === 0); // null bytes → binary
                return {
                  ...m,
                  source: "agt_relay_e2e",
                  message_type: "file_transfer",
                  file_name: parsed.file_name,
                  file_size_bytes: buf.length,
                  content: isText
                    ? buf.toString("utf-8")
                    // Binary file still carries raw bytes here, meaning the
                    // auto-save handler (plugin.ts ~2152) hasn't overwritten
                    // this entry yet — so we intentionally do NOT claim a
                    // save path. The next mesh_inbox call will reflect the
                    // real `saved_to` once the handler finishes.
                    : `[binary file: ${parsed.file_name}, ${buf.length} bytes — auto-save pending; re-check mesh_inbox for saved_to path]`,
                };
              }
            } catch { /* fall through */ }
            return { ...m, source: "agt_relay_e2e" };
          });

          const allMessages = [
            ...decoded,
            ...routerMessages.map((m: any) => ({ ...m, source: "router_http" })),
          ];

          return { content: [{ type: "text", text: JSON.stringify({
            count: allMessages.length,
            agt_relay_count: decoded.length,
            router_count: routerMessages.length,
            filtered_protocol_messages: agtMessages.length - userMessages.length,
            messages: allMessages,
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

        if (!agtMeshClient || !agtIdentity) {
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

            transferId = await meshSend(agtMeshClient, targetAmid, fileMsg, log);

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
          if (handoffProgress) {
            const sinceStep = typeof params?.since_step === "number" ? params.since_step : 0;
            const allSteps = handoffProgress.steps;
            const newSteps = sinceStep > 0 ? allSteps.slice(sinceStep) : allSteps;
            return { content: [{ type: "text", text: safeJson({
              phase: handoffProgress.phase,
              status: handoffProgress.status,
              direction: handoffProgress.direction,
              active: handoffProgress.status === "running",
              total_steps: allSteps.length,
              new_steps: newSteps,
              error: handoffProgress.error,
              result: handoffProgress.result,
              instruction: handoffProgress.status === "running"
                ? `Relay ONLY these new_steps to the user right now (one message per step). Then call handoff_status again in 3-5 seconds with since_step=${allSteps.length}.`
                : handoffProgress.status === "complete"
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
          if (handoffProgress?.status === "running") {
            return { content: [{ type: "text", text: safeJson({
              status: "error",
              error: "A handoff is already in progress. Use azureclaw_handoff_status to check.",
            }) }] };
          }

          // Initialize progress tracker
          handoffProgress = {
            phase: "confirmed",
            status: "running",
            steps: [`✅ Handoff confirmed — transferring to ${dirLabel}`],
            direction,
            started_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          };

          const adminToken = await _readAdminToken();
          if (!adminToken) {
            handoffProgress.status = "error";
            handoffProgress.phase = "error";
            handoffProgress.error = "Admin token not found";
            handoffProgress.steps.push("❌ Admin token not found — cannot execute handoff");
            handoffProgress.updated_at = new Date().toISOString();
            return { content: [{ type: "text", text: safeJson(handoffProgress) }] };
          }

          // Run orchestration synchronously — return all progress when complete.
          // LLMs don't autonomously poll tools, so we block here and return
          // the full step-by-step result when the handoff finishes.
          try {
            await _runHandoffOrchestration(handoffToken, adminToken, direction, dirLabel);
          } catch (err: any) {
            log.warn(`Handoff orchestration error: ${err.message}`);
            if (handoffProgress) {
              handoffProgress.status = "error";
              handoffProgress.phase = "error";
              handoffProgress.error = err.message;
              handoffProgress.steps.push(`❌ ${err.message}`);
              handoffProgress.updated_at = new Date().toISOString();
            }
          }

          return { content: [{ type: "text", text: safeJson({
            ...handoffProgress,
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

    if (!bannerAlreadyPrinted) log.info(
      "AzureClaw agent tools registered: azureclaw_spawn, azureclaw_spawn_status, azureclaw_mesh_send, azureclaw_mesh_inbox, azureclaw_mesh_transfer_file, azureclaw_spawn_destroy, azureclaw_spawn_list, azureclaw_discover, azureclaw_handoff_status, http_fetch",
    );

    // ── http_fetch: routed through the inference router's egress proxy ──
    // The sandbox (UID 1000) cannot reach the internet directly (iptables).
    // This tool routes requests through the router (UID 1001) which enforces
    // blocklist, allowlist, and learn mode before proxying the request.
    api.registerTool({
      name: "http_fetch",
      label: "HTTP Fetch (Egress Proxy)",
      description:
        "Make an HTTP request to an external URL. The request is routed through the AzureClaw security proxy which enforces blocklist (51K+ malicious domains blocked), allowlist, and learn mode. Use this for ANY external API call (Telegram, HackerNews, web APIs, etc.). Direct internet access via curl/fetch is blocked by the egress guard.",
      parameters: {
        type: "object",
        properties: {
          url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/bot.../getMe)" },
          method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
          headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
          body: { type: "string", description: "Optional request body (for POST/PUT)" },
        },
        required: ["url"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("POST", "/egress/fetch", {
            url: params.url,
            method: (params.method as string) || "GET",
            headers: params.headers || {},
            body: params.body || undefined,
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Fetch failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Code Interpreter: server-side Python execution ──────────
    // Runs in Azure AI Foundry's managed sandbox with pre-installed data
    // science libraries (pandas, numpy, matplotlib, etc.). No egress needed.
    api.registerTool({
      name: "foundry_code_execute",
      label: "Foundry Code Interpreter",
      description:
        "Execute Python code server-side via Azure AI Foundry's code_interpreter. " +
        "Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, " +
        "charts, complex math, and file processing. Runs in a managed Foundry sandbox " +
        "(not the local sandbox). No egress policy needed.",
      parameters: {
        type: "object",
        properties: {
          input: {
            type: "string",
            description: "Natural language instruction or Python code to execute. " +
              "The model will write and run Python code to fulfill the request.",
          },
          model: {
            type: "string",
            description: "Model to use (default: gpt-4.1). Must support code_interpreter.",
          },
        },
        required: ["input"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
            model: (params.model as string) || "gpt-4.1",
            input: params.input,
            tools: [{ type: "code_interpreter", container: { type: "auto" } }],
            store: false,
          });
          // Extract text output from Responses API format
          const output = result.output || result;
          const textParts: string[] = [];
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item.type === "message" && item.content) {
                for (const c of item.content) {
                  if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                }
              } else if (item.type === "code_interpreter_call") {
                textParts.push(`\`\`\`python\n${item.code}\n\`\`\`\nOutput: ${item.output || "(no output)"}`);
              }
            }
          }
          return {
            content: [{
              type: "text",
              text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
            }],
          };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry code execution failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Image Generation: create images from text ───────────────
    api.registerTool({
      name: "foundry_image_generation",
      label: "Foundry Image Generation",
      description:
        "Generate images from text prompts via Azure AI Foundry's image_generation tool. " +
        "Supports any deployed image model (gpt-image-1, FLUX.2-pro, etc.). Returns base64-encoded image data. " +
        "Use when the user asks to create, draw, or generate an image, diagram, or visual.",
      parameters: {
        type: "object",
        properties: {
          prompt: {
            type: "string",
            description: "Text description of the image to generate.",
          },
          quality: {
            type: "string",
            enum: ["low", "medium", "high"],
            description: "Image quality (default: 'medium'). Higher = slower + more detailed.",
          },
          size: {
            type: "string",
            enum: ["1024x1024", "1024x1536", "1536x1024"],
            description: "Image dimensions (default: '1024x1024').",
          },
          image_model: {
            type: "string",
            description: "Image generation model deployment name (default: 'gpt-image-1').",
          },
        },
        required: ["prompt"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const imgModel = (params.image_model as string) || "gpt-image-1";
          const quality = (params.quality as string) || "medium";
          const size = (params.size as string) || "1024x1024";
          const n = 1;

          // Use the standard OpenAI Images API (POST /images/generations)
          // The router proxies this to Azure OpenAI: /openai/deployments/{model}/images/generations
          const result = await _routerCall("POST",
            `/openai/deployments/${encodeURIComponent(imgModel)}/images/generations?api-version=2025-04-01-preview`,
            { prompt: params.prompt, n, size, quality },
            90000,
          );

          // Response format: { data: [{ b64_json: "...", revised_prompt: "..." }] }
          const images = result?.data || [];
          const parts: string[] = [];
          const fs = await import("node:fs");
          const nodePath = await import("node:path");
          const os = await import("node:os");
          const imgDir = nodePath.join(os.tmpdir(), "azureclaw-images");
          fs.mkdirSync(imgDir, { recursive: true });

          for (const img of images) {
            if (img.b64_json) {
              // Save image to temp file so user can view it
              const ts = Date.now();
              const imgFile = nodePath.join(imgDir, `image-${ts}.png`);
              fs.writeFileSync(imgFile, Buffer.from(img.b64_json, "base64"));
              parts.push(`📁 Image saved: ${imgFile}`);
              if (img.revised_prompt) parts.push(`Revised prompt: ${img.revised_prompt}`);
            } else if (img.url) {
              parts.push(`![Generated Image](${img.url})`);
              parts.push(`Image URL: ${img.url}`);
            }
          }
          if (parts.length === 0) parts.push(safeJson(result));
          return { content: [{ type: "text", text: parts.join("\n\n") }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry image generation failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Web Search: real-time Bing-grounded search ──────────────
    // Server-side web search via Bing grounding — no egress policy needed.
    // Results include inline URL citations.
    api.registerTool({
      name: "foundry_web_search",
      label: "Foundry Web Search",
      description:
        "Search the web in real-time via Azure AI Foundry's Bing grounding. " +
        "Returns answers with inline URL citations. Runs server-side — no egress " +
        "policy exceptions needed. Use for current events, news, recent changes, " +
        "verifying facts, or any query needing up-to-date information.",
      parameters: {
        type: "object",
        properties: {
          query: {
            type: "string",
            description: "The search query or question to look up on the web.",
          },
          model: {
            type: "string",
            description: "Model to use (default: gpt-4.1).",
          },
        },
        required: ["query"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          // Connection ID: env var override → auto-discover first GroundingWithBingSearch connection.
          // The Responses API requires the FULL resource ID, not short /connections/name.
          let connId = process.env.BING_CONNECTION_ID;
          if (!connId) {
            try {
              const conns = await routerCall("GET", "/connections?api-version=2025-05-15-preview");
              const bingConn = (conns.value || conns || []).find(
                (c: any) => c.type === "GroundingWithBingSearch" ||
                  c.properties?.category === "GroundingWithBingSearch"
              );
              if (bingConn) connId = bingConn.id; // full resource ID
            } catch { /* fall through to default */ }
          }

          const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
            model: (params.model as string) || "gpt-4.1",
            input: params.query,
            tools: [{
              type: "bing_grounding",
              bing_grounding: {
                search_configurations: [{ project_connection_id: connId }],
              },
            }],
            store: false,
          });
          const output = result.output || result;
          const textParts: string[] = [];
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item.type === "message" && item.content) {
                for (const c of item.content) {
                  if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                }
              }
            }
          }
          return {
            content: [{
              type: "text",
              text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
            }],
          };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry web search failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry File Search: RAG over uploaded documents ─────────────────
    // Knowledge retrieval from vector stores via Foundry's file_search tool.
    api.registerTool({
      name: "foundry_file_search",
      label: "Foundry File Search (RAG)",
      description:
        "Search documents and manage vector stores via Azure AI Foundry's file_search. " +
        "Operations: 'search' for RAG queries, 'create_vector_store' to create a store, " +
        "'list_vector_stores' to list stores, 'delete_vector_store' to remove one, " +
        "'upload_file' to add a file to a store. Use search for document Q&A.",
      parameters: {
        type: "object",
        properties: {
          operation: {
            type: "string",
            enum: ["search", "create_vector_store", "list_vector_stores", "delete_vector_store", "upload_file"],
            description: "Operation: 'search' (default), or manage vector stores/files.",
          },
          query: {
            type: "string",
            description: "The question or search query (for 'search').",
          },
          vector_store_ids: {
            type: "array",
            items: { type: "string" },
            description: "Vector store IDs to search (for 'search'). Omit to search all.",
          },
          store_name: {
            type: "string",
            description: "Name for the vector store (for 'create_vector_store').",
          },
          vector_store_id: {
            type: "string",
            description: "Vector store ID (for 'delete_vector_store' or 'upload_file').",
          },
          file_id: {
            type: "string",
            description: "File ID to add to vector store (for 'upload_file' — upload file via foundry_code_execute first).",
          },
          model: {
            type: "string",
            description: "Model to use for search (default: gpt-4.1).",
          },
        },
        required: [],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const op = (params.operation as string) || "search";
          const apiVer = "api-version=2025-11-15-preview";

          if (op === "list_vector_stores") {
            const result = await routerCall("GET", `/openai/vector_stores?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "create_vector_store") {
            const result = await routerCall("POST", `/openai/vector_stores?${apiVer}`, {
              name: params.store_name || "azureclaw-store",
            });
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "delete_vector_store") {
            await routerCall("DELETE", `/openai/vector_stores/${params.vector_store_id}?${apiVer}`);
            return { content: [{ type: "text", text: `Vector store ${params.vector_store_id} deleted.` }] };
          } else if (op === "upload_file") {
            const result = await routerCall("POST",
              `/openai/vector_stores/${params.vector_store_id}/files?${apiVer}`,
              { file_id: params.file_id });
            return { content: [{ type: "text", text: safeJson(result) }] };
          }

          // Default: search operation
          const fileSearchTool: any = { type: "file_search" };
          if (params.vector_store_ids) {
            fileSearchTool.file_search = { vector_store_ids: params.vector_store_ids };
          }
          const result = await routerCall("POST", `/openai/responses?${apiVer}`, {
            model: (params.model as string) || "gpt-4.1",
            input: params.query,
            tools: [fileSearchTool],
            store: false,
          });
          const output = result.output || result;
          const textParts: string[] = [];
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item.type === "message" && item.content) {
                for (const c of item.content) {
                  if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                }
              } else if (item.type === "file_search_call" && item.results) {
                for (const r of item.results) {
                  textParts.push(`[${r.filename || "source"}] ${r.text || ""}`);
                }
              }
            }
          }
          return {
            content: [{
              type: "text",
              text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
            }],
          };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry file search failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Memory: persistent semantic memory store ────────────────
    api.registerTool({
      name: "foundry_memory",
      label: "Foundry Memory Store",
      description:
        "Manage persistent agent memory via Azure AI Foundry Memory Store. " +
        "Store facts, preferences, and context that persists across conversations. " +
        "Supports semantic search over stored memories.",
      parameters: {
        type: "object",
        properties: {
          operation: {
            type: "string",
            enum: ["search", "update", "delete_scope"],
            description: "Operation: 'search' to find relevant memories, 'update' to store new facts/preferences, 'delete_scope' to clear all memories in a scope.",
          },
          text: {
            type: "string",
            description: "For 'update': the fact or preference to remember (e.g. 'User prefers dark roast coffee'). For 'search': the query to find relevant memories (e.g. 'coffee preferences').",
          },
          scope: { type: "string", description: "Memory scope (default: sandbox name). Use to partition memories by user." },
          store_name: { type: "string", description: "Memory store name (default: 'memory-{agent}')." },
        },
        required: ["operation", "text"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
          const store = (params.store_name as string) || `memory-${agentName}`;
          const scope = (params.scope as string) || agentName;
          const op = params.operation as string;
          const text = (params.text as string) || "";
          const apiVer = "api-version=2025-11-15-preview";

          // Build Foundry-format conversation item (same for both update and search)
          const makeItem = (content: string) => ({
            type: "message",
            role: "user",
            content: [{ type: "input_text", text: content }],
          });

          // Poll an update operation until complete (LRO)
          const pollUpdate = async (updateId: string, maxWaitMs = 60000) => {
            const start = Date.now();
            while (Date.now() - start < maxWaitMs) {
              await new Promise(r => setTimeout(r, 2000));
              try {
                const status = await routerCall("GET", `/memory_stores/${store}/updates/${updateId}?${apiVer}`);
                const state = status?.status || status?.state;
                if (state === "completed" || state === "succeeded") return status;
                if (state === "failed" || state === "error") throw new Error(`Memory update failed: ${safeJson(status)}`);
              } catch (e: any) {
                if (!e.message?.includes("404")) throw e;
              }
            }
            return { status: "timeout", message: "Memory update still processing. It will complete in the background." };
          };

          // Auto-create memory store if it doesn't exist yet
          const ensureStore = async () => {
            try {
              await routerCall("GET", `/memory_stores/${store}?${apiVer}`);
            } catch (e: any) {
              if (e.message?.includes("404") || e.message?.includes("not_found") || e.message?.includes("not found")) {
                const chatModel = process.env.OPENCLAW_MODEL || "gpt-4.1";
                const embeddingModel = foundryProject?.deployments?.find(
                  (d: any) => d.id?.includes("embedding") || d.model?.includes("embedding")
                )?.id || "text-embedding-3-small";
                log.info(`Creating memory store '${store}' (chat=${chatModel}, embedding=${embeddingModel})`);
                await routerCall("POST", `/memory_stores?${apiVer}`, {
                  name: store,
                  description: "AzureClaw agent persistent memory",
                  definition: {
                    kind: "default",
                    chat_model: chatModel,
                    embedding_model: embeddingModel,
                    options: {
                      user_profile_enabled: true,
                      user_profile_details: "Store user preferences, decisions, and project context",
                      chat_summary_enabled: true,
                    },
                  },
                });
                log.info(`Memory store '${store}' created successfully`);
              }
            }
          };

          if (op === "search") {
            const body = {
              scope,
              items: [makeItem(text)],
              options: { max_memories: 10 },
            };
            try {
              const result = await routerCall("POST", `/memory_stores/${store}:search_memories?${apiVer}`, body);
              return { content: [{ type: "text", text: safeJson(result) }] };
            } catch (e: any) {
              if (e.message?.includes("not found") || e.message?.includes("not_found")) {
                try {
                  await ensureStore();
                  // Retry search after store creation
                  const result = await routerCall("POST", `/memory_stores/${store}:search_memories?${apiVer}`, body);
                  return { content: [{ type: "text", text: safeJson(result) }] };
                } catch {
                  return { content: [{ type: "text", text: "Memory store just created — no memories stored yet. Try saving something first." }] };
                }
              }
              // Don't crash session on memory errors — return graceful message
              log.warn(`Memory search failed: ${e.message}`);
              return { content: [{ type: "text", text: `Memory search failed: ${e.message}. The memory service may still be initializing.` }] };
            }
          } else if (op === "update") {
            const body = {
              scope,
              items: [makeItem(text)],
              update_delay: 0,
            };
            const doUpdate = async () => {
              const result = await routerCall("POST", `/memory_stores/${store}:update_memories?${apiVer}`, body);
              // update_memories is a LRO — log completion in background, don't block chat
              const updateId = result?.update_id || result?.id;
              if (updateId && (result?.status === "queued" || result?.status === "running")) {
                pollUpdate(updateId).then(
                  (r) => log.info(`Memory update ${updateId} completed: ${JSON.stringify(r?.memory_operations?.length ?? 0)} ops`),
                  (e) => log.warn(`Memory update ${updateId} failed: ${e.message}`),
                );
              }
              return result;
            };
            try {
              const result = await doUpdate();
              const status = result?.status || "submitted";
              return { content: [{ type: "text", text: `Memory update ${status}. The memory will be available shortly.` }] };
            } catch (e: any) {
              if (e.message?.includes("not found") || e.message?.includes("not_found")) {
                try {
                  await ensureStore();
                  const result = await doUpdate();
                  const status = result?.status || "submitted";
                  return { content: [{ type: "text", text: `Memory update ${status}. The memory will be available shortly.` }] };
                } catch (retryErr: any) {
                  log.warn(`Memory update failed after store creation: ${retryErr.message}`);
                  return { content: [{ type: "text", text: `Memory update failed: ${retryErr.message}` }] };
                }
              }
              log.warn(`Memory update failed: ${e.message}`);
              return { content: [{ type: "text", text: `Memory update failed: ${e.message}. The memory service may still be initializing.` }] };
            }
          } else if (op === "delete_scope") {
            await routerCall("POST", `/memory_stores/${store}:delete_scope?${apiVer}`, { scope });
            return { content: [{ type: "text", text: `Scope '${scope}' deleted from memory store '${store}'.` }] };
          }
          return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry memory failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Conversations: persistent multi-turn state ──────────────
    api.registerTool({
      name: "foundry_conversations",
      label: "Foundry Conversations",
      description:
        "Manage persistent server-side conversations via Azure AI Foundry. " +
        "Use cases: maintain long-running multi-turn dialogues across sessions, " +
        "build research threads that survive restarts, keep separate conversation " +
        "contexts for different tasks/topics. Operations: create, list, get, respond, " +
        "add_message, delete.",
      parameters: {
        type: "object",
        properties: {
          operation: {
            type: "string",
            enum: ["create", "list", "get", "respond", "add_message", "delete"],
            description: "Operation to perform. 'get' retrieves full message history for a conversation.",
          },
          conversation_id: { type: "string", description: "Conversation ID (for get/respond/add_message/delete)." },
          input: { type: "string", description: "User input (for 'respond' — generates AI response in conversation context)." },
          message: { type: "string", description: "Message text to add (for 'add_message')." },
          role: { type: "string", description: "Message role: 'user' or 'assistant' (for 'add_message', default: 'user')." },
          metadata: { type: "object", description: "Metadata for new conversation (for 'create')." },
          model: { type: "string", description: "Model to use for responses (default: gpt-4.1)." },
        },
        required: ["operation"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const op = params.operation as string;
          const apiVer = "api-version=2025-11-15-preview";

          if (op === "create") {
            const result = await routerCall("POST", `/openai/conversations?${apiVer}`, {
              metadata: params.metadata || { user: process.env.SANDBOX_NAME || "agent" },
            });
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "list") {
            const result = await routerCall("GET", `/openai/conversations?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "get") {
            const result = await routerCall("GET", `/openai/conversations/${params.conversation_id}?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "respond") {
            const result = await routerCall("POST", `/openai/responses?${apiVer}`, {
              model: (params.model as string) || "gpt-4.1",
              input: params.input,
              conversation: params.conversation_id,
              store: true,
            });
            const output = result.output || result;
            const textParts: string[] = [];
            if (Array.isArray(output)) {
              for (const item of output) {
                if (item.type === "message" && item.content) {
                  for (const c of item.content) {
                    if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                  }
                }
              }
            }
            return { content: [{ type: "text", text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2) }] };
          } else if (op === "add_message") {
            const result = await routerCall("POST", `/openai/conversations/${params.conversation_id}/items?${apiVer}`, {
              items: [{
                type: "message",
                role: (params.role as string) || "user",
                content: [{ type: "input_text", text: params.message }],
              }],
            });
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "delete") {
            await routerCall("DELETE", `/openai/conversations/${params.conversation_id}?${apiVer}`);
            return { content: [{ type: "text", text: `Conversation ${params.conversation_id} deleted.` }] };
          }
          return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry conversations failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Evaluations: model quality testing ──────────────────────
    api.registerTool({
      name: "foundry_evaluations",
      label: "Foundry Evaluations",
      description:
        "Create and run model quality evaluations via Azure AI Foundry Evals API. " +
        "Use cases: benchmark prompt quality before/after changes, validate output " +
        "against golden answers, run regression tests on model responses, compare " +
        "different models. Operations: list, create, run, get_run, list_evaluators.",
      parameters: {
        type: "object",
        properties: {
          operation: {
            type: "string",
            enum: ["list", "create", "run", "get_run", "list_evaluators"],
            description: "Operation: 'list' evals, 'create' one, 'run' it, 'get_run' status/results, or 'list_evaluators'.",
          },
          eval_id: { type: "string", description: "Eval ID (for 'run')." },
          run_id: { type: "string", description: "Run ID (for 'get_run' — check status and results)." },
          name: { type: "string", description: "Eval name (for 'create')." },
          data_source_config: { type: "object", description: "Data source config (for 'create')." },
          testing_criteria: { type: "array", items: { type: "object" }, description: "Testing criteria array (for 'create')." },
          run_config: { type: "object", description: "Run configuration (for 'run')." },
        },
        required: ["operation"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const op = params.operation as string;
          const apiVer = "api-version=2025-11-15-preview";

          if (op === "list") {
            const result = await routerCall("GET", `/openai/evals?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "create") {
            const result = await routerCall("POST", `/openai/evals?${apiVer}`, {
              name: params.name,
              data_source_config: params.data_source_config,
              testing_criteria: params.testing_criteria,
            });
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "run") {
            const result = await routerCall("POST", `/openai/evals/${params.eval_id}/runs?${apiVer}`,
              params.run_config || {});
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "get_run") {
            const result = await routerCall("GET", `/openai/evals/${params.eval_id}/runs/${params.run_id}?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } else if (op === "list_evaluators") {
            const result = await routerCall("GET", `/evaluators?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          }
          return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry evaluations failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Deployments: discover available models and connections ───
    api.registerTool({
      name: "foundry_deployments",
      label: "Foundry Deployments & Connections",
      description:
        "Query available Azure AI Foundry resources: models, connections, " +
        "search indexes, and datasets. Use 'models' to see all available AI models, " +
        "'connections' for data connections, 'indexes' for search indexes.",
      parameters: {
        type: "object",
        properties: {
          resource: {
            type: "string",
            enum: ["models", "connections", "indexes", "datasets"],
            description: "Resource type to query. Use 'models' to list available AI models.",
          },
        },
        required: ["resource"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const resource = params.resource as string;

          if (resource === "models") {
            // Query live Foundry project deployments — returns actual deployed models
            const apiVer = "api-version=2025-11-15-preview";
            try {
              const result = await routerCall("GET", `/deployments?${apiVer}`);
              const deps = result?.value || result?.data || [];
              if (Array.isArray(deps) && deps.length > 0) {
                const currentModel = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";
                const models = deps.map((d: any) => ({
                  id: d.name || d.id,
                  model: d.modelName || d.model || d.name || "unknown",
                  version: d.modelVersion || "",
                  publisher: d.modelPublisher || "",
                  capabilities: d.capabilities || {},
                  sku: d.sku?.name || "unknown",
                  capacity: d.sku?.capacity || 0,
                  current: (d.name || d.id) === currentModel,
                }));
                return { content: [{ type: "text", text: safeJson({
                  source: "foundry_project_deployments",
                  current_model: currentModel,
                  total: models.length,
                  models,
                }) }] };
              }
            } catch { /* fall through to cached */ }

            // Fallback to cached discovery from startup
            if (foundryProject?.deployments && foundryProject.deployments.length > 0) {
              return { content: [{ type: "text", text: safeJson({
                source: "cached_discovery",
                total: foundryProject.deployments.length,
                models: foundryProject.deployments,
              }) }] };
            }

            return { content: [{ type: "text", text: "No deployments found. Check Foundry project configuration." }] };
          }

          // Other resources: try Foundry API first, fall back gracefully
          const apiVer = "api-version=2025-11-15-preview";
          const result = await routerCall("GET", `/${resource}?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry query failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Agents: list and query Foundry-hosted agents ────────────
    api.registerTool({
      name: "foundry_agents",
      label: "Foundry Agents",
      description:
        "List and query Azure AI Foundry hosted agents. Discover available agents, " +
        "their capabilities, and configurations. These are server-side Foundry agents " +
        "(different from AzureClaw sub-agent sandboxes).",
      parameters: {
        type: "object",
        properties: {
          operation: {
            type: "string",
            enum: ["list", "get"],
            description: "Operation: 'list' all agents or 'get' a specific agent.",
          },
          agent_id: { type: "string", description: "Agent ID (for 'get')." },
        },
        required: ["operation"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const apiVer = "api-version=2025-11-15-preview";
          if (params.operation === "get" && params.agent_id) {
            const result = await routerCall("GET", `/agents/${params.agent_id}?${apiVer}`);
            return { content: [{ type: "text", text: safeJson(result) }] };
          }
          const result = await routerCall("GET", `/agents?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Foundry agents query failed: ${e.message}` }] };
        }
      },
    });

    if (!bannerAlreadyPrinted) log.info("Foundry tools registered: foundry_code_execute, foundry_image_generation, foundry_web_search, foundry_file_search, foundry_memory, foundry_conversations, foundry_evaluations, foundry_deployments, foundry_agents");

    // ── Register Azure AI Foundry as a model provider ───────────────────
    // Use dynamically discovered deployments when available, fall back to defaults
    const defaultModels = [
      { id: "gpt-4.1", label: "GPT-4.1 (Azure)", contextWindow: 1047576, maxOutput: 32768 },
      { id: "gpt-5-mini", label: "GPT-5 Mini (Azure)", contextWindow: 1047576, maxOutput: 32768 },
      { id: "gpt-4o", label: "GPT-4o (Azure)", contextWindow: 128000, maxOutput: 16384 },
      { id: "DeepSeek-V3.2", label: "DeepSeek V3.2 (Foundry)", contextWindow: 131072, maxOutput: 8192 },
      { id: "Phi-4", label: "Phi-4 (Microsoft)", contextWindow: 16384, maxOutput: 16384 },
      { id: "Meta-Llama-3.1-405B-Instruct", label: "Llama 3.1 405B (Meta)", contextWindow: 131072, maxOutput: 8192 },
      { id: "o3-mini", label: "o3-mini (Azure)", contextWindow: 200000, maxOutput: 100000 },
    ];

    // If Foundry discovery populated deployments, build models from those
    const chatModels = (foundryProject?.deployments?.length)
      ? foundryProject.deployments.map(d => ({
          id: d.id,
          label: `${d.model || d.id} (Azure Foundry)`,
          contextWindow: 128000,
          maxOutput: 16384,
        }))
      : defaultModels;

    api.registerProvider({
      id: "azure-openai",
      label: "Azure AI Foundry (via AzureClaw)",
      docsPath: "https://github.com/Azure/azureclaw",
      aliases: ["azure", "azureclaw", "foundry"],
      envVars: ["AZURE_OPENAI_API_KEY"],
      models: { chat: chatModels },
      auth: [
        {
          id: "azure-openai-key",
          type: "api-key",
          envVar: "AZURE_OPENAI_API_KEY",
          headerName: "api-key",
          label: "Azure API Key (or 'routed-via-inference-router' for AzureClaw)",
        },
      ],
    });

    // ── Register CLI subcommands: openclaw azureclaw <cmd> ────────────────
    api.registerCli(
      (ctx: PluginCliContext) => {
        const azureclaw = ctx.program
          .command("azureclaw")
          .description("AzureClaw — secure AI agent runtime on Azure");

        azureclaw
          .command("status")
          .description("Show sandbox health, security, and inference metrics")
          .action(async () => {
            const http = await import("node:http");
            try {
              const body = await new Promise<string>((resolve, reject) => {
                const req = http.get(routerUrl("/metrics"), (res) => {
                  let data = "";
                  res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                  res.on("end", () => resolve(data));
                });
                req.on("error", reject);
                req.setTimeout(3000, () => { req.destroy(); reject(new Error("timeout")); });
              });
              console.log("AzureClaw Inference Router — Prometheus Metrics\n");
              console.log(body);
            } catch {
              console.log(`AzureClaw Inference Router: not reachable (${routerUrl("/metrics")})`);
            }
          });

        azureclaw
          .command("connect")
          .description("Connect to the sandbox (host-side only)")
          .action(async () => {
            console.log("'connect' is a host-side command. Inside the sandbox, you're already connected.");
            console.log("From the host, run: azureclaw connect");
          });

        azureclaw
          .command("dev")
          .description("Start a local sandbox (host-side only)")
          .action(async () => {
            console.log("'dev' is a host-side command. This sandbox is already running.");
            console.log("From the host, run: azureclaw dev");
          });

        azureclaw
          .command("logs")
          .option("-f, --follow", "Follow log output")
          .description("Stream sandbox logs (host-side only)")
          .action(async () => {
            console.log("'logs' is a host-side command.");
            console.log("From the host, run: azureclaw logs");
          });
      },
      { commands: ["azureclaw"] }
    );

    // ── Register /azureclaw slash command ─────────────────────────────────
    api.registerCommand({
      name: "azureclaw",
      description: "Show AzureClaw sandbox status, models, and security info",
      handler: async () => {
        return {
          text: [
            "**AzureClaw Sandbox** (Foundry-integrated)",
            `Model: ${config.model}`,
            `Sandbox: ${config.sandboxName}`,
            `Endpoint: ${config.endpoint || "(configured via Foundry)"}`,
            "",
            "**Slash Commands:**",
            "- `/azureclaw` — this help",
            "- `/azureclaw-models` — list available Foundry models",
            "- `/azureclaw-switch <model>` — switch AI model live",
            "- `/azureclaw-agents` — list Foundry agents",
            "- `/azureclaw-memory <agent-id>` — view agent memory (threads)",
            "- `/azureclaw-security` — show isolation level + security posture",
            "",
            "**CLI Commands (from host):**",
            "- `azureclaw model list foundry-agent` — live model catalog",
            "- `azureclaw model set foundry-agent Phi-4` — switch model",
            "- `azureclaw policy get foundry-agent` — show network policy",
            "- `azureclaw approve --list` — pending egress requests",
            "- `azureclaw trace foundry-agent --exec` — eBPF tracing",
          ].join("\n"),
        };
      },
    });

    // ── /azureclaw-models — list available models from Foundry ────────────
    api.registerCommand({
      name: "azureclaw-models",
      description: "List available AI models from Azure Foundry",
      handler: async () => {
        try {
          const http = await import("node:http");
          // Query actual Foundry deployments (not the full catalog)
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(
              routerUrl("/deployments?api-version=2025-11-15-preview"),
              { headers: { "x-azureclaw-sandbox": "self" } },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const deployments = parsed.data || parsed.value || [];
          const lines = deployments.map((d: any) => {
            const name = d.id || d.name || "?";
            const model = d.model?.name || d.model || d.properties?.model?.name || "";
            const status = d.status || d.properties?.provisioningState || "?";
            return `  - **${name}**${model ? ` (${model})` : ""} — ${status}`;
          });
          return {
            text: [
              `**Foundry Deployments** (${deployments.length})`,
              "",
              ...lines,
              "",
              "Switch with: `/azureclaw-switch <model>`",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not query deployments. Is the inference router running?" };
        }
      },
    });

    // ── Shared model switch logic ────────────────────────────────────────
    async function switchModelInternal(model: string): Promise<string> {
      const prevModel = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";

      // 1. Flush conversation context to Foundry memory before switching
      try {
        // Flush any buffered tool calls
        if (memorySyncBuffer.length > 0) {
          const batch = memorySyncBuffer.splice(0);
          const batchSummary = `Pre-switch checkpoint (${batch.length} calls):\n${batch.join("\n")}`;
          await syncToFoundryMemory(batchSummary, log);
        }
        // Save a handoff summary so the new session has context
        const handoff = [
          `Model switch: ${prevModel} → ${model}`,
          `User requested switching to ${model} mid-conversation.`,
          `Session was active with ${prevModel}. Key context should be recalled from prior memories.`,
        ].join("\n");
        await syncToFoundryMemory(handoff, log);
        log.info(`Memory flushed before model switch to ${model}`);
      } catch (e: any) {
        log.warn(`Memory flush before switch failed (non-blocking): ${e.message}`);
      }

      // 2. Update plugin env + config
      process.env.OPENCLAW_MODEL = model;
      config.model = model;

      // 3. Update OpenClaw config files
      try {
        const fs = await import("node:fs");
        const modelsPath = "/sandbox/.openclaw/agents/main/agent/models.json";
        const oclawPath = "/sandbox/.openclaw/openclaw.json";

        const allModels = new Set<string>();
        allModels.add(model);
        if (foundryProject?.deployments) {
          for (const d of foundryProject.deployments) {
            if (!d.id.includes("embedding")) allModels.add(d.id);
          }
        }
        const modelsArr = [...allModels].map(id => ({
          id, name: `${id} (Azure via AzureClaw)`, reasoning: false,
          input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          contextWindow: 200000, maxTokens: 8192, api: "openai-completions",
        }));

        try {
          const mj = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
          if (mj.providers?.["azure-openai"]) {
            mj.providers["azure-openai"].models = modelsArr.map(m => ({ id: m.id, name: m.name }));
          }
          mj.selectedModel = { provider: "azure-openai", id: model };
          fs.writeFileSync(modelsPath, JSON.stringify(mj, null, 2));
        } catch { /* read-only fs */ }

        try {
          const oc = JSON.parse(fs.readFileSync(oclawPath, "utf8"));
          if (oc.models?.providers?.["azure-openai"]) {
            oc.models.providers["azure-openai"].models = modelsArr.map(m => ({ id: m.id, name: m.name }));
          }
          if (oc.agents?.defaults?.model) {
            oc.agents.defaults.model.primary = `azure-openai/${model}`;
          }
          fs.writeFileSync(oclawPath, JSON.stringify(oc, null, 2));
        } catch { /* read-only fs */ }
      } catch { /* non-critical */ }

      // 4. Update router model override
      try {
        const result = await routerCall("PUT", "/admin/model", { model });
        const prev = (result as any)?.previous || prevModel;
        return [
          `✅ Switched **${prev}** → **${model}**`,
          "",
          "Context saved to Foundry memory.",
          "Type `/new` to start a fresh session with **" + model + "** — your conversation context will be recalled automatically.",
        ].join("\n");
      } catch {
        return [
          `⚠️ Plugin updated to **${model}**, but router admin endpoint not reachable.`,
          "",
          "Context saved to Foundry memory.",
          "Type `/new` to start a fresh session with **" + model + "**.",
        ].join("\n");
      }
    }

    // ── /azureclaw-switch — switch model with memory handoff ──────────────
    api.registerCommand({
      name: "azureclaw-switch",
      description: "Switch AI model (e.g. /azureclaw-switch gpt-5.4-mini)",
      acceptsArgs: true,
      handler: async (ctx) => {
        const model = ctx.args?.trim();
        if (!model) {
          const available = foundryProject?.deployments
            ?.filter((d: any) => !d.id?.includes("embedding"))
            ?.map((d: any) => d.id).join(", ") || "unknown";
          return { text: `Usage: /azureclaw-switch <model-name>\nAvailable: ${available}` };
        }
        return { text: await switchModelInternal(model) };
      },
    });

    // ── /switch-model — show/switch AI model (avoids built-in /model) ───
    api.registerCommand({
      name: "switch-model",
      description: "Show or switch AI model (e.g. /switch-model gpt-5.4-mini)",
      acceptsArgs: true,
      handler: async (ctx) => {
        const model = ctx.args?.trim();
        if (!model) {
          // Query live deployments from Foundry
          const current = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";
          let available: string[] = [];
          try {
            const result = await routerCall("GET", `/deployments?api-version=2025-11-15-preview`);
            const deps = (result as any)?.data || (result as any)?.value || [];
            if (Array.isArray(deps)) {
              available = deps
                .filter((d: any) => {
                  const id = d.id || d.name || "";
                  return !id.includes("embedding");
                })
                .map((d: any) => {
                  const id = d.id || d.name || "?";
                  const modelName = d.model?.name || d.model || d.properties?.model?.name || "";
                  const label = modelName && modelName !== id ? `${id} (${modelName})` : id;
                  return id === current ? `**${label}** ← current` : label;
                });
            }
          } catch {
            // Fall back to cached discovery
            available = (foundryProject?.deployments || [])
              .filter((d: any) => !d.id?.includes("embedding"))
              .map((d: any) => d.id === current ? `**${d.id}** ← current` : d.id);
          }
          return { text: [
            `Current model: **${current}**`,
            "",
            "Available deployments:",
            ...available.map((m: string) => `  • ${m}`),
            "",
            "Usage: `/switch-model <name>` to switch",
          ].join("\n") };
        }
        return { text: await switchModelInternal(model) };
      },
    });

    // ── /azureclaw-security — show security posture ───────────────────────
    api.registerCommand({
      name: "azureclaw-security",
      description: "Show sandbox security posture",
      handler: async () => {
        const uname = await import("node:child_process");
        let kernel = "unknown";
        let user = "unknown";
        try {
          kernel = uname.execSync("uname -r", { encoding: "utf-8" }).trim();
          user = uname.execSync("whoami", { encoding: "utf-8" }).trim();
        } catch {}

        const isKata = kernel.includes("mshv");
        return {
          text: [
            "**AzureClaw Security Posture**",
            "",
            `Kernel: ${kernel}`,
            `User: ${user}`,
            `Isolation: ${isKata ? "confidential (Kata VM)" : "enhanced (runc + seccomp)"}`,
            `Root filesystem: read-only`,
            `Capabilities: ALL dropped`,
            `Seccomp: ${isKata ? "RuntimeDefault (VM boundary)" : "Localhost (azureclaw-strict)"}`,
            `Network: default-deny egress + iptables UID guard`,
            `Inference: routed through AzureClaw inference router`,
            `Foundry Agent API: proxied via ${routerBase()}/agents/*`,
            `Auth: IMDS (kubelet MI, zero keys)`,
          ].join("\n"),
        };
      },
    });

    // ── /azureclaw-agt — AGT governance status + policy evaluation ────────
    api.registerCommand({
      name: "azureclaw-agt",
      description: "AGT governance status. /azureclaw-agt check <action> to evaluate policy",
      acceptsArgs: true,
      handler: async (ctx) => {
        const args = ctx.args?.trim() || "";

        // Policy check mode: /azureclaw-agt check shell:rm -rf /
        if (args.startsWith("check ")) {
          const action = args.slice(6).trim();
          if (agtPolicy) {
            const decision = agtPolicy.evaluate(action);
            return {
              text: [
                `**AGT Policy Check** (via @agentmesh/sdk)`,
                `Action: \`${action}\``,
                `Decision: **${decision.effect}**`,
                decision.effect === "deny" ? "Blocked by AGT policy" : "Allowed",
              ].join("\n"),
            };
          }
          // Fallback to router-native policy
          try {
            const http = await import("node:http");
            const postData = JSON.stringify({ action });
            const body = await new Promise<string>((resolve, reject) => {
              const req = http.request({ hostname: "127.0.0.1", port: 8443, path: "/agt/evaluate", method: "POST", headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) } }, (res) => {
                let data = ""; res.on("data", (c: Buffer) => { data += c.toString(); }); res.on("end", () => resolve(data));
              });
              req.on("error", reject); req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
              req.write(postData); req.end();
            });
            const parsed = JSON.parse(body);
            return { text: `**Policy Check** (router-native)\nAction: \`${action}\`\nDecision: **${parsed.decision || parsed.error}**` };
          } catch {
            return { text: "Could not evaluate policy. Is the router running?" };
          }
        }

        // Status mode
        const sdkStatus = agtPolicy ? "active (@agentmesh/sdk)" : "unavailable (using router-native)";
        const trustStatus = agtTrustStore ? "active (Ed25519, 0-1000 scale)" : "unavailable";
        const auditStatus = agtAuditLogger ? "active (hash-chain)" : "unavailable";
        const meshStatus = agtMeshClient
          ? (agtMeshClient.isConnected ? "connected (E2E encrypted)" : "initialized (not connected)")
          : "unavailable";
        const identityStatus = agtIdentity ? `AMID: ${agtIdentity.amid}` : "not generated";

        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(routerUrl("/agt/status"), (res) => {
              let data = ""; res.on("data", (c: Buffer) => { data += c.toString(); }); res.on("end", () => resolve(data));
            });
            req.on("error", reject); req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          return {
            text: [
              "**AzureClaw AGT Governance**",
              "",
              "**Application Layer** (plugin, @agentmesh/sdk):",
              `  Identity: ${identityStatus}`,
              `  Mesh client: ${meshStatus}`,
              `  Policy engine: ${sdkStatus}`,
              `  Trust store: ${trustStatus}`,
              `  Audit logger: ${auditStatus}`,
              "",
              "**Infrastructure Layer** (Rust router):",
              `  Governance: ${parsed.enabled ? "enabled" : "disabled"}`,
              `  Sandbox: ${parsed.sandbox}`,
              `  Audit entries: ${parsed.audit_entries}`,
              `  Mesh inbox: ${parsed.inbox_messages} messages`,
              `  Mesh sessions: ${parsed.mesh_sessions ?? 0}  sent: ${parsed.mesh_messages_sent ?? 0}  recv: ${parsed.mesh_messages_received ?? 0}`,
              `  Trust updates: ${parsed.trust_updates ?? 0}  total interactions: ${parsed.total_interactions ?? 0}`,
              parsed.blocklist_domains ? `  Blocklist: ${parsed.blocklist_domains} domains` : "",
              "",
              "**Overlap resolution:**",
              "  Tool policy → AGT SDK (plugin)",
              "  Mesh routing → Rust router (K8s DNS)",
              "  Content safety → AzureClaw (Azure AI)",
              "  Token budgets → AzureClaw (router)",
              "  Network/FS → AzureClaw (iptables/seccomp)",
              "",
              "Check policy: `/azureclaw-agt check shell:rm -rf /`",
            ].filter(Boolean).join("\n"),
          };
        } catch {
          return {
            text: [
              "**AzureClaw AGT Governance**",
              `Policy engine: ${sdkStatus}`,
              `Trust store: ${trustStatus}`,
              `Audit logger: ${auditStatus}`,
              "",
              "Router unreachable — showing SDK-only status.",
            ].join("\n"),
          };
        }
      },
    });

    // ── /azureclaw-agents — list Foundry agents via proxied API ───────────
    api.registerCommand({
      name: "azureclaw-agents",
      description: "List Foundry agents available in this sandbox",
      handler: async () => {
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(routerUrl("/agents"), (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const agents = parsed.data || [];
          if (agents.length === 0) {
            return {
              text: [
                "**Foundry Agents**: none created yet",
                "",
                "Create an agent via the Foundry Agent API:",
                "```",
                `POST ${routerBase()}/agents`,
                '{"name": "my-agent", "model": "gpt-4.1", "instructions": "You are a helpful assistant"}',
                "```",
                "",
                "The router authenticates and proxies to Foundry automatically.",
              ].join("\n"),
            };
          }
          return {
            text: [
              `**Foundry Agents** (${agents.length})`,
              "",
              ...agents.map((a: any) => `- **${a.name || a.id}** (model: ${a.model || "default"}, id: ${a.id})`),
              "",
              "Use `/azureclaw-memory <agent-id>` to view threads.",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not query Foundry agents. Is the inference router running?" };
        }
      },
    });

    // ── /azureclaw-memory — list Foundry threads (agent memory) ───────────
    api.registerCommand({
      name: "azureclaw-memory",
      description: "List Foundry threads (agent memory) — /azureclaw-memory [agent-id]",
      acceptsArgs: true,
      handler: async (ctx) => {
        const agentId = ctx.args?.trim();
        if (!agentId) {
          return { text: "Usage: `/azureclaw-memory <agent-id>`\n\nUse `/azureclaw-agents` to list agents first." };
        }
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(routerUrl(`/agents/${agentId}/threads`), (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const threads = parsed.data || [];
          if (threads.length === 0) {
            return { text: `No threads found for agent ${agentId}. Memory is created when the agent processes messages.` };
          }
          return {
            text: [
              `**Agent Memory** (${threads.length} threads for ${agentId})`,
              "",
              ...threads.slice(0, 10).map((t: any) =>
                `- Thread ${t.id} (created: ${t.created_at || "unknown"})`
              ),
              threads.length > 10 ? `\n... and ${threads.length - 10} more` : "",
            ].join("\n"),
          };
        } catch {
          return { text: `Could not query threads for agent ${agentId}. Check that the agent exists.` };
        }
      },
    });

    // ── /azureclaw-spawn — spawn a sub-agent sandbox via router ────────────
    api.registerCommand({
      name: "azureclaw-spawn",
      description: "Spawn a sub-agent sandbox — /azureclaw-spawn <name> [--model X] [--governance] [--learn-egress]",
      acceptsArgs: true,
      handler: async (ctx) => {
        const raw = ctx.args?.trim() || "";
        if (!raw) {
          return {
            text: [
              "**Usage:** `/azureclaw-spawn <name> [options]`",
              "",
              "**Options:**",
              "  `--model <name>` — model deployment (default: gpt-4.1)",
              "  `--governance` — enable AGT governance + mesh",
              "  `--trust-threshold <n>` — AGT trust threshold (default: 500)",
              "  `--learn-egress` — enable egress learn mode",
              "  `--token-budget-daily <n>` — daily token limit",
              "",
              "**Examples:**",
              "  `/azureclaw-spawn sub-analyst --model gpt-4.1 --governance`",
              "  `/azureclaw-spawn sub-coder --model DeepSeek-V3.2 --learn-egress`",
              "",
              "**After spawning:**",
              "  `/azureclaw-spawn-list` — list your sub-agents",
              "  Use the azureclaw_mesh_send tool to communicate (E2E encrypted)",
            ].join("\n"),
          };
        }

        // Parse args: first token is name, rest are flags
        const tokens = raw.split(/\s+/);
        const name = tokens[0];
        const body: Record<string, unknown> = { agent_id: name };

        for (let i = 1; i < tokens.length; i++) {
          switch (tokens[i]) {
            case "--model":
              body.model = tokens[++i];
              break;
            case "--governance":
              body.governance = true;
              break;
            case "--trust-threshold":
              body.trust_threshold = parseInt(tokens[++i], 10);
              break;
            case "--learn-egress":
              body.learn_egress = true;
              break;
            case "--token-budget-daily":
              body.token_budget_daily = parseInt(tokens[++i], 10);
              break;
            case "--token-budget-per-request":
              body.token_budget_per_request = parseInt(tokens[++i], 10);
              break;
            case "--isolation":
              body.isolation = tokens[++i];
              break;
          }
        }

        try {
          const http = await import("node:http");
          const postData = JSON.stringify(body);
          const result = await new Promise<string>((resolve, reject) => {
            const req = http.request(
              {
                hostname: "127.0.0.1",
                port: 8443,
                path: "/sandbox/spawn",
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) },
              },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(15000, () => { req.destroy(); reject(new Error("timeout")); });
            req.write(postData);
            req.end();
          });
          const parsed = JSON.parse(result);
          if (parsed.error) {
            return { text: `**Spawn failed:** ${parsed.error}` };
          }
          return {
            text: [
              `**Sub-agent spawned:** ${parsed.agent_id}`,
              `Namespace: ${parsed.namespace || "pending"}`,
              `Phase: ${parsed.phase || "Pending"}`,
              parsed.message || "",
              "",
              "**Next steps:**",
              body.governance
                ? "- Send tasks via azureclaw_mesh_send tool (E2E encrypted)"
                : "- Enable governance for inter-agent communication",
              "- Check status: `/azureclaw-spawn-list`",
              "- Tear down: `/azureclaw-spawn-destroy " + name + "`",
            ].join("\n"),
          };
        } catch {
          return { text: `**Spawn error:** Could not reach the inference router. Is it running?` };
        }
      },
    });

    // ── /azureclaw-spawn-list — list spawned sub-agents ───────────────────
    api.registerCommand({
      name: "azureclaw-spawn-list",
      description: "List sub-agents spawned from this sandbox",
      handler: async () => {
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(routerUrl("/sandbox/list"), (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          if (parsed.error) {
            return { text: `**Error:** ${parsed.error}` };
          }
          const sandboxes = parsed.sandboxes || [];
          if (sandboxes.length === 0) {
            return { text: "No sub-agents spawned yet. Use `/azureclaw-spawn <name>` to create one." };
          }
          return {
            text: [
              `**Sub-Agents** (${sandboxes.length})`,
              "",
              ...sandboxes.map((s: any) =>
                `- **${s.agent_id}** — ${s.phase || "unknown"} (model: ${s.model || "default"}, governance: ${s.governance ? "on" : "off"})`
              ),
              "",
              "Communicate via azureclaw_mesh_send tool (E2E encrypted)",
              "Destroy: `/azureclaw-spawn-destroy <name>`",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not list sub-agents. Is the inference router running?" };
        }
      },
    });

    // ── /azureclaw-spawn-destroy — tear down a sub-agent ──────────────────
    api.registerCommand({
      name: "azureclaw-spawn-destroy",
      description: "Destroy a spawned sub-agent — /azureclaw-spawn-destroy <name>",
      acceptsArgs: true,
      handler: async (ctx) => {
        const name = ctx.args?.trim();
        if (!name) {
          return { text: "Usage: `/azureclaw-spawn-destroy <name>`\n\nUse `/azureclaw-spawn-list` to see your sub-agents." };
        }
        try {
          const http = await import("node:http");
          const result = await new Promise<string>((resolve, reject) => {
            const req = http.request(
              {
                hostname: "127.0.0.1",
                port: 8443,
                path: `/sandbox/${encodeURIComponent(name)}`,
                method: "DELETE",
              },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
            req.end();
          });
          const parsed = JSON.parse(result);
          if (parsed.error) {
            return { text: `**Delete failed:** ${parsed.error}` };
          }
          return { text: `**Destroyed:** ${parsed.agent_id} — ${parsed.message || "teardown in progress"}` };
        } catch {
          return { text: `Could not delete sub-agent '${name}'. Is the inference router running?` };
        }
      },
    });

    // ── /azureclaw-spawn-status — check status of a sub-agent ─────────────
    api.registerCommand({
      name: "azureclaw-spawn-status",
      description: "Check status of a spawned sub-agent — /azureclaw-spawn-status <name>",
      acceptsArgs: true,
      handler: async (ctx) => {
        const name = ctx.args?.trim();
        if (!name) {
          return { text: "Usage: `/azureclaw-spawn-status <name>`\n\nUse `/azureclaw-spawn-list` to see your sub-agents." };
        }
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(routerUrl(`/sandbox/${encodeURIComponent(name)}/status`), (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          if (parsed.error) {
            return { text: `**Not found:** ${parsed.error}` };
          }
          const ready = parsed.phase === "Running";
          return {
            text: [
              `**Sub-Agent: ${parsed.agent_id}**`,
              `Phase: ${parsed.phase || "unknown"} ${ready ? "(ready for mesh)" : "(not ready yet)"}`,
              parsed.namespace ? `Namespace: ${parsed.namespace}` : "",
              "",
              ready
                ? "Send a task via azureclaw_mesh_send tool with to_agent: \"" + name + "\""
                : "Wait for phase=Running before sending mesh messages.",
            ].filter(Boolean).join("\n"),
          };
        } catch {
          return { text: `Could not check status of '${name}'. Is the inference router running?` };
        }
      },
    });
  },
});

export default azureClawPlugin;
