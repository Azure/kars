// kars Bridge web — value formatting helpers. Disciplined, audit-friendly
// rendering of machine values (counts, budgets, money).

/** Thousands-separated integer, or an em-dash placeholder when null. */
export function formatInt(n: number | null | undefined): string {
  if (n == null) return "—";
  return n.toLocaleString();
}

export function formatWarmIdle(seconds: number | null | undefined): string {
  const value = seconds ?? 900;
  if (value <= 0) return "immediately";
  if (value < 60) return `${value}s`;
  const minutes = Math.floor(value / 60);
  const remainder = value % 60;
  if (minutes < 60) return remainder ? `${minutes}m ${remainder}s` : `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}

/** Micro-USD (1e-6 USD) rendered as currency, or null when unset. */
export function formatUsdMicros(micros: number | null | undefined): string | null {
  if (micros == null) return null;
  return `$${(micros / 1_000_000).toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

/** Classify an egress host as an internal (in-cluster / private) destination or
 *  an external (public internet) one. Purely presentational — the engine treats
 *  every entry identically as an allowed egress; this only helps the operator
 *  read the list. A host is internal if it targets a Kubernetes service domain,
 *  a private/loopback address, or an explicitly internal suffix. */
export function egressScope(host: string): "internal" | "external" {
  const h = host.trim().toLowerCase().split(":")[0];
  const internalSuffix = [
    ".svc",
    ".svc.cluster.local",
    ".cluster.local",
    ".local",
    ".internal",
    ".in-addr.arpa",
  ];
  if (h === "localhost" || h.endsWith(".localhost")) return "internal";
  if (internalSuffix.some((s) => h.endsWith(s))) return "internal";
  // Bare single-label hostnames (no dot) resolve in-cluster.
  if (!h.includes(".")) return "internal";
  // RFC1918 / loopback / link-local IP ranges.
  if (
    /^10\./.test(h) ||
    /^127\./.test(h) ||
    /^192\.168\./.test(h) ||
    /^169\.254\./.test(h) ||
    /^172\.(1[6-9]|2\d|3[0-1])\./.test(h)
  )
    return "internal";
  return "external";
}

// End users shouldn't see raw in-cluster DNS for MCP servers. Turn e.g.
// "https://github-mcp.default.svc.cluster.local:8080" into "github-mcp".
// Non-cluster identifiers (already human names) are returned unchanged.
export function humanizeMcp(server: string): string {
  let s = server.trim().replace(/^[a-z]+:\/\//i, ""); // strip scheme
  s = s.split("/")[0].split(":")[0]; // host only, drop path + port
  if (s.includes(".svc.cluster.local") || s.endsWith(".local")) {
    s = s.split(".")[0]; // keep the service label
  }
  return s || server;
}
