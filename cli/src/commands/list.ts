// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";

interface ClusterTarget {
  context: string;
  label: string;
}

interface SandboxRow {
  name: string;
  phase: string;
  runtime: string;
  model: string;
  isolation: string;
  ns: string;
}

export function listCommand(): Command {
  const cmd = new Command("list");

  cmd
    .description("List all Kars sandboxes (Docker + every reachable kube context)")
    .option("--aks-only", "Only show AKS sandboxes")
    .option("--docker-only", "Only show local Docker sandboxes")
    .option("--context <name>", "Limit kube query to a single context")
    .action(async (options) => {
      const { execa } = await import("execa");
      const blue = chalk.hex("#0078D4");
      let found = false;

      console.log(blue(`\n  Kars · Sandbox Inventory\n`));

      // ── Local Docker sandboxes ──
      if (!options.aksOnly) {
        try {
          const { stdout } = await execa("docker", [
            "ps", "-a",
            "--filter", "name=kars-",
            "--format",
            "{{.Names}}\t{{.Status}}\t{{.Image}}\t{{.Label \"io.x-k8s.kind.cluster\"}}\t{{.Label \"io.x-k8s.kind.role\"}}",
          ], { stdio: "pipe" });

          const rows: { name: string; status: string; image: string }[] = [];
          for (const line of stdout.trim().split("\n").filter(Boolean)) {
            const [name, status, image, kindCluster, kindRole] = line.split("\t");
            if (!name?.startsWith("kars-")) continue;
            // Skip kind cluster nodes — they match name=kars- when the
            // local-k8s cluster is named "kars-*" (default for
            // `kars dev --target local-k8s`). Detect via the kind
            // labels rather than relying on the image name.
            if (kindCluster || kindRole) continue;
            // Skip AGT infrastructure containers
            if (name.includes("agt-postgres") || name.includes("agt-relay") || name.includes("agt-registry")) continue;
            rows.push({ name, status, image });
          }

          if (rows.length > 0) {
            console.log(chalk.bold("  Local (Docker)"));
            console.log(chalk.dim("  ─────────────────────────────────────────────────────────────────"));
            for (const r of rows) {
              const agentName = r.name.replace(/^kars-/, "");
              const isUp = r.status.toLowerCase().startsWith("up");
              const icon = isUp ? chalk.green("●") : chalk.red("●");
              const shortStatus = r.status.replace(/ \(.*\)/, "");
              console.log(`  ${icon} ${chalk.bold(agentName.padEnd(24))} ${shortStatus.padEnd(20)} ${chalk.dim(r.image || "")}`);
              found = true;
            }
            console.log();
          }
        } catch { /* docker not available */ }
      }

      // ── Kubernetes sandboxes (all reachable contexts) ──
      // Discover every kubectl context once and query each separately
      // so users see both their local-k8s kind cluster AND their AKS
      // cluster in a single `kars list`. Previously this section
      // only queried the *current* context, which silently hid one or
      // the other depending on which `kubectl config use-context` was
      // last run.
      if (!options.dockerOnly) {
        const targets = await discoverKubeContexts(execa, options.context);
        for (const tgt of targets) {
          const rows = await fetchClusterSandboxes(execa, tgt.context);
          if (rows.length === 0) continue;

          console.log(chalk.bold(`  ${tgt.label}`) + chalk.dim(`  (context: ${tgt.context})`));
          console.log(chalk.dim("  ─────────────────────────────────────────────────────────────────"));
          console.log(chalk.dim(`  ${"NAME".padEnd(22)} ${"STATUS".padEnd(12)} ${"RUNTIME".padEnd(22)} ${"MODEL".padEnd(14)} ${"ISO".padEnd(10)} NS`));

          for (const r of rows) {
            let icon: string;
            if (r.phase === "Running") icon = chalk.green("●");
            else if (r.phase === "Pending" || r.phase === "Creating") icon = chalk.yellow("●");
            else icon = chalk.red("●");

            console.log(`  ${icon} ${chalk.bold(r.name.padEnd(22))} ${r.phase.padEnd(12)} ${r.runtime.padEnd(22)} ${r.model.padEnd(14)} ${r.isolation.padEnd(10)} ${chalk.dim(r.ns)}`);
            found = true;
          }
          console.log();
        }

        if (!found && options.aksOnly) {
          console.log(chalk.dim("  No AKS / kube sandboxes found (kubectl unreachable or no KarsSandbox CRs)\n"));
        }
      }

      if (!found) {
        console.log(chalk.dim("  No sandboxes found.\n"));
        console.log(chalk.dim("  Create one with: kars dev (local) or kars add <name> (AKS)\n"));
      }
    });

  return cmd;
}

/**
 * Resolve the set of kube contexts to query.
 *
 * - `--context <name>` → just that one
 * - Otherwise: every context in kubeconfig that we can reach. A
 *   context is "reachable" if `kubectl --context <name> get ns` returns
 *   within a short timeout. Unreachable contexts are silently skipped
 *   so a stale kubeconfig entry doesn't make `kars list` hang.
 *
 * Returns a friendly label per context: `Local (kind)` for any
 * `kind-*` context, `AKS Cluster` for everything else. This is a
 * heuristic — it covers the documented `kars dev --target
 * local-k8s` (creates `kind-kars-dev`) and `kars up`
 * (configures `kars-aks`).
 */
async function discoverKubeContexts(
  execa: typeof import("execa").execa,
  pinned?: string,
): Promise<ClusterTarget[]> {
  if (pinned) {
    return [{ context: pinned, label: pinned.startsWith("kind-") ? "Local (kind)" : "AKS Cluster" }];
  }

  let allContexts: string[] = [];
  try {
    const { stdout } = await execa("kubectl", ["config", "get-contexts", "-o", "name"], { stdio: "pipe" });
    allContexts = stdout.trim().split("\n").filter(Boolean);
  } catch {
    return [];
  }

  // Probe each context in parallel with a short timeout so a single
  // unreachable AKS cluster doesn't block listing the local one.
  const probed = await Promise.all(
    allContexts.map(async (ctx): Promise<ClusterTarget | null> => {
      try {
        await execa(
          "kubectl",
          ["--context", ctx, "get", "ns", "--request-timeout=3s", "--no-headers"],
          { stdio: "pipe", timeout: 5000 },
        );
        return { context: ctx, label: ctx.startsWith("kind-") ? "Local (kind)" : "AKS Cluster" };
      } catch {
        return null;
      }
    }),
  );
  return probed.filter((t): t is ClusterTarget => t !== null);
}

/**
 * Query KarsSandbox CRs from one kube context, resolving the model
 * via the modern `spec.inferenceRef → InferencePolicy.modelPreference`
 * path (with legacy `spec.inference.model` fallback for pre-S10 CRs).
 *
 * This mirrors the same resolution order used by
 * `cli/src/commands/operator/fetchers/sandboxes.ts:75-82`. Keeping the
 * two in sync means `kars list` and `kars operator` agree
 * on which model each sandbox is using.
 */
async function fetchClusterSandboxes(
  execa: typeof import("execa").execa,
  context: string,
): Promise<SandboxRow[]> {
  try {
    const [sbRes, ipRes] = await Promise.allSettled([
      execa(
        "kubectl",
        ["--context", context, "get", "karssandbox", "-A", "-o", "json", "--request-timeout=5s"],
        { stdio: "pipe", timeout: 8000 },
      ),
      execa(
        "kubectl",
        ["--context", context, "get", "inferencepolicy", "-A", "-o", "json", "--request-timeout=5s"],
        { stdio: "pipe", timeout: 8000 },
      ),
    ]);
    if (sbRes.status !== "fulfilled") return [];

    const ipModel = new Map<string, string>();
    if (ipRes.status === "fulfilled") {
      try {
        const ipData = JSON.parse((ipRes.value as any).stdout);
        for (const it of (ipData.items || []) as any[]) {
          const ns = it?.metadata?.namespace;
          const nm = it?.metadata?.name;
          const dep = it?.spec?.modelPreference?.primary?.deployment;
          if (ns && nm && typeof dep === "string" && dep) {
            ipModel.set(`${ns}/${nm}`, dep);
          }
        }
      } catch { /* non-fatal */ }
    }

    const data = JSON.parse((sbRes.value as any).stdout);
    const out: SandboxRow[] = [];
    for (const sb of (data.items || []) as any[]) {
      const name = sb.metadata?.name || "unknown";
      const ns = sb.metadata?.namespace || "kars-system";
      const phase = sb.status?.phase || "Unknown";
      const runtime = sb.spec?.runtime?.kind || "OpenClaw";
      const isolation = sb.spec?.sandbox?.isolation || "enhanced";
      const inferenceRefName = sb.spec?.inferenceRef?.name as string | undefined;
      const ipKey = inferenceRefName ? `${ns}/${inferenceRefName}` : "";
      const model = (ipKey && ipModel.get(ipKey)) || sb.spec?.inference?.model || "gpt-4.1";
      out.push({ name, phase, runtime, model, isolation, ns });
    }
    return out;
  } catch {
    return [];
  }
}
