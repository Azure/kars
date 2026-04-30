// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.b: OAuth callback server + log/HTML escapers
// extracted from mesh.ts. Public surface preserved (re-exported
// from mesh.ts) for any test or external use.

import * as http from "node:http";

export interface OAuthResult {
  success: boolean;
  amid: string;
  provider: string;
  verified_identity?: {
    provider: string;
    provider_id: string;
    email?: string;
    username?: string;
    display_name?: string;
  };
  certificate?: string;
  error?: string;
}

// HTML-escape user-controlled strings before embedding in HTML responses
// (CWE-79: reflected-xss). Minimal escaper for untrusted text content.
export function escapeHtml(s: unknown): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// Strip CR/LF from untrusted data before logging so attackers can't forge
// log lines (CWE-117: log-injection). Classic pattern recognized by CodeQL.
export function sanitizeForLog(s: unknown): string {
  return String(s ?? "")
    .replace(/\r/g, "")
    .replace(/\n/g, " ")
    .replace(/\t/g, " ");
}

export async function waitForOAuthCallback(
  port: number,
  timeoutMs: number = 300_000,
): Promise<OAuthResult> {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const url = new URL(req.url ?? "/", `http://localhost:${port}`);

      if (url.pathname === "/callback") {
        // The registry redirects here with the verification result as query params
        const resultJson = url.searchParams.get("result");
        if (resultJson) {
          try {
            const result = JSON.parse(
              Buffer.from(resultJson, "base64").toString("utf-8"),
            ) as OAuthResult;

            // Return a nice HTML page — escape user-controlled fields to
            // prevent reflected XSS (result comes from the registry redirect).
            res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
            res.end(`
              <html><body style="font-family: system-ui; text-align: center; padding-top: 80px;">
                <h2>${result.success ? "✅ Authenticated!" : "❌ Authentication failed"}</h2>
                <p>${result.success ? "You can close this tab and return to the terminal." : escapeHtml(result.error ?? "Unknown error")}</p>
              </body></html>
            `);

            server.close();
            resolve(result);
          } catch {
            res.writeHead(400, { "Content-Type": "text/plain" });
            res.end("Invalid callback data");
          }
        } else {
          res.writeHead(400, { "Content-Type": "text/plain" });
          res.end("Missing result parameter");
        }
      } else {
        res.writeHead(404);
        res.end();
      }
    });

    server.listen(port, "127.0.0.1");

    const timer = setTimeout(() => {
      server.close();
      reject(new Error("OAuth callback timed out after 5 minutes"));
    }, timeoutMs);

    server.on("close", () => clearTimeout(timer));
  });
}
