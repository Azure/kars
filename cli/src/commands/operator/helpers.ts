// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Operator-TUI pure helper functions.
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.1) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * All helpers are pure: no I/O, no closure capture, no state.
 * They were already module-level in operator.ts (not nested inside
 * `startDashboard`) and are byte-identical to the originals.
 */

/**
 * Build a `kubectl` argv with an optional `--context` prefix.
 *
 * `kctl(["get","pods"], "my-cluster")` → `["--context","my-cluster","get","pods"]`
 * `kctl(["get","pods"], undefined)`     → `["get","pods"]`
 */
export function kctl(args: string[], context?: string): string[] {
  return context ? ["--context", context, ...args] : args;
}

/**
 * Return a compact human-readable duration string for "time since `date`".
 *
 *   < 60s  → "Ns"
 *   < 60m  → "Nm"
 *   < 24h  → "Nh"
 *   else   → "Nd"
 *
 * Future dates clamp to "0s" (no negative output).
 */
export function timeSince(date: Date): string {
  const seconds = Math.floor((Date.now() - date.getTime()) / 1000);
  if (seconds < 0) return "0s";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return `${days}d`;
}

/**
 * Parse Prometheus text format and sum values for a given metric name.
 * Optionally filter by label key=value pairs.
 *
 * Example line: `kars_tokens_total{direction="input",model="gpt-4",sandbox="s1"} 8500`
 */
export function sumPrometheusCounter(
  text: string,
  metricName: string,
  labelFilter?: Record<string, string>,
): number {
  let total = 0;
  for (const line of text.split("\n")) {
    if (line.startsWith("#") || !line.startsWith(metricName)) continue;

    // Check label filter
    if (labelFilter) {
      let match = true;
      for (const [k, v] of Object.entries(labelFilter)) {
        if (!line.includes(`${k}="${v}"`)) { match = false; break; }
      }
      if (!match) continue;
    }

    // Extract numeric value after the closing brace (or after metric name if no labels)
    const valMatch = line.match(/\}\s+([0-9eE.+-]+)$/) || line.match(/^[^\s{]+\s+([0-9eE.+-]+)$/);
    if (valMatch) {
      total += parseFloat(valMatch[1]) || 0;
    }
  }
  return total;
}

/**
 * Compact one-letter platform tag for a sandbox row.
 *
 * The TUI needs to fit the host type into a tiny column or a topology
 * subtitle. The three values cover all the deployment surfaces the
 * controller supports today:
 *
 *   D — Docker (local docker-engine sandbox; `runtime === "docker"`)
 *   K — Kind  (local Kubernetes via kind; `runtime === "aks"` but
 *              `kubeContext` starts with `kind-`)
 *   C — Cloud (real AKS / any other kubernetes context;
 *              `runtime === "aks"` and not kind)
 *
 * `runtime: "aks"` is a legacy field name from before kind support
 * landed — it now means "any kubernetes context", not specifically
 * Azure Kubernetes Service. The `kubeContext` tag added in the
 * multi-cluster refactor is what distinguishes kind from real AKS.
 */
export function platformTag(s: {
  runtime: "docker" | "aks";
  kubeContext?: string;
}): "D" | "K" | "C" {
  if (s.runtime === "docker") return "D";
  if (s.kubeContext && s.kubeContext.startsWith("kind-")) return "K";
  return "C";
}

/**
 * Full word for the platform — used where space allows (e.g. tooltips,
 * status lines). Same mapping as `platformTag` above.
 */
export function platformLabel(s: {
  runtime: "docker" | "aks";
  kubeContext?: string;
}): "docker" | "kind" | "cloud" {
  if (s.runtime === "docker") return "docker";
  if (s.kubeContext && s.kubeContext.startsWith("kind-")) return "kind";
  return "cloud";
}

/**
 * Stable, globally-unique key for a sandbox across runtimes and clusters.
 *
 * Sandbox NAMES are not unique: the same agent name (e.g. "analyst") can
 * exist simultaneously as a docker container, a kind-cluster pod, and an
 * AKS pod. Keying per-sandbox maps (securityStates, egressByAgent) by bare
 * `name` causes these to collide — a stale/empty same-named entry overwrites
 * the live one, so the operator shows no DID / no audit for the shadowed
 * agent. Compose the origin (kubeContext for K8s, runtime for docker) with
 * the namespace and name to disambiguate.
 */
export function sandboxKey(s: {
  name: string;
  namespace: string;
  runtime?: "docker" | "aks";
  kubeContext?: string;
}): string {
  const origin = s.kubeContext ?? s.runtime ?? "local";
  return `${origin}::${s.namespace}::${s.name}`;
}

/**
 * Compact "<tag> <cluster-name>" used in the agent-table Cluster column
 * and the CRD snapshot status line. Docker sandboxes show just "D"
 * (no cluster name). Kube sandboxes show "K kars-dev" or
 * "C kars-aks" — the kubeContext with the `kind-` prefix stripped.
 */
export function clusterOriginTag(s: {
  runtime: "docker" | "aks";
  kubeContext?: string;
}): string {
  const tag = platformTag(s);
  const clusterName = s.runtime === "docker"
    ? ""
    : s.kubeContext?.replace(/^kind-/, "") ?? "";
  return clusterName ? `${tag} ${clusterName}` : tag;
}
