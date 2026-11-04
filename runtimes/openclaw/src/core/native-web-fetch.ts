// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { NativeMissionApi } from "./native-mission-api.js";

export const NATIVE_WEB_PROVIDER = "kars-router";
const object = (value: unknown): Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value)
  ? value as Record<string, unknown> : {};

export interface NativeFetchAuthority {
  signal: AbortSignal;
  assertCurrent: () => void;
  telemetry?: { scope_id: string; call_id: string };
}

type Fetch = (args: Record<string, unknown>, signal: AbortSignal, native: boolean) => Promise<Record<string, unknown>>;
type Claim = (args: Record<string, unknown>, signal?: AbortSignal) => NativeFetchAuthority;

/** Public pinned-host extractors; never import private OpenClaw implementation paths. */
async function extract(body: string, contentType: string, extractMode: "text" | "markdown") {
  const sdkPath = "openclaw/plugin-sdk/web-content-extractor";
  const sdk = await import(sdkPath) as {
    extractBasicHtmlContent: (params: { html: string; extractMode: "text" | "markdown" }) => Promise<{ text: string; title?: string } | null>;
    markdownToText: (text: string) => string;
    stripInvisibleUnicode: (text: string) => string;
  };
  if (contentType === "text/html" || contentType === "application/xhtml+xml") {
    const result = await sdk.extractBasicHtmlContent({ html: body, extractMode });
    if (!result) throw new Error("No readable content in the router response");
    return { ...result, extractor: "kars-router-html" };
  }
  if (contentType && !contentType.startsWith("text/") && contentType !== "application/json"
    && !contentType.endsWith("+json") && contentType !== "application/xml" && !contentType.endsWith("+xml")) {
    throw new Error(`Native web_fetch cannot extract ${contentType}; use an appropriate download tool`);
  }
  const text = extractMode === "text" && contentType === "text/markdown" ? sdk.markdownToText(body) : body;
  return { text: sdk.stripInvisibleUnicode(text), extractor: "kars-router-text" };
}

function validateUrl(value: unknown): URL {
  if (typeof value !== "string") throw new Error("Native web fetch requires a URL");
  const url = new URL(value);
  if (url.protocol !== "https:" || url.username || url.password) throw new Error("Native web fetch requires credential-free HTTPS");
  return url;
}

export function registerNativeWebFetch(api: NativeMissionApi, claim: Claim, fetch: Fetch): void {
  api.registerWebFetchProvider({
    id: NATIVE_WEB_PROVIDER, label: "Kars router", hint: "Governed native web retrieval through the sandbox router",
    envVars: [], placeholder: "No credential required", signupUrl: "https://github.com/Azure/kars",
    credentialPath: `plugins.entries.${api.id}.config`, requiresCredential: false,
    getCredentialValue: () => undefined,
    setCredentialValue: () => { throw new Error("Kars router web fetch does not accept agent credentials"); },
    createTool: () => {
      return {
        description: "Fetch readable public content through the Kars router; native OpenClaw wraps and spills the result.",
        parameters: { type: "object", properties: { url: { type: "string" }, extractMode: { type: "string", enum: ["markdown", "text"] },
          maxChars: { type: "number" } }, required: ["url"] },
        execute: async (args, context) => {
          const authority = claim(args, context?.signal);
          let url = validateUrl(args.url);
          const extractMode = args.extractMode === "text" ? "text" : "markdown";
          const seen = new Set<string>();
          const started = performance.now();
          for (let redirects = 0; ; redirects++) {
            authority.assertCurrent();
            if (seen.has(url.href)) throw new Error("Native web fetch redirect loop");
            seen.add(url.href);
            // The router validates every redirect destination; no cookies, auth or Host override is forwarded.
            const response = await fetch({ url: url.href, method: "GET", ...(authority.telemetry ? { telemetry: authority.telemetry } : {}), headers: {
              Accept: "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.1",
            } }, authority.signal, true);
            authority.assertCurrent();
            const status = response.status as number;
            const headers = Object.fromEntries(Object.entries(object(response.headers)).map(([key, value]) => [key.toLowerCase(), value]));
            if ([301, 302, 303, 307, 308].includes(status) && typeof headers.location === "string") {
              if (redirects >= 3) throw new Error("Native web fetch exceeded three redirects");
              url = validateUrl(new URL(headers.location, url).href);
              continue;
            }
            const contentType = typeof headers["content-type"] === "string" ? headers["content-type"].split(";")[0].trim().toLowerCase() : "";
            const content = await extract(response.body as string, contentType, extractMode);
            authority.assertCurrent();
            return { ...content, finalUrl: url.href, status, contentType, truncated: response.truncated,
              ...(response.truncated ? { warning: "The router truncated the upstream response at its byte limit; this is incomplete source content." } : {}),
              rawLength: content.text.length, fetchedAt: new Date().toISOString(), tookMs: performance.now() - started };
          }
        },
      };
    },
  });
}
