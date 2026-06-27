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
  parseVersionTag,
} from "../lib/release.js";

const NS = "kars-system";
/** Namespace holding the standalone AgentMesh relay + registry Deployments
 *  (applied from `deploy/agentmesh-agt.yaml`, NOT part of the Helm chart). */
const MESH_NS = "agentmesh";

/**
 * Resolve the upgrade target tag. `latest`/`stable` (or no `--to`) resolves to
 * the newest published GitHub release; an explicit `--to` must be a valid
 * version tag (e.g. `v0.1.20`). Returns `{error}` instead of a target when the
 * input is unusable — callers surface it and abort. Exported for tests.
 *
 * Fixes the bug where `kars upgrade --to latest` passed the literal string
 * "latest" into `compareVersions`, which treated it as older than the current
 * version and wrongly refused with "Cluster is NEWER … use --force to downgrade".
 */
export async function resolveTargetVersion(
  to: string | undefined,
  fetchLatest: () => Promise<string | null> = fetchLatestReleaseTag,
): Promise<{ target?: string; error?: string }> {
  const raw = (to ?? "").trim();
  if (!raw || /^(latest|stable)$/i.test(raw)) {
    const t = await fetchLatest();
    return t
      ? { target: t }
      : {
          error:
            "Couldn't reach the GitHub releases API to find the latest version. " +
            "Pass an explicit tag: `kars upgrade --to v0.1.20`.",
        };
  }
  if (!parseVersionTag(raw)) {
    return {
      error: `'${to}' is not a valid release tag. Use a version like v0.1.20, or 'latest'.`,
    };
  }
  return { target: raw };
}

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

/** Chart `runtimes.*` value keys → ACR repo names. The single place the
 *  upgrade path enumerates the multi-runtime adapter images, so adding a
 *  runtime (e.g. langgraph-ts) is a one-line change that stays in sync with
 *  the import plan + the `kars up` install. */
const RUNTIME_IMAGE_VALUES: ReadonlyArray<readonly [valueKey: string, repo: string]> = [
  ["runtimes.openaiAgents.image", "kars-runtime-openai-agents"],
  ["runtimes.mafPython.image", "kars-runtime-maf-python"],
  ["runtimes.anthropic.image", "kars-runtime-anthropic"],
  ["runtimes.langgraph.image", "kars-runtime-langgraph"],
  ["runtimes.langgraphTs.image", "kars-runtime-langgraph-ts"],
  ["runtimes.pydanticAi.image", "kars-runtime-pydantic-ai"],
  ["runtimes.hermes.image", "kars-runtime-hermes"],
];

/** Build the `helm upgrade` args.
 *
 *  - `--atomic` auto-rolls-back a failed upgrade so the cluster never lands
 *    half-migrated.
 *  - Image **tags are pinned to the target version** (NOT `:latest`). This is
 *    what makes the upgrade real and reversible: a changed tag makes
 *    `helm upgrade --wait` actually recreate the pods and wait for them
 *    healthy (so `--atomic` can catch a bad image), and `helm rollback`
 *    restores the *previous* version's tag instead of a no-op `:latest`→
 *    `:latest`. The version-tagged images are imported into ACR alongside
 *    `:latest` (see the import step).
 *  - `--reuse-values` preserves operator config (Foundry, fedcred, mesh) the
 *    flags below don't restate, but every image value IS restated explicitly
 *    so an older cluster missing a value (e.g. a runtime added in a later
 *    release) gets it rather than silently falling back to a chart default
 *    that points at the wrong registry.
 *
 *  Exported for tests. */
export function buildHelmUpgradeArgs(
  ctx: UpgradeContext,
  helmPath: string,
  target?: string,
  opts: { skipRuntimeImages?: boolean } = {},
): string[] {
  const tag = target && target.length > 0 ? target : "latest";
  const args = [
    "upgrade", "--install", "kars", helmPath,
    "--namespace", NS,
    "--create-namespace",
    "--reuse-values",
    "--set", `controller.image.repository=${ctx.acrLoginServer}/kars-controller`,
    "--set", `controller.image.tag=${tag}`,
    "--set", `inferenceRouter.image.repository=${ctx.acrLoginServer}/kars-inference-router`,
    "--set", `inferenceRouter.image.tag=${tag}`,
    "--set", `sandbox.image.repository=${ctx.acrLoginServer}/openclaw-sandbox`,
    "--set", `sandbox.image.tag=${tag}`,
    "--set", `azure.workloadIdentity.clientId=${ctx.wiClientId || ""}`,
    "--set", `azure.keyVaultCsi.keyVaultName=${ctx.keyVaultName || ""}`,
  ];
  // Pin every runtime adapter image explicitly (don't rely on --reuse-values,
  // which can't materialise a value an older install never set). Skipped only
  // when the operator opted out of importing them — then we leave whatever the
  // prior install set (via --reuse-values) untouched.
  if (!opts.skipRuntimeImages) {
    for (const [valueKey, repo] of RUNTIME_IMAGE_VALUES) {
      args.push("--set", `${valueKey}=${ctx.acrLoginServer}/${repo}:${tag}`);
    }
  }
  args.push("--atomic", "--wait", "--timeout", "8m");
  if (ctx.foundryEndpoint) {
    args.push("--set", `inferenceRouter.azure.openai.endpoint=${ctx.foundryEndpoint}`);
  }
  // Stamp the deployed release as a Helm value too (belt-and-suspenders for the
  // version detection, which primarily reads the controller image tag). Carried
  // forward by --reuse-values on later upgrades that don't re-set it.
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
          const rbHealth = await verifyHealth(execa);
          if (rbHealth.healthy) stepper.done("Cluster healthy after rollback");
          else stepper.warn(`Rollback applied but the cluster isn't fully healthy yet: ${rbHealth.reason} — check \`kars status\``);
          stepper.summary();
          process.exit(0);
        }

        // ── Step 2: Resolve target version ────────────────────────────
        stepper.step("Resolving target release...");
        const resolved = await resolveTargetVersion(options.to);
        if (resolved.error || !resolved.target) {
          stepper.fail("Could not determine the target release");
          console.error(chalk.red(`\n  ${resolved.error}\n`));
          process.exit(1);
        }
        const target: string = resolved.target;
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
          // Import BOTH the `:latest` tag (chart fallbacks / other tooling) and
          // the immutable `:<version>` tag. The chart pins the version tag
          // (buildHelmUpgradeArgs), so the version import is REQUIRED for
          // required images — without it the upgrade would reference a tag that
          // isn't in ACR. Both pull from the same version-tagged GHCR source.
          const versioned = img.target.replace(/:latest$/, `:${target}`);
          const okLatest = await acrImport(execa, acrName, img.src, img.target);
          const okVersioned = await acrImport(execa, acrName, img.src, versioned);
          if ((!okLatest || !okVersioned) && img.required) {
            requiredFailures++;
            stepper.detail("info", `${img.target} — import FAILED (required)`);
          } else if (!okLatest || !okVersioned) {
            stepper.detail("info", `${img.target} — import failed (optional)`);
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
        await execa("helm", buildHelmUpgradeArgs(ctx, helmPath, target, {
          skipRuntimeImages: options.skipRuntimeImages,
        }), { stdio: "pipe" });
        stepper.done("Helm upgrade applied (auto-rollback on failure via --atomic)");

        // ── Step 5: Roll workloads to the new images ──────────────────
        // The version-pinned Helm upgrade above already recreated the
        // Helm-managed pods; this also refreshes the standalone AgentMesh
        // relay/registry (not Helm-managed) and is a harmless belt-and-braces
        // for the Helm-managed ones.
        stepper.step("Rolling AgentMesh, controller, router, and sandboxes to the new images...");
        await rolloutRestartAll(execa);
        stepper.done("Workloads restarted");

        // ── Step 6: Verify health (gates success) ─────────────────────
        stepper.step("Verifying cluster health...");
        const health = await verifyHealth(execa);
        if (!health.healthy) {
          stepper.fail("Cluster is not healthy after the upgrade");
          console.error(chalk.red(`\n  ${health.reason}\n`));
          console.error(chalk.yellow(
            "  The Helm upgrade was atomic (auto-rolled-back on a failed rollout). If pods\n" +
            "  are still unhealthy — e.g. ImagePullBackOff / CrashLoopBackOff — revert with:\n" +
            "      kars upgrade --rollback\n",
          ));
          process.exit(1);
        }
        stepper.done("Cluster healthy on the new release");

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

/** Determine the deployed kars release, most-authoritative source first:
 *
 *  1. The **controller Deployment's image tag** — un-spoofable: it is the
 *     literal version of the bits running in the cluster (the upgrade path pins
 *     a version tag, not `:latest`). Only accepted when it parses as a version
 *     (a `:latest`-era cluster has no version here and falls through).
 *  2. The `karsRelease` Helm value stamped by `kars up` / `kars upgrade`.
 *  3. The chart `appVersion` — but the chart ships a static `0.1.0` sentinel
 *     that is never bumped, so that exact value is treated as "unknown".
 *
 *  Returns "" when genuinely unknown — important so a freshly-provisioned or
 *  unstamped cluster isn't wrongly treated as `v0.1.0` and blocked from
 *  upgrading by the "cluster is NEWER than target" guard. */
export async function detectCurrentVersion(execa: Execa, appVersion?: string): Promise<string> {
  // 1. Controller image tag (what's actually running).
  const { stdout: imgRaw } = await execa("kubectl", [
    "get", "deployment", "kars-controller", "-n", NS,
    "-o", "jsonpath={.spec.template.spec.containers[0].image}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  const imageTag = (imgRaw || "").trim().split(":").pop() ?? "";
  if (imageTag && imageTag !== "latest" && parseVersionTag(imageTag)) {
    return imageTag.startsWith("v") ? imageTag : `v${imageTag}`;
  }
  // 2. Stamped Helm value.
  const { stdout } = await execa("helm", [
    "get", "values", "kars", "-n", NS, "-o", "json",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  try {
    const vals = JSON.parse(stdout || "{}") as { karsRelease?: string };
    if (vals.karsRelease) return vals.karsRelease;
  } catch { /* ignore */ }
  // 3. Chart appVersion, ignoring the static `0.1.0` sentinel.
  const av = (appVersion ?? "").replace(/^v/, "");
  return av && av !== "0.1.0" ? `v${av}` : "";
}

/** `az acr import --force` one image. Returns true on success. */
async function acrImport(execa: Execa, acrName: string, src: string, target: string): Promise<boolean> {
  return execa("az", [
    "acr", "import", "--name", acrName, "--source", src, "--image", target, "--force",
  ], { stdio: "pipe" }).then(() => true).catch(() => false);
}

/** Rolling-restart every kars workload so the new images are pulled.
 *
 *  Order matters: the AgentMesh relay + registry are restarted (and waited on)
 *  FIRST, so the controller + sandboxes that connect to the mesh come up
 *  against the already-upgraded mesh rather than briefly attaching to the old
 *  one and getting disconnected. The mesh deployments are standalone (in the
 *  `agentmesh` namespace, applied from a manifest, NOT Helm-managed), so
 *  nothing else refreshes them. We target the two known deployments by name
 *  rather than `--all` to keep the blast radius off anything else that might
 *  share the namespace. All best-effort — a missing deployment/namespace is a
 *  no-op, never a hard failure. */
export async function rolloutRestartAll(execa: Execa): Promise<void> {
  // 1. AgentMesh relay + registry first, then wait for them.
  for (const dep of ["agentmesh-relay", "agentmesh-registry"]) {
    await execa("kubectl", ["rollout", "restart", `deployment/${dep}`, "-n", MESH_NS], { stdio: "pipe" }).catch(() => {});
    await execa("kubectl", ["rollout", "status", `deployment/${dep}`, "-n", MESH_NS, "--timeout=180s"], { stdio: "pipe" }).catch(() => {});
  }
  // 2. Controller (the inference-router runs as a sidecar inside each sandbox).
  await execa("kubectl", ["rollout", "restart", "deployment", "-n", NS, "-l", "app.kubernetes.io/name=kars"], { stdio: "pipe" }).catch(() => {});
  // 3. Sandboxes (per-component label, across namespaces).
  await execa("kubectl", ["rollout", "restart", "deployment", "-A", "-l", "kars.azure.com/component=sandbox"], { stdio: "pipe" }).catch(() => {});
  // 4. Wait for the controller to settle.
  await execa("kubectl", ["rollout", "status", "deployment", "-n", NS, "kars-controller", "--timeout=300s"], { stdio: "pipe" }).catch(() => {});
}

/** Structured health verdict for a post-upgrade / post-rollback check. */
export interface HealthResult {
  healthy: boolean;
  reason: string;
}

/** Verify the cluster is actually healthy after an image change — strong enough
 *  to gate success. Checks: (1) the controller Deployment is Available=True,
 *  and (2) no kars pod in `kars-system` or `agentmesh` is wedged in
 *  ImagePullBackOff / ErrImagePull / CrashLoopBackOff (the exact symptoms a bad
 *  `:version` image produces). Best-effort kubectl; any probe error degrades to
 *  a clear, non-healthy reason rather than a false "healthy". */
export async function verifyHealth(execa: Execa): Promise<HealthResult> {
  const { stdout: ctrl } = await execa("kubectl", [
    "get", "deployment", "kars-controller", "-n", NS,
    "-o", "jsonpath={.status.conditions[?(@.type=='Available')].status}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  if (ctrl.trim() !== "True") {
    return { healthy: false, reason: "kars-controller Deployment is not Available." };
  }
  // Scan for image-pull / crash-loop across the control-plane + mesh namespaces.
  for (const ns of [NS, MESH_NS]) {
    const { stdout } = await execa("kubectl", [
      "get", "pods", "-n", ns,
      "-o", "jsonpath={range .items[*]}{range .status.containerStatuses[*]}{.state.waiting.reason}{\" \"}{end}{end}",
    ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
    const bad = ["ImagePullBackOff", "ErrImagePull", "CrashLoopBackOff"].find((r) => stdout.includes(r));
    if (bad) {
      return { healthy: false, reason: `One or more pods in '${ns}' are in ${bad} (likely a bad image).` };
    }
  }
  return { healthy: true, reason: "" };
}
