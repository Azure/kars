// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Background handoff orchestration — extracted from plugin.ts in S15.f.7.
//
// runHandoffOrchestration executes the multi-step transfer of an agent's
// state to a successor (cloud → local or local → cloud): encrypted state
// snapshot, sub-agent state collection over the mesh, sub-agent registry
// re-registration under the successor, and ratchet-to-successor signal.
//
// State the function needs (handoffProgress tracker, mesh client + identity,
// the AGT inbox) is owned by plugin.ts singletons and threaded via a deps
// bag — same pattern as agt-task-loop.ts (S15.f.6) and agt-offload.ts
// (S15.f.5).

import { amidToName, nameToAmid } from "./amid-cache.js";
import { routerCall, routerCallStrict } from "./router-client.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshClient = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshIdentity = any;
type Logger = { info: (m: string) => void; warn: (m: string) => void };

export interface HandoffProgress {
  phase: string;
  status: "running" | "complete" | "error" | "partial";
  steps: string[];
  direction?: string;
  started_at: string;
  updated_at: string;
  error?: string;
  result?: Record<string, unknown>;
}

export interface AgtInboxEntry {
  from_amid: string;
  from_agent: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  content: any;
  timestamp: string;
  id: string;
  message_type?: string;
}

export interface HandoffDeps {
  /** Mutable progress tracker. Mutations propagate to the caller. */
  progress: HandoffProgress;
  /** Mutable inbox; orchestration uses splice/findIndex on it. */
  inbox: AgtInboxEntry[];
  meshClient: () => AnyMeshClient | null;
  identity: () => AnyMeshIdentity | null;
  /** Auto-chunking mesh-send wrapper from plugin.ts. */
  meshSend: (
    client: AnyMeshClient,
    target: string,
    msg: Record<string, unknown>,
    log?: Logger,
  ) => Promise<string | undefined>;
  log: Logger;
}

export async function runHandoffOrchestration(
  handoffToken: string,
  adminToken: string,
  direction: string,
  dirLabel: string,
  deps: HandoffDeps,
): Promise<void> {
  const { progress: handoffProgress, inbox: agtInbox, log: _log } = deps;

  // _hp closure — same as plugin.ts _hp helper but binds to deps.
  const _hp = (phase: string, step: string): void => {
    handoffProgress.phase = phase;
    handoffProgress.steps.push(step);
    handoffProgress.updated_at = new Date().toISOString();
    _log.info(`Handoff [${phase}]: ${step}`);
  };

  // Local accessors for hot paths — keeps the body byte-identical to the
  // plugin.ts original (which read `agtMeshClient` / `agtIdentity` directly).
  const _routerCall = routerCall;
  const _routerCallStrict = routerCallStrict;

  // BEGIN body — copied verbatim from plugin.ts. Only changes:
  //   - `agtMeshClient` → `deps.meshClient()`
  //   - `agtIdentity` → `deps.identity()`
  // (handoffProgress and agtInbox are bound above as aliases.)

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
      if (deps.meshClient() && deps.identity()) {
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
            await deps.meshClient().send(subAmid, {
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
            await deps.meshClient().send(subAmid, {
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
  const myAmid = deps.meshClient()?.getAmid?.() || deps.identity()?.amid;
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
  if (!deps.meshClient() || !deps.identity()) {
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
      await deps.meshSend(deps.meshClient(), targetAmid, handoffMessage, _log);
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
        await deps.meshSend(deps.meshClient(), targetAmid, handoffMessage, _log);
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
