// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * up/images.ts — the `kars up` image-acquisition step (Step 6).
 *
 * Three mutually-exclusive sources, checked in this order:
 *   1. --release [version]  → import the PUBLIC, cosign-signed GHCR release
 *                             images (ghcr.io/azure/*) into the user's ACR.
 *                             No local build, no Rust toolchain. Bare
 *                             --release uses :latest; --release <tag> pins.
 *   2. --build              → build from source and push (developer mode).
 *                             Stages Rust binaries natively on linux/amd64,
 *                             else uses the *.multistage Dockerfiles.
 *   3. (default)            → import pre-built images from --source-acr.
 *
 * Extracted from up.ts to keep that file under the LOC budget and to make
 * the image step independently testable.
 */

import { execa } from "execa";
import * as path from "path";
import type { Stepper } from "../../stepper.js";
import { ensureAgtRepo, ensureAgtWheels } from "../../lib/agt-bootstrap.js";
import { stageRustBinaries } from "../../lib/stage-rust-bin.js";
import { isPhaseSkippable, markPhaseDone, type ResumeTopology } from "./resume.js";

export interface AcquireImagesContext {
  stepper: Stepper;
  options: {
    build?: boolean;
    /** `true` for bare `--release` (→ latest), or a version/tag string. */
    release?: string | boolean;
    sourceAcr: string;
    skipRuntimeImages?: boolean;
  };
  /** e.g. "karsaovbu7.azurecr.io" */
  acrLoginServer: string;
  /** ACR name (login server without the `.azurecr.io` suffix). */
  acr: string;
  repoRoot: string;
  resumeFromPhase: Parameters<typeof isPhaseSkippable>[1];
  resumeTopology: ResumeTopology;
}

/** Acquire all kars images into the user's ACR per the chosen source. */
export async function acquireImages(ctx: AcquireImagesContext): Promise<void> {
  const { stepper, options, acrLoginServer, acr, repoRoot, resumeFromPhase, resumeTopology } = ctx;

  // --release [version]: pull the PUBLIC signed GHCR release images
  // instead of building or importing from a private source ACR. Bare
  // --release → latest published; --release <tag> pins. These are the
  // same cosign-signed multi-arch images `kars dev --release` runs.
  const releaseMode =
    options.release === true ||
    (typeof options.release === "string" && options.release.length > 0);
  const releaseVersion =
    typeof options.release === "string" && options.release.length > 0
      ? options.release
      : "latest";

  if (isPhaseSkippable("images", resumeFromPhase)) {
    stepper.step(
      releaseMode
        ? "Importing published GHCR release images..."
        : options.build
          ? "Building and pushing images..."
          : "Importing images from source ACR...",
    );
    stepper.detail("ok", "Already pushed/imported in previous run — skipping");
    stepper.done("Images (skipped — resumed from prior run)");
  } else if (releaseMode) {
    // ── Release mode: import public GHCR images into the user's ACR ──
    // `az acr import` pulls straight from public GHCR (no auth needed)
    // and re-tags into the user's ACR under the :latest names the Helm
    // chart + agentmesh manifest reference. No local build, no Rust, no
    // source-ACR access required.
    stepper.step(`Importing published GHCR release images (${releaseVersion})...`);
    const GHCR = "ghcr.io/azure";
    // source GHCR repo (version-tagged) → target ACR repo (:latest)
    const releaseImages: Array<{ src: string; target: string; required: boolean }> = [
      { src: `${GHCR}/kars-controller:${releaseVersion}`, target: "kars-controller:latest", required: true },
      { src: `${GHCR}/kars-inference-router:${releaseVersion}`, target: "kars-inference-router:latest", required: true },
      { src: `${GHCR}/openclaw-sandbox:${releaseVersion}`, target: "openclaw-sandbox:latest", required: true },
      // Mesh images: GHCR publishes `kars-agentmesh-*`; the agentmesh
      // manifest references `agentmesh-*-agt:latest`, so re-tag on import.
      { src: `${GHCR}/kars-agentmesh-relay:${releaseVersion}`, target: "agentmesh-relay-agt:latest", required: true },
      { src: `${GHCR}/kars-agentmesh-registry:${releaseVersion}`, target: "agentmesh-registry-agt:latest", required: true },
    ];
    if (!options.skipRuntimeImages) {
      for (const rt of [
        "kars-runtime-openai-agents", "kars-runtime-maf-python", "kars-runtime-anthropic",
        "kars-runtime-langgraph", "kars-runtime-langgraph-ts", "kars-runtime-pydantic-ai",
        "kars-runtime-hermes",
      ]) {
        releaseImages.push({ src: `${GHCR}/${rt}:${releaseVersion}`, target: `${rt}:latest`, required: false });
      }
    }
    let releaseFailures = 0;
    for (const img of releaseImages) {
      stepper.update(`Importing ${img.target} from ${img.src}...`);
      try {
        await execa("az", [
          "acr", "import",
          "--name", acr,
          "--source", img.src,
          "--image", img.target,
          "--force",
        ], { stdio: "pipe" });
        stepper.detail("ok", img.target);
      } catch (e) {
        const msg = ((e as { message?: string }).message ?? "").split("\n")[0].slice(0, 90);
        if (img.required) {
          releaseFailures++;
          stepper.detail("skip", `${img.target} — import FAILED (${msg})`);
        } else {
          stepper.detail("skip", `${img.target} — import failed (${msg})`);
        }
      }
    }
    if (releaseFailures > 0) {
      throw new Error(
        `Failed to import ${releaseFailures} required release image(s) for '${releaseVersion}'. ` +
          `Verify the tag exists on GHCR (https://github.com/orgs/Azure/packages?repo_name=kars) ` +
          `and that 'az acr import' can reach ghcr.io.`,
      );
    }
    stepper.done(`Imported published release images (${releaseVersion}) into ACR`);
  } else if (options.build) {
    // Developer mode: build locally and push
    stepper.step("Building and pushing images...");
    stepper.update("Logging into ACR...");
    await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });

    const buildPush = async (dockerfile: string, tag: string, buildArgs: string[] = [], context?: string) => {
      stepper.update(`Building ${tag}...`);
      const args = [
        "build", "--platform", "linux/amd64",
        "--provenance=false", "--sbom=false",
        "-f", path.join(repoRoot, dockerfile),
        "-t", `${acrLoginServer}/${tag}`,
        ...buildArgs,
        context ? path.join(repoRoot, context) : repoRoot,
      ];
      await execa("docker", args, { stdio: "pipe" });
      // Push with retry — ACR tokens/connections can go stale after long builds
      for (let attempt = 1; attempt <= 3; attempt++) {
        try {
          stepper.update(`Pushing ${tag}${attempt > 1 ? ` (retry ${attempt}/3)` : ""}...`);
          if (attempt > 1) await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });
          await execa("docker", ["push", `${acrLoginServer}/${tag}`], { stdio: "pipe" });
          break;
        } catch (e: unknown) {
          if (attempt === 3) throw e;
          stepper.update(`Push ${tag} failed, retrying...`);
          await new Promise((r) => setTimeout(r, 5000));
        }
      }
    };

    // Rust images (controller + router) ship as COPY-only Dockerfiles
    // that expect a pre-compiled `bin/<arch>/<binary>` in the build
    // context. On a native linux/amd64 host we cargo-build + stage them
    // (fast). On any other host (Apple Silicon, Intel Mac, linux/arm64)
    // host cargo can't produce a linux/amd64 ELF, so we use the
    // *.multistage Dockerfile which compiles Rust INSIDE the (emulated)
    // amd64 docker build. Mirrors `kars push` / `kars dev`. Without this
    // `kars up --build` failed on a fresh non-Linux machine with
    // "COPY bin/amd64/kars-controller: no such file or directory".
    const canStageNatively =
      process.platform === "linux" && process.arch === "x64";
    const controllerDf = canStageNatively
      ? "controller/Dockerfile"
      : "controller/Dockerfile.multistage";
    const routerDf = canStageNatively
      ? "inference-router/Dockerfile"
      : "inference-router/Dockerfile.multistage";
    if (canStageNatively) {
      stepper.update("Compiling Rust binaries (controller + inference-router)...");
      await stageRustBinaries(
        repoRoot,
        ["kars-controller", "kars-inference-router"],
        "amd64",
      );
    }

    await buildPush(controllerDf, "kars-controller:latest");
    await buildPush(routerDf, "kars-inference-router:latest");

    // Build sandbox base if not already in ACR
    let baseExists = false;
    try {
      await execa("docker", ["image", "inspect", `${acrLoginServer}/kars-sandbox-base:latest`], { stdio: "pipe" });
      baseExists = true;
    } catch { /* not cached locally — need to build */ }
    if (!baseExists) {
      await buildPush(
        "sandbox-images/openclaw/Dockerfile.base",
        "kars-sandbox-base:latest",
        ["--build-arg", `OPENCLAW_CACHE_BUST=${Date.now()}`],
      );
    }

    await buildPush(
      "sandbox-images/openclaw/Dockerfile",
      "openclaw-sandbox:latest",
      ["--build-arg", `SANDBOX_BASE_IMAGE=${acrLoginServer}/kars-sandbox-base:latest`,
        "--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/kars-inference-router:latest`],
    );

    // Multi-runtime adapter images. Tags must match the controller's
    // DEFAULT_*_IMAGE constants in `reconciler/runtime.rs`. Skipped
    // when --skip-runtime-images is passed (faster first deploy).
    if (!options.skipRuntimeImages) {
      // The Python runtime Dockerfiles COPY runtimes/wheels/. ensureAgtWheels()
      // auto-clones the pinned AGT repo if missing and builds the wheels into
      // runtimes/wheels/. No-op when the cache stamp matches the current pin.
      stepper.update("Bootstrapping AGT toolkit + Python wheels...");
      const agtRepo = await ensureAgtRepo(undefined, repoRoot);
      await ensureAgtWheels(agtRepo, repoRoot);
      for (const rt of [
        { dir: "openai-agents", tag: "kars-runtime-openai-agents:latest" },
        { dir: "maf-python", tag: "kars-runtime-maf-python:latest" },
        { dir: "anthropic", tag: "kars-runtime-anthropic:latest" },
        { dir: "langgraph", tag: "kars-runtime-langgraph:latest" },
        { dir: "langgraph-ts", tag: "kars-runtime-langgraph-ts:latest" },
        { dir: "pydantic-ai", tag: "kars-runtime-pydantic-ai:latest" },
        { dir: "hermes", tag: "kars-runtime-hermes:latest" },
      ]) {
        await buildPush(`sandbox-images/${rt.dir}/Dockerfile`, rt.tag);
      }
    }

    // AgentMesh relay+registry images: kars does not build these (vendored
    // forks removed in Phase 5.2). Import the pre-built AGT-compatible images
    // from the public source ACR — the deploy/agentmesh-agt.yaml manifest
    // references them by tag.
    for (const tag of ["agentmesh-relay-agt:latest", "agentmesh-registry-agt:latest"]) {
      stepper.update(`Importing ${tag} from ${options.sourceAcr}...`);
      await execa("az", [
        "acr", "import",
        "--name", acr,
        "--source", `${options.sourceAcr}/${tag}`,
        "--image", tag,
        "--force",
      ], { stdio: "pipe" }).then(() => {
        stepper.detail("ok", tag);
      }).catch((e: { message?: string }) => {
        stepper.detail("skip", `${tag} — import failed (${(e.message ?? "").split("\n")[0].slice(0, 80)})`);
      });
    }

    stepper.done("Images built and pushed to ACR");
  } else {
    // Customer mode: import pre-built images from source ACR
    stepper.step("Importing images from source ACR...");
    const sourceAcr = options.sourceAcr;
    // `required` images must land or the cluster can't run; runtime adapters
    // are optional (a given source ACR may not host every runtime). Mesh
    // images MUST use the `-agt` suffix — that's the tag the
    // deploy/agentmesh-agt.yaml manifest references; importing them without it
    // (the old bug) left the relay/registry in ImagePullBackOff while the
    // deploy still reported success.
    const images: Array<{ source: string; target: string; required: boolean }> = [
      { source: `${sourceAcr}/kars-controller:latest`, target: "kars-controller:latest", required: true },
      { source: `${sourceAcr}/kars-inference-router:latest`, target: "kars-inference-router:latest", required: true },
      { source: `${sourceAcr}/openclaw-sandbox:latest`, target: "openclaw-sandbox:latest", required: true },
      { source: `${sourceAcr}/agentmesh-relay-agt:latest`, target: "agentmesh-relay-agt:latest", required: true },
      { source: `${sourceAcr}/agentmesh-registry-agt:latest`, target: "agentmesh-registry-agt:latest", required: true },
      // Multi-runtime adapter images. Failures here are non-fatal —
      // some source ACRs may not host every runtime.
      { source: `${sourceAcr}/kars-runtime-openai-agents:latest`, target: "kars-runtime-openai-agents:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-maf-python:latest`, target: "kars-runtime-maf-python:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-anthropic:latest`, target: "kars-runtime-anthropic:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-langgraph:latest`, target: "kars-runtime-langgraph:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-langgraph-ts:latest`, target: "kars-runtime-langgraph-ts:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-pydantic-ai:latest`, target: "kars-runtime-pydantic-ai:latest", required: false },
      { source: `${sourceAcr}/kars-runtime-hermes:latest`, target: "kars-runtime-hermes:latest", required: false },
    ];

    let customerFailures = 0;
    for (const img of images) {
      stepper.update(`Importing ${img.target}...`);
      try {
        await execa("az", [
          "acr", "import",
          "--name", acr,
          "--source", img.source,
          "--image", img.target,
          "--force",
        ], { stdio: "pipe" });
        stepper.detail("ok", img.target);
      } catch (e) {
        const msg = ((e as { message?: string }).message ?? "").split("\n")[0].slice(0, 90);
        if (img.required) {
          customerFailures++;
          stepper.detail("skip", `${img.target} — import FAILED (${msg})`);
        } else {
          stepper.detail("skip", `${img.target} — import failed (optional: ${msg})`);
        }
      }
    }
    if (customerFailures > 0) {
      throw new Error(
        `Failed to import ${customerFailures} required image(s) from ${sourceAcr}. ` +
          `Verify you can 'az acr import' from it (network + auth) and that it hosts the ` +
          `controller, inference-router, openclaw-sandbox, and agentmesh-*-agt images. ` +
          `Tip: 'kars up --release' imports the public GHCR images instead — no source-ACR access needed.`,
      );
    }

    stepper.done("Images available in ACR");
  }
  markPhaseDone("images", {}, resumeTopology);
}
