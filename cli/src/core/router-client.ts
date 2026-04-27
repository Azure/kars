/**
 * Router-client helpers — small, pure I/O wrappers around the in-pod
 * inference-router. Extracted from `plugin.ts` to satisfy the LOC budget
 * (`docs/implementation-plan.md` §4.2) and to give the conformance corpus a
 * stable seam to mock against.
 *
 * Intentionally narrow:
 *   - `routerBase` / `routerWsBase` / `routerUrl` / `routerWsUrl` —
 *     the documented router base URL plumbing (env-overridable for tests).
 *   - `routerCall` / `routerCallStrict` — generic request helpers used across
 *     the plugin (handoff, memory sync, registry verify, mesh send, ...).
 *   - `readAdminToken` / `readAdminTokenSync` — read the admin bearer token
 *     from the documented mount points. Used to authenticate sandbox→router
 *     mutations that must not be forgeable from a sub-agent's PID space.
 *   - `pushTrustToRouter` / `pushSigningCounter` — best-effort metric pushes
 *     to the router's AGT endpoints. Failures are swallowed by design (the
 *     router is the source of truth; this is just a hot-path nudge).
 *
 * No behaviour change vs. the in-plugin originals; identifiers are exported
 * without the leading underscore. `plugin.ts` re-exports the URL helpers so
 * the public surface (`./plugin.js` imports in tests) is unchanged.
 */

import { createRequire as __createRequire__ } from "node:module";

// CommonJS interop shim — mirrors the one in plugin.ts. Required because
// `readAdminTokenSync` cannot use `await import()`.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const require: NodeRequire = (() => {
  try {
    return __createRequire__(import.meta.url);
  } catch {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return ((id: string) => (globalThis as any).require(id)) as NodeRequire;
  }
})();

const DEFAULT_ROUTER_BASE = "http://127.0.0.1:8443";

export function routerBase(): string {
  return process.env.AZURECLAW_ROUTER_URL || DEFAULT_ROUTER_BASE;
}

export function routerWsBase(): string {
  // http → ws, https → wss; preserves host:port and trailing path if any.
  return routerBase().replace(/^http/, "ws");
}

export function routerUrl(path: string): string {
  return new URL(path, routerBase()).toString();
}

export function routerWsUrl(path: string): string {
  return new URL(path, routerWsBase()).toString();
}

const ADMIN_TOKEN_PATHS = [
  "/tmp/.agt-admin-token",
  "/etc/azureclaw/secrets/admin-token",
  "/run/secrets/admin-token",
] as const;

export async function routerCall(
  method: string,
  path: string,
  body?: unknown,
  timeoutMs = 15000,
  extraHeaders?: Record<string, string>,
): Promise<any> {
  const http = await import("node:http");
  const url = new URL(path, routerBase());
  return new Promise((resolve, reject) => {
    const opts: any = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      method,
      headers: { "x-azureclaw-sandbox": "self", ...extraHeaders } as Record<string, string>,
    };
    if (body) {
      opts.headers["content-type"] = "application/json";
    }
    const req = http.request(opts, (res: any) => {
      let data = "";
      res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
      res.on("end", () => {
        try { resolve(JSON.parse(data)); } catch { resolve(data); }
      });
    });
    req.on("error", reject);
    req.setTimeout(timeoutMs, () => { req.destroy(); reject(new Error("timeout")); });
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

// Strict variant that rejects on HTTP >= 400 — used by handoff orchestration
// where a non-2xx is a real failure, not a soft signal we can ignore.
export async function routerCallStrict(
  method: string,
  path: string,
  body?: unknown,
  timeoutMs = 15000,
  extraHeaders?: Record<string, string>,
): Promise<any> {
  const http = await import("node:http");
  const url = new URL(path, routerBase());
  return new Promise((resolve, reject) => {
    const opts: any = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      method,
      headers: { "x-azureclaw-sandbox": "self", ...extraHeaders } as Record<string, string>,
    };
    if (body) {
      opts.headers["content-type"] = "application/json";
    }
    const req = http.request(opts, (res: any) => {
      let data = "";
      res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
      res.on("end", () => {
        if (res.statusCode && res.statusCode >= 400) {
          reject(new Error(`HTTP ${res.statusCode}: ${data.slice(0, 500)}`));
          return;
        }
        try { resolve(JSON.parse(data)); } catch { resolve(data); }
      });
    });
    req.on("error", reject);
    req.setTimeout(timeoutMs, () => { req.destroy(); reject(new Error("timeout")); });
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

// Read admin token from the filesystem (used by handoff orchestration).
export async function readAdminToken(): Promise<string> {
  const fs = await import("node:fs");
  for (const p of ADMIN_TOKEN_PATHS) {
    try { const t = fs.readFileSync(p, "utf-8").trim(); if (t) return t; } catch { /* skip */ }
  }
  return process.env.ADMIN_TOKEN || "";
}

// Synchronous variant for use inside synchronous helpers. Cached after first
// hit so we only stat the filesystem once per process. Returns "" if no token
// is available.
let _cachedAdminToken: string | null = null;
export function readAdminTokenSync(): string {
  if (_cachedAdminToken !== null) return _cachedAdminToken;
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const fsSync = require("node:fs") as typeof import("node:fs");
  for (const p of ADMIN_TOKEN_PATHS) {
    try { const t = fsSync.readFileSync(p, "utf-8").trim(); if (t) { _cachedAdminToken = t; return t; } } catch { /* skip */ }
  }
  _cachedAdminToken = process.env.ADMIN_TOKEN || "";
  return _cachedAdminToken;
}

// Push a trust-score delta to the router's AGT trust endpoint. Best-effort.
export async function pushTrustToRouter(agentId: string, scoreDelta: number): Promise<void> {
  try {
    const http = await import("node:http");
    const fs = await import("node:fs");
    const body = JSON.stringify({
      agent_id: agentId,
      score: Math.round(500 + scoreDelta * 500), // 0.0-1.0 → 0-1000 scale
      interactions: 1,
    });
    // Read admin token for trust mutation auth (prevents sandbox from forging scores)
    let adminToken = "";
    for (const p of ADMIN_TOKEN_PATHS) {
      if (adminToken) break;
      try { adminToken = fs.readFileSync(p, "utf-8").trim(); } catch { /* skip */ }
    }
    const headers: Record<string, string | number> = {
      "Content-Type": "application/json",
      "Content-Length": Buffer.byteLength(body),
    };
    if (adminToken) headers["Authorization"] = `Bearer ${adminToken}`;
    await new Promise<void>((resolve, reject) => {
      const req = http.request(routerUrl("/agt/trust"), {
        method: "POST",
        headers,
        timeout: 5000,
      }, (res: any) => {
        res.resume();
        res.on("end", () => {
          if (res.statusCode && res.statusCode >= 400) {
            reject(new Error(`HTTP ${res.statusCode}`));
          } else {
            resolve();
          }
        });
      });
      req.on("error", reject);
      req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
      req.write(body);
      req.end();
    });
  } catch {
    // Startup race: router may not be ready on first plugin load (double-load pattern).
    // Trust will be seeded on the second load — no need to alarm the operator.
  }
}

// Push Ed25519 signing counter to router for /agt/status metrics. Best-effort.
export async function pushSigningCounter(action: "signed" | "verified" | "rejected"): Promise<void> {
  try {
    const http = await import("node:http");
    const body = JSON.stringify({ action });
    await new Promise<void>((resolve) => {
      const req = http.request(
        { hostname: "127.0.0.1", port: 8443, path: "/agt/signing-counter", method: "POST",
          headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
        () => resolve(),
      );
      req.on("error", () => resolve());
      req.setTimeout(1000, () => { req.destroy(); resolve(); });
      req.write(body);
      req.end();
    });
  } catch { /* best effort */ }
}
