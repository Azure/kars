// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import path from "path";
import fs from "fs";
import os from "os";
import { loadContext } from "../config.js";
import { stageRustBinaries } from "../lib/stage-rust-bin.js";
import { stageMeshPlugin } from "../lib/stage-mesh-plugin.js";
import { ensureAgtRepo } from "../lib/agt-bootstrap.js";

const DEFAULT_AGT_REPO = path.join(os.homedir(), "agent-governance-toolkit");

export function pushCommand(): Command {
  const cmd = new Command("push");

  cmd
    .description("Build and push images to ACR (uses cached context from last deploy)")
    .option("--acr <name>", "ACR name (default: from last deploy)")
    .option("--only <image>", "Build only one image: controller, router, sandbox, sandbox-base, relay, registry")
    .option("--include-base", "Include sandbox-base in a full push (skipped by default — rebuild only when upgrading OpenClaw/Python/Go)")
    .option("--apply", "Restart deployments after push so pods pick up new images")
    .option(
      "-m, --mesh-provider <provider>",
      "Mesh stack to build. Only 'agt' is supported (Microsoft AGT, in-memory). " +
        "Kept as a flag for backward-compatible scripts; the vendored Rust relay/registry have been removed.",
      "agt",
    )
    .option(
      "--agt-repo <path>",
      `Path to the agent-governance-toolkit checkout (relay/registry images are built from here). Defaults to $KARS_AGT_REPO or ${DEFAULT_AGT_REPO}`,
    )
    .option(
      "--agt-sdk-tarball <path>",
      "Path to a locally-packed @microsoft/agent-governance-sdk .tgz to install in the sandbox image (auto-discovered from $KARS_AGT_REPO/agent-governance-typescript otherwise).",
    )
    .action(async (options) => {
      const { execa } = await import("execa");
      const blue = chalk.hex("#0078D4");

      if (options.meshProvider && options.meshProvider !== "agt") {
        console.error(
          chalk.red(
            `\n  Error: --mesh-provider must be 'agt' (got "${options.meshProvider}"). ` +
              `Vendored Rust relay/registry were removed in Phase 5.2.\n`,
          ),
        );
        process.exit(1);
      }
      const meshProvider = "agt" as const;

      // Resolve AGT repo path — required to (re)build relay/registry images
      // Find repo root (look for deploy/helm directory). Done up-front
      // because the auto-clone path below reads vendor/agt/pin.json
      // relative to the root.
      let repoRoot = process.cwd();
      for (let i = 0; i < 5; i++) {
        if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
        repoRoot = path.dirname(repoRoot);
      }
      if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
        console.error(chalk.red("\n  Not in an kars repo. Run from the repo root.\n"));
        process.exit(1);
      }

      let agtRepo: string;
      const agtDockerfileRel = "agent-governance-python/agent-mesh/docker/Dockerfile";
      // Auto-clone the pinned AGT fork (vendor/agt/pin.json) when no
      // local clone is available. Lets fresh-machine `kars push --apply`
      // / `kars dev` work without the user having to know about the
      // AGT-main-vs-released schema gap. Caller-supplied --agt-repo or
      // $KARS_AGT_REPO still win. See cli/src/lib/agt-bootstrap.ts.
      try {
        agtRepo = await ensureAgtRepo(options.agtRepo, repoRoot);
      } catch (e: unknown) {
        agtRepo = options.agtRepo || process.env.KARS_AGT_REPO || DEFAULT_AGT_REPO;
        if (!options.only || options.only === "relay" || options.only === "registry") {
          console.error(chalk.red(`\n  Auto-cloning AGT failed:\n    ${(e as Error).message}\n`));
          console.error(chalk.red(`  Pass --agt-repo <path> or set $KARS_AGT_REPO, or pass --only <image> to skip mesh.\n`));
          process.exit(1);
        }
      }
      const agtRepoMissing = !fs.existsSync(path.join(agtRepo, agtDockerfileRel));
      if (agtRepoMissing && (!options.only || options.only === "relay" || options.only === "registry")) {
        console.error(chalk.red(`\n  Building relay/registry requires the AGT repo.`));
        console.error(chalk.red(`  Looked for: ${path.join(agtRepo, agtDockerfileRel)}`));
        console.error(chalk.red(`  Pass --agt-repo <path> or set $KARS_AGT_REPO, or pass --only <image> to skip mesh.\n`));
        process.exit(1);
      }

      // Resolve ACR from context or flag
      const ctx = loadContext();
      const acrName = options.acr || ctx?.acrName;
      const acrLoginServer = acrName ? `${acrName}.azurecr.io` : null;

      if (!acrName || !acrLoginServer) {
        console.error(chalk.red("\n  No ACR configured. Run 'kars up' first or pass --acr <name>.\n"));
        process.exit(1);
      }

      console.log(blue(`\n  kars · Push Images → ${acrLoginServer}\n`));

      // Login to ACR
      const spinner = ora("Logging into ACR...").start();
      try {
        await execa("az", ["acr", "login", "--name", acrName], { stdio: "pipe" });
        spinner.succeed("ACR login");
      } catch (e: any) {
        spinner.fail("ACR login failed");
        console.error(chalk.red(`  ${e.message}\n`));
        process.exit(1);
      }

      // Define mesh images. Only AGT is supported; the vendored fork was
      // removed in Phase 5.2. Tagged agentmesh-{relay,registry}-agt:latest
      // to match deploy/agentmesh-agt.yaml.
      const meshImages = agtRepoMissing
        ? []
        : [
            {
              name: "relay",
              tag: "agentmesh-relay-agt:latest",
              dockerfile: path.join(agtRepo, agtDockerfileRel),
              absoluteContext: agtRepo,
              buildArgs: ["--build-arg", "COMPONENT=relay", "--build-arg", `CACHE_BUST=${Date.now()}`],
            },
            {
              name: "registry",
              tag: "agentmesh-registry-agt:latest",
              dockerfile: path.join(agtRepo, agtDockerfileRel),
              absoluteContext: agtRepo,
              buildArgs: ["--build-arg", "COMPONENT=registry", "--build-arg", `CACHE_BUST=${Date.now()}`],
            },
          ];

      // Stage the AGT SDK tarball into .agt-sdk/ (build context). Same logic
      // as cli/src/commands/dev.ts: the sandbox Dockerfile always COPYs
      // .agt-sdk/ (the .keep file ensures it never fails); the RUN step
      // installs the tarball only when AGT_SDK_TARBALL is set.
      const sandboxBuildArgs: string[] = [
        "--build-arg", `SANDBOX_BASE_IMAGE=${acrLoginServer}/kars-sandbox-base:latest`,
        "--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/kars-inference-router:latest`,
        "--build-arg", `SANDBOX_CACHE_BUST=${Date.now()}`,
        "--build-arg", `MESH_PROVIDER=${meshProvider}`,
      ];
      const agtSdkStagingDir = path.join(repoRoot, ".agt-sdk");
      if (fs.existsSync(agtSdkStagingDir)) {
        for (const f of fs.readdirSync(agtSdkStagingDir)) {
          if (f.endsWith(".tgz") || f.endsWith(".tar.gz")) {
            fs.unlinkSync(path.join(agtSdkStagingDir, f));
          }
        }
      }
      {
        let tarballPath: string | null = null;
        if (options.agtSdkTarball) {
          if (!fs.existsSync(options.agtSdkTarball)) {
            console.error(chalk.red(`\n  Error: --agt-sdk-tarball not found: ${options.agtSdkTarball}\n`));
            process.exit(1);
          }
          tarballPath = options.agtSdkTarball;
        } else if (!agtRepoMissing) {
          // Auto-discover: a packed tarball next to the AGT TS workspace
          try {
            const tsDir = path.join(agtRepo, "agent-governance-typescript");
            const candidates = fs.readdirSync(tsDir).filter(
              f => f.startsWith("microsoft-agent-governance-sdk-") && f.endsWith(".tgz"),
            );
            if (candidates.length > 0) {
              tarballPath = path.join(tsDir, candidates[0]);
              console.log(chalk.dim(`  Auto-discovered AGT SDK tarball: ${candidates[0]}`));
            }
          } catch {
            /* AGT repo missing TS dir — fall through to npm install */
          }
        }
        if (tarballPath) {
          if (!fs.existsSync(agtSdkStagingDir)) fs.mkdirSync(agtSdkStagingDir, { recursive: true });
          const basename = path.basename(tarballPath);
          fs.copyFileSync(tarballPath, path.join(agtSdkStagingDir, basename));
          sandboxBuildArgs.push("--build-arg", `AGT_SDK_TARBALL=${basename}`);
        }
      }

      // Define all images
      // push.ts targets AKS, which is amd64. If we're running on an
      // arm64 host (Apple Silicon developer machine), `cargo build` on
      // the host produces a macOS arm64 binary that fails with
      // 'exec format error' when COPY'd into a linux/amd64 image. To
      // avoid that, swap the COPY-only Dockerfiles for the multi-stage
      // ones (which compile rust INSIDE docker for the right target).
      // CI runs on linux amd64 so it keeps the fast COPY-only path.
      const hostIsAmd64 = process.arch === "x64";
      const controllerDf = hostIsAmd64
        ? "controller/Dockerfile"
        : "controller/Dockerfile.multistage";
      const routerDf = hostIsAmd64
        ? "inference-router/Dockerfile"
        : "inference-router/Dockerfile.multistage";

      const images: Array<{
        name: string;
        tag: string;
        dockerfile: string;
        context?: string;
        absoluteContext?: string;
        buildArgs?: string[];
      }> = [
        { name: "controller", tag: "kars-controller:latest", dockerfile: controllerDf },
        { name: "router", tag: "kars-inference-router:latest", dockerfile: routerDf,
          buildArgs: ["--build-arg", `ROUTER_CACHE_BUST=${Date.now()}`] },
        { name: "sandbox-base", tag: "kars-sandbox-base:latest", dockerfile: "sandbox-images/openclaw/Dockerfile.base",
          buildArgs: ["--build-arg", `OPENCLAW_CACHE_BUST=${Date.now()}`] },
        { name: "sandbox", tag: "openclaw-sandbox:latest", dockerfile: "sandbox-images/openclaw/Dockerfile",
          buildArgs: sandboxBuildArgs },
        ...meshImages,
        // Multi-runtime adapter images — must match controller defaults in
        // `controller/src/reconciler/runtime.rs` (DEFAULT_*_IMAGE constants).
        { name: "runtime-openai-agents", tag: "kars-runtime-openai-agents:latest",
          dockerfile: "sandbox-images/openai-agents/Dockerfile" },
        { name: "runtime-maf-python", tag: "kars-runtime-maf-python:latest",
          dockerfile: "sandbox-images/maf-python/Dockerfile" },
        { name: "runtime-anthropic", tag: "kars-runtime-anthropic:latest",
          dockerfile: "sandbox-images/anthropic/Dockerfile" },
        { name: "runtime-langgraph", tag: "kars-runtime-langgraph:latest",
          dockerfile: "sandbox-images/langgraph/Dockerfile" },
        { name: "runtime-langgraph-ts", tag: "kars-runtime-langgraph-ts:latest",
          dockerfile: "sandbox-images/langgraph-ts/Dockerfile" },
        { name: "runtime-pydantic-ai", tag: "kars-runtime-pydantic-ai:latest",
          dockerfile: "sandbox-images/pydantic-ai/Dockerfile" },
      ];

      // Filter if --only specified; skip sandbox-base unless explicitly requested
      let targets = options.only
        ? images.filter(i => i.name === options.only)
        : options.includeBase
          ? images
          : images.filter(i => i.name !== "sandbox-base");

      // Auto-include sandbox-base if sandbox is in targets but base doesn't exist in ACR
      const hasSandbox = targets.some(i => i.name === "sandbox");
      const hasBase = targets.some(i => i.name === "sandbox-base");
      if (hasSandbox && !hasBase) {
        // Check if base image exists locally (would have been pushed previously)
        try {
          await execa("docker", ["image", "inspect", `${acrLoginServer}/kars-sandbox-base:latest`], { stdio: "pipe" });
        } catch {
          // Base not found locally — include it so the build succeeds
          console.log(chalk.yellow("  ℹ sandbox-base not found locally — building it first\n"));
          const baseImg = images.find(i => i.name === "sandbox-base")!;
          targets = [baseImg, ...targets];
        }
      }

      if (targets.length === 0) {
        console.error(chalk.red(`\n  Unknown image: ${options.only}. Options: controller, router, sandbox-base, sandbox, relay, registry\n`));
        process.exit(1);
      }

      let failures = 0;
      for (const img of targets) {
        const spin = ora(`Building ${img.tag}...`).start();
        try {
          // Rust images (controller + router) use COPY-only Dockerfiles
          // ONLY when host is amd64 (so cargo's native output matches
          // the linux/amd64 image). Otherwise we used Dockerfile.multistage
          // (see above) which compiles inside docker — no host-side stage
          // needed. AKS nodes are amd64.
          if (hostIsAmd64) {
            if (img.name === "controller") {
              spin.text = `Compiling kars-controller (amd64) for ${img.tag}...`;
              await stageRustBinaries(repoRoot, ["kars-controller"], "amd64");
            } else if (img.name === "router") {
              spin.text = `Compiling kars-inference-router (amd64) for ${img.tag}...`;
              await stageRustBinaries(repoRoot, ["kars-inference-router"], "amd64");
            }
          }
          if (img.name === "sandbox") {
            spin.text = `Building mesh-plugin (TypeScript) for ${img.tag}...`;
            await stageMeshPlugin(repoRoot);
          }
          spin.text = `Building ${img.tag}...`;
          // Dockerfile path: absolute if provided absolute (AGT case), else relative to repoRoot
          const dockerfilePath = path.isAbsolute(img.dockerfile)
            ? img.dockerfile
            : path.join(repoRoot, img.dockerfile);
          // Build context: absolute override > relative > repoRoot
          const buildContext = img.absoluteContext
            ? img.absoluteContext
            : img.context
              ? path.join(repoRoot, img.context)
              : repoRoot;
          const args = [
            "build", "--platform", "linux/amd64",
            "--provenance=false", "--sbom=false",
            "-f", dockerfilePath,
            "-t", `${acrLoginServer}/${img.tag}`,
            ...(img.buildArgs || []),
            buildContext,
          ];
          await execa("docker", args, { stdio: "pipe" });
          spin.text = `Pushing ${img.tag}...`;

          // Push with retry
          for (let attempt = 1; attempt <= 3; attempt++) {
            try {
              if (attempt > 1) await execa("az", ["acr", "login", "--name", acrName], { stdio: "pipe" });
              await execa("docker", ["push", `${acrLoginServer}/${img.tag}`], { stdio: "pipe" });
              break;
            } catch (e: any) {
              if (attempt === 3) throw e;
              spin.text = `Push ${img.tag} failed, retry ${attempt + 1}/3...`;
              await new Promise(r => setTimeout(r, 3000));
            }
          }
          spin.succeed(`${img.tag}`);
        } catch (e: any) {
          spin.fail(`${img.tag} — ${e.message?.split("\n")[0] || "failed"}`);
          failures++;
        }
      }

      if (failures > 0) {
        console.error(chalk.red(`\n  ${failures}/${targets.length} images failed.\n`));
        process.exit(1);
      }

      console.log(chalk.green(`\n  ✓ ${targets.length} image(s) pushed to ${acrLoginServer}\n`));

      // Rollout restart if --apply
      if (options.apply) {
        const ns = "kars-system";

        // The AGT manifest is the only mesh stack now; on --apply we ensure
        // it's installed and the helm mesh.provider=agt value is set so
        // future controller restarts and sandbox spawns carry the AGT env.
        if (!options.only || options.only === "relay" || options.only === "registry") {
          const meshSpin = ora("Applying AGT mesh manifest (deploy/agentmesh-agt.yaml)...").start();
          try {
            const agtManifest = path.join(repoRoot, "deploy/agentmesh-agt.yaml");
            if (!fs.existsSync(agtManifest)) {
              throw new Error(`AGT manifest missing: ${agtManifest}`);
            }
            await execa("kubectl", ["apply", "-f", agtManifest], { stdio: "pipe" });
            meshSpin.succeed("AGT mesh manifest applied");
          } catch (e: any) {
            meshSpin.fail(`Mesh manifest apply failed: ${e.message?.split("\n")[0]}`);
          }

          const helmSpin = ora("Setting helm mesh.provider=agt...").start();
          try {
            await execa("helm", [
              "upgrade", "kars", path.join(repoRoot, "deploy/helm/kars"),
              "--namespace", ns,
              "--reuse-values",
              "--set", "mesh.provider=agt",
            ], { stdio: "pipe" });
            helmSpin.succeed("helm mesh.provider=agt");
          } catch (e: any) {
            helmSpin.fail(`helm upgrade failed: ${e.message?.split("\n")[0]} (apply manually: helm upgrade kars deploy/helm/kars --reuse-values --set mesh.provider=agt)`);
          }
        }

        // Restart controller (manages all sandbox pods)
        const spin = ora("Restarting deployments...").start();
        try {
          await execa("kubectl", ["rollout", "restart", "deployment", "kars-controller", "-n", ns], { stdio: "pipe" });
          spin.text = "Restarted kars-controller";

          // If sandbox/router image changed, roll the sandbox Deployments.
          // We use `kubectl rollout restart deploy -A -l kars.azure.com/component=sandbox`
          // — annotation-bump that triggers a real rolling restart. The
          // previous `delete pods --grace-period=10` approach was racy:
          // the deletion fired BEFORE ACR/kubelet finished propagating
          // the new `:latest` digest, so the recreated pod sometimes
          // grabbed the OLD digest from a node-cache and the new push
          // silently became "the next latest" with no one to re-pull.
          // Rollout restart is the K8s-native fix that the deployment
          // controller waits on (new pod must be Ready before old goes).
          const sandboxImages = ["sandbox", "router"];
          if (!options.only || sandboxImages.includes(options.only)) {
            const { stdout: nsLines } = await execa("kubectl", [
              "get", "namespaces", "-o", "name",
            ], { stdio: "pipe" });
            const perAgentNs = nsLines
              .split("\n")
              .map(l => l.replace("namespace/", "").trim())
              .filter(n => n.startsWith("kars-") && n !== "kars-system");
            if (perAgentNs.length > 0) {
              spin.text = `Rolling out new image to ${perAgentNs.length} sandbox(es)...`;
              // Per-namespace rollout restart of every Deployment labeled
              // as a sandbox. Runs in parallel; each waits for the
              // Deployment controller to bring up a Ready replica with
              // the new pulled image before the old one terminates.
              await Promise.all(perAgentNs.map(async nsName => {
                try {
                  const { stdout: deps } = await execa("kubectl", [
                    "get", "deploy", "-n", nsName,
                    "-l", "kars.azure.com/component=sandbox",
                    "-o", "name",
                  ], { stdio: "pipe" });
                  const names = deps.split("\n").map(s => s.trim()).filter(Boolean);
                  for (const dep of names) {
                    await execa("kubectl", [
                      "rollout", "restart", dep, "-n", nsName,
                    ], { stdio: "pipe" }).catch(() => {});
                  }
                } catch { /* skip namespace */ }
              }));
              spin.text = `Waiting for ${perAgentNs.length} sandbox rollout(s) to complete...`;
              // Wait for the rollouts to finish so the user can trust
              // that on success their pods ARE running the new image.
              await Promise.all(perAgentNs.map(async nsName => {
                try {
                  const { stdout: deps } = await execa("kubectl", [
                    "get", "deploy", "-n", nsName,
                    "-l", "kars.azure.com/component=sandbox",
                    "-o", "name",
                  ], { stdio: "pipe" });
                  for (const dep of deps.split("\n").map(s => s.trim()).filter(Boolean)) {
                    await execa("kubectl", [
                      "rollout", "status", dep, "-n", nsName, "--timeout=120s",
                    ], { stdio: "pipe" }).catch(() => {});
                  }
                } catch { /* skip */ }
              }));
            }
          }

          // If relay/registry changed, restart agentmesh
          if (!options.only || options.only === "relay" || options.only === "registry") {
            await execa("kubectl", ["rollout", "restart", "deployment", "-n", "agentmesh"], { stdio: "pipe" }).catch(() => {});
          }

          spin.succeed("Deployments restarted — pods will pull new images");
        } catch (e: any) {
          spin.fail(`Rollout restart failed: ${e.message?.split("\n")[0]}`);
        }
      } else if (ctx?.aksCluster) {
        console.log(chalk.dim(`  To apply: kars push --apply`));
        console.log(chalk.dim(`  Or manually: kubectl rollout restart deployment -n kars-system\n`));
      }
    });

  return cmd;
}
