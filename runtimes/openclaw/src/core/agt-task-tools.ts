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
      description: "Write text content directly to a file inside the sandbox. Use this (NOT exec_command with shell redirection) whenever you need to save an artifact — it bypasses the shell redirect policy. Path must be absolute and under /sandbox/ or /tmp/. Parent directories are created automatically. Overwrites existing files.",
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
      description: "Search the web in real-time via Azure AI Foundry's Bing grounding. Returns answers with inline URL citations. Runs server-side — no egress policy exceptions needed. Use for current events, news, recent changes, verifying facts, or any query needing up-to-date information.",
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
      description: "Execute Python code server-side via Azure AI Foundry's code_interpreter. Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, charts, complex math, and file processing.",
      parameters: {
        type: "object",
        properties: {
          code: { type: "string", description: "Python code to execute." },
        },
        required: ["code"],
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
      description: "Send an E2E encrypted TEXT/JSON message to any agent in the mesh — siblings, parent, or any discovered agent. Auto-discovers the target by name. Use for peer-to-peer text or JSON. To send a FILE / IMAGE / BINARY, use `mesh_transfer_file` instead — peer agents run in separate containers and cannot read your /sandbox or /tmp paths, so a file_transfer envelope must contain real base64 bytes. Plain JSON metadata is accepted; envelopes with placeholder file_data (e.g. `<base64-image-data>`) are rejected.",
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
      description: "Send a FILE / IMAGE / BINARY to another mesh agent. Reads the file from your local container, base64-encodes it, and ships it to the recipient via the chunked E2E encrypted transfer protocol (files up to ~30 MB). The recipient's gateway auto-saves it under /sandbox/.openclaw/workspace/incoming/<file_name> and the recipient sees the saved path in their mesh_inbox. Use this for any file you want to ship — never hand-craft a `file_transfer` JSON envelope through `mesh_send`.",
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
      description: "Check for incoming messages from peer agents via the AGT E2E encrypted mesh relay. Returns peer messages received since boot. Internal protocol traffic (handoff/file_transfer ack) and the seeding task itself are filtered out — you only see real peer-to-peer payloads. file_transfer messages are auto-decoded: small text files inline; binaries surface `saved_to` (the local path your container has, written by the gateway) plus `file_name` and `size_bytes`. By default returns unread messages only and PEEKS without marking them read so concurrent sessions all see the same data; set mark_read=true to mark returned entries as read.",
      parameters: {
        type: "object",
        properties: {
          unread_only: { type: "boolean", description: "If true (default) return only entries not yet marked read." },
          mark_read: { type: "boolean", description: "If true mark returned entries as read so subsequent calls don't repeat them. Defaults to false (peek-only)." },
          limit: { type: "number", description: "Maximum number of entries to return (default 50, most recent kept)." },
        },
      },
    },
  },
];
