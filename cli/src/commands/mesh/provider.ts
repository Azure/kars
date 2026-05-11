// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// `azureclaw mesh provider <vendored|agt>` — live-switch the mesh stack
// of a deployed cluster without rebuilding any images.
//
// Pre-condition: both `agentmesh-{relay,registry}:latest` and
// `agentmesh-{relay,registry}-agt:latest` already exist in the ACR
// (run `azureclaw push --mesh-provider=agt` once to seed AGT images,
// `azureclaw push` to seed the vendored pair). This command only
// flips manifests + helm value + restarts the controller; it does not
// build or push anything.
//
// What it does, in order:
//   1. Detect the current cluster provider (does `kubectl get deploy
//      -n agentmesh postgres` succeed? then vendored, else AGT).
//   2. If already on the target, exit no-op.
//   3. kubectl delete -f deploy/agentmesh-<current>.yaml --ignore-not-found
//   4. kubectl apply  -f deploy/agentmesh-<target>.yaml
//   5. helm upgrade azureclaw … --reuse-values --set mesh.provider=<target>
//   6. kubectl rollout restart deploy/<controller> -n <ns>
//   7. (Optional, --restart-sandboxes) restart every sandbox deployment
//      in azureclaw-* namespaces so new pods pick up the env change.
//
// Existing sandbox pods keep using their compiled-in provider until
// restarted; sub-agents spawned from existing parents inherit the
// parent's provider unless the parent is restarted. With
// --restart-sandboxes we kill all of them so the next message goes
// through the new stack.

import { Command } from "commander";
import chalk from "chalk";
import * as fs from "node:fs";
import * as path from "node:path";
import { execa } from "execa";
import { banner, section, kvLine, checkLine } from "../../stepper.js";
import { loadContext } from "../../config.js";

type Provider = "vendored" | "agt";

function repoRoot(): string {
  // mesh/ is two directories below repo/cli/src/commands/mesh/
  // dist layout differs, but resolving up from cwd works in both because
  // we always invoke via the bin shim and cwd may be anywhere. Use the
  // env var set by the bin wrapper, or fall back to a sensible default.
  if (process.env.AZURECLAW_REPO_ROOT) return process.env.AZURECLAW_REPO_ROOT;
  // Walk up from this module's URL.
  let dir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..", "..", "..");
  // If dist/, climb once more.
  if (path.basename(dir) === "dist") dir = path.dirname(dir);
  return dir;
}

async function detectCurrentProvider(): Promise<Provider | null> {
  // Vendored has Postgres; AGT does not.
  try {
    await execa("kubectl", ["get", "deploy", "postgres", "-n", "agentmesh"], { stdio: "pipe", timeout: 10_000 });
    return "vendored";
  } catch {
    /* not vendored */
  }
  try {
    await execa("kubectl", ["get", "deploy", "agentmesh-relay", "-n", "agentmesh"], { stdio: "pipe", timeout: 10_000 });
    return "agt";
  } catch {
    return null;
  }
}

async function helmRelease(): Promise<{ name: string; ns: string } | null> {
  try {
    const { stdout } = await execa("helm", ["list", "-A", "-o", "json"], { stdio: "pipe", timeout: 15_000 });
    const releases: Array<{ name: string; namespace: string; chart: string }> = JSON.parse(stdout);
    const r = releases.find((x) => /azureclaw/i.test(x.chart) || /azureclaw/i.test(x.name));
    if (r) return { name: r.name, ns: r.namespace };
  } catch { /* fall through */ }
  return null;
}

export function attachProviderSubcommand(parent: Command): void {
  parent
    .command("provider <target>")
    .description("Switch the deployed mesh stack between 'vendored' and 'agt' (no rebuild)")
    .option("--restart-sandboxes", "Also roll every sandbox deployment so existing pods pick up the new provider", false)
    .option("--manifest-dir <path>", "Override the deploy/ directory (default: repo root /deploy)")
    .action(async (target: string, opts: { restartSandboxes?: boolean; manifestDir?: string }) => {
      banner("AzureClaw · Mesh", `Switch provider → ${target}`);

      if (target !== "vendored" && target !== "agt") {
        console.error(chalk.red(`  ✗ Invalid target '${target}'. Use 'vendored' or 'agt'.`));
        process.exit(2);
      }

      const root = repoRoot();
      const manifestDir = opts.manifestDir ? path.resolve(opts.manifestDir) : path.join(root, "deploy");
      const vendoredManifest = path.join(manifestDir, "agentmesh.yaml");
      const agtManifest = path.join(manifestDir, "agentmesh-agt.yaml");

      if (!fs.existsSync(vendoredManifest) || !fs.existsSync(agtManifest)) {
        console.error(chalk.red(`  ✗ Manifests not found under ${manifestDir}`));
        console.error(chalk.dim(`    Looked for: agentmesh.yaml + agentmesh-agt.yaml`));
        console.error(chalk.dim(`    Pass --manifest-dir or set cwd to the repo root.`));
        process.exit(2);
      }

      section("Detect");
      const current = await detectCurrentProvider();
      if (current === null) {
        console.log(chalk.yellow("  ⚠ Could not detect a mesh stack in the 'agentmesh' namespace."));
        console.log(chalk.dim("    Will install the target stack from scratch."));
      } else {
        kvLine("Current provider", current);
      }
      kvLine("Target provider", target);

      if (current === target) {
        console.log();
        console.log(chalk.green("  ✓ Already on ") + chalk.bold(target) + chalk.green(". Nothing to do."));
        return;
      }

      const oldManifest = current === "agt" ? agtManifest : vendoredManifest;
      const newManifest = target === "agt" ? agtManifest : vendoredManifest;

      // Step 1 — remove old manifest if any.
      if (current !== null) {
        section("Remove old stack");
        try {
          await execa("kubectl", ["delete", "-f", oldManifest, "--ignore-not-found", "--wait=false"], {
            stdio: "inherit",
            timeout: 60_000,
          });
          checkLine(true, `Deleted ${path.basename(oldManifest)}`);
        } catch (err) {
          checkLine(false, `Failed to delete ${path.basename(oldManifest)}`);
          throw err;
        }
      }

      // Step 2 — apply new manifest.
      section("Apply new stack");
      try {
        await execa("kubectl", ["apply", "-f", newManifest], {
          stdio: "inherit",
          timeout: 120_000,
        });
        checkLine(true, `Applied ${path.basename(newManifest)}`);
      } catch (err) {
        checkLine(false, `Failed to apply ${path.basename(newManifest)}`);
        throw err;
      }

      // Step 3 — flip helm value.
      section("Update helm release");
      const release = await helmRelease();
      if (!release) {
        console.log(chalk.yellow("  ⚠ No azureclaw helm release found — skipping helm upgrade."));
        console.log(chalk.dim("    Controller env var will not change until next helm install."));
      } else {
        kvLine("Release", `${release.name} (ns ${release.ns})`);
        try {
          await execa("helm", [
            "upgrade", release.name,
            path.join(root, "deploy/helm/azureclaw"),
            "-n", release.ns,
            "--reuse-values",
            "--set", `mesh.provider=${target}`,
          ], { stdio: "inherit", timeout: 180_000 });
          checkLine(true, `helm upgrade --set mesh.provider=${target}`);
        } catch (err) {
          checkLine(false, "helm upgrade failed");
          throw err;
        }
      }

      // Step 4 — restart controller so AZURECLAW_MESH_PROVIDER takes effect.
      if (release) {
        section("Restart controller");
        try {
          await execa("kubectl", ["rollout", "restart", "deploy/azureclaw-controller", "-n", release.ns], {
            stdio: "inherit",
            timeout: 60_000,
          });
          await execa("kubectl", ["rollout", "status", "deploy/azureclaw-controller", "-n", release.ns, "--timeout=120s"], {
            stdio: "inherit",
            timeout: 130_000,
          });
          checkLine(true, "controller restarted");
        } catch (err) {
          checkLine(false, "controller restart failed");
          throw err;
        }
      }

      // Step 5 — optional sandbox roll.
      if (opts.restartSandboxes) {
        section("Restart sandbox pods");
        try {
          const { stdout } = await execa("kubectl", [
            "get", "deploy", "-A",
            "-l", "app.kubernetes.io/managed-by=azureclaw",
            "-o", "jsonpath={range .items[*]}{.metadata.namespace}/{.metadata.name}{\"\\n\"}{end}",
          ], { stdio: "pipe", timeout: 30_000 });
          const deps = stdout.split("\n").map((s) => s.trim()).filter(Boolean);
          if (deps.length === 0) {
            console.log(chalk.dim("  No sandbox deployments found."));
          } else {
            for (const d of deps) {
              const [ns, name] = d.split("/");
              try {
                await execa("kubectl", ["rollout", "restart", `deploy/${name}`, "-n", ns], {
                  stdio: "pipe",
                  timeout: 30_000,
                });
                checkLine(true, `${ns}/${name}`);
              } catch {
                checkLine(false, `${ns}/${name} (skipped)`);
              }
            }
          }
        } catch {
          console.log(chalk.yellow("  ⚠ Could not list sandbox deployments — skipping rollout."));
        }
      } else {
        console.log();
        console.log(chalk.dim("  Existing sandbox pods keep their compiled-in provider until restarted."));
        console.log(chalk.dim("  Re-run with --restart-sandboxes to flip them now, or just `azureclaw up` new sandboxes."));
      }

      console.log();
      console.log(chalk.green("  ✓ ") + chalk.bold(`Mesh provider switched to ${target}.`));
      console.log();

      // Touch context so future commands know which provider we're on.
      try {
        const ctx = loadContext();
        if (ctx) {
          // Cast through unknown since meshProvider is not on the formal type.
          (ctx as unknown as { meshProvider?: Provider }).meshProvider = target;
          // saveContext is intentionally not imported to avoid coupling; the
          // next dev/push/up command will refresh the cache anyway.
        }
      } catch { /* best-effort */ }
    });
}
