// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * ToolPolicy panel (S2) — list + appliesTo + commerce / approval / rate-limit.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader, summarizeItems } from "./util.js";

export const toolPolicyPanel: Panel = {
  id: "toolpolicy",
  title: "ToolPolicy",
  category: "infrastructure",
  purpose: "allow/deny tools, approval gates, rate limits, commerce mandates",
  refreshIntervalMs: 30_000,

  summarize(state) { return summarizeItems(state.toolPolicies); },

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const items = opts?.sandbox
      ? state.toolPolicies.filter((t) => t.appliesToSandbox === opts.sandbox || !t.appliesToSandbox)
      : state.toolPolicies;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const t of items) {
      lines.push(renderItemHeader(t));
      const applies = t.appliesToSandbox ?? "<all>";
      const rate = t.rateLimitPerMin !== undefined ? `${t.rateLimitPerMin}/min` : "—";
      lines.push(`  appliesTo: {cyan-fg}${applies}{/}   rules=${t.ruleCount ?? 0}   rate-limit=${rate}`);
      const c = t.commerce;
      if (c && (c.mandates !== undefined || c.floorUsd !== undefined)) {
        const mandates = c.mandates ? "yes" : "no";
        const floor = c.floorUsd !== undefined ? `$${c.floorUsd.toFixed(2)}` : "—";
        lines.push(`  commerce: mandates=${mandates}   floor=${floor}`);
      }
      lines.push(formatConditions(t.conditions));
    }
    return lines.join("\n");
  },
};
