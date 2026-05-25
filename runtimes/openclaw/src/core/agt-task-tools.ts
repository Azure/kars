// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Tool-calling loop tool definitions — extracted from
// plugin.ts processTaskWithTools() in S15.f.4.
//
// Pure OpenAI function-call schema array. No closures, no runtime
// dependencies — these are inert tool descriptors consumed by the
// chat-completions request body the offload / sub-agent loop sends.
// The actual handlers live in plugin.ts processTaskWithTools()'s
// switch block; only the *shape* moves here.

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const WEB_SEARCH_TOOL: any = {
  type: "function" as const,
  function: {
    name: "web_search",
    description: "Search the web via DuckDuckGo's HTML endpoint. Routes through the egress-controlled proxy. Returns a parsed list of top results (title, url, snippet). Use for current events, news, or any query needing live information when foundry_web_search isn't available (GitHub Models slim mode).",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "The search query." },
        max_results: { type: "number", description: "Maximum results to return (default 8, max 20)." },
      },
      required: ["query"],
    },
  },
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const MEMORY_TOOL: any = {
  type: "function" as const,
  function: {
    name: "memory",
    description: "Persistent agent memory backed by a local JSON store at /sandbox/.openclaw/memory.json. Slim-mode replacement for foundry_memory. Use 'update' to record a fact, 'search' to retrieve relevant entries by substring match. Memory persists across tool calls within this sub-agent's container; it does NOT sync with peers.",
    parameters: {
      type: "object",
      properties: {
        operation: { type: "string", enum: ["search", "update", "list"], description: "Operation: 'update' to add a fact, 'search' to query by substring, 'list' to dump all entries." },
        text: { type: "string", description: "For 'update': the fact to remember. For 'search': substring to match. Ignored for 'list'." },
      },
      required: ["operation"],
    },
  },
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const TASK_TOOLS: any[] = [
  {
    type: "function" as const,
    function: {
      name: "exec_command",
      description: "Execute a shell command inside the sandbox and return stdout/stderr. Use for system info (uname, hostname, ip addr, cat /etc/os-release, etc.), file operations, or any command-line task. NOTE: Direct internet access (curl to external URLs) is blocked — use http_fetch for external HTTP requests. NOTE: The sandbox shell policy blocks redirection operators (`>`, `>>`, `<<`, `<<<`, pipes into writes). To save content to a file, use the `file_write` tool instead.",
      parameters: {
        type: "object",
        properties: {
          command: { type: "string", description: "The shell command to execute" },
        },
        required: ["command"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "file_write",
      description: "Write text content directly to a file inside the sandbox. Use this whenever you need to save an artifact LOCALLY (e.g. so foundry_code_execute can read it, or as the source for mesh_transfer_file). Path must be absolute and under /sandbox/ or /tmp/. Parent directories are created automatically. Overwrites existing files. **SIZE LIMIT — read carefully:** the `content` argument is part of the tool-call JSON arguments string; if it exceeds ~4 KB the LLM occasionally emits malformed escapes and the call fails to parse. For artifacts larger than ~4 KB use `foundry_code_execute` with `json.dump(data, open('/mnt/data/x.json','w'))` (or write/append the file in chunks) and then ship the resulting file via `mesh_transfer_file`. NEVER pass a multi-kilobyte JSON blob inline as a `mesh_send` payload — siblings expect FILES for anything over a few KB.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Absolute path to write (e.g. /sandbox/.openclaw/workspace/report.md)" },
          content: { type: "string", description: "File contents as UTF-8 text" },
        },
        required: ["path", "content"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "file_read",
      description: "Read text content from a file inside the sandbox. Path must be absolute and resolve under /sandbox/ or /tmp/. Use this to read artifacts delivered by other agents over the mesh (typically under /sandbox/.openclaw/workspace/incoming/) BEFORE embedding their content in a downstream tool call such as foundry_code_execute. Returns the file as UTF-8 text. Symlinks and paths that escape the sandbox via `..` are rejected. For binary files use foundry_code_execute with python (open(p,'rb')) — file_read decodes as UTF-8 and is intended for JSON, markdown, and other text artifacts.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Absolute path to read (e.g. /sandbox/.openclaw/workspace/incoming/analyst.json). Must resolve under /sandbox/ or /tmp/." },
          max_bytes: { type: "number", description: "Optional cap on bytes returned. Default 1048576 (1 MiB). Files larger than this are truncated and the truncation is noted in the response." },
        },
        required: ["path"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "http_fetch",
      description: "Make an HTTP request to an external URL through the security proxy. The request goes through blocklist checking and allowlist enforcement. Use this for any external API calls (Telegram, HackerNews, web APIs, etc.).",
      parameters: {
        type: "object",
        properties: {
          url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/...)" },
          method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
          headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
          body: { type: "string", description: "Optional request body (for POST/PUT)" },
        },
        required: ["url"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_web_search",
      description: "Search the web in real-time via Azure AI Foundry's Bing grounding. Returns answers with inline URL citations. Runs server-side — no egress policy exceptions needed. Use for current events, news, recent changes, verifying facts, or any query needing up-to-date information. WORKFLOW HINT for downstream delivery: once you've collected enough sources, ASSEMBLE the final structured artifact using `foundry_code_execute` (build a Python dict and `json.dump` it to `/mnt/data/<name>.json` — the wrapper downloads it to `/sandbox/.openclaw/workspace/<name>.json`). Then ship that file with `mesh_transfer_file`. DO NOT try to stuff multi-kilobyte JSON into a `mesh_send` payload or a `file_write` content arg — tool-call argument JSON corrupts at scale. Files only.",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string", description: "The search query or question to look up on the web." },
        },
        required: ["query"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_code_execute",
      description: "Execute Python code server-side via Azure AI Foundry's code_interpreter. Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, charts, complex math, and file processing. **Output files:** write artifacts to `/mnt/data/<name>` from inside the snippet — the wrapper auto-downloads them to `/sandbox/.openclaw/workspace/<name>` and returns the local paths in a `<downloaded_files>` JSON tail block. DO NOT shell out to copy files into `/sandbox/...` from inside Python — that path doesn't exist in the Foundry container.",
      parameters: {
        type: "object",
        properties: {
          code: { type: "string", description: "Python code to execute. To produce a downloadable artifact, write it to `/mnt/data/<filename>`." },
        },
        required: ["code"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_download_file",
      description: "Download a single file from a Foundry code_interpreter container by file_id + container_id and save it to /sandbox/.openclaw/workspace/. Use this only when foundry_code_execute did NOT auto-download a file you expected.",
      parameters: {
        type: "object",
        properties: {
          file_id: { type: "string", description: "Foundry container file id (e.g. cfile_abc123)." },
          container_id: { type: "string", description: "Foundry container id (e.g. cntr_abc123)." },
          local_basename: { type: "string", description: "Optional output filename (no slashes). Defaults to '<file_id>.bin'." },
        },
        required: ["file_id", "container_id"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_file_search",
      description: "Search uploaded documents and knowledge bases via Azure AI Foundry's file_search. Requires vector_store_ids — use foundry_memory instead for general memory/knowledge storage.",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string", description: "The search query." },
          vector_store_ids: { type: "array", items: { type: "string" }, description: "Vector store IDs to search (required)." },
        },
        required: ["query", "vector_store_ids"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_memory",
      description: "Persistent agent memory via Azure AI Foundry Memory Store. Store facts, preferences, and context that persists across sessions. Use 'search' to recall, 'update' to store new knowledge.",
      parameters: {
        type: "object",
        properties: {
          operation: { type: "string", enum: ["search", "update"], description: "Operation: 'search' to find relevant memories, 'update' to store new facts." },
          text: { type: "string", description: "For 'update': the fact to remember. For 'search': the query to find relevant memories." },
        },
        required: ["operation", "text"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "foundry_image_generation",
      description: "Generate images from text prompts via Azure AI Foundry (gpt-image-1). Returns file path of saved image.",
      parameters: {
        type: "object",
        properties: {
          prompt: { type: "string", description: "Text description of the image to generate." },
          quality: { type: "string", enum: ["low", "medium", "high"], description: "Image quality (default: medium)" },
          size: { type: "string", enum: ["1024x1024", "1024x1536", "1536x1024"], description: "Image dimensions (default: 1024x1024)" },
        },
        required: ["prompt"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "mesh_send",
      description: "Send a SHORT E2E encrypted TEXT/JSON message to another agent — siblings, parent, or any discovered agent. Auto-discovers the target by name. **SIZE LIMIT: keep `message` under ~2 KB.** This is for control-plane chatter: status pings, acknowledgements, small metadata, clarification questions, pointers (e.g. \"artifact.json ready, see mesh_transfer_file\"). For anything substantive (multi-source research JSON, full briefs, code, base64 anything, anything with embedded escaped JSON) build the artifact as a FILE with `foundry_code_execute` and ship it via `mesh_transfer_file`. Stuffing multi-kilobyte JSON into `message` is the #1 cause of `tool_failure` — the LLM corrupts its own tool-call arguments string and the call never reaches the wire.\n\nROUTING: `to_agent` MUST be the explicit downstream peer named in your task (e.g. `to_agent='writer'` when the task says \"hand to writer\" or describes a pipeline like analyst→viz→writer). Do NOT default to `to_agent='parent'` for sibling-bound text/JSON; parent will not forward to the named sibling and the sibling will time out waiting. Use `to_agent='parent'` only when the task explicitly says to return to parent/spawner, you are the FINAL agent in the pipeline, or no downstream peer is named.",
      parameters: {
        type: "object",
        properties: {
          to_agent: { type: "string", description: "Name of the target agent" },
          message: { type: "string", description: "The message body — text or stringified JSON. Do NOT hand-craft `{type:'file_transfer', file_data:'<base64-bytes>'}` here; use `mesh_transfer_file` for files." },
        },
        required: ["to_agent", "message"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "mesh_transfer_file",
      description: "Send a FILE / IMAGE / BINARY to another mesh agent. Reads the file from your local container, base64-encodes it, and ships it to the recipient via the chunked E2E encrypted transfer protocol (files up to ~30 MB). The recipient's gateway auto-saves it under /sandbox/.openclaw/workspace/incoming/<file_name> and the recipient sees the saved path in their mesh_inbox. Use this for any file you want to ship — never hand-craft a `file_transfer` JSON envelope through `mesh_send`.\n\nROUTING: `to_agent` MUST be the explicit downstream peer named in your task (e.g. `to_agent='writer'` when the task says \"hand to writer\" or describes a pipeline like analyst→viz→writer). Do NOT default to `to_agent='parent'` unless your task explicitly says to return to parent / spawner, or you are the FINAL agent in the pipeline. Sending an artifact to `parent` when the task names a sibling target is a routing bug: parent will not forward, and the named sibling will time out. Re-read your task description before each call to confirm the right target.",
      parameters: {
        type: "object",
        properties: {
          to_agent: { type: "string", description: "Name of the target agent" },
          file_path: { type: "string", description: "Path to the file to send (relative to /sandbox/.openclaw/workspace, or absolute under /sandbox)" },
          description: { type: "string", description: "Optional human-readable description for the recipient" },
        },
        required: ["to_agent", "file_path"],
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "discover",
      description: "Find agents in the mesh network. Returns names, trust scores, and online status. Useful to see who's available, but mesh_send auto-discovers — you don't need to call discover before sending.",
      parameters: {
        type: "object",
        properties: {
          pattern: { type: "string", description: "Glob pattern to filter agents (default: '*' for all)" },
        },
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "mesh_inbox",
      description: "Check for incoming messages from peer agents via the AGT E2E encrypted mesh relay. Returns peer messages received since boot. Internal protocol traffic (handoff/file_transfer ack) and the seeding task itself are filtered out — you only see real peer-to-peer payloads. file_transfer messages are auto-decoded: small text files inline; binaries surface `saved_to` (the local path your container has, written by the gateway) plus `file_name` and `size_bytes`. By default returns unread messages only and PEEKS without marking them read so concurrent sessions all see the same data; set mark_read=true to mark returned entries as read. **Server-side blocking:** when waiting for a peer to deliver something, set `block_until_message=true` (with `timeout_seconds`, default 120, max 300) — the tool sleeps server-side until the next non-internal message lands or the timeout expires. Use this instead of polling.",
      parameters: {
        type: "object",
        properties: {
          unread_only: { type: "boolean", description: "If true (default) return only entries not yet marked read." },
          mark_read: { type: "boolean", description: "If true mark returned entries as read so subsequent calls don't repeat them. Defaults to false (peek-only)." },
          limit: { type: "number", description: "Maximum number of entries to return (default 50, most recent kept)." },
          block_until_message: { type: "boolean", description: "If true and inbox is currently empty, block server-side until the next non-internal message arrives or `timeout_seconds` elapses. Default false." },
          timeout_seconds: { type: "number", description: "Max seconds to block when block_until_message=true. Default 120, capped at 300." },
        },
      },
    },
  },
  {
    type: "function" as const,
    function: {
      name: "mesh_await",
      description: "Block server-side until ALL listed sender agents have delivered ≥1 unread non-internal mesh message (or until timeout). Use this BEFORE assembly steps that depend on multiple sibling outputs — it costs zero LLM turns and resolves as soon as the last expected message lands. Returns matched message ids per sender + a `missing` list on partial timeout. The matched messages stay in the inbox; call mesh_inbox to read their contents (or pass mark_read=true here).",
      parameters: {
        type: "object",
        properties: {
          senders: { type: "array", items: { type: "string" }, description: "Agent names to wait for. The tool resolves once each named sender has at least one unread non-internal message in the inbox." },
          timeout_seconds: { type: "number", description: "Max seconds to block. Default 180, capped at 600." },
          mark_read: { type: "boolean", description: "If true, mark matched messages as read on resolve. Default false — leave unread for mesh_inbox to surface." },
        },
        required: ["senders"],
      },
    },
  },
];

/**
 * Returns the tool list visible to a sub-agent's LLM.
 *
 * In GH-token slim modes (`AZURECLAW_PROVIDER=github-models` or
 * `github-copilot`), Foundry-only tools (foundry_web_search,
 * foundry_code_execute, foundry_file_search, foundry_memory,
 * foundry_image_generation, foundry_download_file) are hidden because the
 * inference router has no Foundry endpoint to call — exposing them just
 * burns context with verbose JSON-schema and tempts the model to call tools
 * that will 404. A DuckDuckGo-backed `web_search` tool is appended so the
 * model still has live web access.
 *
 * **Strict mode** (`AZURECLAW_STRICT_TOOLS=1`): adds `strict: true` +
 * `additionalProperties: false` to tools whose schemas already satisfy the
 * OpenAI strict-mode constraints (all params required, no free-form objects).
 * This makes the model's outer tool-call arguments JSON grammar-constrained
 * at decode time, eliminating the multi-KB escape-corruption failures that
 * affect mesh_send, file_write, and foundry_code_execute. Disabled in slim
 * mode because not every upstream provider behind GitHub Models / Copilot
 * implements strict mode consistently. Default OFF; flip the env var to opt
 * in. Backward compatible — when the flag is unset the behaviour is
 * byte-identical to the pre-strict release.
 */
// Tools that go strict by simply adding the flag + `additionalProperties:false`
// (every property already in `required`, no free-form objects).
const STRICT_ELIGIBLE = new Set([
  "exec_command",
  "file_write",
  "foundry_web_search",
  "foundry_code_execute",
  "foundry_memory",
  "foundry_file_search",
  "mesh_send",
]);

// Tools whose base schema has optional params — strict mode requires every
// property in `required` and optionals expressed as nullable. The schemas
// below are functionally equivalent to the base schema (handlers already
// treat null and undefined identically via `args.x || default` and `typeof`
// guards) but reshaped to satisfy OpenAI strict-mode constraints.
//
// http_fetch is intentionally absent — its `headers` field is a free-form
// key-value object which strict mode forbids (every nested object must
// enumerate all properties + set additionalProperties:false). Refactoring
// headers to an enumerated allow-list would change semantics, so http_fetch
// stays non-strict for now.
//
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const STRICT_SCHEMA_OVERRIDES: Record<string, any> = {
  mesh_transfer_file: {
    type: "object",
    properties: {
      to_agent: { type: "string", description: "Name of the target agent" },
      file_path: { type: "string", description: "Path to the file to send (relative to /sandbox/.openclaw/workspace, or absolute under /sandbox)" },
      description: { type: ["string", "null"], description: "Optional human-readable description for the recipient (null if not needed)" },
    },
    required: ["to_agent", "file_path", "description"],
    additionalProperties: false,
  },
  mesh_inbox: {
    type: "object",
    properties: {
      unread_only: { type: ["boolean", "null"], description: "If true (default) return only entries not yet marked read. null = default true." },
      mark_read: { type: ["boolean", "null"], description: "If true mark returned entries as read. null = default false (peek-only)." },
      limit: { type: ["number", "null"], description: "Maximum number of entries to return. null = default 50." },
      block_until_message: { type: ["boolean", "null"], description: "If true block server-side until next non-internal message arrives. null = default false." },
      timeout_seconds: { type: ["number", "null"], description: "Max seconds to block when block_until_message=true. null = default 120, capped at 300." },
    },
    required: ["unread_only", "mark_read", "limit", "block_until_message", "timeout_seconds"],
    additionalProperties: false,
  },
  mesh_await: {
    type: "object",
    properties: {
      senders: { type: "array", items: { type: "string" }, description: "Agent names to wait for. The tool resolves once each named sender has at least one unread non-internal message in the inbox." },
      timeout_seconds: { type: ["number", "null"], description: "Max seconds to block. null = default 180, capped at 600." },
      mark_read: { type: ["boolean", "null"], description: "If true mark matched messages as read on resolve. null = default false." },
    },
    required: ["senders", "timeout_seconds", "mark_read"],
    additionalProperties: false,
  },
  discover: {
    type: "object",
    properties: {
      pattern: { type: ["string", "null"], description: "Glob pattern to filter agents. null = default '*' for all." },
    },
    required: ["pattern"],
    additionalProperties: false,
  },
  foundry_image_generation: {
    type: "object",
    properties: {
      prompt: { type: "string", description: "Text description of the image to generate." },
      quality: { type: ["string", "null"], enum: ["low", "medium", "high", null], description: "Image quality. null = default medium." },
      size: { type: ["string", "null"], enum: ["1024x1024", "1024x1536", "1536x1024", null], description: "Image dimensions. null = default 1024x1024." },
    },
    required: ["prompt", "quality", "size"],
    additionalProperties: false,
  },
  foundry_download_file: {
    type: "object",
    properties: {
      file_id: { type: "string", description: "Foundry container file id (e.g. cfile_abc123)." },
      container_id: { type: "string", description: "Foundry container id (e.g. cntr_abc123)." },
      local_basename: { type: ["string", "null"], description: "Optional output filename (no slashes). null = default '<file_id>.bin'." },
    },
    required: ["file_id", "container_id", "local_basename"],
    additionalProperties: false,
  },
  web_search: {
    type: "object",
    properties: {
      query: { type: "string", description: "The search query." },
      max_results: { type: ["number", "null"], description: "Maximum results to return. null = default 8, max 20." },
    },
    required: ["query", "max_results"],
    additionalProperties: false,
  },
  memory: {
    type: "object",
    properties: {
      operation: { type: "string", enum: ["search", "update", "list"], description: "Operation: 'update' to add a fact, 'search' to query by substring, 'list' to dump all entries." },
      text: { type: ["string", "null"], description: "For 'update': the fact to remember. For 'search': substring to match. null/ignored for 'list'." },
    },
    required: ["operation", "text"],
    additionalProperties: false,
  },
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function applyStrict(tool: any): any {
  const name = tool?.function?.name;
  if (!name) return tool;
  const override = STRICT_SCHEMA_OVERRIDES[name];
  if (override) {
    return {
      ...tool,
      function: {
        ...tool.function,
        strict: true,
        parameters: override,
      },
    };
  }
  if (!STRICT_ELIGIBLE.has(name)) return tool;
  const params = tool.function.parameters;
  if (!params || params.type !== "object") return tool;
  return {
    ...tool,
    function: {
      ...tool.function,
      strict: true,
      parameters: {
        ...params,
        additionalProperties: false,
      },
    },
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function getTaskTools(): any[] {
  const provider = process.env.AZURECLAW_PROVIDER;
  const slim = provider === "github-models" || provider === "github-copilot";
  // Strict mode is opt-in (env-flag) AND only applied on the Foundry path
  // because slim-mode providers vary in strict-schema support.
  // Additionally, only enable when the configured model is a known
  // OpenAI-family deployment that supports strict mode — Anthropic /
  // Claude / Gemini / Mistral models exposed via Foundry's OpenAI-compat
  // shim do NOT implement strict and the shim's behaviour is version-
  // dependent (silent ignore vs schema reject). Default to off for anything
  // outside the GPT family.
  const model = (process.env.AZURECLAW_MODEL || process.env.OPENCLAW_MODEL || process.env.OPENAI_MODEL || "").toLowerCase();
  const STRICT_MODEL_RE = /^(gpt-4o|gpt-4\.1|gpt-5|o1|o3|o4)/;
  const modelOk = STRICT_MODEL_RE.test(model);
  const strict = !slim && modelOk && process.env.AZURECLAW_STRICT_TOOLS === "1";
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const maybeStrict = (t: any) => (strict ? applyStrict(t) : t);

  if (slim) {
    const FOUNDRY = new Set([
      "foundry_web_search",
      "foundry_code_execute",
      "foundry_file_search",
      "foundry_memory",
      "foundry_image_generation",
      "foundry_download_file",
    ]);
    return [
      ...TASK_TOOLS.filter((t) => !FOUNDRY.has(t.function?.name)).map(maybeStrict),
      WEB_SEARCH_TOOL,
      MEMORY_TOOL,
    ];
  }
  return TASK_TOOLS.map(maybeStrict);
}
