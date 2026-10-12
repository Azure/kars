// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Shared installer for the full Headlamp + kars plugin + Prometheus
 * observability stack.
 *
 * Previously lived inside cli/src/commands/dev/local-k8s.ts and was
 * coupled to `kind-<cluster>` context names. Extracted here so the
 * same stack lands on AKS via `kars headlamp --install` with identical
 * UX to `kars dev --target local-k8s`.
 *
 * Each step accepts the active kubectl context explicitly. Both
 * helpers from local-k8s.ts (which prepend `kind-`) and headlamp.ts
 * (which uses whatever the user passes / current-context) can call
 * these with no string-rewriting.
 *
 * Network plumbing on AKS: AKS pods can be reached via
 * `kubectl port-forward` exactly like kind. Port collisions are
 * resolved by killing any prior `kubectl port-forward` bound to the
 * same local port before respawning (same pattern as the local-k8s
 * variant).
 */

import { execa } from "execa";
import chalk from "chalk";
import * as path from "node:path";
import { existsSync, readFileSync, statSync, readdirSync } from "node:fs";
import { resolveBundledAsset, findRepoRootOrNull } from "../../lib/repo-assets.js";
import { hasCoreHeadlamp, HEADLAMP_OPERATOR_LOGIN } from "../../lib/headlamp-ownership.js";
import { installMonitoring } from "./monitoring.js";
import { monitoringServices } from "../../lib/monitoring-services.js";

/** Pin the same chart version as local-k8s so the kars plugin's
 * pluginLib API stays compatible. Bumping requires re-testing the
 * plugin against the new Headlamp image. */
export const HEADLAMP_CHART_VERSION = "0.41.0";

export interface HeadlampStackOptions {
  /** kubectl context name. e.g. `kars-aks` or `kind-kars`. */
  context: string;
  /**
   * Optional repo root, used only to locate the Headlamp plugin SOURCE for
   * an on-demand rebuild in a dev checkout. When omitted (npm-installed CLI),
   * the prebuilt plugin + monitoring manifests are resolved from the bundled
   * package instead.
   */
  repoRoot?: string;
}

/** Build a kubectl arg list scoped to the target context. */
function kctl(ctx: string, args: string[]): string[] {
  return ["--context", ctx, ...args];
}

/**
 * Install Headlamp from the upstream Helm chart into the `headlamp`
 * namespace. Idempotent — re-running re-applies via server-side
 * apply, no Helm release state is written.
 */
export async function installHeadlamp(opts: HeadlampStackOptions): Promise<void> {
  const { context } = opts;
  if (await hasCoreHeadlamp(context)) {
    console.log("  Using Core-managed Headlamp; its image and plugin are updated through Core configuration.");
    return;
  }
  try {
    await execa("kubectl", kctl(context, ["create", "namespace", "headlamp"]), { stdio: "pipe" });
  } catch {
    /* already exists — fine */
  }

  try {
    await execa("helm", ["repo", "add", "headlamp", "https://kubernetes-sigs.github.io/headlamp/"], { stdio: "pipe" });
  } catch {
    /* already added */
  }
  await execa("helm", ["repo", "update", "headlamp"], { stdio: "pipe" });

  const { stdout: manifest } = await execa("helm", [
    "template",
    "headlamp",
    "headlamp/headlamp",
    "--version",
    HEADLAMP_CHART_VERSION,
    "--namespace",
    "headlamp",
    "--set",
    "config.useNodeInternalDNS=false",
  ], { stdio: "pipe" });

  await execa(
    "kubectl",
    kctl(context, ["apply", "-f", "-", "--server-side", "--force-conflicts"]),
    { input: manifest, stdio: ["pipe", "inherit", "inherit"] },
  );

  try {
    await execa(
      "kubectl",
      kctl(context, ["rollout", "status", "deployment/headlamp", "-n", "headlamp", "--timeout=90s"]),
      { stdio: "inherit" },
    );
  } catch {
    console.warn(chalk.yellow("  ⚠ Headlamp deployment did not become Ready within 90s — check 'kubectl get pods -n headlamp'."));
  }
}

/**
 * Side-load the kars Headlamp plugin via ConfigMap + volume mount.
 *
 * On-demand builds the plugin if `tools/headlamp-plugin/dist/main.js`
 * is missing. Falls back to a warning + skip when the build fails so
 * the dashboard still works for built-in resources.
 */
export async function installKarsPlugin(opts: HeadlampStackOptions): Promise<void> {
  const { context } = opts;
  if (await hasCoreHeadlamp(context)) return;

  // Prefer the prebuilt plugin bundled with the CLI (so this works with no
  // repo checkout — npm-installed `kars`). Fall back to a repo checkout,
  // rebuilding on demand if the source is newer than the built dist.
  let mainJs = resolveBundledAsset("tools/headlamp-plugin/dist/main.js");
  let pkgJson = resolveBundledAsset("tools/headlamp-plugin/package.json");

  if (!mainJs || !pkgJson) {
    const repoRoot = opts.repoRoot ?? findRepoRootOrNull(process.cwd());
    if (!repoRoot) {
      console.warn(chalk.yellow(
        "    ⚠ Headlamp plugin not bundled and no repo checkout found — skipping " +
          "(dashboard still works for built-in resources; kars CRD views won't load).",
      ));
      return;
    }
    const pluginDir = path.join(repoRoot, "tools", "headlamp-plugin");
    const distDir = path.join(pluginDir, "dist");
    const distMain = path.join(distDir, "main.js");
    pkgJson = path.join(pluginDir, "package.json");

    // Rebuild when ANY source file is newer than dist/main.js (stale dist was
    // the source of "no agents visible / wrong CRD group" bugs).
    let needsBuild = !existsSync(distMain);
    if (!needsBuild) {
      try {
        const distMtime = statSync(distMain).mtimeMs;
        const srcDir = path.join(pluginDir, "src");
        const candidates = [pkgJson];
        if (existsSync(srcDir)) {
          for (const f of readdirSync(srcDir)) candidates.push(path.join(srcDir, f));
        }
        for (const f of candidates) {
          if (existsSync(f) && statSync(f).mtimeMs > distMtime) {
            needsBuild = true;
            break;
          }
        }
      } catch {
        needsBuild = true;
      }
    }

    if (needsBuild) {
      const reason = !existsSync(distMain) ? "no dist yet" : "src newer than dist";
      console.log(chalk.dim(`    rebuilding plugin (${reason}) — npm run build in tools/headlamp-plugin…`));
      if (!existsSync(path.join(pluginDir, "node_modules"))) {
        try {
          await execa("npm", ["install", "--no-audit", "--no-fund"], { cwd: pluginDir, stdio: "inherit" });
        } catch (err) {
          console.warn(chalk.yellow(
            `    ⚠ npm install failed (${(err as Error).message}); skipping plugin install. ` +
              "Run 'cd tools/headlamp-plugin && npm install && npm run build' manually then re-run.",
          ));
          return;
        }
      }
      try {
        await execa("npm", ["run", "build"], { cwd: pluginDir, stdio: "inherit" });
      } catch (err) {
        console.warn(chalk.yellow(`    ⚠ plugin build failed (${(err as Error).message}); skipping plugin install.`));
        return;
      }
    }
    mainJs = distMain;
  }

  const mainContent = readFileSync(mainJs, "utf8");
  const pkgContent = readFileSync(pkgJson, "utf8");
  const indent = (s: string, n: number): string =>
    s.split("\n").map((l) => " ".repeat(n) + l).join("\n");

  const cmYaml = `apiVersion: v1
kind: ConfigMap
metadata:
  name: kars-headlamp-plugin
  namespace: headlamp
data:
  main.js: |
${indent(mainContent, 4)}
  package.json: |
${indent(pkgContent, 4)}
`;

  await execa(
    "kubectl",
    kctl(context, [
      "apply", "--server-side", "--force-conflicts",
      "--field-manager=kars-cli", "-f", "-",
    ]),
    { input: cmYaml, stdio: ["pipe", "inherit", "inherit"] },
  );

  // Patch Headlamp's container args + add volume + mount.
  // /headlamp-plugins is a separate dir from the chart's in-image
  // /build/plugins, so we don't clobber shipped plugins.
  const patch = JSON.stringify({
    spec: {
      template: {
        spec: {
          volumes: [{ name: "kars-plugin", configMap: { name: "kars-headlamp-plugin" } }],
          containers: [{
            name: "headlamp",
            args: [
              "-in-cluster",
              "-in-cluster-context-name=main",
              "-plugins-dir=/headlamp-plugins",
              "-session-ttl=86400",
            ],
            volumeMounts: [{ name: "kars-plugin", mountPath: "/headlamp-plugins/kars" }],
          }],
        },
      },
    },
  });

  await execa("kubectl", kctl(context, [
    "patch", "deployment", "headlamp", "-n", "headlamp",
    "--type=strategic", "-p", patch,
  ]));

  try {
    await execa(
      "kubectl",
      kctl(context, ["rollout", "status", "deployment/headlamp", "-n", "headlamp", "--timeout=90s"]),
      { stdio: "inherit" },
    );
  } catch {
    console.warn(chalk.yellow("    ⚠ Headlamp rollout did not complete in 90s after plugin patch — check 'kubectl get pods -n headlamp'."));
  }
}

export async function installPrometheus(opts: HeadlampStackOptions, tools = { kubectl: "kubectl", helm: "helm" }): Promise<void> {
  await installMonitoring(opts.context, tools);
}


/**
 * Kill any prior port-forward on `localPort` then spawn a fresh
 * detached one. Same pattern as the local-k8s impl so re-runs
 * don't hit EADDRINUSE.
 */
async function killAndForward(context: string, localPort: number, ns: string, target: string, targetPort: number): Promise<void> {
  const { spawn } = await import("node:child_process");
  try {
    const { stdout } = await execa("lsof", ["-ti", `:${localPort}`]);
    for (const pid of stdout.trim().split(/\s+/).filter(Boolean)) {
      try { await execa("kill", [pid]); } catch { /* already gone */ }
    }
  } catch {
    /* nothing listening — fine */
  }
  const child = spawn("kubectl", [
    "--context", context, "port-forward", "-n", ns, target, `${localPort}:${targetPort}`,
  ], { detached: true, stdio: "ignore" });
  child.unref();
}

/** Start Headlamp and installed monitoring service port-forwards detached. */
export async function startPortForwards(opts: {
  context: string;
  headlampPort: number;
  grafanaPort: number;
  prometheusPort: number;
}): Promise<{ grafana: boolean; prometheus: boolean }> {
  const services = await monitoringServices(opts.context);
  await killAndForward(opts.context, opts.headlampPort, "headlamp", "service/headlamp", 80);
  if (services.grafana) await killAndForward(opts.context, opts.grafanaPort, "monitoring", "service/kps-grafana", 80);
  if (services.prometheus) await killAndForward(opts.context, opts.prometheusPort, "monitoring", "service/kps-kube-prometheus-stack-prometheus", 9090);
  await new Promise((r) => setTimeout(r, 1500));
  return services;
}

/** Cross-platform `open <url>`. Best-effort. */
export async function openBrowser(url: string): Promise<void> {
  const cmd =
    process.platform === "darwin" ? "open"
    : process.platform === "win32" ? "start"
    : "xdg-open";
  try {
    await execa(cmd, [url], { stdio: "ignore" });
  } catch {
    /* user can click the printed URL */
  }
}

/** Mint a 24-hour ServiceAccount token for the headlamp SA so the
 * dashboard can log in. */
export async function mintHeadlampToken(context: string): Promise<string | null> {
  if (await hasCoreHeadlamp(context)) {
    console.log(HEADLAMP_OPERATOR_LOGIN);
    return null;
  }
  try {
    const { stdout } = await execa("kubectl", kctl(context, [
      "create", "token", "headlamp",
      "-n", "headlamp",
      "--duration=24h",
    ]), { stdio: "pipe" });
    return stdout.trim();
  } catch {
    return null;
  }
}
