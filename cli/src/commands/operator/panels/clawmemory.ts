/**
 * ClawMemory panel (S5) — list + Foundry binding + RBAC scope summary.
 *
 * Note: the runtime path for memory binding is router-side (S7+); this
 * panel surfaces what the controller observed (status.phase) plus the
 * RBAC scope summary the operator already records on the binding.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader } from "./util.js";

export const clawMemoryPanel: Panel = {
  id: "clawmemory",
  title: "ClawMemory",
  refreshIntervalMs: 30_000,

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const items = opts?.sandbox
      ? state.clawMemories.filter((m) => m.sandboxRef === opts.sandbox)
      : state.clawMemories;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const m of items) {
      lines.push(renderItemHeader(m));
      const sb = m.sandboxRef ?? "?";
      const store = m.storeName ?? "—";
      const scope = m.scope ?? "—";
      const retention = m.retentionDays !== undefined ? `${m.retentionDays}d` : "—";
      lines.push(`  sandbox: {cyan-fg}${sb}{/}   store=${store}   scope=${scope}   retention=${retention}`);
      const bindColor =
        m.foundryBound === "bound" ? "green" :
        m.foundryBound === "failed" ? "red" :
        m.foundryBound === "pending" ? "yellow" : "gray";
      const bindValue = m.foundryBound ?? "unknown";
      lines.push(`  foundry-binding: {${bindColor}-fg}${bindValue}{/}`);
      if (m.rbacScopeSummary) {
        lines.push(`  rbac: {gray-fg}${m.rbacScopeSummary}{/}`);
      }
      lines.push(formatConditions(m.conditions));
    }
    return lines.join("\n");
  },
};
