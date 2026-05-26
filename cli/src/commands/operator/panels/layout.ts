// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Panel registry + layout.
 *
 * Default layout (S20+):
 *
 *   1. Header              — agent count, alert count.
 *   2. 🚨 Health alerts    — items with non-Ready/False/Unknown conditions
 *                            or down/degraded sandboxes/providers, listed
 *                            verbatim. Always shown — when the cluster is
 *                            healthy it renders "✓ No alerts".
 *   3. CRD sections        — one section per non-empty CRD type
 *                            (KarsSandbox, InferencePolicy, ToolPolicy,
 *                            KarsMemory, KarsEval, MCPServer, A2AAgent,
 *                            KarsPairing). Each section is a compact
 *                            table with [#], NAME, NAMESPACE, PHASE, AGE,
 *                            STATUS columns. Drill-in via numeric keys
 *                            opens a per-instance detail popup.
 *
 * Legacy "dump every panel" behavior is preserved behind `--panels=all`
 * for muscle memory; `--panels=triage` collapses to header + alerts only.
 */
import type { Panel, ClusterState, PanelCategory } from "./types.js";
import { bucketFromConditions } from "./util.js";
import { clawSandboxPanel } from "./karssandbox.js";
import { clawPairingPanel } from "./karspairing.js";
import { mcpServerPanel } from "./mcpserver.js";
import { toolPolicyPanel } from "./toolpolicy.js";
import { inferencePolicyPanel } from "./inferencepolicy.js";
import { a2aAgentPanel } from "./a2aagent.js";
import { clawMemoryPanel } from "./karsmemory.js";
import { clawEvalPanel } from "./karseval.js";
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
//  Health alerts (formerly "Triage")
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
    { panel: "KarsPairing",     items: state.pairings },
    { panel: "McpServer",       items: state.mcpServers },
    { panel: "ToolPolicy",      items: state.toolPolicies },
    { panel: "InferencePolicy", items: state.inferencePolicies },
    { panel: "A2AAgent",        items: state.a2aAgents },
    { panel: "KarsMemory",      items: state.clawMemories },
    { panel: "KarsEval",        items: state.clawEvals },
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
        panel: "KarsSandbox",
        name: sb.name,
        namespace: `kars-${sb.name}`,
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

function renderHealthAlerts(items: TriageItem[]): string {
  const header = `{bold}🚨 Health alerts{/}`;
  if (items.length === 0) {
    return `${header}\n  {green-fg}✓ No alerts{/}\n${PANEL_RULE}`;
  }
  const lines: string[] = [];
  lines.push(`{bold}{red-fg}🚨 Health alerts — ${items.length} ${items.length === 1 ? "issue" : "issues"}{/}{/}`);
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
//  Per-CRD-type compact sections (default layout)
// ─────────────────────────────────────────────────────────────────────

/** A single row in a CRD section's compact table. */
export interface CrdRow {
  index: number;          // 1-based, used for drill-in keybinding
  kind: string;           // CRD kind (e.g. "KarsSandbox") — same as section title
  name: string;
  namespace: string;
  phase: string;          // bucketed: healthy / degraded / down / pending / warning / error / unknown
  age: string;
  status: string;         // one-line key-status summary specific to the CRD type
}

/** Phase string + blessed color for a CRD row. */
function phaseColor(phase: string): string {
  switch (phase) {
    case "healthy": return "green";
    case "degraded":
    case "warning":
    case "pending": return "yellow";
    case "down":
    case "error":   return "red";
    case "dormant": return "blue";
    default:        return "gray";
  }
}

/** Map a list of CrdItem conditions onto a phase bucket label. */
function phaseFromConditions(conds: import("./types.js").CrdCondition[] | undefined): string {
  switch (bucketFromConditions(conds)) {
    case "healthy": return "healthy";
    case "warning": return "warning";
    case "error":   return "error";
    default:        return "unknown";
  }
}

/** Best-effort one-line status per CRD type. */
function buildRows(state: ClusterState): CrdRow[] {
  const rows: CrdRow[] = [];
  let i = 0;
  const next = () => ++i;

  // KarsSandbox — uses SandboxInfo (no conditions; health field is authoritative).
  for (const s of state.sandboxes) {
    const rk = s.runtimeKind || "OpenClaw";
    const status = `${s.model || "-"}  ·  ${rk}  ·  ${s.isolation || "-"}  ·  ${s.role}`;
    rows.push({
      index: next(),
      kind: "KarsSandbox",
      name: s.name,
      namespace: s.namespace || `kars-${s.name}`,
      phase: s.health,
      age: s.age || "-",
      status,
    });
  }

  // KarsMemory
  for (const m of state.clawMemories) {
    const parts: string[] = [];
    if (m.sandboxRef) parts.push(`sandbox=${m.sandboxRef}`);
    if (m.storeName) parts.push(`store=${m.storeName}`);
    if (m.scope) parts.push(`scope=${m.scope}`);
    if (m.foundryBound) parts.push(`binding=${m.foundryBound}`);
    rows.push({
      index: next(), kind: "KarsMemory",
      name: m.name, namespace: m.namespace,
      phase: phaseFromConditions(m.conditions),
      age: m.age || "-",
      status: parts.join("  ·  ") || "—",
    });
  }

  // KarsEval
  for (const e of state.clawEvals) {
    const parts: string[] = [];
    if (e.sandboxRef) parts.push(`sandbox=${e.sandboxRef}`);
    if (e.suite) parts.push(`suite=${e.suite}`);
    if (e.lastScore) parts.push(`score=${e.lastScore}`);
    if (e.schedule) parts.push(`sched=${e.schedule}`);
    rows.push({
      index: next(), kind: "KarsEval",
      name: e.name, namespace: e.namespace,
      phase: phaseFromConditions(e.conditions),
      age: e.age || "-",
      status: parts.join("  ·  ") || "—",
    });
  }

  // KarsPairing — the pairing reconciler emits `status.phase` (PendingPairing,
  // Active, Expired, Revoked) but does NOT publish conditions, so a pure
  // condition-based bucketing always returns "unknown". Map the phase
  // string directly to a health bucket.
  for (const p of state.pairings) {
    const parts: string[] = [];
    parts.push(`${p.agentA ?? "?"} ↔ ${p.agentB ?? "?"}`);
    if (p.state) parts.push(`state=${p.state}`);
    if (p.trust) parts.push(`trust=${p.trust}`);
    let phase: string;
    if (p.conditions && p.conditions.length > 0) {
      phase = phaseFromConditions(p.conditions);
    } else {
      phase = p.state === "Active" ? "healthy"
            : p.state === "PendingPairing" ? "pending"
            : p.state === "Expired" ? "warning"
            : p.state === "Revoked" ? "error"
            : "unknown";
    }
    rows.push({
      index: next(), kind: "KarsPairing",
      name: p.name, namespace: p.namespace,
      phase,
      age: p.age || "-",
      status: parts.join("  ·  "),
    });
  }

  // InferencePolicy
  for (const ip of state.inferencePolicies) {
    const parts: string[] = [];
    parts.push(`appliesTo=${ip.appliesToSandbox ?? "<all>"}`);
    if (ip.modelPreference?.length) parts.push(`model=${ip.modelPreference[0]}`);
    if (ip.dailyTokens !== undefined) parts.push(`daily=${ip.dailyTokens}`);
    if (ip.guardrailFloor) parts.push(`floor=${ip.guardrailFloor}`);
    rows.push({
      index: next(), kind: "InferencePolicy",
      name: ip.name, namespace: ip.namespace,
      phase: phaseFromConditions(ip.conditions),
      age: ip.age || "-",
      status: parts.join("  ·  "),
    });
  }

  // ToolPolicy
  for (const t of state.toolPolicies) {
    const parts: string[] = [];
    parts.push(`appliesTo=${t.appliesToSandbox ?? "<all>"}`);
    if (t.ruleCount !== undefined) parts.push(`rules=${t.ruleCount}`);
    if (t.rateLimitPerMin !== undefined) parts.push(`rate=${t.rateLimitPerMin}/min`);
    rows.push({
      index: next(), kind: "ToolPolicy",
      name: t.name, namespace: t.namespace,
      phase: phaseFromConditions(t.conditions),
      age: t.age || "-",
      status: parts.join("  ·  "),
    });
  }

  // McpServer
  for (const m of state.mcpServers) {
    const parts: string[] = [];
    if (m.url) parts.push(`url=${m.url}`);
    if (m.productionMode !== undefined) parts.push(`prod=${m.productionMode ? "yes" : "no"}`);
    if (m.jwksSecretPresent) parts.push(`jwks=<${m.jwksSecretPresent}>`);
    if (m.allowedToolCount !== undefined) parts.push(`tools=${m.allowedToolCount}`);
    rows.push({
      index: next(), kind: "MCPServer",
      name: m.name, namespace: m.namespace,
      phase: phaseFromConditions(m.conditions),
      age: m.age || "-",
      status: parts.join("  ·  ") || "—",
    });
  }

  // A2AAgent
  for (const a of state.a2aAgents) {
    const parts: string[] = [];
    if (a.endpointUrl) parts.push(`endpoint=${a.endpointUrl}`);
    if (a.productionMode !== undefined) parts.push(`prod=${a.productionMode ? "yes" : "no"}`);
    if (a.agentCardPublished) parts.push(`card=${a.agentCardPublished}`);
    rows.push({
      index: next(), kind: "A2AAgent",
      name: a.name, namespace: a.namespace,
      phase: phaseFromConditions(a.conditions),
      age: a.age || "-",
      status: parts.join("  ·  ") || "—",
    });
  }

  return rows;
}

/** Section order — only sections with rows are rendered. */
const SECTION_ORDER = [
  "KarsSandbox",
  "InferencePolicy",
  "ToolPolicy",
  "KarsMemory",
  "KarsEval",
  "MCPServer",
  "A2AAgent",
  "KarsPairing",
] as const;

const SECTION_PURPOSE: Record<string, string> = {
  KarsSandbox:     "the agents themselves — pod, runtime, model, isolation",
  InferencePolicy: "model preference, daily token caps, guardrail floor",
  ToolPolicy:      "allow/deny tools, approval gates, rate limits",
  KarsMemory:      "Foundry Memory Store binding — scope, retention",
  KarsEval:        "reproducible eval runs — pinned image+config",
  MCPServer:       "MCP servers reachable from sandboxes",
  A2AAgent:        "A2A ingress + signing-key trust anchors",
  KarsPairing:     "controller-managed handshake state",
};

function padTag(s: string, width: number, color?: string): string {
  const trimmed = s.length > width ? s.substring(0, width - 1) + "…" : s;
  const padded = trimmed + " ".repeat(Math.max(0, width - trimmed.length));
  return color ? `{${color}-fg}${padded}{/}` : padded;
}

function renderCrdSection(kind: string, rows: CrdRow[]): string {
  const purpose = SECTION_PURPOSE[kind] ? `  {gray-fg}— ${SECTION_PURPOSE[kind]}{/}` : "";
  const lines: string[] = [];
  lines.push(`{bold}{blue-fg}═══ ${kind} (${rows.length}) ═══{/}{/}${purpose}`);
  lines.push(
    `  {gray-fg}${"#".padEnd(4)}${"NAME".padEnd(28)} ${"NAMESPACE".padEnd(22)} ${"PHASE".padEnd(10)} ${"AGE".padEnd(6)} STATUS{/}`,
  );
  for (const r of rows) {
    const idx = `[${r.index}]`.padEnd(4);
    const phase = padTag(r.phase, 10, phaseColor(r.phase));
    lines.push(
      `  ${idx}${padTag(r.name, 28)} ${padTag(r.namespace, 22)} ${phase} ${padTag(r.age, 6)} ${r.status}`,
    );
  }
  return lines.join("\n");
}

/** Build all rows + render every non-empty CRD-type section. Also returns
 *  the flat row list so callers (e.g. drill-in) can map index → item. */
export function renderCrdSections(state: ClusterState): { body: string; rows: CrdRow[] } {
  const all = buildRows(state);
  const byKind = new Map<string, CrdRow[]>();
  for (const r of all) {
    if (!byKind.has(r.kind)) byKind.set(r.kind, []);
    byKind.get(r.kind)!.push(r);
  }
  const sections: string[] = [];
  for (const kind of SECTION_ORDER) {
    const rows = byKind.get(kind);
    if (!rows || rows.length === 0) continue;
    sections.push(renderCrdSection(kind, rows));
  }
  if (sections.length === 0) {
    sections.push(`{gray-fg}(no CRD instances){/}`);
  }
  return { body: sections.join("\n\n"), rows: all };
}

// ─────────────────────────────────────────────────────────────────────
//  Legacy detail (preserved for `--panels=all` and `--per-sandbox`)
// ─────────────────────────────────────────────────────────────────────

function categoryOf(p: Panel): PanelCategory {
  return p.category ?? "infrastructure";
}

/** Pick which panels deserve a full detail render in legacy/per-sandbox mode. */
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
  return `{bold}Kars Operator{/}  —  ${agents} ${agentWord}, ` +
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

  // ── Default: health-alerts-first, per-CRD-type sections ──
  const triage = collectTriage(state);
  const sections: string[] = [];
  sections.push(renderHeader(state, triage.length));
  sections.push(renderHealthAlerts(triage));
  if (spec === "triage") return sections.join("\n");

  // Per-sandbox grouping is a legacy mode kept for muscle memory.
  if (opts.perSandbox && state.sandboxes.length > 0) {
    const detailPanels = pickDetailPanels(state);
    for (const sb of state.sandboxes) {
      sections.push(`{bold}{blue-fg}══ Sandbox: ${sb.name} ══{/}{/}`);
      for (const p of detailPanels) sections.push(renderPanel(p, state, sb.name));
    }
    return sections.join("\n");
  }

  const { body } = renderCrdSections(state);
  sections.push(body);
  return sections.join("\n\n");
}

// ─────────────────────────────────────────────────────────────────────
//  Drill-in detail renderer
// ─────────────────────────────────────────────────────────────────────

/** Render the verbose details of a single CRD item (drill-in dialog).
 *  Returns a friendly placeholder if the item can't be resolved. */
export function renderCrdItemDetail(state: ClusterState, kind: string, name: string, namespace: string): string {
  const empty = (): ClusterState => ({
    sandboxes: [], pairings: [], mcpServers: [], toolPolicies: [],
    inferencePolicies: [], a2aAgents: [], clawMemories: [], clawEvals: [],
    providers: { perSandbox: new Map(), cluster: [] },
  });
  const match = <T extends { name: string; namespace: string }>(items: T[]): T | undefined =>
    items.find((it) => it.name === name && it.namespace === namespace);

  const heading = (title: string, body: string) =>
    `{bold}{blue-fg}${title}{/}{/}\n{gray-fg}${name} (${namespace}){/}\n\n${body}`;

  switch (kind) {
    case "KarsSandbox": {
      const sb = state.sandboxes.find((s) => s.name === name);
      if (!sb) return `{red-fg}sandbox '${name}' not found{/}`;
      const sub = empty(); sub.sandboxes = [sb];
      return heading("KarsSandbox detail", clawSandboxPanel.render(sub));
    }
    case "KarsMemory": {
      const m = match(state.clawMemories);
      if (!m) return `{red-fg}KarsMemory '${name}' not found{/}`;
      const sub = empty(); sub.clawMemories = [m];
      return heading("KarsMemory detail", clawMemoryPanel.render(sub));
    }
    case "KarsEval": {
      const e = match(state.clawEvals);
      if (!e) return `{red-fg}KarsEval '${name}' not found{/}`;
      const sub = empty(); sub.clawEvals = [e];
      return heading("KarsEval detail", clawEvalPanel.render(sub));
    }
    case "KarsPairing": {
      const p = match(state.pairings);
      if (!p) return `{red-fg}KarsPairing '${name}' not found{/}`;
      const sub = empty(); sub.pairings = [p];
      return heading("KarsPairing detail", clawPairingPanel.render(sub));
    }
    case "InferencePolicy": {
      const ip = match(state.inferencePolicies);
      if (!ip) return `{red-fg}InferencePolicy '${name}' not found{/}`;
      const sub = empty(); sub.inferencePolicies = [ip];
      return heading("InferencePolicy detail", inferencePolicyPanel.render(sub));
    }
    case "ToolPolicy": {
      const t = match(state.toolPolicies);
      if (!t) return `{red-fg}ToolPolicy '${name}' not found{/}`;
      const sub = empty(); sub.toolPolicies = [t];
      return heading("ToolPolicy detail", toolPolicyPanel.render(sub));
    }
    case "MCPServer": {
      const m = match(state.mcpServers);
      if (!m) return `{red-fg}MCPServer '${name}' not found{/}`;
      const sub = empty(); sub.mcpServers = [m];
      return heading("MCPServer detail", mcpServerPanel.render(sub));
    }
    case "A2AAgent": {
      const a = match(state.a2aAgents);
      if (!a) return `{red-fg}A2AAgent '${name}' not found{/}`;
      const sub = empty(); sub.a2aAgents = [a];
      return heading("A2AAgent detail", a2aAgentPanel.render(sub));
    }
    default:
      return `{red-fg}unknown CRD kind '${kind}'{/}`;
  }
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
