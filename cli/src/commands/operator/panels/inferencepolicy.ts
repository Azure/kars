/**
 * InferencePolicy panel (S4) — list + budgets + guardrail floor + model preference.
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY, formatConditions, renderItemHeader } from "./util.js";

export const inferencePolicyPanel: Panel = {
  id: "inferencepolicy",
  title: "InferencePolicy",
  refreshIntervalMs: 30_000,

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const items = opts?.sandbox
      ? state.inferencePolicies.filter((p) => p.appliesToSandbox === opts.sandbox || !p.appliesToSandbox)
      : state.inferencePolicies;
    if (items.length === 0) return EMPTY;

    const lines: string[] = [];
    for (const p of items) {
      lines.push(renderItemHeader(p));
      const applies = p.appliesToSandbox ?? "<all>";
      const daily = p.dailyTokens !== undefined ? `${p.dailyTokens}` : "—";
      const perReq = p.perRequestTokens !== undefined ? `${p.perRequestTokens}` : "—";
      lines.push(`  appliesTo: {cyan-fg}${applies}{/}   tokens: daily=${daily}   per-req=${perReq}`);
      const floor = p.guardrailFloor ?? "—";
      const floorColor =
        floor === "high" ? "green" :
        floor === "medium" ? "yellow" :
        floor === "low" ? "red" : "gray";
      lines.push(`  guardrail-floor: {${floorColor}-fg}${floor}{/}`);
      const models = (p.modelPreference ?? []);
      const modelLine = models.length > 0
        ? models.map((m, i) => `${i + 1}.${m}`).join(" → ")
        : "{gray-fg}(no preference){/}";
      lines.push(`  models: ${modelLine}`);
      lines.push(formatConditions(p.conditions));
    }
    return lines.join("\n");
  },
};
