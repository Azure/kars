// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Panel registry + layout.
 *
 * Renders the operator dashboard with a triage-first structure:
 *
 *   1. Header        — agent count, issue count.
 *   2. 🔥 Triage     — items with non-Ready/False/Unknown conditions or down
 *                      health, listed verbatim. Hidden when the cluster is
 *                      healthy.
 *   3. 📋 At-a-glance — one line per CRD, grouped by category. Optional
 *                      CRDs with zero instances are omitted.
 *   4. 📑 Detail      — full per-item rendering, scoped to non-empty panels
 *                      (or to user-requested panels via `--panels=...`).
 *
 * The legacy "dump every panel" behavior is preserved behind
 * `--panels=all` for muscle memory, and `--panels=triage` collapses to just
 * the header + triage section.
 */
import type { Panel, ClusterState, PanelCategory, PanelSummary } from "./types.js";
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
  /** Comma-separated list of panel ids; undefined → triage layout.
   *  Special values: "all" → legacy full dump, "triage" → header+triage only. */
  panels?: string;
  /** When true, render each sandbox's panel set as a vertical block (legacy). */
  perSandbox?: boolean;
}

/** Resolve `--panels` flag value into an ordered Panel list (legacy resolver). */
export function resolvePanels(spec: string | undefined): Panel[] {
  if (!spec || spec.trim() === "" || spec === "all") return DEFAULT_PANELS;
  if (spec === "triage") return [];
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

// ─────────────────────────────────────────────────────────────────────
//  Triage
// ─────────────────────────────────────────────────────────────────────

interface TriageItem {
  panel: string;
  name: string;
  namespace: string;
  conditionType: string;
  status: string;
  reason?: string;
  message?: string;
}

/** Walk the ClusterState and return every CRD condition that warrants
 *  operator attention (status="False" or "Unknown"). */
export function collectTriage(state: ClusterState): TriageItem[] {
  const out: TriageItem[] = [];
  const lists: Array<{ panel: string; items: { name: string; namespace: string; conditions: { type: string; status: string; reason?: string; message?: string }[] }[] }> = [
    { panel: "ClawPairing",     items: state.pairings },
    { panel: "McpServer",       items: state.mcpServers },
    { panel: "ToolPolicy",      items: state.toolPolicies },
    { panel: "InferencePolicy", items: state.inferencePolicies },
    { panel: "A2AAgent",        items: state.a2aAgents },
    { panel: "ClawMemory",      items: state.clawMemories },
    { panel: "ClawEval",        items: state.clawEvals },
  ];
  for (const { panel, items } of lists) {
    for (const it of items) {
      for (const c of it.conditions ?? []) {
        if (c.status === "False" || c.status === "Unknown") {
          out.push({
            panel,
            name: it.name,
            namespace: it.namespace,
            conditionType: c.type,
            status: c.status,
            reason: c.reason,
            message: c.message,
          });
        }
      }
    }
  }
  // Down/degraded sandboxes are also triage-worthy.
  for (const sb of state.sandboxes) {
    if (sb.health === "down" || sb.health === "degraded") {
      out.push({
        panel: "ClawSandbox",
        name: sb.name,
        namespace: `azureclaw-${sb.name}`,
        conditionType: "Health",
        status: sb.health === "down" ? "False" : "Unknown",
        reason: sb.health,
      });
    }
  }
  // Down/unknown providers.
  for (const p of state.providers.cluster) {
    if (p.status === "down" || p.status === "degraded") {
      out.push({
        panel: "Providers",
        name: p.label,
        namespace: "—",
        conditionType: "Provider",
        status: p.status === "down" ? "False" : "Unknown",
        reason: p.reason,
      });
    }
  }
  return out;
}

function renderTriage(items: TriageItem[]): string {
  if (items.length === 0) return "";
  const lines: string[] = [];
  lines.push(`{bold}{red-fg}🔥 Triage — ${items.length} ${items.length === 1 ? "issue" : "issues"}{/}{/}`);
  for (const t of items) {
    const color = t.status === "False" ? "red" : "yellow";
    const reason = t.reason ? ` ${t.reason}` : "";
    const msg = t.message ? `: ${t.message}` : "";
    lines.push(
      `  {${color}-fg}●{/} {bold}${t.panel}{/}/${t.name} ` +
      `{gray-fg}(${t.namespace}){/}  ${t.conditionType}=${t.status}${reason}${msg}`,
    );
  }
  lines.push(PANEL_RULE);
  return lines.join("\n");
}

// ─────────────────────────────────────────────────────────────────────
//  At-a-glance
// ─────────────────────────────────────────────────────────────────────

const CATEGORY_LABEL: Record<PanelCategory, string> = {
  agent:          "AGENTS",
  infrastructure: "INFRASTRUCTURE",
  optional:       "OPTIONAL FEATURES",
  internal:       "INTERNAL",
  providers:      "PROVIDERS",
};

const CATEGORY_ORDER: PanelCategory[] = [
  "agent",
  "infrastructure",
  "optional",
  "providers",
  "internal",
];

function categoryOf(p: Panel): PanelCategory {
  return p.category ?? "infrastructure";
}

/** Should this panel surface in the at-a-glance summary? */
function shouldSurface(p: Panel, summary: PanelSummary): boolean {
  const cat = categoryOf(p);
  // Always show agents and providers.
  if (cat === "agent" || cat === "providers") return true;
  // Optional + internal panels collapse when empty.
  if ((cat === "optional" || cat === "internal") && summary.total === 0) return false;
  return true;
}

function summaryBadge(s: PanelSummary): string {
  if (s.total === 0) return "{gray-fg}0{/}";
  const parts: string[] = [];
  parts.push(`${s.total}`);
  const tags: string[] = [];
  if (s.error)   tags.push(`{red-fg}${s.error}✕{/}`);
  if (s.warning) tags.push(`{yellow-fg}${s.warning}!{/}`);
  if (s.unknown) tags.push(`{gray-fg}${s.unknown}?{/}`);
  if (s.healthy) tags.push(`{green-fg}${s.healthy}✓{/}`);
  if (tags.length > 0) parts.push(`(${tags.join(" ")})`);
  return parts.join(" ");
}

function renderAtAGlance(state: ClusterState): string {
  const lines: string[] = [`{bold}📋 At a glance{/}`];
  for (const cat of CATEGORY_ORDER) {
    const panelsInCat = DEFAULT_PANELS.filter((p) => categoryOf(p) === cat);
    const visible: { panel: Panel; summary: PanelSummary }[] = [];
    for (const p of panelsInCat) {
      const summary = p.summarize ? p.summarize(state) : { total: 0, healthy: 0, warning: 0, error: 0, unknown: 0 };
      if (shouldSurface(p, summary)) visible.push({ panel: p, summary });
    }
    if (visible.length === 0) continue;
    lines.push(`  {dim}${CATEGORY_LABEL[cat]}{/}`);
    for (const { panel, summary } of visible) {
      const badge = summaryBadge(summary);
      const purpose = panel.purpose ? `  {gray-fg}— ${panel.purpose}{/}` : "";
      const detail = summary.detail ? `  {gray-fg}[${summary.detail}]{/}` : "";
      lines.push(`    ${panel.title.padEnd(18)} ${badge}${detail}${purpose}`);
    }
  }
  lines.push(PANEL_RULE);
  return lines.join("\n");
}

// ─────────────────────────────────────────────────────────────────────
//  Detail
// ─────────────────────────────────────────────────────────────────────

/** Pick which panels deserve a full detail render. */
function pickDetailPanels(state: ClusterState): Panel[] {
  // Always render agent + providers (small, signal-heavy).
  // Render optional/infrastructure panels only when they have items.
  // Internal panels stay hidden in default detail.
  const out: Panel[] = [];
  for (const p of DEFAULT_PANELS) {
    const cat = categoryOf(p);
    if (cat === "agent" || cat === "providers") {
      out.push(p);
      continue;
    }
    if (cat === "internal") continue;
    const total = p.summarize ? p.summarize(state).total : 0;
    if (total > 0) out.push(p);
  }
  return out;
}

// ─────────────────────────────────────────────────────────────────────
//  Top-level renderer
// ─────────────────────────────────────────────────────────────────────

function renderHeader(state: ClusterState, triageCount: number): string {
  const agents = state.sandboxes.length;
  const agentWord = agents === 1 ? "agent" : "agents";
  const issueWord = triageCount === 1 ? "issue" : "issues";
  const issueColor = triageCount === 0 ? "green" : (triageCount > 3 ? "red" : "yellow");
  return `{bold}AzureClaw Operator{/}  —  ${agents} ${agentWord}, ` +
         `{${issueColor}-fg}${triageCount} ${issueWord}{/}\n${PANEL_RULE}`;
}

/** Top-level dashboard renderer. Returns a single blessed-tag string. */
export function renderDashboard(
  state: ClusterState,
  opts: LayoutOpts = {},
): string {
  const spec = (opts.panels ?? "").trim();

  // ── Legacy modes preserved for muscle memory ──
  if (spec === "all") {
    return renderLegacy(DEFAULT_PANELS, state, opts);
  }
  if (spec && spec !== "triage") {
    // Explicit panel list — render only those, full detail.
    const picked = resolvePanels(spec);
    if (picked.length === 0) return "{gray-fg}(no panels selected){/}";
    return renderLegacy(picked, state, opts);
  }

  // ── Default: triage-first layout ──
  const triage = collectTriage(state);
  const sections: string[] = [];
  sections.push(renderHeader(state, triage.length));
  if (triage.length > 0) sections.push(renderTriage(triage));
  if (spec === "triage") return sections.join("\n");

  sections.push(renderAtAGlance(state));

  // Detail rendering — preserves per-sandbox grouping when requested.
  const detailPanels = pickDetailPanels(state);
  if (opts.perSandbox && state.sandboxes.length > 0) {
    for (const sb of state.sandboxes) {
      sections.push(`{bold}{blue-fg}══ Sandbox: ${sb.name} ══{/}{/}`);
      for (const p of detailPanels) sections.push(renderPanel(p, state, sb.name));
    }
  } else {
    for (const p of detailPanels) sections.push(renderPanel(p, state));
  }
  return sections.join("\n");
}

/** Old "dump every panel" rendering, kept for `--panels=all` and explicit
 *  `--panels=a,b,c` selections. */
function renderLegacy(panels: Panel[], state: ClusterState, opts: LayoutOpts): string {
  if (opts.perSandbox) {
    if (state.sandboxes.length === 0) {
      return panels.map((p) => renderPanel(p, state)).join("\n");
    }
    const sections: string[] = [];
    for (const sb of state.sandboxes) {
      sections.push(`{bold}{blue-fg}══ Sandbox: ${sb.name} ══{/}{/}`);
      for (const p of panels) sections.push(renderPanel(p, state, sb.name));
    }
    return sections.join("\n");
  }
  return panels.map((p) => renderPanel(p, state)).join("\n");
}
