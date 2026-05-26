// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Connect-to-agent (Enter key) — extracted from operator.ts startDashboard
// closure (S15.e.7) so the closure stays under the §4.2 800-LOC cap.
// Body byte-identical to the original; closure-captured state is passed
// via `ConnectAgentContext`.

import type { SandboxInfo } from "../types.js";

interface ActivityLog { log(msg: string): void; }

export interface ConnectAgentContext {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  screen: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  agentTable: any;
  sandboxes: SandboxInfo[];
  focusedPanel: "agents" | "egress";
  activityLog: ActivityLog;
  kctl: (args: string[], kubeContext: string | undefined) => string[];
  kubeContext: string | undefined;
  devMode: boolean;
  refreshInterval: number;
  refresh: () => Promise<void>;
  render: () => void;
  setDialogOpen: (open: boolean) => void;
  setConnectedToAgent: (connected: boolean) => void;
  getRefreshTimer: () => ReturnType<typeof setInterval> | null;
  setRefreshTimer: (t: ReturnType<typeof setInterval> | null) => void;
}

export async function connectToAgent(ctx: ConnectAgentContext): Promise<void> {
  const {
    screen, agentTable, sandboxes, focusedPanel,
    activityLog, kctl, kubeContext, devMode, refreshInterval,
    refresh, render,
    setDialogOpen, setConnectedToAgent, getRefreshTimer, setRefreshTimer,
  } = ctx;
  if (focusedPanel !== "agents") return;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const idx = (agentTable as any).rows?.selected ?? 0;
  const sb = sandboxes[idx];
  if (!sb) return;

  // ── AKS sandboxes: port-forward + WebUI URL (do NOT exec into pod) ──
  // The `kars-sandbox-exec-ban` ValidatingAdmissionPolicy denies
  // exec/attach into the openclaw container. The legitimate flow (per
  // the policy's own message and `kars connect`) is:
  //   1. read the gateway-token Secret (RBAC-gated)
  //   2. kubectl port-forward to 18789
  //   3. open WebUI at http://localhost:18789/#token=...
  // We don't take over stdin or alternate buffer here — we just start
  // the port-forward in the background and surface the URL in the
  // activity log so the operator can click it from their terminal.
  if (sb.runtime === "aks") {
    const isAks = !devMode;
    if (!isAks) {
      // sb says aks but we're not pointed at AKS — refuse early.
      activityLog.log(`{red-fg}✗ ${sb.name}: AKS sandbox but operator started with --dev. Run 'kars connect ${sb.name}' from another terminal.{/}`);
      render(); screen.render();
      return;
    }
    const localPort = "18789";
    const { execa } = await import("execa");

    activityLog.log(`{cyan-fg}⟩ ${sb.name}: reading gateway-token Secret...{/}`);
    render(); screen.render();
    let gatewayToken = "";
    try {
      const { stdout: tokenB64 } = await execa("kubectl", kctl([
        "get", "secret", "-n", sb.namespace, "gateway-token",
        "-o", "jsonpath={.data.token}",
      ], kubeContext), { stdio: "pipe" });
      if (tokenB64.trim()) {
        gatewayToken = Buffer.from(tokenB64.trim(), "base64").toString("utf-8").trim();
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message.split("\n")[0] : String(e);
      activityLog.log(`{red-fg}✗ ${sb.name}: gateway-token Secret not readable: ${msg}{/}`);
      render(); screen.render();
      return;
    }
    if (!gatewayToken) {
      activityLog.log(`{red-fg}✗ ${sb.name}: gateway-token Secret empty. Sandbox running?{/}`);
      render(); screen.render();
      return;
    }

    activityLog.log(`{cyan-fg}⟩ ${sb.name}: starting port-forward localhost:${localPort} → 18789...{/}`);
    render(); screen.render();
    const pf = execa("kubectl", kctl([
      "port-forward", "-n", sb.namespace,
      `deploy/${sb.name}`, `${localPort}:18789`,
    ], kubeContext), { stdio: ["ignore", "pipe", "pipe"] });
    pf.stderr?.on("data", (chunk: Buffer) => {
      const line = chunk.toString().trim();
      if (line && /error|denied|unable|forbidden|refused|reset|EOF|lost connection|address already in use/i.test(line)) {
        activityLog.log(`{red-fg}  [kubectl] ${line}{/}`);
        render(); screen.render();
      }
    });
    pf.catch(() => {
      activityLog.log(`{yellow-fg}⏏ ${sb.name}: port-forward ended.{/}`);
      render(); screen.render();
    });

    await new Promise(r => setTimeout(r, 1500));
    const url = `http://localhost:${localPort}/#token=${gatewayToken}`;
    activityLog.log(`{green-fg}→ ${sb.name}: ${url}{/}`);
    activityLog.log(`{dim}  (Ctrl-click the URL in your terminal; port-forward stays alive.){/}`);
    render(); screen.render();
    return;
  }

  setDialogOpen(true);
  setConnectedToAgent(true);
  const sessionId = `operator-${sb.name}`;

  // Stop the refresh timer — prevents blessed from writing to stdout
  const existing = getRefreshTimer();
  if (existing) { clearInterval(existing); setRefreshTimer(null); }

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

  // Spawn PTY for proper TTY passthrough with colors. AKS path is
  // handled above (port-forward); we only reach here for local Docker
  // sandboxes where there is no admission policy to block exec.
  const nodePty = await import("node-pty");
  const connectCmd = "docker";
  const connectArgs = ["exec", "-it", sb.podName!, "openclaw", "tui"];
  void kctl; void kubeContext; // referenced only by AKS branch above

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

    setConnectedToAgent(false);
    setDialogOpen(false);

    // Restart refresh timer
    setRefreshTimer(setInterval(async () => { await refresh(); }, refreshInterval));

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
