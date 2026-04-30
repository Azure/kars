// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Panel registry + layout — assembles the operator-TUI dashboard from a
 * list of `Panel` modules, with optional `--panels <a,b,c>` filtering and
 * a `--per-sandbox` vertical grouping mode (plan §S14).
 */
import type { Panel, ClusterState } from "./types.js";
import { clawSandboxPanel } from "./clawsandbox.js";
import { clawPairingPanel } from "./clawpairing.js";
import { mcpServerPanel } from "./mcpserver.js";
import { toolPolicyPanel } from "./toolpolicy.js";
import { inferencePolicyPanel } from "./inferencepolicy.js";
import { a2aAgentPanel } from "./a2aagent.js";
import { clawMemoryPanel } from "./clawmemory.js";
import { clawEvalPanel } from "./claweval.js";
import { providerStatusPanel } from "./provider_status.js";

/** Default panel order (also doubles as `--panels=all` resolver). */
export const DEFAULT_PANELS: Panel[] = [
  clawSandboxPanel,
  clawPairingPanel,
  mcpServerPanel,
  toolPolicyPanel,
  inferencePolicyPanel,
  a2aAgentPanel,
  clawMemoryPanel,
  clawEvalPanel,
  providerStatusPanel,
];

export const PANEL_BY_ID: Record<string, Panel> = Object.fromEntries(
  DEFAULT_PANELS.map((p) => [p.id, p]),
);

export interface LayoutOpts {
  /** Comma-separated list of panel ids; undefined → all defaults. */
  panels?: string;
  /** When true, render each sandbox's panel set as a vertical block. */
  perSandbox?: boolean;
}

/** Resolve `--panels` flag value into an ordered Panel list. */
export function resolvePanels(spec: string | undefined): Panel[] {
  if (!spec || spec.trim() === "" || spec === "all") return DEFAULT_PANELS;
  const ids = spec.split(",").map((s) => s.trim()).filter(Boolean);
  const out: Panel[] = [];
  for (const id of ids) {
    const p = PANEL_BY_ID[id];
    if (p) out.push(p);
  }
  return out;
}

const PANEL_RULE = "─".repeat(72);

function renderPanel(p: Panel, state: ClusterState, sandbox?: string): string {
  const heading = sandbox
    ? `┄ ${p.title}  ({cyan-fg}${sandbox}{/}) ┄`
    : `┄ ${p.title} ┄`;
  const body = p.render(state, sandbox ? { sandbox } : undefined);
  return `{bold}${heading}{/}\n${body}\n${PANEL_RULE}`;
}

/** Top-level dashboard renderer. Returns a single blessed-tag string. */
export function renderDashboard(
  state: ClusterState,
  opts: LayoutOpts = {},
): string {
  const panels = resolvePanels(opts.panels);
  if (panels.length === 0) return "{gray-fg}(no panels selected){/}";

  if (opts.perSandbox) {
    if (state.sandboxes.length === 0) {
      return panels.map((p) => renderPanel(p, state)).join("\n");
    }
    const sections: string[] = [];
    for (const sb of state.sandboxes) {
      sections.push(`{bold}{blue-fg}══ Sandbox: ${sb.name} ══{/}{/}`);
      for (const p of panels) {
        sections.push(renderPanel(p, state, sb.name));
      }
    }
    return sections.join("\n");
  }

  return panels.map((p) => renderPanel(p, state)).join("\n");
}
