// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Provider-agnostic registry abstraction for AgentMesh discovery.
//
// Two registries are wire-incompatible:
//
//   vendored agentmesh-registry — GET /registry/search?capability=X     → array of records
//                                  GET /registry/lookup?amid=X           → single record
//                                  POST /registry/heartbeat              → heartbeat keepalive
//
//   AGT registry (microsoft/agent-governance-toolkit)
//                                — GET /v1/discover?capability=X&limit=N → {results,total}
//                                  GET /v1/agents/{did}                  → single record
//                                  (no heartbeat — relay tracks liveness)
//
// Callers in the runtime hardcoded the vendored URLs. When the sandbox runs
// with AZURECLAW_MESH_PROVIDER=agt the inference router proxies HTTP to the
// AGT registry, which 404s those paths — agents go invisible.
//
// `IMeshRegistry` normalizes both wire shapes into a common envelope and
// `getMeshRegistry()` returns the right impl based on the env var. Callers
// always see the vendored field names (display_name, signing_public_key,
// last_seen, status) — the AGT impl synthesises them from the AGT record
// shape so we don't churn 8+ callsites for the wire diff alone.

import * as http from "node:http";

/**
 * Common normalized registry entry. Field names match the vendored wire
 * shape so existing callers continue to work without changes.
 *
 * Provider-specific extras:
 * - `did` — AGT canonical agent ID (mirrors `amid` for vendored).
 * - `reputation_score` — present from both registries.
 * - `metadata` — AGT exposes arbitrary metadata; vendored returns {} here.
 */
export interface RegistryEntry {
  amid: string;
  did?: string;
  display_name?: string;
  capabilities?: string[];
  last_seen?: string;
  status?: "online" | "offline" | string;
  signing_public_key?: string;
  public_info?: { signing_public_key?: string };
  reputation_score?: number;
  metadata?: Record<string, string>;
}

export interface SearchResponse {
  results: RegistryEntry[];
  total?: number;
}

export interface IMeshRegistry {
  /** Search by capability/display_name. */
  search(
    capability: string,
    opts?: { limit?: number; timeoutMs?: number },
  ): Promise<RegistryEntry[]>;
  /** Look up a single agent by AMID/DID. */
  lookup(
    amid: string,
    opts?: { timeoutMs?: number },
  ): Promise<RegistryEntry | null>;
  /** Optional heartbeat — vendored only. AGT relay does its own liveness. */
  heartbeat?(
    amid: string,
    capabilities: string[],
    opts?: { timeoutMs?: number },
  ): Promise<boolean>;
  /** Provider tag — useful for guard logic / diagnostics. */
  readonly provider: "vendored" | "agt";
}

// ── Shared HTTP helpers ──────────────────────────────────────────

function httpGetJson(
  url: string,
  timeoutMs: number,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.get(url, (res) => {
      let d = "";
      res.on("data", (c: Buffer) => {
        d += c.toString();
      });
      res.on("end", () =>
        resolve({ status: res.statusCode ?? 0, body: d }),
      );
    });
    req.on("error", reject);
    req.setTimeout(timeoutMs, () => {
      req.destroy();
      reject(new Error("timeout"));
    });
  });
}

function httpPostJson(
  url: string,
  payload: unknown,
  timeoutMs: number,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const data = JSON.stringify(payload);
    const u = new URL(url);
    const req = http.request(
      {
        method: "POST",
        hostname: u.hostname,
        port: u.port || 80,
        path: `${u.pathname}${u.search}`,
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(data).toString(),
        },
      },
      (res) => {
        let d = "";
        res.on("data", (c: Buffer) => {
          d += c.toString();
        });
        res.on("end", () =>
          resolve({ status: res.statusCode ?? 0, body: d }),
        );
      },
    );
    req.on("error", reject);
    req.setTimeout(timeoutMs, () => {
      req.destroy();
      reject(new Error("timeout"));
    });
    req.write(data);
    req.end();
  });
}

// ── Vendored registry impl ───────────────────────────────────────

class VendoredMeshRegistry implements IMeshRegistry {
  readonly provider = "vendored" as const;
  constructor(private readonly base: string) {}

  async search(
    capability: string,
    opts: { limit?: number; timeoutMs?: number } = {},
  ): Promise<RegistryEntry[]> {
    const timeoutMs = opts.timeoutMs ?? 5000;
    try {
      const { status, body } = await httpGetJson(
        `${this.base}/registry/search?capability=${encodeURIComponent(capability)}`,
        timeoutMs,
      );
      if (status < 200 || status >= 300) return [];
      const parsed = JSON.parse(body) as
        | RegistryEntry[]
        | { results?: RegistryEntry[] };
      const results = Array.isArray(parsed) ? parsed : (parsed.results ?? []);
      const limit = opts.limit;
      return limit ? results.slice(0, limit) : results;
    } catch {
      return [];
    }
  }

  async lookup(
    amid: string,
    opts: { timeoutMs?: number } = {},
  ): Promise<RegistryEntry | null> {
    const timeoutMs = opts.timeoutMs ?? 3000;
    try {
      const { status, body } = await httpGetJson(
        `${this.base}/registry/lookup?amid=${encodeURIComponent(amid)}`,
        timeoutMs,
      );
      if (status < 200 || status >= 300) return null;
      const parsed = JSON.parse(body) as RegistryEntry;
      return parsed && (parsed.amid || (parsed as { id?: string }).id)
        ? { ...parsed, amid: parsed.amid ?? (parsed as { id?: string }).id! }
        : null;
    } catch {
      return null;
    }
  }

  async heartbeat(
    amid: string,
    capabilities: string[],
    opts: { timeoutMs?: number } = {},
  ): Promise<boolean> {
    const timeoutMs = opts.timeoutMs ?? 3000;
    try {
      const { status } = await httpPostJson(
        `${this.base}/registry/heartbeat`,
        { amid, capabilities },
        timeoutMs,
      );
      return status >= 200 && status < 300;
    } catch {
      return false;
    }
  }
}

// ── AGT registry impl ────────────────────────────────────────────

// AGT registry record shape (snake_case from FastAPI).
interface AgtAgentRecord {
  did: string;
  capabilities?: string[];
  metadata?: Record<string, string>;
  reputation_score?: number;
  last_seen?: string;
  // Public X25519 identity key (base64url). Not directly usable as Ed25519
  // signing key but present in the record. Vendored field `signing_public_key`
  // is synthesised from metadata.signing_public_key if registered.
  public_key?: string;
}

interface AgtDiscoverResult {
  results: Array<{
    did: string;
    capabilities?: string[];
    reputation_score?: number;
    last_seen?: string;
  }>;
  total: number;
}

class AgtMeshRegistry implements IMeshRegistry {
  readonly provider = "agt" as const;
  constructor(private readonly base: string) {}

  /**
   * Map an AGT record to the runtime's expected normalized shape.
   * `display_name` is pulled from metadata.display_name first (set by
   * MeshClient.registerSelf at AGT-side), then falls back to the first
   * capability that doesn't look like a DID.
   */
  private mapAgent(r: AgtAgentRecord): RegistryEntry {
    const meta = r.metadata ?? {};
    const display =
      typeof meta.display_name === "string" && meta.display_name.length > 0
        ? meta.display_name
        : (r.capabilities ?? []).find((c) => !c.startsWith("did:"));
    // AGT doesn't expose Ed25519 signing keys in the public record. Sandbox
    // hosts that need signing-key verification register it under
    // metadata.signing_public_key. Empty string when absent — callers handle
    // missing-key fallback.
    const signing =
      typeof meta.signing_public_key === "string" ? meta.signing_public_key : "";
    return {
      amid: r.did,
      did: r.did,
      display_name: display,
      capabilities: r.capabilities,
      last_seen: r.last_seen,
      // AGT doesn't expose status; if last_seen is recent, treat as online.
      status: r.last_seen ? "online" : "offline",
      signing_public_key: signing || undefined,
      public_info: signing ? { signing_public_key: signing } : undefined,
      reputation_score: r.reputation_score,
      metadata: meta,
    };
  }

  async search(
    capability: string,
    opts: { limit?: number; timeoutMs?: number } = {},
  ): Promise<RegistryEntry[]> {
    const timeoutMs = opts.timeoutMs ?? 5000;
    const limit = opts.limit ?? 50;
    try {
      const { status, body } = await httpGetJson(
        `${this.base}/v1/discover?capability=${encodeURIComponent(capability)}&limit=${limit}`,
        timeoutMs,
      );
      if (status < 200 || status >= 300) return [];
      const parsed = JSON.parse(body) as AgtDiscoverResult;
      const results = parsed.results ?? [];
      // Discover only returns a subset of fields. For each hit, fetch the
      // full record to populate metadata.display_name and signing key. This
      // is the same overhead the vendored registry imposes (its /search
      // already returns full records) so net cost is comparable.
      const full = await Promise.all(
        results.map(async (hit) => {
          const rec = await this.lookup(hit.did, { timeoutMs });
          if (rec) return rec;
          // Fall back to the discover hit alone — better than dropping it.
          return this.mapAgent({
            did: hit.did,
            capabilities: hit.capabilities,
            reputation_score: hit.reputation_score,
            last_seen: hit.last_seen,
          });
        }),
      );
      return full;
    } catch {
      return [];
    }
  }

  async lookup(
    amid: string,
    opts: { timeoutMs?: number } = {},
  ): Promise<RegistryEntry | null> {
    const timeoutMs = opts.timeoutMs ?? 3000;
    try {
      const { status, body } = await httpGetJson(
        `${this.base}/v1/agents/${encodeURIComponent(amid)}`,
        timeoutMs,
      );
      if (status < 200 || status >= 300) return null;
      const parsed = JSON.parse(body) as AgtAgentRecord;
      if (!parsed || !parsed.did) return null;
      return this.mapAgent(parsed);
    } catch {
      return null;
    }
  }

  // AGT registry has no heartbeat endpoint — the relay's WebSocket
  // connection IS the liveness signal. Return true so callers don't
  // treat absence as a failure.
  async heartbeat(): Promise<boolean> {
    return true;
  }
}

// ── Factory + singleton ──────────────────────────────────────────

let cached: { provider: string; base: string; impl: IMeshRegistry } | null = null;

/**
 * Resolve the active mesh registry based on AZURECLAW_MESH_PROVIDER. Pass
 * a `routerUrl` helper so this module avoids a circular dep on plugin.ts.
 *
 * Cached per (provider, base) pair — flipping AZURECLAW_MESH_PROVIDER at
 * runtime (tests do this) invalidates the cache.
 */
export function getMeshRegistry(
  routerUrl: (path: string) => string,
): IMeshRegistry {
  const provider = (process.env.AZURECLAW_MESH_PROVIDER || "vendored")
    .trim()
    .toLowerCase();
  // Runtime (UID 1000) is iptables-confined to localhost. AGT_REGISTRY_URL
  // is set by the sandbox launcher as the router's UPSTREAM target — it
  // points at the real registry which the runtime cannot reach directly
  // (ECONNREFUSED, then silent empty results from the catch-all). Always
  // route through the local inference-router proxy.
  const base = routerUrl("/agt/registry").replace(/\/+$/, "");
  if (cached && cached.provider === provider && cached.base === base) {
    return cached.impl;
  }
  const impl: IMeshRegistry =
    provider === "agt"
      ? new AgtMeshRegistry(base)
      : new VendoredMeshRegistry(base);
  cached = { provider, base, impl };
  return impl;
}

/** Test-only: clear the cached registry between tests. */
export function __resetMeshRegistryForTesting(): void {
  cached = null;
}
