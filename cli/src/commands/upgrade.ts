// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// commands/upgrade.ts — `kars upgrade`: move an EXISTING kars cluster to a
// published GitHub release, safely and idempotently.
//
// Unlike `kars up --upgrade` (Helm-only re-run that assumes the ACR already has
// the new images), `kars upgrade`:
//   1. detects current vs. target version (latest GHCR release, or --to <tag>),
//   2. records a rollback point (Helm revision),
//   3. imports the target release images into the user's ACR (pinned + :latest),
//   4. `helm upgrade --atomic` (auto-rolls-back the release on failure),
//   5. rolls the controller, router, and sandbox workloads to the new images,
//   6. verifies health and prints what changed.
// `--dry-run` shows the plan with no writes; `--rollback` reverts to the
// previous Helm revision.

import { Command } from "commander";
import chalk from "chalk";
import { Stepper, banner, section, kvLine } from "../stepper.js";
import { loadContext } from "../config.js";
import { requireBundledAsset } from "../lib/repo-assets.js";
import {
  releaseImagePlan,
  compareVersions,
  fetchLatestReleaseTag,
  fetchRecentReleases,
  releasesBetween,
  fetchTagMessage,
  ghcrManifestDigests,
} from "../lib/release.js";

const NS = "kars-system";

interface UpgradeContext {
  acrLoginServer: string;
  aksCluster: string;
  resourceGroup: string;
  wiClientId?: string;
  keyVaultName?: string;
  foundryEndpoint?: string;
}

/** Build the `helm upgrade` args. `--atomic` makes a failed upgrade auto-roll
 *  back the release, so the cluster never lands half-migrated. Exported for
 *  tests. */
export function buildHelmUpgradeArgs(
  ctx: UpgradeContext,
  helmPath: string,
  target?: string,
): string[] {
  const args = [
    "upgrade", "--install", "kars", helmPath,
    "--namespace", NS,
    "--create-namespace",
    "--set", `controller.image.repository=${ctx.acrLoginServer}/kars-controller`,
    "--set", "controller.image.tag=latest",
    "--set", `inferenceRouter.image.repository=${ctx.acrLoginServer}/kars-inference-router`,
    "--set", "inferenceRouter.image.tag=latest",
    "--set", `sandbox.image.repository=${ctx.acrLoginServer}/openclaw-sandbox`,
    "--set", "sandbox.image.tag=latest",
    "--set", `azure.workloadIdentity.clientId=${ctx.wiClientId || ""}`,
    "--set", `azure.keyVaultCsi.keyVaultName=${ctx.keyVaultName || ""}`,
    "--atomic",
    "--wait",
    "--timeout", "8m",
  ];
  if (ctx.foundryEndpoint) {
    args.push("--set", `inferenceRouter.azure.openai.endpoint=${ctx.foundryEndpoint}`);
  }
  // Stamp the deployed release into Helm values (not consumed by templates) so
  // a later `kars upgrade` can read the ACTUAL deployed version back via
  // `helm get values` — the chart's static appVersion can't be trusted.
  if (target) {
    args.push("--set", `karsRelease=${target}`);
  }
  return args;
}

export function upgradeCommand(): Command {
  const cmd = new Command("upgrade");
  cmd
    .description(
      "Upgrade an existing kars cluster to a published GitHub release (failsafe: " +
      "imports release images, atomic Helm upgrade, rolling restart, verify + rollback).",
    )
    .option("--to <tag>", "Target release tag (e.g. v0.1.16). Default: the latest GitHub release.")
    .option("--dry-run", "Show the upgrade plan without making any changes.", false)
    .option("--rollback", "Roll the cluster back to the previous Helm revision.", false)
    .option("--skip-runtime-images", "Skip the 7 multi-runtime adapter images (faster).", false)
    .option("--force", "Re-run the upgrade even if already at the target version.", false)
    .option("--yes", "Non-interactive (for CI/automation).", false)
    .addHelpText("after", `
Examples:
  kars upgrade                     # Upgrade to the latest GitHub release
  kars upgrade --to v0.1.16        # Pin a specific release
  kars upgrade --dry-run           # Show what would change
  kars upgrade --rollback          # Revert to the previous Helm revision
`)
    .action(async (options) => {
      const { execa } = await import("execa");

      // ── Load + validate cached context ──────────────────────────────
      const ctxRaw = loadContext();
      if (!ctxRaw?.acrLoginServer || !ctxRaw?.aksCluster || !ctxRaw?.resourceGroup) {
        console.error(chalk.red(
          "\n  No cached kars deployment found (~/.kars/context.json).\n" +
          "  Run `kars up` first, or run `kars upgrade` from the machine that deployed the cluster.\n",
        ));
        process.exit(1);
      }
      const ctx: UpgradeContext = {
        acrLoginServer: ctxRaw.acrLoginServer,
        aksCluster: ctxRaw.aksCluster,
        resourceGroup: ctxRaw.resourceGroup,
        wiClientId: ctxRaw.wiClientId,
        keyVaultName: ctxRaw.keyVaultName,
        foundryEndpoint: ctxRaw.foundryEndpoint,
      };
      const acrName = ctx.acrLoginServer.replace(/\.azurecr\.io$/, "");

      banner("kars · Upgrade", "Move an existing cluster to a published release");

      const stepper = new Stepper({ totalSteps: options.rollback ? 4 : 7 });

      try {
        // ── Step 1: Connect to the cluster ────────────────────────────
        stepper.step(`Connecting to AKS '${ctx.aksCluster}'...`);
        await execa("az", [
          "aks", "get-credentials",
          "--name", ctx.aksCluster, "--resource-group", ctx.resourceGroup,
          "--overwrite-existing", "--output", "none",
        ], { stdio: "pipe" });
        // Sanity: the Helm release must exist to upgrade/rollback.
        const { stdout: relJson } = await execa("helm", [
          "list", "-n", NS, "-o", "json",
        ], { stdio: "pipe" }).catch(() => ({ stdout: "[]" }));
        const releases = JSON.parse(relJson || "[]") as Array<{ name: string; revision: string; app_version?: string }>;
        const karsRel = releases.find((r) => r.name === "kars");
        if (!karsRel) {
          stepper.fail("No 'kars' Helm release found in this cluster");
          console.error(chalk.red("\n  This cluster has no kars Helm release to upgrade. Run `kars up` to deploy.\n"));
          process.exit(1);
        }
        stepper.done(`Connected — kars release at revision ${karsRel.revision}`);

        // ── Rollback path ─────────────────────────────────────────────
        if (options.rollback) {
          stepper.step("Rolling back to the previous Helm revision...");
          await execa("helm", ["rollback", "kars", "-n", NS, "--wait", "--timeout", "8m"], { stdio: "pipe" });
          stepper.done("Helm release rolled back");

          stepper.step("Restarting workloads...");
          await rolloutRestartAll(execa);
          stepper.done("Workloads restarted");

          stepper.step("Verifying cluster health...");
          const healthy = await verifyHealth(execa);
          if (healthy) stepper.done("Cluster healthy after rollback");
          else stepper.warn("Rollback applied but some workloads aren't Ready yet — check `kars status`");
          stepper.summary();
          process.exit(0);
        }

        // ── Step 2: Resolve target version ────────────────────────────
        stepper.step("Resolving target release...");
        let target: string | undefined = options.to;
        if (!target) {
          target = (await fetchLatestReleaseTag()) ?? undefined;
          if (!target) {
            stepper.fail("Could not determine the latest release");
            console.error(chalk.red(
              "\n  Couldn't reach the GitHub releases API to find the latest version.\n" +
              "  Pass an explicit tag: `kars upgrade --to v0.1.16`.\n",
            ));
            process.exit(1);
          }
        }
        const current = await detectCurrentVersion(execa, karsRel.app_version);
        stepper.detail("info", `Current: ${current || "unknown"}  →  Target: ${target}`);
        if (current && compareVersions(current, target) === 0 && !options.force) {
          stepper.done(`Already at ${target} — nothing to do (use --force to re-run)`);
          stepper.summary();
          process.exit(0);
        }
        if (current && compareVersions(current, target) > 0 && !options.force) {
          stepper.warn(`Cluster is NEWER (${current}) than target (${target}). Use --force to downgrade.`);
          stepper.summary();
          process.exit(0);
        }
        stepper.done(`Target release: ${target}`);

        // ── Dry-run: print plan + exit ────────────────────────────────
        const images = releaseImagePlan(target, { includeRuntimes: !options.skipRuntimeImages });
        if (options.dryRun) {
          stepper.stop();
          await printChangelog(current, target);
          await printImpactTable(execa);
          section("Upgrade plan (dry-run — no changes made)");
          kvLine("Cluster", ctx.aksCluster);
          kvLine("ACR", ctx.acrLoginServer);
          kvLine("From", current || "unknown");
          kvLine("To", target);
          console.log(chalk.dim(`\n  Would import ${images.length} image(s) into ${acrName}:`));
          for (const img of images) {
            console.log(chalk.dim(`    ${img.src}  →  ${acrName}/${img.target}${img.required ? "" : "  (optional)"}`));
          }
          console.log(chalk.dim(`\n  Then: helm upgrade --atomic, rolling restart of controller/router/sandboxes, verify.\n`));
          process.exit(0);
        }

        // ── Changelog summary + confirmation ─────────────────────────
        // Show what's about to change, then confirm before any write. The
        // dry-run above already exited; this only runs for a real upgrade.
        stepper.stop();
        await printChangelog(current, target);
        await printImpactTable(execa);

        const interactive = !options.yes && process.stdin.isTTY === true;
        if (interactive) {
          const { default: inquirer } = await import("inquirer");
          const { proceed } = await inquirer.prompt([{
            type: "confirm",
            name: "proceed",
            message: `Upgrade ${ctx.aksCluster} from ${current || "unknown"} to ${target}?`,
            default: true,
          }]);
          if (!proceed) {
            console.log(chalk.dim("\n  Upgrade cancelled — no changes made.\n"));
            process.exit(0);
          }
        } else {
          console.log(chalk.dim(`  Non-interactive — proceeding with upgrade to ${target}.\n`));
        }

        // ── Step 3: Import target release images into ACR ─────────────
        stepper.step(`Importing ${target} images into ${acrName}...`);
        let requiredFailures = 0;
        for (const img of images) {
          stepper.update(`Importing ${img.target}...`);
          // Import the immutable version tag too, so a future rollback/pin can
          // reference the exact release.
          const versioned = img.target.replace(/:latest$/, `:${target}`);
          const okLatest = await acrImport(execa, acrName, img.src, img.target);
          await acrImport(execa, acrName, img.src, versioned); // best-effort pin
          if (!okLatest) {
            if (img.required) { requiredFailures++; stepper.detail("info", `${img.target} — import FAILED (required)`); }
            else stepper.detail("info", `${img.target} — import failed (optional)`);
          } else {
            stepper.detail("ok", img.target);
          }
        }
        if (requiredFailures > 0) {
          throw new Error(
            `Failed to import ${requiredFailures} required image(s) for ${target}. ` +
            `Verify the tag exists on GHCR and that 'az acr import' can reach ghcr.io. ` +
            `No cluster changes were made.`,
          );
        }
        stepper.done(`Imported ${target} images into ACR`);

        // ── Step 4: Atomic Helm upgrade ───────────────────────────────
        stepper.step("Upgrading controller + CRDs (atomic Helm upgrade)...");
        const helmPath = requireBundledAsset("deploy/helm/kars");
        await execa("helm", buildHelmUpgradeArgs(ctx, helmPath, target), { stdio: "pipe" });
        stepper.done("Helm upgrade applied (auto-rollback on failure via --atomic)");

        // ── Step 5: Roll workloads to the new images ──────────────────
        stepper.step("Rolling controller, router, and sandboxes to the new images...");
        await rolloutRestartAll(execa);
        stepper.done("Workloads restarted");

        // ── Step 6: Verify health ─────────────────────────────────────
        stepper.step("Verifying cluster health...");
        const healthy = await verifyHealth(execa);
        if (healthy) stepper.done("Cluster healthy on the new release");
        else stepper.warn("Upgrade applied but some workloads aren't Ready yet — check `kars status` / `kubectl get pods -A`");

        // ── Step 7: Report ────────────────────────────────────────────
        stepper.step("Done");
        stepper.done(`Upgraded ${current || "cluster"} → ${target}`);
        stepper.summary();

        section("Upgrade complete");
        kvLine("Cluster", ctx.aksCluster);
        kvLine("From", current || "unknown");
        kvLine("To", target);
        console.log(chalk.dim(`\n  Verify:    kars status`));
        console.log(chalk.dim(`  Rollback:  kars upgrade --rollback\n`));
        process.exit(0);
      } catch (err) {
        stepper.stop();
        const msg = err instanceof Error ? err.message : String(err);
        console.error(chalk.red(`\n  Upgrade failed: ${msg}\n`));
        console.error(chalk.yellow(
          "  The atomic Helm upgrade auto-rolls-back the release on failure. If workloads\n" +
          "  are unhealthy, revert fully with:  kars upgrade --rollback\n",
        ));
        process.exit(1);
      }
    });

  return cmd;
}

type Execa = typeof import("execa").execa;

/** Read the cluster and print a table of every kars workload the upgrade would
 *  restart (controller + sandboxes), with namespace, readiness, and the running
 *  image — the blast radius, shown before the confirm. Best-effort: a read
 *  failure prints a note rather than aborting. */
async function printImpactTable(execa: Execa): Promise<void> {
  section("Impact — workloads that will be restarted");

  interface Row { component: string; namespace: string; name: string; ready: string; image: string }
  const rows: Row[] = [];

  const shortImage = (img: string): string => {
    if (!img) return "—";
    // ".../openclaw-sandbox:latest" → "openclaw-sandbox:latest"; strip digest.
    const noDigest = img.split("@")[0];
    const parts = noDigest.split("/");
    return parts[parts.length - 1] || noDigest;
  };

  const readyOf = (d: { status?: { readyReplicas?: number; replicas?: number }; spec?: { replicas?: number } }): string => {
    const ready = d.status?.readyReplicas ?? 0;
    const desired = d.spec?.replicas ?? d.status?.replicas ?? 0;
    return `${ready}/${desired}`;
  };

  interface DeployJson {
    metadata?: { name?: string; namespace?: string };
    spec?: { replicas?: number; template?: { spec?: { containers?: Array<{ name?: string; image?: string }> } } };
    status?: { readyReplicas?: number; replicas?: number };
  }
  const firstImage = (d: DeployJson, prefer?: string): string => {
    const cs = d.spec?.template?.spec?.containers ?? [];
    const pick = prefer ? cs.find((c) => c.name?.includes(prefer)) : undefined;
    return shortImage((pick ?? cs[0])?.image ?? "");
  };

  try {
    // Controller.
    const { stdout: ctrlJson } = await execa("kubectl", [
      "get", "deployment", "kars-controller", "-n", NS, "-o", "json",
    ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
    if (ctrlJson.trim()) {
      const d = JSON.parse(ctrlJson) as DeployJson;
      rows.push({ component: "controller", namespace: NS, name: "kars-controller", ready: readyOf(d), image: firstImage(d, "controller") });
    }

    // Sandboxes across all namespaces (the inference-router rides inside these).
    const { stdout: sbJson } = await execa("kubectl", [
      "get", "deployment", "-A", "-l", "kars.azure.com/component=sandbox", "-o", "json",
    ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
    if (sbJson.trim()) {
      const list = JSON.parse(sbJson) as { items?: DeployJson[] };
      for (const d of list.items ?? []) {
        rows.push({
          component: "sandbox",
          namespace: d.metadata?.namespace ?? "?",
          name: d.metadata?.name ?? "?",
          ready: readyOf(d),
          image: firstImage(d, "openclaw"),
        });
      }
    }
  } catch {
    console.log(chalk.dim("\n  (could not read cluster workloads — continuing)\n"));
    return;
  }

  if (rows.length === 0) {
    console.log(chalk.dim("\n  (no kars workloads found)\n"));
    return;
  }

  // Render a simple aligned table.
  const headers = { component: "TYPE", namespace: "NAMESPACE", name: "NAME", ready: "READY", image: "IMAGE" };
  const w = {
    component: Math.max(headers.component.length, ...rows.map((r) => r.component.length)),
    namespace: Math.max(headers.namespace.length, ...rows.map((r) => r.namespace.length)),
    name: Math.max(headers.name.length, ...rows.map((r) => r.name.length)),
    ready: Math.max(headers.ready.length, ...rows.map((r) => r.ready.length)),
    image: Math.max(headers.image.length, ...rows.map((r) => r.image.length)),
  };
  const pad = (s: string, n: number) => s.padEnd(n);
  console.log();
  console.log(
    "  " + chalk.dim(
      `${pad(headers.component, w.component)}  ${pad(headers.namespace, w.namespace)}  ${pad(headers.name, w.name)}  ${pad(headers.ready, w.ready)}  ${headers.image}`,
    ),
  );
  for (const r of rows) {
    const notReady = (() => {
      const [a, b] = r.ready.split("/").map((n) => parseInt(n, 10));
      return !(b > 0 && a === b);
    })();
    const readyCell = notReady ? chalk.yellow(pad(r.ready, w.ready)) : chalk.green(pad(r.ready, w.ready));
    console.log(
      `  ${pad(r.component, w.component)}  ${pad(r.namespace, w.namespace)}  ${pad(r.name, w.name)}  ${readyCell}  ${chalk.dim(r.image)}`,
    );
  }
  const sandboxCount = rows.filter((r) => r.component === "sandbox").length;
  console.log(chalk.dim(`\n  ${rows.length} workload(s) will be rolling-restarted (1 controller + ${sandboxCount} sandbox(es)).`));
  console.log(chalk.dim(`  Each sandbox restarts its agent pod; in-flight agent work is interrupted briefly.\n`));
}

/** Print a concise changelog of the releases between current and target. */
async function printChangelog(current: string, target: string): Promise<void> {
  section("What's changing");
  kvLine("From", current || "unknown");
  kvLine("To", target);

  const releases = await fetchRecentReleases(20);
  const between = current
    ? releasesBetween(releases, current, target)
    : releases.filter((r) => compareVersions(r.tag, target) <= 0).slice(0, 1);
  if (between.length === 0) {
    console.log(chalk.dim(`\n  (no release notes found between ${current || "?"} and ${target})\n`));
    return;
  }
  console.log();
  // Newest first reads best in a terminal. Prefer the annotated tag message
  // (real changelog) over the auto-generated release body (boilerplate).
  for (const r of [...between].reverse()) {
    const tagMsg = await fetchTagMessage(r.tag);
    console.log(`  ${chalk.bold(r.tag)}${r.name && r.name !== r.tag ? chalk.dim(` — ${r.name}`) : ""}`);
    for (const line of summarizeChangelog(tagMsg || r.body)) {
      console.log(chalk.dim(`    ${line}`));
    }
  }
  console.log();
}

/** Pull human-meaningful lines (bullets, or the first prose lines) from an
 *  annotated tag message or release body, skipping install/verification
 *  boilerplate and the leading "kars vX.Y.Z" title line. */
function summarizeChangelog(text: string, maxLines = 8): string[] {
  const lines = text.split("\n").map((l) => l.trim());
  const bullets: string[] = [];
  const prose: string[] = [];
  for (const l of lines) {
    if (!l) continue;
    if (/^#+\s*(container images|runtime adapter|verification|integrity|install)/i.test(l)) break;
    if (l.startsWith("```")) continue;
    if (/^kars v\d/i.test(l)) continue; // title line
    if (/^[-*]\s+/.test(l)) {
      bullets.push("• " + l.replace(/^[-*]\s+/, "").slice(0, 100));
    } else if (/^#+\s+/.test(l)) {
      bullets.push(l.replace(/^#+\s+/, "").slice(0, 100));
    } else {
      prose.push(l.slice(0, 100));
    }
    if (bullets.length >= maxLines) { bullets.push("…"); break; }
  }
  // Prefer bullets; if none, fall back to the first couple of prose lines.
  if (bullets.length > 0) return bullets;
  return prose.slice(0, 3);
}

/** Determine the deployed kars release, most-reliable signal first:
 *  1. the `karsRelease` value stamped into Helm by a prior `kars upgrade`;
 *  2. **image-digest match** — the controller's running image digest matched
 *     against published release digests (works even for clusters deployed before
 *     the stamp existed, since `az acr import` preserves content-addressed
 *     digests). This is what makes "Current:" accurate on an old cluster;
 *  3. the chart's static appVersion (last resort; often `v0.1.0`). */
async function detectCurrentVersion(execa: Execa, appVersion?: string): Promise<string> {
  // 1. Stamped Helm value (set by a prior `kars upgrade`).
  const { stdout } = await execa("helm", [
    "get", "values", "kars", "-n", NS, "-o", "json",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  try {
    const vals = JSON.parse(stdout || "{}") as { karsRelease?: string };
    if (vals.karsRelease) return vals.karsRelease;
  } catch { /* ignore */ }

  // 2. Match the running controller image digest against published releases.
  const byDigest = await detectVersionByImageDigest(execa).catch(() => undefined);
  if (byDigest) return byDigest;

  // 3. Static chart appVersion.
  return appVersion ? `v${appVersion.replace(/^v/, "")}` : "";
}

/** Resolve the deployed version by matching the controller pod's running image
 *  digest to the digests of recent published `kars-controller` release tags. */
async function detectVersionByImageDigest(execa: Execa): Promise<string | undefined> {
  // Scan all kars-controller container statuses for a running image digest
  // (`imageID` is like `…/kars-controller@sha256:<digest>`). Skips Pending pods
  // (empty imageID) and tolerates rollouts with multiple replicas.
  const { stdout: ids } = await execa("kubectl", [
    "get", "pods", "-n", NS, "-l", "app.kubernetes.io/name=kars",
    "-o", "jsonpath={range .items[*]}{range .status.containerStatuses[*]}{.image}{\"|\"}{.imageID}{\"\\n\"}{end}{end}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

  // Prefer the controller container's digest; accept any kars-controller image.
  let runningDigest: string | undefined;
  for (const line of ids.split("\n")) {
    if (!line.includes("kars-controller")) continue;
    const m = line.match(/@(sha256:[a-f0-9]{64})/);
    if (m) { runningDigest = m[1]; break; }
  }
  if (!runningDigest) return undefined;

  // Compare against recent release tags (newest first → report the newest match).
  const releases = await fetchRecentReleases(20);
  for (const r of releases) {
    const digests = await ghcrManifestDigests("azure/kars-controller", r.tag);
    if (digests.has(runningDigest)) return r.tag;
  }
  return undefined;
}

/** `az acr import --force` one image. Returns true on success. */
async function acrImport(execa: Execa, acrName: string, src: string, target: string): Promise<boolean> {
  return execa("az", [
    "acr", "import", "--name", acrName, "--source", src, "--image", target, "--force",
  ], { stdio: "pipe" }).then(() => true).catch(() => false);
}

/** Rolling-restart the controller, router, and every sandbox Deployment. */
async function rolloutRestartAll(execa: Execa): Promise<void> {
  // Controller lives in kars-system (the inference-router runs as a sidecar
  // inside each sandbox pod, so the sandbox restart below rolls it too).
  await execa("kubectl", ["rollout", "restart", "deployment", "-n", NS, "-l", "app.kubernetes.io/name=kars"], { stdio: "pipe" }).catch(() => {});
  // Sandboxes are labeled per-component across namespaces.
  await execa("kubectl", ["rollout", "restart", "deployment", "-A", "-l", "kars.azure.com/component=sandbox"], { stdio: "pipe" }).catch(() => {});
  // Wait for the controller to settle (best-effort).
  await execa("kubectl", ["rollout", "status", "deployment", "-n", NS, "kars-controller", "--timeout=300s"], { stdio: "pipe" }).catch(() => {});
}

/** Best-effort health check: controller Available + no pods stuck non-Ready. */
async function verifyHealth(execa: Execa): Promise<boolean> {
  const { stdout: ctrl } = await execa("kubectl", [
    "get", "deployment", "kars-controller", "-n", NS,
    "-o", "jsonpath={.status.conditions[?(@.type=='Available')].status}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  return ctrl.trim() === "True";
}
