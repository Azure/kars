// kars Bridge — curated catalog of popular MCP servers, so an operator can add a
// connected service in one click instead of hand-authoring a McpServer spec.
//
// Each entry pre-fills the friendly add form (name + endpoint URL + allowed
// tools). URLs are the vendors' documented hosted MCP endpoints where one exists
// (the operator confirms/edits before creating); self-hosted reference servers
// carry a placeholder URL + a docs link so the operator points it at their own
// deployment. Nothing is created until the operator reviews and submits.

export type McpHosting = "hosted" | "managed" | "external";

export interface McpCatalogEntry {
  id: string;
  name: string; // default McpServer name (editable)
  label: string; // human title
  category: "Dev & code" | "Productivity" | "Data" | "Web & search" | "Automation" | "Observability" | "Payments & CRM";
  icon: string;
  description: string;
  /** Documented hosted endpoint. Empty for managed/external entries. */
  url: string;
  hosting: McpHosting;
  /** Controller-owned workload recipe for `hosting: managed`. */
  managedPreset?: "playwright" | "everything";
  /** Whether the vendor endpoint requires OAuth (→ production mode). */
  oauth: boolean;
  /** Sensible default allowed tools (`["*"]` = all, governed further by ToolPolicy). */
  allowedTools: string[];
  /** Router env var holding outbound bearer credentials for hosted servers. */
  bearerFromEnv?: string;
  /** Link to the server's docs so the operator can confirm the endpoint/auth. */
  docs: string;
}

export const MCP_CATALOG: McpCatalogEntry[] = [
  {
    id: "github",
    name: "github",
    label: "GitHub",
    category: "Dev & code",
    icon: "🐙",
    description: "Repositories, issues, pull requests, Actions, and code search across your GitHub org.",
    url: "https://api.githubcopilot.com/mcp/",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    bearerFromEnv: "COPILOT_GITHUB_TOKEN",
    docs: "https://github.com/github/github-mcp-server",
  },
  {
    id: "deepwiki",
    name: "deepwiki",
    label: "DeepWiki (public repository research)",
    category: "Dev & code",
    icon: "DW",
    description:
      "No-auth research and Q&A for public GitHub repositories through DeepWiki's official hosted Streamable HTTP endpoint.",
    url: "https://mcp.deepwiki.com/mcp",
    hosting: "hosted",
    oauth: false,
    allowedTools: ["read_wiki_structure", "read_wiki_contents", "ask_question"],
    docs: "https://docs.devin.ai/work-with-devin/deepwiki-mcp",
  },
  {
    id: "sentry",
    name: "sentry",
    label: "Sentry",
    category: "Observability",
    icon: "🛑",
    description: "Query issues, events, and stack traces from your Sentry projects.",
    url: "https://mcp.sentry.dev/mcp",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    docs: "https://docs.sentry.io/product/sentry-mcp/",
  },
  {
    id: "linear",
    name: "linear",
    label: "Linear",
    category: "Productivity",
    icon: "📐",
    description: "Read and manage Linear issues, projects, and cycles.",
    url: "https://mcp.linear.app/sse",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    docs: "https://linear.app/docs/mcp",
  },
  {
    id: "notion",
    name: "notion",
    label: "Notion",
    category: "Productivity",
    icon: "📝",
    description: "Search and read Notion pages and databases.",
    url: "https://mcp.notion.com/mcp",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    docs: "https://developers.notion.com/docs/mcp",
  },
  {
    id: "stripe",
    name: "stripe",
    label: "Stripe",
    category: "Payments & CRM",
    icon: "💳",
    description: "Query customers, charges, invoices, and products in Stripe (read-scoped by default).",
    url: "https://mcp.stripe.com",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    docs: "https://docs.stripe.com/mcp",
  },
  {
    id: "atlassian",
    name: "atlassian",
    label: "Jira & Confluence",
    category: "Productivity",
    icon: "🔷",
    description: "Atlassian Jira issues and Confluence pages.",
    url: "https://mcp.atlassian.com/v1/sse",
    hosting: "hosted",
    oauth: true,
    allowedTools: ["*"],
    docs: "https://www.atlassian.com/platform/remote-mcp-server",
  },
  {
    id: "brave-search",
    name: "brave-search",
    label: "Brave Search",
    category: "Web & search",
    icon: "🦁",
    description: "Web and local search via the Brave Search API. Needs a BRAVE_API_KEY credential.",
    url: "",
    hosting: "external",
    oauth: false,
    allowedTools: ["brave_web_search", "brave_local_search"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/brave-search",
  },
  {
    id: "fetch",
    name: "fetch",
    label: "Fetch (web content)",
    category: "Web & search",
    icon: "🌐",
    description: "Fetch a URL and return its content as markdown for the agent to read.",
    url: "",
    hosting: "external",
    oauth: false,
    allowedTools: ["fetch"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
  },
  {
    id: "filesystem",
    name: "filesystem",
    label: "Filesystem",
    category: "Dev & code",
    icon: "📁",
    description: "Read/write files within an allow-listed directory the server exposes.",
    url: "",
    hosting: "external",
    oauth: false,
    allowedTools: ["read_file", "list_directory", "search_files"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem",
  },
  {
    id: "postgres",
    name: "postgres",
    label: "PostgreSQL",
    category: "Data",
    icon: "🐘",
    description: "Run read-only SQL queries and inspect schema against a Postgres database.",
    url: "",
    hosting: "external",
    oauth: false,
    allowedTools: ["query"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/postgres",
  },
  {
    id: "slack",
    name: "slack",
    label: "Slack",
    category: "Productivity",
    icon: "💬",
    description: "Read channels and post messages in a Slack workspace.",
    url: "",
    hosting: "external",
    oauth: false,
    allowedTools: ["list_channels", "post_message", "get_channel_history"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/slack",
  },
  {
    id: "playwright",
    name: "playwright",
    label: "Playwright (browser)",
    category: "Automation",
    icon: "🎭",
    description: "Drive a headless browser: navigate, click, extract, and screenshot pages.",
    url: "",
    hosting: "managed",
    managedPreset: "playwright",
    oauth: false,
    allowedTools: ["*"],
    docs: "https://github.com/microsoft/playwright-mcp",
  },
  {
    id: "everything",
    name: "everything",
    label: "MCP Everything (verification utility)",
    category: "Automation",
    icon: "🧰",
    description:
      "Deterministic reference server for protocol, sampling, resource, prompt, and utility-tool verification.",
    url: "",
    hosting: "managed",
    managedPreset: "everything",
    oauth: false,
    allowedTools: ["*"],
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/everything",
  },
];

export const MCP_CATEGORIES = [
  "Dev & code",
  "Productivity",
  "Data",
  "Web & search",
  "Automation",
  "Observability",
  "Payments & CRM",
] as const;
