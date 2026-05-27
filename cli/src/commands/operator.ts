// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * kars Operator TUI — live terminal dashboard for managing sandboxes.
 *
 * The rendered layout, key bindings, and status-bar copy are documented
 * alongside their extracted data in `./operator/keymap.ts` (per plan
 * §4.2 + §6 item 12 Phase 0 decomposition). See `BINDINGS` in that file
 * for the canonical reference.
 */

import { Command } from "commander";
import { execa } from "execa";
import blessed from "blessed";
import contrib from "blessed-contrib";
import { listSecretVariants } from "../config.js";
import {
  statusBarForAgents,
  statusBarForTopology,
  statusBarForCluster,
} from "./operator/keymap.js";
import type {
  SandboxInfo,
  EgressDomain,
  SecurityState,
  ClusterHealth,
  MeshHealth,
} from "./operator/types.js";
import { timeSince, kctl, platformTag, clusterOriginTag } from "./operator/helpers.js";
import { fetchSandboxes } from "./operator/fetchers/sandboxes.js";
import {
  fetchEgressDomains,
  fetchSecurityState,
  fetchAgtQuick,
} from "./operator/fetchers/security.js";
import {
  fetchMeshHealth,
  fetchClusterHealth,
} from "./operator/fetchers/cluster.js";
import { createActions } from "./operator/actions.js";
import { renderCluster as _renderCluster } from "./operator/render/cluster.js";
import { renderTopology as _renderTopology } from "./operator/render/topology.js";
import {
  renderSecurity as _renderSecurity,
  renderAGT as _renderAGT,
  renderAGTFull as _renderAGTFull,
} from "./operator/render/security.js";
import { renderHeader as _renderHeader } from "./operator/render/header.js";
import { openSpawnDialog } from "./operator/dialogs/spawn.js";
import { deleteSelectedAgent as _deleteSelectedAgent } from "./operator/dialogs/delete.js";
import { connectToAgent as _connectToAgent } from "./operator/dialogs/connect.js";
import { openAgentDetailDialog } from "./operator/dialogs/agent_detail.js";
import { openEgressDrawer } from "./operator/dialogs/egress.js";
import { createPanelsOverlay } from "./operator/panels_overlay.js";

// ── Command ─────────────────────────────────────────────────────────

export function operatorCommand(): Command {
  const cmd = new Command("operator");

  cmd
    .description("Live operator dashboard — manage all sandboxes from one screen")
    .option("--refresh <seconds>", "Auto-refresh interval", "10")
    .option("--context <name>", "Kubernetes context to use")
    .option("--dev", "Dev mode — discover Docker containers instead of K8s pods")
    .option("--panels <list>", "CRD panel ids (comma-separated). Default = all.")
    .option("--per-sandbox", "Group panels vertically per sandbox-name")
    .option("--snapshot", "Render one snapshot to stdout and exit")
    .action(async (options) => {
      const isDevMode = !!options.dev;
      const defaultRefresh = isDevMode ? 3 : 10;
      const refreshInterval = (options.refresh ? parseInt(options.refresh, 10) : defaultRefresh) * 1000;
      const panelOpts = { panels: options.panels, perSandbox: !!options.perSandbox };
      if (options.snapshot) {
        const { runSnapshot } = await import("./operator/panels_snapshot.js");
        await runSnapshot({ kubeContext: options.context, ...panelOpts });
        return;
      }
      await startDashboard(refreshInterval, options.context, isDevMode, panelOpts);
    });

  return cmd;
}

// Helper to build kubectl args with optional context

async function startDashboard(refreshInterval: number, kubeContext?: string, devMode = false, panelOpts: { panels?: string; perSandbox?: boolean } = {}) {
  // ── Resolve cluster ───────────────────────────────────────────────
  let clusterName = devMode ? "docker (dev)" : "unknown";
  if (!devMode) {
    try {
      const { stdout } = await execa("kubectl", kctl([
        "config", "current-context",
      ], kubeContext), { stdio: "pipe" });
      clusterName = stdout.trim();
    } catch { /* offline */ }
  }
  // In dev mode, also check if kubectl is reachable for unified view
  let hasKubectl = !devMode;
  if (devMode) {
    try {
      await execa("kubectl", ["version", "--client", "--short"], { stdio: "pipe", timeout: 3000 });
      try {
        const { stdout } = await execa("kubectl", kctl(["config", "current-context"], kubeContext), { stdio: "pipe", timeout: 5000 });
        clusterName = `docker + ${stdout.trim()}`;
        hasKubectl = true;
      } catch { /* no context */ }
    } catch { /* no kubectl */ }
  }

  const screen = blessed.screen({
    smartCSR: true,
    title: "kars Operator",
    fullUnicode: true,
  });

  // ── Layout (12×12 grid) ───────────────────────────────────────────

  const grid = new contrib.grid({ rows: 12, cols: 12, screen });

  // Row 0: Header
  const header = grid.set(0, 0, 1, 12, blessed.box, {
    tags: true,
    wrap: false,
    style: { fg: "white", bold: true },
  });

  // Rows 1–4: Agent table (full width)
  const agentTable = grid.set(1, 0, 4, 12, contrib.table, {
    keys: false,
    vi: false,
    fg: "white",
    label: " Agents  [↑↓ navigate] ",
    columnSpacing: 1,
    columnWidth: [3, 32, 14, 10, 14, 10, 5, 6, 18],
    interactive: true,
    style: {
      border: { fg: "cyan" },
      header: { fg: "cyan", bold: true },
      cell: { selected: { bg: "blue", fg: "white" } },
    },
  });

  // Rows 5–9: Security panel (cols 0–3)
  const securityBox = grid.set(5, 0, 5, 4, blessed.box, {
    tags: true,
    label: " 🔒 Security Controls ",
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    style: { border: { fg: "magenta" }, fg: "white" },
    padding: { left: 1 },
  });

  // Rows 5–9: Egress list (cols 4–8) — uses blessed.list for colored tags
  const egressList = grid.set(5, 4, 5, 4, blessed.list, {
    keys: false,
    vi: false,
    tags: true,
    label: " Egress  [a]pprove [d]eny [e]nforce ",
    interactive: true,
    mouse: true,
    scrollable: true,
    style: {
      border: { fg: "yellow" },
      fg: "white",
      selected: { bg: "yellow", fg: "black" },
    },
  });

  // Rows 5–6: Activity log (cols 8–11)
  const activityLog = grid.set(5, 8, 2, 4, contrib.log, {
    fg: "green",
    label: " Log ",
    tags: true,
    style: { border: { fg: "green" } },
    bufferLength: 80,
  });

  // Rows 7–9: AGT Governance panel (cols 8–11)
  const agtPanel = grid.set(7, 8, 3, 4, blessed.box, {
    tags: true,
    label: " 🛡  AGT Governance ",
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    style: { border: { fg: "blue" }, fg: "white" },
    padding: { left: 1 },
  });

  // Rows 10–11: Status bar
  const statusBar = grid.set(10, 0, 2, 12, blessed.box, {
    tags: true,
    style: { fg: "white", bg: "default" },
  });

  // ── Cluster Health Overlay (hidden by default) ──────────────────

  // Agent-detail panels (the bottom row: security, egress, log, AGT)
  const agentDetailPanels = [securityBox, egressList, activityLog, agtPanel];

  // Cluster overlay: node table (left) + cluster info (right)
  const clusterNodeBox = blessed.box({
    parent: screen, hidden: true,
    top: "42%", left: 0, width: "66%", height: "42%",
    tags: true, scrollable: true, alwaysScroll: true, mouse: true,
    label: " 🖥  Nodes ",
    border: { type: "line" },
    style: { border: { fg: "blue" }, fg: "white" },
    padding: { left: 1 },
  });

  const clusterInfoBox = blessed.box({
    parent: screen, hidden: true,
    top: "42%", left: "66%", width: "34%", height: "42%",
    tags: true, scrollable: true, alwaysScroll: true, mouse: true,
    label: " ⚡ Cluster Status ",
    border: { type: "line" },
    style: { border: { fg: "blue" }, fg: "white" },
    padding: { left: 1 },
  });

  const clusterPanels = [clusterNodeBox, clusterInfoBox];

  // Topology view (takes full content area when active — rows 1-9)
  const topologyBox = grid.set(1, 0, 9, 12, blessed.box, {
    tags: true,
    label: " 🔗 Agent Topology ",
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    style: { border: { fg: "cyan" }, fg: "white" },
    padding: { left: 1 },
    hidden: true,
  });

  // AGT full-detail overlay (shown with 'g' key)
  const agtOverlay = blessed.box({
    parent: screen,
    hidden: true,
    top: 1, left: 0, right: 0, bottom: 1,
    tags: true,
    scrollable: true,
    alwaysScroll: true,
    mouse: true,
    label: " 🛡 AGT Governance ",
    border: { type: "line" },
    style: { border: { fg: "blue" }, fg: "white", bg: "default" },
    padding: { left: 1, right: 1 },
  });

  // S14 panels overlay (modular per-CRD dashboard) — body in panels_overlay.ts
  const panelsOverlay = createPanelsOverlay({
    screen,
    kubeContext,
    panelOpts,
    dialogOpen: () => dialogOpen,
  });

  // ── State ─────────────────────────────────────────────────────────

  let sandboxes: SandboxInfo[] = [];
  let egressByAgent: Map<string, EgressDomain[]> = new Map();
  let securityStates: Map<string, SecurityState> = new Map();
  let clusterData: ClusterHealth | null = null;
  let meshHealth: MeshHealth | null = null;
  let viewMode: "agents" | "cluster" | "topology" = "agents";
  let focusedPanel: "agents" | "egress" = "agents";
  let refreshCount = 0;
  let isRefreshing = false;
  let dialogOpen = false;
  let lastLogState = "";
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let connectedToAgent = false;  // suppress refresh rendering while connected
  let agtOverlayOpen = false;

  // Tiered refresh: not everything needs to refresh every cycle.
  // AKS: Sandboxes every cycle (10s). Security/egress every 3rd (30s). Cluster every 6th (60s).
  // Dev:  Everything every cycle (3s) — local Docker, negligible overhead.
  const TIER_DETAIL = devMode ? 1 : 3;
  const TIER_CLUSTER = devMode ? 2 : 6;

  /** Egress domains for the currently selected agent. */
  function selectedEgressDomains(): EgressDomain[] {
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return [];
    return egressByAgent.get(sb.name) || [];
  }

  /** Total egress domain count across all agents. */
  function totalEgressCount(): number {
    let n = 0;
    for (const domains of egressByAgent.values()) n += domains.length;
    return n;
  }

  // Spinner
  const spinFrames = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
  let spinIdx = 0;
  let spinTimer: ReturnType<typeof setInterval> | null = null;

  function startSpinner() {
    if (spinTimer) return;
    spinTimer = setInterval(() => {
      spinIdx = (spinIdx + 1) % spinFrames.length;
      renderHeader();
      screen.render();
    }, 80);
  }
  function stopSpinner() {
    if (spinTimer) { clearInterval(spinTimer); spinTimer = null; }
  }

  // ── Actions (extracted to operator/actions.ts) ────────────────────
  // Slice 5c.1: approveDomain/denyDomain removed — the egress
  // allowlist is now signed and published by the controller, not
  // mutated by operator key chords. `a`/`A` keys deleted; `d` keeps
  // its delete-agent meaning. `e` (enforce) and `l` (learn) flip the
  // CRD `egressMode` field; the controller-published allowlist
  // remains the authority on which hosts L7 actually permits.
  const { enforceEgress, learnEgress } = createActions({
    getSandboxes: () => sandboxes,
    activityLog,
    kubeContext,
  });

  // ── Rendering ─────────────────────────────────────────────────────

  // Thin wrappers — bodies extracted to operator/render/header.ts (S15.e.5c)
  function renderHeader(): void {
    _renderHeader({
      sandboxes,
      clusterData,
      meshHealth,
      isRefreshing,
      spinFrames,
      spinIdx,
      clusterName,
      viewMode,
      totalEgressCount: totalEgressCount(),
      header,
      screen,
    });
  }

  function renderSecurity(): void {
    _renderSecurity({ agentTable, sandboxes, securityStates, securityBox, agtPanel });
  }

  function renderAGTFull(sb: SandboxInfo): string {
    return _renderAGTFull(sb, sandboxes, securityStates);
  }

  function renderAGT(): void {
    _renderAGT({ agentTable, sandboxes, securityStates, securityBox, agtPanel });
  }

  function renderTopology(): void {
    _renderTopology({ sandboxes, securityStates, topologyBox });
  }

  function renderCluster(): void {
    _renderCluster({ clusterData, clusterNodeBox, clusterInfoBox });
  }

  function render() {
    // Toggle panel visibility based on view mode
    if (viewMode === "cluster") {
      for (const p of agentDetailPanels) (p as any).hide();
      for (const p of clusterPanels) (p as any).show();
      (topologyBox as any).hide();
      (agentTable as any).show();
      renderCluster();
    } else if (viewMode === "topology") {
      for (const p of clusterPanels) (p as any).hide();
      for (const p of agentDetailPanels) (p as any).hide();
      (agentTable as any).hide();
      (topologyBox as any).show();
      renderTopology();
    } else {
      for (const p of clusterPanels) (p as any).hide();
      for (const p of agentDetailPanels) (p as any).show();
      (topologyBox as any).hide();
      (agentTable as any).show();
    }

    // Agent table — with tree hierarchy, handoff state, and runtime source
    const agentData = sandboxes.map((s) => {
      let hIcon: string;
      if (s.handoffState === "dormant") {
        hIcon = "~";
      } else if (s.handoffState === "active-successor") {
        hIcon = "^";
      } else if (s.handoffState === "returning") {
        hIcon = "<";
      } else {
        hIcon = s.health === "healthy" ? "*" :
                s.health === "degraded" ? "!" :
                s.health === "down" ? "x" :
                s.health === "pending" ? "o" : "?";
      }
      const restartStr = s.restarts > 0 ? ` R:${s.restarts}` : "";
      // Runtime tag — ASCII only for reliable column alignment
      const rtTag = s.runtime === "docker" ? "D " : "C ";
      // Tree prefix + handoff state annotations
      let displayName = s.name;
      if (s.handoffState === "dormant") {
        displayName = `${rtTag}${s.name} (offloaded)`;
      } else if (s.handoffState === "active-successor") {
        displayName = `${rtTag}${s.name} (handoff)`;
      } else if (s.handoffState === "returning") {
        displayName = `${rtTag}${s.name} (returning)`;
      } else if (s.role === "sub-agent") {
        displayName = `${rtTag} └ ${s.name}`;
      } else {
        displayName = `${rtTag}${s.name}`;
      }
      const statusStr = s.handoffState === "dormant"
        ? "Offloaded"
        : s.handoffState === "returning"
          ? "Returning"
          : `${s.status}${restartStr}`;
      // Short tag for the runtime kind so the column stays narrow:
      // OpenClaw → "OC", OpenAIAgents → "OAI", MicrosoftAgentFramework → "MAF",
      // LangGraph → "LG", Anthropic → "Anthropic", PydanticAi → "PydAI", BYO → "BYO".
      const rk = s.runtimeKind || "OpenClaw";
      const rkTag =
        rk === "OpenClaw" ? "OC" :
        rk === "OpenAIAgents" ? "OAI" :
        rk === "MicrosoftAgentFramework" ? "MAF" :
        rk === "LangGraph" ? "LG" :
        rk === "Anthropic" ? "Anthropic" :
        rk === "PydanticAi" ? "PydAI" :
        rk === "BYO" ? "BYO" :
        rk;
      const clusterTag = clusterOriginTag(s);
      return [hIcon, displayName, statusStr, rkTag, s.model, s.isolation, s.channels, s.age, clusterTag];
    });
    (agentTable as any).setData({
      headers: [" ", " Name", " Status", " Runtime", " Model", " Isolation", " Ch", " Age", " Cluster"],
      data: agentData.length > 0 ? agentData : [["", "(no agents)", "", "", "", "", "", "", ""]],
    });

    // Egress list — filtered to selected agent, colored by status
    const domains = selectedEgressDomains();
    const pendingCount = domains.filter((d) => d.state === "learned").length;
    const approvedCount = domains.filter((d) => d.state === "approved").length;
    const egressItems = domains.map((d) => {
      if (d.state === "approved") {
        return `{green-fg}✓ A{/green-fg} ${d.domain}`;
      }
      return `{yellow-fg}● P{/yellow-fg} ${d.domain}`;
    });
    const selAgent = sandboxes[(agentTable as any).rows?.selected ?? 0];
    const egressLabel = selAgent
      ? ` Egress: ${selAgent.name} `
      : " Egress ";
    const legend = pendingCount > 0 || approvedCount > 0
      ? `{yellow-fg}●P{/}=${pendingCount} {green-fg}✓A{/}=${approvedCount}`
      : "";
    (egressList as any).setLabel(`${egressLabel} ${legend} `);
    (egressList as any).setItems(
      egressItems.length > 0 ? egressItems : ["{gray-fg}(no domains){/}"],
    );

    // Security panel (follows agent selection)
    renderSecurity();

    // AGT Governance panel
    renderAGT();

    // Header
    renderHeader();

    // Status bar
    if (viewMode === "agents") {
      statusBar.setContent(
        statusBarForAgents({ focusedPanel, viewMode }),
      );
    } else if (viewMode === "topology") {
      statusBar.setContent(statusBarForTopology());
    } else {
      statusBar.setContent(statusBarForCluster());
    }

    // Focus border color
    if (focusedPanel === "agents") {
      (agentTable as any).style.border.fg = "cyan";
      (egressList as any).style.border.fg = "gray";
    } else {
      (agentTable as any).style.border.fg = "gray";
      (egressList as any).style.border.fg = "yellow";
    }

    screen.render();
  }

  // ── Refresh ───────────────────────────────────────────────────────

  async function refresh() {
    if (isRefreshing) return;
    if (connectedToAgent) return; // don't poll or render while inside agent session
    isRefreshing = true;
    startSpinner();
    render();

    try {
      // Always fetch sandbox list (lightweight: 1 CRD query + parallel pod/secret)
      sandboxes = await fetchSandboxes(kubeContext);
      const running = sandboxes.filter((s) => s.podName);

      // Tiered: only fetch detail data every Nth cycle to reduce API load
      const fetchDetail = refreshCount === 0 || refreshCount % TIER_DETAIL === 0;
      const fetchCluster = refreshCount === 0 || refreshCount % TIER_CLUSTER === 0;

      const promises: Promise<any>[] = [];

      if (fetchDetail) {
        promises.push(
          Promise.allSettled(running.map((s) => fetchEgressDomains(s, s.kubeContext ?? kubeContext))).then((settled) => {
            egressByAgent = new Map();
            for (let i = 0; i < running.length; i++) {
              const r = settled[i];
              if (r.status === "fulfilled") egressByAgent.set(running[i].name, r.value);
            }
          }),
          Promise.allSettled(running.map((s) => fetchSecurityState(s, s.kubeContext ?? kubeContext))).then((settled) => {
            securityStates = new Map();
            for (let i = 0; i < running.length; i++) {
              const r = settled[i];
              if (r.status === "fulfilled") securityStates.set(r.value.sandbox, r.value);
            }
          }),
        );
      } else {
        // Fast AGT-only poll on non-detail cycles to keep mesh data alive
        promises.push(
          Promise.allSettled(running.map((s) => fetchAgtQuick(s, securityStates.get(s.name), s.kubeContext ?? kubeContext))),
        );
      }

      if (fetchCluster) {
        promises.push(
          fetchClusterHealth(devMode, kubeContext).then((d) => { clusterData = d; }).catch(() => {}),
          fetchMeshHealth(devMode, kubeContext).then((d) => { meshHealth = d; }).catch(() => {}),
        );
      }

      if (promises.length > 0) await Promise.allSettled(promises);

      refreshCount++;
      const newState = `${sandboxes.length}:${totalEgressCount()}:${sandboxes.map(s => s.status).join(",")}`;
      if (newState !== lastLogState) {
        if (lastLogState) {
          const [prevAgents, prevDomains] = lastLogState.split(":").map(Number);
          const agentDiff = sandboxes.length - prevAgents;
          const domainDiff = totalEgressCount() - prevDomains;
          const parts: string[] = [];
          if (agentDiff > 0) parts.push(`+${agentDiff} agent${agentDiff !== 1 ? "s" : ""}`);
          else if (agentDiff < 0) parts.push(`${agentDiff} agent${agentDiff !== -1 ? "s" : ""}`);
          if (domainDiff > 0) parts.push(`${domainDiff} new domain${domainDiff !== 1 ? "s" : ""}`);
          else if (domainDiff < 0) parts.push(`${domainDiff} domain${domainDiff !== -1 ? "s" : ""}`);
          if (parts.length === 0) parts.push("status changed");
          activityLog.log(`{cyan-fg}↻{/} ${parts.join(", ")}`);
        } else {
          activityLog.log(
            `{cyan-fg}↻{/} ${sandboxes.length} agent(s)  ${totalEgressCount()} domain(s)`,
          );
        }
        lastLogState = newState;
      }
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Refresh:{/} ${e.message?.substring(0, 50)}`);
    }

    isRefreshing = false;
    stopSpinner();
    render();
  }

  // ── Navigation ────────────────────────────────────────────────────

  function getActiveTable(): any {
    return focusedPanel === "agents" ? agentTable : egressList;
  }
  function getActiveList(): any[] {
    return focusedPanel === "agents" ? sandboxes : selectedEgressDomains();
  }
  function moveSelection(delta: number) {
    if (dialogOpen) return;
    const widget = getActiveTable();
    const list = getActiveList();
    if (list.length === 0) return;
    // contrib.table uses .rows sub-widget; blessed.list has .selected directly
    const target = (widget as any).rows || widget;
    const current = target.selected ?? 0;
    const next = Math.max(0, Math.min(list.length - 1, current + delta));
    target.select(next);
    // Update security + egress panels when agent selection changes
    if (focusedPanel === "agents") render();
    screen.render();
  }

  // ── Keyboard ──────────────────────────────────────────────────────

  screen.key(["q", "escape"], () => {
    if (dialogOpen) return;
    if (panelsOverlay.isOpen()) {
      panelsOverlay.hide();
      return;
    }
    if (agtOverlayOpen) {
      (agtOverlay as any).hide();
      agtOverlayOpen = false;
      screen.render();
      return;
    }
    stopSpinner();
    screen.destroy();
    process.exit(0);
  });

  screen.key(["tab"], () => {
    if (dialogOpen) return;
    focusedPanel = focusedPanel === "agents" ? "egress" : "agents";
    const widget = getActiveTable();
    const target = (widget as any).rows || widget;
    target.focus();
    render();
  });

  screen.key(["up", "k"], () => moveSelection(-1));
  screen.key(["down", "j"], () => moveSelection(1));
  screen.key(["r"], async () => { if (!dialogOpen) await refresh(); });

  // Cluster view toggle
  screen.key(["c"], () => {
    if (dialogOpen || agtOverlayOpen) return;
    viewMode = viewMode === "cluster" ? "agents" : "cluster";
    render();
  });

  // Topology view toggle
  screen.key(["t"], () => {
    if (dialogOpen || agtOverlayOpen) return;
    viewMode = viewMode === "topology" ? "agents" : "topology";
    render();
  });

  // AGT full-detail overlay
  screen.key(["g"], () => {
    if (dialogOpen) return;
    if (agtOverlayOpen) {
      (agtOverlay as any).hide();
      agtOverlayOpen = false;
      screen.render();
      return;
    }
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    const content = renderAGTFull(sb);
    (agtOverlay as any).setLabel(` 🛡 AGT Governance — ${sb.name} `);
    agtOverlay.setContent(content);
    (agtOverlay as any).show();
    agtOverlay.focus();
    agtOverlayOpen = true;
    screen.render();
  });

  // S14 — panels overlay (modular per-CRD dashboard)
  screen.key(["S-p"], async () => { await panelsOverlay.toggle(); });

  // Egress actions
  // Slice 5c.1: `a` / `Shift-A` (approve) and `d`-when-egress-focused
  // (deny) were removed — there is no longer an in-memory side door
  // for the allowlist. The `d` key now always means "delete agent".
  screen.key(["d"], async () => {
    if (dialogOpen) return;
    deleteSelectedAgent();
  });
  screen.key(["e"], async () => {
    if (dialogOpen) return;
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (sb) { await enforceEgress(sb); await refresh(); }
  });

  // Learning ↔ Enforcement toggle
  screen.key(["S-l"], async () => {
    if (dialogOpen || agtOverlayOpen) return;
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    const sec = securityStates.get(sb.name);
    const mode = sec?.egressMode;
    if (mode === "learning") {
      await enforceEgress(sb);
      await refresh();
    } else if (mode === "enforcing") {
      dialogOpen = true;
      const confirmBox = blessed.box({
        parent: screen,
        border: { type: "line" },
        height: 5,
        width: "half",
        top: "center",
        left: "center",
        tags: true,
        content: `{bold}Switch ${sb.name} to learning mode?{/}\n\n  {green-fg}[y]{/} Yes   {gray-fg}[esc]{/} Cancel`,
        style: { border: { fg: "yellow" }, fg: "white" },
      });
      screen.render();

      const yesHandler = async () => {
        screen.unkey("y", yesHandler);
        screen.unkey("escape", cancelHandler);
        (confirmBox as any).destroy();
        await learnEgress(sb);
        await refresh();
        dialogOpen = false;
        screen.render();
      };
      const cancelHandler = () => {
        screen.unkey("y", yesHandler);
        screen.unkey("escape", cancelHandler);
        (confirmBox as any).destroy();
        dialogOpen = false;
        screen.render();
      };
      screen.key(["y"], yesHandler);
      screen.key(["escape"], cancelHandler);
    }
  });

  // Spawn — multi-step wizard
  // Spawn — multi-step wizard (body extracted to operator/dialogs/spawn.ts in S15.e.6)
  screen.key(["n"], () => {
    if (dialogOpen) return;
    openSpawnDialog({
      screen,
      activityLog,
      kctl,
      kubeContext,
      devMode,
      setDialogOpen: (v: boolean) => { dialogOpen = v; },
      refresh,
      learnEgress,
    });
  });

  // Model switch
  screen.key(["m"], () => {
    if (dialogOpen) return;
    if (sandboxes.length === 0) return;
    dialogOpen = true;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    const inputBox = blessed.textbox({
      parent: screen, top: "center", left: "center",
      width: 56, height: 3,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: ` Model for ${sb.name} (${sb.model}) `,
      inputOnFocus: true,
    });
    inputBox.focus();
    screen.render();
    inputBox.on("submit", async (value: string) => {
      inputBox.destroy(); screen.render(); setTimeout(() => { dialogOpen = false; }, 50);
      const model = value.trim();
      if (!model) return;
      activityLog.log(`{cyan-fg}⏳ ${sb.name} → ${model}...{/}`);
      screen.render();
      try {
        await execa("kars", ["model", "set", sb.name, model], { stdio: "pipe" });
        activityLog.log(`{green-fg}✓ Model{/} ${sb.name} → ${model}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}✗ Model fail:{/} ${(e.stderr || e.message)?.substring(0, 50)}`);
      }
      await refresh();
    });
    inputBox.on("cancel", () => { inputBox.destroy(); screen.render(); setTimeout(() => { dialogOpen = false; }, 50); });
  });

  // Logs
  screen.key(["l"], async () => {
    if (dialogOpen) return;
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb?.podName) return;
    activityLog.log(`{cyan-fg}📋 Logs {bold}${sb.name}{/bold}{/}`);
    screen.render();
    try {
      const { stdout } = await execa("kubectl", kctl([
        "logs", "-n", sb.namespace, sb.podName,
        "-c", "openclaw", "--tail=20",
      ], kubeContext), { stdio: "pipe" });
      for (const line of stdout.split("\n").slice(-10)) {
        if (line.trim()) activityLog.log(line.substring(0, 100));
      }
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Logs fail:{/} ${e.message?.substring(0, 50)}`);
    }
    screen.render();
  });

  // Delete
  // Delete (body extracted to operator/dialogs/delete.ts in S15.e.7)
  function deleteSelectedAgent() {
    if (dialogOpen) return;
    _deleteSelectedAgent({
      screen,
      agentTable,
      sandboxes,
      activityLog,
      setDialogOpen: (v: boolean) => { dialogOpen = v; },
      refresh,
    });
  }

  screen.key(["x"], () => deleteSelectedAgent());

  // ── Per-agent drill-in (i) ────────────────────────────────────────
  screen.key(["i"], () => {
    if (dialogOpen) return;
    if (sandboxes.length === 0) return;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    openAgentDetailDialog({
      screen,
      sandbox: sb,
      kubeContext,
      activityLog,
      setDialogOpen: (v: boolean) => { dialogOpen = v; },
      refresh,
    });
  });

  // ── Egress drawer (Shift-E) ───────────────────────────────────────
  screen.key(["S-e"], () => {
    if (dialogOpen) return;
    openEgressDrawer({
      screen,
      sandboxes,
      egressByAgent,
      securityStates,
      activityLog,
      setDialogOpen: (v: boolean) => { dialogOpen = v; },
      refresh,
    });
  });
  // (body extracted to operator/dialogs/connect.ts in S15.e.7)
  async function connectToAgent() {
    if (dialogOpen) return;
    await _connectToAgent({
      screen,
      agentTable,
      sandboxes,
      focusedPanel,
      activityLog,
      kctl,
      kubeContext,
      devMode,
      refreshInterval,
      refresh,
      render,
      setDialogOpen: (v: boolean) => { dialogOpen = v; },
      setConnectedToAgent: (v: boolean) => { connectedToAgent = v; },
      getRefreshTimer: () => refreshTimer,
      setRefreshTimer: (t) => { refreshTimer = t; },
    });
  }

  screen.key(["enter"], () => {
    if (dialogOpen || agtOverlayOpen) return;
    connectToAgent();
  });

  // ── Boot ──────────────────────────────────────────────────────────

  activityLog.log("{green-fg}🔱 kars Operator{/}");
  activityLog.log(`{gray-fg}ctx: ${clusterName}{/}`);
  activityLog.log(`{gray-fg}refresh: ${refreshInterval / 1000}s{/}`);
  render();

  // Initial focus
  setTimeout(() => {
    const rows = (agentTable as any).rows;
    if (rows) rows.focus();
  }, 100);

  await refresh();

  refreshTimer = setInterval(async () => { await refresh(); }, refreshInterval);
  screen.on("destroy", () => { if (refreshTimer) clearInterval(refreshTimer); stopSpinner(); });
}
