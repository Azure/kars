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

import { TASK_TOOLS } from "./agt-task-tools.js";
import { resolveAmidByName } from "./amid-cache.js";
import { sanitizeLog } from "./log-redact.js";
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
        : "You are an AzureClaw sub-agent — a governed, sandboxed AI worker in the AzureClaw multi-agent platform on Azure. Always identify as an AzureClaw agent. Your tools:\n- file_write: write text content directly to a file (use this for all artifacts — shell redirection is blocked)\n- exec_command: run shell commands (no `>`, `>>`, `<<`, `<<<` — use file_write instead)\n- http_fetch: HTTP requests through security proxy (egress-controlled)\n- foundry_web_search: real-time web search via Bing grounding\n- foundry_code_execute: run Python code server-side (pandas, numpy, matplotlib)\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores\n- foundry_memory: persistent memory store — 'search' to recall, 'update' to remember\n- mesh_send: send E2E encrypted messages to ANY agent (parent, siblings, or others) — auto-discovers the target\n- mesh_inbox: check for incoming messages from any agent\n- discover: list agents in the mesh network with status and trust scores\n\nPEER-TO-PEER MESH: You can message any agent directly — not just your parent. To forward data to a sibling agent (e.g. 'writer'), just call mesh_send with to_agent='writer'. Discovery is automatic. After sending, the recipient can reply via mesh_send back to you — check mesh_inbox for replies.\n\nExecute tasks immediately — do not announce, just act. When asked to forward results to another agent, DO IT directly with mesh_send. Chain tool calls as needed. Be concise, report results.",
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
        let result: string;
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
            log.info(`AGT sub-agent mesh_send: to=${toAgent} msg=${(meshMsg || "").slice(0, 100)}`);
            try {
              let targetAmid = await resolveAmidByName(toAgent, routerUrl);
              if (targetAmid) {
                log.info(`AGT sub-agent mesh_send: resolved AMID for '${toAgent}' (${targetAmid.slice(0, 12)}...)`);
              }

              for (let attempt = 1; attempt < 8 && !targetAmid; attempt++) {
                log.info(`AGT sub-agent mesh_send: waiting for '${toAgent}' to register (${attempt}/7)...`);
                await new Promise(r => setTimeout(r, 2000));
                targetAmid = await resolveAmidByName(toAgent, routerUrl, { bypassCache: true });
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
              const meshClient = deps.meshClient();
              if (meshClient && typeof meshClient.drain === "function") {
                const messages = await meshClient.drain();
                result = messages.length > 0
                  // eslint-disable-next-line @typescript-eslint/no-explicit-any
                  ? JSON.stringify(messages.map((m: any) => ({ from: m.from_agent || m.sender, content: m.content || m.text, timestamp: m.timestamp })))
                  : "No pending messages";
              } else {
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
                const agtInbox = (globalThis as any).__agtInbox || [];
                result = agtInbox.length > 0
                  ? JSON.stringify(agtInbox)
                  : "No pending messages";
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
