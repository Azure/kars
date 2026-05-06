// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Shared rendering helpers for operator-TUI panels (S14).
 *
 * Tiny pure utilities so individual panel modules stay short. The
 * blessed-tag flavor matches the rest of `cli/src/commands/operator/`.
 */
import type { CrdCondition, CrdItem } from "./types.js";

/** Wrap text as a panel section: title line + horizontal rule + body. */
export function section(title: string, body: string): string {
  return `{bold}${title}{/}\n${body}`;
}

/** Compact "(empty)" indicator for empty lists. */ // ci:stub-ok: documentation comment, not a code stub
export const EMPTY = "{gray-fg}(none){/}";

/** Render a list of CrdConditions as one-liners. The reason+message is
 *  preserved verbatim — no creative rephrasing (plan §0.2 #10). */
export function formatConditions(conds: CrdCondition[]): string {
  if (conds.length === 0) return "  {gray-fg}(no conditions){/}";
  return conds
    .map((c) => {
      const color =
        c.status === "True" ? "green" :
        c.status === "False" ? "red" : "yellow";
      const reason = c.reason ? ` ${c.reason}` : "";
      const msg = c.message ? `: ${c.message}` : "";
      return `  {${color}-fg}●{/} ${c.type}=${c.status}${reason}${msg}`;
    })
    .join("\n");
}

/** Pad a string to a fixed visible width (no blessed-tag stripping needed
 *  because callers pass plain strings here). */
export function pad(s: string, width: number): string {
  if (s.length >= width) return s.substring(0, width);
  return s + " ".repeat(width - s.length);
}

/** Render a uniform "name | namespace | age" header column for any CrdItem. */
export function renderItemHeader(item: CrdItem): string {
  const age = item.age ? ` {gray-fg}${item.age}{/}` : "";
  return `{cyan-fg}${item.name}{/} {gray-fg}(${item.namespace}){/}${age}`;
}

/** Filter a list of items by sandbox association. The association is
 *  derived from a property name on the item (one of "appliesToSandbox",
 *  "sandboxRef", "agentA"/"agentB" for pairings).
 *
 *  Items without the property pass through (cluster-scoped panels).
 */
export function filterBySandbox<T extends Record<string, unknown>>(
  items: T[],
  sandbox: string | undefined,
  keys: string[],
): T[] {
  if (!sandbox) return items;
  return items.filter((it) => {
    for (const k of keys) {
      const v = it[k];
      if (typeof v === "string" && v === sandbox) return true;
    }
    return false;
  });
}

/** Health bucket for a single CrdItem, derived from its conditions.
 *
 *  Kubernetes uses two polarities for condition types:
 *    - **Positive** (Ready, Available, Established, Bound, Synced):
 *      True=good, False=bad, Unknown=unknown.
 *    - **Negative** (Degraded, Failing, ReconcileError):
 *      True=bad (regardless of Ready), False=good (do NOT count as error),
 *      Unknown=ignored.
 *  Informational types (Progressing) don't gate health — Progressing=False
 *  is the steady-state for a fully reconciled object and must not be
 *  reported as an error.
 *
 *  Rules:
 *   - Any negative-polarity condition with status="True" → "error".
 *   - The single positive-polarity Ready/Available/etc. condition decides
 *     the bucket: True=healthy, False=error, Unknown=unknown.
 *   - Empty list, or no positive condition found → "unknown".
 */
const POSITIVE_TYPES = new Set([
  "Ready", "Available", "Established", "Bound", "Synced",
]);
const NEGATIVE_TYPES = new Set([
  "Degraded", "Failing", "ReconcileError",
]);

export function bucketFromConditions(
  conds: CrdCondition[] | undefined,
): "healthy" | "warning" | "error" | "unknown" {
  if (!conds || conds.length === 0) return "unknown";
  let positive: "healthy" | "error" | "unknown" | undefined;
  for (const c of conds) {
    if (NEGATIVE_TYPES.has(c.type) && c.status === "True") return "error";
    if (POSITIVE_TYPES.has(c.type)) {
      if (c.status === "True" && positive !== "error") positive = "healthy";
      else if (c.status === "False") positive = "error";
      else if (c.status === "Unknown" && positive === undefined) positive = "unknown";
    }
  }
  return positive ?? "unknown";
}

/** Roll a list of CrdItems into a PanelSummary. */
export function summarizeItems<T extends CrdItem>(
  items: T[],
  detail?: string,
): import("./types.js").PanelSummary {
  const s = { total: items.length, healthy: 0, warning: 0, error: 0, unknown: 0, detail };
  for (const it of items) {
    const b = bucketFromConditions(it.conditions);
    s[b] += 1;
  }
  return s;
}
