// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Auto-clone the pinned official AGT toolkit into ~/agent-governance-toolkit
 * (or `$KARS_AGT_REPO` if set) so `kars dev` / `kars up` / `kars push`
 * work out-of-the-box on a fresh machine without the user having to know
 * about the AGT-main-vs-released schema gap.
 *
 * The pin lives in `vendor/agt/pin.json` (single source of truth, also
 * referenced from `Cargo.toml [patch.crates-io]` and the `file:` deps
 * in `mesh-plugin/package.json` and `runtimes/openclaw/package.json`).
 * The selected AGT 5 revision binds SDKs, native ACS and mesh servers
 * together. Matching published artifacts must pass the same installed
 * acceptance before replacing that source pin. An explicit KARS_AGT_REPO
 * remains supported, but must be an exact clean checkout.
 */

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { createHash } from "node:crypto";
import { execa } from "execa";

interface AgtPin {
  url: string;
  branch: string;
  sha: string;
  shortSha: string;
}

const DEFAULT_AGT_REPO = path.join(os.homedir(), "agent-governance-toolkit");
export const AGT_MESH_DOCKERFILE = "sandbox-images/agentmesh/Dockerfile";
export const AGT_CORE_MANIFEST = "agent-governance-python/agent-governance-toolkit-core/pyproject.toml";
const AGT_WHEEL_PACKAGES = [
  "agent_governance_toolkit_core",
  "agent_governance_toolkit_cli",
  "a2a_agentmesh",
];
const AGT_WHEEL_BUILD_INPUTS = [
  "build-agt-wheels.sh", "Dockerfile.agt-native",
  "build-native-agt-wheel.py", "install-agt-wheels.py",
];

function hasCompleteWheelSet(directory: string): boolean {
  if (!fs.existsSync(directory)) return false;
  const entries = fs.readdirSync(directory, { withFileTypes: true })
    .filter(entry => entry.name.endsWith(".whl"));
  if (entries.some(entry => !entry.isFile())) return false;
  const wheels = entries.map(entry => entry.name);
  const pureComplete = wheels.length === AGT_WHEEL_PACKAGES.length && AGT_WHEEL_PACKAGES.every(
    name => wheels.includes(`${name}-5.0.0-py3-none-any.whl`),
  );
  if (!pureComplete) return false;
  for (const [architecture, platform, machine] of [
    ["amd64", "x86_64", 62],
    ["arm64", "aarch64", 183],
  ] as const) {
    const native = path.join(directory, architecture);
    if (!fs.existsSync(native) || !fs.lstatSync(native).isDirectory()) return false;
    const wheel = `agent_control_specification-0.4.0b0-cp311-abi3-linux_${platform}.whl`;
    const files = fs.readdirSync(native, { withFileTypes: true });
    if (files.length !== 2 || files.some(file => !file.isFile()) ||
        !files.some(file => file.name === wheel) || !files.some(file => file.name === "build.json")) return false;
    const receipt: unknown = JSON.parse(fs.readFileSync(path.join(native, "build.json"), "utf8"));
    if (typeof receipt !== "object" || receipt === null ||
        !("wheel" in receipt) || receipt.wheel !== wheel ||
        !("elf_machine" in receipt) || receipt.elf_machine !== machine ||
        !("target" in receipt) || receipt.target !== `${platform}-unknown-linux-gnu` ||
        !("rust" in receipt) || receipt.rust !== "1.95.0" ||
        !("maturin" in receipt) || receipt.maturin !== "1.8.7" ||
        !("sha256" in receipt) || receipt.sha256 !== createHash("sha256")
          .update(fs.readFileSync(path.join(native, wheel))).digest("hex")) return false;
  }
  return true;
}

/**
 * Return the path to the AGT clone, auto-cloning the pinned
 * SHA when it doesn't exist. Honors `KARS_AGT_REPO` and an explicit
 * caller-supplied `agtRepo` argument (--agt-repo flag) over auto-clone.
 *
 * Existing checkouts must match the pin and have no local changes. Never
 * reset, switch, or otherwise modify a caller's existing checkout.
 */
export async function ensureAgtRepo(
  callerSupplied?: string,
  repoRoot?: string,
): Promise<string> {
  const agtRepo =
    callerSupplied || process.env.KARS_AGT_REPO || DEFAULT_AGT_REPO;
  const root = repoRoot || process.cwd();
  const pinPath = path.join(root, "vendor", "agt", "pin.json");
  if (!fs.existsSync(pinPath)) {
    throw new Error(
      `vendor/agt/pin.json not found at ${pinPath}; can't auto-clone AGT.\n` +
        `  Run from the kars repo root or provide its asset directory.`,
    );
  }
  const pin: unknown = JSON.parse(fs.readFileSync(pinPath, "utf-8"));
  if (
    typeof pin !== "object" || pin === null ||
    !("sha" in pin) || typeof pin.sha !== "string" || !/^[a-f0-9]{40}$/.test(pin.sha) ||
    !("url" in pin) || typeof pin.url !== "string" || !pin.url.trim()
  ) {
    throw new Error(`Invalid AGT source pin at ${pinPath}; expected a URL and full commit SHA.`);
  }
  const { sha, url } = pin;

  async function verifyCheckout(directory: string): Promise<void> {
    const { stdout: top } = await execa("git", ["rev-parse", "--show-toplevel"], { cwd: directory });
    if (fs.realpathSync(top.trim()) !== fs.realpathSync(directory)) {
      throw new Error(`AGT path ${directory} is not the root of its checkout.`);
    }
    const { stdout: head } = await execa("git", ["rev-parse", "HEAD"], { cwd: directory });
    if (head.trim() !== sha) {
      throw new Error(
        `AGT checkout ${directory} is at ${head.trim()}, but Kars requires ${sha}. ` +
        `Use a separate checkout at the required revision; the existing checkout was not changed.`,
      );
    }
    const { stdout: status } = await execa("git", ["status", "--porcelain", "--untracked-files=normal"], {
      cwd: directory,
    });
    if (status.trim()) {
      throw new Error(`AGT checkout ${directory} has local changes; use a clean pinned checkout. No files were changed.`);
    }
  }

  if (fs.lstatSync(agtRepo, { throwIfNoEntry: false })) {
    await verifyCheckout(agtRepo);
    return agtRepo;
  }

  process.stderr.write(
    `[kars] AGT clone missing at ${agtRepo} — fetching ${url}@${sha.slice(0, 8)}\n`,
  );
  const parent = path.dirname(path.resolve(agtRepo));
  fs.mkdirSync(parent, { recursive: true });
  const staging = fs.mkdtempSync(path.join(parent, ".kars-agt-"));
  try {
    await execa("git", ["init", "--quiet", staging]);
    await execa("git", ["remote", "add", "origin", url], { cwd: staging });
    await execa("git", ["fetch", "--depth", "1", "origin", sha], { cwd: staging, stdio: "inherit" });
    await execa("git", ["checkout", "--detach", "--quiet", sha], { cwd: staging });
    await verifyCheckout(staging);
    if (fs.lstatSync(agtRepo, { throwIfNoEntry: false })) {
      throw new Error(`AGT destination ${agtRepo} appeared during preparation; it was not changed.`);
    }
    fs.renameSync(staging, agtRepo);
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
  return agtRepo;
}

/**
 * Build the AGT Python wheels into `runtimes/wheels/` so the six
 * Python sandbox-image Dockerfiles (anthropic, hermes, langgraph,
 * maf-python, openai-agents, pydantic-ai) can `COPY runtimes/wheels/`
 * into their build context.
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
 * current pin SHA and build-input digest AND all three AGT 5 owner wheels
 * plus both checksum-verified native ACS wheels are present. Delete the
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

  await ensureAgtRepo(agtRepo, repoRoot);
  const pinSha = (JSON.parse(fs.readFileSync(pinPath, "utf-8")) as AgtPin).sha;
  const recipe = createHash("sha256");
  for (const input of AGT_WHEEL_BUILD_INPUTS) {
    recipe.update(input).update("\0")
      .update(fs.readFileSync(path.join(repoRoot, "runtimes", input))).update("\0");
  }
  const stampValue = `${pinSha}\n${recipe.digest("hex")}`;
  if (!force && fs.existsSync(cacheStamp) && fs.existsSync(wheelDir)) {
    const stamp = fs.readFileSync(cacheStamp, "utf-8").trim();
    if (stamp === stampValue && hasCompleteWheelSet(wheelDir)) {
      return;
    }
  }

  const agtPython = path.join(agtRepo, "agent-governance-python");
  if (!fs.existsSync(path.join(agtPython, "agent-governance-toolkit-core", "pyproject.toml")) ||
      !fs.existsSync(path.join(agtRepo, "policy-engine", "sdk", "python", "pyproject.toml"))) {
    throw new Error(
      `AGT Python tree not found at ${agtPython}. Re-run after ` +
        `ensureAgtRepo(), or check the agent-governance-toolkit clone.`,
    );
  }

  process.stderr.write(
    `[kars] Building AGT Python wheels (pin: ${pinSha.slice(0, 8)})...\n`,
  );
  fs.mkdirSync(wheelDir, { recursive: true });
  fs.rmSync(cacheStamp, { force: true });
  await execa("bash", [buildScript], {
    cwd: repoRoot,
    env: { ...process.env, AGT_PYTHON_DIR: agtPython },
    stdio: "inherit",
  });
  if (!hasCompleteWheelSet(wheelDir)) {
    throw new Error(
      `AGT wheel build did not produce exactly one wheel for each required distribution: ` +
      AGT_WHEEL_PACKAGES.join(", ") + ", and validated amd64/arm64 native ACS wheels",
    );
  }
  fs.writeFileSync(cacheStamp, stampValue + "\n");
}

export { DEFAULT_AGT_REPO };
