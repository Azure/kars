/**
 * AzureClaw Operator TUI — live terminal dashboard for managing sandboxes.
 *
 * Panels:
 *   - Agent table: name, status, model, channels, isolation, age
 *   - Egress queue: pending/learned domains to approve/deny
 *   - Activity log: recent events and alerts
 *
 * Keyboard:
 *   Tab       — cycle focus between panels
 *   ↑/↓       — navigate rows
 *   a         — approve selected egress domain
 *   d         — deny selected egress domain
 *   e         — enforce egress (lock down)
 *   n         — spawn new agent
 *   m         — switch model for selected agent
 *   l         — stream logs for selected agent
 *   r         — refresh now
 *   q / Esc   — quit
 */

import { Command } from "commander";
import { execa } from "execa";
import blessed from "blessed";
import contrib from "blessed-contrib";
import chalk from "chalk";
import { getAdminToken } from "../router-admin.js";

interface SandboxInfo {
  name: string;
  namespace: string;
  status: string;
  model: string;
  isolation: string;
  channels: string;
  age: string;
  podName: string;
}

interface EgressDomain {
  domain: string;
  sandbox: string;
  namespace: string;
}

export function operatorCommand(): Command {
  const cmd = new Command("operator");

  cmd
    .description("Live operator dashboard — manage all sandboxes from one screen")
    .option("--refresh <seconds>", "Auto-refresh interval", "10")
    .action(async (options) => {
      const refreshInterval = parseInt(options.refresh, 10) * 1000;
      await startDashboard(refreshInterval);
    });

  return cmd;
}

async function startDashboard(refreshInterval: number) {
  const screen = blessed.screen({
    smartCSR: true,
    title: "AzureClaw Operator",
    fullUnicode: true,
  });

  // ── Layout ──────────────────────────────────────────────────────────

  const grid = new contrib.grid({ rows: 12, cols: 12, screen });

  // Header
  const header = grid.set(0, 0, 1, 12, blessed.box, {
    content: " 🔱 AzureClaw Operator Dashboard",
    style: { fg: "white", bg: "blue", bold: true },
    tags: true,
  });

  // Agent table (top half)
  const agentTable = grid.set(1, 0, 5, 12, contrib.table, {
    keys: true,
    vi: true,
    fg: "white",
    label: " Agents [Tab to focus, ↑/↓ navigate] ",
    columnSpacing: 2,
    columnWidth: [30, 16, 18, 12, 10, 6],
    interactive: true,
    style: {
      border: { fg: "cyan" },
      header: { fg: "cyan", bold: true },
      cell: { selected: { bg: "blue", fg: "white" } },
    },
  });

  // Egress panel (bottom left — wider)
  const egressTable = grid.set(6, 0, 4, 8, contrib.table, {
    keys: true,
    vi: true,
    fg: "white",
    label: " Egress — Learned Domains [a=approve, d=deny, e=enforce] ",
    columnSpacing: 2,
    columnWidth: [44, 28],
    interactive: true,
    style: {
      border: { fg: "yellow" },
      header: { fg: "yellow", bold: true },
      cell: { selected: { bg: "yellow", fg: "black" } },
    },
  });

  // Activity log (bottom right)
  const activityLog = grid.set(6, 8, 4, 4, contrib.log, {
    fg: "green",
    label: " Activity Log ",
    tags: true,
    style: { border: { fg: "green" } },
    bufferLength: 50,
  });

  // Status bar
  const statusBar = grid.set(10, 0, 2, 12, blessed.box, {
    content: " [Tab] Switch  [a] Approve  [d] Deny  [e] Enforce  [n] Spawn  [m] Model  [l] Logs  [x] Delete  [r] Refresh  [q] Quit",
    style: { fg: "white", bg: "gray" },
    tags: true,
  });

  // ── State ───────────────────────────────────────────────────────────

  let sandboxes: SandboxInfo[] = [];
  let egressDomains: EgressDomain[] = [];
  let focusedPanel: "agents" | "egress" = "agents";
  let selectedAgentIdx = 0;
  let selectedEgressIdx = 0;

  // ── Data fetching ───────────────────────────────────────────────────

  async function fetchSandboxes(): Promise<SandboxInfo[]> {
    try {
      const { stdout } = await execa("kubectl", [
        "get", "clawsandbox", "-A",
        "-o", `jsonpath={range .items[*]}{.metadata.name}|{.metadata.namespace}|{.status.phase}|{.spec.inference.model}|{.spec.sandbox.isolation}|{.metadata.creationTimestamp}{\"\\n\"}{end}`,
      ], { stdio: "pipe" });

      const results: SandboxInfo[] = [];
      for (const line of stdout.trim().split("\n").filter(Boolean)) {
        const parts = line.split("|");
        const [name, , phase, model, isolation, created] = parts;
        if (!name) continue;

        const sandboxNs = `azureclaw-${name}`;
        let podStatus = phase || "Unknown";
        let podName = "";
        let channels = "";

        try {
          const { stdout: podOut } = await execa("kubectl", [
            "get", "pods", "-n", sandboxNs,
            "-o", "jsonpath={.items[0].metadata.name}|{.items[0].status.phase}|{.items[0].status.containerStatuses[*].ready}",
          ], { stdio: "pipe" });
          const [pn, pPhase, ready] = podOut.split("|");
          podName = pn || "";
          const readyCount = (ready || "").split(" ").filter((r) => r === "true").length;
          const totalCount = (ready || "").split(" ").filter(Boolean).length;
          podStatus = totalCount > 0 ? `${pPhase} (${readyCount}/${totalCount})` : (pPhase || "Pending");
        } catch { /* no pod yet */ }

        try {
          const { stdout: secretOut } = await execa("kubectl", [
            "get", "secret", `${name}-credentials`, "-n", sandboxNs,
            "-o", "jsonpath={.data}",
          ], { stdio: "pipe" });
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
          model: model || "gpt-4.1", isolation: isolation || "enhanced",
          channels, age, podName,
        });
      }
      return results;
    } catch {
      return [];
    }
  }

  async function fetchEgressDomains(sandbox: SandboxInfo): Promise<EgressDomain[]> {
    try {
      const { stdout } = await execa("kubectl", [
        "exec", "-n", sandbox.namespace, sandbox.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "http://localhost:8443/egress/learned",
      ], { stdio: "pipe" });
      const data = JSON.parse(stdout);
      return (data.domains || []).map((d: string) => ({
        domain: d,
        sandbox: sandbox.name,
        namespace: sandbox.namespace,
      }));
    } catch {
      return [];
    }
  }

  async function approveDomain(domain: EgressDomain) {
    const sb = sandboxes.find((s) => s.name === domain.sandbox);
    if (!sb || !sb.podName) {
      activityLog.log(`{red-fg}❌ No pod found for:{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", [
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/approve",
      ], { stdio: "pipe" });
      activityLog.log(`{green-fg}✅ Approved:{/} ${domain.domain} (${domain.sandbox})`);
    } catch (e: any) {
      activityLog.log(`{red-fg}❌ Approve failed:{/} ${e.message}`);
    }
  }

  async function denyDomain(domain: EgressDomain) {
    const sb = sandboxes.find((s) => s.name === domain.sandbox);
    if (!sb || !sb.podName) {
      activityLog.log(`{red-fg}❌ No pod found for:{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", [
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/deny",
      ], { stdio: "pipe" });
      activityLog.log(`{yellow-fg}🚫 Denied:{/} ${domain.domain} (${domain.sandbox})`);
    } catch (e: any) {
      activityLog.log(`{red-fg}❌ Deny failed:{/} ${e.message}`);
    }
  }

  async function enforceEgress(sandbox: SandboxInfo) {
    try {
      await execa("kubectl", [
        "exec", "-n", sandbox.namespace, sandbox.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/enforce",
      ], { stdio: "pipe" });
      activityLog.log(`{green-fg}🔒 Enforced:{/} ${sandbox.name} — egress locked down`);
    } catch (e: any) {
      activityLog.log(`{red-fg}❌ Enforce failed:{/} ${e.message}`);
    }
  }

  // ── Render ──────────────────────────────────────────────────────────

  function render() {
    // Agent table
    const agentData = sandboxes.map((s) => [
      s.name, s.status, s.model, s.isolation, s.channels, s.age,
    ]);
    agentTable.setData({
      headers: ["Name", "Status", "Model", "Isolation", "Channels", "Age"],
      data: agentData.length > 0 ? agentData : [["(no sandboxes found)", "", "", "", "", ""]],
    });

    // Egress table
    const egressData = egressDomains.map((d) => [d.domain, d.sandbox]);
    egressTable.setData({
      headers: ["Domain", "Agent"],
      data: egressData.length > 0 ? egressData : [["(no learned domains)", ""]],
    });

    // Update header with agent count
    const now = new Date().toLocaleTimeString();
    header.setContent(` 🔱 AzureClaw Operator Dashboard  │  ${sandboxes.length} agent(s)  │  ${egressDomains.length} learned domain(s)  │  ${now}`);

    // Focus indicator
    if (focusedPanel === "agents") {
      (agentTable as any).style.border.fg = "cyan";
      (egressTable as any).style.border.fg = "gray";
    } else {
      (agentTable as any).style.border.fg = "gray";
      (egressTable as any).style.border.fg = "yellow";
    }

    screen.render();
  }

  // ── Refresh loop ────────────────────────────────────────────────────

  async function refresh() {
    activityLog.log("{cyan-fg}↻ Refreshing...{/}");
    sandboxes = await fetchSandboxes();

    // Fetch egress for all sandboxes in parallel
    egressDomains = [];
    const egressResults = await Promise.all(
      sandboxes.filter((s) => s.podName).map((s) => fetchEgressDomains(s)),
    );
    for (const domains of egressResults) {
      egressDomains.push(...domains);
    }

    activityLog.log(`{cyan-fg}↻ ${sandboxes.length} agent(s), ${egressDomains.length} domain(s){/}`);
    render();
  }

  // ── Keyboard handlers ───────────────────────────────────────────────

  screen.key(["q", "escape"], () => {
    screen.destroy();
    process.exit(0);
  });

  screen.key(["tab"], () => {
    focusedPanel = focusedPanel === "agents" ? "egress" : "agents";
    if (focusedPanel === "agents") {
      (agentTable as any).focus();
    } else {
      (egressTable as any).focus();
    }
    render();
  });

  screen.key(["r"], async () => {
    await refresh();
  });

  // Approve domain
  screen.key(["a"], async () => {
    if (focusedPanel === "egress" && egressDomains.length > 0) {
      const idx = (egressTable as any).rows?.selected || 0;
      const domain = egressDomains[idx];
      if (domain) {
        await approveDomain(domain);
        await refresh();
      }
    }
  });

  // Deny domain
  screen.key(["d"], async () => {
    if (focusedPanel === "egress" && egressDomains.length > 0) {
      const idx = (egressTable as any).rows?.selected || 0;
      const domain = egressDomains[idx];
      if (domain) {
        await denyDomain(domain);
        await refresh();
      }
    }
  });

  // Enforce egress
  screen.key(["e"], async () => {
    if (sandboxes.length > 0) {
      const idx = (agentTable as any).rows?.selected || 0;
      const sandbox = sandboxes[idx];
      if (sandbox) {
        await enforceEgress(sandbox);
        await refresh();
      }
    }
  });

  // Spawn new agent
  screen.key(["n"], async () => {
    // Prompt for agent name using a simple input box
    const inputBox = blessed.textbox({
      parent: screen,
      top: "center",
      left: "center",
      width: 50,
      height: 3,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: " New agent name ",
      inputOnFocus: true,
    });
    inputBox.focus();
    screen.render();

    inputBox.on("submit", async (value: string) => {
      inputBox.destroy();
      screen.render();
      const name = value.trim();
      if (!name) return;

      activityLog.log(`{cyan-fg}⏳ Spawning ${name}...{/}`);
      screen.render();
      try {
        await execa("azureclaw", ["add", name, "--learn-egress"], { stdio: "pipe" });
        activityLog.log(`{green-fg}✅ Spawned:{/} ${name}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}❌ Spawn failed:{/} ${e.stderr || e.message}`);
      }
      await refresh();
    });

    inputBox.on("cancel", () => {
      inputBox.destroy();
      screen.render();
    });
  });

  // Switch model
  screen.key(["m"], async () => {
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected || 0;
    const sandbox = sandboxes[idx];
    if (!sandbox) return;

    const inputBox = blessed.textbox({
      parent: screen,
      top: "center",
      left: "center",
      width: 50,
      height: 3,
      border: { type: "line" },
      style: { border: { fg: "cyan" }, fg: "white", bg: "black" },
      label: ` Model for ${sandbox.name} (current: ${sandbox.model}) `,
      inputOnFocus: true,
    });
    inputBox.focus();
    screen.render();

    inputBox.on("submit", async (value: string) => {
      inputBox.destroy();
      screen.render();
      const model = value.trim();
      if (!model) return;

      activityLog.log(`{cyan-fg}⏳ Switching ${sandbox.name} → ${model}...{/}`);
      screen.render();
      try {
        await execa("azureclaw", ["model", "set", sandbox.name, model], { stdio: "pipe" });
        activityLog.log(`{green-fg}✅ Model:{/} ${sandbox.name} → ${model}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}❌ Model switch failed:{/} ${e.stderr || e.message}`);
      }
      await refresh();
    });

    inputBox.on("cancel", () => {
      inputBox.destroy();
      screen.render();
    });
  });

  // View logs
  screen.key(["l"], async () => {
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected || 0;
    const sandbox = sandboxes[idx];
    if (!sandbox) return;

    activityLog.log(`{cyan-fg}📋 Fetching logs for ${sandbox.name}...{/}`);
    screen.render();
    try {
      const { stdout } = await execa("kubectl", [
        "logs", "-n", sandbox.namespace, sandbox.podName,
        "-c", "openclaw", "--tail=20",
      ], { stdio: "pipe" });
      for (const line of stdout.split("\n").slice(-10)) {
        activityLog.log(line.substring(0, 100));
      }
    } catch (e: any) {
      activityLog.log(`{red-fg}❌ Logs failed:{/} ${e.message}`);
    }
    screen.render();
  });

  // Delete agent
  screen.key(["x"], async () => {
    if (sandboxes.length === 0) return;
    const idx = (agentTable as any).rows?.selected || 0;
    const sandbox = sandboxes[idx];
    if (!sandbox) return;

    const confirmBox = blessed.question({
      parent: screen,
      top: "center",
      left: "center",
      width: 50,
      height: 5,
      border: { type: "line" },
      style: { border: { fg: "red" }, fg: "white", bg: "black" },
      label: " ⚠️  Confirm Delete ",
    });

    confirmBox.ask(`Delete ${sandbox.name}?`, async (err: any, answer: string) => {
      confirmBox.destroy();
      screen.render();
      if (!answer) return;

      activityLog.log(`{red-fg}🗑️  Deleting ${sandbox.name}...{/}`);
      screen.render();
      try {
        await execa("azureclaw", ["rm", sandbox.name], { stdio: "pipe" });
        activityLog.log(`{green-fg}✅ Deleted:{/} ${sandbox.name}`);
      } catch (e: any) {
        activityLog.log(`{red-fg}❌ Delete failed:{/} ${e.stderr || e.message}`);
      }
      await refresh();
    });
  });

  // ── Start ───────────────────────────────────────────────────────────

  activityLog.log("{green-fg}🔱 AzureClaw Operator starting...{/}");
  render();

  await refresh();

  // Auto-refresh timer
  const timer = setInterval(async () => {
    await refresh();
  }, refreshInterval);

  screen.on("destroy", () => clearInterval(timer));
}

function timeSince(date: Date): string {
  const seconds = Math.floor((Date.now() - date.getTime()) / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return `${days}d`;
}
