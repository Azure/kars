/**
 * Operator-TUI pure helper functions.
 *
 * Extracted from `cli/src/commands/operator.ts` (S15.e.1) so the
 * top-level dashboard module can stay under §4.2's 800-line cap.
 *
 * Both helpers are pure: no I/O, no closure capture, no state.
 * They were already module-level in operator.ts (not nested inside
 * `startDashboard`) and are byte-identical to the originals.
 */

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
 * Example line: `azureclaw_tokens_total{direction="input",model="gpt-4",sandbox="s1"} 8500`
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
