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
      description: "Send an E2E encrypted message to any agent in the mesh — siblings, parent, or any discovered agent. Auto-discovers the target by name (no need to call discover first). Use for peer-to-peer communication between agents.",
      parameters: {
        type: "object",
        properties: {
          to_agent: { type: "string", description: "Name of the target agent" },
          message: { type: "string", description: "The message to send" },
        },
        required: ["to_agent", "message"],
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
      description: "Check for incoming messages from other agents via the AGT E2E encrypted mesh relay. Returns pending messages.",
      parameters: {
        type: "object",
        properties: {},
      },
    },
  },
];
