// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Delete-agent confirm dialog (`x` key) — extracted from operator.ts
// startDashboard closure (S15.e.7) so the closure stays under the §4.2
// 800-LOC cap. Body byte-identical to the original; closure-captured
// state is passed via `DeleteDialogContext`.

import blessed from "blessed";
import { execa } from "execa";
import type { SandboxInfo } from "../types.js";

interface ActivityLog { log(msg: string): void; }

export interface DeleteDialogContext {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  screen: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  agentTable: any;
  sandboxes: SandboxInfo[];
  activityLog: ActivityLog;
  setDialogOpen: (open: boolean) => void;
  refresh: () => Promise<void>;
}

export function deleteSelectedAgent(ctx: DeleteDialogContext): void {
  const { screen, agentTable, sandboxes, activityLog, setDialogOpen, refresh } = ctx;
  if (sandboxes.length === 0) return;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const idx = (agentTable as any).rows?.selected ?? 0;
  const sb = sandboxes[idx];
  if (!sb) return;
  setDialogOpen(true);

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

  const cleanup = () => { dialog.destroy(); screen.render(); setTimeout(() => { setDialogOpen(false); }, 50); };

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
            await execa("kars", ["destroy", sb.name, "--cloud", "--yes"], { stdio: "pipe" });
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
