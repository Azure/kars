// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import path from "path";
import fs from "fs";
import os from "os";
import { loadContext } from "../config.js";
import { preparePushTarget } from "../lib/deployment-target.js";
import { inspectMeshInstallation } from "../lib/mesh-release.js";
import { applyPushedImages } from "./push-apply.js";
import { dockerPushDigest, MANAGED_MCP_IMAGE_TARGET, PUSH_COMPONENTS, RUNTIME_IMAGE_TARGETS } from "../lib/image-targets.js";
import { stageRustBinaries } from "../lib/stage-rust-bin.js";
import { stageMeshPlugin } from "../lib/stage-mesh-plugin.js";
import { ensureAgtRepo, ensureAgtWheels } from "../lib/agt-bootstrap.js";

const DEFAULT_AGT_REPO = path.join(os.homedir(), "agent-governance-toolkit");

export function pushCommand(): Command {
  const cmd = new Command("push");

  cmd
    .description("Build and push images to ACR (uses cached context from last deploy)")
    .option("--acr <name>", "ACR name (default: from last deploy)")
    .option("--subscription <id>", "Azure subscription (must match saved deployment when present)")
    .option("--only <image>", "Build one image: controller, router, sandbox, sandbox-base, relay, registry, mcp-everything, or runtime-*")
    .option("--include-base", "Include sandbox-base in a full push (skipped by default — rebuild only when upgrading OpenClaw/Python/Go)")
    .option("--apply", "Apply selected image configuration and verify the resulting rollouts")
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
      const { execa: rawExeca } = await import("execa");
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
      if (options.only && !PUSH_COMPONENTS.includes(options.only)) throw new Error(`Unknown image component: ${options.only}`);
      if (options.apply && options.only === "sandbox-base") {
        throw new Error("sandbox-base is build-only; omit --apply and rebuild a deployable sandbox image.");
      }

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

      const ctx = loadContext();
      const execa = await preparePushTarget(rawExeca, ctx, options);
      const updatesMesh = !options.only || options.only === "relay" || options.only === "registry";
      const mesh = options.apply && updatesMesh ? await inspectMeshInstallation(execa) : undefined;
      if (mesh?.kind === "absent") throw new Error("AgentMesh is not installed; install it before pushing mesh updates");
      if (mesh?.kind === "external") {
        throw new Error("External AgentMesh will not be changed. Select an explicit core --only target (controller, router, sandbox, or runtime-*).");
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
        } else {
          // 1. Prefer the vendored tarball in `vendor/agt/` (single source of
          //    truth pinned by `vendor/agt/pin.json`). This is the build the
          //    Cargo `[patch.crates-io]` block + the file: deps in
          //    mesh-plugin/runtimes/openclaw package.json all reference. If
          //    we shipped a different SDK in the sandbox image we'd get a
          //    DID-derivation mismatch between in-pod TS code and in-pod
          //    Rust code (controller mesh peer registers as `did:mesh:<x>`,
          //    sandbox-side code computes `did:mesh:<y>` from the same key).
          const vendoredDir = path.join(repoRoot, "vendor", "agt");
          try {
            const vendored = fs.readdirSync(vendoredDir).filter(
              f => f.startsWith("microsoft-agent-governance-sdk-") && f.endsWith(".tgz"),
            );
            if (vendored.length > 0) {
              tarballPath = path.join(vendoredDir, vendored[0]);
              console.log(chalk.dim(`  Vendored AGT SDK tarball: ${vendored[0]}`));
            }
          } catch { /* no vendored dir */ }

          // 2. Fall back to the AGT clone's packed output. Used when developers
          //    re-pack the SDK locally (`npm pack`) without copying it back
          //    into vendor/agt/ — keeps the inner-loop fast without forcing a
          //    `git add` step.
          if (!tarballPath && !agtRepoMissing) {
            try {
              const tsDir = path.join(agtRepo, "agent-governance-typescript");
              const candidates = fs.readdirSync(tsDir).filter(
                f => f.startsWith("microsoft-agent-governance-sdk-") && f.endsWith(".tgz"),
              );
              if (candidates.length > 0) {
                tarballPath = path.join(tsDir, candidates[0]);
                console.log(chalk.dim(`  Auto-discovered AGT SDK tarball from clone: ${candidates[0]}`));
              }
            } catch { /* AGT repo missing TS dir — fall through to npm install */ }
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
        { name: MANAGED_MCP_IMAGE_TARGET.name, tag: `${MANAGED_MCP_IMAGE_TARGET.repo}:latest`,
          dockerfile: "sandbox-images/mcp-everything/Dockerfile", context: "sandbox-images/mcp-everything" },
        // Shared with release imports, upgrade values, and push application.
        ...RUNTIME_IMAGE_TARGETS.map(runtime => ({
          name: runtime.name, tag: `${runtime.repo}:latest`,
          dockerfile: `sandbox-images/${runtime.name.slice("runtime-".length)}/Dockerfile`,
        })),
      ];

      // Sandbox images whose Dockerfile `COPY runtimes/wheels/` and
      // therefore require `ensureAgtWheels()` to have run before
      // `docker build`. Keep in lockstep with the `COPY runtimes/wheels/`
      // grep results across sandbox-images/*/Dockerfile.
      const PYTHON_RUNTIMES = new Set([
        "runtime-openai-agents",
        "runtime-maf-python",
        "runtime-anthropic",
        "runtime-langgraph",
        "runtime-pydantic-ai",
        "runtime-hermes",
      ]);

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
      const pushedArtifacts = new Map<string, string>();
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
          // Python runtime sandbox images COPY runtimes/wheels/*.whl
          // into their build context. The wheel directory is .gitignored
          // and only this auto-build keeps `kars push --only runtime-<X>`
          // working on a fresh checkout. ensureAgtWheels() is a no-op
          // when the cache stamp matches the current AGT pin SHA.
          if (PYTHON_RUNTIMES.has(img.name) && !agtRepoMissing) {
            spin.text = `Building AGT Python wheels for ${img.tag}...`;
            await ensureAgtWheels(agtRepo, repoRoot);
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
              const pushed = await execa("docker", ["push", `${acrLoginServer}/${img.tag}`], { stdio: "pipe" });
              if (options.apply && img.name !== "sandbox-base") {
                const digest = dockerPushDigest(`${pushed.stdout}\n${pushed.stderr}`);
                if (!digest) throw new Error(`Docker did not provide an unambiguous pushed digest for ${img.name}; refusing to apply a mutable tag`);
                pushedArtifacts.set(img.name, `${acrLoginServer}/${img.tag}@${digest}`);
              }
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
        const spin = ora("Applying pushed images to their owning deployments...").start();
        try {
          const applied = await applyPushedImages(execa, targets.map(image => ({
            name: image.name, image: pushedArtifacts.get(image.name) ?? `${acrLoginServer}/${image.tag}`,
          })), path.join(repoRoot, "deploy/helm/kars"), mesh);
          spin.succeed(`Selected image configuration applied; ${applied.updatedSandboxes} eligible sandbox deployment(s) verified`);
          if (applied.updatedManagedMcp !== undefined) console.log(chalk.dim(`  ${applied.updatedManagedMcp} managed MCP workload(s) verified; unreferenced defaults updated only.`));
          if (applied.preservedOverrides) console.log(chalk.dim(`  Preserved ${applied.preservedOverrides} explicit sandbox/overlay image override(s).`));
          if (applied.buildOnly.length) console.log(chalk.dim(`  Build-only (not deployed): ${applied.buildOnly.join(", ")}.`));
        } catch (e: any) {
          spin.fail(`Image apply failed: ${e.message?.split("\n")[0]}`);
          throw e;
        }
      } else if (ctx?.aksCluster) {
        console.log(chalk.dim(`  To apply: kars push --apply`));
        console.log(chalk.dim(`  Or manually: kubectl rollout restart deployment -n kars-system\n`));
      }
    });

  return cmd;
}
