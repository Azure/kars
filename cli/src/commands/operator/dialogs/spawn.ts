// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Spawn-agent dialog (`n` key) — extracted from operator.ts startDashboard
// closure (S15.e.6) so the closure stays under the §4.2 800-LOC cap.
// Body byte-identical to the original; closure-captured state is passed
// via `SpawnDialogContext`. The `if (dialogOpen) return;` guard is kept in
// the operator.ts keymap wrapper; this function unconditionally opens.

import blessed from "blessed";
import { execa } from "execa";
import { listSecretVariants } from "../../../config.js";
import type { SandboxInfo } from "../types.js";

interface ActivityLog {
  log(msg: string): void;
}

export interface SpawnDialogContext {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  screen: any;
  activityLog: ActivityLog;
  kctl: (args: string[], kubeContext: string | undefined) => string[];
  kubeContext: string | undefined;
  devMode: boolean;
  setDialogOpen: (open: boolean) => void;
  refresh: () => Promise<void>;
  learnEgress: (sb: SandboxInfo) => Promise<void>;
}

export function openSpawnDialog(ctx: SpawnDialogContext): void {
  const { screen, activityLog, kctl, kubeContext, devMode, setDialogOpen, refresh, learnEgress } = ctx;
  setDialogOpen(true);


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
          const afMatch = storedAllowFrom.find((v: { value: string }) => v.value === afVal);
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

    function close() { dialog.destroy(); screen.render(); setTimeout(() => { setDialogOpen(false); }, 50); }

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
}
