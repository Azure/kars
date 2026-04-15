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
let activeOffload: { requestId: string; startedAt: number } | null = null;

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
      });

      // ── cloud_offload ──
      api.registerTool({
        name: "cloud_offload",
        description:
          "Delegate a task to a governed AzureClaw cloud sandbox. " +
          "The task runs with full GPU/inference capabilities and AGT governance. " +
          "Results are returned via E2E encrypted mesh. Requires prior pairing.",
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
              description: "[Not yet implemented] Workspace files to send to the sandbox",
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

async function meshPairHandler(params: { token: string }): Promise<string> {
  const { token } = params;

  const payload = decodeToken(token);
  if (!payload) {
    return "❌ Invalid pairing token. Must start with azcp_1_ and contain valid data.";
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

  // Collect file metadata
  const filePaths = params.files || [];
  let totalBytes = 0;
  for (const f of filePaths) {
    try {
      const stat = fs.statSync(f);
      totalBytes += stat.size;
    } catch {
      // file doesn't exist or unreadable — skip
    }
  }

  const request: OffloadRequestMessage = {
    type: "offload_request",
    task: params.task,
    files: filePaths,
    file_count: filePaths.length,
    total_bytes: totalBytes,
    preferences: {
      model: params.model,
      timeout_minutes: params.timeout_minutes || 30,
    },
    request_id: requestId,
    timestamp: new Date().toISOString(),
  };

  try {
    await connection.send(activePairing.controllerAmid, request);
  } catch (err: any) {
    return `❌ Failed to send offload request: ${err.message}`;
  }

  activeOffload = { requestId, startedAt: Date.now() };

  // Wait for initial status (validating/spawning)
  let initialStatus: OffloadStatusMessage | OffloadErrorMessage | null = null;
  try {
    initialStatus = await connection.waitForMessage(
      (content) => {
        const msg = content as Record<string, unknown>;
        if (
          (msg?.type === "offload_status" || msg?.type === "offload_error") &&
          msg?.request_id === requestId
        ) {
          return msg as unknown as OffloadStatusMessage | OffloadErrorMessage;
        }
        return null;
      },
      30_000
    );
  } catch {
    // Timeout waiting for initial status — keep going
  }

  if (initialStatus?.type === "offload_error") {
    activeOffload = null;
    const errMsg = initialStatus as OffloadErrorMessage;
    return `❌ Offload rejected: ${errMsg.error} (phase: ${errMsg.phase})`;
  }

  const statusPhase = (initialStatus as OffloadStatusMessage)?.phase || "submitted";
  const statusMsg = (initialStatus as OffloadStatusMessage)?.message || "Request sent to cluster";

  return [
    `☁️ Offload request accepted`,
    "",
    `  Request ID:  ${requestId}`,
    `  Phase:       ${statusPhase}`,
    `  Task:        ${params.task.slice(0, 100)}${params.task.length > 100 ? "..." : ""}`,
    `  Files:       ${filePaths.length} (${(totalBytes / 1024).toFixed(1)} KB)`,
    `  Status:      ${statusMsg}`,
    "",
    "Use offload_status to monitor progress. Results will arrive via mesh.",
  ].join("\n");
}

async function offloadStatusHandler(_params: Record<string, unknown>): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!connection) return "❌ Mesh client not connected.";
  if (!activeOffload) return "No active offload. Use cloud_offload to start one.";

  const inbox = connection.getInbox();
  const requestId = activeOffload.requestId;
  const elapsed = Math.round((Date.now() - activeOffload.startedAt) / 1000);

  // Find latest status for this request
  let latestStatus: OffloadStatusMessage | null = null;
  let latestProgress: OffloadProgressMessage | null = null;
  let doneMsg: OffloadDoneMessage | null = null;
  let errorMsg: OffloadErrorMessage | null = null;

  for (const msg of inbox) {
    const content = msg.content as Record<string, unknown>;
    if (content?.request_id !== requestId) continue;

    switch (content?.type) {
      case "offload_status":
        latestStatus = content as unknown as OffloadStatusMessage;
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
    }
  }

  if (errorMsg) {
    activeOffload = null;
    return `❌ Offload failed (phase: ${errorMsg.phase}): ${errorMsg.error}`;
  }

  if (doneMsg) {
    activeOffload = null;
    const totalTokens = (doneMsg.tokens_used?.prompt || 0) + (doneMsg.tokens_used?.completion || 0);
    return [
      `✅ Offload complete`,
      "",
      `  Duration:     ${doneMsg.duration_seconds}s`,
      `  Tokens used:  ${totalTokens.toLocaleString()}`,
      `  Output files: ${doneMsg.output_files.length > 0 ? doneMsg.output_files.join(", ") : "none"}`,
      "",
      doneMsg.summary,
    ].join("\n");
  }

  if (latestProgress) {
    return `☁️ Running (${latestProgress.pct}%) — ${latestProgress.stage}: ${latestProgress.message} [${elapsed}s]`;
  }

  if (latestStatus) {
    return `☁️ ${latestStatus.phase}: ${latestStatus.message} [${elapsed}s]`;
  }

  return `☁️ Offload in progress... (${elapsed}s elapsed, waiting for status update)`;
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

// Re-export for consumers
export { decodeToken } from "./pairing.js";
export type * from "./types.js";
