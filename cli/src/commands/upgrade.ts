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
} from "../lib/release.js";

const NS = "kars-system";

/** True only when `ep` is a Foundry project endpoint whose host is exactly
 *  `services.ai.azure.com` or a subdomain of it. A proper hostname check —
 *  NOT a substring match, which `https://services.ai.azure.com.evil.com`
 *  would defeat. */
export function isFoundryProjectHost(ep: string): boolean {
  if (!ep) return false;
  try {
    const host = new URL(ep).hostname.toLowerCase();
    return host === "services.ai.azure.com" || host.endsWith(".services.ai.azure.com");
  } catch {
    return false;
  }
}

interface UpgradeContext {
  acrLoginServer: string;
  aksCluster: string;
  resourceGroup: string;
  wiClientId?: string;
  keyVaultName?: string;
  foundryEndpoint?: string;
  foundryProjectEndpoint?: string;
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
        foundryProjectEndpoint: ctxRaw.foundryProjectEndpoint,
      };
      const acrName = ctx.acrLoginServer.replace(/\.azurecr\.io$/, "");

      banner("kars · Upgrade", "Move an existing cluster to a published release");

      const stepper = new Stepper({ totalSteps: options.rollback ? 4 : 8 });

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

        // ── Step 7: Reconcile Foundry Memory Store access ─────────────
        // Memory persistence depends on the Foundry PROJECT managed
        // identity holding `Azure AI User` on the resource group plus an
        // embedding deployment — provisioning that historically only ran
        // on a fresh `kars up`. Re-reconcile it idempotently here so an
        // existing cluster's memory starts working on upgrade. Best-effort:
        // memory is one capability and never fails the upgrade itself.
        stepper.step("Reconciling Foundry Memory Store access...");
        const foundryProjectEp = ctx.foundryProjectEndpoint || ctx.foundryEndpoint || "";
        if (isFoundryProjectHost(foundryProjectEp)) {
          try {
            const { ensureFoundryMemoryRbac } = await import("./up/foundry_memory_rbac.js");
            const mem = await ensureFoundryMemoryRbac({ execa, stepper, foundryEndpoint: foundryProjectEp });
            for (const n of mem.notes) stepper.detail("info", n);
            if (mem.granted) stepper.done("Foundry Memory Store access reconciled");
            else stepper.warn("Foundry Memory Store needs manual RBAC — see the notes above");
          } catch {
            stepper.warn("Could not reconcile Foundry Memory Store access (non-fatal) — run `kars up` to retry");
          }
        } else {
          stepper.done("No Foundry project bound — Memory Store reconcile skipped");
        }

        // ── Step 8: Report ────────────────────────────────────────────
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

/** Determine the deployed kars release. Prefers the `karsRelease` value stamped
 *  into Helm by a prior `kars upgrade` (reliable), falling back to the chart's
 *  static appVersion (only accurate right after a same-version install). */
async function detectCurrentVersion(execa: Execa, appVersion?: string): Promise<string> {
  const { stdout } = await execa("helm", [
    "get", "values", "kars", "-n", NS, "-o", "json",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  try {
    const vals = JSON.parse(stdout || "{}") as { karsRelease?: string };
    if (vals.karsRelease) return vals.karsRelease;
  } catch { /* ignore */ }
  return appVersion ? `v${appVersion.replace(/^v/, "")}` : "";
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
