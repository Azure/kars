// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Foundry AGT tool registrations — extracted from plugin.ts in S15.f.8.
//
// Nine tools that proxy through the inference router to Azure AI Foundry
// services. All are read-only / RPC-style: no closure over plugin.ts state,
// no mesh interaction. Safe to extract as a self-contained module.
//
// Tools registered:
//   foundry_code_execute     foundry_image_generation
//   foundry_web_search       foundry_file_search
//   foundry_memory           foundry_conversations
//   foundry_evaluations      foundry_deployments
//   foundry_agents

import { routerCall, routerCallBinary, callPlatformTool } from "../router-client.js";
import { safeJson } from "../safe-json.js";
import type { FoundryProjectInfo } from "../foundry-discovery.js";

// _routerCall is the historical alias used inside these blocks; keep it for
// byte-identical bodies.
const _routerCall = routerCall;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyApi = any;

export interface FoundryToolsDeps {
  // Logger surface (subset of OpenClaw logger used in tool bodies).
  log: { info: (m: string) => void; warn: (m: string) => void };
  // Read-only access to the resolved plugin config (only `config.model` is
  // referenced inside foundry tools).
  config: { model: string };
  // Late-bound accessor for the Foundry project discovered asynchronously by
  // `initFoundry`; tool bodies reference it through `getFoundryProject()` since
  // discovery may complete after `register()` returns.
  getFoundryProject: () => FoundryProjectInfo | null;
}

export function registerFoundryTools(api: AnyApi, deps: FoundryToolsDeps): void {
  const { log, config } = deps;
  const getFoundryProject = deps.getFoundryProject;
  api.registerTool({
    name: "foundry_code_execute",
    label: "Foundry Code Interpreter",
    description:
      "Execute Python code server-side via Azure AI Foundry's code_interpreter. " +
      "Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, " +
      "charts, complex math, and file processing. Runs in a managed Foundry sandbox " +
      "(not the local sandbox). No egress policy needed.\n\n" +
      "📂 OUTPUT FILES — IMPORTANT: " +
      "(1) Foundry's container is EPHEMERAL — files written to `/mnt/data/<name>` " +
      "in the snippet exist only for the duration of the call. " +
      "(2) This tool ALREADY downloads any chart / image / CSV / file that the " +
      "snippet writes to `/mnt/data/` and saves it under " +
      "`/sandbox/.openclaw/workspace/<filename>`. The downloaded paths are " +
      "returned in the `downloaded_files` field of the tool result. " +
      "(3) DO NOT shell out (`cp`, `mv`, `shutil.copy`) inside Python to copy " +
      "files into `/sandbox/...` — the Foundry sandbox cannot see your local " +
      "filesystem; that path does not exist there. Just write to " +
      "`/mnt/data/<name>` and read the `downloaded_files` field. " +
      "(4) If a file you expected does not appear in `downloaded_files`, retry " +
      "with `foundry_download_file(file_id=..., container_id=...)` using the " +
      "ids surfaced in the call output.",
    parameters: {
      type: "object",
      properties: {
        input: {
          type: "string",
          description: "Natural language instruction or Python code to execute. " +
            "The model will write and run Python code to fulfill the request. " +
            "To produce a downloadable artifact, write it to `/mnt/data/<filename>`.",
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
        // Guard: Foundry's code-interpreter container has its OWN /sandbox
        // and /tmp that are not visible to us. Block code that tries to write
        // to those paths — those copies "succeed" silently and we'd never
        // see the file. Save under /mnt/data/ instead.
        const inputStr = typeof params.input === "string" ? params.input : "";
        const forbidden = /(["'])(\/sandbox\/|\/tmp\/)/;
        if (forbidden.test(inputStr)) {
          return { content: [{ type: "text", text: "foundry_code_execute REJECTED: code references '/sandbox/' or '/tmp/' as a destination path. Foundry's code-interpreter container has its OWN /sandbox and /tmp that are NOT visible to your agent. Save files ONLY under /mnt/data/ — the wrapper auto-downloads them to your real /sandbox/.openclaw/workspace/. Then use the returned `path` value with mesh_transfer_file." }] };
        }
        const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
          model: (params.model as string) || "gpt-4.1",
          input: params.input,
          tools: [{ type: "code_interpreter", container: { type: "auto" } }],
          // Force code_interpreter invocation. Without tool_choice, the
          // model often just describes the code in prose without actually
          // executing it — producing no container_id and no output files.
          tool_choice: { type: "code_interpreter" },
          store: false,
        });
        // Extract text output from Responses API format
        const output = result.output || result;
        const textParts: string[] = [];
        // Container file references found in code_interpreter output —
        // matplotlib PNGs / CSVs / etc. live in Foundry's per-run container
        // and must be downloaded explicitly. Keys are "<container>/<file>"
        // to dedupe across multiple annotations referencing the same file.
        const fileRefs = new Map<
          string,
          { container_id: string; file_id: string; filename?: string }
        >();
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const collectFileRef = (cid: unknown, fid: unknown, fname?: unknown) => {
          if (typeof cid !== "string" || typeof fid !== "string") return;
          if (!cid || !fid) return;
          const key = `${cid}/${fid}`;
          if (fileRefs.has(key)) return;
          fileRefs.set(key, {
            container_id: cid,
            file_id: fid,
            filename: typeof fname === "string" ? fname : undefined,
          });
        };
        if (Array.isArray(output)) {
          for (const item of output) {
            if (item.type === "message" && item.content) {
              for (const c of item.content) {
                if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                // Look for container_file_citation annotations on the text.
                if (Array.isArray(c.annotations)) {
                  for (const a of c.annotations) {
                    if (a?.type === "container_file_citation") {
                      collectFileRef(a.container_id, a.file_id, a.filename);
                    }
                  }
                }
              }
            } else if (item.type === "code_interpreter_call") {
              const codeBlock = `\`\`\`python\n${item.code}\n\`\`\``;
              // Newer Responses API shape: outputs is an array of objects
              // (e.g. { type: "image", file_id, container_id }, or
              // { type: "logs", logs: "..." }).
              const outputsArr = Array.isArray(item.outputs)
                ? item.outputs
                : (Array.isArray(item.output) ? item.output : []);
              const logLines: string[] = [];
              for (const out of outputsArr) {
                if (!out || typeof out !== "object") continue;
                if (out.type === "logs" && typeof out.logs === "string") {
                  logLines.push(out.logs);
                } else if (out.type === "image" || out.type === "file") {
                  collectFileRef(
                    out.container_id ?? item.container_id,
                    out.file_id,
                    out.filename,
                  );
                }
              }
              const tail = logLines.length > 0
                ? `\nOutput: ${logLines.join("\n")}`
                : (typeof item.output === "string" ? `\nOutput: ${item.output}` : "");
              textParts.push(`${codeBlock}${tail}`);
            }
          }
        }
        // Annotation harvesting alone is unreliable: Foundry only emits
        // container_file_citation when the model writes a markdown sandbox:
        // link in its reply. Authoritative discovery is GET /openai/containers/
        // {cid}/files which lists everything actually written. Aggregate
        // container_ids from any code_interpreter_call we saw and list each.
        try {
          const containerIds = new Set<string>();
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item?.type === "code_interpreter_call" && typeof item.container_id === "string") {
                containerIds.add(item.container_id);
              }
            }
          }
          for (const cid of containerIds) {
            try {
              const listed = await routerCall("GET", `/openai/containers/${encodeURIComponent(cid)}/files?api-version=2025-11-15-preview`);
              const data = Array.isArray(listed?.data) ? listed.data : (Array.isArray(listed) ? listed : []);
              for (const f of data) {
                // Skip user-uploaded inputs; only collect assistant-produced files.
                if (f && (f.source === "assistant" || f.source === undefined)) {
                  collectFileRef(cid, f.id, f.path || f.filename);
                }
              }
            } catch (listErr: any) {
              log.warn(`foundry_code_execute: list files failed for ${cid.slice(0, 12)}...: ${listErr?.message || listErr}`);
            }
          }
        } catch { /* best-effort */ }
        // Download every container file we discovered and write it to the
        // local workspace, so downstream tools (mesh_transfer_file, file_write,
        // mesh_send) can ship the bytes back to the parent.
        const downloadedFiles: Array<{
          path: string;
          filename: string;
          bytes: number;
          file_id: string;
          container_id: string;
        }> = [];
        const failedDownloads: Array<{
          file_id: string;
          container_id: string;
          error: string;
        }> = [];
        if (fileRefs.size > 0) {
          const fs = await import("node:fs");
          const path = await import("node:path");
          const workspaceDir = "/sandbox/.openclaw/workspace";
          try { fs.mkdirSync(workspaceDir, { recursive: true }); } catch { /* exists */ }
          const downloadedLines: string[] = [];
          for (const ref of fileRefs.values()) {
            const safeName = (ref.filename && /^[A-Za-z0-9._-]+$/.test(ref.filename))
              ? ref.filename
              : `${ref.file_id}.bin`;
            const dest = path.join(workspaceDir, safeName);
            const dlPath = `/openai/containers/${encodeURIComponent(ref.container_id)}/files/${encodeURIComponent(ref.file_id)}/content?api-version=2025-11-15-preview`;
            try {
              const bytes = await routerCallBinary(dlPath, 60000);
              fs.writeFileSync(dest, bytes);
              log.info(`foundry_code_execute: saved ${safeName} (${bytes.length} bytes) from container ${ref.container_id.slice(0, 12)}...`);
              downloadedLines.push(`- ${dest} (${bytes.length} bytes, file_id=${ref.file_id})`);
              downloadedFiles.push({
                path: dest,
                filename: safeName,
                bytes: bytes.length,
                file_id: ref.file_id,
                container_id: ref.container_id,
              });
            } catch (dlErr: any) {
              log.warn(`foundry_code_execute: download failed for ${ref.file_id}: ${dlErr?.message || dlErr}`);
              downloadedLines.push(`- (download failed) file_id=${ref.file_id}: ${dlErr?.message || dlErr}`);
              failedDownloads.push({
                file_id: ref.file_id,
                container_id: ref.container_id,
                error: String(dlErr?.message || dlErr),
              });
            }
          }
          if (downloadedLines.length > 0) {
            textParts.push(`\nGenerated files (saved to local workspace):\n${downloadedLines.join("\n")}`);
          }
        }
        // Surface a structured tail block so the LLM can parse paths
        // deterministically and route them to mesh_transfer_file.
        if (downloadedFiles.length > 0 || failedDownloads.length > 0) {
          textParts.push(`\n<downloaded_files>${JSON.stringify({
            downloaded: downloadedFiles,
            failed: failedDownloads,
            hint: downloadedFiles.length > 0
              ? "Use the `path` values directly with mesh_transfer_file or file_write — DO NOT cp/shutil.copy from inside Python."
              : "No files downloaded. If a file was expected, retry with foundry_download_file(file_id, container_id).",
          })}</downloaded_files>`);
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

  // ── Foundry Download File: escape-hatch for missed container files ──
  api.registerTool({
    name: "foundry_download_file",
    label: "Foundry Download File",
    description:
      "Download a single file from a Foundry code_interpreter container by " +
      "file_id + container_id and save it to /sandbox/.openclaw/workspace/. " +
      "Use this ONLY when foundry_code_execute did NOT auto-download a file " +
      "you expected (e.g. the snippet wrote it to /mnt/data/ but the response " +
      "lacked a container_file_citation annotation, or download failed in the " +
      "main call). The ids are visible in the foundry_code_execute output " +
      "(`code_interpreter_call.outputs[].file_id` / `container_id`). Returns " +
      "the local path so you can hand it to mesh_transfer_file or file_write.",
    parameters: {
      type: "object",
      properties: {
        file_id: {
          type: "string",
          description: "Foundry container file id (e.g. cfile_abc123).",
        },
        container_id: {
          type: "string",
          description: "Foundry container id (e.g. cntr_abc123). Surfaced alongside file_id in the code_interpreter_call output.",
        },
        local_basename: {
          type: "string",
          description: "Optional output filename (e.g. 'chart.png'). If omitted, defaults to '<file_id>.bin'. Must be a single safe filename — no slashes.",
        },
      },
      required: ["file_id", "container_id"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const fileId = String(params.file_id || "").trim();
      const containerId = String(params.container_id || "").trim();
      if (!fileId || !containerId) {
        return { content: [{ type: "text", text: "foundry_download_file: file_id and container_id are required." }] };
      }
      const requestedName = typeof params.local_basename === "string"
        ? params.local_basename.trim()
        : "";
      const safeName = (requestedName && /^[A-Za-z0-9._-]+$/.test(requestedName))
        ? requestedName
        : `${fileId}.bin`;
      try {
        const fs = await import("node:fs");
        const path = await import("node:path");
        const workspaceDir = "/sandbox/.openclaw/workspace";
        try { fs.mkdirSync(workspaceDir, { recursive: true }); } catch { /* exists */ }
        const dest = path.join(workspaceDir, safeName);
        const dlPath = `/openai/containers/${encodeURIComponent(containerId)}/files/${encodeURIComponent(fileId)}/content?api-version=2025-11-15-preview`;
        const bytes = await routerCallBinary(dlPath, 60000);
        fs.writeFileSync(dest, bytes);
        log.info(`foundry_download_file: saved ${safeName} (${bytes.length} bytes) from container ${containerId.slice(0, 12)}...`);
        return {
          content: [{
            type: "text",
            text: `Downloaded ${safeName} (${bytes.length} bytes) → ${dest}\n\n<downloaded_files>${JSON.stringify({
              downloaded: [{
                path: dest,
                filename: safeName,
                bytes: bytes.length,
                file_id: fileId,
                container_id: containerId,
              }],
              failed: [],
              hint: "Use the `path` value directly with mesh_transfer_file or file_write.",
            })}</downloaded_files>`,
          }],
        };
      } catch (e: any) {
        return { content: [{ type: "text", text: `foundry_download_file failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Image Generation: create images from text ───────────────
  api.registerTool({
    name: "foundry_image_generation",
    label: "Foundry Image Generation",
    description:
      "Generate images from text prompts via Azure AI Foundry's image_generation tool. " +
      "Supports any deployed image model (gpt-image-1, FLUX.2-pro, etc.). " +
      "Images are decoded from base64 and saved under " +
      "`/sandbox/.openclaw/workspace/<output_filename>` (default `image-<ts>.png`) " +
      "so the returned `path` is directly usable with `kars_mesh_transfer_file`. " +
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
        output_filename: {
          type: "string",
          description:
            "Optional stable filename (no directory components, .png recommended) under " +
            "/sandbox/.openclaw/workspace/. Defaults to image-<ts>.png. Use a stable name " +
            "like 'hero.png' when you need to mesh_transfer_file the result downstream.",
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
        // Persist into the sandbox workspace so the file is directly visible
        // to the agent FS and can be handed to kars_mesh_transfer_file
        // without a copy step. Matches the foundry_code_execute download path.
        const workspaceDir = "/sandbox/.openclaw/workspace";
        try { fs.mkdirSync(workspaceDir, { recursive: true }); } catch { /* exists */ }

        // Sanitise caller-supplied filename: keep basename only, default to .png
        const rawName = typeof params.output_filename === "string" ? params.output_filename.trim() : "";
        const safeBase = rawName
          ? nodePath.basename(rawName).replace(/[^A-Za-z0-9._-]+/g, "_")
          : "";
        const defaultName = `image-${Date.now()}.png`;
        const baseName = safeBase || defaultName;

        let idx = 0;
        for (const img of images) {
          if (img.b64_json) {
            // When multiple images come back, suffix the stable name with -N
            // so the caller never silently overwrites a previous file.
            const fileName = images.length > 1 && idx > 0
              ? baseName.replace(/(\.[A-Za-z0-9]+)?$/, (m: string) => `-${idx}${m || ".png"}`)
              : baseName;
            const imgFile = nodePath.join(workspaceDir, fileName);
            const bytes = Buffer.from(img.b64_json, "base64");
            fs.writeFileSync(imgFile, bytes);
            parts.push(`📁 Image saved: ${imgFile} (${bytes.length} bytes)`);
            parts.push(`path: ${imgFile}`);
            if (img.revised_prompt) parts.push(`Revised prompt: ${img.revised_prompt}`);
            idx++;
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
        // Keyless, Microsoft-managed web search. The `web_search` tool uses a
        // Microsoft-managed Bing resource and authenticates via the router's
        // Entra/IMDS token (ai.azure.com) — NO Bing API key, NO user-created
        // `Microsoft.Bing/accounts` resource, and NO `GroundingWithBingSearch`
        // connection on the project. This is the right fit for kars's
        // no-API-keys principle. (The classic `bing_grounding` tool, by
        // contrast, REQUIRES a key-based connection's `project_connection_id`
        // and 400s without one — which is what previously broke this tool when
        // no/stale Bing connection existed.)
        const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
          model: (params.model as string) || "gpt-4.1",
          input: params.query,
          tools: [{ type: "web_search" }],
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
            name: params.store_name || "kars-store",
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
  // Thin client. The router's platform MCP `foundry.memory` tool owns the
  // Memory Store REST contract, store/scope resolution from the KarsMemory
  // binding, auto-provision, retry, and CRD status reporting. This handler
  // only forwards intent — no Foundry contract logic lives in the agent.
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
        scope: {
          type: "string",
          description: "Memory scope/partition (default: the bound KarsMemory scope, else 'agent_<sandbox>'). Use '_' as the separator (e.g. 'session_<id>', 'user_<id>') — colons are rejected by the API.",
        },
        top_k: {
          type: "integer",
          description: "For 'search': max memories to return (default 10).",
        },
      },
      required: ["operation"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      const op = String(params.operation ?? "");
      const args: Record<string, unknown> = { operation: op };
      if (typeof params.text === "string" && params.text.length > 0) args.text = params.text;
      if (typeof params.scope === "string" && params.scope.length > 0) args.scope = params.scope;
      if (typeof params.top_k === "number") args.top_k = params.top_k;
      try {
        const { text, isError } = await callPlatformTool("foundry.memory", args);
        return { content: [{ type: "text", text }], isError };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry memory failed: ${e.message}` }], isError: true };
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
          const cachedFp = getFoundryProject();
          if (cachedFp?.deployments && cachedFp.deployments.length > 0) {
            return { content: [{ type: "text", text: safeJson({
              source: "cached_discovery",
              total: cachedFp.deployments.length,
              models: cachedFp.deployments,
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
      "(different from kars sub-agent sandboxes).",
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
}
