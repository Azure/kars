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
    .option("--yes", "Skip the confirmation prompt (assume yes).", false)
    .addHelpText("after", `
Examples:
  kars upgrade                     # Upgrade to the latest GitHub release
  kars upgrade --to v0.1.16        # Pin a specific release
  kars upgrade --dry-run           # Show changelog + impact + plan, make no changes
  kars upgrade --yes               # Skip the confirmation prompt
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

        // ── Pre-flight: cluster must be able to run the upgrade ────────
        // Fail fast on a degraded/stopped cluster (e.g. all nodes NotReady)
        // BEFORE the image import + Helm --wait that would only time out and
        // roll back. Read-only; only hard-blocks when NO node is Ready. The
        // existing post-upgrade health gate still guards correctness.
        if (!options.rollback) {
          const pre = await assertClusterUpgradeable(execa);
          if (!pre.ok) {
            stepper.stop();
            console.error(chalk.red(`\n  ✗ Cluster is not in a state to upgrade — no changes made.`));
            console.error(chalk.red(`  ${pre.reason}\n`));
            for (const hint of pre.hints) console.error(chalk.dim(`  ${hint}`));
            console.error();
            process.exit(1);
          }
          for (const hint of pre.hints) stepper.detail("info", hint);
        }

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

        // ── Changelog summary + impact + confirmation ─────────────────
        // Show what's about to change and the blast radius, then confirm
        // before any write. The dry-run above already exited; this only runs
        // for a real upgrade. Auto-proceeds under --yes or a non-TTY stdin.
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
 *  3. **Image-digest match** — the controller's running image digest matched
 *     against published release digests. Recovers the real version on a cluster
 *     deployed before the stamp existed and still on `:latest` (where 1 and 2
 *     both come up empty), since `az acr import` preserves content-addressed
 *     digests. Best-effort network call; never overrides 1 or 2.
 *  4. The chart `appVersion` — but the chart ships a static `0.1.0` sentinel
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
  // 3. Image-digest match (recovers an old `:latest` cluster's real version).
  const byDigest = await detectVersionByImageDigest(execa).catch(() => undefined);
  if (byDigest) return byDigest;
  // 4. Chart appVersion, ignoring the static `0.1.0` sentinel.
  const av = (appVersion ?? "").replace(/^v/, "");
  return av && av !== "0.1.0" ? `v${av}` : "";
}

/** Resolve the deployed version by matching the controller pod's running image
 *  digest to the digests of recent published `kars-controller` release tags.
 *  Read-only + best-effort: any failure returns undefined so the caller falls
 *  through to the appVersion sentinel. */
async function detectVersionByImageDigest(execa: Execa): Promise<string | undefined> {
  // Scan kars-controller container statuses for a running image digest
  // (`imageID` is like `…/kars-controller@sha256:<digest>`). Skips Pending pods
  // (empty imageID) and tolerates rollouts with multiple replicas.
  const { stdout: ids } = await execa("kubectl", [
    "get", "pods", "-n", NS, "-l", "app.kubernetes.io/name=kars",
    "-o", "jsonpath={range .items[*]}{range .status.containerStatuses[*]}{.image}{\"|\"}{.imageID}{\"\\n\"}{end}{end}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

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

  interface DeployJson {
    metadata?: { name?: string; namespace?: string };
    spec?: { replicas?: number; template?: { spec?: { containers?: Array<{ name?: string; image?: string }> } } };
    status?: { readyReplicas?: number; replicas?: number };
  }
  const readyOf = (d: DeployJson): string => {
    const ready = d.status?.readyReplicas ?? 0;
    const desired = d.spec?.replicas ?? d.status?.replicas ?? 0;
    return `${ready}/${desired}`;
  };
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
  const controllerCount = rows.length - sandboxCount;
  console.log(chalk.dim(`\n  ${rows.length} workload(s) will be rolling-restarted (${controllerCount} controller + ${sandboxCount} sandbox(es)).`));
  console.log(chalk.dim(`  Each sandbox restarts its agent pod; in-flight agent work is interrupted briefly.\n`));
}

/** Print a concise changelog of the releases between current and target. Reads
 *  public GitHub release/tag APIs; best-effort and never throws. */
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
export function summarizeChangelog(text: string, maxLines = 8): string[] {
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

/** Pre-flight: can this cluster actually accept an upgrade right now? The upgrade
 *  reimports images and runs `helm upgrade --wait`, which needs schedulable,
 *  Ready nodes. A stopped/degraded cluster (all nodes NotReady — e.g. an AKS
 *  cluster whose VMSS was deallocated, or a broken CNI) would burn minutes and
 *  then time out + roll back. Detect it up front. Read-only. */
export async function assertClusterUpgradeable(
  execa: Execa,
): Promise<{ ok: boolean; reason: string; hints: string[] }> {
  const { stdout } = await execa("kubectl", [
    "get", "nodes",
    "-o", "jsonpath={range .items[*]}{.metadata.name}{\"|\"}{range .status.conditions[?(@.type=='Ready')]}{.status}{end}{\"\\n\"}{end}",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

  const lines = stdout.split("\n").map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) {
    // Couldn't read nodes — don't hard-block on an unexpected API shape; the
    // later `helm --wait` still guards correctness.
    return { ok: true, reason: "", hints: [] };
  }
  const total = lines.length;
  const ready = lines.filter((l) => l.endsWith("|True")).length;

  if (ready === 0) {
    return {
      ok: false,
      reason: `All ${total} cluster node(s) are NotReady — the upgrade can't schedule new pods and would time out.`,
      hints: [
        "Check node health:   kubectl get nodes",
        "If the AKS cluster is stopped, start it:   az aks start -g <rg> -n <cluster>",
        "If nodes are stuck (CNI/kubelet), check:   kubectl describe nodes",
        "Re-run `kars upgrade` once nodes are Ready.",
      ],
    };
  }
  // Some-but-not-all Ready is allowed (the upgrade can still proceed) but worth
  // surfacing — the controller wants 2 replicas and `helm --wait` needs them.
  return { ok: true, reason: "", hints: ready < total ? [`Note: ${ready}/${total} nodes Ready.`] : [] };
}
