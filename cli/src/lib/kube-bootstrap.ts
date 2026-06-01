// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * bootstrapKubeContext — explicit-opt-in kube context bootstrap.
 *
 * Auto-discovering a context silently is DANGEROUS for write commands:
 * a user with prod + staging + dev kubeconfigs would have no way to
 * notice that `kars push --apply` rolled out their controller image
 * against the wrong cluster.
 *
 * This bootstrap therefore does NOTHING by default. It only acts when
 * the user has *explicitly opted in* via one of:
 *
 *   1. `KUBECONFIG=...`  → already pointing kubectl at a file with a
 *                          current-context. We don't touch it.
 *   2. `KARS_KUBE_CONTEXT=<name>` → user picked a context by name.
 *                          We write a temp kubeconfig overlay that
 *                          selects it as current, scoped to this CLI
 *                          process tree only. The user's
 *                          `~/.kube/config` is never modified.
 *
 * When neither is set AND no `kubectl config current-context` is
 * configured, we print a loud, actionable error and exit with code 2
 * — instead of falling through to localhost:8080 garbage or silently
 * targeting an unrelated cluster.
 *
 * Skipped cheaply:
 *   - `--help` / `-V` / `--version` (no command runs)
 *   - `kubectl` not on PATH (dev-only docker users)
 */

import { execa } from "execa";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const HELP_FLAGS = new Set(["--help", "-h", "--version", "-V"]);

export async function bootstrapKubeContext(argv: string[]): Promise<void> {
  if (argv.some(a => HELP_FLAGS.has(a))) return;
  if (process.env.KUBECONFIG) return;

  try {
    await execa("which", ["kubectl"], { stdio: "pipe" });
  } catch { return; }

  const explicit = process.env.KARS_KUBE_CONTEXT?.trim();
  if (explicit) {
    writeOverlay(explicit);
    return;
  }

  // No explicit pick. Don't auto-resolve — too risky for write
  // commands (push --apply, destroy, etc). Just verify the user has
  // SOME current-context set; if not, fail loudly with a clear
  // recovery path.
  try {
    const { stdout } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
    if (stdout.trim()) return;
  } catch { /* fall through to the loud error */ }

  let available: string[] = [];
  try {
    const { stdout } = await execa("kubectl", ["config", "get-contexts", "-o", "name"], { stdio: "pipe" });
    available = stdout.trim().split("\n").filter(Boolean);
  } catch { /* no kubeconfig at all */ }

  process.stderr.write("\n  \x1b[31m✗ No kubectl current-context set.\x1b[0m\n");
  process.stderr.write("    kars needs to know which cluster to talk to — pick ONE:\n\n");
  if (available.length > 0) {
    process.stderr.write("    \x1b[36mexport KARS_KUBE_CONTEXT=<name>\x1b[0m       (per-shell, kars-only — safe)\n");
    process.stderr.write("    \x1b[36mkubectl config use-context <name>\x1b[0m    (persistent, affects every kubectl)\n\n");
    process.stderr.write("    Available contexts in your kubeconfig:\n");
    for (const c of available) process.stderr.write(`      • ${c}\n`);
  } else {
    process.stderr.write("    Your kubeconfig has no contexts. Add one with:\n");
    process.stderr.write("      \x1b[36maz aks get-credentials --resource-group <rg> --name <cluster>\x1b[0m\n");
  }
  process.stderr.write("\n");
  process.exit(2);
}

function writeOverlay(ctx: string): void {
  const home = os.homedir();
  const defaultKc = path.join(home, ".kube", "config");
  if (!fs.existsSync(defaultKc)) return;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kars-kube-"));
  const overlay = path.join(tmp, "config");
  const raw = fs.readFileSync(defaultKc, "utf-8");
  const rewritten = raw.match(/^current-context:/m)
    ? raw.replace(/^current-context:.*$/m, `current-context: ${ctx}`)
    : `${raw.trimEnd()}\ncurrent-context: ${ctx}\n`;
  fs.writeFileSync(overlay, rewritten, { mode: 0o600 });
  process.env.KUBECONFIG = overlay;
  process.on("exit", () => {
    try { fs.rmSync(tmp, { recursive: true, force: true }); } catch {}
  });
}

