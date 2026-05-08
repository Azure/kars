// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * ClawSandbox panel — list of sandboxes with health, model, isolation.
 *
 * S14 refactor: wraps the existing operator-TUI sandbox table data into
 * the `Panel` interface so it lives alongside the new Phase-2 panels.
 * The byte-level rendering of the legacy agent table is unchanged — this
 * panel is for the new modular-panels layout (`--panels` / `--per-sandbox`).
 */
import type { Panel, PanelRenderOpts, ClusterState } from "./types.js";
import { EMPTY } from "./util.js";

export const clawSandboxPanel: Panel = {
  id: "clawsandbox",
  title: "ClawSandbox",
  category: "agent",
  purpose: "the agents themselves — pod, runtime, model, isolation",
  refreshIntervalMs: 10_000,

  summarize(state) {
    const items = state.sandboxes;
    const s = { total: items.length, healthy: 0, warning: 0, error: 0, unknown: 0 };
    for (const it of items) {
      if (it.health === "healthy") s.healthy += 1;
      else if (it.health === "down") s.error += 1;
      else if (it.health === "degraded") s.warning += 1;
      else s.unknown += 1;
    }
    return s;
  },

  render(state: ClusterState, opts?: PanelRenderOpts): string {
    const all = state.sandboxes;
    const items = opts?.sandbox ? all.filter((s) => s.name === opts.sandbox) : all;
    if (items.length === 0) return `${EMPTY}`;

    const lines: string[] = [];
    lines.push(
      `{gray-fg}${"NAME".padEnd(28)} ${"HEALTH".padEnd(10)} ${"RUNTIME".padEnd(10)} ${"MODEL".padEnd(20)} ${"ISOLATION".padEnd(12)} ${"AGE".padEnd(6)} ROLE{/}`,
    );
    for (const s of items) {
      const healthColor =
        s.health === "healthy" ? "green" :
        s.health === "degraded" ? "yellow" :
        s.health === "down" ? "red" :
        s.health === "dormant" ? "blue" : "gray";
      const rk = s.runtimeKind || "OpenClaw";
      const rkTag =
        rk === "OpenClaw" ? "OC" :
        rk === "OpenAIAgents" ? "OAI" :
        rk === "MicrosoftAgentFramework" ? "MAF" :
        rk === "LangGraph" ? "LG" :
        rk === "Anthropic" ? "Anthropic" :
        rk === "PydanticAi" ? "PydAI" :
        rk === "BYO" ? "BYO" : rk;
      lines.push(
        `${s.name.padEnd(28)} ` +
          `{${healthColor}-fg}${s.health.padEnd(10)}{/} ` +
          `${rkTag.padEnd(10)} ` +
          `${(s.model || "-").padEnd(20)} ` +
          `${(s.isolation || "-").padEnd(12)} ` +
          `${(s.age || "-").padEnd(6)} ` +
          `${s.role}`,
      );
    }
    return lines.join("\n");
  },
};
