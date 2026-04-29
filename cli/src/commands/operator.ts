/**
 * AzureClaw Operator TUI — live terminal dashboard for managing sandboxes.
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
import { timeSince, kctl } from "./operator/helpers.js";
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

// ── Command ─────────────────────────────────────────────────────────

export function operatorCommand(): Command {
  const cmd = new Command("operator");

  cmd
    .description("Live operator dashboard — manage all sandboxes from one screen")
    .option("--refresh <seconds>", "Auto-refresh interval", "10")
    .option("--context <name>", "Kubernetes context to use")
    .option("--dev", "Dev mode — discover Docker containers instead of K8s pods")
    .action(async (options) => {
      const isDevMode = !!options.dev;
      // Dev mode: default 3s refresh (local Docker, no K8s latency)
      const defaultRefresh = isDevMode ? 3 : 10;
      const refreshInterval = (options.refresh ? parseInt(options.refresh, 10) : defaultRefresh) * 1000;
      await startDashboard(refreshInterval, options.context, isDevMode);
    });

  return cmd;
}

// Helper to build kubectl args with optional context


async function startDashboard(refreshInterval: number, kubeContext?: string, devMode = false) {
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
    title: "AzureClaw Operator",
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
    columnWidth: [3, 40, 14, 12, 12, 5, 6],
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
  const { approveDomain, denyDomain, enforceEgress, learnEgress } = createActions({
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
      return [hIcon, displayName, statusStr, s.model, s.isolation, s.channels, s.age];
    });
    (agentTable as any).setData({
      headers: [" ", " Name", " Status", " Model", " Isolation", " Ch", " Age"],
      data: agentData.length > 0 ? agentData : [["", "(no agents)", "", "", "", "", ""]],
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
          Promise.allSettled(running.map((s) => fetchEgressDomains(s, kubeContext))).then((settled) => {
            egressByAgent = new Map();
            for (let i = 0; i < running.length; i++) {
              const r = settled[i];
              if (r.status === "fulfilled") egressByAgent.set(running[i].name, r.value);
            }
          }),
          Promise.allSettled(running.map((s) => fetchSecurityState(s, kubeContext))).then((settled) => {
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
          Promise.allSettled(running.map((s) => fetchAgtQuick(s, securityStates.get(s.name), kubeContext))),
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

  // Egress actions
  screen.key(["a"], async () => {
    if (dialogOpen) return;
    if (focusedPanel !== "egress") return;
    const domains = selectedEgressDomains();
    if (domains.length === 0) return;
    const idx = (egressList as any).selected ?? 0;
    const domain = domains[idx];
    if (domain && domain.state === "learned") {
      await approveDomain(domain);
      await refresh();
    }
  });
  screen.key(["d"], async () => {
    if (dialogOpen) return;
    if (focusedPanel === "egress") {
      // Deny selected domain
      const domains = selectedEgressDomains();
      if (domains.length === 0) return;
      const idx = (egressList as any).selected ?? 0;
      const domain = domains[idx];
      if (domain && domain.state === "learned") {
        await denyDomain(domain);
        await refresh();
      }
    } else {
      // Delete selected agent
      deleteSelectedAgent();
    }
  });
  // Approve all learned domains for selected agent
  screen.key(["S-a"], async () => {
    const domains = selectedEgressDomains().filter((d) => d.state === "learned");
    if (domains.length === 0) return;
    const sb = sandboxes[(agentTable as any).rows?.selected ?? 0];
    activityLog.log(`{cyan-fg}⏳ Approving ${domains.length} domain(s) for {bold}${sb?.name}{/bold}...{/}`);
    screen.render();
    for (const d of domains) { await approveDomain(d); }
    await refresh();
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
  screen.key(["n"], () => {
    if (dialogOpen) return;
    dialogOpen = true;

    const state = {
      name: "", model: "gpt-4.1", isolation: "enhanced",
      channel: "", telegramToken: "", slackToken: "", discordToken: "",
      telegramAllowFrom: "",
      learnEgress: true,
      cursor: 0, editing: false,
    };

    // Pre-load stored token variants for each channel
    const storedTokens: Record<string, Array<{ key: string; label: string; value: string }>> = {
      telegram: listSecretVariants("telegram-token"),
      slack: listSecretVariants("slack-token"),
      discord: listSecretVariants("discord-token"),
    };
    const storedAllowFrom = listSecretVariants("telegram-allow-from");
    const tokenStateKey: Record<string, "telegramToken" | "slackToken" | "discordToken"> = {
      telegram: "telegramToken", slack: "slackToken", discord: "discordToken",
    };
    // Auto-fill default token if exactly one variant exists
    for (const [ch, variants] of Object.entries(storedTokens)) {
      const sk = tokenStateKey[ch];
      if (sk && variants.length === 1) state[sk] = variants[0].value;
    }
    // Auto-fill allow-from if exactly one variant
    if (storedAllowFrom.length === 1) state.telegramAllowFrom = storedAllowFrom[0].value;

    const isoOpts = ["enhanced", "standard", "confidential"];
    const chOpts = ["", "telegram", "slack", "discord"];
    const chLabels: Record<string, string> = { "": "(none)", telegram: "telegram", slack: "slack", discord: "discord" };
    const fields = () => {
      const f = ["name", "model", "isolation", "channel"];
      if (state.channel && tokenStateKey[state.channel]) f.push("chtoken");
      if (state.channel === "telegram") f.push("challowfrom");
      f.push("egress", "launch");
      return f;
    };

    const dialog = blessed.box({
      parent: screen, top: "center", left: "center",
      width: 62, height: 18,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: " 🚀 Spawn New Agent ",
      tags: true,
    });

    const formBox = blessed.box({
      parent: dialog, top: 0, left: 1, width: 58, height: 14,
      tags: true, style: { fg: "white", bg: "black" },
    });

    function draw() {
      const ff = fields();
      const lines: string[] = [];
      for (let i = 0; i < ff.length; i++) {
        const sel = state.cursor === i ? "{cyan-fg}▸{/}" : " ";
        const f = ff[i];
        if (f === "name") {
          lines.push(`${sel} {bold}Name:{/}       ${state.name || "{gray-fg}(press Enter to type){/}"}`);
        } else if (f === "model") {
          lines.push(`${sel} {bold}Model:{/}      ${state.model || "{gray-fg}(press Enter to type){/}"}`);
        } else if (f === "isolation") {
          lines.push(`${sel} {bold}Isolation:{/}  {green-fg}${state.isolation}{/}  {gray-fg}←→{/}`);
        } else if (f === "channel") {
          lines.push(`${sel} {bold}Channel:{/}    ${chLabels[state.channel] || "(none)"}  {gray-fg}←→{/}`);
        } else if (f === "chtoken") {
          const sk = tokenStateKey[state.channel];
          const tokenVal = sk ? state[sk] : "";
          const variants = storedTokens[state.channel] || [];
          const matchedVariant = variants.find(v => v.value === tokenVal);
          const display = matchedVariant
            ? `{green-fg}${matchedVariant.label}{/} (●●●●${tokenVal.slice(-4)})`
            : tokenVal ? "●●●●" + tokenVal.slice(-4) : "{gray-fg}(press Enter to type){/}";
          const hint = variants.length > 1 ? `  {gray-fg}←→ ${variants.length} stored{/}` : "";
          const label = state.channel.charAt(0).toUpperCase() + state.channel.slice(1);
          lines.push(`${sel} {bold}${label} Token:{/} ${display}${hint}`);
        } else if (f === "challowfrom") {
          const afVal = state.telegramAllowFrom;
          const afMatch = storedAllowFrom.find(v => v.value === afVal);
          const afDisplay = afMatch
            ? `{green-fg}${afMatch.label}{/} (${afVal.length > 20 ? afVal.slice(0, 17) + "…" : afVal})`
            : afVal || "{gray-fg}(press Enter to type){/}";
          const afHint = storedAllowFrom.length > 1 ? `  {gray-fg}←→ ${storedAllowFrom.length} stored{/}` : "";
          lines.push(`${sel} {bold}Allow From:{/} ${afDisplay}${afHint}`);
        } else if (f === "egress") {
          const val = state.learnEgress ? "{green-fg}learn mode{/}" : "{yellow-fg}deny all{/}";
          lines.push(`${sel} {bold}Egress:{/}     ${val}  {gray-fg}←→{/}`);
        } else if (f === "launch") {
          lines.push("");
          lines.push(`${sel} {cyan-fg}{bold}[ 🚀 Launch ]{/}`);
        }
      }
      lines.push("", "{gray-fg}↑↓ move  Enter edit/select  ←→ cycle  Esc cancel{/}");
      formBox.setContent(lines.join("\n"));
      screen.render();
    }

    function close() { dialog.destroy(); screen.render(); setTimeout(() => { dialogOpen = false; }, 50); }

    function startEdit(field: "name" | "model" | "telegramToken" | "slackToken" | "discordToken" | "telegramAllowFrom") {
      state.editing = true;
      const input = blessed.textbox({
        parent: dialog, bottom: 0, left: 1, width: 58, height: 1,
        style: { fg: "white", bg: "blue" },
        inputOnFocus: true,
        keys: true,
        vi: false,
      });
      input.setValue(state[field]);
      input.focus();
      screen.render();

      const finish = (value?: string) => {
        if (value) state[field] = value.trim();
        state.editing = false;
        input.destroy();
        const ff = fields();
        if (state.cursor < ff.length - 1) state.cursor++;
        draw();
      };

      input.on("submit", (value: string) => finish(value));
      input.on("cancel", () => finish());
      input.readInput(() => {});
    }

    async function launch() {
      close();
      if (!state.name.trim()) {
        activityLog.log("{red-fg}✗ No name provided{/}");
        return;
      }

      // Pre-flight: check for Kata nodepool when confidential (K8s only)
      if (!devMode && state.isolation === "confidential") {
        try {
          const { stdout } = await execa("kubectl", kctl([
            "get", "nodes", "-l", "azureclaw.azure.com/pool=sandbox-kata", "--no-headers",
          ], kubeContext), { stdio: "pipe" });
          if (!stdout.trim()) throw new Error("no kata nodes");
        } catch {
          activityLog.log("{red-fg}✗ No Kata nodepool found — cannot spawn confidential agent{/}");
          activityLog.log("{yellow-fg}  Run: az aks nodepool add --workload-runtime KataVmIsolation{/}");
          return;
        }
      }

      let args: string[];
      const tokenFlag: Record<string, string> = {
        telegram: "--telegram-token",
        slack: "--slack-token",
        discord: "--discord-token",
      };
      const currentToken = tokenStateKey[state.channel] ? state[tokenStateKey[state.channel]] : "";

      if (devMode) {
        args = ["dev", "--name", state.name.trim(), "--model", state.model];
        if (state.channel) {
          args.push("--channels", state.channel);
          if (currentToken && tokenFlag[state.channel]) {
            args.push(tokenFlag[state.channel], currentToken);
          }
          if (state.channel === "telegram" && state.telegramAllowFrom) {
            args.push("--telegram-allow-from", state.telegramAllowFrom);
          }
        }
      } else {
        args = ["add", state.name.trim(), "--model", state.model, "--isolation", state.isolation];
        if (state.learnEgress) args.push("--learn-egress");
        if (state.channel) {
          args.push("--channels", state.channel);
          if (currentToken && tokenFlag[state.channel]) {
            args.push(tokenFlag[state.channel], currentToken);
          }
          if (state.channel === "telegram" && state.telegramAllowFrom) {
            args.push("--telegram-allow-from", state.telegramAllowFrom);
          }
        }
      }
      activityLog.log(`{cyan-fg}⏳ Spawning {bold}${state.name}{/bold} (${state.model}, ${state.isolation})...{/}`);
      screen.render();
      try {
        await execa("azureclaw", args, { stdio: "pipe" });
        activityLog.log(`{green-fg}✓ Spawned{/} ${state.name}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}✗ Spawn fail:{/} ${(e.stderr || e.message)?.substring(0, 60)}`);
      }
      await refresh();
    }

    const onKey = (_ch: any, key: any) => {
      if (state.editing) return; // textbox handles its own input
      const ff = fields();
      const f = ff[state.cursor];
      if (key.name === "escape") {
        screen.removeListener("keypress", onKey);
        close();
      } else if (key.name === "up") {
        state.cursor = Math.max(0, state.cursor - 1);
        draw();
      } else if (key.name === "down") {
        state.cursor = Math.min(ff.length - 1, state.cursor + 1);
        draw();
      } else if (key.name === "left" || key.name === "right") {
        const d = key.name === "left" ? -1 : 1;
        if (f === "isolation") {
          const i = isoOpts.indexOf(state.isolation);
          state.isolation = isoOpts[(i + d + isoOpts.length) % isoOpts.length];
        } else if (f === "channel") {
          const i = chOpts.indexOf(state.channel);
          state.channel = chOpts[(i + d + chOpts.length) % chOpts.length];
          // Auto-fill token when switching channel (default or single stored)
          const sk = tokenStateKey[state.channel];
          if (sk && !state[sk]) {
            const variants = storedTokens[state.channel] || [];
            if (variants.length === 1) state[sk] = variants[0].value;
          }
        } else if (f === "chtoken") {
          // Cycle through stored token variants with ←→
          const variants = storedTokens[state.channel] || [];
          if (variants.length > 1) {
            const sk = tokenStateKey[state.channel];
            const currentVal = sk ? state[sk] : "";
            const idx = variants.findIndex(v => v.value === currentVal);
            const next = variants[(idx + d + variants.length) % variants.length];
            if (sk) state[sk] = next.value;
            // Auto-correlate: fill matching allow-from variant (e.g. cloud→cloud)
            if (state.channel === "telegram") {
              const afMatch = storedAllowFrom.find(v => v.label === next.label);
              if (afMatch) state.telegramAllowFrom = afMatch.value;
            }
          }
        } else if (f === "challowfrom") {
          // Cycle through stored allow-from variants with ←→
          if (storedAllowFrom.length > 1) {
            const idx = storedAllowFrom.findIndex(v => v.value === state.telegramAllowFrom);
            const next = storedAllowFrom[(idx + d + storedAllowFrom.length) % storedAllowFrom.length];
            state.telegramAllowFrom = next.value;
          }
        } else if (f === "egress") {
          state.learnEgress = !state.learnEgress;
        }
        draw();
      } else if (key.name === "return" || key.name === "enter") {
        if (f === "name") startEdit("name");
        else if (f === "model") startEdit("model");
        else if (f === "chtoken") {
          const sk = tokenStateKey[state.channel];
          if (sk) startEdit(sk);
        }
        else if (f === "challowfrom") startEdit("telegramAllowFrom");
        else if (f === "launch") { screen.removeListener("keypress", onKey); launch(); }
        else {
          // Cycle fields advance on Enter too
          state.cursor = Math.min(ff.length - 1, state.cursor + 1);
          draw();
        }
      }
    };

    screen.on("keypress", onKey);
    draw();
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
        await execa("azureclaw", ["model", "set", sb.name, model], { stdio: "pipe" });
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
  function deleteSelectedAgent() {
    if (dialogOpen) return;
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    dialogOpen = true;

    // Custom confirm dialog with selectable buttons
    const dialog = blessed.box({
      parent: screen, top: "center", left: "center",
      width: 52, height: 7,
      border: { type: "line" },
      style: { border: { fg: "red" }, fg: "white", bg: "black" },
      label: " ⚠  Confirm Delete ",
      tags: true,
    });
    blessed.box({
      parent: dialog, top: 0, left: 2, width: 46, height: 1,
      tags: true, style: { fg: "white", bg: "black" },
      content: `Destroy agent {bold}${sb.name}{/bold}?`,
    });

    let selected = 0; // 0 = Yes, 1 = Cancel
    const btnYes = blessed.button({
      parent: dialog, top: 2, left: 8, width: 12, height: 1,
      content: "  [ Yes ]  ", tags: true, mouse: true,
      style: { fg: "white", bg: "red", focus: { bg: "red", fg: "white", bold: true } },
    });
    const btnCancel = blessed.button({
      parent: dialog, top: 2, left: 28, width: 14, height: 1,
      content: "  [ Cancel ]  ", tags: true, mouse: true,
      style: { fg: "white", bg: "gray", focus: { bg: "gray", fg: "white", bold: true } },
    });

    function updateButtons() {
      btnYes.style.bg = selected === 0 ? "red" : "black";
      btnYes.style.bold = selected === 0;
      btnCancel.style.bg = selected === 1 ? "gray" : "black";
      btnCancel.style.bold = selected === 1;
      screen.render();
    }

    const cleanup = () => { dialog.destroy(); screen.render(); setTimeout(() => { dialogOpen = false; }, 50); };

    const onKey = async (_ch: any, key: any) => {
      if (key.name === "left" || key.name === "right" || key.name === "tab") {
        selected = selected === 0 ? 1 : 0;
        updateButtons();
      } else if (key.name === "return" || key.name === "enter") {
        screen.removeListener("keypress", onKey);
        cleanup();
        if (selected === 0) {
          activityLog.log(`{red-fg}🗑  Destroying {bold}${sb.name}{/bold}...{/}`);
          screen.render();
          try {
            if (sb.runtime === "docker") {
              await execa("docker", ["rm", "-f", sb.podName!], { stdio: "pipe" });
            } else {
              await execa("azureclaw", ["destroy", sb.name, "--cloud", "--yes"], { stdio: "pipe" });
            }
            activityLog.log(`{green-fg}✓ Destroyed{/} ${sb.name}`);
          } catch (e: any) {
            activityLog.log(`{red-fg}✗ Destroy fail:{/} ${(e.stderr || e.message)?.substring(0, 60)}`);
          }
          await refresh();
        }
      } else if (key.name === "escape" || key.name === "q") {
        screen.removeListener("keypress", onKey);
        cleanup();
      }
    };

    screen.on("keypress", onKey);
    btnYes.on("press", () => { selected = 0; onKey(null, { name: "return" }); });
    btnCancel.on("press", () => { selected = 1; onKey(null, { name: "return" }); });

    updateButtons();
    btnYes.focus();
    screen.render();
  }

  screen.key(["x"], () => deleteSelectedAgent());

  // ── Connect to agent (Enter) ──────────────────────────────────────
  async function connectToAgent() {
    if (dialogOpen) return;
    if (focusedPanel !== "agents") return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;

    dialogOpen = true;
    connectedToAgent = true;
    const sessionId = `operator-${sb.name}`;

    // Stop the refresh timer — prevents blessed from writing to stdout
    if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null; }

    // Save blessed state and leave alternate screen
    try { screen.program.lsaveCursor("operator"); } catch {}
    screen.program.normalBuffer();
    screen.program.showCursor();
    screen.program.flush();

    // Remove ALL blessed listeners from stdin so we're the sole reader.
    const savedDataListeners = process.stdin.listeners("data").slice();
    const savedKeypressListeners = process.stdin.listeners("keypress").slice();
    process.stdin.removeAllListeners("data");
    process.stdin.removeAllListeners("keypress");

    // Spawn PTY for proper TTY passthrough with colors
    const nodePty = await import("node-pty");
    const connectCmd = devMode ? "docker" : "kubectl";
    const connectArgs = devMode
      ? ["exec", "-it", sb.podName!, "openclaw", "tui"]
      : kctl(["exec", "-it", "-n", sb.namespace, `deploy/${sb.name}`, "-c", "openclaw", "--", "openclaw", "tui"], kubeContext);

    const cols = process.stdout.columns || 80;
    const rows = process.stdout.rows || 24;
    const ptyProcess = nodePty.spawn(connectCmd, connectArgs, {
      name: "xterm-256color",
      cols,
      rows,
      cwd: process.cwd(),
      env: { ...process.env, TERM: "xterm-256color" } as Record<string, string>,
    });

    // Pipe PTY output to stdout
    ptyProcess.onData((data: string) => {
      process.stdout.write(data);
    });

    // Raw mode: forward keystrokes to PTY.
    // Detach: Ctrl+\ (0x1c) or Ctrl+] (0x1d)
    if (process.stdin.isTTY) {
      process.stdin.setRawMode(true);
    }
    process.stdin.resume();

    const onData = (data: Buffer) => {
      // Ctrl+\ = 0x1c, Ctrl+] = 0x1d — detach
      if (data.length === 1 && (data[0] === 0x1c || data[0] === 0x1d)) {
        cleanup("detach");
        return;
      }
      try { ptyProcess.write(data.toString()); } catch {}
    };
    process.stdin.on("data", onData);

    // Forward resize to PTY
    const onResize = () => {
      try { ptyProcess.resize(process.stdout.columns || 80, process.stdout.rows || 24); } catch {}
    };
    process.stdout.on("resize", onResize);

    // Suppress SIGINT — let it reach child via PTY
    const sigintHandler = () => {};
    process.on("SIGINT", sigintHandler);

    ptyProcess.onExit(() => cleanup("exit"));

    let cleaned = false;
    function cleanup(reason: string) {
      if (cleaned) return;
      cleaned = true;

      process.stdin.removeAllListeners("data");
      process.stdout.removeListener("resize", onResize);
      process.removeListener("SIGINT", sigintHandler);
      try { ptyProcess.kill(); } catch {}

      // Restore ALL blessed stdin listeners
      for (const fn of savedDataListeners) process.stdin.on("data", fn as (...args: any[]) => void);
      for (const fn of savedKeypressListeners) process.stdin.on("keypress", fn as (...args: any[]) => void);

      // Restore blessed terminal state
      if (process.stdin.isTTY) process.stdin.setRawMode(true);
      process.stdin.resume();
      process.stdout.write("\x1b[?25l\x1b[?1049h");
      screen.program.alternateBuffer();
      try { screen.program.lrestoreCursor("operator"); } catch {}
      screen.program.hideCursor();
      screen.program.flush();
      screen.alloc();

      connectedToAgent = false;
      dialogOpen = false;

      // Restart refresh timer
      refreshTimer = setInterval(async () => { await refresh(); }, refreshInterval);

      if (reason === "detach") {
        activityLog.log(`{cyan-fg}⏏ Detached from ${sb.name}{/}`);
      } else {
        activityLog.log(`{green-fg}↩ Back from ${sb.name} (session: ${sessionId}){/}`);
      }
      render();
      screen.render();

      // Immediate refresh to catch any changes while we were connected
      setTimeout(() => refresh(), 500);
    }

    // Show hint
    process.stdout.write(`\r\n\x1b[36m⟩ Connected to ${sb.name}. Press Ctrl+\\ to detach, /exit to quit.\x1b[0m\r\n\r\n`);
  }

  screen.key(["enter"], () => {
    if (dialogOpen || agtOverlayOpen) return;
    connectToAgent();
  });

  // ── Boot ──────────────────────────────────────────────────────────

  activityLog.log("{green-fg}🔱 AzureClaw Operator{/}");
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
