// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// AGT in-process tool-calling loop — extracted from plugin.ts in S15.f.6.
//
// Drives a sub-agent's task execution against the inference router using the
// TASK_TOOLS schema (S15.f.4 module). Handles 25 tool-calling rounds, with
// handoff-interrupt checkpointing that saves progress to disk if the parent
// requests a handoff mid-task.
//
// Dependencies that change at runtime (mesh client, handoff interrupt flags)
// are threaded via a `TaskLoopDeps` bag because they are still owned by
// plugin.ts singletons. Pure imports (TASK_TOOLS, routerUrl, resolveAmidByName,
// sanitizeLog) are pulled directly from sibling core modules.

import type { AgtInboxEntry } from "./agt-handoff.js";
import { TASK_TOOLS } from "./agt-task-tools.js";
import { resolveAmidByName, getStaleAmid } from "./amid-cache.js";
import { sanitizeLog } from "./log-redact.js";
import { validateMeshPayload } from "./mesh-payload-guard.js";
import { routerUrl } from "./router-client.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshClient = any;
type Logger = { info: (m: string) => void; warn: (m: string) => void };

export interface TaskLoopDeps {
  /** Returns the current AGT mesh client, or null if not connected. */
  meshClient: () => AnyMeshClient | null;
  /** True if a handoff interrupt has been requested for this task. */
  isInterruptRequested: () => boolean;
  /** Read the textual reason for the current interrupt (e.g. "cli_handoff"). */
  interruptReason: () => string;
  /** Mark the interrupt as latched (so we do not re-fire on the next round). */
  setInterrupt: (requested: boolean, reason: string) => void;
  /**
   * Shared AGT inbox buffer (same array as parent-tools' deps.inbox in
   * agt-tools/agt.ts). Direct reference — pushInbox mutates it in place.
   * The sub-agent task-loop's `mesh_inbox` reads from this so messages
   * received between sub-agent boot and the start of a task_request
   * session are visible to the LLM. Without this, two concurrent
   * processTaskWithTools sessions could not exchange peer artifacts.
   */
  inbox?: AgtInboxEntry[];
  /**
   * Notify diagnostics that entries were marked read. Mirrors
   * AgtToolsDeps.markRead so inboxStats.read_total stays in sync
   * regardless of which tool path consumed the messages.
   */
  markRead?: (ids: string[]) => void;
}

export async function processTaskWithTools(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  taskContent: any,
  deps: TaskLoopDeps,
  log: Logger,
): Promise<string> {
  const http = await import("node:http");
  const { execSync } = await import("node:child_process");
  const model = process.env.MODEL || "gpt-4.1";

  const tools = TASK_TOOLS;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const messages: Array<{ role: string; content?: string; tool_calls?: any[]; tool_call_id?: string; name?: string }> = [
    {
      role: "system",
      content: process.env.OFFLOAD_REQUEST_ID
        ? "You are an AzureClaw OFFLOAD WORKER — a short-lived sandboxed agent executing ONE task on behalf of a remote parent agent. Always identify as an AzureClaw offload worker. Your tools:\n- file_write: write text content directly to a file (USE THIS for all artifacts — never use shell redirection)\n- exec_command: run shell commands (read-only ops are fine; DO NOT use `>`, `>>`, `<<`, `<<<` — the shell policy blocks redirection. Use file_write instead.)\n- http_fetch: HTTP requests through security proxy (egress-controlled)\n- foundry_web_search: real-time web search via Bing grounding\n- foundry_code_execute: run Python code server-side (pandas, numpy, matplotlib)\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores\n- foundry_memory: persistent memory store — 'search' to recall, 'update' to remember\n- mesh_send: send E2E encrypted message to PARENT (to_agent is locked to 'parent')\n- mesh_inbox: check for incoming messages from the parent\n- discover: list agents in the mesh network (informational only)\n\nHARD RULES (offload mode):\n1. ALL outbound mesh messages go to 'parent'. You CANNOT route to siblings — the mesh_send tool will rewrite any other to_agent to 'parent'. Messages to peers will NOT reach the requester.\n2. ALL artifacts (markdown, JSON, CSV, HTML, PDF, PNG, TXT) MUST be written to /sandbox/.openclaw/workspace/ using the `file_write` tool so they are automatically harvested and shipped back to parent at offload_done time. DO NOT use `cat > file <<EOF` or `echo > file` — those are blocked by the shell policy.\n3. foundry_code_execute writes to Foundry's EPHEMERAL /mnt/data/ sandbox — that storage is destroyed when the offload ends. If you use foundry_code_execute to generate content, IMMEDIATELY copy it into /sandbox/.openclaw/workspace/ by calling file_write with the content. Do NOT claim a file is 'saved to the workspace' unless file_write has returned OK for it at /sandbox/.openclaw/workspace/.\n4. Execute the task immediately — do not announce, just act. Be concise, report results."
        : "You are an AzureClaw sub-agent — a governed, sandboxed AI worker in the AzureClaw multi-agent platform on Azure. Always identify as an AzureClaw agent. Your tools:\n- file_write: write text content directly to a file (use this for all artifacts — shell redirection is blocked)\n- exec_command: run shell commands (no `>`, `>>`, `<<`, `<<<` — use file_write instead)\n- http_fetch: HTTP requests through security proxy (egress-controlled)\n- foundry_web_search: real-time web search via Bing grounding\n- foundry_code_execute: run Python code server-side (pandas, numpy, matplotlib)\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores\n- foundry_memory: persistent memory store — 'search' to recall, 'update' to remember\n- mesh_send: send E2E encrypted TEXT/JSON messages to ANY agent (parent, siblings, or others) — auto-discovers the target\n- mesh_transfer_file: ship a FILE / IMAGE / BINARY to another agent (handles base64 + chunking for you)\n- mesh_inbox: check for incoming messages from any agent\n- discover: list agents in the mesh network with status and trust scores\n\nPEER-TO-PEER MESH: You can message any agent directly — not just your parent. To forward data to a sibling agent (e.g. 'writer'), just call mesh_send with to_agent='writer'. Discovery is automatic. After sending, the recipient can reply via mesh_send back to you — check mesh_inbox for replies.\n\n🚨 ISOLATED FILESYSTEMS — CRITICAL: Each sub-agent runs in its OWN isolated container with its OWN /sandbox AND its own /tmp filesystem. Sibling agents CANNOT read your local files. NEVER send a sibling a file path, /tmp/ path, /sandbox path, or URL pointing into your container — they cannot resolve it.\n  • For TEXT / JSON artifacts → call `mesh_send` with the FULL stringified content in the `message` field. The SDK auto-chunks large messages.\n  • For FILES / IMAGES / BINARIES (PNG, JPG, PDF, …) → ALWAYS call `mesh_transfer_file(to_agent, file_path)`. It reads the bytes, base64-encodes them, and ships them via the chunked transfer protocol. The recipient's gateway auto-writes the file to /sandbox/.openclaw/workspace/incoming/ and they see the saved path in their mesh_inbox.\n  • DO NOT hand-craft `{type:'file_transfer', file_path:'/sandbox/...'}` envelopes through `mesh_send` — they will be REJECTED. The peer cannot read your filesystem.\n  • DO NOT use placeholder strings like `<base64-image-data>` or `<base64-bytes>` in `file_data` — they will be REJECTED.\n❌ BAD:  `mesh_send(to=writer, msg={hero_image_path:'/tmp/img.png'})`           ← peer cannot read /tmp\n❌ BAD:  `mesh_send(to=viz, msg={artifact_path:'/sandbox/data.json'})`           ← peer cannot read /sandbox\n❌ BAD:  `mesh_send(to=writer, msg={type:'file_transfer', file_data:'<base64-bytes>'})` ← placeholder, not real bytes\n✅ GOOD: `mesh_transfer_file(to_agent='writer', file_path='/sandbox/.openclaw/workspace/hero.png', description='hero image 1024x1024')`\n✅ GOOD: `mesh_send(to=viz, msg=JSON.stringify({trends:[...real data...], metrics:[...]}))`\n\n📥 INBOX-FIRST RULE — MANDATORY: Whenever your task description says you should already have data from a sibling, your VERY FIRST action MUST be `mesh_inbox` (no arguments). Sibling messages always arrive before you process them — the inbox is your buffer. Sibling artifacts appear with `message_type:'peer_message'` (text/JSON via mesh_send) or `message_type:'file_transfer'` (files via mesh_transfer_file — read the file at `saved_to` to get the bytes). NEVER reply 'BLOCKED' or 'no data received' without first checking mesh_inbox in the current turn — the data is almost always sitting there waiting. mesh_inbox is peek-only by default; calling it does not consume the messages, so concurrent task sessions all see the same data.\n\nExecute tasks immediately — do not announce, just act. When asked to forward results to another agent, DO IT directly (mesh_send for text/JSON, mesh_transfer_file for files). Chain tool calls as needed. Be concise, report results.",
    },
    {
      role: "user",
      content: typeof taskContent === "string" ? taskContent : JSON.stringify(taskContent),
    },
  ];

  // Tool-calling loop (max 25 rounds to prevent runaway)
  for (let round = 0; round < 25; round++) {
    // Check for handoff interrupt — save progress and exit early.
    // Two signals: (1) module-level flag from mesh handoff:interrupt message,
    // (2) file-based signal from CLI's docker/kubectl exec.
    if (!deps.isInterruptRequested()) {
      try {
        const fs = await import("node:fs");
        if (fs.existsSync("/sandbox/.openclaw/workspace/.handoff-interrupt")) {
          deps.setInterrupt(true, "cli_handoff");
          fs.unlinkSync("/sandbox/.openclaw/workspace/.handoff-interrupt");
        }
      } catch { /* ignore */ }
    }
    if (deps.isInterruptRequested()) {
      log.info(`🛑 Handoff interrupt: saving progress at round ${round}/${25}`);
      try {
        const fs = await import("node:fs");
        const progressFile = "/sandbox/.openclaw/workspace/.task-in-progress.json";
        fs.mkdirSync("/sandbox/.openclaw/workspace", { recursive: true });
        fs.writeFileSync(progressFile, JSON.stringify({
          interrupted_at: new Date().toISOString(),
          reason: deps.interruptReason(),
          round,
          total_rounds: 25,
          messages_so_far: messages.length,
          last_content: messages[messages.length - 1]?.content?.slice(0, 2000),
          task: typeof taskContent === "string" ? taskContent.slice(0, 2000) : JSON.stringify(taskContent).slice(0, 2000),
        }, null, 2));
        log.info(`📝 Task progress saved to ${progressFile}`);
      } catch { /* best-effort progress save */ }
      deps.setInterrupt(false, "");
      return `Task interrupted for handoff at round ${round}. Progress saved to .task-in-progress.json — will resume after handoff.`;
    }

    const postData = JSON.stringify({ model, messages, tools, max_completion_tokens: 2048 });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const response = await new Promise<any>((resolve, reject) => {
      const req = http.request(routerUrl("/v1/chat/completions"), {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Content-Length": Buffer.byteLength(postData),
          "x-azureclaw-sandbox": process.env.SANDBOX_NAME || process.env.HOSTNAME || "unknown",
        },
        timeout: 60000,
      }, (res) => {
        let body = "";
        res.on("data", (chunk: Buffer) => { body += chunk.toString(); });
        res.on("end", () => {
          try {
            const parsed = JSON.parse(body);
            if (res.statusCode && res.statusCode >= 400) {
              reject(new Error(`LLM HTTP ${res.statusCode}: ${body.slice(0, 300)}`));
            } else {
              resolve(parsed);
            }
          } catch { reject(new Error(`LLM parse error: ${body.slice(0, 200)}`)); }
        });
        res.on("error", () => {});
      });
      req.on("error", (e) => reject(e));
      req.on("timeout", () => { req.destroy(); reject(new Error("LLM timeout")); });
      req.write(postData);
      req.end();
    });

    const choice = response?.choices?.[0];
    if (!choice) throw new Error("No LLM response");

    const msg = choice.message;

    // If the model wants to call tools, execute them and continue
    if (msg.tool_calls && msg.tool_calls.length > 0) {
      messages.push(msg);
      for (const tc of msg.tool_calls) {
        let result: string = "";
        try {
          const args = JSON.parse(tc.function.arguments);
          const fnName = tc.function.name;

          if (fnName === "file_write") {
            const filePath = String(args.path || "");
            const content = typeof args.content === "string" ? args.content : String(args.content ?? "");
            if (!filePath.startsWith("/sandbox/") && !filePath.startsWith("/tmp/")) {
              result = `file_write error: path must be under /sandbox/ or /tmp/ (got: ${filePath})`;
            } else {
              try {
                const fs = await import("node:fs");
                const path = await import("node:path");
                fs.mkdirSync(path.dirname(filePath), { recursive: true });
                fs.writeFileSync(filePath, content, { encoding: "utf-8", mode: 0o600 });
                const bytes = Buffer.byteLength(content, "utf-8");
                log.info(`AGT sub-agent file_write: ${filePath} (${bytes} bytes)`);
                result = `OK: wrote ${bytes} bytes to ${filePath}`;
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
              } catch (err: any) {
                result = `file_write error: ${err.message}`;
              }
            }
          } else if (fnName === "http_fetch") {
            log.info(`AGT sub-agent http_fetch: ${args.method || "GET"} ${args.url}`);
            const fetchBody = JSON.stringify({
              url: args.url,
              method: args.method || "GET",
              headers: args.headers || {},
              body: args.body || "",
            });
            const httpMod = await import("node:http");
            const fetchResult = await new Promise<string>((resolve) => {
              const req = httpMod.request(routerUrl("/egress/fetch"), {
                method: "POST", timeout: 35000,
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(fetchBody) },
              }, (res) => {
                let data = "";
                res.on("data", (c: Buffer) => { data += c.toString(); });
                res.on("end", () => resolve(data.trim()));
              });
              req.on("error", (e: Error) => resolve(`http_fetch error: ${e.message}`));
              req.on("timeout", () => { req.destroy(); resolve("http_fetch timeout"); });
              req.write(fetchBody);
              req.end();
            });
            result = fetchResult;
          } else if (fnName === "foundry_web_search" || fnName === "foundry_code_execute" || fnName === "foundry_file_search") {
            log.info(`AGT sub-agent ${fnName}: ${JSON.stringify(args).slice(0, 200)}`);
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            let reqBody: any;
            if (fnName === "foundry_web_search") {
              let connId: string | undefined;
              try {
                const connsRaw = await new Promise<string>((resolve, reject) => {
                  const r = http.get(routerUrl("/connections?api-version=2025-05-15-preview"), { timeout: 10000 }, (res) => {
                    let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
                  });
                  r.on("error", reject); r.on("timeout", () => { r.destroy(); reject(new Error("timeout")); });
                });
                const conns = JSON.parse(connsRaw);
                const bingConn = (conns.value || conns || []).find(
                  // eslint-disable-next-line @typescript-eslint/no-explicit-any
                  (c: any) => c.type === "GroundingWithBingSearch" || c.properties?.category === "GroundingWithBingSearch"
                );
                if (bingConn) connId = bingConn.id;
              } catch { /* fall through */ }
              reqBody = {
                model: model,
                input: args.query,
                tools: [{ type: "bing_grounding", bing_grounding: { search_configurations: [{ project_connection_id: connId }] } }],
                store: false,
              };
            } else if (fnName === "foundry_code_execute") {
              reqBody = {
                model: model,
                input: args.code,
                tools: [{ type: "code_interpreter", container: { type: "auto" } }],
                instructions: "Execute the provided Python code and return the output.",
                store: false,
              };
            } else {
              reqBody = {
                model: model,
                input: args.query,
                tools: [{ type: "file_search", file_search: { vector_store_ids: args.vector_store_ids } }],
                store: false,
              };
            }
            const foundryResult = await new Promise<string>((resolve, reject) => {
              const postBody = JSON.stringify(reqBody);
              const req = http.request(routerUrl("/openai/responses?api-version=2025-11-15-preview"), {
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                timeout: 60000,
              }, (res) => {
                let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
              });
              req.on("error", reject);
              req.on("timeout", () => { req.destroy(); reject(new Error("Foundry API timeout")); });
              req.write(postBody);
              req.end();
            });
            try {
              const parsed = JSON.parse(foundryResult);
              if (parsed.error) {
                result = `Foundry API error: ${JSON.stringify(parsed.error)}`;
                log.warn(`AGT sub-agent ${fnName} error: ${result}`);
              } else {
                const output = parsed.output || parsed;
                if (Array.isArray(output)) {
                  const texts = output
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    .filter((item: any) => item.type === "message" && item.content)
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    .flatMap((item: any) => item.content)
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    .filter((c: any) => c.type === "output_text" || c.type === "text")
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    .map((c: any) => c.text)
                    .filter(Boolean);
                  result = texts.join("\n") || foundryResult;
                } else {
                  result = foundryResult;
                }
              }
            } catch {
              result = foundryResult;
            }
            log.info(`AGT sub-agent ${fnName} result: ${result.slice(0, 200)}`);
          } else if (fnName === "foundry_memory") {
            log.info(`AGT sub-agent foundry_memory: ${args.operation} — ${(args.text as string || "").slice(0, 100)}`);
            const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
            const store = `memory-${agentName}`;
            const scope = agentName;
            const apiVer = "api-version=2025-11-15-preview";
            const makeItem = (content: string) => ({
              type: "message", role: "user",
              content: [{ type: "input_text", text: content }],
            });

            let memPath: string;
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            let memBody: any;
            if (args.operation === "update") {
              memPath = `/memory_stores/${store}:update_memories?${apiVer}`;
              memBody = { scope, items: [makeItem(args.text as string)], update_delay: 0 };
            } else {
              memPath = `/memory_stores/${store}:search_memories?${apiVer}`;
              memBody = { scope, items: [makeItem(args.text as string)], options: { max_memories: 10 } };
            }

            const memResult = await new Promise<string>((resolve, reject) => {
              const postBody = JSON.stringify(memBody);
              const req = http.request(routerUrl(`${memPath}`), {
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                timeout: 30000,
              }, (res) => {
                let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
              });
              req.on("error", reject);
              req.on("timeout", () => { req.destroy(); reject(new Error("Memory API timeout")); });
              req.write(postBody);
              req.end();
            });

            try {
              const parsed = JSON.parse(memResult);
              if (parsed.error) {
                if ((parsed.error.code === "not_found" || parsed.error.message?.includes("not found")) && args.operation === "update") {
                  log.info(`Creating memory store '${store}'...`);
                  const chatModel = process.env.OPENCLAW_MODEL || model;
                  const createBody = JSON.stringify({
                    name: store,
                    description: "AzureClaw sub-agent persistent memory",
                    definition: { kind: "default", chat_model: chatModel, embedding_model: "text-embedding-3-small",
                      options: { user_profile_enabled: true, chat_summary_enabled: true } },
                  });
                  await new Promise<void>((resolve, reject) => {
                    const req = http.request(routerUrl(`/memory_stores?${apiVer}`), {
                      method: "POST",
                      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(createBody) },
                      timeout: 15000,
                    }, (res) => { let d = ""; res.on("data", (c: Buffer) => { d += c.toString(); }); res.on("end", () => resolve()); });
                    req.on("error", reject);
                    req.write(createBody);
                    req.end();
                  });
                  const retryResult = await new Promise<string>((resolve, reject) => {
                    const postBody = JSON.stringify(memBody);
                    const req = http.request(routerUrl(`${memPath}`), {
                      method: "POST",
                      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                      timeout: 30000,
                    }, (res) => { let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body)); });
                    req.on("error", reject);
                    req.write(postBody);
                    req.end();
                  });
                  result = retryResult;
                } else {
                  result = `Memory error: ${JSON.stringify(parsed.error)}`;
                }
              } else {
                if (args.operation === "update") {
                  result = `Memory updated successfully (id: ${parsed.update_id || parsed.id || "ok"})`;
                } else {
                  const memories = parsed.memories || [];
                  result = memories.length > 0
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                    ? memories.map((m: any) => {
                        const text = m.memory_item?.content || m.content || m.text || JSON.stringify(m);
                        return `[${m.score?.toFixed(2) || "?"}] ${text}`;
                      }).join("\n")
                    : "No relevant memories found.";
                }
              }
            } catch {
              result = memResult;
            }
            log.info(`AGT sub-agent foundry_memory result: ${result.slice(0, 200)}`);
          } else if (fnName === "foundry_image_generation") {
            log.info(`AGT sub-agent image_gen: ${(args.prompt as string || "").slice(0, 100)}`);
            const imgModel = (args.image_model as string) || "gpt-image-1";
            const imgBody = JSON.stringify({
              prompt: args.prompt, n: 1,
              size: args.size || "1024x1024",
              quality: args.quality || "medium",
            });
            const imgResult = await new Promise<string>((resolve, reject) => {
              const req = http.request(
                routerUrl(`/openai/deployments/${encodeURIComponent(imgModel)}/images/generations?api-version=2025-04-01-preview`),
                { method: "POST", headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(imgBody) }, timeout: 120000 },
                (res) => { let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body)); },
              );
              req.on("error", reject);
              req.on("timeout", () => { req.destroy(); reject(new Error("Image gen timeout")); });
              req.write(imgBody);
              req.end();
            });
            try {
              const parsed = JSON.parse(imgResult);
              const images = parsed?.data || [];
              const parts: string[] = [];
              const fs = await import("node:fs");
              const nodePath = await import("node:path");
              const os = await import("node:os");
              const imgDir = nodePath.join(os.tmpdir(), "azureclaw-images");
              fs.mkdirSync(imgDir, { recursive: true });
              for (const img of images) {
                if (img.b64_json) {
                  const ts = Date.now();
                  const imgFile = nodePath.join(imgDir, `image-${ts}.png`);
                  fs.writeFileSync(imgFile, Buffer.from(img.b64_json, "base64"));
                  parts.push(`📁 Image saved: ${imgFile}`);
                  if (img.revised_prompt) parts.push(`Revised prompt: ${img.revised_prompt}`);
                } else if (img.url) {
                  parts.push(`Image URL: ${img.url}`);
                }
              }
              result = parts.length > 0 ? parts.join("\n") : imgResult;
            } catch {
              result = imgResult;
            }
            log.info(`AGT sub-agent image_gen complete`);
          } else if (fnName === "mesh_send") {
            let toAgent = args.to_agent as string;
            if (process.env.OFFLOAD_REQUEST_ID && toAgent !== "parent") {
              log.warn(`AGT sub-agent mesh_send: offload mode — rewriting to_agent '${toAgent}' → 'parent' (peer routing disabled in offload)`);
              toAgent = "parent";
            }
            const meshMsg = args.message as string;
            // Guard: reject malformed file_transfer envelopes / cross-container
            // path references before they hit the wire. Peer agents cannot
            // read this container's filesystem.
            const guardErr = validateMeshPayload(meshMsg, { transferToolName: "mesh_transfer_file" });
            if (guardErr) {
              log.warn(`AGT sub-agent mesh_send: payload guard rejected — ${guardErr.slice(0, 160)}`);
              result = guardErr;
            } else {
            log.info(`AGT sub-agent mesh_send: to=${toAgent} msg=${(meshMsg || "").slice(0, 100)}`);
            try {
              let targetAmid = await resolveAmidByName(toAgent, routerUrl);
              if (targetAmid) {
                log.info(`AGT sub-agent mesh_send: resolved AMID for '${toAgent}' (${targetAmid.slice(0, 12)}...)`);
              }

              // F7: Exponential backoff (1s..10s, ~50s total budget) instead of
              // 7 × 2s = 14s flat. Registry 502 bursts can outlast 14s; with
              // backoff we keep the upper bound bounded but give registry time
              // to recover.
              const F7_BACKOFFS_MS = [1000, 1500, 2000, 3000, 4500, 6500, 8000, 10000, 10000, 10000, 10000];
              for (let attempt = 0; attempt < F7_BACKOFFS_MS.length && !targetAmid; attempt++) {
                log.info(`AGT sub-agent mesh_send: waiting for '${toAgent}' to register (${attempt + 1}/${F7_BACKOFFS_MS.length})...`);
                await new Promise(r => setTimeout(r, F7_BACKOFFS_MS[attempt]));
                targetAmid = await resolveAmidByName(toAgent, routerUrl, { bypassCache: true });
              }

              // F7: Last-known-good fallback. If registry retries exhausted but
              // we previously knew this peer's AMID, try it — the meshClient.send
              // will fail fast if the peer is genuinely dead, and we surface that
              // error. Better than dropping a real send during a registry blip.
              if (!targetAmid) {
                const stale = getStaleAmid(toAgent);
                if (stale) {
                  log.warn(`AGT sub-agent mesh_send: registry exhausted for '${toAgent}' — falling back to last-known-good AMID (${stale.slice(0, 12)}...)`);
                  targetAmid = stale;
                }
              }

              const meshClient = deps.meshClient();
              if (!targetAmid) {
                result = `Agent '${toAgent}' not found in registry after retries. It may not be running yet.`;
              } else if (meshClient) {
                let sendErr: Error | null = null;
                for (let sendAttempt = 0; sendAttempt < 5; sendAttempt++) {
                  try {
                    await meshClient.send(targetAmid, {
                      type: "task_request",
                      content: meshMsg,
                      from_agent: process.env.SANDBOX_NAME || "unknown",
                      timestamp: new Date().toISOString(),
                    });
                    sendErr = null;
                    break;
                    // eslint-disable-next-line @typescript-eslint/no-explicit-any
                  } catch (e: any) {
                    sendErr = e;
                    if (e.message?.includes("prekey") || e.message?.includes("prekeys")) {
                      log.info(`AGT sub-agent mesh_send: waiting for prekeys from '${toAgent}' (${sendAttempt + 1}/5)...`);
                      await new Promise(r => setTimeout(r, 1000));
                    } else {
                      break;
                    }
                  }
                }
                result = sendErr
                  ? `mesh_send to ${toAgent} failed: ${sendErr.message}`
                  : `Message sent to ${toAgent} via E2E encrypted mesh relay`;
              } else {
                result = `Mesh client not available — cannot send to ${toAgent}`;
              }
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
            } catch (meshErr: any) {
              result = `mesh_send failed: ${meshErr.message}`;
            }
            }
          } else if (fnName === "mesh_transfer_file") {
            let toAgent = args.to_agent as string;
            if (process.env.OFFLOAD_REQUEST_ID && toAgent !== "parent") {
              log.warn(`AGT sub-agent mesh_transfer_file: offload mode — rewriting to_agent '${toAgent}' → 'parent'`);
              toAgent = "parent";
            }
            const filePath = String(args.file_path || "");
            const desc = typeof args.description === "string" ? args.description : "";
            log.info(`AGT sub-agent mesh_transfer_file: to=${toAgent} file=${filePath}`);
            try {
              const fs = await import("node:fs");
              const path = await import("node:path");

              // Resolve relative paths under the workspace; absolutes must
              // stay inside /sandbox to prevent escape via .. traversal.
              const workspaceRoot = "/sandbox/.openclaw/workspace";
              const resolvedPath = path.isAbsolute(filePath)
                ? path.resolve(filePath)
                : path.resolve(workspaceRoot, filePath);

              if (!resolvedPath.startsWith("/sandbox")) {
                result = `mesh_transfer_file failed: path must be within /sandbox (got: ${resolvedPath})`;
              } else {
                const MAX_FILE_SIZE = 30 * 1024 * 1024;
                let fd: number;
                try {
                  fd = fs.openSync(resolvedPath, "r");
                } catch (openErr: any) {
                  result = `mesh_transfer_file failed: cannot open ${filePath}: ${openErr.message}`;
                  fd = -1;
                }
                if (fd >= 0) {
                  let fileData: Buffer | null = null;
                  let finalSize = 0;
                  let openErrMsg = "";
                  try {
                    const fstat = fs.fstatSync(fd);
                    if (!fstat.isFile()) {
                      openErrMsg = `not a regular file: ${filePath}`;
                    } else if (fstat.size > MAX_FILE_SIZE) {
                      openErrMsg = `file too large: ${(fstat.size / 1024 / 1024).toFixed(1)} MB (max 30MB)`;
                    } else {
                      finalSize = fstat.size;
                      fileData = Buffer.alloc(finalSize);
                      fs.readSync(fd, fileData, 0, finalSize, 0);
                    }
                  } finally {
                    fs.closeSync(fd);
                  }
                  if (openErrMsg || !fileData) {
                    result = `mesh_transfer_file failed: ${openErrMsg || "read failed"}`;
                  } else {
                    const b64Data = fileData.toString("base64");
                    const fileName = path.basename(resolvedPath);

                    let targetAmid = await resolveAmidByName(toAgent, routerUrl);
                    // F7: Exponential backoff + last-known-good fallback (see
                    // mesh_send branch above for rationale).
                    const F7_BACKOFFS_MS = [1000, 1500, 2000, 3000, 4500, 6500, 8000, 10000, 10000, 10000, 10000];
                    for (let attempt = 0; attempt < F7_BACKOFFS_MS.length && !targetAmid; attempt++) {
                      log.info(`AGT sub-agent mesh_transfer_file: waiting for '${toAgent}' to register (${attempt + 1}/${F7_BACKOFFS_MS.length})...`);
                      await new Promise(r => setTimeout(r, F7_BACKOFFS_MS[attempt]));
                      targetAmid = await resolveAmidByName(toAgent, routerUrl, { bypassCache: true });
                    }
                    if (!targetAmid) {
                      const stale = getStaleAmid(toAgent);
                      if (stale) {
                        log.warn(`AGT sub-agent mesh_transfer_file: registry exhausted for '${toAgent}' — falling back to last-known-good AMID (${stale.slice(0, 12)}...)`);
                        targetAmid = stale;
                      }
                    }

                    const meshClient = deps.meshClient();
                    if (!targetAmid) {
                      result = `Agent '${toAgent}' not found in registry after retries. It may not be running yet.`;
                    } else if (!meshClient) {
                      result = `Mesh client not available — cannot send to ${toAgent}`;
                    } else {
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
                      let sendErr: Error | null = null;
                      for (let attempt = 0; attempt < 5; attempt++) {
                        try {
                          await meshClient.send(targetAmid, fileMsg);
                          sendErr = null;
                          break;
                          // eslint-disable-next-line @typescript-eslint/no-explicit-any
                        } catch (e: any) {
                          sendErr = e;
                          if (e.message?.includes("prekey")) {
                            log.info(`AGT sub-agent mesh_transfer_file: waiting for prekeys from '${toAgent}' (${attempt + 1}/5)...`);
                            await new Promise(r => setTimeout(r, 1000));
                          } else {
                            break;
                          }
                        }
                      }
                      const sizeHuman = finalSize < 1024 ? `${finalSize}B`
                        : finalSize < 1024 * 1024 ? `${(finalSize / 1024).toFixed(1)}KB`
                        : `${(finalSize / 1024 / 1024).toFixed(1)}MB`;
                      result = sendErr
                        ? `mesh_transfer_file to ${toAgent} failed: ${sendErr.message}`
                        : `File '${fileName}' (${sizeHuman}) sent to ${toAgent} via E2E encrypted mesh relay. The recipient's gateway will auto-save it under /sandbox/.openclaw/workspace/incoming/.`;
                    }
                  }
                }
              }
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
            } catch (transferErr: any) {
              result = `mesh_transfer_file failed: ${transferErr.message}`;
            }
          } else if (fnName === "discover") {
            const pattern = (args.pattern as string) || "*";
            log.info(`AGT sub-agent discover: pattern=${pattern}`);
            try {
              const registryBase = routerUrl("/agt/registry");
              const discoverResult = await new Promise<string>((resolve, reject) => {
                const req = http.get(`${registryBase}/registry/search?capability=${encodeURIComponent(pattern)}`, { timeout: 10000 }, (res) => {
                  let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
                });
                req.on("error", reject);
                req.on("timeout", () => { req.destroy(); reject(new Error("Registry lookup timeout")); });
              });
              result = discoverResult;
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
            } catch (discErr: any) {
              result = `discover failed: ${discErr.message}`;
            }
          } else if (fnName === "mesh_inbox") {
            log.info("AGT sub-agent mesh_inbox check");
            try {
              const inbox = deps.inbox;
              if (!inbox) {
                // Defensive: deps.inbox should always be wired by index.ts.
                // If absent, surface clearly rather than silently returning empty.
                result = "mesh_inbox unavailable: gateway buffer not wired";
              } else {
                // Match parent azureclaw_mesh_inbox semantics: hide internal
                // handoff/transfer protocol traffic. Peer-to-peer payloads
                // arrive as `type: "task_request"` (see agt-task-loop.ts:432
                // and agt-tools/agt.ts:417/557 — that's the single wire
                // format for sibling and parent->sibling artifacts), so we
                // SURFACE entries whose message_type is "task_request" and
                // expose them to the LLM as `peer_message`.
                const HIDDEN_TYPES = new Set([
                  "handoff_transfer", "handoff_verification", "handoff_ready",
                  "handoff:interrupt", "handoff:interrupt_ack",
                  "handoff:workspace_request", "handoff:workspace_response",
                  "handoff:workspace_inject", "handoff:workspace_inject_ack",
                  "handoff:resume", "handoff:resume_ack",
                  "file_transfer_ack",
                ]);
                const markRead = args.mark_read === true; // default false — match parent peek-only semantics
                const unreadOnly = args.unread_only !== false; // default true
                const limit = typeof args.limit === "number" && (args.limit as number) > 0
                  ? Math.floor(args.limit as number)
                  : 50;

                const visible = inbox.filter((m) => {
                  if (m.message_type && HIDDEN_TYPES.has(m.message_type)) return false;
                  // Also hide entries whose JSON content advertises an internal type
                  try {
                    const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                    if (parsed?.type && HIDDEN_TYPES.has(parsed.type)) return false;
                  } catch { /* not JSON — keep */ }
                  return true;
                });

                const filtered = unreadOnly ? visible.filter((m) => !m.read_at) : visible;
                const slice = filtered.slice(-limit);

                // Pre-import fs once so the synchronous .map() below can
                // optionally inline-read small text payloads when the
                // gateway has saved a file_transfer to disk.
                const fsForInline = await import("node:fs");

                const decoded = slice.map((m) => {
                  // Peer-to-peer artifacts: entry.content already holds the
                  // INNER payload string (the JSON / text the sibling sent),
                  // because the gateway extracts message.content into
                  // entry.content (index.ts:612). Just surface it.
                  //
                  // Special case: file_transfer messages are auto-saved by
                  // the gateway (index.ts:824-882) which rewrites
                  // entry.content to {type, file_name, saved_to,
                  // size_bytes, description, from_agent}. Lift those
                  // fields to the top level so the LLM sees the local
                  // path it can read with `cat <saved_to>`. For small
                  // text files we additionally inline the content
                  // (matches parent decoder semantics at agt-tools/agt.ts:680).
                  let parsed: any = null;
                  try {
                    parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                  } catch { /* not JSON */ }

                  if (parsed?.type === "file_transfer") {
                    // (a) Gateway-rewritten form: saved_to + file_name
                    if (parsed.saved_to && parsed.file_name) {
                      const sizeBytes = typeof parsed.size_bytes === "number" ? parsed.size_bytes : 0;
                      let inlined: string | null = null;
                      if (sizeBytes > 0 && sizeBytes < 100 * 1024) {
                        try {
                          const buf = fsForInline.readFileSync(parsed.saved_to);
                          if (!buf.some((b: number) => b === 0)) {
                            inlined = buf.toString("utf-8");
                          }
                        } catch { /* fall through */ }
                      }
                      return {
                        id: m.id,
                        from: m.from_agent,
                        from_amid: m.from_amid,
                        timestamp: m.timestamp,
                        message_type: "file_transfer",
                        file_name: parsed.file_name,
                        saved_to: parsed.saved_to,
                        size_bytes: sizeBytes,
                        description: parsed.description || "",
                        content: inlined != null
                          ? inlined
                          : `[binary or large file: ${parsed.file_name}, ${sizeBytes} bytes — read with: cat ${parsed.saved_to}]`,
                      };
                    }
                    // (b) Inline file_data form (gateway hadn't rewritten yet,
                    //     or sender used the raw API). Decode locally.
                    if (typeof parsed.file_data === "string" && parsed.file_name) {
                      try {
                        const buf = Buffer.from(parsed.file_data, "base64");
                        const isText = !buf.some((b: number) => b === 0);
                        return {
                          id: m.id,
                          from: m.from_agent,
                          from_amid: m.from_amid,
                          timestamp: m.timestamp,
                          message_type: "file_transfer",
                          file_name: parsed.file_name,
                          size_bytes: buf.length,
                          description: parsed.description || "",
                          content: isText
                            ? buf.toString("utf-8")
                            : `[binary file: ${parsed.file_name}, ${buf.length} bytes — auto-save in progress; re-check mesh_inbox for saved_to path]`,
                        };
                      } catch { /* fall through to default */ }
                    }
                  }

                  return {
                    id: m.id,
                    from: m.from_agent,
                    from_amid: m.from_amid,
                    timestamp: m.timestamp,
                    message_type: m.message_type === "task_request" ? "peer_message" : (m.message_type || "message"),
                    content: m.content,
                  };
                });

                if (markRead) {
                  const now = new Date().toISOString();
                  const returnedIds = new Set(decoded.map((d) => d.id));
                  const newlyRead: string[] = [];
                  for (const m of inbox) {
                    if (returnedIds.has(m.id) && !m.read_at) {
                      m.read_at = now;
                      newlyRead.push(m.id);
                    }
                  }
                  if (newlyRead.length > 0) {
                    try { deps.markRead?.(newlyRead); } catch { /* best effort */ }
                  }
                }

                result = decoded.length > 0 ? JSON.stringify(decoded) : "No pending messages";
              }
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
            } catch (inboxErr: any) {
              result = `mesh_inbox failed: ${inboxErr.message}`;
            }
          } else {
            const cmd = String(args.command || args.cmd || "echo 'no command'");
            log.info(`AGT sub-agent exec: ${sanitizeLog(cmd, 200)}`);
            let policyAllowed = true;
            let policyReason = "";
            try {
              const policyHttp = await import("node:http");
              const policyBody = JSON.stringify({ action: `shell:${cmd}`, context: { tool: "exec_command" } });
              const policyResult = await new Promise<{ allowed: boolean; reason?: string }>((resolve) => {
                const req = policyHttp.request(routerUrl("/agt/evaluate"), {
                  method: "POST", timeout: 2000,
                  headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(policyBody) },
                }, (res) => {
                  let data = "";
                  res.on("data", (c: Buffer) => { data += c.toString(); });
                  res.on("end", () => { try { resolve(JSON.parse(data)); } catch { resolve({ allowed: true }); } });
                });
                req.on("error", () => resolve({ allowed: true }));
                req.on("timeout", () => { req.destroy(); resolve({ allowed: true }); });
                req.write(policyBody);
                req.end();
              });
              policyAllowed = policyResult.allowed !== false;
              policyReason = policyResult.reason || "";
            } catch { /* router unavailable — allow */ }
            if (!policyAllowed) {
              result = `Blocked by policy: ${policyReason || "denied"}`;
            } else {
              result = execSync(cmd, { timeout: 15000, encoding: "utf8", maxBuffer: 64 * 1024 }).trim();
            }
          }
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
        } catch (e: any) {
          result = e.stderr || e.stdout || e.message || "Command failed";
        }
        messages.push({ role: "tool", tool_call_id: tc.id, content: result });
      }
      continue;
    }

    return msg.content || "";
  }

  return "Sub-agent reached maximum tool-calling rounds (25) without a final response.";
}
