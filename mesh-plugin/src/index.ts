/**
 * @azureclaw/mesh — OpenClaw plugin for mesh federation.
 *
 * Enables any OpenClaw agent to:
 * 1. Pair with a trusted AzureClaw cluster (one-time)
 * 2. Offload tasks to governed cloud sandboxes
 * 3. Communicate with other mesh agents (send/inbox/discover)
 *
 * Dependencies: ws (WebSocket client)
 * No Docker, no Rust, no AzureClaw CLI required.
 */

import { loadOrCreateIdentity, getIdentityPath, type MeshIdentity } from "./identity.js";
import {
  decodeToken,
  savePairing,
  getDefaultPairing,
  type StoredPairing,
} from "./pairing.js";
import { MeshConnection } from "./connection.js";
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

let connection: MeshConnection | null = null;
let meshIdentity: MeshIdentity | null = null;
let activePairing: StoredPairing | null = null;
let initialized = false;
let activeOffload: {
  requestId: string;
  startedAt: number;
  sandboxName?: string;
  sandboxAmid?: string;
  filesSent?: number;
  phase: string;
} | null = null;

// Directory to save incoming files from sandboxes
const INCOMING_DIR = path.join(
  process.env.HOME || process.env.USERPROFILE || "/tmp",
  ".azureclaw-mesh",
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
    id: "azureclaw-mesh",
    name: "AzureClaw Mesh",
    register(api: any) {
      // ── mesh_pair ──
      api.registerTool({
        name: "mesh_pair",
        description:
          "One-time pairing with an AzureClaw cluster using an admin-provided token. " +
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
          "Delegate a task to a governed AzureClaw cloud sandbox. " +
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
          },
          required: ["task"],
        },
        handler: cloudOffloadHandler,
        execute: cloudOffloadHandler,
      });

      // ── offload_status ──
      api.registerTool({
        name: "offload_status",
        description:
          "Check the status of an active cloud offload. " +
          "Returns current phase, progress percentage, and status message.",
        parameters: {
          type: "object",
          properties: {},
        },
        handler: offloadStatusHandler,
        execute: offloadStatusHandler,
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
          "Read incoming messages from the E2E encrypted mesh inbox.",
        parameters: {
          type: "object",
          properties: {
            limit: {
              type: "number",
              description: "Max messages to return (default: 10)",
            },
          },
        },
        handler: meshInboxHandler,
        execute: meshInboxHandler,
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
    },
  };
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

async function ensureInitialized(): Promise<string | null> {
  if (initialized) return null;

  meshIdentity = loadOrCreateIdentity();
  activePairing = getDefaultPairing();

  if (activePairing) {
    try {
      connection = new MeshConnection({
        relayUrl: activePairing.relayUrl,
        registryUrl: activePairing.registryUrl,
        identity: meshIdentity,
      });
      await connection.connect();
      initialized = true;
    } catch (err: any) {
      return `Failed to connect to mesh: ${err.message}`;
    }
  }

  initialized = true;
  return null;
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

async function meshPairHandler(params: any): Promise<string> {
  // OpenClaw may pass args in various shapes — extract token defensively
  const token: string | undefined =
    params?.token ??
    params?.arguments?.token ??
    params?.params?.token ??
    (typeof params === "string" ? params : undefined);

  if (!token) {
    return `❌ No token provided. Pass a pairing token starting with azcp_1_. (received keys: ${JSON.stringify(Object.keys(params ?? {}))})`;
  }

  const payload = decodeToken(token);
  if (!payload) {
    return `❌ Invalid pairing token. Must start with azcp_1_ and contain valid data. (token length: ${token.length}, starts: ${token.slice(0, 20)}...)`;
  }

  meshIdentity = loadOrCreateIdentity();
  const identityPath = getIdentityPath();

  // Connect to relay
  try {
    connection = new MeshConnection({
      relayUrl: payload.relay_url,
      registryUrl: payload.registry_url,
      identity: meshIdentity,
    });
    await connection.connect();
  } catch (err: any) {
    return `❌ Failed to connect to mesh relay: ${err.message}`;
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

  // Wait for pair_response (up to 15s)
  let response: PairResponseMessage | null = null;
  try {
    response = await connection.waitForMessage<PairResponseMessage>(
      (content) => {
        const msg = content as Record<string, unknown>;
        if (msg?.type === "pair_response") return msg as unknown as PairResponseMessage;
        return null;
      },
      15_000
    );
  } catch {
    return "❌ Pairing timed out — no response from controller after 15s. Is the cluster online?";
  }

  if (!response.success) {
    return `❌ Pairing rejected: ${response.error || "Unknown error"}`;
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
    `✅ Paired successfully with AzureClaw cluster "${clusterName}"`,
    "",
    `  Your AMID:     ${meshIdentity.amid}`,
    `  Identity:      ${identityPath}`,
    `  Budget:        ${budgetStr} tokens`,
    `  Capabilities:  ${(response.capabilities_granted || []).join(", ")}`,
    `  Expires:       ${response.expires_at || "—"}`,
    "",
    "You can now use cloud_offload to delegate tasks to the cloud.",
  ].join("\n");
}

async function cloudOffloadHandler(params: {
  task: string;
  files?: string[];
  model?: string;
  timeout_minutes?: number;
}): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!activePairing) return "❌ Not paired with any AzureClaw cluster. Use mesh_pair first.";
  if (!connection?.isConnected) return "❌ Mesh connection lost. Reconnecting...";
  if (activeOffload) return `❌ Offload already in progress (${activeOffload.requestId}). Use offload_status to check.`;

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

  // Phase 1: Send offload request to controller (matchmaker)
  try {
    await connection.send(activePairing.controllerAmid, request);
  } catch (sendErr: any) {
    return `❌ Failed to send offload request: ${sendErr.message}`;
  }

  activeOffload = { requestId, startedAt: Date.now(), phase: "submitted" };

  // Phase 2: Wait for controller to spawn sandbox and report "ready" with sandbox_name.
  // Controller flow: validate pairing → create CRD → wait for pod Running → send sandbox_name.
  // This can take up to ~5 minutes (image pull, scheduling).
  const readyTimeoutMs = 5 * 60 * 1000;
  let sandboxName: string | null = null;

  try {
    const readyStatus = await connection.waitForMessage<OffloadStatusMessage>(
      (content) => {
        const msg = content as Record<string, unknown>;
        if (msg?.request_id !== requestId) return null;

        // Handle errors at any phase
        if (msg?.type === "offload_error") {
          throw new Error(`${msg.error} (phase: ${msg.phase})`);
        }

        // Absorb intermediate statuses (validating, spawning, scheduled)
        if (msg?.type === "offload_status") {
          activeOffload!.phase = String(msg.phase);
          if (msg.phase === "ready" && msg.sandbox_name) {
            return msg as unknown as OffloadStatusMessage;
          }
        }
        return null;
      },
      readyTimeoutMs,
    );
    sandboxName = readyStatus.sandbox_name || null;
  } catch (waitErr: any) {
    activeOffload = null;
    return `❌ Offload failed: ${waitErr.message}`;
  }

  if (!sandboxName) {
    activeOffload = null;
    return "❌ Controller reported ready but no sandbox name — cannot proceed.";
  }

  activeOffload.sandboxName = sandboxName;
  activeOffload.phase = "connecting";

  // Phase 3: Discover sandbox AMID via registry
  let sandboxAmid: string | null = null;
  const discoveryRetries = 10;
  for (let i = 0; i < discoveryRetries; i++) {
    sandboxAmid = await connection.resolveAmid(sandboxName);
    if (sandboxAmid) break;
    await new Promise((r) => setTimeout(r, 3000)); // sandbox may still be registering
  }

  if (!sandboxAmid) {
    activeOffload = null;
    return `❌ Could not find sandbox '${sandboxName}' on the mesh after ${discoveryRetries} attempts. It may have failed to start.`;
  }

  activeOffload.sandboxAmid = sandboxAmid;

  // Phase 4: Send files directly to sandbox (if any)
  if (validFiles.length > 0) {
    activeOffload.phase = "uploading";
    activeOffload.filesSent = 0;

    for (const filePath of validFiles) {
      const fileName = path.basename(filePath);
      try {
        const ack = await connection.sendFile(sandboxAmid, filePath, {
          description: `Offload file for request ${requestId}`,
          timeoutMs: 30_000,
          retries: 3,
        });

        if (!ack.success) {
          activeOffload = null;
          return `❌ File transfer failed for '${fileName}': ${ack.error || "no ACK"}`;
        }

        activeOffload.filesSent = (activeOffload.filesSent || 0) + 1;
      } catch (ftErr: any) {
        activeOffload = null;
        return `❌ File transfer error for '${fileName}': ${ftErr.message}`;
      }
    }
  }

  // Phase 5: Send the task to sandbox directly via mesh
  activeOffload.phase = "running";
  try {
    await connection.send(sandboxAmid, {
      type: "offload_task",
      request_id: requestId,
      task: params.task,
      files: validFiles.map((f) => path.basename(f)),
      from_agent: connection.amid,
      timestamp: new Date().toISOString(),
    });
  } catch (taskErr: any) {
    activeOffload = null;
    return `❌ Failed to send task to sandbox: ${taskErr.message}`;
  }

  // Return immediately — sandbox works asynchronously.
  // Results arrive via mesh (offload_done, file_transfer) and are picked up by offload_status.
  const filesInfo = validFiles.length > 0
    ? `${validFiles.length} file(s) (${(totalBytes / 1024).toFixed(1)} KB) uploaded`
    : "no files";

  return [
    `☁️ Offload running`,
    "",
    `  Request ID:  ${requestId}`,
    `  Sandbox:     ${sandboxName}`,
    `  Task:        ${params.task.slice(0, 100)}${params.task.length > 100 ? "..." : ""}`,
    `  Files:       ${filesInfo}`,
    `  Timeout:     ${params.timeout_minutes || 30}m`,
    "",
    "The sandbox is working on your task. Use offload_status to check progress.",
    "Results and output files will arrive automatically via mesh.",
  ].join("\n");
}

async function offloadStatusHandler(_params: Record<string, unknown>): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";
  if (!activeOffload) return "No active offload. Use cloud_offload to start one.";

  const inbox = connection.getInbox();
  const requestId = activeOffload.requestId;
  const sandboxAmid = activeOffload.sandboxAmid;
  const elapsed = Math.round((Date.now() - activeOffload.startedAt) / 1000);

  // Scan inbox for messages from the sandbox or controller about this offload
  let latestProgress: OffloadProgressMessage | null = null;
  let doneMsg: OffloadDoneMessage | null = null;
  let errorMsg: OffloadErrorMessage | null = null;
  const receivedFiles: string[] = [];

  for (const msg of inbox) {
    const content = msg.content as Record<string, unknown>;
    if (!content) continue;

    // Match by request_id or by sender (sandbox AMID)
    const matchesRequest = content.request_id === requestId;
    const matchesSandbox = sandboxAmid && msg.from === sandboxAmid;

    if (!matchesRequest && !matchesSandbox) continue;

    switch (content.type) {
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
        // Auto-save incoming file from sandbox
        if (matchesSandbox && content.file_name) {
          try {
            const result = await connection!.handleFileTransfer(
              msg.from,
              content as Record<string, unknown>,
              INCOMING_DIR,
            );
            if (result) {
              receivedFiles.push(`${result.fileName} (${(result.sizeBytes / 1024).toFixed(1)} KB) → ${result.savedPath}`);
            }
          } catch { /* best effort */ }
        }
        break;
    }
  }

  if (errorMsg) {
    activeOffload = null;
    return `❌ Offload failed (phase: ${errorMsg.phase}): ${errorMsg.error}`;
  }

  if (doneMsg) {
    activeOffload = null;
    const totalTokens = (doneMsg.tokens_used?.prompt || 0) + (doneMsg.tokens_used?.completion || 0);
    const lines = [
      `✅ Offload complete`,
      "",
      `  Duration:     ${doneMsg.duration_seconds}s`,
      `  Tokens used:  ${totalTokens.toLocaleString()}`,
    ];
    if (doneMsg.output_files.length > 0) {
      lines.push(`  Output files: ${doneMsg.output_files.join(", ")}`);
    }
    if (receivedFiles.length > 0) {
      lines.push(`  Saved files:  ${receivedFiles.length}`);
      for (const f of receivedFiles) {
        lines.push(`    • ${f}`);
      }
    }
    lines.push("", doneMsg.summary);
    return lines.join("\n");
  }

  if (latestProgress) {
    return `☁️ Running (${latestProgress.pct}%) — ${latestProgress.stage}: ${latestProgress.message} [${elapsed}s]`;
  }

  // Show current phase from the offload tracker
  const phase = activeOffload.phase || "running";
  const sandbox = activeOffload.sandboxName || "pending";
  const filesInfo = activeOffload.filesSent
    ? `${activeOffload.filesSent} file(s) sent`
    : "";

  return [
    `☁️ Offload in progress [${elapsed}s]`,
    `  Phase:   ${phase}`,
    `  Sandbox: ${sandbox}`,
    filesInfo ? `  Files:   ${filesInfo}` : "",
    "",
    "Waiting for results from sandbox...",
  ].filter(Boolean).join("\n");
}

async function meshSendHandler(params: {
  to: string;
  message: string;
}): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection?.isConnected) return "❌ Mesh client not connected. Pair first with mesh_pair.";

  try {
    await connection.send(params.to, { type: "message", content: params.message });
    return `✓ Message sent to ${params.to} (E2E encrypted)`;
  } catch (err: any) {
    return `❌ Send failed: ${err.message}`;
  }
}

async function meshInboxHandler(params: { limit?: number }): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";

  const limit = params.limit || 10;
  const messages = connection.getInbox(limit);

  if (messages.length === 0) return "📭 Inbox empty.";

  const lines = messages.map((m, i) => {
    const content =
      typeof m.content === "string"
        ? m.content.slice(0, 200)
        : JSON.stringify(m.content).slice(0, 200);
    return `  ${i + 1}. [${m.from}] ${content}`;
  });

  return `📬 ${messages.length} message(s):\n${lines.join("\n")}`;
}

async function discoverHandler(params: {
  capability?: string;
  limit?: number;
}): Promise<string> {
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
export type * from "./types.js";
