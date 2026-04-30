/**
 * A2AAgent panel (S3) — list + Conditions + AgentCard publication status.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader } from "./util.js";

export const a2aAgentPanel: Panel = {
  id: "a2aagent",
  title: "A2AAgent",
  refreshIntervalMs: 30_000,

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    // A2AAgent is namespace-scoped; per-sandbox view filters by namespace.
    const items = opts?.sandbox
      ? state.a2aAgents.filter((a) => a.namespace === `azureclaw-${opts.sandbox}`)
      : state.a2aAgents;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const a of items) {
      lines.push(renderItemHeader(a));
      const url = a.endpointUrl ?? "(no endpoint)";
      const prod = a.productionMode === undefined ? "?" : (a.productionMode ? "yes" : "no");
      lines.push(`  endpoint: {cyan-fg}${url}{/}   production=${prod}`);
      const cardColor =
        a.agentCardPublished === "published" ? "green" :
        a.agentCardPublished === "failed" ? "red" :
        a.agentCardPublished === "pending" ? "yellow" : "gray";
      const cardValue = a.agentCardPublished ?? "unknown";
      const cardReason = a.agentCardReason ? ` {gray-fg}(${a.agentCardReason}){/}` : "";
      lines.push(`  AgentCard: {${cardColor}-fg}${cardValue}{/}${cardReason}`);
      const caps = a.capabilities ?? [];
      if (caps.length > 0) {
        lines.push(`  capabilities: ${caps.join(", ")}`);
      }
      lines.push(formatConditions(a.conditions));
    }
    return lines.join("\n");
  },
};
