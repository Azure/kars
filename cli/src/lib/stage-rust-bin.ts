// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * stage-rust-bin — shared helper used by `kars dev` (docker + local-k8s)
 * and `kars push` (AKS).
 *
 * The CI build-once refactor turned every Rust runtime image into a
 * COPY-only Dockerfile that expects `bin/<arch>/<binary>` to already
 * exist. Every code path that runs `docker build` against one of those
 * Dockerfiles must first stage the binary via `cargo build --release`.
 *
 * This helper:
 *   1. Maps each Cargo workspace package to its built-binary name.
 *   2. Skips packages whose binary is already up-to-date in bin/<arch>/.
 *   3. Runs `cargo build --release -p <pkg>` for the rest in a single
 *      invocation (cargo dedupes shared deps).
 *   4. Copies the resulting binary into bin/<arch>/<binary>.
 *
 * Arch is callable-controlled: `kars dev` passes the host arch (so the
 * docker image runs natively on Apple Silicon), `kars push` always
 * passes "amd64" because AKS sandboxes are amd64.
 */

import { execa } from "execa";
import { existsSync, mkdirSync, copyFileSync, statSync } from "node:fs";
import * as path from "node:path";
import chalk from "chalk";

export type RustArch = "amd64" | "arm64";

/** Cargo package name → produced binary name. */
const PACKAGE_TO_BINARY: Record<string, string> = {
  "kars-controller": "kars-controller",
  "kars-inference-router": "kars-inference-router",
  "kars-a2a-gateway": "kars-a2a-gateway",
  "kars-conformance-runner": "kars-conformance-runner",
};

/**
 * Build the requested packages (if not already staged) and copy their
 * binaries into `${repoRoot}/bin/${arch}/`.
 *
 * Returns the list of binary paths that ended up staged.
 */
export async function stageRustBinaries(
  repoRoot: string,
  packages: string[],
  arch: RustArch,
  opts: { forceRebuild?: boolean } = {},
): Promise<string[]> {
  // Hard guard: cargo on the host produces a binary for the HOST OS.
  // On macOS that's a Mach-O binary which can't run inside a Linux
  // container — `exec format error` at pod start. The COPY-only
  // Dockerfile pattern only works when the host OS is Linux (CI).
  // Callers running on macOS / Windows must use the *.multistage
  // Dockerfile variants which compile rust inside the docker build.
  if (process.platform !== "linux") {
    throw new Error(
      `stage-rust-bin: cannot cross-build for linux/${arch} from ${process.platform}. ` +
      `Use the *.multistage Dockerfile variant instead (compiles rust inside docker).`,
    );
  }
  const binDir = path.join(repoRoot, "bin", arch);
  mkdirSync(binDir, { recursive: true });

  // Decide which packages actually need a (re)build.
  const toBuild: string[] = [];
  const cargoTargetDir = path.join(repoRoot, "target/release");
  const cargoTomlMtime = statSync(path.join(repoRoot, "Cargo.toml")).mtimeMs;

  for (const pkg of packages) {
    const binName = PACKAGE_TO_BINARY[pkg];
    if (!binName) {
      throw new Error(`Unknown Rust package: ${pkg}. Add it to PACKAGE_TO_BINARY in cli/src/lib/stage-rust-bin.ts.`);
    }
    const stagedPath = path.join(binDir, binName);
    const cargoOutput = path.join(cargoTargetDir, binName);

    if (opts.forceRebuild) {
      toBuild.push(pkg);
      continue;
    }
    if (!existsSync(stagedPath)) {
      toBuild.push(pkg);
      continue;
    }
    // Up-to-date check: staged binary newer than Cargo.toml and cargo
    // output. If cargo output exists and is newer than staged, the
    // user has built since we last copied — refresh the staged copy
    // without re-running cargo.
    const stagedMtime = statSync(stagedPath).mtimeMs;
    if (stagedMtime < cargoTomlMtime) {
      toBuild.push(pkg);
      continue;
    }
    if (existsSync(cargoOutput) && statSync(cargoOutput).mtimeMs > stagedMtime) {
      copyFileSync(cargoOutput, stagedPath);
    }
  }

  if (toBuild.length > 0) {
    console.log(
      chalk.dim(
        `  cargo build --release ${toBuild.map((p) => `-p ${p}`).join(" ")}`,
      ),
    );
    const args = ["build", "--release"];
    for (const pkg of toBuild) args.push("-p", pkg);
    await execa("cargo", args, { stdio: "inherit", cwd: repoRoot });

    for (const pkg of toBuild) {
      const binName = PACKAGE_TO_BINARY[pkg];
      copyFileSync(
        path.join(cargoTargetDir, binName),
        path.join(binDir, binName),
      );
    }
  }

  return packages.map((p) => path.join(binDir, PACKAGE_TO_BINARY[p]));
}

/**
 * Pick the right `bin/<arch>/` slot based on a docker `--platform`
 * argument. Anything ending in `/arm64` → "arm64", everything else
 * → "amd64".
 */
export function archForDockerPlatform(platform: string): RustArch {
  return platform.endsWith("/arm64") ? "arm64" : "amd64";
}
