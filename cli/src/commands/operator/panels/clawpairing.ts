// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * ClawPairing panel — peer-pairing CRDs with trust state.
 *
 * S14 refactor: existing pairing surface (used by `azureclaw pair` and
 * `azureclaw mesh`) lifted into a panel. CR shape: `clawpairings` with
 * `spec.agentA`, `spec.agentB`, `status.phase`, `status.trustState`.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader, summarizeItems } from "./util.js";

export const clawPairingPanel: Panel = {
  id: "clawpairing",
  title: "ClawPairing",
  category: "internal",
  purpose: "controller-managed handshake state — usually safe to ignore",
  refreshIntervalMs: 15_000,

  summarize(state) { return summarizeItems(state.pairings); },

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const items = opts?.sandbox
      ? state.pairings.filter(
          (p) => p.agentA === opts.sandbox || p.agentB === opts.sandbox,
        )
      : state.pairings;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const p of items) {
      lines.push(renderItemHeader(p));
      lines.push(
        `  pair: {cyan-fg}${p.agentA ?? "?"}{/} ↔ {cyan-fg}${p.agentB ?? "?"}{/}` +
          (p.state ? `   state=${p.state}` : "") +
          (p.trust ? `   trust=${p.trust}` : ""),
      );
      lines.push(formatConditions(p.conditions));
    }
    return lines.join("\n");
  },
};
