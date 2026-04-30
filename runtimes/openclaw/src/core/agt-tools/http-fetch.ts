// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// http_fetch AGT tool registration — extracted from plugin.ts in S15.f.8.

import { routerCall } from "../router-client.js";
import { safeJson } from "../safe-json.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyApi = any;

export function registerHttpFetchTool(api: AnyApi): void {
  api.registerTool({
    name: "http_fetch",
    label: "HTTP Fetch (Egress Proxy)",
    description:
      "Make an HTTP request to an external URL. The request is routed through the AzureClaw security proxy which enforces blocklist (51K+ malicious domains blocked), allowlist, and learn mode. Use this for ANY external API call (Telegram, HackerNews, web APIs, etc.). Direct internet access via curl/fetch is blocked by the egress guard.",
    parameters: {
      type: "object",
      properties: {
        url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/bot.../getMe)" },
        method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
        headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
        body: { type: "string", description: "Optional request body (for POST/PUT)" },
      },
      required: ["url"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const result = await routerCall("POST", "/egress/fetch", {
          url: params.url,
          method: (params.method as string) || "GET",
          headers: params.headers || {},
          body: params.body || undefined,
        });
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Fetch failed: ${e.message}` }] };
      }
    },
  });
}
