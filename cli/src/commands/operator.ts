/**
 * AzureClaw Operator TUI — live terminal dashboard for managing sandboxes.
 *
 * Layout:
 *   ┌─────────────────────── Header ───────────────────────────┐
 *   │  🔱 AzureClaw Operator │ cluster │ health │ time         │
 *   ├────────────────── Agent Table ───────────────────────────┤
 *   │  ● name      status    model    isolation  ch   age      │
 *   ├──── Security ──────┬── Egress ──────┬──── Log ──────────┤
 *   │ Isolation  enhanced │ domain  agent  │ ↻ #3 3 agents... │
 *   │ Seccomp    strict   │ ...            │ ✓ Approved foo   │
 *   │ Blocklist  48231    │                │                   │
 *   │ Egress     learning │                │ ▃▅▇▅▃▁ activity  │
 *   ├─────────────────── Status Bar ───────────────────────────┤
 *   │ [Tab] Focus [↑↓] Nav [a] Approve [d] Deny ...           │
 *   └─────────────────────────────────────────────────────────-┘
 *
 * Keyboard:
 *   Tab       — cycle focus: agents → egress → (repeat)
 *   ↑/↓ j/k   — navigate rows in focused table
 *   a         — approve selected egress domain
 *   d         — deny selected egress domain
 *   e         — enforce egress (lock down)
 *   n         — spawn new agent
 *   m         — switch model for selected agent
 *   l         — tail logs for selected agent
 *   x         — delete selected agent (with confirmation)
 *   c         — toggle cluster health view
 *   r         — refresh now
 *   q / Esc   — quit
 */

import { Command } from "commander";
import { execa } from "execa";
import blessed from "blessed";
import contrib from "blessed-contrib";

// ── Types ───────────────────────────────────────────────────────────

type HealthState = "healthy" | "degraded" | "down" | "pending" | "unknown";

interface SandboxInfo {
  name: string;
  namespace: string;
  status: string;
  health: HealthState;
  model: string;
  isolation: string;
  channels: string;
  age: string;
  podName: string;
  restarts: number;
}

interface EgressDomain {
  domain: string;
  sandbox: string;
  namespace: string;
  state: "learned" | "approved";
}

/** Security state polled from a single sandbox's router + k8s objects. */
interface SecurityState {
  sandbox: string;
  isolation: string;
  runtime: string;         // "runc" | "kata-vm-isolation"
  seccomp: string;         // "azureclaw-strict" | "RuntimeDefault"
  networkPolicy: boolean;
  adminAuth: boolean;
  readyz: boolean;
  readyzDetail: string;    // "ok" | "not ready — ..." | "content safety unreachable"
  egressMode: string;      // "learning" | "enforcing" | "unknown"
  learnedDomains: number;
  allowlistDomains: number;
  blocklistDomains: number;
  blocklistLearnMode: boolean;
  agtEnabled: boolean;
  agtAuditEntries: number;
  agtAuditIntegrity: boolean;
  agtKnownAgents: number;
  agtTrustThreshold: number;
  // Prometheus metrics
  totalRequests: number;
  errorRequests: number;
  inputTokens: number;
  outputTokens: number;
  avgLatencyMs: number;
}

interface NodeInfo {
  name: string;
  pool: string;
  status: string;
  version: string;
  cpuCores: string;
  cpuPct: string;
  memBytes: string;
  memPct: string;
  os: string;
  runtime: string;
}

interface ClusterHealth {
  apiLatencyMs: number;
  apiReachable: boolean;
  nodes: NodeInfo[];
  quotas: { namespace: string; cpuUsed: string; cpuHard: string; memUsed: string; memHard: string }[];
  pvcs: { namespace: string; name: string; phase: string; size: string }[];
  warnings: { time: string; reason: string; object: string; message: string }[];
}

// ── Command ─────────────────────────────────────────────────────────

export function operatorCommand(): Command {
  const cmd = new Command("operator");

  cmd
    .description("Live operator dashboard — manage all sandboxes from one screen")
    .option("--refresh <seconds>", "Auto-refresh interval", "10")
    .option("--context <name>", "Kubernetes context to use")
    .action(async (options) => {
      const refreshInterval = parseInt(options.refresh, 10) * 1000;
      await startDashboard(refreshInterval, options.context);
    });

  return cmd;
}

// Helper to build kubectl args with optional context
function kctl(args: string[], context?: string): string[] {
  return context ? ["--context", context, ...args] : args;
}

async function startDashboard(refreshInterval: number, kubeContext?: string) {
  // ── Resolve cluster ───────────────────────────────────────────────
  let clusterName = "unknown";
  try {
    const { stdout } = await execa("kubectl", kctl([
      "config", "current-context",
    ], kubeContext), { stdio: "pipe" });
    clusterName = stdout.trim();
  } catch { /* offline */ }

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
    style: { fg: "white", bold: true },
  });

  // Rows 1–4: Agent table (full width)
  const agentTable = grid.set(1, 0, 4, 12, contrib.table, {
    keys: false,
    vi: false,
    fg: "white",
    label: " Agents  [↑↓ navigate] ",
    columnSpacing: 1,
    columnWidth: [3, 28, 16, 16, 12, 8, 6],
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

  // Rows 5–9: Egress table (cols 4–8)
  const egressTable = grid.set(5, 4, 5, 4, contrib.table, {
    keys: false,
    vi: false,
    fg: "white",
    label: " Egress  [a]pprove [d]eny [e]nforce ",
    columnSpacing: 1,
    columnWidth: [3, 40],
    interactive: true,
    style: {
      border: { fg: "yellow" },
      header: { fg: "yellow", bold: true },
      cell: { selected: { bg: "yellow", fg: "black" } },
    },
  });

  // Rows 5–7: Activity log (cols 8–11)
  const activityLog = grid.set(5, 8, 3, 4, contrib.log, {
    fg: "green",
    label: " Log ",
    tags: true,
    style: { border: { fg: "green" } },
    bufferLength: 80,
  });

  // Rows 8–9: Sparkline (cols 8–11)
  const sparkline = grid.set(8, 8, 2, 4, contrib.sparkline, {
    label: " Activity ",
    tags: true,
    style: { fg: "cyan", border: { fg: "cyan" } },
  });

  // Rows 10–11: Status bar
  const statusBar = grid.set(10, 0, 2, 12, blessed.box, {
    tags: true,
    style: { fg: "white", bg: "default" },
  });

  // ── Cluster Health Overlay (hidden by default) ──────────────────

  // Agent-detail panels (the bottom row: security, egress, log, sparkline)
  const agentDetailPanels = [securityBox, egressTable, activityLog, sparkline];

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

  // ── State ─────────────────────────────────────────────────────────

  let sandboxes: SandboxInfo[] = [];
  let egressByAgent: Map<string, EgressDomain[]> = new Map();
  let securityStates: Map<string, SecurityState> = new Map();
  let clusterData: ClusterHealth | null = null;
  let viewMode: "agents" | "cluster" = "agents";
  let focusedPanel: "agents" | "egress" = "agents";
  let refreshCount = 0;
  let isRefreshing = false;

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

  const sparkData: number[] = new Array(30).fill(0);

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

  // ── Data fetching ─────────────────────────────────────────────────

  async function fetchSandboxes(): Promise<SandboxInfo[]> {
    try {
      const { stdout } = await execa("kubectl", kctl([
        "get", "clawsandbox", "-A",
        "-o", `jsonpath={range .items[*]}{.metadata.name}|{.metadata.namespace}|{.status.phase}|{.spec.inference.model}|{.spec.sandbox.isolation}|{.metadata.creationTimestamp}{\"\\n\"}{end}`,
      ], kubeContext), { stdio: "pipe" });

      const results: SandboxInfo[] = [];
      for (const line of stdout.trim().split("\n").filter(Boolean)) {
        const parts = line.split("|");
        const [name, , phase, model, isolation, created] = parts;
        if (!name) continue;

        const sandboxNs = `azureclaw-${name}`;
        let podStatus = phase || "Unknown";
        let podName = "";
        let channels = "";
        let health: HealthState = "pending";
        let restarts = 0;

        try {
          const { stdout: podJson } = await execa("kubectl", kctl([
            "get", "pods", "-n", sandboxNs, "-o", "json",
          ], kubeContext), { stdio: "pipe" });
          const pods = JSON.parse(podJson);
          if (pods.items?.length > 0) {
            const pod = pods.items[0];
            podName = pod.metadata?.name || "";
            const pPhase = pod.status?.phase || "Unknown";
            const statuses: any[] = pod.status?.containerStatuses || [];
            const readyCount = statuses.filter((c: any) => c.ready).length;
            const totalCount = statuses.length;
            restarts = statuses.reduce((sum: number, c: any) => sum + (c.restartCount || 0), 0);
            podStatus = totalCount > 0 ? `${pPhase} (${readyCount}/${totalCount})` : pPhase;

            const hasCrash = statuses.some((c: any) =>
              c.state?.waiting?.reason === "CrashLoopBackOff" ||
              c.state?.waiting?.reason === "Error",
            );
            if (hasCrash || pPhase === "Failed") health = "down";
            else if (readyCount === totalCount && totalCount > 0) health = "healthy";
            else if (readyCount > 0) health = "degraded";
            else if (pPhase === "Pending") health = "pending";
            else health = "unknown";
          }
        } catch { /* no pod */ }

        try {
          const { stdout: secretOut } = await execa("kubectl", kctl([
            "get", "secret", `${name}-credentials`, "-n", sandboxNs,
            "-o", "jsonpath={.data}",
          ], kubeContext), { stdio: "pipe" });
          const chs: string[] = [];
          if (secretOut.includes("TELEGRAM")) chs.push("TG");
          if (secretOut.includes("SLACK")) chs.push("SL");
          if (secretOut.includes("DISCORD")) chs.push("DC");
          channels = chs.join(",") || "-";
        } catch { channels = "-"; }

        let age = "-";
        if (created) {
          const d = new Date(created);
          if (!isNaN(d.getTime())) age = timeSince(d);
        }

        results.push({
          name, namespace: sandboxNs, status: podStatus,
          health, model: model || "gpt-4.1", isolation: isolation || "enhanced",
          channels, age, podName, restarts,
        });
      }
      return results;
    } catch {
      return [];
    }
  }

  async function fetchEgressDomains(sb: SandboxInfo): Promise<EgressDomain[]> {
    if (!sb.podName) return [];
    const routerCurl = (path: string) => execa("kubectl", kctl([
      "exec", "-n", sb.namespace, sb.podName,
      "-c", "inference-router", "--",
      "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
    ], kubeContext), { stdio: "pipe" });

    try {
      const [learnedRes, allowRes] = await Promise.allSettled([
        routerCurl("/egress/learned"),
        routerCurl("/egress/allowlist"),
      ]);

      const learnedDomains: Set<string> = new Set();
      const approvedDomains: Set<string> = new Set();

      if (learnedRes.status === "fulfilled") {
        const data = JSON.parse((learnedRes.value as any).stdout);
        for (const d of (data.domains || [])) learnedDomains.add(d);
      }
      if (allowRes.status === "fulfilled") {
        const data = JSON.parse((allowRes.value as any).stdout);
        for (const d of (data.domains || [])) approvedDomains.add(d);
      }

      // Merge: domains in allowlist are "approved", rest are "learned" (pending)
      const results: EgressDomain[] = [];
      const allDomains = new Set([...learnedDomains, ...approvedDomains]);
      for (const d of allDomains) {
        results.push({
          domain: d,
          sandbox: sb.name,
          namespace: sb.namespace,
          state: approvedDomains.has(d) ? "approved" : "learned",
        });
      }
      // Sort: pending first, then approved
      results.sort((a, b) => {
        if (a.state !== b.state) return a.state === "learned" ? -1 : 1;
        return a.domain.localeCompare(b.domain);
      });
      return results;
    } catch {
      return [];
    }
  }

  /** Poll security-relevant endpoints for a single sandbox. */
  async function fetchSecurityState(sb: SandboxInfo): Promise<SecurityState> {
    const state: SecurityState = {
      sandbox: sb.name,
      isolation: sb.isolation,
      runtime: sb.isolation === "confidential" ? "kata-vm" : "runc",
      seccomp: sb.isolation === "enhanced" ? "azureclaw-strict" :
               sb.isolation === "confidential" ? "RuntimeDefault" : "RuntimeDefault",
      networkPolicy: false,
      adminAuth: false,
      readyz: false,
      readyzDetail: "unknown",
      egressMode: "unknown",
      learnedDomains: 0,
      allowlistDomains: 0,
      blocklistDomains: 0,
      blocklistLearnMode: false,
      agtEnabled: false,
      agtAuditEntries: 0,
      agtAuditIntegrity: false,
      agtKnownAgents: 0,
      agtTrustThreshold: 0,
      totalRequests: 0,
      errorRequests: 0,
      inputTokens: 0,
      outputTokens: 0,
      avgLatencyMs: 0,
    };

    if (!sb.podName) return state;

    const routerExec = (path: string) => execa("kubectl", kctl([
      "exec", "-n", sb.namespace, sb.podName,
      "-c", "inference-router", "--",
      "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
    ], kubeContext), { stdio: "pipe" });

    // Run all checks in parallel
    const checks = await Promise.allSettled([
      // 0: NetworkPolicy
      execa("kubectl", kctl([
        "get", "networkpolicy", "sandbox-policy", "-n", sb.namespace,
        "-o", "name",
      ], kubeContext), { stdio: "pipe" }),
      // 1: Admin token secret
      execa("kubectl", kctl([
        "get", "secret", "router-admin-token", "-n", sb.namespace,
        "-o", "name",
      ], kubeContext), { stdio: "pipe" }),
      // 2: /readyz (body, not just status code)
      routerExec("/readyz"),
      // 3: /blocklist/status
      routerExec("/blocklist/status"),
      // 4: /agt/status
      routerExec("/agt/status"),
      // 5: /egress/allowlist
      routerExec("/egress/allowlist"),
      // 6: /metrics (Prometheus text)
      routerExec("/metrics"),
    ]);

    // NetworkPolicy
    if (checks[0].status === "fulfilled") state.networkPolicy = true;

    // Admin token
    if (checks[1].status === "fulfilled") state.adminAuth = true;

    // readyz
    if (checks[2].status === "fulfilled") {
      const body = ((checks[2].value as any).stdout || "").trim();
      state.readyz = body.startsWith("ok");
      state.readyzDetail = body || "ok";
    }

    // blocklist/status
    if (checks[3].status === "fulfilled") {
      try {
        const bl = JSON.parse((checks[3].value as any).stdout);
        state.blocklistDomains = bl.domain_count || 0;
        state.blocklistLearnMode = bl.learn_mode ?? false;
        state.learnedDomains = bl.learned_domains || 0;
        state.egressMode = bl.learn_mode ? "learning" : "enforcing";
      } catch { /* parse fail */ }
    }

    // agt/status
    if (checks[4].status === "fulfilled") {
      try {
        const agt = JSON.parse((checks[4].value as any).stdout);
        state.agtEnabled = agt.enabled ?? false;
        state.agtAuditEntries = agt.audit_entries || 0;
        state.agtAuditIntegrity = agt.audit_integrity ?? false;
        state.agtKnownAgents = agt.known_agents || 0;
      } catch { /* parse fail */ }
    }

    // egress/allowlist
    if (checks[5].status === "fulfilled") {
      try {
        const al = JSON.parse((checks[5].value as any).stdout);
        state.allowlistDomains = al.count || 0;
      } catch { /* parse fail */ }
    }

    // /metrics — parse Prometheus text format
    if (checks[6].status === "fulfilled") {
      const metricsText = (checks[6].value as any).stdout || "";
      state.totalRequests = sumPrometheusCounter(metricsText, "azureclaw_inference_requests_total");
      state.errorRequests = sumPrometheusCounter(metricsText, "azureclaw_inference_requests_total", { status: "error" });
      state.inputTokens = sumPrometheusCounter(metricsText, "azureclaw_tokens_total", { direction: "input" });
      state.outputTokens = sumPrometheusCounter(metricsText, "azureclaw_tokens_total", { direction: "output" });

      // Average latency from histogram sum/count
      const latSum = sumPrometheusCounter(metricsText, "azureclaw_inference_latency_seconds_sum");
      const latCount = sumPrometheusCounter(metricsText, "azureclaw_inference_latency_seconds_count");
      state.avgLatencyMs = latCount > 0 ? Math.round((latSum / latCount) * 1000) : 0;
    }

    return state;
  }

  // ── Cluster Health ────────────────────────────────────────────────

  async function fetchClusterHealth(): Promise<ClusterHealth> {
    const result: ClusterHealth = {
      apiLatencyMs: -1, apiReachable: false,
      nodes: [], quotas: [], pvcs: [], warnings: [],
    };

    // All queries in parallel
    const [apiRes, nodesRes, topRes, quotaRes, pvcRes, eventsRes] = await Promise.allSettled([
      // 0: API server latency
      (async () => {
        const start = Date.now();
        await execa("kubectl", kctl(["get", "ns", "--no-headers"], kubeContext),
          { stdio: "pipe", timeout: 10000 });
        return Date.now() - start;
      })(),
      // 1: Node info
      execa("kubectl", kctl([
        "get", "nodes", "-o",
        `jsonpath={range .items[*]}{.metadata.name}|{.metadata.labels.agentpool}|` +
        `{.status.conditions[?(@.type=="Ready")].status}|{.status.nodeInfo.kubeletVersion}|` +
        `{.status.nodeInfo.osImage}|{.status.nodeInfo.containerRuntimeVersion}{\"\\n\"}{end}`,
      ], kubeContext), { stdio: "pipe" }),
      // 2: kubectl top nodes
      execa("kubectl", kctl(["top", "nodes", "--no-headers"], kubeContext),
        { stdio: "pipe", timeout: 10000 }),
      // 3: Resource quotas
      execa("kubectl", kctl([
        "get", "resourcequotas", "-A", "-o",
        `jsonpath={range .items[*]}{.metadata.namespace}|{.status.used.cpu}|{.status.hard.cpu}|` +
        `{.status.used.memory}|{.status.hard.memory}{\"\\n\"}{end}`,
      ], kubeContext), { stdio: "pipe" }),
      // 4: PVCs
      execa("kubectl", kctl([
        "get", "pvc", "-A", "-o",
        `jsonpath={range .items[*]}{.metadata.namespace}|{.metadata.name}|{.status.phase}|` +
        `{.spec.resources.requests.storage}{\"\\n\"}{end}`,
      ], kubeContext), { stdio: "pipe" }),
      // 5: Warning events
      execa("kubectl", kctl([
        "get", "events", "-A", "--field-selector", "type=Warning",
        "--sort-by=.lastTimestamp", "-o",
        `jsonpath={range .items[-8:]}{.lastTimestamp}|{.reason}|` +
        `{.involvedObject.kind}/{.involvedObject.name}|{.message}{\"\\n\"}{end}`,
      ], kubeContext), { stdio: "pipe" }),
    ]);

    // Parse API latency
    if (apiRes.status === "fulfilled") {
      result.apiLatencyMs = apiRes.value as number;
      result.apiReachable = true;
    }

    // Parse nodes
    if (nodesRes.status === "fulfilled") {
      const topMap = new Map<string, { cpu: string; cpuPct: string; mem: string; memPct: string }>();
      if (topRes.status === "fulfilled") {
        for (const line of (topRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
          const parts = line.trim().split(/\s+/);
          if (parts.length >= 5) {
            topMap.set(parts[0], { cpu: parts[1], cpuPct: parts[2], mem: parts[3], memPct: parts[4] });
          }
        }
      }

      for (const line of (nodesRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
        const [name, pool, ready, version, os, runtime] = line.split("|");
        if (!name) continue;
        const top = topMap.get(name);
        result.nodes.push({
          name, pool: pool || "-",
          status: ready === "True" ? "Ready" : "NotReady",
          version: version || "-",
          cpuCores: top?.cpu || "-", cpuPct: top?.cpuPct || "-",
          memBytes: top?.mem || "-", memPct: top?.memPct || "-",
          os: os || "-", runtime: runtime || "-",
        });
      }
    }

    // Parse quotas
    if (quotaRes.status === "fulfilled") {
      for (const line of (quotaRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
        const [ns, cpuUsed, cpuHard, memUsed, memHard] = line.split("|");
        if (ns) result.quotas.push({ namespace: ns, cpuUsed: cpuUsed || "0", cpuHard: cpuHard || "-", memUsed: memUsed || "0", memHard: memHard || "-" });
      }
    }

    // Parse PVCs
    if (pvcRes.status === "fulfilled") {
      for (const line of (pvcRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
        const [ns, name, phase, size] = line.split("|");
        if (ns) result.pvcs.push({ namespace: ns, name: name || "-", phase: phase || "Unknown", size: size || "-" });
      }
    }

    // Parse warnings
    if (eventsRes.status === "fulfilled") {
      for (const line of (eventsRes.value as any).stdout.trim().split("\n").filter(Boolean)) {
        const [time, reason, object, ...rest] = line.split("|");
        if (reason) result.warnings.push({
          time: time ? timeSince(new Date(time)) : "-",
          reason: reason || "-",
          object: (object || "-").substring(0, 40),
          message: (rest.join("|") || "").substring(0, 60),
        });
      }
    }

    return result;
  }

  // ── Actions ───────────────────────────────────────────────────────

  async function approveDomain(domain: EgressDomain) {
    const sb = sandboxes.find((s) => s.name === domain.sandbox);
    if (!sb?.podName) {
      activityLog.log(`{red-fg}✗ No pod for{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", kctl([
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/approve",
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{green-fg}✓ Approved{/} ${domain.domain}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Approve fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function denyDomain(domain: EgressDomain) {
    const sb = sandboxes.find((s) => s.name === domain.sandbox);
    if (!sb?.podName) {
      activityLog.log(`{red-fg}✗ No pod for{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", kctl([
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/deny",
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{yellow-fg}✗ Denied{/} ${domain.domain}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Deny fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function enforceEgress(sb: SandboxInfo) {
    if (!sb.podName) return;
    try {
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/enforce",
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{green-fg}🔒 Enforced{/} ${sb.name}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Enforce fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  // ── Rendering ─────────────────────────────────────────────────────

  const ok = (v: boolean) => v ? "{green-fg}●{/}" : "{red-fg}●{/}";

  function healthSummary(): string {
    const h = sandboxes.filter((s) => s.health === "healthy").length;
    const d = sandboxes.filter((s) => s.health === "degraded").length;
    const x = sandboxes.filter((s) => s.health === "down").length;
    const parts: string[] = [];
    if (h > 0) parts.push(`{green-fg}${h} healthy{/}`);
    if (d > 0) parts.push(`{yellow-fg}${d} degraded{/}`);
    if (x > 0) parts.push(`{red-fg}${x} down{/}`);
    if (parts.length === 0) return "{gray-fg}no agents{/}";
    return parts.join("  ");
  }

  function renderHeader() {
    const now = new Date().toLocaleTimeString();
    const spin = isRefreshing ? `{cyan-fg}${spinFrames[spinIdx]}{/} ` : "";
    const ctx = `{gray-fg}${clusterName}{/}`;

    // Cluster health indicator
    let clusterTag = "";
    if (clusterData) {
      const readyNodes = clusterData.nodes.filter((n) => n.status === "Ready").length;
      const totalNodes = clusterData.nodes.length;
      const nColor = readyNodes === totalNodes ? "green" : readyNodes > 0 ? "yellow" : "red";
      const apiTag = clusterData.apiReachable ? "{green-fg}●{/}" : "{red-fg}●{/}";
      clusterTag = `${apiTag} API  {${nColor}-fg}${readyNodes}/${totalNodes}{/} nodes  │  `;
    }

    const viewLabel = viewMode === "cluster" ? "  {blue-fg}{bold}[CLUSTER VIEW]{/bold}{/}  │  " : "";
    header.setContent(
      ` ${spin}{bold}🔱 AzureClaw Operator{/bold}  │  ${ctx}  │  ${viewLabel}` +
      `${clusterTag}${healthSummary()}  │  ${totalEgressCount()} domain(s)  │  {gray-fg}${now}{/}`,
    );
  }

  function renderSecurity() {
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) {
      securityBox.setContent("{gray-fg}No agent selected{/}");
      return;
    }

    const sec = securityStates.get(sb.name);
    if (!sec) {
      securityBox.setContent(`{bold}${sb.name}{/}\n\n{gray-fg}Polling...{/}`);
      return;
    }

    const seccompLabel = sec.seccomp === "azureclaw-strict"
      ? "{green-fg}strict (~219){/}" : `{yellow-fg}${sec.seccomp}{/}`;

    const egressLabel = sec.egressMode === "learning"
      ? `{yellow-fg}learning{/} (${sec.learnedDomains} found)`
      : sec.egressMode === "enforcing"
      ? `{green-fg}enforcing{/} (${sec.allowlistDomains} allowed)`
      : "{gray-fg}unknown{/}";

    const blLabel = sec.blocklistDomains > 0
      ? `{green-fg}${sec.blocklistDomains.toLocaleString()}{/} domains`
      : "{yellow-fg}not loaded{/}";

    const readyzLabel = sec.readyz
      ? `{green-fg}${sec.readyzDetail}{/}`
      : `{red-fg}${sec.readyzDetail}{/}`;

    // Token formatting
    const fmtTokens = (n: number) =>
      n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` :
      n >= 1_000 ? `${(n / 1_000).toFixed(1)}K` :
      `${n}`;

    const totalTokens = sec.inputTokens + sec.outputTokens;
    const errRate = sec.totalRequests > 0
      ? `${((sec.errorRequests / sec.totalRequests) * 100).toFixed(1)}%`
      : "-";
    const errColor = sec.errorRequests > 0 ? "red" : "green";

    const lines: string[] = [
      `{bold}${sb.name}{/}`,
      "",
      `{bold}{underline}Infrastructure{/}`,
      ` Isolation     ${sec.isolation} (${sec.runtime})`,
      ` Seccomp       ${seccompLabel}`,
      ` NetworkPolicy ${ok(sec.networkPolicy)} ${sec.networkPolicy ? "active" : "missing"}`,
      ` iptables      ${ok(true)} redirect → proxy`,
      ` Fwd Proxy     ${ok(true)} localhost:8444`,
      ` Admin Auth    ${ok(sec.adminAuth)} ${sec.adminAuth ? "token set" : "disabled"}`,
      ` Router Ready  ${ok(sec.readyz)} ${readyzLabel}`,
      "",
      `{bold}{underline}Egress Control{/}`,
      ` Mode          ${egressLabel}`,
      ` Blocklist     ${blLabel}`,
      ` Allowlist     ${sec.allowlistDomains} domain(s)`,
      "",
      `{bold}{underline}Token Usage{/}`,
      ` Requests      ${sec.totalRequests} total  {${errColor}-fg}${sec.errorRequests} err (${errRate}){/}`,
      ` Tokens In     ${fmtTokens(sec.inputTokens)}`,
      ` Tokens Out    ${fmtTokens(sec.outputTokens)}`,
      ` Total         {bold}${fmtTokens(totalTokens)}{/}`,
      ` Avg Latency   ${sec.avgLatencyMs > 0 ? `${sec.avgLatencyMs}ms` : "-"}`,
    ];

    if (sec.agtEnabled) {
      lines.push(
        "",
        `{bold}{underline}AGT Governance{/}`,
        ` Status        ${ok(sec.agtEnabled)} enabled`,
        ` Audit Chain   ${sec.agtAuditEntries} entries ${ok(sec.agtAuditIntegrity)} ${sec.agtAuditIntegrity ? "valid" : "COMPROMISED"}`,
        ` Known Agents  ${sec.agtKnownAgents}`,
      );
    }

    securityBox.setContent(lines.join("\n"));
  }

  function renderCluster() {
    if (!clusterData) {
      clusterNodeBox.setContent(" {gray-fg}Loading cluster data...{/}");
      clusterInfoBox.setContent(" {gray-fg}Loading...{/}");
      return;
    }

    const c = clusterData;

    // ── Node table ──
    const readyCount = c.nodes.filter((n) => n.status === "Ready").length;
    const totalNodes = c.nodes.length;
    const nodeColor = readyCount === totalNodes ? "green" : readyCount > 0 ? "yellow" : "red";
    clusterNodeBox.setLabel(` 🖥  Nodes  {${nodeColor}-fg}${readyCount}/${totalNodes} Ready{/} `);

    // Build per-pool summary
    const pools = new Map<string, NodeInfo[]>();
    for (const n of c.nodes) {
      const p = n.pool || "default";
      if (!pools.has(p)) pools.set(p, []);
      pools.get(p)!.push(n);
    }

    const nodeLines: string[] = [];
    for (const [pool, nodes] of pools) {
      const poolReady = nodes.filter((n) => n.status === "Ready").length;
      const poolColor = poolReady === nodes.length ? "green" : "yellow";
      nodeLines.push(`{bold}{underline}Pool: ${pool}{/}  {${poolColor}-fg}${poolReady}/${nodes.length} Ready{/}`);
      nodeLines.push("");

      // Column headers
      nodeLines.push(` {cyan-fg}${"Node".padEnd(38)} ${"Status".padEnd(10)} ${"CPU".padEnd(12)} ${"Memory".padEnd(12)} Version{/}`);

      for (const n of nodes) {
        const dot = n.status === "Ready" ? "{green-fg}●{/}" : "{red-fg}●{/}";
        const shortName = n.name.length > 36 ? n.name.substring(0, 36) + ".." : n.name;

        // CPU bar
        const cpuNum = parseInt(n.cpuPct, 10);
        const cpuBar = !isNaN(cpuNum) ? makeBar(cpuNum) : n.cpuPct;

        // Mem bar
        const memNum = parseInt(n.memPct, 10);
        const memBar = !isNaN(memNum) ? makeBar(memNum) : n.memPct;

        nodeLines.push(` ${dot} ${shortName.padEnd(37)} ${n.status.padEnd(10)} ${cpuBar.padEnd(12)} ${memBar.padEnd(12)} ${n.version}`);
      }
      nodeLines.push("");
    }

    clusterNodeBox.setContent(nodeLines.join("\n"));

    // ── Cluster info ──
    const apiColor = !c.apiReachable ? "red" : c.apiLatencyMs < 1000 ? "green" : "yellow";
    const apiLabel = !c.apiReachable ? "unreachable" : `${c.apiLatencyMs}ms`;

    const infoLines: string[] = [
      `{bold}{underline}API Server{/}`,
      ` Health     {${apiColor}-fg}● ${apiLabel}{/}`,
      "",
      `{bold}{underline}Resources{/}`,
    ];

    // Aggregate CPU/mem from top data
    let totalCpuMilli = 0;
    let totalMemMi = 0;
    for (const n of c.nodes) {
      const cpuStr = n.cpuCores;
      if (cpuStr.endsWith("m")) totalCpuMilli += parseInt(cpuStr, 10) || 0;
      const memStr = n.memBytes;
      if (memStr.endsWith("Mi")) totalMemMi += parseInt(memStr, 10) || 0;
    }
    infoLines.push(
      ` CPU Used   ${totalCpuMilli}m (${c.nodes.length > 0 ? (c.nodes.reduce((s, n) => s + (parseInt(n.cpuPct, 10) || 0), 0) / c.nodes.length).toFixed(0) : 0}% avg)`,
      ` Mem Used   ${(totalMemMi / 1024).toFixed(1)}Gi (${c.nodes.length > 0 ? (c.nodes.reduce((s, n) => s + (parseInt(n.memPct, 10) || 0), 0) / c.nodes.length).toFixed(0) : 0}% avg)`,
    );

    // Quotas
    if (c.quotas.length > 0) {
      infoLines.push("", `{bold}{underline}Quotas{/}`);
      for (const q of c.quotas) {
        infoLines.push(` ${q.namespace}`);
        if (q.cpuHard !== "-") infoLines.push(`   CPU  ${q.cpuUsed}/${q.cpuHard}`);
        if (q.memHard !== "-") infoLines.push(`   Mem  ${q.memUsed}/${q.memHard}`);
      }
    }

    // PVCs
    if (c.pvcs.length > 0) {
      const bound = c.pvcs.filter((p) => p.phase === "Bound").length;
      const pending = c.pvcs.filter((p) => p.phase === "Pending").length;
      const pvColor = pending > 0 ? "yellow" : "green";
      infoLines.push("", `{bold}{underline}Storage{/}`);
      infoLines.push(` PVCs  {${pvColor}-fg}${bound} bound{/}${pending > 0 ? ` {yellow-fg}${pending} pending{/}` : ""} / ${c.pvcs.length} total`);
    }

    // Warnings
    if (c.warnings.length > 0) {
      infoLines.push("", `{bold}{underline}⚠  Recent Warnings{/}`);
      for (const w of c.warnings.slice(-6)) {
        const color = ["BackOff", "OOMKilled", "ImagePullBackOff", "Evicted", "NodeNotReady"].includes(w.reason) ? "red" : "yellow";
        infoLines.push(` {${color}-fg}${w.reason}{/} ${w.object}`);
        infoLines.push(`   {gray-fg}${w.message} (${w.time} ago){/}`);
      }
    } else {
      infoLines.push("", `{green-fg}✓ No warnings{/}`);
    }

    clusterInfoBox.setContent(infoLines.join("\n"));
  }

  /** Render a small bar chart: ██░░ 34% */
  function makeBar(pct: number): string {
    const width = 6;
    const filled = Math.round((pct / 100) * width);
    const color = pct > 80 ? "red" : pct > 50 ? "yellow" : "green";
    const bar = "█".repeat(filled) + "░".repeat(width - filled);
    return `{${color}-fg}${bar}{/} ${pct}%`;
  }

  function render() {
    // Toggle panel visibility based on view mode
    if (viewMode === "cluster") {
      for (const p of agentDetailPanels) (p as any).hide();
      for (const p of clusterPanels) (p as any).show();
      renderCluster();
    } else {
      for (const p of clusterPanels) (p as any).hide();
      for (const p of agentDetailPanels) (p as any).show();
    }

    // Agent table (always visible)
    const agentData = sandboxes.map((s) => {
      const hIcon = s.health === "healthy" ? "●" :
                    s.health === "degraded" ? "●" :
                    s.health === "down" ? "●" :
                    s.health === "pending" ? "◌" : "?";
      const restartStr = s.restarts > 0 ? ` R:${s.restarts}` : "";
      return [hIcon, s.name, `${s.status}${restartStr}`, s.model, s.isolation, s.channels, s.age];
    });
    (agentTable as any).setData({
      headers: [" ", "Name", "Status", "Model", "Isolation", "Ch", "Age"],
      data: agentData.length > 0 ? agentData : [["", "(no agents)", "", "", "", "", ""]],
    });

    // Egress table — filtered to selected agent, with status indicator
    const domains = selectedEgressDomains();
    const egressData = domains.map((d) => {
      const tag = d.state === "approved" ? "✓" : "P";
      return [tag, d.domain];
    });
    const selAgent = sandboxes[(agentTable as any).rows?.selected ?? 0];
    const egressLabel = selAgent
      ? ` Egress: ${selAgent.name}  [a]pprove [d]eny [e]nforce `
      : " Egress  [a]pprove [d]eny [e]nforce ";
    (egressTable as any).setLabel(egressLabel);
    (egressTable as any).setData({
      headers: ["", "Domain"],
      data: egressData.length > 0 ? egressData : [["", "(no domains)"]],
    });

    // Security panel (follows agent selection)
    renderSecurity();

    // Sparkline
    sparkline.setData(["Agents"], [sparkData]);

    // Header
    renderHeader();

    // Status bar
    const viewTag = viewMode === "cluster"
      ? "{blue-fg}{bold}[Cluster]{/bold}{/}"
      : "{gray-fg}Cluster{/}";
    if (viewMode === "agents") {
      const focusTag = focusedPanel === "agents"
        ? "{cyan-fg}{bold}[Agents]{/bold}{/}  {gray-fg}Egress{/}"
        : "{gray-fg}Agents{/}  {yellow-fg}{bold}[Egress]{/bold}{/}";
      statusBar.setContent(
        ` ${focusTag}  ${viewTag}  │  [Tab] Focus  [c] Cluster  [↑↓] Nav  [a] Approve  [A] All  ` +
        `[e] Enforce  [n] Spawn  [x] Del  [r] Refresh  [q] Quit`,
      );
    } else {
      statusBar.setContent(
        ` ${viewTag}  │  [c] Back to Agents  [r] Refresh  [q] Quit`,
      );
    }

    // Focus border color
    if (focusedPanel === "agents") {
      (agentTable as any).style.border.fg = "cyan";
      (egressTable as any).style.border.fg = "gray";
    } else {
      (agentTable as any).style.border.fg = "gray";
      (egressTable as any).style.border.fg = "yellow";
    }

    screen.render();
  }

  // ── Refresh ───────────────────────────────────────────────────────

  async function refresh() {
    if (isRefreshing) return;
    isRefreshing = true;
    startSpinner();
    render();

    try {
      sandboxes = await fetchSandboxes();

      // Parallel: egress + security + cluster for all running sandboxes
      const running = sandboxes.filter((s) => s.podName);
      const [egressResults, secResults, cluster] = await Promise.all([
        Promise.all(running.map((s) => fetchEgressDomains(s))),
        Promise.all(running.map((s) => fetchSecurityState(s))),
        fetchClusterHealth(),
      ]);

      egressByAgent = new Map();
      for (let i = 0; i < running.length; i++) {
        egressByAgent.set(running[i].name, egressResults[i]);
      }
      securityStates = new Map();
      for (const sec of secResults) {
        securityStates.set(sec.sandbox, sec);
      }
      clusterData = cluster;

      // Sparkline
      sparkData.push(sandboxes.length);
      if (sparkData.length > 30) sparkData.shift();

      refreshCount++;
      activityLog.log(
        `{cyan-fg}↻{/} #${refreshCount}  ${sandboxes.length} agent(s)  ${totalEgressCount()} domain(s)`,
      );
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Refresh:{/} ${e.message?.substring(0, 50)}`);
    }

    isRefreshing = false;
    stopSpinner();
    render();
  }

  // ── Navigation ────────────────────────────────────────────────────

  function getActiveTable(): any {
    return focusedPanel === "agents" ? agentTable : egressTable;
  }
  function getActiveList(): any[] {
    return focusedPanel === "agents" ? sandboxes : selectedEgressDomains();
  }
  function moveSelection(delta: number) {
    const table = getActiveTable();
    const list = getActiveList();
    if (list.length === 0) return;
    const rows = (table as any).rows;
    if (!rows) return;
    const current = rows.selected ?? 0;
    const next = Math.max(0, Math.min(list.length - 1, current + delta));
    rows.select(next);
    // Update security + egress panels when agent selection changes
    if (focusedPanel === "agents") render();
    screen.render();
  }

  // ── Keyboard ──────────────────────────────────────────────────────

  screen.key(["q", "escape"], () => {
    stopSpinner();
    screen.destroy();
    process.exit(0);
  });

  screen.key(["tab"], () => {
    focusedPanel = focusedPanel === "agents" ? "egress" : "agents";
    const table = getActiveTable();
    if ((table as any).rows) (table as any).rows.focus();
    render();
  });

  screen.key(["up", "k"], () => moveSelection(-1));
  screen.key(["down", "j"], () => moveSelection(1));
  screen.key(["r"], async () => { await refresh(); });

  // Cluster view toggle
  screen.key(["c"], () => {
    viewMode = viewMode === "agents" ? "cluster" : "agents";
    render();
  });

  // Egress actions
  screen.key(["a"], async () => {
    if (focusedPanel !== "egress") return;
    const domains = selectedEgressDomains();
    if (domains.length === 0) return;
    const idx = (egressTable as any).rows?.selected ?? 0;
    const domain = domains[idx];
    if (domain && domain.state === "learned") {
      await approveDomain(domain);
      await refresh();
    }
  });
  screen.key(["d"], async () => {
    if (focusedPanel !== "egress") return;
    const domains = selectedEgressDomains();
    if (domains.length === 0) return;
    const idx = (egressTable as any).rows?.selected ?? 0;
    const domain = domains[idx];
    if (domain && domain.state === "learned") {
      await denyDomain(domain);
      await refresh();
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
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (sb) { await enforceEgress(sb); await refresh(); }
  });

  // Spawn
  screen.key(["n"], () => {
    const inputBox = blessed.textbox({
      parent: screen, top: "center", left: "center",
      width: 56, height: 3,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: " New agent name ",
      inputOnFocus: true,
    });
    inputBox.focus();
    screen.render();
    inputBox.on("submit", async (value: string) => {
      inputBox.destroy(); screen.render();
      const name = value.trim();
      if (!name) return;
      activityLog.log(`{cyan-fg}⏳ Spawning {bold}${name}{/bold}...{/}`);
      screen.render();
      try {
        await execa("azureclaw", ["add", name, "--learn-egress"], { stdio: "pipe" });
        activityLog.log(`{green-fg}✓ Spawned{/} ${name}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}✗ Spawn fail:{/} ${(e.stderr || e.message)?.substring(0, 50)}`);
      }
      await refresh();
    });
    inputBox.on("cancel", () => { inputBox.destroy(); screen.render(); });
  });

  // Model switch
  screen.key(["m"], () => {
    if (sandboxes.length === 0) return;
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
      inputBox.destroy(); screen.render();
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
    inputBox.on("cancel", () => { inputBox.destroy(); screen.render(); });
  });

  // Logs
  screen.key(["l"], async () => {
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
  screen.key(["x"], () => {
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) return;
    const confirmBox = blessed.question({
      parent: screen, top: "center", left: "center",
      width: 56, height: 5,
      border: { type: "line" },
      style: { border: { fg: "red" }, fg: "white", bg: "black" },
      label: " ⚠  Confirm Delete ",
    });
    confirmBox.ask(`Delete ${sb.name}?`, async (_err: any, answer: string) => {
      confirmBox.destroy(); screen.render();
      if (!answer) return;
      activityLog.log(`{red-fg}🗑  Deleting {bold}${sb.name}{/bold}...{/}`);
      screen.render();
      try {
        await execa("azureclaw", ["rm", sb.name], { stdio: "pipe" });
        activityLog.log(`{green-fg}✓ Deleted{/} ${sb.name}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}✗ Delete fail:{/} ${(e.stderr || e.message)?.substring(0, 50)}`);
      }
      await refresh();
    });
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

  const timer = setInterval(async () => { await refresh(); }, refreshInterval);
  screen.on("destroy", () => { clearInterval(timer); stopSpinner(); });
}

function timeSince(date: Date): string {
  const seconds = Math.floor((Date.now() - date.getTime()) / 1000);
  if (seconds < 0) return "0s";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return `${days}d`;
}

/**
 * Parse Prometheus text format and sum values for a given metric name.
 * Optionally filter by label key=value pairs.
 *
 * Example line: `azureclaw_tokens_total{direction="input",model="gpt-4",sandbox="s1"} 8500`
 */
function sumPrometheusCounter(
  text: string,
  metricName: string,
  labelFilter?: Record<string, string>,
): number {
  let total = 0;
  for (const line of text.split("\n")) {
    if (line.startsWith("#") || !line.startsWith(metricName)) continue;

    // Check label filter
    if (labelFilter) {
      let match = true;
      for (const [k, v] of Object.entries(labelFilter)) {
        if (!line.includes(`${k}="${v}"`)) { match = false; break; }
      }
      if (!match) continue;
    }

    // Extract numeric value after the closing brace (or after metric name if no labels)
    const valMatch = line.match(/\}\s+([0-9eE.+-]+)$/) || line.match(/^[^\s{]+\s+([0-9eE.+-]+)$/);
    if (valMatch) {
      total += parseFloat(valMatch[1]) || 0;
    }
  }
  return total;
}
