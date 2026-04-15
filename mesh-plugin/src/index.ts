/**
 * @azureclaw/mesh — OpenClaw plugin for mesh federation.
 *
 * Enables any OpenClaw agent to:
 * 1. Pair with a trusted AzureClaw cluster (one-time)
 * 2. Offload tasks to governed cloud sandboxes
 * 3. Handoff full agent state to the cloud and recall it
 * 4. Communicate with other mesh agents (send/inbox/discover)
 *
 * Dependencies: @agentmesh/sdk (+ libsodium-wrappers)
 * No Docker, no Rust, no AzureClaw CLI required.
 */

import { loadOrCreateIdentity, getIdentityPath } from "./identity.js";
import {
  decodeToken,
  savePairing,
  getDefaultPairing,
  type StoredPairing,
} from "./pairing.js";
import type {
  PairRequestMessage,
  PairResponseMessage,
  OffloadRequestMessage,
  OffloadStatusMessage,
  OffloadProgressMessage,
  OffloadDoneMessage,
} from "./types.js";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let meshClient: any = null;
let meshIdentity: ReturnType<typeof loadOrCreateIdentity> | null = null;
let activePairing: StoredPairing | null = null;
let initialized = false;

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
              description: "Workspace files to send to the sandbox (relative paths)",
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
      const sdk = await import("@agentmesh/sdk");
      meshClient = await sdk.AgentMeshClient.create({
        registryUrl: activePairing.registryUrl,
        relayUrl: activePairing.relayUrl,
      } as any);
      await meshClient.connect(meshIdentity!.amid, {
        displayName: `external-${meshIdentity!.amid.slice(0, 8)}`,
        capabilities: ["external", "offload"],
      });
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

  // Decode token
  const payload = decodeToken(token);
  if (!payload) {
    return "❌ Invalid pairing token. Must start with azcp_1_ and contain valid data.";
  }

  // Generate/load identity
  meshIdentity = loadOrCreateIdentity();
  const identityPath = getIdentityPath();

  // Connect to mesh
  let sdk: any;
  try {
    sdk = await import("@agentmesh/sdk");
  } catch {
    return "❌ @agentmesh/sdk not found. Install with: npm install @agentmesh/sdk";
  }

  try {
    meshClient = await sdk.AgentMeshClient.create({
      registryUrl: payload.registry_url,
      relayUrl: payload.relay_url,
    } as any);
    await meshClient.connect(meshIdentity.amid, {
      displayName: `external-${meshIdentity.amid.slice(0, 8)}`,
      capabilities: ["external", "offload"],
    });
  } catch (err: any) {
    return `❌ Failed to connect to mesh: ${err.message}`;
  }

  // Send pair_request to controller
  const pairRequest: PairRequestMessage = {
    type: "pair_request",
    secret: payload.secret,
    pubkey_ed25519: meshIdentity.signingPublicKey.toString("base64"),
    display_name: `external-${meshIdentity.amid.slice(0, 8)}`,
    capabilities_requested: ["offload", "handoff"],
  };

  try {
    await meshClient.send(payload.controller_amid, pairRequest);
  } catch (err: any) {
    return `❌ Failed to send pair request: ${err.message}`;
  }

  // Wait for pair_response (up to 15s)
  let response: PairResponseMessage | null = null;
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 500));
    const messages = await meshClient.getInbox?.() || [];
    for (const msg of messages) {
      const content = typeof msg.content === "string" ? JSON.parse(msg.content) : msg.content;
      if (content?.type === "pair_response") {
        response = content as PairResponseMessage;
        break;
      }
    }
    if (response) break;
  }

  if (!response) {
    return "❌ Pairing timed out — no response from controller after 15s. Is the cluster online?";
  }

  if (!response.success) {
    return `❌ Pairing rejected: ${response.error || "Unknown error"}`;
  }

  // Save pairing
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
  if (!meshClient) return "❌ Mesh client not connected.";

  const requestId = crypto.randomUUID();
  const request: OffloadRequestMessage = {
    type: "offload_request",
    task: params.task,
    files: params.files || [],
    file_count: (params.files || []).length,
    total_bytes: 0, // Will be calculated when sending files
    preferences: {
      model: params.model,
      timeout_minutes: params.timeout_minutes || 30,
    },
    request_id: requestId,
    timestamp: new Date().toISOString(),
  };

  try {
    await meshClient.send(activePairing.controllerAmid, request);
  } catch (err: any) {
    return `❌ Failed to send offload request: ${err.message}`;
  }

  // TODO: Phase 5 — upload workspace files via file_transfer protocol
  // TODO: Phase 5 — monitor progress and receive results

  return [
    `☁️ Offload request sent to ${activePairing.clusterName}`,
    "",
    `  Request ID:  ${requestId}`,
    `  Task:        ${params.task.slice(0, 100)}${params.task.length > 100 ? "..." : ""}`,
    `  Files:       ${(params.files || []).length}`,
    "",
    "Use offload_status to check progress.",
  ].join("\n");
}

async function offloadStatusHandler(_params: Record<string, unknown>): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!meshClient) return "❌ Mesh client not connected.";

  // Check inbox for status/progress/done messages
  const messages = await meshClient.getInbox?.() || [];
  const statusMsgs: (OffloadStatusMessage | OffloadProgressMessage | OffloadDoneMessage)[] = [];

  for (const msg of messages) {
    try {
      const content = typeof msg.content === "string" ? JSON.parse(msg.content) : msg.content;
      if (
        content?.type === "offload_status" ||
        content?.type === "offload_progress" ||
        content?.type === "offload_done"
      ) {
        statusMsgs.push(content);
      }
    } catch {
      // skip unparseable
    }
  }

  if (statusMsgs.length === 0) {
    return "No active offloads or status updates.";
  }

  const latest = statusMsgs[statusMsgs.length - 1];
  if (latest.type === "offload_done") {
    const done = latest as OffloadDoneMessage;
    return [
      `✅ Offload complete`,
      "",
      `  Duration:     ${done.duration_seconds}s`,
      `  Tokens used:  ${done.tokens_used.prompt + done.tokens_used.completion}`,
      `  Output files: ${done.output_files.join(", ")}`,
      "",
      done.summary,
    ].join("\n");
  }

  if (latest.type === "offload_progress") {
    const prog = latest as OffloadProgressMessage;
    return `☁️ Running (${prog.pct}%) — ${prog.stage}: ${prog.message}`;
  }

  const status = latest as OffloadStatusMessage;
  return `☁️ ${status.phase}: ${status.message}`;
}

async function meshSendHandler(params: {
  to: string;
  message: string;
}): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!meshClient) return "❌ Mesh client not connected. Pair first with mesh_pair.";

  try {
    await meshClient.send(params.to, { type: "message", content: params.message });
    return `✓ Message sent to ${params.to} (E2E encrypted)`;
  } catch (err: any) {
    return `❌ Send failed: ${err.message}`;
  }
}

async function meshInboxHandler(params: { limit?: number }): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!meshClient) return "❌ Mesh client not connected.";

  const limit = params.limit || 10;
  const messages = (await meshClient.getInbox?.() || []).slice(-limit);

  if (messages.length === 0) return "📭 Inbox empty.";

  const lines = messages.map((m: any, i: number) => {
    const from = m.from || m.sender || "unknown";
    const content =
      typeof m.content === "string"
        ? m.content.slice(0, 200)
        : JSON.stringify(m.content).slice(0, 200);
    return `  ${i + 1}. [${from}] ${content}`;
  });

  return `📬 ${messages.length} message(s):\n${lines.join("\n")}`;
}

async function discoverHandler(params: {
  capability?: string;
  limit?: number;
}): Promise<string> {
  const err = await ensureInitialized();
  if (err) return `❌ ${err}`;
  if (!meshClient) return "❌ Mesh client not connected.";

  try {
    const results = await meshClient.discover?.({
      capability: params.capability,
      limit: params.limit || 20,
    }) || [];

    if (results.length === 0) return "No agents found on the mesh.";

    const lines = results.map((a: any) =>
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
