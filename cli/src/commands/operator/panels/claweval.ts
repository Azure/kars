// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * ClawEval panel (S6) — list + lastRunAt + lastScore + nextScheduledAt.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader, summarizeItems } from "./util.js";

export const clawEvalPanel: Panel = {
  id: "claweval",
  title: "ClawEval",
  category: "optional",
  purpose: "reproducible eval runs — pinned image+config + scored history",
  refreshIntervalMs: 30_000,

  summarize(state) { return summarizeItems(state.clawEvals); },

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const items = opts?.sandbox
      ? state.clawEvals.filter((e) => e.sandboxRef === opts.sandbox)
      : state.clawEvals;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const e of items) {
      lines.push(renderItemHeader(e));
      const sb = e.sandboxRef ?? "?";
      const suite = e.suite ?? "—";
      const sched = e.schedule ?? "(on-demand)";
      lines.push(`  sandbox: {cyan-fg}${sb}{/}   suite=${suite}   schedule=${sched}`);
      const last = e.lastRunAt ?? "(never)";
      const score = e.lastScore ?? "—";
      const next = e.nextScheduledAt ?? "—";
      lines.push(`  last-run: ${last}   score: ${score}   next: ${next}`);
      lines.push(formatConditions(e.conditions));
    }
    return lines.join("\n");
  },
};
