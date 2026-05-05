// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Per-agent drill-in dialog (`i` key) — turns the operator from a flat
 * "all CRDs in the cluster" viewer into an agent-centric control plane.
 *
 * Shows everything attached to ONE sandbox (inference policy, tool policy,
 * MCP servers, ClawMemory bindings, A2A peers, ClawEval history, egress
 * counts) and offers three actions, all of which shell out to the existing
 * `azureclaw <subcommand>` CLI so validation and CRD shape stay in one
 * place:
 *
 *   [a] Attach...   pick a kind → minimal-form prompts → `azureclaw <kind> apply`
 *   [x] Detach      kubectl delete the highlighted attachment (confirm)
 *   [v] Run eval    prompts for suite → `azureclaw eval run`
 *   [r] Refresh     re-snapshot
 *   [q/Esc]         back to dashboard
 *
 * No new APIs are invented. The dialog is a thin TUI veneer over the
 * verbs that already exist as of S6 / S10 / P7.
 */

import blessed from "blessed";
import { execa } from "execa";
import type { SandboxInfo } from "../types.js";
import type {
  ClusterState, McpServerItem, ToolPolicyItem, InferencePolicyItem,
  A2AAgentItem, ClawMemoryItem, ClawEvalItem,
} from "../panels/types.js";
import { KubectlDataSource } from "../panels/datasource.js";

interface ActivityLog { log(msg: string): void; }

export interface AgentDetailContext {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  screen: any;
  sandbox: SandboxInfo;
  /** Optional pre-fetched cluster state. If omitted, the dialog fetches its own snapshot. */
  state?: ClusterState;
  kubeContext?: string;
  activityLog: ActivityLog;
  setDialogOpen: (open: boolean) => void;
  refresh: () => Promise<void>;
}

interface RowEntry {
  category: string;
  kind: string;       // CRD plural (kubectl alias)
  name: string;       // CR metadata.name
  namespace: string;
  summary: string;
}

const ATTACH_CHOICES = [
  { label: "MCP server", kind: "mcpserver", cmd: "mcp" },
  { label: "Inference policy", kind: "inferencepolicy", cmd: "inferencepolicy" },
  { label: "Tool policy", kind: "toolpolicy", cmd: "toolpolicy" },
  { label: "A2A peer", kind: "a2aagent", cmd: "a2a" },
  { label: "Memory binding", kind: "clawmemory", cmd: "memory" },
] as const;

type AttachKind = typeof ATTACH_CHOICES[number]["cmd"];

/** Build the rows shown in the drill-in list, filtered to one sandbox. */
export function collectAttachments(state: ClusterState, sandbox: string): RowEntry[] {
  const rows: RowEntry[] = [];

  // InferencePolicy — appliesToSandbox == sandbox
  for (const ip of state.inferencePolicies as InferencePolicyItem[]) {
    if (ip.appliesToSandbox && ip.appliesToSandbox !== sandbox) continue;
    if (!ip.appliesToSandbox && state.inferencePolicies.length > 1) continue;
    const pref = ip.modelPreference?.[0] ?? "(default)";
    const cap = ip.dailyTokens ? `${ip.dailyTokens}/d` : "uncapped";
    rows.push({
      category: "Inference policy", kind: "inferencepolicies",
      name: ip.name, namespace: ip.namespace,
      summary: `model=${pref}  tokens=${cap}  guard=${ip.guardrailFloor ?? "-"}`,
    });
  }

  // ToolPolicy — appliesToSandbox match
  for (const tp of state.toolPolicies as ToolPolicyItem[]) {
    if (tp.appliesToSandbox && tp.appliesToSandbox !== sandbox) continue;
    if (!tp.appliesToSandbox && state.toolPolicies.length > 1) continue;
    rows.push({
      category: "Tool policy", kind: "toolpolicies",
      name: tp.name, namespace: tp.namespace,
      summary: `rules=${tp.ruleCount ?? 0}  approval=${tp.approvalRequired ? "yes" : "no"}  rl=${tp.rateLimitPerMin ?? "-"}`,
    });
  }

  // McpServer — allowedSandboxes label match (we don't have label index here,
  // so show every MCP and let the operator filter — production wiring can refine).
  for (const m of state.mcpServers as McpServerItem[]) {
    rows.push({
      category: "MCP server", kind: "mcpservers",
      name: m.name, namespace: m.namespace,
      summary: `${m.url ?? "-"}  prod=${m.productionMode ? "yes" : "no"}  tools=${m.allowedToolCount ?? 0}`,
    });
  }

  // ClawMemory — sandboxRef
  for (const cm of state.clawMemories as ClawMemoryItem[]) {
    if (cm.sandboxRef && cm.sandboxRef !== sandbox) continue;
    rows.push({
      category: "Memory binding", kind: "clawmemories",
      name: cm.name, namespace: cm.namespace,
      summary: `store=${cm.storeName ?? "-"}  scope=${cm.scope ?? "-"}  bound=${cm.foundryBound ?? "?"}`,
    });
  }

  // A2AAgent — capability cluster, no sandbox filter (peer is shared)
  for (const a of state.a2aAgents as A2AAgentItem[]) {
    rows.push({
      category: "A2A peer", kind: "a2aagents",
      name: a.name, namespace: a.namespace,
      summary: `${a.endpointUrl ?? "-"}  card=${a.agentCardPublished ?? "?"}`,
    });
  }

  // ClawEval — sandboxRef
  for (const ev of state.clawEvals as ClawEvalItem[]) {
    if (ev.sandboxRef && ev.sandboxRef !== sandbox) continue;
    rows.push({
      category: "Eval history", kind: "clawevals",
      name: ev.name, namespace: ev.namespace,
      summary: `suite=${ev.suite ?? "-"}  last=${ev.lastScore ?? "-"}  next=${ev.nextScheduledAt ?? "-"}`,
    });
  }

  return rows;
}

/** Render the body content of the dialog. Exported for unit tests. */
export function formatBody(rows: RowEntry[]): string {
  if (rows.length === 0) return "  {gray-fg}(no attachments yet — press [a] to attach a policy / MCP / memory / A2A peer){/}";
  let lastCat = "";
  const out: string[] = [];
  for (const r of rows) {
    if (r.category !== lastCat) {
      out.push(`{cyan-fg}{bold}${r.category}{/bold}{/}`);
      lastCat = r.category;
    }
    out.push(`  • ${r.name}  {gray-fg}${r.summary}{/}`);
  }
  return out.join("\n");
}

export function openAgentDetailDialog(ctx: AgentDetailContext): void {
  const { screen, sandbox, activityLog, setDialogOpen, refresh } = ctx;
  setDialogOpen(true);

  let rows: RowEntry[] = ctx.state ? collectAttachments(ctx.state, sandbox.name) : [];
  let selected = 0;

  const dialog = blessed.box({
    parent: screen, top: "center", left: "center",
    width: "80%", height: "80%",
    border: { type: "line" },
    style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
    label: ` 🔬 Agent: ${sandbox.name}  (${sandbox.health}, ${sandbox.model}) `,
    tags: true, keys: true, mouse: true,
  });

  const header = blessed.box({
    parent: dialog, top: 0, left: 1, right: 1, height: 3,
    tags: true, style: { fg: "white", bg: "black" },
    content:
      `  Namespace:  ${sandbox.namespace}    Isolation: ${sandbox.isolation}    Channels: ${sandbox.channels || "-"}\n` +
      `  Status:     ${sandbox.status}    Restarts: ${sandbox.restarts}    Pod: ${sandbox.podName || "-"}\n`,
  });

  const list = blessed.list({
    parent: dialog, top: 4, left: 1, right: 1, bottom: 3,
    keys: false, mouse: true, tags: true,
    style: {
      fg: "white", bg: "black",
      selected: { fg: "black", bg: "cyan", bold: true },
    },
    items: [],
  });

  const footer = blessed.box({
    parent: dialog, bottom: 0, left: 1, right: 1, height: 2,
    tags: true, style: { fg: "white", bg: "black" },
    content:
      `  {bold}[a]{/bold} Attach  {bold}[x]{/bold} Detach  {bold}[v]{/bold} Run eval  {bold}[r]{/bold} Refresh  {bold}[q/Esc]{/bold} Back`,
  });
  void header; void footer;

  const renderList = () => {
    const items: string[] = [];
    let lastCat = "";
    const indexMap: number[] = []; // listIndex -> rows[] idx
    for (let i = 0; i < rows.length; i++) {
      const r = rows[i];
      if (r.category !== lastCat) {
        items.push(`{cyan-fg}{bold}═ ${r.category} ═{/bold}{/}`);
        indexMap.push(-1);
        lastCat = r.category;
      }
      items.push(`  ${r.name}   {gray-fg}${r.summary}{/}`);
      indexMap.push(i);
    }
    if (rows.length === 0) {
      items.push("");
      items.push("  {gray-fg}(no attachments yet){/}");
      items.push("  {gray-fg}Press [a] to attach a policy / MCP / memory / A2A peer.{/}");
      indexMap.push(-1, -1, -1);
    }
    list.setItems(items);
    (list as unknown as { _indexMap: number[] })._indexMap = indexMap;
    if (selected >= items.length) selected = Math.max(0, items.length - 1);
    // Skip header rows when selecting
    while (selected < items.length && indexMap[selected] === -1) selected++;
    list.select(selected);
    screen.render();
  };

  const cleanup = () => {
    screen.removeListener("keypress", onKey);
    dialog.destroy();
    screen.render();
    setTimeout(() => { setDialogOpen(false); }, 50);
  };

  const reload = async () => {
    activityLog.log(`{cyan-fg}↻ refreshing {bold}${sandbox.name}{/bold} attachments...{/}`);
    try {
      const ds = new KubectlDataSource(ctx.kubeContext);
      const fresh = await ds.fetch();
      rows = collectAttachments(fresh, sandbox.name);
      renderList();
    } catch (e: unknown) {
      const err = e as { message?: string };
      activityLog.log(`{red-fg}✗ refresh failed:{/} ${(err.message || "").substring(0, 100)}`);
    }
    await refresh();
  };

  // First fetch when no pre-supplied state.
  if (!ctx.state) {
    void reload();
  }

  const onKey = async (_ch: unknown, key: { name?: string; shift?: boolean }) => {
    if (!key.name) return;
    switch (key.name) {
      case "escape":
      case "q":
        cleanup();
        return;
      case "r":
        await reload();
        return;
      case "up":
      case "k": {
        const map = (list as unknown as { _indexMap: number[] })._indexMap || [];
        let i = selected - 1;
        while (i >= 0 && map[i] === -1) i--;
        if (i >= 0) { selected = i; list.select(selected); screen.render(); }
        return;
      }
      case "down":
      case "j": {
        const map = (list as unknown as { _indexMap: number[] })._indexMap || [];
        let i = selected + 1;
        while (i < map.length && map[i] === -1) i++;
        if (i < map.length) { selected = i; list.select(selected); screen.render(); }
        return;
      }
      case "a":
        await openAttachPicker(ctx, () => reload());
        return;
      case "x": {
        const map = (list as unknown as { _indexMap: number[] })._indexMap || [];
        const ri = map[selected];
        if (ri === undefined || ri < 0 || !rows[ri]) {
          activityLog.log(`{yellow-fg}Detach: select an attachment row first.{/}`);
          screen.render();
          return;
        }
        const r = rows[ri];
        cleanup();
        await runDetach(ctx, r);
        return;
      }
      case "v":
        cleanup();
        await openEvalPrompt(ctx);
        return;
    }
  };
  void rows; // (eslint: rows is captured by reload via collectAttachments)

  screen.on("keypress", onKey);
  renderList();
  list.focus();
  screen.render();
}

// ── Attach picker ─────────────────────────────────────────────────

function openAttachPicker(ctx: AgentDetailContext, after: () => Promise<void>): Promise<void> {
  return new Promise<void>((resolve) => {
    const { screen } = ctx;
    const picker = blessed.list({
      parent: screen, top: "center", left: "center",
      width: 50, height: ATTACH_CHOICES.length + 4,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black",
        selected: { fg: "black", bg: "cyan", bold: true } },
      label: " Attach... ",
      keys: true, tags: true, mouse: true,
      items: ATTACH_CHOICES.map((c) => `  ${c.label}  {gray-fg}(azureclaw ${c.cmd} apply){/}`),
    });
    picker.focus();
    screen.render();

    const onPickerKey = async (_ch: unknown, key: { name?: string }) => {
      if (key.name === "escape" || key.name === "q") {
        screen.removeListener("keypress", onPickerKey);
        picker.destroy(); screen.render(); resolve();
      } else if (key.name === "return" || key.name === "enter") {
        const idx = (picker as unknown as { selected: number }).selected;
        const choice = ATTACH_CHOICES[idx];
        screen.removeListener("keypress", onPickerKey);
        picker.destroy(); screen.render();
        if (choice) await runAttachForm(ctx, choice.cmd);
        await after();
        resolve();
      }
    };
    screen.on("keypress", onPickerKey);
  });
}

interface FieldDef { key: string; label: string; required: boolean; default?: string; auto?: boolean }

/**
 * Attach forms — minimum-viable input. Anything that can be derived from
 * the selected sandbox is `auto: true` and pre-filled; the operator only
 * sees prompts for fields that genuinely need a human decision.
 *
 * Memory binding is the extreme case: pass nothing and it works
 * (store=<sandbox>-mem, scope=agent:<sandbox>, no retention).
 */
const ATTACH_FIELDS: Record<AttachKind, FieldDef[]> = {
  mcp: [
    { key: "name", label: "Name", required: false, auto: true },
    { key: "url",  label: "URL (https://...)", required: true },
    { key: "production-mode", label: "Production mode (y/n)", required: false, default: "n" },
  ],
  inferencepolicy: [
    { key: "name", label: "Name", required: false, auto: true },
    { key: "primary-model", label: "Primary model", required: true, default: "gpt-4.1" },
    { key: "daily-tokens", label: "Daily token cap (blank = uncapped)", required: false },
  ],
  toolpolicy: [
    { key: "name", label: "Name", required: false, auto: true },
    { key: "applies-to-sandbox", label: "Applies-to sandbox", required: true, auto: true },
    { key: "approval-required", label: "Approval required (y/n)", required: false, default: "n" },
  ],
  a2a: [
    { key: "name", label: "Name", required: false, auto: true },
    { key: "endpoint-url", label: "Endpoint URL", required: true },
  ],
  memory: [
    { key: "name", label: "Name", required: false, auto: true },
    { key: "sandbox", label: "Sandbox", required: true, auto: true },
    { key: "store", label: "Foundry Memory Store name", required: false, auto: true },
    { key: "scope", label: "Scope key", required: false, auto: true },
  ],
};

/**
 * Derive smart defaults for a given attach kind + sandbox name.
 * Exported for unit tests.
 */
export function autoDefaults(kind: AttachKind, sandboxName: string): Record<string, string> {
  const base = sandboxName.toLowerCase().replace(/[^a-z0-9-]/g, "-");
  switch (kind) {
    case "mcp":
      return { name: `${base}-mcp` };
    case "inferencepolicy":
      return { name: `${base}-inference`, "applies-to-sandbox": sandboxName };
    case "toolpolicy":
      return { name: `${base}-tools`, "applies-to-sandbox": sandboxName };
    case "a2a":
      return { name: `${base}-peer` };
    case "memory":
      return {
        name: `${base}-memory`,
        sandbox: sandboxName,
        store: `${base}-mem`,
        scope: `agent:${sandboxName}`,
      };
  }
}

async function runAttachForm(ctx: AgentDetailContext, kind: AttachKind): Promise<void> {
  const fields = ATTACH_FIELDS[kind];
  const defaults = autoDefaults(kind, ctx.sandbox.name);
  const values: Record<string, string> = { ...defaults };

  // Only prompt for fields that genuinely need a human input. `auto: true`
  // fields are pre-filled from defaults and never asked.
  for (const f of fields) {
    if (f.auto && values[f.key]) continue;
    // eslint-disable-next-line no-await-in-loop
    const v = await prompt(ctx, `[${kind}] ${f.label}${f.default ? ` [${f.default}]` : ""}`);
    if (v === null) {
      ctx.activityLog.log(`{yellow-fg}attach ${kind}: cancelled.{/}`);
      return;
    }
    values[f.key] = v.trim() || (f.default ?? values[f.key] ?? "");
    if (f.required && !values[f.key]) {
      ctx.activityLog.log(`{red-fg}attach ${kind}: '${f.label}' is required.{/}`);
      return;
    }
  }

  const args: string[] = [kind, "apply", values["name"], "-n", "default"];
  // Map form keys → CLI flags.
  for (const [k, v] of Object.entries(values)) {
    if (k === "name" || !v) continue;
    if (k === "production-mode") {
      if (v.toLowerCase().startsWith("y")) args.push("--production-mode");
      continue;
    }
    if (k === "approval-required") {
      if (v.toLowerCase().startsWith("y")) args.push("--require-approval");
      continue;
    }
    args.push(`--${k}`, v);
  }

  ctx.activityLog.log(`{cyan-fg}⏳ azureclaw ${args.join(" ")}{/}`);
  ctx.screen.render();
  // Final confirm so the operator sees exactly what will be applied —
  // critical for the all-auto cases (e.g. memory) where we never prompted.
  const ok = await confirmYesNo(ctx, `Attach ${kind}/${values["name"]} to ${ctx.sandbox.name}?\n  azureclaw ${args.join(" ")}`);
  if (!ok) {
    ctx.activityLog.log(`{yellow-fg}attach ${kind}: cancelled at confirm.{/}`);
    return;
  }
  try {
    await execa("azureclaw", args, { stdio: "pipe" });
    ctx.activityLog.log(`{green-fg}✓ attached{/} ${kind}/${values["name"]} → ${ctx.sandbox.name}`);
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string };
    ctx.activityLog.log(`{red-fg}✗ attach failed:{/} ${(err.stderr || err.message || "").substring(0, 120)}`);
  }
}

// ── Detach ────────────────────────────────────────────────────────

async function runDetach(ctx: AgentDetailContext, r: RowEntry): Promise<void> {
  const ok = await confirmYesNo(ctx, `Detach ${r.kind}/${r.name}?`);
  if (!ok) return;
  ctx.activityLog.log(`{cyan-fg}⏳ kubectl delete ${r.kind} ${r.name} -n ${r.namespace || "default"}{/}`);
  ctx.screen.render();
  try {
    await execa("kubectl", ["delete", r.kind, r.name, "-n", r.namespace || "default"], { stdio: "pipe" });
    ctx.activityLog.log(`{green-fg}✓ detached{/} ${r.kind}/${r.name}`);
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string };
    ctx.activityLog.log(`{red-fg}✗ detach failed:{/} ${(err.stderr || err.message || "").substring(0, 120)}`);
  }
  await ctx.refresh();
}

// ── Eval ──────────────────────────────────────────────────────────

async function openEvalPrompt(ctx: AgentDetailContext): Promise<void> {
  const evaluator = await prompt(ctx, "Evaluator id (e.g. relevance, coherence) or blank for default");
  if (evaluator === null) return;
  const args = ["eval", ctx.sandbox.name];
  if (evaluator.trim()) args.push("--evaluator", evaluator.trim());
  ctx.activityLog.log(`{cyan-fg}⏳ azureclaw ${args.join(" ")}{/}`);
  ctx.screen.render();
  try {
    const { stdout } = await execa("azureclaw", args, { stdio: "pipe", timeout: 120_000 });
    ctx.activityLog.log(`{green-fg}✓ eval{/} ${ctx.sandbox.name}: ${stdout.split("\n")[0] || "(no stdout)"}`);
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string };
    ctx.activityLog.log(`{red-fg}✗ eval failed:{/} ${(err.stderr || err.message || "").substring(0, 160)}`);
  }
}

// ── Tiny prompt helper ────────────────────────────────────────────

function prompt(ctx: AgentDetailContext, label: string): Promise<string | null> {
  return new Promise((resolve) => {
    const { screen } = ctx;
    const box = blessed.textbox({
      parent: screen, top: "center", left: "center",
      width: Math.min(80, Math.max(40, label.length + 8)), height: 3,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: ` ${label} `,
      inputOnFocus: true,
    });
    box.focus(); screen.render();
    box.on("submit", (v: string) => { box.destroy(); screen.render(); resolve(v); });
    box.on("cancel", () => { box.destroy(); screen.render(); resolve(null); });
  });
}

function confirmYesNo(ctx: AgentDetailContext, label: string): Promise<boolean> {
  return new Promise((resolve) => {
    const { screen } = ctx;
    const dialog = blessed.box({
      parent: screen, top: "center", left: "center",
      width: Math.min(72, Math.max(50, label.length + 12)), height: 7,
      border: { type: "line" },
      style: { border: { fg: "yellow" }, fg: "white", bg: "black" },
      label: " ⚠  Confirm ", tags: true,
    });
    blessed.box({
      parent: dialog, top: 0, left: 2, right: 2, height: 2,
      tags: true, style: { fg: "white", bg: "black" },
      content: label,
    });
    let sel = 0;
    const btnYes = blessed.button({
      parent: dialog, top: 3, left: 8, width: 12, height: 1,
      content: "  [ Yes ]  ", tags: true, mouse: true,
      style: { fg: "white", bg: "green", focus: { bg: "green", fg: "white", bold: true } },
    });
    const btnNo = blessed.button({
      parent: dialog, top: 3, left: 28, width: 12, height: 1,
      content: "  [ No ]  ", tags: true, mouse: true,
      style: { fg: "white", bg: "gray", focus: { bg: "gray", fg: "white", bold: true } },
    });
    const updateBtns = () => {
      btnYes.style.bold = sel === 0;
      btnNo.style.bold = sel === 1;
      btnYes.style.bg = sel === 0 ? "green" : "black";
      btnNo.style.bg = sel === 1 ? "gray" : "black";
      screen.render();
    };
    const finish = (v: boolean) => {
      screen.removeListener("keypress", onConfKey);
      dialog.destroy(); screen.render(); resolve(v);
    };
    const onConfKey = (_ch: unknown, key: { name?: string }) => {
      if (key.name === "left" || key.name === "right" || key.name === "tab") {
        sel = sel === 0 ? 1 : 0; updateBtns();
      } else if (key.name === "y" || key.name === "Y") finish(true);
      else if (key.name === "n" || key.name === "N" || key.name === "escape") finish(false);
      else if (key.name === "return" || key.name === "enter") finish(sel === 0);
    };
    screen.on("keypress", onConfKey);
    btnYes.on("press", () => finish(true));
    btnNo.on("press", () => finish(false));
    updateBtns();
    btnYes.focus();
    screen.render();
  });
}

export const __test = { collectAttachments, formatBody, ATTACH_CHOICES, ATTACH_FIELDS, autoDefaults };
