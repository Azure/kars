// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Auto-clone the pinned AGT toolkit fork into ~/agent-governance-toolkit
 * (or `$KARS_AGT_REPO` if set) so `kars dev` / `kars up` / `kars push`
 * work out-of-the-box on a fresh machine without the user having to know
 * about the AGT-main-vs-released schema gap.
 *
 * The pin lives in `vendor/agt/pin.json` (single source of truth, also
 * referenced from `Cargo.toml [patch.crates-io]` and the `file:` deps
 * in `mesh-plugin/package.json` and `runtimes/openclaw/package.json`).
 * When upstream AGT cuts a release containing PR
 * https://github.com/microsoft/agent-governance-toolkit/pull/2772, the
 * pin file is deleted and the CLI no longer auto-clones — `kars push`
 * then refuses to build relay/registry from source because the
 * published `ghcr.io/microsoft/agentmesh/{relay,registry}:X.Y.Z`
 * images are usable directly.
 *
 * Why we ship a fork pin instead of just published packages right now:
 *
 *   1. AGT v4.0.0 server-side has hard POP enforcement (server PR
 *      #2533 + #2632) that the published `npm
 *      @microsoft/agent-governance-sdk@4.0.0` does NOT yet sign.
 *      Mismatch is documented; #2772 is the SDK-side fix.
 *
 *   2. Our kars-built relay/registry images (`kars push --only
 *      relay/registry`) are now built from the same SHA as the SDK
 *      tarball under `vendor/agt/`, so the wire protocol is
 *      consistent edge-to-edge.
 *
 *   3. Setting `KARS_AGT_REPO=/path/to/your/clone` still wins — the
 *      auto-clone is purely a fresh-machine convenience.
 */

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execa } from "execa";

interface AgtPin {
  url: string;
  branch: string;
  sha: string;
  shortSha: string;
}

const DEFAULT_AGT_REPO = path.join(os.homedir(), "agent-governance-toolkit");

/**
 * Return the path to the AGT clone, auto-cloning the pinned fork
 * SHA when it doesn't exist. Honors `KARS_AGT_REPO` and an explicit
 * caller-supplied `agtRepo` argument (--agt-repo flag) over auto-clone.
 *
 * Returns the path; throws if auto-clone fails. The caller can `try`
 * around it and fall back to "skip mesh image build" semantics if
 * desired.
 */
export async function ensureAgtRepo(
  callerSupplied?: string,
  repoRoot?: string,
): Promise<string> {
  const agtRepo =
    callerSupplied || process.env.KARS_AGT_REPO || DEFAULT_AGT_REPO;
  if (fs.existsSync(path.join(agtRepo, ".git"))) {
    return agtRepo;
  }

  const root = repoRoot || process.cwd();
  const pinPath = path.join(root, "vendor", "agt", "pin.json");
  if (!fs.existsSync(pinPath)) {
    throw new Error(
      `vendor/agt/pin.json not found at ${pinPath}; can't auto-clone AGT.\n` +
        `  Either run from the kars repo root, or set KARS_AGT_REPO to an ` +
        `existing AGT toolkit checkout.`,
    );
  }
  const pin = JSON.parse(fs.readFileSync(pinPath, "utf-8")) as AgtPin;

  process.stderr.write(
    `[kars] AGT clone missing at ${agtRepo} — auto-cloning ${pin.url}@${pin.shortSha}\n`,
  );
  // Shallow clone of the pinned SHA. Use `--filter=tree:0` so the
  // clone is < 50 MB even though we only need the relay/registry
  // Dockerfile + the TS SDK source.
  await execa(
    "git",
    [
      "clone",
      "--branch",
      pin.branch,
      "--depth",
      "1",
      "--filter=tree:0",
      pin.url,
      agtRepo,
    ],
    { stdio: "inherit" },
  );
  // The SDK build step (`cd agent-governance-typescript && npm run build`)
  // needs the dependency tree, but it isn't required for the relay/
  // registry Dockerfile build. The vendored `.tgz` under `vendor/agt/`
  // is already pre-built so the sandbox image doesn't have to invoke
  // npm against the AGT clone at all. We only ensure the path
  // exists here.
  return agtRepo;
}

/**
 * Build the AGT Python wheels into `runtimes/wheels/` so the seven
 * Python sandbox-image Dockerfiles (anthropic, hermes, langgraph,
 * maf-python, openai-agents, pydantic-ai, plus any future Python
 * runtime) can `COPY runtimes/wheels/` into their build context.
 *
 * Why this exists: `runtimes/wheels/` is `.gitignored` and the
 * Dockerfiles assume the wheels already exist on the host. Without
 * an auto-trigger, a fresh `kars push --only runtime-hermes` (or any
 * Python runtime) fails with `failed to compute cache key: "/runtimes
 * /wheels" not found`. Calling this from `kars push`, `kars up`, and
 * `kars dev` BEFORE the docker build step closes that out-of-the-box
 * gap.
 *
 * Caching: skips the build if `runtimes/wheels/.agt-sha` matches the
 * current pin SHA AND at least one `.whl` is present. Delete the
 * sentinel or pass `force: true` to rebuild.
 *
 * @param agtRepo  Path returned by {@link ensureAgtRepo}.
 * @param repoRoot kars repo root (for `runtimes/wheels/` path).
 * @param force    When true, rebuild even if cache is valid.
 */
export async function ensureAgtWheels(
  agtRepo: string,
  repoRoot: string,
  force = false,
): Promise<void> {
  const wheelDir = path.join(repoRoot, "runtimes", "wheels");
  const buildScript = path.join(repoRoot, "runtimes", "build-agt-wheels.sh");
  const pinPath = path.join(repoRoot, "vendor", "agt", "pin.json");
  const cacheStamp = path.join(wheelDir, ".agt-sha");

  if (!fs.existsSync(buildScript)) {
    throw new Error(
      `runtimes/build-agt-wheels.sh not found at ${buildScript}; ` +
        `repo layout unexpected.`,
    );
  }

  // Cache check: skip if the pin SHA matches what produced the
  // current wheels.
  const pinSha = fs.existsSync(pinPath)
    ? (JSON.parse(fs.readFileSync(pinPath, "utf-8")) as AgtPin).sha
    : "no-pin";
  if (!force && fs.existsSync(cacheStamp) && fs.existsSync(wheelDir)) {
    const stamp = fs.readFileSync(cacheStamp, "utf-8").trim();
    const hasWheels = fs
      .readdirSync(wheelDir)
      .some((f) => f.endsWith(".whl"));
    if (stamp === pinSha && hasWheels) {
      return;
    }
  }

  const agtPython = path.join(agtRepo, "agent-governance-python");
  if (!fs.existsSync(path.join(agtPython, "agent-mesh", "pyproject.toml"))) {
    throw new Error(
      `AGT Python tree not found at ${agtPython}. Re-run after ` +
        `ensureAgtRepo(), or check the agent-governance-toolkit clone.`,
    );
  }

  process.stderr.write(
    `[kars] Building AGT Python wheels (pin: ${pinSha.slice(0, 8)})...\n`,
  );
  fs.mkdirSync(wheelDir, { recursive: true });
  await execa("bash", [buildScript], {
    cwd: repoRoot,
    env: { ...process.env, AGT_PYTHON_DIR: agtPython },
    stdio: "inherit",
  });
  fs.writeFileSync(cacheStamp, pinSha + "\n");
}

export { DEFAULT_AGT_REPO };
