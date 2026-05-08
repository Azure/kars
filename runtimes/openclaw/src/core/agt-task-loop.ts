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
import { getTaskTools } from "./agt-task-tools.js";
import { resolveAmidByName, getStaleAmid, amidToName, parentTrustedNames } from "./amid-cache.js";
import { sanitizeLog } from "./log-redact.js";
import { meshSendWithIdentity, type MeshIdentity } from "./mesh-transport.js";
import { validateMeshPayload } from "./mesh-payload-guard.js";
import { routerUrl } from "./router-client.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyMeshClient = any;
type Logger = { info: (m: string) => void; warn: (m: string) => void };

export interface TaskLoopDeps {
  /** Returns the current AGT mesh client, or null if not connected. */
  meshClient: () => AnyMeshClient | null;
  /** Returns the current AGT identity for per-message Ed25519 signing in chunked transfers. May be null if not yet loaded. */
  meshIdentity?: () => MeshIdentity | null;
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
  /**
   * Server-side blocking inbox wait. Wired by index.ts to the same
   * `waitForInbox()` helper used by `azureclaw_mesh_inbox`. Used by the
   * sub-agent task-loop's `mesh_inbox` (block_until_message) and
   * `mesh_await` to obviate LLM poll-and-yield. Returns true on wake,
   * false on timeout. Always resolves; never rejects. Optional — when
   * absent, blocking tools fall back to a single immediate read.
   */
  waitForInbox?: (timeoutMs: number) => Promise<boolean>;
}

export async function processTaskWithTools(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  taskContent: any,
  deps: TaskLoopDeps,
  log: Logger,
): Promise<string> {
  const http = await import("node:http");
  const { execSync } = await import("node:child_process");
  const model = process.env.OPENCLAW_MODEL || process.env.MODEL || "gpt-4.1";

  const tools = getTaskTools();
  const slim = process.env.AZURECLAW_PROVIDER === "github-models";

  const offloadToolBlock = slim
    ? "- file_write: write text content to a file (preferred over shell redirection, which is blocked)\n- exec_command: run shell commands (read-only ops; avoid `>`, `>>`, `<<`, `<<<` — use file_write instead). Run Python with `python3 -c '...'` or by writing a script via file_write then executing it.\n- http_fetch: HTTP requests through the egress-controlled security proxy\n- web_search: real-time web search via DuckDuckGo (egress-proxied; returns title/url/snippet list)\n- memory: local persistent memory (operations: 'update' to store a fact, 'search' to query, 'list' to dump). Backed by /sandbox/.openclaw/memory.json — per-container, no peer sync.\n- mesh_send: send an E2E encrypted message to the parent (to_agent is locked to 'parent' in offload mode)\n- mesh_inbox: check for incoming messages from the parent (pass `block_until_message=true` to wait server-side)\n- discover: list agents in the mesh (informational)"
    : "- file_write: write text content to a file (preferred over shell redirection, which is blocked)\n- exec_command: run shell commands (read-only ops; avoid `>`, `>>`, `<<`, `<<<` — use file_write instead)\n- http_fetch: HTTP requests through the egress-controlled security proxy\n- foundry_web_search: real-time web search via Bing grounding\n- foundry_code_execute: run Python server-side (pandas, numpy, matplotlib). Save artifacts to /mnt/data/<name>; the wrapper auto-downloads them to /sandbox/.openclaw/workspace/ and lists local paths in a `<downloaded_files>` JSON block. Avoid copying files inside Python — the local /sandbox path does not exist in Foundry's container.\n- foundry_download_file: fetch a single Foundry container file by file_id+container_id when auto-download missed it. Always pass the literal `cntr_…` and `cfile_…` identifiers from the most recent foundry_code_execute response — never substitute filenames, friendly names, or the string 'default'.\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores\n- foundry_memory: persistent memory store — 'search' to recall, 'update' to remember\n- mesh_send: send an E2E encrypted message to the parent (to_agent is locked to 'parent' in offload mode)\n- mesh_inbox: check for incoming messages from the parent (pass `block_until_message=true` to wait server-side)\n- discover: list agents in the mesh (informational)";

  const subAgentToolBlock = slim
    ? "- file_write: write text to a file (use this for artifacts; shell redirection is blocked)\n- exec_command: run shell commands (avoid `>`, `>>`, `<<`, `<<<` — use file_write instead). Run Python with `python3 -c '...'` or by writing a script via file_write then executing it.\n- http_fetch: HTTP requests through the egress-controlled security proxy\n- web_search: real-time web search via DuckDuckGo (egress-proxied; returns title/url/snippet list). Use for current events, news, or anything needing live information.\n- memory: local persistent memory (operations: 'update' to store, 'search' to query, 'list' to dump). Backed by /sandbox/.openclaw/memory.json — per-container, no peer sync.\n- mesh_send: send an E2E encrypted text/JSON message to any agent — auto-discovers the target\n- mesh_transfer_file: ship a file/image/binary to another agent (handles base64 + chunking)\n- mesh_inbox: check for incoming messages (pass `block_until_message=true` to wait server-side)\n- mesh_await: block until all named senders have delivered at least one message, e.g. `mesh_await(senders=['analyst','viz'], timeout_seconds=300)`\n- discover: list agents in the mesh with status and trust scores"
    : "- file_write: write text to a file (use this for artifacts; shell redirection is blocked)\n- exec_command: run shell commands (avoid `>`, `>>`, `<<`, `<<<` — use file_write instead)\n- http_fetch: HTTP requests through the egress-controlled security proxy\n- foundry_web_search: real-time web search via Bing grounding\n- foundry_code_execute: run Python server-side (pandas, numpy, matplotlib). Save artifacts to /mnt/data/<name>; the wrapper auto-downloads them to /sandbox/.openclaw/workspace/ and lists local paths in a `<downloaded_files>` JSON block. Avoid copying files inside Python — the local /sandbox path does not exist in Foundry's container.\n- foundry_download_file: fetch a single Foundry container file by file_id+container_id when auto-download missed it. Always pass the literal `cntr_…` and `cfile_…` identifiers from the most recent foundry_code_execute response — never substitute filenames, friendly names, or the string 'default'.\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores\n- foundry_memory: persistent memory store — 'search' to recall, 'update' to remember\n- mesh_send: send an E2E encrypted text/JSON message to any agent — auto-discovers the target\n- mesh_transfer_file: ship a file/image/binary to another agent (handles base64 + chunking)\n- mesh_inbox: check for incoming messages (pass `block_until_message=true` to wait server-side)\n- mesh_await: block until all named senders have delivered at least one message, e.g. `mesh_await(senders=['analyst','viz'], timeout_seconds=300)`\n- discover: list agents in the mesh with status and trust scores";

  const offloadConventions = slim
    ? "Offload-mode conventions:\n1. Outbound mesh messages always go to 'parent'. The mesh_send tool rewrites any other to_agent to 'parent'.\n2. Place every output artifact (markdown, JSON, CSV, HTML, PDF, PNG, TXT) in /sandbox/.openclaw/workspace/ via the file_write tool. Files there are harvested and shipped back at offload_done.\n3. For Python data work, use exec_command with `python3 -c` or file_write a script + exec_command. Pandas/numpy/matplotlib are pre-installed.\n4. Execute the task immediately — no preamble, just act. Be concise."
    : "Offload-mode conventions:\n1. Outbound mesh messages always go to 'parent'. The mesh_send tool rewrites any other to_agent to 'parent'.\n2. Place every output artifact (markdown, JSON, CSV, HTML, PDF, PNG, TXT) in /sandbox/.openclaw/workspace/ via the file_write tool. Files there are harvested and shipped back at offload_done.\n3. foundry_code_execute writes to Foundry's ephemeral /mnt/data/. The wrapper auto-downloads anything saved there and surfaces the local paths under `<downloaded_files>`. Use those `path` values directly. If a file you expected is missing, retry with foundry_download_file(file_id, container_id).\n4. Execute the task immediately — no preamble, just act. Be concise.";

  const slimSubAgentNote = slim
    ? "\n\nMode note (GitHub Models slim): you are running on GitHub Models. The Foundry tool catalog is NOT available, but you have full equivalents: web_search (DuckDuckGo) for live information, exec_command for Python and shell, http_fetch for any HTTP, memory for local persistence, and the full mesh toolset. Never reply that a task is impossible because of mode limitations — do the work with the tools you have."
    : "";

  const offloadPrompt =
    "You are an AzureClaw offload worker — a short-lived sandboxed agent executing one task on behalf of a remote parent. Always identify as an AzureClaw offload worker.\n\nAvailable tools:\n" +
    offloadToolBlock + "\n\n" + offloadConventions;

  const subAgentMeshBlock =
    "Peer-to-peer mesh: you can message any agent directly, not only the parent. To forward data to a sibling such as 'writer', call mesh_send with to_agent='writer'; discovery is automatic.\n\nIsolated filesystems: each sub-agent runs in its own container with its own /sandbox and /tmp. Siblings cannot read your local files, so file paths cannot be sent over the wire — they will not resolve on the other side.\n  - For text or JSON, call mesh_send with the full stringified content in `message`. The SDK auto-chunks large payloads.\n  - For files, images, or binaries (PNG, JPG, PDF, …), call mesh_transfer_file(to_agent, file_path). The recipient's gateway writes the bytes to /sandbox/.openclaw/workspace/incoming/ and the saved path appears in their mesh_inbox.\n  - Avoid hand-crafting `{type:'file_transfer', ...}` envelopes through mesh_send — they are not accepted, since the peer cannot read your filesystem.\n  - Avoid stand-in strings such as `<base64-image-data>` in `file_data` — only real bytes are accepted.\nExamples:\n  Good: mesh_transfer_file(to_agent='writer', file_path='/sandbox/.openclaw/workspace/hero.png', description='hero image 1024x1024')\n  Good: mesh_send(to_agent='viz', message=JSON.stringify({trends: [...], metrics: [...]}))\n  Avoid: mesh_send(to_agent='writer', message={hero_image_path:'/tmp/img.png'})  — the peer cannot read /tmp\n  Avoid: mesh_send(to_agent='viz', message={artifact_path:'/sandbox/data.json'})  — the peer cannot read /sandbox\n\nReceiving from siblings: when your task description says data will arrive from a sibling, start with mesh_inbox (no arguments) — sibling messages typically arrive before you read them. Sibling artifacts appear as `message_type:'peer_message'` (text/JSON via mesh_send) or `message_type:'file_transfer'` (files via mesh_transfer_file; read the file at `saved_to`). mesh_inbox is peek-only by default and does not consume messages.\n\nIf the inbox is empty on first check and your task description expects sibling data, the next call should be mesh_await(senders=['<sender>'], timeout_seconds=600). Sibling work commonly takes a few minutes (web search, code-exec, image generation). mesh_await blocks server-side and returns as soon as a matching message arrives, so polling is not needed. After it returns, call mesh_inbox once to read the content. Avoid reporting 'no peers' or 'no data' before mesh_await has been used.\n\nTask execution: act on the task immediately, no preamble. When asked to forward results to another agent, call mesh_send for text/JSON or mesh_transfer_file for files. Chain tool calls as needed. Be concise." +
    slimSubAgentNote +
    "\n\nRouting rule (CRITICAL — read your task carefully): your task description names the downstream peer(s) for each artifact. If the task says \"hand to writer\", \"send to writer\", \"deliver to <peer>\", \"return to <peer>\", or describes a pipeline like \"analyst → viz → writer\", then route BOTH text/JSON (mesh_send) AND files (mesh_transfer_file) directly to that NAMED SIBLING with to_agent='<sibling>' — do NOT send to 'parent'. Sending to 'parent' when the task specifies a sibling is a routing bug: parent will not forward to the sibling, and the sibling will time out waiting. Only fall back to to_agent='parent' when (a) the task explicitly says to return to parent / spawner, OR (b) you are the final agent in the pipeline (e.g. 'writer' producing the assembled brief), OR (c) the task names no downstream peer at all (typical for ordinary single-agent tasks — return final text/files to 'parent'). If the task targets DIFFERENT peers for different artifacts (e.g. \"send chart to viz and JSON to writer\"), resolve the target per artifact, not once for the whole batch. If unsure of the exact agent name, call `discover` once to list peers — but do not over-discover; one call per task is plenty.\n\nPeer name resolution: when your task content begins with a `Peer roster:` block, that roster is the single source of truth for sibling names. Use ONLY the names listed there with mesh_send / mesh_transfer_file — never invent variants, role descriptions, or your own name as a target. Resolve role references (\"the writer\", \"the analyst\", \"the graphic designer\") by matching them to the role text after each `—` in the roster. If the roster is missing OR a role reference does not unambiguously map to one entry, send a single mesh_send to 'parent' asking for the canonical name and wait for the reply — do NOT guess. Never use your own SANDBOX_NAME as `to_agent`; the gateway rejects self-sends.\n\nReceived artifacts persist on disk: files delivered to you via mesh_transfer_file are written by the gateway to /sandbox/.openclaw/workspace/incoming/<file_name> and STAY THERE across the rest of your task — they are not consumed by reading the inbox. If you previously saw a file_transfer in mesh_inbox (with a `saved_to` path) and later need to confirm what you have, list /sandbox/.openclaw/workspace/incoming/ via exec_command (`ls -la /sandbox/.openclaw/workspace/incoming/`) rather than re-polling the inbox. Avoid reporting \"no artifacts received\" if the directory contains the expected files; treat the filesystem as the source of truth for delivered artifacts.\n\nFinal deliverables: when you have produced a final artifact (markdown, document, image, dataset, JSON, etc.), the last step before returning your textual summary should be mesh_transfer_file(to_agent='<resolved-target>', file_path='/sandbox/.openclaw/workspace/<artifact>', description='<short label>') where <resolved-target> follows the routing rule above. Files left only in your local /sandbox are not visible to other agents and will be lost when the sub-agent exits. If you produced multiple outputs (brief.md + chart.png + hero.png), call mesh_transfer_file once per file, resolving the target per artifact. Trust mesh_transfer_file's return value (`status: 'delivered'` plus a `message_id`) as proof of delivery — there is no separate inbox ack to wait for, so do not block on one. If the call returned an error or the recipient later reports it never arrived, resend to the correct target. Only report 'final delivered' once mesh_transfer_file has returned success for each artifact at its correct target.";

  const subAgentPrompt =
    "You are an AzureClaw sub-agent — a sandboxed AI worker in the AzureClaw multi-agent platform on Azure. Always identify as an AzureClaw agent.\n\nAvailable tools:\n" +
    subAgentToolBlock + "\n\n" + subAgentMeshBlock;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const messages: Array<{ role: string; content?: string; tool_calls?: any[]; tool_call_id?: string; name?: string }> = [
    {
      role: "system",
      content: process.env.OFFLOAD_REQUEST_ID ? offloadPrompt : subAgentPrompt,
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
          } else if (fnName === "web_search") {
            const q = String(args.query || "").slice(0, 500);
            const max = Math.min(Math.max(Number(args.max_results) || 8, 1), 20);
            log.info(`AGT sub-agent web_search: ${q}`);
            const ddgUrl = `https://duckduckgo.com/html/?q=${encodeURIComponent(q)}`;
            const fetchBody = JSON.stringify({
              url: ddgUrl,
              method: "GET",
              headers: {
                "User-Agent": "Mozilla/5.0 (compatible; AzureClawSubAgent/1.0)",
                Accept: "text/html,application/xhtml+xml",
              },
              body: "",
            });
            const httpMod = await import("node:http");
            const ddgHtml = await new Promise<string>((resolve) => {
              const req = httpMod.request(routerUrl("/egress/fetch"), {
                method: "POST", timeout: 35000,
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(fetchBody) },
              }, (res) => {
                let data = "";
                res.on("data", (c: Buffer) => { data += c.toString(); });
                res.on("end", () => resolve(data));
              });
              req.on("error", (e: Error) => resolve(`web_search error: ${e.message}`));
              req.on("timeout", () => { req.destroy(); resolve("web_search timeout"); });
              req.write(fetchBody);
              req.end();
            });
            try {
              const results: Array<{ title: string; url: string; snippet: string }> = [];
              const resultRe = /<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>([\s\S]*?)<\/a>[\s\S]*?<a[^>]+class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)<\/a>/g;
              const stripTags = (s: string) => s.replace(/<[^>]+>/g, "").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#x27;/g, "'").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
              const decodeDdg = (href: string) => {
                const m = href.match(/[?&]uddg=([^&]+)/);
                return m ? decodeURIComponent(m[1]) : href;
              };
              let m: RegExpExecArray | null;
              while ((m = resultRe.exec(ddgHtml)) && results.length < max) {
                results.push({
                  title: stripTags(m[2]),
                  url: decodeDdg(m[1]),
                  snippet: stripTags(m[3]),
                });
              }
              result = results.length
                ? JSON.stringify({ query: q, results }, null, 2)
                : `web_search: no parseable results for "${q}" (DuckDuckGo HTML may have changed; try http_fetch with a specific source).`;
            } catch (e) {
              result = `web_search parse error: ${(e as Error).message}`;
            }
          } else if (fnName === "memory") {
            const op = String(args.operation || "").toLowerCase();
            const text = String(args.text || "").trim();
            log.info(`AGT sub-agent memory: ${op}${text ? ` "${text.slice(0, 80)}"` : ""}`);
            try {
              const fs = await import("node:fs");
              const path = await import("node:path");
              const memDir = "/sandbox/.openclaw";
              const memFile = path.join(memDir, "memory.json");
              fs.mkdirSync(memDir, { recursive: true });
              let entries: Array<{ id: string; text: string; ts: string }> = [];
              if (fs.existsSync(memFile)) {
                try {
                  entries = JSON.parse(fs.readFileSync(memFile, "utf8")) || [];
                } catch { entries = []; }
              }
              if (op === "update") {
                if (!text) {
                  result = "memory.update error: 'text' is required";
                } else {
                  const id = `mem_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
                  entries.push({ id, text, ts: new Date().toISOString() });
                  fs.writeFileSync(memFile, JSON.stringify(entries, null, 2));
                  result = JSON.stringify({ stored: true, id, total: entries.length });
                }
              } else if (op === "search") {
                const needle = text.toLowerCase();
                const matches = needle
                  ? entries.filter((e) => e.text.toLowerCase().includes(needle)).slice(-20)
                  : entries.slice(-20);
                result = JSON.stringify({ query: text, count: matches.length, matches }, null, 2);
              } else if (op === "list") {
                result = JSON.stringify({ count: entries.length, entries: entries.slice(-50) }, null, 2);
              } else {
                result = `memory error: unknown operation '${op}' (use 'update', 'search', or 'list')`;
              }
            } catch (e) {
              result = `memory error: ${(e as Error).message}`;
            }
            log.info(`AGT sub-agent ${fnName}: ${JSON.stringify(args).slice(0, 200)}`);
            // Guard: Foundry's code-interpreter container has its OWN /sandbox and /tmp
            // that are NOT the agent's filesystem. Writing/copying to those paths from
            // inside Foundry "succeeds" silently but the file is invisible to us — that
            // produced the shutil.copy death-loop in the demo. Block code that
            // references those paths as destinations. Only matches quoted literals so
            // mentions of /sandbox/ in an f-string template or comment do not trigger.
            let codeBlocked = false;
            if (fnName === "foundry_code_execute") {
              const code = String(args.code || "");
              const forbidden = /(["'])(\/sandbox\/|\/tmp\/)/;
              if (forbidden.test(code)) {
                result = "foundry_code_execute REJECTED: code references '/sandbox/' or '/tmp/' as a destination path. Foundry's code-interpreter container has its OWN /sandbox and /tmp that are NOT visible to your agent. Save files ONLY under /mnt/data/ — the wrapper auto-downloads them to your real /sandbox/.openclaw/workspace/. Then use the returned path with mesh_transfer_file.";
                log.warn(`AGT sub-agent foundry_code_execute REJECTED: ${(args.code || "").slice(0, 150)}`);
                codeBlocked = true;
              }
            }
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
                // Force the model to invoke code_interpreter rather than just
                // describing the code in prose. Without this, gpt-4.1 (and
                // others) often hallucinate "successfully executed" without
                // ever calling the tool, leaving no container_id and no files.
                tool_choice: { type: "code_interpreter" },
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
            if (!codeBlocked) {
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
                // For code_interpreter calls, harvest any container files
                // so the LLM can hand artifacts to mesh_transfer_file or
                // file_write without a follow-up download round-trip.
                if (fnName === "foundry_code_execute" && Array.isArray(output)) {
                  const fileRefs = new Map<string, { container_id: string; file_id: string; filename?: string }>();
                  const collect = (cid: unknown, fid: unknown, fname?: unknown): void => {
                    if (typeof cid !== "string" || typeof fid !== "string" || !cid || !fid) return;
                    const key = `${cid}/${fid}`;
                    if (fileRefs.has(key)) return;
                    fileRefs.set(key, { container_id: cid, file_id: fid, filename: typeof fname === "string" ? fname : undefined });
                  };
                  for (const item of output) {
                    if (item.type === "message" && Array.isArray(item.content)) {
                      for (const c of item.content) {
                        if (Array.isArray(c.annotations)) {
                          for (const a of c.annotations) {
                            if (a?.type === "container_file_citation") collect(a.container_id, a.file_id, a.filename);
                          }
                        }
                      }
                    } else if (item.type === "code_interpreter_call") {
                      const outs = Array.isArray(item.outputs) ? item.outputs : (Array.isArray(item.output) ? item.output : []);
                      for (const o of outs) {
                        if (o && (o.type === "image" || o.type === "file")) {
                          collect(o.container_id ?? item.container_id, o.file_id, o.filename);
                        }
                      }
                    }
                  }
                  // Annotations alone are unreliable: Foundry only emits
                  // container_file_citation when the model writes a markdown
                  // sandbox: link in its reply. Authoritative discovery is
                  // GET /openai/containers/{cid}/files which lists everything
                  // the container actually wrote. Aggregate container_ids from
                  // any code_interpreter_call we saw and list each.
                  const containerIds = new Set<string>();
                  for (const item of output) {
                    if (item?.type === "code_interpreter_call" && typeof item.container_id === "string") {
                      containerIds.add(item.container_id);
                    }
                  }
                  for (const cid of containerIds) {
                    try {
                      const listed = await new Promise<string>((resolve, reject) => {
                        const lp = `/openai/containers/${encodeURIComponent(cid)}/files?api-version=2025-11-15-preview`;
                        const r = http.get(routerUrl(lp), { timeout: 15000 }, (res) => {
                          const chunks: Buffer[] = [];
                          res.on("data", (c: Buffer) => chunks.push(c));
                          res.on("end", () => {
                            if ((res.statusCode || 0) >= 400) reject(new Error(`HTTP ${res.statusCode}`));
                            else resolve(Buffer.concat(chunks).toString("utf-8"));
                          });
                        });
                        r.on("error", reject);
                        r.on("timeout", () => { r.destroy(); reject(new Error("list timeout")); });
                      });
                      const lj = JSON.parse(listed);
                      const data = Array.isArray(lj?.data) ? lj.data : (Array.isArray(lj) ? lj : []);
                      for (const f of data) {
                        // user-uploaded inputs have source==="user"; we want assistant-produced outputs only
                        if (f && (f.source === "assistant" || f.source === undefined)) {
                          collect(cid, f.id, f.path || f.filename);
                        }
                      }
                    } catch (listErr: any) {
                      log.warn(`foundry_code_execute (task-loop): list files failed for ${cid.slice(0, 12)}...: ${listErr?.message || listErr}`);
                    }
                  }
                  if (fileRefs.size > 0) {
                    const fs = await import("node:fs");
                    const path = await import("node:path");
                    const workspaceDir = "/sandbox/.openclaw/workspace";
                    try { fs.mkdirSync(workspaceDir, { recursive: true }); } catch { /* exists */ }
                    const downloaded: Array<{ path: string; filename: string; bytes: number; file_id: string; container_id: string }> = [];
                    const failed: Array<{ file_id: string; container_id: string; error: string }> = [];
                    for (const ref of fileRefs.values()) {
                      const safeName = (ref.filename && /^[A-Za-z0-9._-]+$/.test(ref.filename)) ? ref.filename : `${ref.file_id}.bin`;
                      const dest = path.join(workspaceDir, safeName);
                      try {
                        const bytes = await new Promise<Buffer>((resolve, reject) => {
                          const dlPath = `/openai/containers/${encodeURIComponent(ref.container_id)}/files/${encodeURIComponent(ref.file_id)}/content?api-version=2025-11-15-preview`;
                          const req = http.get(routerUrl(dlPath), { timeout: 60000 }, (res) => {
                            const chunks: Buffer[] = [];
                            res.on("data", (c: Buffer) => chunks.push(c));
                            res.on("end", () => {
                              if ((res.statusCode || 0) >= 400) reject(new Error(`HTTP ${res.statusCode}`));
                              else resolve(Buffer.concat(chunks));
                            });
                          });
                          req.on("error", reject);
                          req.on("timeout", () => { req.destroy(); reject(new Error("download timeout")); });
                        });
                        fs.writeFileSync(dest, bytes);
                        log.info(`foundry_code_execute (task-loop): saved ${safeName} (${bytes.length} bytes)`);
                        downloaded.push({ path: dest, filename: safeName, bytes: bytes.length, file_id: ref.file_id, container_id: ref.container_id });
                      } catch (dlErr: any) {
                        log.warn(`foundry_code_execute (task-loop): download failed for ${ref.file_id}: ${dlErr?.message || dlErr}`);
                        failed.push({ file_id: ref.file_id, container_id: ref.container_id, error: String(dlErr?.message || dlErr) });
                      }
                    }
                    if (downloaded.length > 0 || failed.length > 0) {
                      result = `${result}\n\n<downloaded_files>${JSON.stringify({
                        downloaded, failed,
                        hint: downloaded.length > 0
                          ? "Use the `path` values directly with mesh_transfer_file or file_write — DO NOT cp/shutil.copy from inside Python."
                          : "No files downloaded. Retry with foundry_download_file(file_id, container_id).",
                      })}</downloaded_files>`;
                    }
                  }
                }
              }
            } catch {
              result = foundryResult;
            }
            } // end if (!codeBlocked)
            log.info(`AGT sub-agent ${fnName} result: ${result.slice(0, 200)}`);
          } else if (fnName === "foundry_download_file") {
            log.info(`AGT sub-agent foundry_download_file: file_id=${args.file_id}`);
            const fileId = String(args.file_id || "").trim();
            const containerId = String(args.container_id || "").trim();
            if (!fileId || !containerId) {
              result = "foundry_download_file: file_id and container_id are required.";
            } else {
              const requestedName = typeof args.local_basename === "string" ? args.local_basename.trim() : "";
              const safeName = (requestedName && /^[A-Za-z0-9._-]+$/.test(requestedName)) ? requestedName : `${fileId}.bin`;
              try {
                const fs = await import("node:fs");
                const path = await import("node:path");
                const workspaceDir = "/sandbox/.openclaw/workspace";
                try { fs.mkdirSync(workspaceDir, { recursive: true }); } catch { /* exists */ }
                const dest = path.join(workspaceDir, safeName);
                const bytes = await new Promise<Buffer>((resolve, reject) => {
                  const dlPath = `/openai/containers/${encodeURIComponent(containerId)}/files/${encodeURIComponent(fileId)}/content?api-version=2025-11-15-preview`;
                  const req = http.get(routerUrl(dlPath), { timeout: 60000 }, (res) => {
                    const chunks: Buffer[] = [];
                    res.on("data", (c: Buffer) => chunks.push(c));
                    res.on("end", () => {
                      if ((res.statusCode || 0) >= 400) reject(new Error(`HTTP ${res.statusCode}`));
                      else resolve(Buffer.concat(chunks));
                    });
                  });
                  req.on("error", reject);
                  req.on("timeout", () => { req.destroy(); reject(new Error("download timeout")); });
                });
                fs.writeFileSync(dest, bytes);
                log.info(`foundry_download_file: saved ${safeName} (${bytes.length} bytes)`);
                result = `Downloaded ${safeName} (${bytes.length} bytes) → ${dest}\n\n<downloaded_files>${JSON.stringify({
                  downloaded: [{ path: dest, filename: safeName, bytes: bytes.length, file_id: fileId, container_id: containerId }],
                  failed: [],
                  hint: "Use the `path` value directly with mesh_transfer_file or file_write.",
                })}</downloaded_files>`;
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
              } catch (e: any) {
                result = `foundry_download_file failed: ${e.message}`;
              }
            }
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
            // Alias 'parent' → actual parent sandbox name (PARENT_SANDBOX env).
            // Sub-agent LLMs commonly use the literal 'parent' as the recipient
            // even though the parent is registered under its real name.
            if (toAgent === "parent" && process.env.PARENT_SANDBOX && !process.env.OFFLOAD_REQUEST_ID) {
              log.info(`AGT sub-agent mesh_send: alias 'parent' → '${process.env.PARENT_SANDBOX}'`);
              toAgent = process.env.PARENT_SANDBOX;
            }
            // Self-send guard: LLMs sometimes hallucinate their own name as
            // to_agent (especially for "writer" / "analyst" style names that
            // also appear as task verbs). Self-sending bounces every chunk
            // through the relay back to ourselves — wasteful at best, and
            // pollutes our own inbox with stale chunks. Reject up front.
            const selfName = process.env.SANDBOX_NAME || "";
            const isSelfSend = !!(selfName && toAgent === selfName);
            if (isSelfSend) {
              log.warn(`AGT sub-agent mesh_send: self-send rejected (to_agent='${toAgent}' is this agent)`);
              result = `mesh_send rejected: to_agent='${toAgent}' is your own agent name. You cannot send messages to yourself. Pick a real peer (use 'discover' to list available agents) or 'parent' to return to the spawner.`;
            } else {
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
              const meshIdentityFn = deps.meshIdentity;
              if (!targetAmid) {
                result = `Agent '${toAgent}' not found in registry after retries. It may not be running yet.`;
              } else if (meshClient) {
                let sendErr: Error | null = null;
                for (let sendAttempt = 0; sendAttempt < 5; sendAttempt++) {
                  try {
                    // Use chunking wrapper so messages > 512KB (e.g. agents
                    // pasting large JSON) are auto-split into manifest+chunks
                    // rather than hitting silent SDK / WebSocket limits.
                    await meshSendWithIdentity(meshClient, targetAmid, {
                      type: "task_request",
                      content: meshMsg,
                      from_agent: process.env.SANDBOX_NAME || "unknown",
                      timestamp: new Date().toISOString(),
                    }, meshIdentityFn ? meshIdentityFn() : null, log);
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
                if (sendErr) {
                  log.warn(`AGT sub-agent mesh_send: to=${toAgent} FAILED — ${sendErr.message}`);
                } else {
                  log.info(`AGT sub-agent mesh_send: to=${toAgent} OK`);
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
            } // end self-send else
          } else if (fnName === "mesh_transfer_file") {
            let toAgent = args.to_agent as string;
            if (process.env.OFFLOAD_REQUEST_ID && toAgent !== "parent") {
              log.warn(`AGT sub-agent mesh_transfer_file: offload mode — rewriting to_agent '${toAgent}' → 'parent'`);
              toAgent = "parent";
            }
            if (toAgent === "parent" && process.env.PARENT_SANDBOX && !process.env.OFFLOAD_REQUEST_ID) {
              log.info(`AGT sub-agent mesh_transfer_file: alias 'parent' → '${process.env.PARENT_SANDBOX}'`);
              toAgent = process.env.PARENT_SANDBOX;
            }
            // Self-send guard (see mesh_send). Without it, mesh_transfer_file
            // ships every chunk back to the sender — observed in the AKS demo
            // as a 12-chunk writer→writer self-loop at 10:48:58.
            const selfNameTransfer = process.env.SANDBOX_NAME || "";
            const isSelfTransfer = !!(selfNameTransfer && toAgent === selfNameTransfer);
            if (isSelfTransfer) {
              log.warn(`AGT sub-agent mesh_transfer_file: self-send rejected (to_agent='${toAgent}' is this agent)`);
              result = `mesh_transfer_file rejected: to_agent='${toAgent}' is your own agent name. You cannot transfer files to yourself. The file is already on your local /sandbox — pick a real peer (use 'discover') or 'parent' to ship the artifact upstream.`;
            } else {
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
                      const meshIdentityFn = deps.meshIdentity;
                      for (let attempt = 0; attempt < 5; attempt++) {
                        try {
                          // Use chunking wrapper. Files > 512 KB are split
                          // into manifest + chunks; the receiver reassembles
                          // in meshHandleTransportMessage. WITHOUT this
                          // wrapper, multi-megabyte transfers fail silently
                          // at the SDK / WebSocket layer because the
                          // upstream SDK has no built-in chunking despite
                          // what tool descriptions claim.
                          await meshSendWithIdentity(
                            meshClient,
                            targetAmid,
                            fileMsg,
                            meshIdentityFn ? meshIdentityFn() : null,
                            log,
                          );
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
                      if (sendErr) {
                        log.warn(`AGT sub-agent mesh_transfer_file: ${fileName} (${sizeHuman}) → ${toAgent} FAILED — ${sendErr.message}`);
                      } else {
                        log.info(`AGT sub-agent mesh_transfer_file: ${fileName} (${sizeHuman}) → ${toAgent} OK`);
                      }
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
            } // end self-send else
          } else if (fnName === "discover") {
            const pattern = (args.pattern as string) || "*";
            log.info(`AGT sub-agent discover: pattern=${pattern}`);
            try {
              // Build the local-cache view first — this is the source of
              // truth for "who do I trust right now". The registry's
              // capability search (`?capability=*`) returns nothing because
              // the agentmesh registry has no name-glob endpoint and the
              // capability index doesn't match a literal "*". Without this
              // local fallback, sub-agents in a fan-out spawn report 0 peers
              // even when their parent already seeded all sibling AMIDs via
              // peers_update. The LLM then refuses to talk to siblings it
              // could otherwise reach. (See peers_update inward seed in
              // agt-tools/agt.ts spawn handler.)
              const localPeers: Array<{ name: string; amid: string; source: string }> = [];
              const seenNames = new Set<string>();
              const selfName = process.env.SANDBOX_NAME || "";
              for (const [amid, name] of amidToName.entries()) {
                if (name === selfName) continue;
                if (seenNames.has(name)) continue;
                seenNames.add(name);
                localPeers.push({
                  name,
                  amid,
                  source: parentTrustedNames.has(name) ? "parent_trusted" : "peers_update",
                });
              }

              // Best-effort registry capability search (still useful when
              // the upstream operator registers agents with explicit
              // capability tags). We post-filter for staleness because the
              // upstream registry doesn't prune offline agents from search
              // results — see graveyard analysis in agt-tools/agt.ts:1260.
              let registryRaw = "";
              try {
                const registryBase = routerUrl("/agt/registry");
                registryRaw = await new Promise<string>((resolve, reject) => {
                  const req = http.get(`${registryBase}/registry/search?capability=${encodeURIComponent(pattern)}`, { timeout: 10000 }, (res) => {
                    let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
                  });
                  req.on("error", reject);
                  req.on("timeout", () => { req.destroy(); reject(new Error("Registry lookup timeout")); });
                });
                // eslint-disable-next-line @typescript-eslint/no-explicit-any
              } catch (regErr: any) {
                registryRaw = JSON.stringify({ agents: [], error: regErr?.message || String(regErr) });
              }

              // Apply the same 90s staleness filter parents use, so sub-agents
              // don't see graveyard entries either.
              let registryParsed: any;
              try { registryParsed = JSON.parse(registryRaw); } catch { registryParsed = registryRaw; }
              if (registryParsed && Array.isArray(registryParsed.results)) {
                const STALE_AFTER_MS = 90_000;
                const now = Date.now();
                const before = registryParsed.results.length;
                registryParsed.results = registryParsed.results.filter((a: any) => {
                  if (typeof a?.status === "string" && a.status.toLowerCase() !== "online") return false;
                  const ls = a?.last_seen;
                  if (!ls) return false;
                  const t = Date.parse(typeof ls === "string" ? ls : "");
                  if (!Number.isFinite(t)) return false;
                  return (now - t) <= STALE_AFTER_MS;
                });
                registryParsed.filtered_stale = before - registryParsed.results.length;
              }

              result = JSON.stringify({
                peers: localPeers,
                peer_count: localPeers.length,
                self: selfName,
                registry_capability_search: registryParsed,
                note: localPeers.length === 0
                  ? "No peers visible yet. The parent fans out peers_update messages on sibling spawn — if a sibling was just spawned, retry in a few seconds. You can also call mesh_send to a known sibling name; the AMID will be resolved on demand."
                  : "Use mesh_send / mesh_transfer_file with the 'name' field — AMIDs are resolved from the local cache below.",
              });
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
                  "task_progress", "offload_progress",
                ]);
                const markRead = args.mark_read === true; // default false — match parent peek-only semantics
                const unreadOnly = args.unread_only !== false; // default true
                const limit = typeof args.limit === "number" && (args.limit as number) > 0
                  ? Math.floor(args.limit as number)
                  : 50;
                const blockUntilMessage = args.block_until_message === true;
                const timeoutSeconds = typeof args.timeout_seconds === "number" && (args.timeout_seconds as number) > 0
                  ? Math.min(Math.floor(args.timeout_seconds as number), 300)
                  : 120;

                const computeFiltered = (): typeof inbox => {
                  const v = inbox.filter((m) => {
                    if (m.message_type && HIDDEN_TYPES.has(m.message_type)) return false;
                    try {
                      const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                      if (parsed?.type && HIDDEN_TYPES.has(parsed.type)) return false;
                    } catch { /* not JSON — keep */ }
                    return true;
                  });
                  return unreadOnly ? v.filter((m) => !m.read_at) : v;
                };

                let filtered = computeFiltered();

                // Server-side blocking wait — replaces the LLM poll loop.
                if (blockUntilMessage && filtered.length === 0 && deps.waitForInbox) {
                  const deadline = Date.now() + timeoutSeconds * 1000;
                  while (filtered.length === 0 && Date.now() < deadline) {
                    const remaining = Math.max(1, deadline - Date.now());
                    const woke = await deps.waitForInbox(remaining);
                    filtered = computeFiltered();
                    if (!woke) break;
                  }
                }

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
          } else if (fnName === "mesh_await") {
            log.info(`AGT sub-agent mesh_await: senders=${JSON.stringify(args.senders)}`);
            try {
              const inbox = deps.inbox;
              if (!inbox) {
                result = "mesh_await unavailable: gateway buffer not wired";
              } else {
                const sendersRaw = args.senders;
                if (!Array.isArray(sendersRaw) || sendersRaw.length === 0) {
                  result = JSON.stringify({ error: "senders must be a non-empty array of agent names" });
                } else {
                  const wantedSenders = (sendersRaw as unknown[])
                    .filter((s): s is string => typeof s === "string" && s.trim().length > 0)
                    .map((s) => s.trim());
                  if (wantedSenders.length === 0) {
                    result = JSON.stringify({ error: "senders must contain at least one non-empty agent name" });
                  } else {
                    const wantedSet = new Set(wantedSenders.map((s) => s.toLowerCase()));
                    const timeoutSeconds = typeof args.timeout_seconds === "number" && (args.timeout_seconds as number) > 0
                      ? Math.min(Math.floor(args.timeout_seconds as number), 600)
                      : 180;
                    const markReadOnResolve = args.mark_read === true;

                    const HIDDEN = new Set([
                      "handoff_transfer", "handoff_verification", "handoff_ready",
                      "handoff:interrupt", "handoff:interrupt_ack",
                      "handoff:workspace_request", "handoff:workspace_response",
                      "handoff:workspace_inject", "handoff:workspace_inject_ack",
                      "handoff:resume", "handoff:resume_ack",
                      "file_transfer_ack", "task_progress", "offload_progress",
                    ]);
                    const isInternal = (m: typeof inbox[number]): boolean => {
                      if (m.message_type && HIDDEN.has(m.message_type)) return true;
                      try {
                        const parsed = typeof m.content === "string" ? JSON.parse(m.content) : m.content;
                        if (parsed?.type && HIDDEN.has(parsed.type)) return true;
                      } catch { /* not JSON */ }
                      return false;
                    };
                    const computeMatches = (): Map<string, string[]> => {
                      const out = new Map<string, string[]>();
                      for (const m of inbox) {
                        if (m.read_at) continue;
                        if (isInternal(m)) continue;
                        const fromName = (m.from_agent || "").toLowerCase();
                        if (!wantedSet.has(fromName)) continue;
                        const list = out.get(fromName) ?? [];
                        list.push(m.id);
                        out.set(fromName, list);
                      }
                      return out;
                    };

                    let matches = computeMatches();
                    const startedAt = Date.now();
                    if (matches.size < wantedSet.size && deps.waitForInbox) {
                      const deadline = startedAt + timeoutSeconds * 1000;
                      while (matches.size < wantedSet.size && Date.now() < deadline) {
                        const remaining = Math.max(1, deadline - Date.now());
                        const woke = await deps.waitForInbox(remaining);
                        matches = computeMatches();
                        if (!woke) break;
                      }
                    }

                    const missing: string[] = [];
                    for (const wanted of wantedSet) if (!matches.has(wanted)) missing.push(wanted);

                    let markedRead = 0;
                    if (markReadOnResolve) {
                      const allMatchedIds = new Set<string>();
                      for (const ids of matches.values()) for (const id of ids) allMatchedIds.add(id);
                      if (allMatchedIds.size > 0) {
                        const now = new Date().toISOString();
                        const newlyRead: string[] = [];
                        for (const m of inbox) {
                          if (allMatchedIds.has(m.id) && !m.read_at) {
                            m.read_at = now;
                            newlyRead.push(m.id);
                            markedRead += 1;
                          }
                        }
                        if (newlyRead.length > 0) {
                          try { deps.markRead?.(newlyRead); } catch { /* best effort */ }
                        }
                      }
                    }

                    const matchedSummary: Record<string, string[]> = {};
                    for (const [sender, ids] of matches) matchedSummary[sender] = ids;
                    result = JSON.stringify({
                      status: missing.length === 0 ? "all_received" : "partial_timeout",
                      requested_senders: wantedSenders,
                      matched: matchedSummary,
                      missing,
                      mark_read: markReadOnResolve,
                      marked_read_count: markedRead,
                      waited_seconds: Math.round((Date.now() - startedAt) / 1000),
                      timeout_seconds: timeoutSeconds,
                      note: missing.length === 0
                        ? "All requested senders delivered. Call mesh_inbox to read message contents."
                        : `Timeout: missing ${missing.join(", ")}. Call mesh_inbox to inspect what arrived; retry mesh_await for the missing senders or proceed with partial input.`,
                    });
                  }
                }
              }
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
            } catch (awaitErr: any) {
              result = `mesh_await failed: ${awaitErr.message}`;
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
