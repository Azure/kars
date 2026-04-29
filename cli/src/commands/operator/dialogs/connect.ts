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
