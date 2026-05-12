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
// before a registry blip points at a dead identity in the registry).
export const nameToAmidTs: Map<string, number> = new Map();
// 60s — long enough to amortize registry lookups across normal sends, short
// enough that a crashed-and-restarted sub-agent is re-resolved within one
// follow-up send cycle.
export const AMID_CACHE_TTL_MS = 60_000;

// Spawned-sibling roster: name → role/persona description provided at spawn
// time by the parent. Used by azureclaw_mesh_send to prepend a deterministic
// "Peer roster:" block to outbound task content so sub-agents can resolve
// role references ("the writer", "the graphic designer") to canonical names
// without LLM guessing. Populated on azureclaw_spawn, cleaned on
// azureclaw_spawn_destroy.
export const spawnedRoster: Map<string, string> = new Map();

// ── Name-based trust (authoritative) ──────────────────────────────────
// Trust is keyed by NAME (admission-unique sandbox display_name), not by
// the ephemeral AMID. The router enforces that a sandbox can only register
// the registry under its own SANDBOX_NAME, which makes display_name a
// control-plane-attested identity that survives pod restarts and AMID
// rotation.
//
// `parentTrustedNames` is the source of truth, seeded at boot from
// AGT_TRUSTED_PEERS. AMID-keyed trust lives only in `verifiedAmidCache`,
// which is a TTL'd runtime artifact — populated when an incoming KNOCK is
// verified against the registry, and superseded whenever the same name
// rebinds to a fresh AMID.
export const parentTrustedNames: Set<string> = new Set();

interface VerifiedTrust {
  name: string;
  verifiedAt: number;
  // 'env-hint' = pre-seeded warm cache (must be re-verified before granting
  //              the parent-trust bonus on a KNOCK).
  // 'registry-verified' = went through verifyTrustedByName successfully.
  source: "env-hint" | "registry-verified";
}

export const verifiedAmidCache: Map<string, VerifiedTrust> = new Map();

// 2 minutes — long enough that fan-out KNOCK bursts after a parent restart
// don't re-hit the registry repeatedly, short enough that a peer that loses
// trust (e.g. its display_name changed in registry) re-verifies promptly.
export const VERIFIED_TRUST_TTL_MS = 120_000;

// Backwards-compatible re-export — older code paths in plugin.ts and
// agt-handoff still call `parentTrustedAmids.has(amid)` for cheap O(1) probes.
// The new semantics: an AMID is "in" this set if it has a non-expired
// `registry-verified` entry in `verifiedAmidCache`. Mutating this set
// directly (via `.add` / `.delete`) is no longer the right way to grant
// trust — call `verifyTrustedByName` to mint a verified entry instead.
export const parentTrustedAmids: Set<string> = new Set();

// Internal: keep parentTrustedAmids in sync with verifiedAmidCache so existing
// `.has(amid)` call sites continue to function.
function syncTrustedAmidView(amid: string, present: boolean): void {
  if (present) parentTrustedAmids.add(amid);
  else parentTrustedAmids.delete(amid);
}

// AMID → Ed25519 signing public key (base64) — cached from registry lookups
export const peerSigningKeys: Map<string, string> = new Map();

export function getCachedAmid(name: string): string | undefined {
  // Apply TTL uniformly: the parent identity is also ephemeral (pod restart
  // rotates its AMID), so the historical "parent" alias bypass was a fourth
  // stale-AMID bug. resolveAmidByName falls back to the registry on miss.
  const ts = nameToAmidTs.get(name);
  if (ts === undefined) return nameToAmid.get(name);
  if (Date.now() - ts > AMID_CACHE_TTL_MS) {
    nameToAmid.delete(name);
    nameToAmidTs.delete(name);
    return undefined;
  }
  return nameToAmid.get(name);
}

// F7: Last-known-good fallback. Returns the cached AMID for `name` even if
// the TTL has expired. Used by send/transfer paths after the registry retry
// budget is exhausted: if the peer was reachable recently, try its last AMID
// rather than failing the tool call. If the peer truly died, meshClient.send
// will fail fast and the caller surfaces the error to the LLM.
export function getStaleAmid(name: string): string | undefined {
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
    // Use the provider-aware registry abstraction so we hit the right URL
    // shape under AZURECLAW_MESH_PROVIDER=agt. When the caller pinned a
    // custom base, we construct an explicit impl rather than the cached
    // singleton (sub-agents override base via AGT_REGISTRY_URL).
    const { getMeshRegistry } = await import("./mesh-registry.js");
    const reg = getMeshRegistry(() => base);
    const results = await reg.search(agentName, { timeoutMs });
    const match = pickFreshestRegistryMatch(results, agentName, opts.scopeFilter);
    const amid: string | undefined = match?.amid || (match as { id?: string } | undefined)?.id;
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
    const { getMeshRegistry } = await import("./mesh-registry.js");
    const rec = await getMeshRegistry(routerUrl).lookup(amid, { timeoutMs: 3000 });
    if (rec?.display_name) {
      amidToName.set(amid, rec.display_name);
      nameToAmid.set(rec.display_name, amid);
      return rec.display_name;
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
    const { getMeshRegistry } = await import("./mesh-registry.js");
    const rec = await getMeshRegistry(routerUrl).lookup(amid, { timeoutMs: 3000 });
    const key = rec?.signing_public_key || rec?.public_info?.signing_public_key || "";
    if (key) {
      peerSigningKeys.set(amid, key);
      return key;
    }
  } catch { /* best effort */ }
  return "";
}

// ── Name-based trust helpers ──────────────────────────────────────────

/**
 * Parse the AGT_TRUSTED_PEERS env var into seed entries.
 * Format is `name[:AMID][,name[:AMID]...]`. The AMID is an optional warm
 * cache hint — receivers must re-verify it via the registry before granting
 * a parent-trust bonus on a KNOCK.
 *
 * Splits on the FIRST `:` so an AMID containing `:` (forward-compat) is not
 * truncated. Empty / whitespace-only names are dropped.
 */
export function parseTrustedPeers(env: string): Array<{ name: string; amid?: string }> {
  if (!env) return [];
  return env.split(",")
    .map(p => p.trim())
    .filter(Boolean)
    .map(peer => {
      const idx = peer.indexOf(":");
      if (idx < 0) return { name: peer };
      const name = peer.slice(0, idx).trim();
      const amid = peer.slice(idx + 1).trim();
      return { name, amid: amid || undefined };
    })
    .filter(e => e.name.length > 0);
}

/**
 * Seed authoritative trust by name plus a warm AMID hint cache.
 * Hints DO NOT grant the parent-trust bonus on their own — they're only used
 * to skip a registry lookup when an incoming KNOCK's AMID happens to match.
 * The first call to `verifyTrustedByName` for an unknown AMID re-resolves
 * via the registry and supersedes any stale hint.
 */
export function seedTrustedPeers(env: string): void {
  for (const { name, amid } of parseTrustedPeers(env)) {
    parentTrustedNames.add(name);
    if (amid) {
      verifiedAmidCache.set(amid, {
        name,
        verifiedAt: Date.now(),
        source: "env-hint",
      });
      // Keep amidToName / nameToAmid in lockstep for send-side resolution.
      amidToName.set(amid, name);
      nameToAmid.set(name, amid);
      nameToAmidTs.set(name, Date.now());
    }
  }
}

/**
 * Evict every cached entry pointing at `oldAmid` for the trusted name `name`
 * — used when a fresh AMID for the same name is verified, e.g. after a
 * parent restart. This is what closes the door on the receiver-side stale
 * trust bug. SDK ratchet sessions are keyed by AMID and naturally re-KNOCK
 * for the new AMID, so we only evict our own caches.
 */
export function evictPriorAmidsForName(name: string, keepAmid: string): void {
  // Sweep verifiedAmidCache for the same name with a different AMID.
  for (const [amid, entry] of verifiedAmidCache.entries()) {
    if (entry.name === name && amid !== keepAmid) {
      verifiedAmidCache.delete(amid);
      syncTrustedAmidView(amid, false);
      amidToName.delete(amid);
      peerSigningKeys.delete(amid);
    }
  }
  // nameToAmid is point-in-time; let the next lookup refresh it. Resetting
  // here would cause an extra registry hit; leave the rebind to the caller.
  if (nameToAmid.get(name) !== keepAmid) {
    nameToAmid.set(name, keepAmid);
    nameToAmidTs.set(name, Date.now());
  }
}

/**
 * Return whether `amid` currently has a non-expired `registry-verified`
 * trust entry. Used by the KNOCK handler to decide whether to skip the
 * verify-by-name round trip on a hot path. Hints (`source: "env-hint"`)
 * never count — they must be promoted to verified first.
 */
export function isAmidVerified(amid: string, now: number = Date.now()): boolean {
  const entry = verifiedAmidCache.get(amid);
  if (!entry) {
    syncTrustedAmidView(amid, false);
    return false;
  }
  if (entry.source !== "registry-verified") return false;
  if (now - entry.verifiedAt > VERIFIED_TRUST_TTL_MS) {
    verifiedAmidCache.delete(amid);
    syncTrustedAmidView(amid, false);
    return false;
  }
  syncTrustedAmidView(amid, true);
  return true;
}

interface VerificationOptions {
  /** Total time budget for retries on registry lag, ms. */
  retryBudgetMs?: number;
  /** Per-attempt fetch timeout, ms. */
  perRequestTimeoutMs?: number;
  /** Override base URL (sub-agents use AGT_REGISTRY_URL when set). */
  registryBase?: string;
}

interface VerificationResult {
  trusted: boolean;
  /** Resolved display_name (when registry returned one). */
  name?: string;
  /** Reason a non-trusted result was returned — for logging only. */
  reason?: string;
}

/**
 * Verify that `fromAmid` belongs to a peer whose display_name is in
 * `parentTrustedNames`, AND that the same AMID is the freshest live
 * registration in the registry for that name (defense against a stale
 * ghost being chosen for an attacker who later squats the name — though
 * the router-side enforcement should already prevent the squat).
 *
 * Bounded retry handles the registry-lag race after a parent restart:
 * the new parent's AMID may not be visible the instant the KNOCK arrives.
 */
export async function verifyTrustedByName(
  fromAmid: string,
  routerUrl: (path: string) => string,
  opts: VerificationOptions = {},
): Promise<VerificationResult> {
  const cached = verifiedAmidCache.get(fromAmid);
  if (cached
    && cached.source === "registry-verified"
    && Date.now() - cached.verifiedAt <= VERIFIED_TRUST_TTL_MS) {
    syncTrustedAmidView(fromAmid, true);
    return { trusted: true, name: cached.name };
  }

  const retryBudgetMs = opts.retryBudgetMs ?? 2000;
  const perRequestTimeoutMs = opts.perRequestTimeoutMs ?? 1500;
  const base = opts.registryBase ?? routerUrl("/agt/registry");

  const start = Date.now();
  const delays = [0, 100, 250, 500, 1000];
  let attempt = 0;
  let lastReason = "registry_unreachable";

  while (Date.now() - start < retryBudgetMs) {
    if (attempt > 0) {
      const wait = delays[Math.min(attempt, delays.length - 1)];
      if (Date.now() - start + wait > retryBudgetMs) break;
      await new Promise(r => setTimeout(r, wait));
    }
    attempt += 1;

    const lookupName = await registryLookupDisplayName(fromAmid, base, perRequestTimeoutMs);
    if (!lookupName) {
      lastReason = "amid_not_in_registry";
      continue;
    }
    if (!parentTrustedNames.has(lookupName)) {
      // No retry — name is well-defined and not in our trust set.
      return { trusted: false, name: lookupName, reason: "name_not_trusted" };
    }
    const freshestAmid = await registrySearchFreshestAmid(lookupName, base, perRequestTimeoutMs);
    if (!freshestAmid) {
      lastReason = "search_returned_no_match";
      continue;
    }
    if (freshestAmid !== fromAmid) {
      // Either we're early (registry hasn't indexed the new identity yet)
      // or the AMID is genuinely stale. Retry a few times — if the new
      // AMID is the legitimate one, registry will catch up; if the legit
      // owner already rotated past, this AMID is dead and we must reject.
      lastReason = `freshest_amid_mismatch(${freshestAmid.slice(0, 12)}...)`;
      continue;
    }

    // Verified — record it and evict any stale AMIDs that share the name.
    verifiedAmidCache.set(fromAmid, {
      name: lookupName,
      verifiedAt: Date.now(),
      source: "registry-verified",
    });
    syncTrustedAmidView(fromAmid, true);
    evictPriorAmidsForName(lookupName, fromAmid);
    amidToName.set(fromAmid, lookupName);
    return { trusted: true, name: lookupName };
  }

  return { trusted: false, reason: lastReason };
}

async function registryLookupDisplayName(
  amid: string,
  base: string,
  timeoutMs: number,
): Promise<string | undefined> {
  try {
    const { getMeshRegistry } = await import("./mesh-registry.js");
    // Use base override (some callers pin a specific registry endpoint).
    const reg = getMeshRegistry(() => base);
    const rec = await reg.lookup(amid, { timeoutMs });
    return rec?.display_name;
  } catch {
    return undefined;
  }
}

async function registrySearchFreshestAmid(
  name: string,
  base: string,
  timeoutMs: number,
): Promise<string | undefined> {
  try {
    const { getMeshRegistry } = await import("./mesh-registry.js");
    const reg = getMeshRegistry(() => base);
    const results = await reg.search(name, { timeoutMs });
    // Authoritative match is the display_name field — the router enforces
    // a sandbox can only register its own SANDBOX_NAME there. Capability
    // self-assertion is a softer signal; we ignore it here.
    const match = pickFreshestRegistryMatch(
      results,
      name,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (a: any) => a?.display_name === name,
    );
    return match?.amid || (match as { id?: string } | undefined)?.id;
  } catch {
    return undefined;
  }
}
