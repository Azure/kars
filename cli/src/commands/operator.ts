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
 *   L         — toggle learning ↔ enforcement
 *   g         — open/close full AGT detail overlay
 *   t         — toggle topology view
 *   n         — spawn new agent
 *   m         — switch model for selected agent
 *   l         — tail logs for selected agent
 *   x         — delete selected agent (with confirmation)
 *   Enter     — connect to selected agent (shell session)
 *   c         — toggle cluster health view
 *   r         — refresh now
 *   q / Esc   — quit (or close overlay)
 */

import { Command } from "commander";
import { execa } from "execa";
import blessed from "blessed";
import contrib from "blessed-contrib";
import { listSecretVariants } from "../config.js";

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
  role: "controller" | "sub-agent";
  parent: string;  // parent agent name (empty if controller)
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
  // AGT detail — populated from /agt/audit, /agt/trust, relay/registry logs
  agtRecentAudit: string[];     // last few audit entries
  agtTrustScores: { agent: string; score: number; tier: string; interactions: number; lastSeen: string }[];
  agtRelayConnected: boolean;
  agtRegistryAgents: number;
  agtAmid: string;  // cryptographic identity from registry
  // Mesh counters (from router MeshMetrics)
  agtMeshSessions: number;
  agtMeshSent: number;
  agtMeshReceived: number;
  agtTrustUpdates: number;
  agtTotalInteractions: number;
  // Registry reputation (from agentmesh-registry)
  agtReputation: {
    score: number;       // 0.0–1.0 composite
    tier: string;
    completionRate: number;
    totalSessions: number;
    feedbackCount: number;
    avgFeedback: number;
  } | null;
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

interface MeshHealth {
  relayReady: boolean;
  registryReady: boolean;
  registryPods: number;
  registryReadyPods: number;
}

// ── Command ─────────────────────────────────────────────────────────

export function operatorCommand(): Command {
  const cmd = new Command("operator");

  cmd
    .description("Live operator dashboard — manage all sandboxes from one screen")
    .option("--refresh <seconds>", "Auto-refresh interval", "10")
    .option("--context <name>", "Kubernetes context to use")
    .option("--dev", "Dev mode — discover Docker containers instead of K8s pods")
    .action(async (options) => {
      const refreshInterval = parseInt(options.refresh, 10) * 1000;
      await startDashboard(refreshInterval, options.context, !!options.dev);
    });

  return cmd;
}

// Helper to build kubectl args with optional context
function kctl(args: string[], context?: string): string[] {
  return context ? ["--context", context, ...args] : args;
}

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
    columnWidth: [3, 38, 14, 14, 14, 6, 8],
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
  // Sandboxes: every cycle (10s). Security/egress: every 3rd (30s). Cluster: every 6th (60s).
  const TIER_DETAIL = 3;   // security + egress every 3 cycles
  const TIER_CLUSTER = 6;  // cluster health every 6 cycles

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

  // ── Data fetching ─────────────────────────────────────────────────

  async function fetchSandboxes(): Promise<SandboxInfo[]> {
    if (devMode) return fetchSandboxesDocker();
    try {
      const { stdout } = await execa("kubectl", kctl([
        "get", "clawsandbox", "-A", "-o", "json",
      ], kubeContext), { stdio: "pipe" });

      const data = JSON.parse(stdout);
      const items: any[] = data.items || [];

      // Fetch pods + secrets for ALL sandboxes in parallel (not sequentially)
      const enriched = await Promise.allSettled(items.map(async (item) => {
        const name: string = item.metadata?.name || "";
        if (!name) return null;
        const phase: string = item.status?.phase || "Unknown";
        const model: string = item.spec?.inference?.model || "gpt-4.1";
        const isolation: string = item.spec?.sandbox?.isolation || "enhanced";
        const created: string = item.metadata?.creationTimestamp || "";
        const labels: Record<string, string> = item.metadata?.labels || {};
        const parentLabel = labels["azureclaw.azure.com/parent"] || "";
        const role: "controller" | "sub-agent" = parentLabel ? "sub-agent" : "controller";

        const sandboxNs = `azureclaw-${name}`;
        let podStatus = phase;
        let podName = "";
        let podCreated = "";
        let channels = "";
        let health: HealthState = "pending";
        let restarts = 0;

        // Fetch pods and secret in parallel
        const [podResult, secretResult] = await Promise.allSettled([
          execa("kubectl", kctl([
            "get", "pods", "-n", sandboxNs, "-o", "json",
          ], kubeContext), { stdio: "pipe", timeout: 15000 }),
          execa("kubectl", kctl([
            "get", "secret", `${name}-credentials`, "-n", sandboxNs,
            "-o", "jsonpath={.data}",
          ], kubeContext), { stdio: "pipe", timeout: 10000 }),
        ]);

        if (podResult.status === "fulfilled") {
          try {
            const pods = JSON.parse(podResult.value.stdout);
            if (pods.items?.length > 0) {
              const sorted = [...pods.items].sort((a: any, b: any) => {
                const order = (p: any) => {
                  if (p.metadata?.deletionTimestamp) return 3;
                  const phase = p.status?.phase || "";
                  if (phase === "Running") return 0;
                  if (phase === "Pending") return 1;
                  return 2;
                };
                return order(a) - order(b);
              });
              const pod = sorted[0];
              const isTerminating = !!pod.metadata?.deletionTimestamp;
              podName = pod.metadata?.name || "";
              podCreated = pod.metadata?.creationTimestamp || "";
              const pPhase = isTerminating ? "Terminating" : (pod.status?.phase || "Unknown");
              const statuses: any[] = pod.status?.containerStatuses || [];
              const readyCount = statuses.filter((c: any) => c.ready).length;
              const totalCount = statuses.length;
              restarts = statuses.reduce((sum: number, c: any) => sum + (c.restartCount || 0), 0);
              podStatus = totalCount > 0 ? `${pPhase} (${readyCount}/${totalCount})` : pPhase;

              const hasCrash = statuses.some((c: any) =>
                c.state?.waiting?.reason === "CrashLoopBackOff" ||
                c.state?.waiting?.reason === "Error",
              );
              if (isTerminating) health = "degraded";
              else if (hasCrash || pPhase === "Failed") health = "down";
              else if (readyCount === totalCount && totalCount > 0) health = "healthy";
              else if (readyCount > 0) health = "degraded";
              else if (pPhase === "Pending") health = "pending";
              else health = "unknown";

              if (isTerminating || pPhase === "Pending" || readyCount === 0) podName = "";
            }
          } catch { /* bad JSON */ }
        }

        if (secretResult.status === "fulfilled") {
          const secretOut = secretResult.value.stdout;
          const chs: string[] = [];
          if (secretOut.includes("TELEGRAM")) chs.push("TG");
          if (secretOut.includes("SLACK")) chs.push("SL");
          if (secretOut.includes("DISCORD")) chs.push("DC");
          channels = chs.join(",") || "-";
        } else {
          channels = "-";
        }

        let age = "-";
        const ageSource = podCreated || created;
        if (ageSource) {
          const d = new Date(ageSource);
          if (!isNaN(d.getTime())) age = timeSince(d);
        }

        return {
          name, namespace: sandboxNs, status: podStatus,
          health, model, isolation,
          channels, age, podName, restarts,
          role, parent: parentLabel,
        } as SandboxInfo;
      }));

      const results: SandboxInfo[] = enriched
        .filter((r): r is PromiseFulfilledResult<SandboxInfo | null> => r.status === "fulfilled" && r.value !== null)
        .map((r) => r.value!);

      // Build tree: controllers sorted alphabetically, sub-agents right after their parent
      const controllers = results.filter((s) => s.role === "controller").sort((a, b) => a.name.localeCompare(b.name));
      const subAgents = results.filter((s) => s.role === "sub-agent");
      const tree: SandboxInfo[] = [];
      for (const ctrl of controllers) {
        tree.push(ctrl);
        // Attach sub-agents that belong to this controller
        const children = subAgents.filter((s) => s.parent === ctrl.name).sort((a, b) => a.name.localeCompare(b.name));
        tree.push(...children);
      }
      // Orphaned sub-agents (parent not in list) go at the end
      const placed = new Set(tree.map((s) => s.name));
      for (const s of subAgents) {
        if (!placed.has(s.name)) tree.push(s);
      }

      return tree;
    } catch {
      return [];
    }
  }

  async function fetchSandboxesDocker(): Promise<SandboxInfo[]> {
    try {
      const { stdout } = await execa("docker", [
        "ps", "-a", "--format",
        "{{.Names}}|{{.Status}}|{{.Label \"azureclaw.parent\"}}|{{.Label \"azureclaw.spawned-by\"}}|{{.CreatedAt}}",
        "--filter", "name=azureclaw-",
      ], { stdio: "pipe" });

      const results: SandboxInfo[] = [];
      for (const line of stdout.split("\n").filter(Boolean)) {
        const [containerName, status, parent, , createdAt] = line.split("|");
        if (!containerName?.startsWith("azureclaw-")) continue;
        // Skip AGT infrastructure containers
        if (containerName.includes("agt-postgres") || containerName.includes("agt-relay") || containerName.includes("agt-registry")) continue;

        const name = containerName.replace(/^azureclaw-/, "");
        const isUp = status?.startsWith("Up") ?? false;
        const health: HealthState = isUp ? "healthy" : "down";
        const podStatus = isUp ? "Running" : "Exited";
        const role: "controller" | "sub-agent" = parent ? "sub-agent" : "controller";

        let age = "-";
        if (createdAt) {
          const d = new Date(createdAt);
          if (!isNaN(d.getTime())) age = timeSince(d);
        }

        // Probe model from router
        let model = "gpt-4.1";
        if (isUp) {
          try {
            const { stdout: readyz } = await execa("docker", [
              "exec", containerName, "curl", "-s", "--max-time", "2", "http://localhost:8443/readyz",
            ], { stdio: "pipe" });
            const r = JSON.parse(readyz);
            if (r.model) model = r.model;
          } catch { /* probe fail */ }
        }

        results.push({
          name,
          namespace: containerName,
          status: podStatus,
          health,
          model,
          isolation: "standard",
          channels: "-",
          age,
          podName: containerName,
          restarts: 0,
          role,
          parent: parent || "",
        });
      }

      // Tree ordering: controllers first, sub-agents after their parent
      const controllers = results.filter((s) => s.role === "controller").sort((a, b) => a.name.localeCompare(b.name));
      const subAgents = results.filter((s) => s.role === "sub-agent");
      const tree: SandboxInfo[] = [];
      for (const ctrl of controllers) {
        tree.push(ctrl);
        tree.push(...subAgents.filter((s) => s.parent === ctrl.name));
      }
      const placed = new Set(tree.map((s) => s.name));
      for (const s of subAgents) {
        if (!placed.has(s.name)) tree.push(s);
      }

      return tree;
    } catch {
      return [];
    }
  }

  async function fetchEgressDomains(sb: SandboxInfo): Promise<EgressDomain[]> {
    if (!sb.podName) return [];
    const routerCurl = devMode
      ? (path: string) => execa("docker", [
          "exec", sb.podName!,
          "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
        ], { stdio: "pipe" })
      : (path: string) => execa("kubectl", kctl([
          "exec", "-n", sb.namespace, sb.podName!,
          "-c", "inference-router", "--",
          "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
        ], kubeContext), { stdio: "pipe", timeout: 10000 });

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
      agtRecentAudit: [],
      agtTrustScores: [],
      agtRelayConnected: false,
      agtRegistryAgents: 0,
      agtAmid: "",
      agtMeshSessions: 0,
      agtMeshSent: 0,
      agtMeshReceived: 0,
      agtTrustUpdates: 0,
      agtTotalInteractions: 0,
      agtReputation: null,
      totalRequests: 0,
      errorRequests: 0,
      inputTokens: 0,
      outputTokens: 0,
      avgLatencyMs: 0,
    };

    if (!sb.podName) return state;

    const routerExec = devMode
      ? (path: string) => execa("docker", [
          "exec", sb.podName!,
          "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
        ], { stdio: "pipe" })
      : (path: string) => execa("kubectl", kctl([
          "exec", "-n", sb.namespace, sb.podName!,
          "-c", "inference-router", "--",
          "curl", "-s", "--max-time", "3", `http://localhost:8443${path}`,
        ], kubeContext), { stdio: "pipe", timeout: 10000 });

    // Run all checks in parallel
    // In dev mode, skip K8s-only checks (NetworkPolicy, admin token)
    const k8sCheck = (args: string[]) => devMode
      ? Promise.reject("dev-mode")
      : execa("kubectl", kctl(args, kubeContext), { stdio: "pipe", timeout: 10000 });

    const checks = await Promise.allSettled([
      // 0: NetworkPolicy (K8s only)
      k8sCheck(["get", "networkpolicy", "sandbox-policy", "-n", sb.namespace, "-o", "name"]),
      // 1: Admin token secret (K8s only)
      k8sCheck(["get", "secret", "router-admin-token", "-n", sb.namespace, "-o", "name"]),
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
      // 7: /agt/audit (last entries)
      routerExec("/agt/audit"),
      // 8: /agt/reputation (registry + local trust)
      routerExec("/agt/reputation"),
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

    // agt/status — includes trust_states and inbox count
    if (checks[4].status === "fulfilled") {
      try {
        const agt = JSON.parse((checks[4].value as any).stdout);
        state.agtEnabled = agt.enabled ?? false;
        state.agtAuditEntries = agt.audit_entries || 0;
        state.agtAuditIntegrity = agt.audit_integrity ?? false;
        state.agtKnownAgents = agt.known_agents || 0;
        // Trust states from /agt/status response
        const ts = agt.trust_states || [];
        state.agtTrustScores = ts.map((a: any) => ({
          agent: a.agent_id || a.name || "unknown",
          score: a.score ?? 0,
          tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : a.score >= 200 ? "Observed" : "Anonymous"),
          interactions: a.interactions ?? 0,
          lastSeen: a.last_interaction || "",
        }));
        state.agtRegistryAgents = ts.length;
        // Mesh metrics from router MeshMetrics counters
        state.agtMeshSessions = agt.mesh_sessions || 0;
        state.agtMeshSent = agt.mesh_messages_sent || 0;
        state.agtMeshReceived = agt.mesh_messages_received || 0;
        state.agtTrustUpdates = agt.trust_updates || 0;
        state.agtTotalInteractions = agt.total_interactions || 0;
        // If no trust states but governance is enabled, show self
        if (state.agtEnabled && ts.length === 0) {
          const threshold = agt.trust_threshold ?? 500;
          state.agtTrustScores = [{
            agent: agt.sandbox || sb.name,
            score: 500,
            tier: "Known (self)",
            interactions: 0,
            lastSeen: "",
          }];
          state.agtRegistryAgents = 1;
        }
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

    // /agt/audit — extract last few entries
    if (checks[7].status === "fulfilled") {
      try {
        const audit = JSON.parse((checks[7].value as any).stdout);
        const entries = Array.isArray(audit) ? audit : audit.entries || [];
        state.agtRecentAudit = entries.slice(-5).map((e: any) => {
          const action = e.action || e.type || "unknown";
          const agent = e.agent_id || e.agent || "";
          const tool = e.tool || "";
          const result = e.result || e.decision || "";
          let ts = "";
          if (e.timestamp) {
            const d = new Date(e.timestamp);
            ts = isNaN(d.getTime()) ? "" : d.toLocaleTimeString();
          }
          const parts = [action];
          if (tool) parts.push(`[${tool}]`);
          if (agent) parts.push(agent.substring(0, 16));
          if (result) parts.push(`→ ${result}`);
          return ts ? `${ts} ${parts.join(" ")}` : parts.join(" ");
        });
      } catch { /* parse fail */ }
    }

    // /agt/reputation — registry score + local trust
    if (checks[8].status === "fulfilled") {
      try {
        const rep = JSON.parse((checks[8].value as any).stdout);
        // AMID (cryptographic identity from registry lookup)
        if (rep.amid) state.agtAmid = rep.amid;
        // Registry reputation (from agentmesh-registry Postgres)
        if (rep.registry) {
          const r = rep.registry;
          state.agtReputation = {
            score: r.score ?? 0,
            tier: r.tier || "unknown",
            completionRate: r.completion_rate ?? 0,
            totalSessions: r.total_sessions ?? 0,
            feedbackCount: r.feedback_count ?? 0,
            avgFeedback: r.average_feedback ?? 0,
          };
        }
        // Local trust store (from router in-memory)
        const local = rep.local_trust || [];
        if (local.length > 0) {
          state.agtTrustScores = local.map((a: any) => ({
            agent: a.agent_id || a.name || "unknown",
            score: a.score ?? 0,
            tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : "Anonymous"),
            interactions: a.interactions ?? 0,
            lastSeen: a.last_interaction || "",
          }));
          state.agtRegistryAgents = local.length;
        }
      } catch { /* parse fail */ }
    }

    return state;
  }

  // ── Fast AGT Poll (every cycle) ─────────────────────────────────
  // Lightweight: only fetches /agt/status per sandbox to keep trust scores
  // and mesh counters alive between full security refreshes.

  async function fetchAgtQuick(sb: SandboxInfo): Promise<void> {
    if (!sb.podName) return;
    const existing = securityStates.get(sb.name);
    if (!existing?.agtEnabled) return;

    try {
      const { stdout } = devMode
        ? await execa("docker", [
            "exec", sb.podName,
            "curl", "-s", "--max-time", "2", "http://localhost:8443/agt/status",
          ], { stdio: "pipe" })
        : await execa("kubectl", kctl([
            "exec", "-n", sb.namespace, sb.podName,
            "-c", "inference-router", "--",
            "curl", "-s", "--max-time", "2", "http://localhost:8443/agt/status",
          ], kubeContext), { stdio: "pipe", timeout: 8000 });

      const agt = JSON.parse(stdout);
      const ts = agt.trust_states || [];
      if (ts.length > 0) {
        existing.agtTrustScores = ts.map((a: any) => ({
          agent: a.agent_id || a.name || "unknown",
          score: a.score ?? 0,
          tier: a.tier || (a.score >= 800 ? "Sovereign" : a.score >= 600 ? "Verified" : a.score >= 400 ? "Known" : a.score >= 200 ? "Observed" : "Anonymous"),
          interactions: a.interactions ?? 0,
          lastSeen: a.last_interaction || "",
        }));
        existing.agtRegistryAgents = ts.length;
      }
      existing.agtMeshSessions = agt.mesh_sessions || 0;
      existing.agtMeshSent = agt.mesh_messages_sent || 0;
      existing.agtMeshReceived = agt.mesh_messages_received || 0;
      existing.agtTrustUpdates = agt.trust_updates || 0;
      existing.agtTotalInteractions = agt.total_interactions || 0;
      existing.agtAuditEntries = agt.audit_entries || 0;
      existing.agtAuditIntegrity = agt.audit_integrity ?? false;
    } catch { /* non-fatal — full refresh on next TIER_DETAIL cycle */ }
  }

  // ── Mesh Health ──────────────────────────────────────────────────

  async function fetchMeshHealth(): Promise<MeshHealth> {
    const result: MeshHealth = { relayReady: false, registryReady: false, registryPods: 0, registryReadyPods: 0 };
    if (devMode) {
      try {
        const { stdout } = await execa("docker", ["ps", "--filter", "name=relay", "--filter", "status=running", "--format", "{{.Names}}"], { stdio: "pipe", timeout: 5000 });
        result.relayReady = stdout.trim().length > 0;
      } catch {}
      try {
        const { stdout } = await execa("docker", ["ps", "--filter", "name=registry", "--filter", "status=running", "--format", "{{.Names}}"], { stdio: "pipe", timeout: 5000 });
        result.registryReady = stdout.trim().length > 0;
        result.registryPods = result.registryReady ? 1 : 0;
        result.registryReadyPods = result.registryPods;
      } catch {}
      return result;
    }
    try {
      const { stdout } = await execa("kubectl", kctl([
        "get", "pods", "-n", "agentmesh", "-o",
        `jsonpath={range .items[*]}{.metadata.labels.app}{"||"}{.status.conditions[?(@.type=="Ready")].status}{"\\n"}{end}`,
      ], kubeContext), { stdio: "pipe", timeout: 10000 });
      for (const line of stdout.trim().split("\n")) {
        if (!line) continue;
        const [app, ready] = line.split("||");
        if (app?.includes("relay") && ready === "True") result.relayReady = true;
        if (app?.includes("registry")) {
          result.registryPods++;
          if (ready === "True") { result.registryReady = true; result.registryReadyPods++; }
        }
      }
    } catch {}
    return result;
  }

  // ── Cluster Health ────────────────────────────────────────────────

  async function fetchClusterHealth(): Promise<ClusterHealth> {
    const result: ClusterHealth = {
      apiLatencyMs: -1, apiReachable: false,
      nodes: [], quotas: [], pvcs: [], warnings: [],
    };

    if (devMode) {
      result.apiReachable = true;
      result.apiLatencyMs = 0;
      try {
        const { stdout } = await execa("docker", ["info", "--format", "{{.NCPU}}|{{.MemTotal}}|{{.ServerVersion}}"], { stdio: "pipe" });
        const [cpus, mem, version] = stdout.trim().split("|");
        const memGb = (parseInt(mem || "0", 10) / 1073741824).toFixed(1);
        result.nodes = [{
          name: "docker-host",
          pool: "local",
          status: "Ready",
          version: version || "",
          cpuCores: `${cpus}`,
          cpuPct: "-",
          memBytes: `${memGb}Gi`,
          memPct: "-",
          os: "Docker",
          runtime: `docker://${version || ""}`,
        }];
      } catch { /* docker info fail */ }
      return result;
    }

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
      // Persist to CRD so the controller preserves the mode across restarts
      await execa("kubectl", kctl([
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { learnEgress: false } } }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{green-fg}🔒 Enforced{/} ${sb.name}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Enforce fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function learnEgress(sb: SandboxInfo) {
    if (!sb.podName) return;
    try {
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/learn",
      ], kubeContext), { stdio: "pipe" });
      // Persist to CRD so the controller preserves the mode across restarts
      await execa("kubectl", kctl([
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { learnEgress: true } } }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{yellow-fg}📖 Learning{/} ${sb.name}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Learn fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  // ── Rendering ─────────────────────────────────────────────────────

  const ok = (v: boolean) => v ? "{green-fg}●{/}" : "{red-fg}●{/}";

  function healthSummary(): string {
    const total = sandboxes.length;
    if (total === 0) return "{gray-fg}no agents{/}";
    const h = sandboxes.filter((s) => s.health === "healthy").length;
    const d = sandboxes.filter((s) => s.health === "degraded").length;
    const x = sandboxes.filter((s) => s.health === "down").length;
    const parts: string[] = [];
    if (h > 0) parts.push(`{green-fg}${h} healthy{/}`);
    if (d > 0) parts.push(`{yellow-fg}${d} degraded{/}`);
    if (x > 0) parts.push(`{red-fg}${x} down{/}`);
    return `${total} agent${total === 1 ? "" : "s"} (${parts.join(", ")})`;
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

    // Mesh health indicator
    let meshTag = "";
    if (meshHealth) {
      const relayColor = meshHealth.relayReady ? "green" : "red";
      const regColor = meshHealth.registryReady ? (meshHealth.registryReadyPods < meshHealth.registryPods ? "yellow" : "green") : "red";
      const regCount = meshHealth.registryPods > 0 ? ` (${meshHealth.registryReadyPods}/${meshHealth.registryPods})` : "";
      meshTag = `{${relayColor}-fg}●{/} relay  {${regColor}-fg}●{/} registry${regCount}  │  `;
    }

    const viewLabel = viewMode === "cluster" ? "{blue-fg}{bold}[CLUSTER]{/bold}{/}  │  " : "";
    const title = ` ${spin}{bold}AzureClaw Operator{/bold}  │  ${ctx}  │  ${viewLabel}`;
    const stats = `${clusterTag}${meshTag}${healthSummary()}  │  ${totalEgressCount()} domain(s)  │  {gray-fg}${now}{/}`;
    const shortStats = `${healthSummary()}  │  {gray-fg}${now}{/}`;

    // Measure visible width (strip blessed tags, account for double-wide emoji)
    const visWidth = (s: string) => {
      const plain = s.replace(/\{[^}]*\}/g, "");
      // Each emoji/surrogate pair occupies ~2 columns
      let w = 0;
      for (const ch of plain) {
        w += ch.codePointAt(0)! > 0xffff ? 2 : 1;
      }
      return w;
    };
    const maxW = (screen.width as number) - 2;
    const full = title + stats;
    if (visWidth(full) > maxW) {
      header.setContent(title + shortStats);
    } else {
      header.setContent(full);
    }
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
        `{bold}{underline}AGT{/}`,
        ` ${ok(sec.agtEnabled)} enabled  │  see AGT panel →`,
      );
    }

    securityBox.setContent(lines.join("\n"));
  }

  /** Full AGT detail — used in the overlay panel. */
  function renderAGTFull(sb: SandboxInfo): string {
    const sec = securityStates.get(sb.name);
    if (!sec) return `{bold}${sb.name}{/}\n{gray-fg}Polling...{/}`;
    if (!sec.agtEnabled) return "{gray-fg}AGT not enabled{/}\n{gray-fg}Use --governance flag{/}";

    const activePeerCount = sec.agtTrustScores.filter((t: any) =>
      t.agent !== sb.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)
    ).length;

    const lines: string[] = [
      `{bold}${sb.name}{/}` + (sec.agtAmid ? ` {gray-fg}${sec.agtAmid}{/}` : ""),
      ` Chain   ${sec.agtAuditEntries} entries ${ok(sec.agtAuditIntegrity)} ${sec.agtAuditIntegrity ? "valid" : "BROKEN"}`,
      ` Agents  ${sec.agtRegistryAgents > 0 ? sec.agtRegistryAgents : activePeerCount} known`,
      ` Mesh    ${sec.agtMeshSessions} sessions  ↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}  ${sec.agtTrustUpdates} trust updates`,
    ];

    if (sec.agtReputation) {
      const r = sec.agtReputation;
      const pct = (r.score * 100).toFixed(0);
      const c = r.score >= 0.7 ? "green" : r.score >= 0.5 ? "yellow" : "red";
      lines.push("", `{bold}Reputation{/} {gray-fg}(registry){/}`);
      lines.push(` {${c}-fg}${pct}%{/} ${r.tier}  ${r.totalSessions} sessions  ${r.feedbackCount} reviews`);
      if (r.totalSessions > 0) {
        lines.push(` Completion ${(r.completionRate * 100).toFixed(0)}%  Avg ${r.avgFeedback.toFixed(2)}`);
      }
    } else {
      lines.push("", `{gray-fg}Reputation  awaiting first session{/}`);
    }

    if (sec.agtTrustScores.length > 0) {
      const self = sec.agtTrustScores.find((t) => t.agent === sb.name);
      const peers = sec.agtTrustScores.filter((t) =>
        t.agent !== sb.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)
      );

      if (peers.length > 0) {
        lines.push("", `{bold}Mesh Traffic{/}`);
        for (const t of peers) {
          const c = t.score >= 600 ? "green" : t.score >= 400 ? "yellow" : "red";
          const filled = Math.round(t.score / 100);
          const bar = "█".repeat(filled) + "░".repeat(10 - filled);
          let ago = "";
          if (t.lastSeen) {
            const d = new Date(/^\d+Z$/.test(t.lastSeen) ? Number(t.lastSeen.slice(0, -1)) * 1000 : t.lastSeen);
            const ms = Date.now() - d.getTime();
            if (!isNaN(ms) && ms >= 0) {
              if (ms < 60_000) ago = `${Math.round(ms / 1000)}s ago`;
              else if (ms < 3_600_000) ago = `${Math.round(ms / 60_000)}m ago`;
              else ago = `${Math.round(ms / 3_600_000)}h ago`;
            }
          }
          const name = t.agent;
          lines.push(` {${c}-fg}${bar}{/} ${t.score} ${name}`);
          lines.push(`   ${t.tier} · ${t.interactions} msg${t.interactions !== 1 ? "s" : ""}${ago ? ` · ${ago}` : ""}`);
        }
        const selfName = self?.agent || sb.name;
        lines.push("");
        for (const t of peers) {
          const peerName = t.agent;
          const arrow = t.interactions > 0 ? `═══⟐ ${t.interactions} msg${t.interactions !== 1 ? "s" : ""} ⟐═══` : `─── idle ───`;
          lines.push(` {cyan-fg}${selfName}{/} ${arrow} {green-fg}${peerName}{/}`);
        }
      } else if (self) {
        lines.push("", `{gray-fg}No peer agents yet{/}`);
      }
    }

    if (sec.agtRecentAudit.length > 0) {
      lines.push("", "{bold}Audit{/}");
      for (const entry of sec.agtRecentAudit) {
        lines.push(` {gray-fg}${entry}{/}`);
      }
    } else {
      lines.push("", "{gray-fg}No audit entries yet{/}");
    }

    return lines.join("\n");
  }

  /** Compact AGT summary for the small panel. */
  function renderAGT() {
    const idx = (agentTable as any).rows?.selected ?? 0;
    const sb = sandboxes[idx];
    if (!sb) {
      agtPanel.setContent("{gray-fg}No agent selected{/}");
      return;
    }

    const sec = securityStates.get(sb.name);
    if (!sec) {
      agtPanel.setContent(`{bold}${sb.name}{/}\n{gray-fg}Polling...{/}`);
      return;
    }

    if (!sec.agtEnabled) {
      agtPanel.setContent("{gray-fg}AGT not enabled{/}\n{gray-fg}Use --governance flag{/}");
      return;
    }

    const mode = sec.egressMode === "enforcing" ? "{green-fg}enforcing{/}" : "{yellow-fg}learning{/}";
    const peers = sec.agtTrustScores.filter((t) =>
      t.agent !== sb.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)
    );

    const lines: string[] = [
      `{bold}${sb.name}{/}` + (sec.agtAmid ? ` {gray-fg}${sec.agtAmid.substring(0, 12)}…{/}` : ""),
      ` ${mode}  ${sec.agtMeshSessions} sessions  ↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}`,
      ` ${peers.length} peer${peers.length !== 1 ? "s" : ""}`,
    ];

    for (const t of peers) {
      const c = t.score >= 600 ? "green" : t.score >= 400 ? "yellow" : "red";
      const filled = Math.round(t.score / 100);
      const bar = "█".repeat(filled) + "░".repeat(4 - Math.min(filled, 4));
      lines.push(` {${c}-fg}${bar}{/} ${t.score} ${t.agent}`);
    }

    if (peers.length === 0) {
      lines.push(` {gray-fg}no peers yet{/}`);
    }

    lines.push(`{gray-fg}[g] full detail{/}`);

    agtPanel.setContent(lines.join("\n"));
  }

  function renderTopology() {
    if (sandboxes.length === 0) {
      topologyBox.setContent("{gray-fg}No agents{/}");
      return;
    }

    const parents = sandboxes.filter((s) => s.role !== "sub-agent");
    const children = sandboxes.filter((s) => s.role === "sub-agent");
    const totalMesh = [...securityStates.values()].reduce((n, s) => n + s.agtMeshSessions, 0);

    const lines: string[] = [];
    lines.push(`{bold}Mesh Topology{/}  ${sandboxes.length} agent${sandboxes.length !== 1 ? "s" : ""}  ·  ${totalMesh} session${totalMesh !== 1 ? "s" : ""}  ·  {gray-fg}[t] back to table{/}`);
    lines.push("");

    function statusIcon(health: string): string {
      return health === "healthy" ? "{green-fg}●{/}" :
             health === "pending" ? "{yellow-fg}◌{/}" :
             health === "degraded" ? "{yellow-fg}◐{/}" : "{red-fg}✗{/}";
    }

    // Fixed column width for all boxes — keeps alignment clean at scale
    const COL_W = 26;  // inner content width
    const BOX_W = COL_W + 4; // +4 for "│ " and " │"
    const CELL_W = BOX_W + 2; // +2 gap between columns

    function padC(text: string, w: number): string {
      // Strip blessed tags for length calculation
      const plain = text.replace(/\{[^}]+\}/g, "");
      const pad = Math.max(0, w - plain.length);
      const left = Math.floor(pad / 2);
      return " ".repeat(left) + text + " ".repeat(pad - left);
    }

    // Visual width of a string, counting emoji (surrogate pairs) as 2 cells
    function visualLen(s: string): number {
      const plain = s.replace(/\{[^}]+\}/g, "");
      let w = 0;
      for (const ch of plain) {
        w += ch.codePointAt(0)! > 0xFFFF ? 2 : 1;
      }
      return w;
    }

    // Fit string to exactly w visual columns (pad or truncate)
    function fitVis(s: string, w: number): string {
      const vw = visualLen(s);
      if (vw <= w) return s + " ".repeat(w - vw);
      let used = 0;
      let result = "";
      let i = 0;
      while (i < s.length) {
        if (s[i] === "{") {
          const end = s.indexOf("}", i);
          if (end !== -1) { result += s.slice(i, end + 1); i = end + 1; continue; }
        }
        const cp = s.codePointAt(i)!;
        const cw = cp > 0xFFFF ? 2 : 1;
        if (used + cw > w - 1) break;
        result += String.fromCodePoint(cp);
        i += cp > 0xFFFF ? 2 : 1;
        used += cw;
      }
      return result + "…" + " ".repeat(Math.max(0, w - used - 1));
    }

    function makeBox(name: string, icon: string, line2: string, line3: string): string[] {
      const border = "─".repeat(COL_W + 2);
      return [
        `┌${border}┐`,
        `│ ${icon} ${fitVis(name, COL_W - 2)} │`,
        `│ ${fitVis(line2, COL_W)} │`,
        `│ ${fitVis(line3, COL_W)} │`,
        `└${border}┘`,
      ];
    }

    for (const p of parents) {
      const sec = securityStates.get(p.name);
      const icon = statusIcon(p.health);
      const mode = sec?.egressMode === "enforcing" ? "{green-fg}enforce{/}" :
                   sec?.egressMode === "learning" ? "{yellow-fg}learn{/}" : "";
      const meshInfo = sec ? `↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}` : "";
      const peerCount = sec?.agtTrustScores.filter((t) => t.agent !== p.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)).length || 0;

      const pBox = makeBox(p.name, icon, `${p.model}  ${mode}`, `${peerCount} peer${peerCount !== 1 ? "s" : ""}  ${meshInfo}  ${p.age}`);
      for (const l of pBox) lines.push(`  ${l}`);

      const subs = children.filter((c) => c.parent === p.name);
      if (subs.length > 0) {
        // Vertical connector from parent center
        const parentCenter = Math.floor(BOX_W / 2) + 2; // +2 for indent
        lines.push(" ".repeat(parentCenter) + "│");

        if (subs.length === 1) {
          // Single child — straight line down
          lines.push(" ".repeat(parentCenter) + "│");
          const childSec = securityStates.get(subs[0].name);
          const ci = statusIcon(subs[0].health);
          const cMesh = childSec ? `↑${childSec.agtMeshSent} ↓${childSec.agtMeshReceived}` : "";
          const cBox = makeBox(subs[0].name, ci, subs[0].model, cMesh);
          // Center single child under parent
          const childIndent = Math.max(2, parentCenter - Math.floor(BOX_W / 2));
          for (const l of cBox) lines.push(" ".repeat(childIndent) + l);
        } else {
          // Multiple children — horizontal bar with drops
          // Each child occupies CELL_W chars; center the group under parent
          const totalGroupW = subs.length * CELL_W - 2; // -2 because last has no trailing gap
          const groupStart = Math.max(4, parentCenter - Math.floor(totalGroupW / 2));

          // Horizontal bar: ├──┬──┬──┤ centered under parent's │
          let bar = " ".repeat(groupStart);
          for (let i = 0; i < subs.length; i++) {
            const mid = Math.floor(BOX_W / 2);
            if (i === 0) {
              bar += "┌" + "─".repeat(mid);
            } else {
              bar += "─".repeat(mid) + "┬";
              if (i < subs.length - 1) {
                bar += "─".repeat(CELL_W - mid - 1);
              }
            }
            if (i === subs.length - 1 && i > 0) {
              bar += "─".repeat(mid) + "┐";
            }
            if (i === 0 && subs.length > 1) {
              bar += "─".repeat(CELL_W - mid - 1);
            }
          }
          lines.push(bar);

          // Drop stubs: │ at center of each column
          let stubs = " ".repeat(groupStart);
          for (let i = 0; i < subs.length; i++) {
            const mid = Math.floor(BOX_W / 2);
            stubs += " ".repeat(mid) + "│" + " ".repeat(CELL_W - mid - 1);
          }
          lines.push(stubs);

          // Render child boxes side-by-side
          const childBoxes: string[][] = [];
          for (const s of subs) {
            const childSec = securityStates.get(s.name);
            const ci = statusIcon(s.health);
            const cMesh = childSec ? `↑${childSec.agtMeshSent} ↓${childSec.agtMeshReceived}` : "";
            childBoxes.push(makeBox(s.name, ci, s.model, cMesh));
          }
          for (let row = 0; row < 5; row++) {
            let line = " ".repeat(groupStart);
            for (let i = 0; i < childBoxes.length; i++) {
              line += childBoxes[i][row];
              if (i < childBoxes.length - 1) line += "  ";
            }
            lines.push(line);
          }
        }

        // Peer-to-peer mesh links
        const peerLinks: string[] = [];
        for (const s of subs) {
          const childSec = securityStates.get(s.name);
          const peers = childSec?.agtTrustScores.filter((t) =>
            t.agent !== s.name && subs.some((sub) => sub.name === t.agent) && t.interactions > 0
          ) || [];
          for (const peer of peers) {
            const key = [s.name, peer.agent].sort().join(":");
            if (!peerLinks.includes(key)) {
              peerLinks.push(key);
              const c = peer.score >= 600 ? "green" : peer.score >= 400 ? "yellow" : "red";
              lines.push(`         {${c}-fg}⟷{/} ${s.name} ↔ ${peer.agent} {gray-fg}(${peer.interactions} msg${peer.interactions !== 1 ? "s" : ""}, trust: ${peer.score}){/}`);
            }
          }
        }
      }

      lines.push("");
    }

    // Orphan sub-agents (parent destroyed but children remain)
    const orphans = children.filter((c) => !parents.some((p) => p.name === c.parent));
    if (orphans.length > 0) {
      lines.push("{gray-fg}─── Orphaned agents ───{/}");
      for (const s of orphans) {
        const icon = statusIcon(s.health);
        lines.push(`  ${icon} ${s.name} {gray-fg}(${s.model}) parent: ${s.parent || "?"}{/}`);
      }
    }

    topologyBox.setContent(lines.join("\n"));
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

    // Agent table — with tree hierarchy
    const agentData = sandboxes.map((s) => {
      const hIcon = s.health === "healthy" ? "●" :
                    s.health === "degraded" ? "●" :
                    s.health === "down" ? "●" :
                    s.health === "pending" ? "◌" : "?";
      const restartStr = s.restarts > 0 ? ` R:${s.restarts}` : "";
      // Tree prefix: sub-agents show parent relationship
      let displayName = s.name;
      if (s.role === "sub-agent") {
        displayName = `└ ${s.name} (sub-agent)`;
      }
      return [hIcon, displayName, `${s.status}${restartStr}`, s.model, s.isolation, s.channels, s.age];
    });
    (agentTable as any).setData({
      headers: [" ", "Name", "Status", "Model", "Isolation", "Ch", "Age"],
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
    const viewTag = viewMode === "cluster"
      ? "{blue-fg}{bold}[Cluster]{/bold}{/}"
      : viewMode === "topology"
        ? "{cyan-fg}{bold}[Topology]{/bold}{/}"
        : "{gray-fg}Cluster{/}";
    const topoTag = viewMode === "topology"
      ? "{cyan-fg}{bold}[Topology]{/bold}{/}"
      : "{gray-fg}Topology{/}";
    if (viewMode === "agents") {
      const focusTag = focusedPanel === "agents"
        ? "{cyan-fg}{bold}[Agents]{/bold}{/}  {gray-fg}Egress{/}"
        : "{gray-fg}Agents{/}  {yellow-fg}{bold}[Egress]{/bold}{/}";
      statusBar.setContent(
        ` ${focusTag}  ${viewTag}  ${topoTag}  │  [Tab] Focus  [↑↓] Nav  [Enter] Connect  [c] Cluster  [t] Topology  ` +
        `[a] Approve  [A] All  [d] Del/Deny  [e] Enforce  [L] Learn/Enforce  [g] AGT  [n] Spawn  [r] Refresh  [q] Quit`,
      );
    } else if (viewMode === "topology") {
      statusBar.setContent(
        ` ${topoTag}  │  [t] Back to Agents  [c] Cluster  [r] Refresh  [q] Quit`,
      );
    } else {
      statusBar.setContent(
        ` ${viewTag}  │  [c] Back to Agents  [t] Topology  [r] Refresh  [q] Quit`,
      );
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
      sandboxes = await fetchSandboxes();
      const running = sandboxes.filter((s) => s.podName);

      // Tiered: only fetch detail data every Nth cycle to reduce API load
      const fetchDetail = refreshCount === 0 || refreshCount % TIER_DETAIL === 0;
      const fetchCluster = refreshCount === 0 || refreshCount % TIER_CLUSTER === 0;

      const promises: Promise<any>[] = [];

      if (fetchDetail) {
        promises.push(
          Promise.allSettled(running.map((s) => fetchEgressDomains(s))).then((settled) => {
            egressByAgent = new Map();
            for (let i = 0; i < running.length; i++) {
              const r = settled[i];
              if (r.status === "fulfilled") egressByAgent.set(running[i].name, r.value);
            }
          }),
          Promise.allSettled(running.map((s) => fetchSecurityState(s))).then((settled) => {
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
          Promise.allSettled(running.map((s) => fetchAgtQuick(s))),
        );
      }

      if (fetchCluster) {
        promises.push(
          fetchClusterHealth().then((d) => { clusterData = d; }).catch(() => {}),
          fetchMeshHealth().then((d) => { meshHealth = d; }).catch(() => {}),
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
            if (devMode) {
              await execa("docker", ["rm", "-f", sb.podName!], { stdio: "pipe" });
            } else {
              await execa("azureclaw", ["destroy", sb.name, "--yes"], { stdio: "pipe" });
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
