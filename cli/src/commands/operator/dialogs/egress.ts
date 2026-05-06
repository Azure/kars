// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Egress drawer (`Shift-E` key) — cluster-wide network egress control
 * surface that the per-panel-focus actions on the main dashboard
 * cannot offer.
 *
 * The main dashboard already has [a] / [d] / [Shift-A] / [e] / [Shift-L]
 * scoped to "the currently-focused egress panel of the currently-
 * selected agent." That works for one-at-a-time triage but doesn't
 * answer "what is my fleet's egress posture, and how do I sign-and-pin
 * the policies that are ready?"
 *
 * The drawer:
 *   - lists every sandbox with (mode, learned-count, allowlist-count)
 *     so the operator can see the whole fleet at a glance,
 *   - lets the operator [s]ign+pin the highlighted sandbox's allowlist
 *     (calls `azureclaw egress <name> --enforce` which auto-signs as of
 *     S12.g),
 *   - lets the operator [A]pprove-all-learned across **every** sandbox
 *     in one keystroke (the on-dashboard [Shift-A] only acts on the
 *     selected sandbox),
 *   - is read-only-by-default — every state-changing action confirms.
 *
 * No new APIs invented; every action shells to existing CLI verbs.
 */

import blessed from "blessed";
import { execa } from "execa";
import type { SandboxInfo } from "../types.js";
import type { EgressDomain } from "../types.js";
import type { SecurityState } from "../types.js";

interface ActivityLog { log(msg: string): void; }

/** Format an execa failure into a single-line, ANSI-stripped reason string.
 *  Many `azureclaw` subcommands write user-facing abort/error messages to
 *  STDOUT via console.log (chalk-colored), not stderr, so a stderr-only
 *  read produces empty output. Surface stderr first, then the last few
 *  meaningful stdout lines, and finally the exit code so the operator
 *  always sees the actual failure reason (not just "Command failed"). */
function formatExecError(e: unknown, maxLen: number): string {
  const err = e as { stdout?: string; stderr?: string; message?: string; exitCode?: number };
  const stripAnsi = (s: string) => s.replace(/\u001b\[[0-9;]*m/g, "");
  const stderrTail = (err.stderr ?? "").split("\n").map((l) => stripAnsi(l).trim()).filter(Boolean).slice(-1)[0] ?? "";
  const stdoutTail = (err.stdout ?? "")
    .split("\n").map((l) => stripAnsi(l).trim()).filter(Boolean).slice(-3).join(" | ");
  const detail = stderrTail || stdoutTail || err.message || "unknown failure";
  const code = err.exitCode !== undefined ? ` (exit ${err.exitCode})` : "";
  return `${detail}${code}`.substring(0, maxLen);
}

export interface EgressDrawerContext {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  screen: any;
  sandboxes: SandboxInfo[];
  egressByAgent: Map<string, EgressDomain[]>;
  securityStates: Map<string, SecurityState>;
  activityLog: ActivityLog;
  setDialogOpen: (open: boolean) => void;
  refresh: () => Promise<void>;
}

export interface FleetRow {
  name: string;
  mode: string;          // "learning" | "enforcing" | "unknown"
  learned: number;       // count of learned-state domains
  allowlist: number;     // approved domains
  signed: boolean;       // allowlistRef present (from SecurityState reasonable proxy)
}

/**
 * Build the per-sandbox egress posture rows. Pure function — exported
 * for unit testing.
 */
export function buildFleetRows(
  sandboxes: SandboxInfo[],
  egressByAgent: Map<string, EgressDomain[]>,
  securityStates: Map<string, SecurityState>,
): FleetRow[] {
  return sandboxes.map((sb) => {
    const domains = egressByAgent.get(sb.name) ?? [];
    const learned = domains.filter((d) => d.state === "learned").length;
    const allowlist = domains.filter((d) => d.state === "approved").length;
    const sec = securityStates.get(sb.name);
    return {
      name: sb.name,
      mode: sec?.egressMode ?? "unknown",
      learned,
      allowlist,
      // Approximation: a sandbox is "signed" when allowlistDomains > 0 and
      // mode is enforcing. The honest signal lives in the
      // `ClawSandbox.spec.networkPolicy.allowlistRef` field which the
      // panels framework surfaces; the drawer treats this as advisory.
      signed: !!sec && sec.egressMode === "enforcing" && (sec.allowlistDomains ?? 0) > 0,
    };
  });
}

const COL = (s: string, w: number): string => {
  if (s.length >= w) return s.substring(0, w);
  return s + " ".repeat(w - s.length);
};

export function formatFleetRow(r: FleetRow): string {
  const mode =
    r.mode === "enforcing" ? "{green-fg}enforcing{/}" :
    r.mode === "learning"  ? "{yellow-fg}learning{/}" :
                             "{gray-fg}unknown{/}";
  const sig = r.signed ? "{green-fg}signed{/}" : "{gray-fg}—{/}";
  return `  ${COL(r.name, 28)}  ${COL(r.mode === "unknown" ? "?" : "", 0)}${mode}  ${COL("", 4)}` +
         `  L:${COL(String(r.learned), 4)} A:${COL(String(r.allowlist), 4)}  ${sig}`;
}

export function openEgressDrawer(ctx: EgressDrawerContext): void {
  const { screen, sandboxes, egressByAgent, securityStates, activityLog, setDialogOpen, refresh } = ctx;
  setDialogOpen(true);

  let rows = buildFleetRows(sandboxes, egressByAgent, securityStates);
  let selected = 0;

  const dialog = blessed.box({
    parent: screen, top: "center", left: "center",
    width: "85%", height: "80%",
    border: { type: "line" },
    style: { border: { fg: "yellow" }, fg: "white", bg: "black" },
    label: " 🚦 Egress drawer — fleet-wide posture ",
    tags: true,
  });

  blessed.box({
    parent: dialog, top: 0, left: 1, right: 1, height: 3,
    tags: true, style: { fg: "white", bg: "black" },
    content:
      `  Fleet egress at a glance. Each row is one sandbox.\n` +
      `  L = learned (pending approval)   A = allowlist (approved)   signed = enforcing + allowlistRef present`,
  });

  blessed.box({
    parent: dialog, top: 3, left: 1, right: 1, height: 1,
    tags: true, style: { fg: "cyan", bg: "black", bold: true },
    content: `  ${COL("AGENT", 28)}  ${COL("MODE", 12)}  ${COL("EGRESS", 14)}  SIGN`,
  });

  const list = blessed.list({
    parent: dialog, top: 5, left: 1, right: 1, bottom: 5,
    keys: false, mouse: true, tags: true,
    style: { fg: "white", bg: "black",
      selected: { fg: "black", bg: "yellow", bold: true } },
    items: [],
  });

  const detail = blessed.box({
    parent: dialog, bottom: 2, left: 1, right: 1, height: 2,
    tags: true, style: { fg: "white", bg: "black" },
    content: "",
  });

  blessed.box({
    parent: dialog, bottom: 0, left: 1, right: 1, height: 2,
    tags: true, style: { fg: "white", bg: "black" },
    content:
      `  {bold}[s]{/bold} Sign+enforce highlighted   {bold}[A]{/bold} Approve-all-learned (entire fleet)   ` +
      `{bold}[L]{/bold} Toggle learn/enforce   {bold}[r]{/bold} Refresh   {bold}[q/Esc]{/bold} Back`,
  });

  const renderRows = () => {
    list.setItems(rows.map(formatFleetRow));
    list.select(Math.min(selected, Math.max(0, rows.length - 1)));
    const cur = rows[selected];
    if (cur) {
      const sec = securityStates.get(cur.name);
      detail.setContent(
        `  Selected: {bold}${cur.name}{/bold}    pending=${cur.learned}  allowlist=${cur.allowlist}  ` +
        `mode=${cur.mode}  ${cur.signed ? "{green-fg}signed{/}" : "{gray-fg}unsigned{/}"}\n` +
        (sec ? `  blocklist=${sec.blocklistDomains ?? 0}  blocklist-learn=${sec.blocklistLearnMode ? "yes" : "no"}` : ""),
      );
    } else {
      detail.setContent("  {gray-fg}(no sandboxes — spawn one with [n]){/}");
    }
    screen.render();
  };

  const cleanup = () => {
    screen.removeListener("keypress", onKey);
    dialog.destroy();
    screen.render();
    setTimeout(() => { setDialogOpen(false); }, 50);
  };

  const reload = async () => {
    activityLog.log(`{cyan-fg}↻ refreshing fleet egress posture...{/}`);
    await refresh();
    rows = buildFleetRows(sandboxes, egressByAgent, securityStates);
    renderRows();
  };

  const sign = async (sb: FleetRow) => {
    const ok = await confirmYesNo(ctx,
      `Sign + enforce ${sb.name}? This pushes a cosign-signed allowlist artifact and switches the sandbox to enforcing mode.`);
    if (!ok) return;

    // Pre-flight: oras + cosign are required by `azureclaw egress --enforce`
    // (auto-signs as of S12.g). Surface a clear, actionable error here rather
    // than letting the subprocess fail later with an opaque "Command failed
    // with exit code 1" — the operator otherwise sees only the ⏳ line and
    // can't tell whether the operation is still running or already failed.
    const missingTools: string[] = [];
    for (const tool of ["oras", "cosign"]) {
      try {
        await execa("which", [tool], { stdio: "pipe", timeout: 5_000 });
      } catch {
        missingTools.push(tool);
      }
    }
    if (missingTools.length > 0) {
      activityLog.log(
        `{red-fg}✗ ${sb.name}: cannot sign — missing tool(s): ${missingTools.join(", ")}.{/} ` +
        `Install: oras → https://oras.land/docs/installation, cosign → https://docs.sigstore.dev/cosign/installation. ` +
        `(Or run with --no-sign for unsigned dev mode — the controller will refuse the artifact.)`,
      );
      screen.render();
      return;
    }

    activityLog.log(`{cyan-fg}⏳ azureclaw egress ${sb.name} --enforce  {gray-fg}(auto-signs){/}{/}`);
    screen.render();
    try {
      const { stdout } = await execa("azureclaw", ["egress", sb.name, "--enforce"], {
        stdio: "pipe", timeout: 180_000,
      });
      const lastLine = stdout.split("\n").map((l) => l.trim()).filter(Boolean).slice(-1)[0] || "(no stdout)";
      activityLog.log(`{green-fg}✓ signed+enforced{/} ${sb.name}: ${lastLine}`);
    } catch (e: unknown) {
      activityLog.log(`{red-fg}✗ sign failed:{/} ${formatExecError(e, 240)}`);
    }
    await reload();
  };

  const approveAllFleet = async () => {
    const eligible = rows.filter((r) => r.learned > 0);
    if (eligible.length === 0) {
      activityLog.log(`{gray-fg}No fleet-wide learned domains to approve.{/}`);
      return;
    }
    const ok = await confirmYesNo(ctx,
      `Approve ALL learned domains across ${eligible.length} sandbox(es)? This is fleet-wide and irreversible from the drawer.`);
    if (!ok) return;
    for (const r of eligible) {
      const domains = egressByAgent.get(r.name) ?? [];
      const learned = domains.filter((d) => d.state === "learned");
      activityLog.log(`{cyan-fg}⏳ ${r.name}: approving ${learned.length} domain(s)...{/}`);
      screen.render();
      for (const d of learned) {
        try {
          // eslint-disable-next-line no-await-in-loop
          await execa("azureclaw", ["egress", r.name, "--approve", d.domain], { stdio: "pipe", timeout: 60_000 });
        } catch (e: unknown) {
          activityLog.log(`{red-fg}✗ ${r.name}/${d.domain}:{/} ${formatExecError(e, 80)}`);
        }
      }
    }
    activityLog.log(`{green-fg}✓ approve-all-fleet complete{/}`);
    await reload();
  };

  const toggleMode = async (sb: FleetRow) => {
    const target = sb.mode === "enforcing" ? "learning" : "enforce";
    const ok = await confirmYesNo(ctx, `Switch ${sb.name} to ${target} mode?`);
    if (!ok) return;
    const flag = target === "enforce" ? "--enforce" : "--learn";
    activityLog.log(`{cyan-fg}⏳ azureclaw egress ${sb.name} ${flag}{/}`);
    try {
      await execa("azureclaw", ["egress", sb.name, flag], { stdio: "pipe", timeout: 60_000 });
      activityLog.log(`{green-fg}✓ ${sb.name} → ${target}{/}`);
    } catch (e: unknown) {
      activityLog.log(`{red-fg}✗ toggle failed:{/} ${formatExecError(e, 160)}`);
    }
    await reload();
  };

  const onKey = async (_ch: unknown, key: { name?: string }) => {
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
      case "k":
        if (selected > 0) { selected--; renderRows(); }
        return;
      case "down":
      case "j":
        if (selected < rows.length - 1) { selected++; renderRows(); }
        return;
      case "s": {
        const cur = rows[selected];
        if (cur) await sign(cur);
        return;
      }
      case "A":
      case "S-a":
        await approveAllFleet();
        return;
      case "L":
      case "S-l": {
        const cur = rows[selected];
        if (cur) await toggleMode(cur);
        return;
      }
    }
  };

  screen.on("keypress", onKey);
  renderRows();
  list.focus();
  screen.render();
}

function confirmYesNo(ctx: EgressDrawerContext, label: string): Promise<boolean> {
  return new Promise((resolve) => {
    const { screen } = ctx;
    const dialog = blessed.box({
      parent: screen, top: "center", left: "center",
      width: Math.min(80, Math.max(50, label.length + 12)), height: 7,
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

export const __test = { buildFleetRows, formatFleetRow };
