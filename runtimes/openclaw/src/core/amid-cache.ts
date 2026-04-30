// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// AMID cache + registry resolvers — extracted from plugin.ts in S15.f.1
// to give plugin.ts headroom under the §4.2 800-LOC cap.
//
// **Module-level state.** The `amidToName`, `nameToAmid`,
// `nameToAmidTs`, `parentTrustedAmids`, and `peerSigningKeys` maps are
// shared singletons used across the plugin lifecycle (handoff, mesh
// send, KNOCK auth, sub-agent restore). They MUST be exported as live
// references — code in plugin.ts mutates them directly. Do not switch
// to encapsulating functions without auditing every call-site.

// AMID → agent name mapping (populated during send via registry search)
export const amidToName: Map<string, string> = new Map();
export const nameToAmid: Map<string, string> = new Map();
// Per-name cache age for nameToAmid. Used to invalidate stale entries when a
// sub-agent crashes and re-registers with a fresh AMID (sandbox identities are
// ephemeral by design — keys regenerate on every pod boot, so an AMID cached
// before a restart points at a dead identity in the registry).
export const nameToAmidTs: Map<string, number> = new Map();
// 60s — long enough to amortize registry lookups across normal sends, short
// enough that a crashed-and-restarted sub-agent is re-resolved within one
// follow-up send cycle. The stable 'parent' alias bypasses TTL (see
// getCachedAmid below) because it's seeded by the operator at spawn and never
// changes for the lifetime of this pod.
export const AMID_CACHE_TTL_MS = 60_000;

// Parent-verified trusted peer AMIDs — pre-seeded at spawn via
// AGT_TRUSTED_PEERS env var. Separate from amidToName to prevent trust
// escalation via arbitrary registry lookups.
export const parentTrustedAmids: Set<string> = new Set();

// AMID → Ed25519 signing public key (base64) — cached from registry lookups
export const peerSigningKeys: Map<string, string> = new Map();

export function getCachedAmid(name: string): string | undefined {
  if (name === "parent") return nameToAmid.get(name);
  const ts = nameToAmidTs.get(name);
  if (ts === undefined) return nameToAmid.get(name);
  if (Date.now() - ts > AMID_CACHE_TTL_MS) {
    nameToAmid.delete(name);
    nameToAmidTs.delete(name);
    return undefined;
  }
  return nameToAmid.get(name);
}

export function setCachedAmid(name: string, amid: string): void {
  nameToAmid.set(name, amid);
  nameToAmidTs.set(name, Date.now());
}

// Pick the freshest live entry from a registry /search response. Filters to
// matches on display_name or capability, prefers online agents, then newest
// last_seen.
//
// **Trust scope (important):** within a single cluster the AgentMesh registry
// is cluster-local (NetworkPolicy-gated) and ClawSandbox names are unique by
// K8s admission, so display_name uniquely identifies one sandbox and the
// duplicates we see in the registry are always different AMIDs of the same
// logical sandbox across pod restarts (sandbox identities are ephemeral by
// design). When this assumption breaks — federated/multi-cluster registry, or
// a compromised sandbox squatting another agent's name — pass a `scopeFilter`
// that requires a known capability (e.g. `parent:<parent-amid>` or
// `cluster:<cluster-id>`) emitted by trusted peers at registration. A
// signed-spawn attestation from the controller is the longer-term fix.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function pickFreshestRegistryMatch(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  results: any[],
  agentName: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  scopeFilter?: (a: any) => boolean,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
): any | undefined {
  if (!Array.isArray(results) || results.length === 0) return undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const nameMatch = (a: any) =>
    a?.display_name === agentName || (Array.isArray(a?.capabilities) && a.capabilities.includes(agentName));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const filtered = results.filter((a: any) => nameMatch(a) && (scopeFilter ? scopeFilter(a) : true));
  // Fall back to results without the name filter only if no name match exists,
  // never widen past the scope filter — that's the trust boundary.
  const candidates = filtered.length > 0
    ? filtered
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    : results.filter((a: any) => (scopeFilter ? scopeFilter(a) : true));
  if (candidates.length === 0) return undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return [...candidates].sort((a: any, b: any) => {
    if (a?.status === "online" && b?.status !== "online") return -1;
    if (b?.status === "online" && a?.status !== "online") return 1;
    return (b?.last_seen || "").localeCompare(a?.last_seen || "");
  })[0];
}

// Single source of truth for "name → live AMID" resolution. Honors the TTL'd
// cache, and on miss queries the registry and picks the freshest live match.
// Used by every send/discovery path so cache + freshness behaviour is uniform
// — there is no other place where this lookup logic should be duplicated.
//
// `routerUrl` is injected by the caller (plugin.ts re-exports the helper from
// `core/router-client.ts`) so this module avoids a circular dep on plugin.ts.
//
// Options:
//   timeoutMs   — request timeout for the registry call (default 5s).
//   registryBase — override base URL (sub-agents use AGT_REGISTRY_URL when set).
//   scopeFilter — optional capability/identity guard (see pickFreshestRegistryMatch).
export async function resolveAmidByName(
  agentName: string,
  routerUrl: (path: string) => string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  opts: { timeoutMs?: number; registryBase?: string; scopeFilter?: (a: any) => boolean; bypassCache?: boolean } = {},
): Promise<string | undefined> {
  if (!opts.bypassCache) {
    const cached = getCachedAmid(agentName);
    if (cached) return cached;
  }

  const base = opts.registryBase ?? routerUrl("/agt/registry");
  const timeoutMs = opts.timeoutMs ?? 5000;
  try {
    const http = await import("node:http");
    const body = await new Promise<string>((resolve, reject) => {
      const req = http.get(
        `${base}/registry/search?capability=${encodeURIComponent(agentName)}`,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (res: any) => {
          let d = "";
          res.on("data", (c: Buffer) => { d += c.toString(); });
          res.on("end", () => resolve(d));
        },
      );
      req.on("error", reject);
      req.setTimeout(timeoutMs, () => { req.destroy(); reject(new Error("timeout")); });
    });
    const parsed = JSON.parse(body);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const results: any[] = Array.isArray(parsed) ? parsed : (parsed?.results || []);
    const match = pickFreshestRegistryMatch(results, agentName, opts.scopeFilter);
    const amid: string | undefined = match?.amid || match?.id;
    if (amid) {
      if (!opts.bypassCache) {
        setCachedAmid(agentName, amid);
        amidToName.set(amid, agentName);
      }
      return amid;
    }
  } catch { /* transient — caller decides whether to retry */ }
  return undefined;
}

// Resolve AMID → display_name via registry lookup (results cached in amidToName).
// Returns the display_name if found, or empty string on failure.
export async function resolveAmidToName(
  amid: string,
  routerUrl: (path: string) => string,
): Promise<string> {
  const cached = amidToName.get(amid);
  if (cached) return cached;
  try {
    const http = await import("node:http");
    const body = await new Promise<string>((resolve, reject) => {
      const req = http.get(
        routerUrl(`/agt/registry/registry/lookup?amid=${amid}`),
        (res) => {
          let d = "";
          res.on("data", (c: Buffer) => { d += c.toString(); });
          res.on("end", () => resolve(d));
        },
      );
      req.on("error", reject);
      req.setTimeout(3000, () => { req.destroy(); reject(new Error("timeout")); });
    });
    const parsed = JSON.parse(body);
    if (parsed.display_name) {
      amidToName.set(amid, parsed.display_name);
      nameToAmid.set(parsed.display_name, amid);
      return parsed.display_name;
    }
  } catch { /* best effort */ }
  return "";
}

// Resolve AMID → Ed25519 signing public key via registry lookup (cached).
// Returns the base64-encoded public key, or empty string if unavailable.
export async function resolveSigningKey(
  amid: string,
  routerUrl: (path: string) => string,
): Promise<string> {
  const cached = peerSigningKeys.get(amid);
  if (cached) return cached;
  try {
    const http = await import("node:http");
    const body = await new Promise<string>((resolve, reject) => {
      const req = http.get(
        routerUrl(`/agt/registry/registry/lookup?amid=${amid}`),
        (res) => {
          let d = "";
          res.on("data", (c: Buffer) => { d += c.toString(); });
          res.on("end", () => resolve(d));
        },
      );
      req.on("error", reject);
      req.setTimeout(3000, () => { req.destroy(); reject(new Error("timeout")); });
    });
    const parsed = JSON.parse(body);
    const key = parsed.signing_public_key || parsed.public_info?.signing_public_key || "";
    if (key) {
      peerSigningKeys.set(amid, key);
      return key;
    }
  } catch { /* best effort */ }
  return "";
}
