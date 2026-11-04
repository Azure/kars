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
      "Make an HTTP request to an external URL. The request is routed through the kars security proxy which enforces blocklist (51K+ malicious domains blocked), allowlist, and learn mode. Use this for ANY external API call (Telegram, HackerNews, web APIs, etc.). Direct internet access via curl/fetch is blocked by the egress guard.",
    parameters: {
      type: "object",
      properties: {
        url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/bot.../getMe)" },
        method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
        headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
        body: { type: "string", description: "Optional request body (for POST/PUT)" },
        wait_for_approval_ms: { type: "integer", minimum: 0, maximum: 300000, description: "Wait for installed egress policy before dispatch; queue approval alone is not permission. Default 0 returns denial and request details immediately." },
      },
      required: ["url"],
    },
    async execute(_id: string, params: Record<string, unknown>, signal?: AbortSignal) {
      try {
        const wait = params.wait_for_approval_ms ?? 0;
        if (!Number.isSafeInteger(wait) || (wait as number) < 0 || (wait as number) > 300000) throw new Error("wait_for_approval_ms must be 0..300000");
        const scope = wait ? await routerCall("GET", "/v1/access-requests", undefined, 15000, undefined, signal) : undefined;
        if (wait && typeof scope?.scope?.id !== "string") throw new Error("Router scope unavailable");
        const result = await routerCall("POST", "/egress/fetch", {
          url: params.url,
          method: (params.method as string) || "GET",
          headers: params.headers || {},
          body: params.body || undefined,
          ...(wait ? { wait_for_approval_ms: wait, scope_id: scope.scope.id } : {}),
        }, (wait as number) + 30000, undefined, signal, { maxBytes: 16 * 1024 * 1024, structuredErrors: true });
        return { content: [{ type: "text", text: safeJson(result) }], isError: Boolean(result?.error) };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Fetch failed: ${e.message}` }], isError: true };
      }
    },
  });
}
