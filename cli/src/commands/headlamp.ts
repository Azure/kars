// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";

/**
 * `kars headlamp` — open the Headlamp Kubernetes dashboard for the
 * current kubectl context (AKS, kind, anything kubectl can reach).
 *
 * Mirrors the UX of `kars connect <name>`: best-effort port-forward
 * + browser open + token print, with a single `--install` escape hatch
 * for clusters that don't have Headlamp installed yet.
 *
 * Headlamp is installed by `kars dev --target local-k8s` out of
 * the box. For AKS / shared clusters, run `kars headlamp --install`
 * once per cluster.
 */
export function headlampCommand(): Command {
  const cmd = new Command("headlamp");

  cmd
    .description("Open the Headlamp K8s dashboard for the current cluster")
    .option("--context <name>", "Kubernetes context to use (defaults to current-context)")
    .option("--port <port>", "Local port to bind", "4466")
    .option("--namespace <ns>", "Namespace where Headlamp is deployed", "headlamp")
    .option("--install", "Install Headlamp + kars plugin into the cluster first", false)
    .option("--no-browser", "Skip opening the browser; just port-forward + print URL")
    .option("--token-duration <dur>", "Token TTL passed to `kubectl create token`", "24h")
    .action(
      async (options: {
        context?: string;
        port: string;
        namespace: string;
        install: boolean;
        browser: boolean;
        tokenDuration: string;
      }) => {
        const { execa } = await import("execa");

        // Resolve the effective context (so error messages name it).
        let ctx = options.context;
        if (!ctx) {
          try {
            const { stdout } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
            ctx = stdout.trim();
          } catch {
            console.error(chalk.red("✖ kubectl has no current-context set."));
            console.error(chalk.dim("  Try: kubectl config use-context <name>"));
            process.exitCode = 1;
            return;
          }
        }

        const kctl = (args: string[]) => ["--context", ctx!, ...args];

        // Optional install — works on any cluster kubectl can reach.
        // Installs the full stack: Headlamp + kars plugin + Prometheus
        // + Grafana + monitoring manifests. Mirrors what
        // `kars dev --target local-k8s` installs for kind, so the
        // same dashboard experience lands on AKS.
        if (options.install) {
          const { installHeadlamp: stackInstallHeadlamp, installKarsPlugin, installPrometheus } =
            await import("./up/headlamp_stack.js");
          const { findRepoRoot } = await import("./up/helpers.js");
          const repoRoot = findRepoRoot(process.cwd());
          console.log(chalk.bold(`  Installing Headlamp + kars plugin + Prometheus into ${chalk.cyan(ctx!)}…\n`));
          await stackInstallHeadlamp({ context: ctx!, repoRoot });
          console.log();
          await installKarsPlugin({ context: ctx!, repoRoot });
          console.log();
          await installPrometheus({ context: ctx!, repoRoot });
          console.log(chalk.green("\n  ✓ Headlamp + kars plugin + Prometheus installed.\n"));
        }

        // Verify Headlamp is present.
        let svcExists = false;
        try {
          await execa(
            "kubectl",
            kctl(["get", "svc", "headlamp", "-n", options.namespace, "--no-headers"]),
            { stdio: "pipe" },
          );
          svcExists = true;
        } catch {
          // fall through
        }
        if (!svcExists) {
          console.error(
            chalk.red(`✖ Headlamp service '${options.namespace}/headlamp' not found in context '${ctx}'.`),
          );
          console.error("");
          console.error(chalk.bold("  Install it first:"));
          console.error(`    ${chalk.cyan(`kars headlamp --install --context ${ctx}`)}`);
          process.exitCode = 1;
          return;
        }

        // Kill any stale port-forward bound to the local port (best-effort).
        await freePort(execa, parseInt(options.port, 10));

        // Mint a fresh service-account token so the user can log in.
        // Headlamp uses bearer-token auth out of the box; the SA is
        // created by the chart in the same namespace as the deployment.
        let token = "";
        try {
          const { stdout } = await execa(
            "kubectl",
            kctl([
              "create",
              "token",
              "headlamp",
              "-n",
              options.namespace,
              `--duration=${options.tokenDuration}`,
            ]),
            { stdio: "pipe" },
          );
          token = stdout.trim();
        } catch (err) {
          console.warn(
            chalk.yellow(
              `  ⚠ could not mint Headlamp token (${(err as Error).message}). ` +
                `You can mint one manually with:\n` +
                `    kubectl --context ${ctx} create token headlamp -n ${options.namespace} --duration=24h`,
            ),
          );
        }

        // Start a detached port-forward so the process survives this
        // command exiting. User stops it via `pkill -f 'port-forward.*headlamp'`.
        console.log(chalk.bold(`  Connecting to Headlamp on context ${chalk.cyan(ctx!)}…`));
        const { spawn } = await import("node:child_process");
        const child = spawn(
          "kubectl",
          kctl(["port-forward", "-n", options.namespace, "service/headlamp", `${options.port}:80`]),
          { detached: true, stdio: "ignore" },
        );
        child.unref();

        // Give the forward ~1.5s to bind before opening the browser.
        await new Promise((r) => setTimeout(r, 1500));

        // Best-effort: if the cluster also has kube-prometheus-stack
        // installed (which `kars dev` ships out of the box), forward
        // Prometheus on :19091 and Grafana on :3000 so the Headlamp
        // kars plugin's metric panels (Mesh Topology, Token
        // Budget, AGT decisions) light up without manual setup. The
        // plugin reads `window.KARS_PROMETHEUS_URL` which defaults
        // to http://127.0.0.1:19091.
        const promPort = 19091;
        const grafanaPort = 3000;
        let promForwarded = false;
        let grafanaForwarded = false;
        try {
          await execa(
            "kubectl",
            kctl(["get", "svc", "kps-kube-prometheus-stack-prometheus", "-n", "monitoring", "--no-headers"]),
            { stdio: "pipe" },
          );
          await freePort(execa, promPort);
          const pfProm = spawn(
            "kubectl",
            kctl([
              "port-forward",
              "-n",
              "monitoring",
              "service/kps-kube-prometheus-stack-prometheus",
              `${promPort}:9090`,
            ]),
            { detached: true, stdio: "ignore" },
          );
          pfProm.unref();
          promForwarded = true;
        } catch {
          // Prometheus not present — plugin panels will show the
          // "Prometheus unreachable" hint, which is the correct UX
          // for a cluster without monitoring.
        }
        try {
          await execa(
            "kubectl",
            kctl(["get", "svc", "kps-grafana", "-n", "monitoring", "--no-headers"]),
            { stdio: "pipe" },
          );
          await freePort(execa, grafanaPort);
          const pfGraf = spawn(
            "kubectl",
            kctl([
              "port-forward",
              "-n",
              "monitoring",
              "service/kps-grafana",
              `${grafanaPort}:80`,
            ]),
            { detached: true, stdio: "ignore" },
          );
          pfGraf.unref();
          grafanaForwarded = true;
        } catch {
          // Grafana not present — fine.
        }
        if (promForwarded || grafanaForwarded) {
          // Give the forwards a moment to bind.
          await new Promise((r) => setTimeout(r, 1500));
        }

        const url = `http://localhost:${options.port}/`;
        console.log("");
        console.log(chalk.bold("  Headlamp dashboard:"));
        console.log(`    ${chalk.cyan(url)}`);
        if (token) {
          console.log("");
          console.log(chalk.bold("  Login token (paste into Headlamp):"));
          console.log(`    ${chalk.dim(token)}`);
        }
        if (promForwarded || grafanaForwarded) {
          console.log("");
          console.log(chalk.bold("  Observability stack:"));
          if (promForwarded) console.log(`    Prometheus: ${chalk.cyan(`http://localhost:${promPort}/`)}`);
          if (grafanaForwarded) console.log(`    Grafana:    ${chalk.cyan(`http://localhost:${grafanaPort}/`)}`);
        }
        console.log("");
        console.log(
          chalk.dim(
            `  Port-forward runs in the background (PID ${child.pid}).\n` +
              `  Stop it later with: pkill -f 'port-forward.*(headlamp|prometheus|grafana)'`,
          ),
        );

        if (options.browser) {
          await openBrowser(execa, url);
        }
      },
    );

  return cmd;
}

/**
 * Best-effort: kill any process holding `port` so a fresh port-forward
 * can bind. Reuses the same lsof/kill dance used by `kars dev`.
 */
async function freePort(execa: typeof import("execa").execa, port: number): Promise<void> {
  try {
    const { stdout } = await execa("lsof", ["-ti", `:${port}`], { stdio: "pipe" });
    const pids = stdout.trim().split(/\s+/).filter(Boolean);
    for (const pid of pids) {
      try {
        await execa("kill", [pid]);
      } catch {
        // process already gone — fine
      }
    }
  } catch {
    // lsof returns non-zero when nothing matches — fine
  }
}

/**
 * Cross-platform browser-open helper. Failure is non-fatal (the URL is
 * printed for the user to click manually).
 */
async function openBrowser(execa: typeof import("execa").execa, url: string): Promise<void> {
  const cmd =
    process.platform === "darwin"
      ? "open"
      : process.platform === "win32"
        ? "start"
        : "xdg-open";
  try {
    await execa(cmd, [url], { stdio: "ignore" });
  } catch {
    // user can click the printed URL
  }
}

/**
 * Install Headlamp + the kars plugin into the active cluster.
 * Pinned to the same Headlamp chart version that `kars dev`
 * installs locally so the bundled plugin remains compatible.
 *
 * Idempotent — re-running re-applies the manifest. Works on AKS,
 * kind, and any other cluster kubectl can reach.
 */
async function installHeadlamp(execa: typeof import("execa").execa, ctx: string): Promise<void> {
  const HEADLAMP_CHART_VERSION = "0.41.0"; // keep in lock-step with cli/src/commands/dev/local-k8s.ts
  const kctl = (args: string[]) => ["--context", ctx, ...args];

  console.log(chalk.bold(`  Installing Headlamp into context ${chalk.cyan(ctx)}…`));

  // Ensure the namespace exists.
  try {
    await execa("kubectl", kctl(["create", "namespace", "headlamp"]), { stdio: "pipe" });
  } catch {
    // already exists — fine
  }

  // Add and update the chart repo.
  try {
    await execa("helm", ["repo", "add", "headlamp", "https://kubernetes-sigs.github.io/headlamp/"], {
      stdio: "pipe",
    });
  } catch {
    // already added — fine
  }
  await execa("helm", ["repo", "update", "headlamp"], { stdio: "pipe" });

  // Render the chart and apply through kubectl --context so the install
  // honours the user's --context flag (helm doesn't always thread it).
  const { stdout: manifest } = await execa(
    "helm",
    [
      "template",
      "headlamp",
      "headlamp/headlamp",
      "--version",
      HEADLAMP_CHART_VERSION,
      "--namespace",
      "headlamp",
      "--set",
      "config.useNodeInternalDNS=false",
    ],
    { stdio: "pipe" },
  );
  await execa(
    "kubectl",
    kctl(["apply", "-f", "-", "--server-side", "--force-conflicts"]),
    { input: manifest, stdio: ["pipe", "inherit", "inherit"] },
  );

  // Wait for the deployment to come up (best-effort 90s).
  try {
    await execa(
      "kubectl",
      kctl(["rollout", "status", "deployment/headlamp", "-n", "headlamp", "--timeout=90s"]),
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow("  ⚠ Headlamp deployment didn't become ready within 90s; proceeding anyway."),
    );
  }

  console.log(chalk.green("  ✓ Headlamp installed."));
}
