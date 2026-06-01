// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * bootstrapKubeContext — global once-per-process fix for the
 * "no current kubectl context" papercut.
 *
 * Several kars commands shell out to `kubectl` (179 callsites across
 * the CLI as of writing). When the user has no current-context set
 * in their kubeconfig — typical right after `az aks get-credentials`
 * for the first time, or when they've imported multiple AKS clusters —
 * those kubectl calls silently fall through to `http://localhost:8080`
 * and either fail with malformed-HTTP-response noise or, worse,
 * succeed against an unrelated local cluster.
 *
 * Patching each callsite to pass `--context <ctx>` is unscalable.
 * Instead, do it once: write a temp kubeconfig that copies the user's
 * existing kubeconfig but selects a reachable context as current, then
 * point `KUBECONFIG` at the temp file. Every subsequent `kubectl`
 * invocation in this process tree inherits it via env.
 *
 * Skipped (cheap, no fork):
 *   - `KUBECONFIG` already explicitly set by the user
 *   - `--help` / `-V` / `--version` flag → no command will run
 *   - `kubectl` not on PATH (dev-only docker users)
 *
 * Skipped (one fork, no rewrite):
 *   - `kubectl config current-context` returns a non-empty value already
 */

import { execa } from "execa";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export async function bootstrapKubeContext(argv: string[]): Promise<void> {
  if (argv.includes("--help") || argv.includes("-h") || argv.includes("--version") || argv.includes("-V")) {
    return;
  }
  if (process.env.KUBECONFIG) return;
  try {
    await execa("which", ["kubectl"], { stdio: "pipe" });
  } catch { return; }
  try {
    const { stdout } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
    if (stdout.trim()) return;
  } catch { /* fall through — no current context */ }

  const { resolveKubeContext } = await import("./kube-context.js");
  const ctx = await resolveKubeContext();
  if (!ctx) return;

  // Default kubeconfig location. We do NOT modify it — instead write a
  // sibling temp file that references it via the (KUBECONFIG-style)
  // path-list with --current-context overridden via the temp file's
  // own `current-context` key.
  const home = os.homedir();
  const defaultKc = path.join(home, ".kube", "config");
  if (!fs.existsSync(defaultKc)) return;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kars-kube-"));
  const overlay = path.join(tmp, "config");
  // Read user's kubeconfig + override current-context. We rewrite the
  // entire file rather than using a KUBECONFIG path-list overlay,
  // because kubectl's overlay merge rules don't support overriding
  // current-context from a side file without losing the merged
  // contexts. Pure copy + targeted line replacement is simpler and
  // touches nothing else in the user's kubeconfig.
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
