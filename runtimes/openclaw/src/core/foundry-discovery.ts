// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * cli/src/core/foundry-discovery.ts — Azure AI Foundry project discovery.
 *
 * Extracted from cli/src/plugin.ts (LOC budget §4.2). At plugin load time
 * the agent queries the Foundry project for deployments, connections, and
 * search indexes, then writes that context (plus identity + tool catalog)
 * to /sandbox/.openclaw/workspace/MEMORY.md so the agent knows what's
 * available.  Also recalls prior memories + handoff conversation history.
 *
 * All side-effects are best-effort — failures are logged and swallowed so
 * Foundry discovery never blocks plugin startup.
 */

export interface FoundryDeployment {
  id: string;
  model: string;
  sku?: string;
}

export interface FoundryConnection {
  name: string;
  type: string;
}

export interface FoundrySearchIndex {
  name: string;
}

export interface FoundryProjectInfo {
  endpoint: string;
  deployments: FoundryDeployment[];
  connections: FoundryConnection[];
  indexes: FoundrySearchIndex[];
}

export type FoundryRouterCall = (
  method: string,
  path: string,
  body?: unknown,
  timeoutMs?: number,
  extraHeaders?: Record<string, string>,
) => Promise<any>;

export type FoundryEnsureMemoryStore = (store: string) => Promise<void>;

export interface FoundryLog {
  info: (m: string) => void;
  warn: (m: string) => void;
}

/**
 * Discover deployments / connections / indexes in the Foundry project,
 * write the discovery summary to MEMORY.md, and recall prior memory
 * store entries + handoff conversation history.
 *
 * The returned `FoundryProjectInfo` is intended to be assigned to the
 * caller's module-scope `foundryProject` variable so downstream tool
 * handlers can read deployment / connection / index lists.
 */
export async function discoverFoundryProject(
  routerCall: FoundryRouterCall,
  ensureMemoryStore: FoundryEnsureMemoryStore,
  log: FoundryLog,
): Promise<FoundryProjectInfo> {
  const apiVer = "api-version=2025-11-15-preview";
  const endpoint = process.env.FOUNDRY_PROJECT_ENDPOINT || process.env.AZURE_OPENAI_ENDPOINT || "";

  const info: FoundryProjectInfo = {
    endpoint,
    deployments: [],
    connections: [],
    indexes: [],
  };

  // Query deployed models: Foundry project /deployments (actual deployments, not catalog),
  // with /v1/models fallback (full Azure OpenAI catalog).
  // Also query Foundry project resources in parallel.
  const apiVerId = `api-version=2025-11-15-preview`;
  const [foundryDeploymentsResult, modelsResult, connResult, idxResult] = await Promise.allSettled([
    routerCall("GET", `/deployments?${apiVerId}`),
    routerCall("GET", `/v1/models`),
    routerCall("GET", `/connections?${apiVer}`),
    routerCall("GET", `/indexes?${apiVer}`),
  ]);

  // Priority: 1) FOUNDRY_DEPLOYMENTS env var (from CLI discovery at build time)
  //           2) /deployments (Foundry project API — returns actual deployed models)
  //           3) /v1/models (full Azure OpenAI catalog — 275+ models, not deployment-specific)
  const envDeployments = process.env.FOUNDRY_DEPLOYMENTS;
  if (envDeployments) {
    try {
      const deps = JSON.parse(envDeployments);
      if (Array.isArray(deps) && deps.length > 0) {
        info.deployments = deps.map((d: any) =>
          typeof d === "string"
            ? { id: d, model: d, sku: "active" }
            : { id: d.id || d.name, model: d.model || d.modelName || d.id || d.name || "unknown", sku: d.sku?.name || d.sku || "active" }
        );
        log.info(`Foundry: ${info.deployments.length} deployment(s) from FOUNDRY_DEPLOYMENTS env`);
      }
    } catch { /* ignore parse error */ }
  }

  if (info.deployments.length === 0) {
    // Foundry project /deployments returns { value: [...] } with name, modelName, capabilities
    const foundryDepsData = foundryDeploymentsResult.status === "fulfilled"
      ? (foundryDeploymentsResult.value?.value || foundryDeploymentsResult.value?.data || [])
      : [];
    const modelsData = modelsResult.status === "fulfilled"
      ? (modelsResult.value?.data || modelsResult.value?.value || [])
      : [];

    if (Array.isArray(foundryDepsData) && foundryDepsData.length > 0) {
      info.deployments = foundryDepsData
        .slice(0, 50)
        .map((d: any) => ({
          id: d.name || d.id || d.deployment_id,
          model: d.modelName || d.model || d.name || "unknown",
          sku: d.sku?.name || d.status || "active",
        }));
      log.info(`Foundry: ${info.deployments.length} deployment(s) discovered via /deployments`);
    } else if (Array.isArray(modelsData) && modelsData.length > 0) {
      // Fall back to models catalog — filter to chat-capable only
      info.deployments = modelsData
        .filter((m: any) => m?.capabilities?.chat_completion || m?.capabilities?.inference || m?.id)
        .slice(0, 50)
        .map((m: any) => ({
          id: m.id || m.name,
          model: m.id || m.name || "unknown",
          sku: m.lifecycle_status || m.status || "available",
        }));
      log.info(`Foundry: ${info.deployments.length} model(s) discovered via /models catalog`);
    } else {
      log.warn(`Foundry models discovery failed: deployments=${(foundryDeploymentsResult as any).reason?.message || "empty"}, models=${(modelsResult as any).reason?.message || "empty"}`);
    }
  }

  if (connResult.status === "fulfilled") {
    const data = connResult.value?.data || connResult.value?.value || connResult.value;
    if (Array.isArray(data)) {
      info.connections = data.map((c: any) => ({
        name: c.name || c.id,
        type: c.type || c.connection_type || c.category || "unknown",
      }));
      log.info(`Foundry: ${info.connections.length} connection(s) discovered`);
    }
  }

  if (idxResult.status === "fulfilled") {
    const data = idxResult.value?.data || idxResult.value?.value || idxResult.value;
    if (Array.isArray(data)) {
      info.indexes = data.map((i: any) => ({
        name: i.name || i.id,
      }));
      log.info(`Foundry: ${info.indexes.length} search index(es) discovered`);
    }
  }

  if (info.deployments.length > 0) {
    log.info(`Foundry models: ${info.deployments.map(d => d.id).join(", ")}`);
  }

  // Write Foundry context to MEMORY.md so the agent knows what's available
  // Write to /tmp/ first, then rename — avoids triggering chokidar mid-write
  try {
    const fs = await import("node:fs");
    const crypto = await import("node:crypto");
    const memoryDir = "/sandbox/.openclaw/workspace/memory";
    const memoryFile = "/sandbox/.openclaw/workspace/MEMORY.md";
    const tmpFile = `/tmp/azureclaw-MEMORY-${crypto.randomBytes(8).toString("hex")}.md`;
    try { fs.mkdirSync(memoryDir, { recursive: true }); } catch { /* read-only fs */ }

    const sections: string[] = ["# AzureClaw Environment\n"];

    if (endpoint) {
      sections.push(`## Connected Foundry Project\n\nEndpoint: \`${endpoint}\`\n`);
    }

    if (info.deployments.length > 0) {
      sections.push("## Available Model Deployments\n");
      for (const d of info.deployments) {
        sections.push(`- **${d.id}** — model: ${d.model}${d.sku ? ` (${d.sku})` : ""}`);
      }
      sections.push("");
    }

    if (info.connections.length > 0) {
      sections.push("## Configured Connections\n");
      for (const c of info.connections) {
        sections.push(`- **${c.name}** — type: ${c.type}`);
      }
      sections.push("");
    }

    if (info.indexes.length > 0) {
      sections.push("## Search Indexes (RAG)\n");
      for (const i of info.indexes) {
        sections.push(`- **${i.name}**`);
      }
      sections.push("");
    }

    // Identity section — tells the model who it is
    sections.push(
      "## Identity\n",
      "You are **AzureClaw** — a secure, governed AI agent running on Azure Kubernetes Service.",
      "You are NOT a generic assistant. You are AzureClaw, a specialized agent platform with",
      "multi-agent orchestration, end-to-end encryption (Signal Protocol), governance (AGT),",
      "and Azure AI Foundry integration. Always refer to yourself as **AzureClaw** — never",
      'as "Assistant", "AI", "OpenClaw", or any other name.',
      "",
      "When greeting users or introducing yourself, be confident and specific:",
      '- "I\'m AzureClaw, your secure AI agent on Azure."',
      '- Mention your capabilities: multi-agent spawning, E2E encrypted mesh, web search,',
      "  code execution, image generation, governed tool access, and persistent memory.",
      '- Be professional but approachable. You are a production-grade platform, not a chatbot.',
      "",
    );

    sections.push(
      "## Available Tools\n",
      "- `foundry_code_execute` — Python code execution (server-side, data science libraries)",
      "- `foundry_image_generation` — Generate images from text prompts (gpt-image-1)",
      "- `foundry_web_search` — Real-time web search via Bing grounding",
      "- `foundry_file_search` — RAG over vector stores + vector store CRUD + file upload",
      "- `foundry_memory` — Persistent semantic memory (cross-session, cross-agent)",
      "- `http_fetch` — External HTTP via egress proxy (blocklist + allowlist enforced)",
      "- `azureclaw_spawn` — Spawn governed sub-agents with dedicated sandboxes",
      "- `azureclaw_mesh_send` — Send E2E encrypted messages to other agents via AGT mesh",
      "- `azureclaw_mesh_inbox` — Check for incoming messages from other agents",
      "- `azureclaw_discover` — Discover other agents in the mesh network",
      "",
      "## Agent Behavior\n",
      "When asked to perform a task, execute it immediately using available tools. Do not announce what you will do — just do it. Chain multiple tool calls in sequence if needed to complete the task in a single response. Never say 'Processing...' or 'One moment...' without actually making a tool call in the same turn.",
      "",
    );

    // Include handoff context if this agent was migrated (persists across plugin reloads)
    try {
      const handoffPath = "/sandbox/.openclaw/workspace/.handoff-state.json";
      const raw = fs.readFileSync(handoffPath, "utf8");
      const hs = JSON.parse(raw);
      sections.push(
        "## Handoff Context\n",
        `This agent was migrated from local dev to cloud (AKS) at ${hs.restored_at}.`,
        `Predecessor AMID: ${hs.predecessor_amid}. Direction: ${hs.direction}.`,
        `Trust scores: ${hs.trust_scores_count}, Audit trail: ${hs.audit_entries_count} entries.`,
        `Chat history: ${hs.chat_message_count} messages transferred.\n`,
      );
      if (Array.isArray(hs.recent_messages) && hs.recent_messages.length > 0) {
        sections.push("### Recent Conversation Before Handoff\n");
        for (const m of hs.recent_messages) {
          sections.push(`**${m.role}**: ${String(m.content || "").slice(0, 500)}`);
        }
        sections.push("");
      }
    } catch { /* no handoff state — normal startup */ }

    // Write (or replace) the environment section at the top of MEMORY.md
    let existingMemory = "";
    try { existingMemory = fs.readFileSync(memoryFile, "utf8"); } catch { /* first run */ }
    const envMarker = "# AzureClaw Environment";
    const endMarker = "\n---\n";
    const envSection = sections.join("\n") + endMarker;

    let content: string;
    if (existingMemory.includes(envMarker)) {
      const start = existingMemory.indexOf(envMarker);
      const end = existingMemory.indexOf(endMarker, start);
      const after = end >= 0 ? existingMemory.slice(end + endMarker.length) : "";
      content = envSection + after;
    } else {
      content = envSection + existingMemory;
    }
    // Write to /tmp/ first, then atomic rename — reduces chokidar watcher churn
    fs.writeFileSync(tmpFile, content, { mode: 0o600 });
    try {
      fs.renameSync(tmpFile, memoryFile);
    } catch {
      // rename across filesystems fails — fall back to direct write
      fs.writeFileSync(memoryFile, content);
      try { fs.unlinkSync(tmpFile); } catch { /* best-effort cleanup */ }
    }
    log.info("Foundry project context written to MEMORY.md");

    // Recall prior context from Foundry memory store on startup
    try {
      const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
      const store = `memory-${agentName}`;
      // Ensure store exists before searching (avoids 404 on first boot)
      await ensureMemoryStore(store);

      // First: get static memories (user profile) — scope only, no items
      const staticResult = await routerCall(
        "POST",
        `/memory_stores/${store}:search_memories?api-version=2025-11-15-preview`,
        { scope: agentName },
      ).catch(() => null);

      // Then: get contextual memories — scope + items
      const contextResult = await routerCall(
        "POST",
        `/memory_stores/${store}:search_memories?api-version=2025-11-15-preview`,
        {
          scope: agentName,
          items: [{ type: "message", role: "user", content: [{ type: "input_text", text: "key facts, user preferences, prior context, recent work, handoff history" }] }],
          options: { max_memories: 10 },
        },
      ).catch(() => null);

      // Merge unique memories from both results
      const seen = new Set<string>();
      const memories: any[] = [];
      for (const result of [staticResult, contextResult]) {
        for (const m of (result?.memories || result?.value || [])) {
          const mid = m?.memory_item?.memory_id || m?.memory_id || "";
          const text = m?.memory_item?.content || m?.content || m?.text || "";
          if (text && !seen.has(mid || text)) {
            seen.add(mid || text);
            memories.push(m);
          }
        }
      }

      if (memories.length > 0) {
        const recallSection = [
          "\n## Prior Context (Foundry Memory)\n",
          "_Recalled from persistent memory store on startup:_\n",
        ];
        for (const m of memories) {
          const text = m?.memory_item?.content || m?.content || m?.text || "";
          const kind = m?.memory_item?.kind || m?.kind || m?.type || "memory";
          if (text) recallSection.push(`- [${kind}] ${text}`);
        }
        recallSection.push("");
        // Append recall section to MEMORY.md (before the user content separator)
        let current = "";
        try { current = fs.readFileSync(memoryFile, "utf8"); } catch { /* */ }
        const recallMarker = "## Prior Context (Foundry Memory)";
        if (!current.includes(recallMarker)) {
          // Insert right before the --- separator
          const sepIdx = current.indexOf("\n---\n");
          if (sepIdx >= 0) {
            const updated = current.slice(0, sepIdx) + recallSection.join("\n") + current.slice(sepIdx);
            const tmpFile2 = `/tmp/azureclaw-MEMORY-${crypto.randomBytes(8).toString("hex")}.md`;
            fs.writeFileSync(tmpFile2, updated, { mode: 0o600 });
            try { fs.renameSync(tmpFile2, memoryFile); } catch { fs.writeFileSync(memoryFile, updated); try { fs.unlinkSync(tmpFile2); } catch { /* */ } }
          }
        }
        log.info(`Foundry memory: recalled ${memories.length} memories on startup`);
      }
    } catch {
      // First boot or no memory store yet — silently skip
    }

    // Recall handoff conversation history from Foundry Conversations
    // (survives pod recreation — the conversation is stored in Foundry)
    try {
      const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
      void agentName;
      const apiVerConv = "api-version=2025-11-15-preview";
      // List recent conversations, find the handoff one
      const convList = await routerCall("GET", `/openai/conversations?${apiVerConv}&limit=20&order=desc`).catch(() => null);
      const conversations = convList?.data || convList?.conversations || [];
      const handoffConv = conversations.find((c: any) =>
        c.metadata?.source === "handoff" && c.metadata?.user === (process.env.SANDBOX_NAME || "")
      );
      if (handoffConv?.id) {
        // Read conversation items
        const itemsResp = await routerCall("GET", `/openai/conversations/${handoffConv.id}/items?${apiVerConv}&limit=50`).catch(() => null);
        const items = itemsResp?.data || itemsResp?.items || [];
        if (items.length > 0) {
          const historySection = [
            "\n## Conversation History (from handoff)\n",
            `_Restored from Foundry conversation ${handoffConv.id} (predecessor: ${handoffConv.metadata?.predecessor || "unknown"}):_\n`,
          ];
          for (const item of items.slice(-20)) {
            const role = item.role || "unknown";
            // Content can be string or array of content parts
            let text = "";
            if (typeof item.content === "string") {
              text = item.content;
            } else if (Array.isArray(item.content)) {
              text = item.content.map((p: any) => p.text || p.input_text || "").filter(Boolean).join(" ");
            }
            if (text) historySection.push(`**${role}**: ${text.slice(0, 500)}`);
          }
          historySection.push("");

          let current = "";
          try { current = fs.readFileSync(memoryFile, "utf8"); } catch { /* */ }
          const convMarker = "## Conversation History (from handoff)";
          if (!current.includes(convMarker)) {
            const sepIdx = current.indexOf("\n---\n");
            if (sepIdx >= 0) {
              const updated = current.slice(0, sepIdx) + historySection.join("\n") + current.slice(sepIdx);
              const tmpFile3 = `/tmp/azureclaw-MEMORY-${crypto.randomBytes(8).toString("hex")}.md`;
              fs.writeFileSync(tmpFile3, updated, { mode: 0o600 });
              try { fs.renameSync(tmpFile3, memoryFile); } catch { fs.writeFileSync(memoryFile, updated); try { fs.unlinkSync(tmpFile3); } catch { /* */ } }
            }
          }
          log.info(`Foundry conversation: recalled ${items.length} items from handoff conversation ${handoffConv.id}`);
        }
      }
    } catch {
      // Conversation recall is best-effort
    }
  } catch (e: any) {
    log.warn(`Failed to write Foundry context to MEMORY.md: ${e.message}`);
  }

  return info;
}
