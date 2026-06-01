// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `kars dev --target local-k8s` — runs a sandbox in a local kind
 * cluster instead of plain Docker. Pairs with a Headlamp dashboard
 * (added in a later phase) so developers get a real K8s view of their
 * agents without needing AKS.
 *
 * Phase 1: skeleton only.
 *   - Detects/creates a kind cluster (default name: kars-dev).
 *   - Loads the locally-built kars images into kind.
 *   - Helm-installs the existing chart in a local-friendly way.
 *   - Prints a `kubectl exec` recipe.
 *
 * Later phases add: values-local-dev overlay, fake-router, Headlamp,
 * kars Headlamp plugin, hot-reload, and lifecycle commands.
 */

import { execa } from "execa";
import chalk from "chalk";
import * as path from "node:path";
import * as os from "node:os";
import { existsSync, writeFileSync, mkdtempSync, rmSync, readFileSync } from "node:fs";
import { Stepper } from "../../stepper.js";
import { loadConfig, getSecret, type KarsConfig } from "../../config.js";
import { loadAgtProfile } from "../../refs.js";
import { stageRustBinaries, type RustArch } from "../../lib/stage-rust-bin.js";
import { stageMeshPlugin } from "../../lib/stage-mesh-plugin.js";

export interface LocalK8sOptions {
  /** Sandbox / agent name. Reused as Helm release name suffix. */
  name: string;
  /** Kind cluster name. */
  clusterName: string;
  /** Sandbox image tag (must be locally available before this runs). */
  image: string;
  /** When true, the cluster is destroyed when the user Ctrl+C's. */
  ephemeral: boolean;
  /** Skip image build assumption — caller already built/loaded. */
  noBuild: boolean;
  /**
   * Optional comma-separated channel list (e.g. "telegram", "slack.dev").
   * Same syntax docker-mode uses; we resolve each entry to a `<channel>-token[.variant]`
   * secret, materialise it into the per-sandbox `<name>-credentials` secret,
   * and the controller mounts it via `envFrom` (TELEGRAM_BOT_TOKEN, etc).
   */
  channels?: string;
  /**
   * If true, force-rebuild any sandbox/router/controller image whose
   * arch doesn't match the host (e.g. cached linux/amd64 from a prior
   * `kars push` on an Apple Silicon laptop) — or that the user
   * explicitly asked to rebuild via the `--build` flag in the
   * common first-run prompt.
   */
  forceRebuild?: boolean;
  /**
   * Mesh stack to deploy in the local kind cluster. Only 'agt' is supported
   * after Phase 5.2 (vendored Rust relay/registry were removed). Kept as a
   * field for backward-compatible scripts and to preserve the multi-provider
   * framework for future implementations.
   */
  meshProvider?: "agt";
  /**
   * Path to the agent-governance-toolkit checkout, used to build AGT relay
   * + registry images when meshProvider==="agt". Defaults to
   * $KARS_AGT_REPO or the same fallback dev.ts uses.
   */
  agtRepo?: string;
  /**
   * Skip mesh-stack deployment entirely. The controller will start but
   * sandboxes won't be able to reach relay/registry. Useful for pure
   * controller smoke tests on hardware without enough RAM for the full
   * stack.
   */
  noMesh?: boolean;
  /**
   * External AgentMesh registry URL (e.g. `https://registry.example.com`
   * or a port-forwarded `http://localhost:18080` from `kars mesh
   * promote --port-forward`). When set, the local-k8s flow skips the
   * in-kind relay+registry deployment and the controller / sandbox env
   * is wired to talk to this URL instead.
   */
  globalRegistry?: string;
}

/**
 * Container runtime backing kind. kind ≥0.20 supports docker (default),
 * podman, and nerdctl via the `KIND_EXPERIMENTAL_PROVIDER` env var. The
 * runtime affects three things:
 *  1. The `KIND_EXPERIMENTAL_PROVIDER` env var kind reads at startup.
 *  2. The image-load fallback path: docker has a shared daemon so we
 *     pipe `docker save` straight into the node's `ctr import`. Podman
 *     and nerdctl behave the same way (`<runtime> save | <runtime>
 *     exec -i node ctr import -`) but the binary differs.
 *  3. The `<runtime> exec` invocation used to introspect node state and
 *     pipe images.
 */
export type ContainerRuntime = "docker" | "podman" | "nerdctl";

/**
 * Outcome of the optional first-run "Add GitHub MCP?" prompt
 * (Slice 4d.4.1 — outbound static-bearer auth for the official
 * `https://api.githubcopilot.com/mcp` server).
 *
 * The mechanism is generic — the controller CRD field `bearerFromEnv`
 * just names a router-process env var. We standardise on
 * `COPILOT_GITHUB_TOKEN` here because:
 *   - github-copilot already wires it (router `copilot_auth.rs` uses it
 *     for inference auth too), so the MCP wiring is a no-op env-side;
 *   - github-models can safely reuse the same PAT under that env name;
 *   - foundry needs us to provision a fresh GitHub token (via `gh auth
 *     token` or a paste prompt) into a dedicated Secret which the
 *     controller then projects under the same env name.
 *
 * `tokenSecretName` is set only for the foundry path (where we provision
 * a brand-new Secret); copilot/models reuse the existing `kars-dev-creds`
 * Secret's api-key. `tokenInline` is set when the user pastes a token
 * inline (foundry path with no `gh` CLI available).
 */
interface GithubMcpDecision {
  enabled: boolean;
  /** Env var name the router process should read. Always `COPILOT_GITHUB_TOKEN`. */
  envVarName: string;
  /** For foundry path only: name of a Secret to mount the token under. */
  tokenSecretName?: string;
  /** For foundry path only: token value to write into the Secret. */
  tokenInline?: string;
}

/**
 * Interactive prompt: should this dev session enable the upstream
 * GitHub MCP server? Tailored per provider so the UX matches the
 * user's mental model (copilot: "you already have a token"; foundry:
 * "we need a fresh GitHub token").
 *
 * Non-interactive (no TTY) or `KARS_MCP_GITHUB=skip` skips the
 * whole prompt — useful for CI / regression scripts.
 */
async function promptForGithubMcp(
  creds: KarsConfig,
): Promise<GithubMcpDecision> {
  const envOverride = (process.env.KARS_MCP_GITHUB ?? "").toLowerCase();
  if (envOverride === "skip" || envOverride === "no" || envOverride === "false") {
    return { enabled: false, envVarName: "COPILOT_GITHUB_TOKEN" };
  }
  if (envOverride === "yes" || envOverride === "true" || envOverride === "on") {
    // Allowed shortcut only for copilot/models (we already have a token).
    if (creds.provider === "github-copilot" || creds.provider === "github-models") {
      return { enabled: true, envVarName: "COPILOT_GITHUB_TOKEN" };
    }
  }
  if (!process.stdin.isTTY) {
    return { enabled: false, envVarName: "COPILOT_GITHUB_TOKEN" };
  }

  const { default: inquirer } = await import("inquirer");

  if (creds.provider === "github-copilot") {
    const { reuse } = await inquirer.prompt([
      {
        type: "confirm",
        name: "reuse",
        message:
          "Also use your GitHub Copilot token for the GitHub MCP server (api.githubcopilot.com/mcp)?",
        default: true,
      },
    ]);
    return { enabled: !!reuse, envVarName: "COPILOT_GITHUB_TOKEN" };
  }

  if (creds.provider === "github-models") {
    const { reuse } = await inquirer.prompt([
      {
        type: "confirm",
        name: "reuse",
        message:
          "Reuse your GitHub PAT for the GitHub MCP server (api.githubcopilot.com/mcp)?\n" +
          "  The PAT must have the same scopes you want the agent to be able to read.",
        default: true,
      },
    ]);
    return { enabled: !!reuse, envVarName: "COPILOT_GITHUB_TOKEN" };
  }

  // Foundry path: need a fresh GitHub token. Try `gh auth token` first,
  // fall back to a paste prompt.
  const { enable } = await inquirer.prompt([
    {
      type: "confirm",
      name: "enable",
      message:
        "Enable the GitHub MCP server (api.githubcopilot.com/mcp) for this sandbox?\n" +
        "  Needs a GitHub token; we'll try `gh auth token` first.",
      default: false,
    },
  ]);
  if (!enable) {
    return { enabled: false, envVarName: "COPILOT_GITHUB_TOKEN" };
  }

  let token: string | undefined;
  try {
    const { stdout } = await execa("gh", ["auth", "token"], { stdio: ["ignore", "pipe", "ignore"] });
    const t = stdout.trim();
    if (t.length > 0) {
      token = t;
      console.log(chalk.dim("  ✓ obtained token via `gh auth token`"));
    }
  } catch {
    // gh not installed or not logged in — fall through to paste prompt.
  }

  if (!token) {
    const { pasted } = await inquirer.prompt([
      {
        type: "password",
        name: "pasted",
        mask: "*",
        message:
          "Paste a GitHub token (PAT or OAuth) with the scopes you want the MCP to use:",
        validate: (v: string) =>
          v.trim().length > 0 ? true : "token cannot be empty",
      },
    ]);
    token = (pasted as string).trim();
  }

  return {
    enabled: true,
    envVarName: "COPILOT_GITHUB_TOKEN",
    tokenSecretName: "github-mcp-token",
    tokenInline: token,
  };
}

interface Tooling {
  kind: string;
  kubectl: string;
  helm: string;
  runtime: string;
  runtimeName: ContainerRuntime;
  /** Env injected into every kind/runtime call. */
  env: NodeJS.ProcessEnv;
}

async function which(bin: string): Promise<string> {
  try {
    const { stdout } = await execa("which", [bin]);
    return stdout.trim();
  } catch {
    throw new Error(
      `${bin} not found on PATH. Install it (https://kind.sigs.k8s.io/, https://helm.sh/) and retry.`,
    );
  }
}

async function whichOptional(bin: string): Promise<string | undefined> {
  try {
    const { stdout } = await execa("which", [bin]);
    return stdout.trim();
  } catch {
    return undefined;
  }
}

const RUNTIME_PRIORITY: ContainerRuntime[] = ["docker", "podman", "nerdctl"];

async function detectRuntime(): Promise<{ name: ContainerRuntime; path: string }> {
  // Honour an explicit override so power users can force a specific
  // runtime even when several are installed.
  const override = process.env.KARS_DEV_RUNTIME?.toLowerCase();
  if (
    override === "docker" ||
    override === "podman" ||
    override === "nerdctl"
  ) {
    const p = await whichOptional(override);
    if (!p) {
      throw new Error(
        `KARS_DEV_RUNTIME=${override} but the '${override}' binary is not on PATH.`,
      );
    }
    return { name: override, path: p };
  }

  // Prefer docker → podman → nerdctl. Docker has the most CI mileage and
  // matches every existing dev's setup; podman/nerdctl get picked up
  // automatically only when docker is absent.
  for (const candidate of RUNTIME_PRIORITY) {
    const p = await whichOptional(candidate);
    if (p) return { name: candidate, path: p };
  }

  throw new Error(
    "No container runtime found on PATH. Install Docker Desktop, colima, " +
      "podman (with `podman machine` on macOS), or nerdctl, then retry. " +
      "Set KARS_DEV_RUNTIME=docker|podman|nerdctl to override " +
      "autodetection.",
  );
}

const MIN_KIND_MAJOR = 0;
const MIN_KIND_MINOR = 20;

async function ensureKindVersion(kindBin: string): Promise<void> {
  let raw: string;
  try {
    const { stdout } = await execa(kindBin, ["--version"]);
    raw = stdout.trim();
  } catch (err) {
    throw new Error(
      `Failed to run \`${kindBin} --version\`: ${(err as Error).message}. ` +
        `kars needs kind v${MIN_KIND_MAJOR}.${MIN_KIND_MINOR}+.`,
    );
  }
  const m = raw.match(/v?(\d+)\.(\d+)\.(\d+)/);
  if (!m) {
    throw new Error(
      `Could not parse kind version from output: "${raw}". ` +
        `kars needs kind v${MIN_KIND_MAJOR}.${MIN_KIND_MINOR}+.`,
    );
  }
  const major = Number(m[1]);
  const minor = Number(m[2]);
  const tooOld =
    major < MIN_KIND_MAJOR ||
    (major === MIN_KIND_MAJOR && minor < MIN_KIND_MINOR);
  if (tooOld) {
    throw new Error(
      `kind v${major}.${minor}.${m[3]} is too old. kars needs ` +
        `v${MIN_KIND_MAJOR}.${MIN_KIND_MINOR}+ (the post-init untaint ` +
        `step for single-node control-plane clusters was introduced in ` +
        `v0.20). Upgrade with \`brew upgrade kind\` or ` +
        `\`go install sigs.k8s.io/kind@latest\`.`,
    );
  }
}

async function ensureTooling(): Promise<Tooling> {
  // Resolved up front so we fail with one actionable error per missing
  // dependency, instead of an opaque ENOENT mid-bringup.
  const [kind, kubectl, helm, runtime] = await Promise.all([
    which("kind"),
    which("kubectl"),
    which("helm"),
    detectRuntime(),
  ]);
  await ensureKindVersion(kind);
  // kind needs KIND_EXPERIMENTAL_PROVIDER=podman|nerdctl to talk to
  // anything other than docker; for docker the var must be unset (or
  // empty) so kind uses its default.
  const env: NodeJS.ProcessEnv = { ...process.env };
  if (runtime.name === "docker") {
    delete env.KIND_EXPERIMENTAL_PROVIDER;
  } else {
    env.KIND_EXPERIMENTAL_PROVIDER = runtime.name;
  }
  return {
    kind,
    kubectl,
    helm,
    runtime: runtime.path,
    runtimeName: runtime.name,
    env,
  };
}

/**
 * Public helper used by `kars dev down` so it can issue
 * `kind delete cluster` against a cluster created under podman or
 * nerdctl. Returns a copy of `process.env` with
 * `KIND_EXPERIMENTAL_PROVIDER` set/cleared based on what's installed.
 * Falls back to `process.env` (i.e. lets kind default to docker) if
 * no runtime is installed — `dev down` should be best-effort.
 */
export async function detectRuntimeEnv(): Promise<NodeJS.ProcessEnv> {
  try {
    const r = await detectRuntime();
    const env: NodeJS.ProcessEnv = { ...process.env };
    if (r.name === "docker") {
      delete env.KIND_EXPERIMENTAL_PROVIDER;
    } else {
      env.KIND_EXPERIMENTAL_PROVIDER = r.name;
    }
    return env;
  } catch {
    return process.env;
  }
}

async function clusterExists(
  kind: string,
  name: string,
  env: NodeJS.ProcessEnv,
): Promise<boolean> {
  const { stdout } = await execa(kind, ["get", "clusters"], { env });
  return stdout.split(/\r?\n/).map((s) => s.trim()).includes(name);
}

async function ensureCluster(
  kind: string,
  name: string,
  env: NodeJS.ProcessEnv,
): Promise<void> {
  if (await clusterExists(kind, name, env)) return;
  // Kind v0.20+ on single-node clusters automatically removes the
  // control-plane NoSchedule taint as a post-init step (it runs
  // `kubectl taint nodes --all node-role.kubernetes.io/control-plane-`
  // inside the node container). So we don't need an inline kubeadm patch
  // — and using one (e.g. InitConfiguration with `taints: []`) actually
  // breaks creation because kind's untaint step then fails with
  // "taint not found" and aborts.
  await execa(kind, ["create", "cluster", "--name", name], {
    stdio: ["pipe", "inherit", "inherit"],
    env,
  });
}

async function loadImageIntoKind(
  kind: string,
  runtime: string,
  clusterName: string,
  image: string,
  env: NodeJS.ProcessEnv,
): Promise<void> {
  // `kind load docker-image` has a known issue where it can silently fail
  // to surface an image into the node's containerd (the import succeeds at
  // the kind layer but `crictl images` doesn't show it — observed on
  // multiple OS/arch combos and tracked in kind#3795). We use it as the
  // primary path, then verify by piping a `<runtime> save` straight into
  // the node's `ctr` as a fallback. The fallback is idempotent.
  //
  // The kind subcommand is named `load docker-image` regardless of the
  // backing runtime — kind reuses the docker terminology even when
  // talking to podman or nerdctl. The save/exec pipe below uses the
  // detected runtime binary so it works under all three.
  try {
    await execa(
      kind,
      ["load", "docker-image", image, "--name", clusterName],
      { stdio: "inherit", env },
    );
  } catch {
    // fall through to the ctr import path
  }

  // Verify the image is on the node; if not, push it via ctr.
  const node = `${clusterName}-control-plane`;
  const present = await execa(runtime, [
    "exec",
    node,
    "crictl",
    "images",
    "-q",
    image,
  ])
    .then((r) => r.stdout.trim().length > 0)
    .catch(() => false);

  if (present) return;

  const save = execa(runtime, ["save", image]);
  const importProc = execa(
    runtime,
    ["exec", "-i", node, "ctr", "-n=k8s.io", "images", "import", "-"],
    { stdio: ["pipe", "inherit", "inherit"] },
  );
  if (save.stdout && importProc.stdin) save.stdout.pipe(importProc.stdin);
  await Promise.all([save, importProc]);
}

async function localImageExists(runtime: string, image: string): Promise<boolean> {
  try {
    await execa(runtime, ["image", "inspect", image]);
    return true;
  } catch {
    return false;
  }
}

/**
 * Returns the OCI architecture of a local image (e.g. "amd64", "arm64"),
 * or null if the image isn't present.
 */
async function imageArch(runtime: string, image: string): Promise<string | null> {
  try {
    const { stdout } = await execa(runtime, [
      "image", "inspect", image, "--format", "{{.Architecture}}",
    ]);
    return stdout.trim() || null;
  } catch {
    return null;
  }
}

/**
 * Maps Node.js process.arch → OCI/Docker arch token.
 */
function hostDockerArch(): string {
  const a = process.arch;
  if (a === "x64") return "amd64";
  if (a === "arm64") return "arm64";
  return a;
}

/**
 * Build the three local-dev images (sandbox, controller, inference-router)
 * with --platform pinned to the host arch. Used by local-k8s when:
 *   - the user passed --build, OR
 *   - the cached image has the wrong arch (running an amd64 image
 *     under Rosetta on Apple Silicon crashes openclaw with
 *     `rt_tgsigqueueinfo failed in pend_signal`), OR
 *   - the image is missing entirely.
 *
 * Returns the list of images that were built (for logging).
 */
async function rebuildDevImages(
  runtime: string,
  repoRoot: string,
  archToken: string,
  forceAll: boolean,
  agtRepo?: string,
): Promise<string[]> {
  const platform = `linux/${archToken}`;
  // Resolve the AGT SDK tarball path. The Dockerfile's `AGT_SDK_TARBALL`
  // build-arg, when non-empty, swaps the stock npm install of
  // @microsoft/agent-governance-sdk@^3.5.0 (which is missing
  // MeshClient.registerSelf / autoRegister / registry-client.js — i.e.
  // sub-agents never POST /v1/agents, so peers cannot discover them and
  // mesh communication silently fails) for a local tarball that ships
  // those pieces. Mirrors the auto-discovery logic dev.ts uses for the
  // docker target so local-k8s isn't second-class on the mesh path.
  let agtSdkTarballBasename: string | undefined;
  let agtSdkTarballHostPath: string | undefined;
  if (agtRepo) {
    const fsMod = await import("node:fs");
    const tsDir = path.join(agtRepo, "agent-governance-typescript");
    const findTarball = (): string | undefined => {
      try {
        const hits = fsMod
          .readdirSync(tsDir)
          .filter((f) => f.startsWith("microsoft-agent-governance-sdk-") && f.endsWith(".tgz"))
          .sort();
        return hits.length > 0 ? hits[hits.length - 1] : undefined;
      } catch { return undefined; }
    };

    let picked = findTarball();
    if (!picked && fsMod.existsSync(path.join(tsDir, "package.json"))) {
      // No pre-packed tarball. Build & pack the SDK from source so the
      // sandbox image gets the patched MeshClient (stock npm 3.5.0
      // lacks registerSelf/autoRegister → sub-agents never register).
      console.log(chalk.dim(`  No microsoft-agent-governance-sdk-*.tgz under ${tsDir} — packing from source (one-time)...\n`));
      try {
        await execa("npm", ["install", "--prefer-offline", "--no-audit", "--no-fund"], { cwd: tsDir, stdio: "inherit" });
        await execa("npm", ["run", "build"], { cwd: tsDir, stdio: "inherit" });
        await execa("npm", ["pack"], { cwd: tsDir, stdio: "inherit" });
        picked = findTarball();
      } catch (e) {
        console.log(chalk.yellow(`  Could not pack AGT SDK: ${(e as Error).message}\n  Continuing with npm @^3.5.0 (mesh registration will not work).\n`));
      }
    }

    if (picked) {
      const stagingDir = path.join(repoRoot, ".agt-sdk");
      if (!fsMod.existsSync(stagingDir)) fsMod.mkdirSync(stagingDir, { recursive: true });
      for (const f of fsMod.readdirSync(stagingDir)) {
        if (f.endsWith(".tgz") || f.endsWith(".tar.gz")) {
          fsMod.unlinkSync(path.join(stagingDir, f));
        }
      }
      fsMod.copyFileSync(path.join(tsDir, picked), path.join(stagingDir, picked));
      agtSdkTarballBasename = picked;
      agtSdkTarballHostPath = path.join(tsDir, picked);
    }
  }
  if (agtSdkTarballBasename) {
    console.log(chalk.dim(`  Using patched AGT SDK tarball: ${agtSdkTarballHostPath}\n`));
  } else if (agtRepo) {
    console.log(chalk.yellow(
      `  Warning: AGT SDK tarball unavailable under ${path.join(agtRepo, "agent-governance-typescript")} — falling back to npm @^3.5.0 (mesh registration will not work).\n`,
    ));
  }
  type Spec = { name: string; tag: string; build: () => Promise<void> };
  const specs: Spec[] = [
    {
      name: "inference-router",
      tag: "kars-inference-router:dev",
      build: async () => {
        // Router Dockerfile is COPY-only — stage the binary first.
        await stageRustBinaries(repoRoot, ["kars-inference-router"], archToken as RustArch);
        await execa(runtime, [
          "build",
          "--platform", platform,
          "--build-arg", `ROUTER_CACHE_BUST=${Date.now()}`,
          "-t", "kars-inference-router:dev",
          "-f", path.join(repoRoot, "inference-router/Dockerfile"),
          repoRoot,
        ], { stdio: "inherit" });
      },
    },
    {
      name: "controller",
      tag: "kars-controller:dev",
      build: async () => {
        // Controller Dockerfile is COPY-only — stage the binary first.
        await stageRustBinaries(repoRoot, ["kars-controller"], archToken as RustArch);
        await execa(runtime, [
          "build",
          "--platform", platform,
          "-t", "kars-controller:dev",
          "-f", path.join(repoRoot, "controller/Dockerfile"),
          repoRoot,
        ], { stdio: "inherit" });
      },
    },
    {
      name: "sandbox",
      tag: "kars-sandbox:dev",
      build: async () => {
        // Sandbox Dockerfile COPYs mesh-plugin/dist — stage it first.
        await stageMeshPlugin(repoRoot);
        // Base image first if not present (heavy — only built once).
        const baseTag = "kars-sandbox-base:dev";
        const azureLinux = "mcr.microsoft.com/azurelinux/base/core:3.0";
        if (!(await localImageExists(runtime, baseTag)) ||
            (await imageArch(runtime, baseTag)) !== archToken) {
          await execa(runtime, ["pull", "--platform", platform, azureLinux], { stdio: "pipe" }).catch(() => undefined);
          await execa(runtime, [
            "build",
            "--platform", platform,
            "--build-arg", `AZURELINUX_BASE=${azureLinux}`,
            "--build-arg", `OPENCLAW_CACHE_BUST=${Date.now()}`,
            "-t", baseTag,
            "-f", path.join(repoRoot, "sandbox-images/openclaw/Dockerfile.base"),
            repoRoot,
          ], { stdio: "inherit" });
        }
        await execa(runtime, [
          "build",
          "--platform", platform,
          "--build-arg", `SANDBOX_BASE_IMAGE=${baseTag}`,
          "--build-arg", `INFERENCE_ROUTER_IMAGE=kars-inference-router:dev`,
          "--build-arg", `MESH_PROVIDER=agt`,
          ...(agtSdkTarballBasename
            ? ["--build-arg", `AGT_SDK_TARBALL=${agtSdkTarballBasename}`]
            : []),
          "-t", "kars-sandbox:dev",
          "-f", path.join(repoRoot, "sandbox-images/openclaw/Dockerfile"),
          repoRoot,
        ], { stdio: "inherit" });
      },
    },
  ];

  const built: string[] = [];
  for (const s of specs) {
    const arch = await imageArch(runtime, s.tag);
    const archMismatch = arch !== null && arch !== archToken;
    const missing = arch === null;
    if (!forceAll && !missing && !archMismatch) continue;
    if (archMismatch) {
      console.log(chalk.dim(
        `  ${s.tag} is ${arch}, host is ${archToken} — rebuilding for ${platform}.`,
      ));
    } else if (missing) {
      console.log(chalk.dim(`  ${s.tag} not present — building for ${platform}.`));
    } else {
      console.log(chalk.dim(`  Rebuilding ${s.tag} for ${platform} (--build).`));
    }
    await s.build();
    built.push(s.tag);
  }
  return built;
}

async function loadImageIfPresent(
  kind: string,
  runtime: string,
  clusterName: string,
  /** Desired tag inside kind (matches values-local-dev.yaml). */
  targetImage: string,
  env: NodeJS.ProcessEnv,
  /** Fallback tags to retag-from if `targetImage` itself isn't local. */
  candidateAliases: string[] = [],
): Promise<{ loaded: boolean; reason?: string }> {
  const tryLoad = async (img: string): Promise<boolean> => {
    if (!(await localImageExists(runtime, img))) return false;
    if (img !== targetImage) {
      // Retag to the canonical name so values-local-dev.yaml's
      // `imagePullPolicy: Never` finds it. `image tag` works the same
      // under docker, podman, and nerdctl.
      await execa(runtime, ["tag", img, targetImage]);
    }
    await loadImageIntoKind(kind, runtime, clusterName, targetImage, env);
    return true;
  };

  for (const candidate of [targetImage, ...candidateAliases]) {
    if (await tryLoad(candidate)) {
      return { loaded: true };
    }
  }
  return {
    loaded: false,
    reason: `'${targetImage}' (and aliases: ${candidateAliases.join(", ") || "<none>"}) not found locally — build via 'make images'`,
  };
}

function findRepoRoot(start: string): string {
  let cur = start;
  while (cur !== "/" && !existsSync(path.join(cur, "Cargo.toml"))) {
    cur = path.dirname(cur);
  }
  if (cur === "/") {
    throw new Error(
      "Could not locate repo root (Cargo.toml). Run from inside the kars checkout.",
    );
  }
  return cur;
}

async function helmInstall(
  helm: string,
  kubectl: string,
  release: string,
  chartDir: string,
  valuesOverlays: string[],
  setArgs: string[] = [],
): Promise<void> {
  // We render-then-apply (rather than `helm install`) to keep failures
  // visible: `kubectl apply -f -` shows precisely which resources didn't
  // accept admission. Phase 4 may switch to `helm install --atomic` once
  // CRDs and the values overlay are stable.
  const args = [
    "template",
    release,
    chartDir,
    "--namespace",
    "kars-system",
    "--include-crds",
  ];
  for (const overlay of valuesOverlays) {
    args.push("-f", overlay);
  }
  for (const kv of setArgs) {
    args.push("--set", kv);
  }
  const { stdout } = await execa(helm, args);
  await execa(
    kubectl,
    ["apply", "-f", "-", "--server-side", "--force-conflicts"],
    {
      input: stdout,
      stdio: ["pipe", "inherit", "inherit"],
    },
  );
}

/**
 * Materialize a per-run Helm overlay carrying real inference creds from
 * `loadConfig()`. The controller picks the values up from its own env
 * (set via `controller.extraEnv`) and propagates `AZURE_OPENAI_API_KEY`
 * / `KARS_PROVIDER` / `COPILOT_GITHUB_TOKEN` to every spawned
 * router sidecar (see `controller/src/reconciler/mod.rs`). The router
 * auto-detects API-key auth when those env vars are present and
 * short-circuits the workload-identity / IMDS path used in AKS.
 *
 * The API key itself lives in a K8s Secret (`kars-dev-creds` in
 * `kars-system`) so it never lands in a values file or in
 * `kubectl describe` output. The overlay only references it via
 * `valueFrom.secretKeyRef`.
 *
 * Returns the absolute path to the rendered overlay; caller owns
 * cleanup. Pure dev creds — never used in AKS production where
 * workload identity handles auth.
 */
async function provisionDevCreds(
  kubectl: string,
  creds: KarsConfig,
  mcpGithub: GithubMcpDecision = { enabled: false, envVarName: "COPILOT_GITHUB_TOKEN" },
): Promise<string> {
  const SECRET_NAME = "kars-dev-creds";
  const NS = "kars-system";

  // Materialize the Secret idempotently. Using `apply` instead of `create`
  // so re-running `kars dev` after rotating creds picks up the new
  // value without having to delete the secret first.
  const dryRun = await execa(kubectl, [
    "create",
    "secret",
    "generic",
    SECRET_NAME,
    "-n",
    NS,
    `--from-literal=api-key=${creds.apiKey}`,
    "--dry-run=client",
    "-o",
    "yaml",
  ]);
  await execa(kubectl, ["apply", "-f", "-"], {
    input: dryRun.stdout,
    stdio: ["pipe", "inherit", "inherit"],
  });

  // For the foundry+MCP path we provision an extra Secret holding the
  // GitHub token. The controller will reference it via secretKeyRef
  // below (so the token never lands in the values file).
  if (mcpGithub.enabled && mcpGithub.tokenSecretName && mcpGithub.tokenInline) {
    const mcpSecret = await execa(kubectl, [
      "create",
      "secret",
      "generic",
      mcpGithub.tokenSecretName,
      "-n",
      NS,
      `--from-literal=token=${mcpGithub.tokenInline}`,
      "--dry-run=client",
      "-o",
      "yaml",
    ]);
    await execa(kubectl, ["apply", "-f", "-"], {
      input: mcpSecret.stdout,
      stdio: ["pipe", "inherit", "inherit"],
    });
  }


  // Build the values fragment. We always set AZURE_OPENAI_ENDPOINT (the
  // controller forwards it to both the OpenClaw container and the router
  // sidecar — see `controller/src/reconciler/mod.rs:1015,1223`). We
  // reference the API key via secretKeyRef so it never leaks into a
  // values file. KARS_PROVIDER + COPILOT_GITHUB_TOKEN are only set
  // for non-Foundry providers — same flag set the docker dev path uses.
  const isCopilot = creds.provider === "github-copilot";
  const isGithubModels = creds.provider === "github-models";
  const providerEnv =
    isCopilot || isGithubModels
      ? `        - name: KARS_PROVIDER\n          value: "${creds.provider}"\n`
      : "";
  // Copilot mode treats the API key as the GitHub PAT — pass it through
  // a second env var because `inference-router/src/copilot_auth.rs`
  // reads `COPILOT_GITHUB_TOKEN`, not `AZURE_OPENAI_API_KEY`.
  const copilotTokenEnv = isCopilot
    ? `        - name: COPILOT_GITHUB_TOKEN\n          valueFrom:\n            secretKeyRef:\n              name: ${SECRET_NAME}\n              key: api-key\n`
    : "";
  const projectEndpointEnv = creds.foundryProjectEndpoint
    ? `        - name: FOUNDRY_PROJECT_ENDPOINT\n          value: "${creds.foundryProjectEndpoint}"\n`
    : "";

  const overlay = [
    "# Auto-generated per-run dev overlay. Rewritten on every `kars dev` invocation.",
    "# Endpoint flows in via `inferenceRouter.azure.openai.endpoint` below — the chart's",
    "# controller-deployment.yaml already wires that into AZURE_OPENAI_ENDPOINT, so",
    "# duplicating it here would collide on apply.",
    "controller:",
    "  extraEnv:",
    "    - name: LEADER_ELECTION_ENABLED",
    '      value: "false"',
    "    - name: AZURE_OPENAI_API_KEY",
    "      valueFrom:",
    "        secretKeyRef:",
    `          name: ${SECRET_NAME}`,
    "          key: api-key",
    ...(isCopilot || isGithubModels
      ? ["    - name: KARS_PROVIDER", `      value: "${creds.provider}"`]
      : []),
    ...(isCopilot
      ? [
          "    - name: COPILOT_GITHUB_TOKEN",
          "      valueFrom:",
          "        secretKeyRef:",
          `          name: ${SECRET_NAME}`,
          "          key: api-key",
        ]
      : []),
    // Slice 4d.4.1 — GitHub MCP outbound bearer wiring. For copilot we
    // already added COPILOT_GITHUB_TOKEN above (router uses it for
    // inference too), so the MCP-only path is the foundry / github-models
    // case. Source:
    //   • github-models: reuse the existing api-key (it IS a GitHub PAT).
    //   • foundry: read from the separate `<tokenSecretName>` Secret we
    //     just provisioned (key=token).
    ...(mcpGithub.enabled && !isCopilot
      ? [
          "    - name: COPILOT_GITHUB_TOKEN",
          "      valueFrom:",
          "        secretKeyRef:",
          `          name: ${mcpGithub.tokenSecretName ?? SECRET_NAME}`,
          `          key: ${mcpGithub.tokenSecretName ? "token" : "api-key"}`,
        ]
      : []),
    // FOUNDRY_PROJECT_ENDPOINT is emitted by the chart from
    // `foundry.projectEndpoint` (templates/controller-deployment.yaml).
    // Setting it via extraEnv would collide on server-side apply with a
    // "duplicate entries for key" error, so set the value here instead.
    ...(creds.foundryProjectEndpoint
      ? ["foundry:", `  projectEndpoint: "${creds.foundryProjectEndpoint}"`]
      : []),
    "inferenceRouter:",
    "  azure:",
    "    openai:",
    `      endpoint: "${creds.endpoint}"`,
    `      deploymentName: "${creds.model}"`,
    "",
  ].join("\n");
  // Suppress unused-var lint warnings in the (unused) string variants.
  void providerEnv;
  void copilotTokenEnv;
  void projectEndpointEnv;

  const tmpDir = mkdtempSync(path.join(os.tmpdir(), "kars-dev-"));
  const overlayPath = path.join(tmpDir, "values-local-dev-creds.yaml");
  writeFileSync(overlayPath, overlay, { mode: 0o600 });
  return overlayPath;
}

/**
 * Build the AGT mesh relay+registry images locally, load them into the
 * kind cluster, and `kubectl apply` the manifest with image refs rewritten
 * to the local tags + imagePullPolicy=Never so the cluster does not try to
 * pull from ACR.
 *
 * Services are named (agentmesh-relay:8765, agentmesh-registry:8080) in
 * namespace "agentmesh", matching the controller's default env wiring.
 */
async function deployAgentMesh(
  tools: Tooling,
  clusterName: string,
  repoRoot: string,
  meshProvider: "agt",
  agtRepo: string | undefined,
  archToken: string,
): Promise<void> {
  void meshProvider;
  const platform = `linux/${archToken}`;
  const localTag = (component: "relay" | "registry"): string =>
    `agentmesh-${component}-agt:dev`;

  // ── Build relay + registry images locally (AGT Python) ────────────
  if (!agtRepo) {
    throw new Error(
      "--mesh-provider=agt requires --agt-repo or $KARS_AGT_REPO pointing at an agent-governance-toolkit checkout.\n" +
      "  Clone it:  git clone https://github.com/microsoft/agent-governance-toolkit",
    );
  }
  const agtDockerfile = path.join(
    agtRepo,
    "agent-governance-python/agent-mesh/docker/Dockerfile",
  );
  if (!existsSync(agtDockerfile)) {
    throw new Error(
      `AGT Dockerfile not found at ${agtDockerfile}\n` +
      `  Clone it:  git clone https://github.com/microsoft/agent-governance-toolkit ${agtRepo}\n` +
      `  Or pass --agt-repo <path> / set $KARS_AGT_REPO if you already have it elsewhere.`,
    );
  }
  for (const component of ["relay", "registry"] as const) {
    console.log(chalk.dim(`  Building agentmesh-${component} (AGT Python)…`));
    await execa(
      tools.runtime,
      [
        "build",
        "--platform",
        platform,
        "--build-arg",
        `COMPONENT=${component}`,
        "-t",
        localTag(component),
        "-f",
        agtDockerfile,
        agtRepo,
      ],
      { stdio: "inherit" },
    );
  }

  // ── Load images into kind ─────────────────────────────────────────
  for (const component of ["relay", "registry"] as const) {
    const tag = localTag(component);
    await execa(tools.kind, ["load", "docker-image", tag, "--name", clusterName], {
      env: tools.env,
      stdio: "inherit",
    });
  }

  // ── Rewrite manifest: swap ACR image refs → local tags, set Never ──
  const manifestPath = path.join(repoRoot, "deploy", "agentmesh-agt.yaml");
  if (!existsSync(manifestPath)) {
    throw new Error(`Mesh manifest not found at ${manifestPath}`);
  }
  let manifest = readFileSync(manifestPath, "utf8");
  // Plain-string replacements (no regex) — the manifest contains fixed ACR
  // image references that we swap for the local kind-loaded tags. Using
  // String.replaceAll avoids regex-anchor pitfalls flagged by CodeQL.
  const acrPrefix = "karsacr.azurecr.io";
  const repls: { from: string; to: string }[] = [
    { from: `${acrPrefix}/agentmesh-relay-agt:latest`, to: localTag("relay") },
    { from: `${acrPrefix}/agentmesh-registry-agt:latest`, to: localTag("registry") },
  ];
  for (const r of repls) {
    manifest = manifest.replaceAll(r.from, r.to);
  }
  // Pin imagePullPolicy=Never for the local images so kind never tries to
  // reach a registry.
  manifest = manifest.replace(
    /(\n\s+image:\s+agentmesh-(?:relay|registry)-agt:dev\b[^\n]*)/g,
    `$1\n          imagePullPolicy: Never`,
  );

  const tmpDir = mkdtempSync(path.join(os.tmpdir(), "kars-mesh-"));
  const rewritten = path.join(tmpDir, path.basename(manifestPath));
  writeFileSync(rewritten, manifest);
  try {
    await execa(tools.kubectl, ["apply", "-f", rewritten], { stdio: "inherit" });
  } finally {
    try {
      rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // Best-effort cleanup.
    }
  }

  // ── Wait for relay + registry rollout ─────────────────────────────
  for (const deploy of ["registry", "relay"]) {
    try {
      await execa(
        tools.kubectl,
        [
          "rollout",
          "status",
          "-n",
          "agentmesh",
          `deployment/${deploy}`,
          "--timeout=120s",
        ],
        { stdio: "inherit" },
      );
    } catch {
      console.warn(
        chalk.yellow(
          `  ⚠ agentmesh/${deploy} did not become ready within 120s — check 'kubectl describe deployment/${deploy} -n agentmesh'.`,
        ),
      );
    }
  }
}

export async function runLocalK8s(opts: LocalK8sOptions): Promise<void> {
  const stepper = new Stepper({ totalSteps: 13 });

  // Fail fast if the user is running outside the kars checkout.
  // `--target local-k8s` rebuilds the controller, router, and sandbox
  // images from local source via docker build, which needs the repo
  // root (Cargo.toml + deploy/helm/kars + sandbox-images/...).
  // Without this check the failure surfaces only AFTER kind cluster
  // creation (10+ seconds wasted, orphaned cluster left behind).
  if (!opts.noBuild) {
    try {
      findRepoRoot(process.cwd());
    } catch {
      throw new Error(
        "`kars dev --target local-k8s` must be run from inside the kars " +
          "repo checkout — the dev flow rebuilds the controller, inference-router, " +
          "and sandbox images from local source.\n\n" +
          "Either:\n" +
          "  • `cd` into your kars checkout and re-run, or\n" +
          "  • pass `--no-build` to use already-loaded images, or\n" +
          "  • use `--target docker` (no cluster, no rebuild — fastest dev loop), or\n" +
          "  • use `kars up` to deploy to an existing AKS cluster (ACR images).",
      );
    }
  }

  stepper.step("Checking local tooling (kind / kubectl / helm / container runtime)…");
  const tools = await ensureTooling();
  stepper.done(
    `tooling ready: ${path.basename(tools.kind)}, ${path.basename(tools.kubectl)}, ${path.basename(tools.helm)}, ${tools.runtimeName}`,
  );

  // Load creds up-front so we fail fast (and with a friendly pointer to
  // `kars credentials`) before paying the cost of cluster bringup
  // and image loading.
  stepper.step("Loading inference credentials…");
  const creds = loadConfig();
  if (!creds || !creds.apiKey || !creds.endpoint) {
    stepper.stop();
    throw new Error(
      "no inference credentials found. Run `kars credentials` (or `kars dev` once " +
        "without --target local-k8s) to configure GitHub Copilot / GitHub Models / Azure Foundry.",
    );
  }
  const providerLabel =
    creds.provider === "github-copilot"
      ? "GitHub Copilot"
      : creds.provider === "github-models"
        ? "GitHub Models"
        : "Azure Foundry / OpenAI";
  stepper.done(`creds: ${providerLabel} (${creds.endpoint})`);

  // Optional: GitHub MCP wiring (Slice 4d.4.1). Asks the user whether to
  // attach the upstream `api.githubcopilot.com/mcp` server to this
  // sandbox. The decision modulates BOTH `provisionDevCreds` (env+secret
  // wiring on the controller deployment) and `autoCreateSandbox`
  // (McpServer CR + mcpServerRefs on the KarsSandbox).
  const mcpGithub = await promptForGithubMcp(creds);
  if (mcpGithub.enabled) {
    console.log(
      chalk.dim(
        "  GitHub MCP will be wired (bearerFromEnv=" +
          mcpGithub.envVarName +
          "; allowedTools = read-only set).",
      ),
    );
  }

  stepper.step(`Ensuring kind cluster '${opts.clusterName}' exists…`);
  await ensureCluster(tools.kind, opts.clusterName, tools.env);
  stepper.done(`kind cluster '${opts.clusterName}' is ready`);

  // Ensure the three local-dev images exist AND match the host arch.
  // Without this, a cached linux/amd64 image left over from
  // `kars push` (which builds for AKS) would crash openclaw under
  // Rosetta on an Apple Silicon laptop with
  // `rt_tgsigqueueinfo failed in pend_signal`.
  if (!opts.noBuild) {
    const archToken = hostDockerArch();
    const repoRootForBuild = findRepoRoot(process.cwd());
    stepper.step(`Checking image arch (host=${archToken})…`);
    const built = await rebuildDevImages(
      tools.runtime,
      repoRootForBuild,
      archToken,
      opts.forceRebuild === true,
      opts.agtRepo,
    );
    if (built.length === 0) {
      stepper.done(`images already match host arch (${archToken})`);
    } else {
      stepper.done(`rebuilt ${built.length} image(s) for linux/${archToken}`);
    }
  }

  // The values-local-dev overlay pins all images to local "dev" tags
  // with imagePullPolicy=Never, so we MUST load all three images that
  // the chart references — sandbox, controller, inference-router.
  // Missing any of them turns the helm install into an ErrImageNeverPull
  // loop with no useful diagnostics.
  stepper.step("Loading kars images into the kind cluster…");
  if (opts.noBuild) {
    stepper.done("skipped image load (--no-build)");
  } else {
    const images: { target: string; aliases: string[] }[] = [
      {
        target: opts.image,
        aliases: [
          "karsacr.azurecr.io/openclaw-sandbox:latest",
          "kars.azurecr.io/openclaw-sandbox:latest",
        ],
      },
      {
        target: "kars-controller:dev",
        aliases: [
          "karsacr.azurecr.io/kars-controller:latest",
          "kars.azurecr.io/kars-controller:latest",
        ],
      },
      {
        target: "kars-inference-router:dev",
        aliases: [
          "karsacr.azurecr.io/kars-inference-router:latest",
          "kars.azurecr.io/kars-inference-router:latest",
        ],
      },
    ];
    const missing: string[] = [];
    for (const img of images) {
      const result = await loadImageIfPresent(
        tools.kind,
        tools.runtime,
        opts.clusterName,
        img.target,
        tools.env,
        img.aliases,
      );
      if (!result.loaded) {
        missing.push(result.reason ?? img.target);
      }
    }
    if (missing.length > 0) {
      console.warn(
        chalk.yellow(
          `  ⚠ some images missing from local ${tools.runtimeName}; the deployment will fail until you build them:\n     - ${missing.join("\n     - ")}\n     Hint: 'make images' or 'make build && make images' from repo root.`,
        ),
      );
    }
    stepper.done(`loaded ${images.length - missing.length}/${images.length} images`);
  }

  // Sandboxes are scheduled with `nodeSelector: kars.azure.com/pool=sandbox`.
  // On a single-node kind cluster we just label the control-plane node — no
  // taint, because tainting would also block system workloads (Headlamp,
  // controller, etc.) from scheduling on the only node we have.
  // (Production AKS uses a dedicated sandbox node pool with the matching
  // taint + sandbox toleration; in dev we don't have isolation to enforce.)
  try {
    const node = `${opts.clusterName}-control-plane`;
    await execa(tools.kubectl, [
      "label",
      "node",
      node,
      "kars.azure.com/pool=sandbox",
      "--overwrite",
    ]);
    // Best-effort: if a previous run added the NoSchedule taint, remove it
    // so Headlamp/controller can still schedule.
    try {
      await execa(tools.kubectl, [
        "taint",
        "node",
        node,
        "kars.azure.com/sandbox-",
      ]);
    } catch {
      // taint not present — fine
    }
  } catch {
    // Best-effort: if the node naming differs the user can fix manually.
  }

  stepper.step("Helm-installing the kars chart (with local-dev overlay)…");
  const repoRoot = findRepoRoot(process.cwd());
  const chartDir = path.join(repoRoot, "deploy", "helm", "kars");
  if (!existsSync(chartDir)) {
    throw new Error(`kars helm chart not found at ${chartDir}`);
  }
  const valuesOverlay = path.join(chartDir, "values-local-dev.yaml");
  if (!existsSync(valuesOverlay)) {
    throw new Error(
      `Expected local-dev overlay at ${valuesOverlay} — your checkout is incomplete.`,
    );
  }
  // Ensure the namespace exists before applying namespaced resources.
  try {
    await execa(tools.kubectl, ["create", "namespace", "kars-system"]);
  } catch {
    // Namespace already exists — proceed.
  }
  // Provision the dev-creds Secret + per-run overlay BEFORE helm-applying,
  // so the controller deployment picks up the secretKeyRef on its first
  // rollout (no second restart needed).
  const credsOverlay = await provisionDevCreds(tools.kubectl, creds, mcpGithub);
  try {
    const meshProvider = opts.meshProvider ?? "agt";
    await helmInstall(tools.helm, tools.kubectl, opts.name, chartDir, [
      valuesOverlay,
      credsOverlay,
    ], [
      `mesh.provider=${meshProvider}`,
      `meshPeer.clusterName=${opts.clusterName}`,
    ]);
  } finally {
    // The overlay only references the API key by name (secretKeyRef);
    // the file itself contains no secret material, but we still clean up
    // to avoid stale state across runs.
    try {
      rmSync(path.dirname(credsOverlay), { recursive: true, force: true });
    } catch {
      // Best-effort cleanup.
    }
  }
  stepper.done("chart applied");

  // Deploy the mesh relay + registry. Without this the controller starts
  // expecting to find agentmesh-relay:8765 in namespace 'agentmesh' but
  // the namespace doesn't exist on a fresh kind cluster — the controller
  // ends up in a WebSocket reconnect loop and sandboxes never get a
  // mesh peer. Phase 5 (AGT default) wires this in.
  //
  // Three skip-paths:
  //   1. --no-mesh: pure controller smoke test, no relay/registry at all.
  //   2. --global-registry: an external registry (port-forwarded from a
  //      remote AKS cluster via `kars mesh promote --port-forward`,
  //      or a shared dev URL) is already reachable — no need to deploy
  //      a second local copy. Federation/handoff scenarios live here.
  //   3. Default: build + deploy AGT relay+registry into the kind cluster.
  const meshProvider = opts.meshProvider ?? "agt";
  if (opts.noMesh === true) {
    stepper.step("Skipping agentmesh deployment (--no-mesh)…");
    stepper.done("mesh skipped — sandboxes will fail KNOCK / E2E");
  } else if (opts.globalRegistry) {
    stepper.step(
      `Skipping in-kind agentmesh deployment — using external registry ${opts.globalRegistry}…`,
    );
    stepper.done(
      `external registry: ${opts.globalRegistry} (no local relay/registry deployed)`,
    );
  } else {
    stepper.step(
      `Deploying agentmesh-${meshProvider} (relay + registry) into kind…`,
    );
    const archToken = hostDockerArch();
    await deployAgentMesh(
      tools,
      opts.clusterName,
      repoRoot,
      meshProvider,
      opts.agtRepo,
      archToken,
    );
    stepper.done(`agentmesh-${meshProvider} ready`);
  }

  stepper.step("Verifying controller deployment is rolling out…");
  // Force a rollout restart in case the deployment already existed (e.g.
  // user re-ran `kars dev` after rotating creds). Helm's apply
  // doesn't trigger a restart when only a referenced Secret changes;
  // explicitly restarting catches that case.
  try {
    await execa(tools.kubectl, [
      "rollout",
      "restart",
      "deployment/kars-controller",
      "-n",
      "kars-system",
    ]);
  } catch {
    // Deployment may not exist yet on first run — fine, rollout status
    // below will wait for the initial rollout instead.
  }
  // Best-effort: don't block forever if the controller image isn't on the
  // node yet. The user-facing exec recipe below works as soon as a
  // sandbox CR is created.
  try {
    await execa(
      tools.kubectl,
      [
        "rollout",
        "status",
        "deployment/kars-controller",
        "-n",
        "kars-system",
        "--timeout=120s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow(
        "  ⚠ controller deployment did not become ready within 120s — check 'kubectl describe deployment/kars-controller -n kars-system'.",
      ),
    );
  }
  stepper.done("controller rollout check finished");

  // Phase 4: Headlamp dashboard for local-k8s observability.
  // We treat Headlamp as a hard dependency of the local-k8s target — the
  // whole point is to give devs a UI without spinning up AKS / Portal.
  stepper.step("Installing Headlamp dashboard…");
  await installHeadlamp(tools, opts.clusterName);
  stepper.done("Headlamp installed");

  // Phase 5: side-load the kars Headlamp plugin (CRD views).
  // Built into a ConfigMap and volume-mounted at /headlamp/plugins/kars
  // so it survives pod restarts.
  stepper.step("Installing kars Headlamp plugin…");
  await installAzureclawPlugin(tools, opts.clusterName);
  stepper.done("kars plugin installed");

  // Phase 5b: Prometheus + Grafana stack so the Headlamp plugin's
  // metric panels (mesh topology msg counts, token budgets, AGT decisions,
  // policy bundle health) have data on first run — no manual helm dance.
  stepper.step("Installing Prometheus + Grafana stack…");
  await installPrometheus(tools, opts.clusterName);
  stepper.done("Prometheus + Grafana installed");

  // Open Headlamp in the user's browser. Port-forward runs detached so
  // it survives the CLI command exiting; user kills it via `kars dev down`
  // (Phase 6) or `pkill -f 'port-forward.*headlamp'`.
  const headlampPort = 4466;
  const headlampUrl = `http://localhost:${headlampPort}/`;
  await startHeadlampPortForward(tools, headlampPort);

  // Grafana + Prometheus port-forwards so the Headlamp plugin embeds work
  // out of the box. Grafana is exposed at :3000 (anonymous Viewer, allow_embedding
  // enabled in the chart values below). Prometheus at :19091 for ad-hoc PromQL.
  const grafanaPort = 3000;
  const grafanaUrl = `http://localhost:${grafanaPort}/`;
  const prometheusPort = 19091;
  const prometheusUrl = `http://localhost:${prometheusPort}/`;
  await startMonitoringPortForwards(tools, grafanaPort, prometheusPort);

  await openBrowser(headlampUrl);

  console.log("");
  console.log(chalk.green("  ✓ Local-k8s dev environment is ready."));
  console.log("");
  console.log(chalk.bold("  Headlamp dashboard:"));
  console.log(`    ${chalk.cyan(headlampUrl)}  (token printed below)`);
  console.log("");
  await printHeadlampToken(tools);
  console.log("");
  console.log(chalk.bold("  Observability stack:"));
  console.log(`    Grafana:    ${chalk.cyan(grafanaUrl)}   (anonymous Viewer)`);
  console.log(`    Prometheus: ${chalk.cyan(prometheusUrl)}`);
  console.log(
    chalk.dim(
      `    Default dashboards: 'kars — Sandbox Fleet Overview' (uid kars-fleet)\n` +
        `                        'kars — Agent Fleet Operations' (uid kars-ops)`,
    ),
  );
  console.log("");

  // ── Phase 9: auto-create sandbox + WebUI port-forward ─────────────
  // Mirrors docker-mode UX: at this point the user has answered
  // creds + name + channels, so go all the way and bring up THEIR
  // sandbox, not just the platform. Saves the manual `kubectl apply
  // -f examples/...` + `kars connect <name>` dance.
  stepper.step(`Creating sandbox '${opts.name}'…`);
  await autoCreateSandbox(tools, opts, creds, mcpGithub);
  stepper.done(`sandbox CR applied (kars-${opts.name})`);

  stepper.step("Waiting for sandbox pod to be ready…");
  await waitForSandboxReady(tools, opts.name);
  stepper.done("sandbox pod is Running");

  stepper.step("Reading gateway token + starting WebUI port-forward…");
  const { url: webUrl, token: gwToken } = await startSandboxConnect(
    tools,
    opts.name,
  );
  stepper.done("WebUI ready");

  console.log("");
  console.log(chalk.bold("  OpenClaw WebUI:"));
  // `startSandboxConnect` already embeds `#token=...` in the returned URL
  // when the gateway token is known, so we print it as-is. Earlier we
  // appended a second `#token=...`, producing a malformed double-fragment.
  console.log(`    ${webUrl}`);
  if (gwToken) {
    console.log("");
    console.log(chalk.dim("  Gateway token (copy if the URL hash is stripped):"));
    console.log(`    ${gwToken}`);
  } else {
    console.log("");
    console.log(
      chalk.yellow(
        "  ⚠ gateway token not yet written — the openclaw container is still " +
          "initializing. Once ready, run 'kars connect " +
          opts.name +
          "' to get a clickable login URL.",
      ),
    );
  }
  console.log("");
  // Only auto-open when we have a token; opening a tokenless URL just
  // lands on "unauthorized: gateway token missing" which is worse UX
  // than printing the connect command above.
  if (gwToken) {
    await openBrowser(webUrl);
  }

  console.log(chalk.bold("  Next steps:"));
  console.log(
    `    kars connect ${opts.name}   ${chalk.dim("# re-open the WebUI later")}`,
  );
  console.log(
    `    kubectl get pods -A --context kind-${opts.clusterName}`,
  );
  console.log("");
  if (opts.ephemeral) {
    console.log(
      chalk.dim(
        `  --ephemeral: cluster will NOT be destroyed automatically yet.\n  Run 'kind delete cluster --name ${opts.clusterName}' when finished.`,
      ),
    );
  }
}

/**
 * Install Headlamp via its official Helm chart. Idempotent — re-running
 * does an upgrade.
 *
 * Using NodePort + a short-lived port-forward (Phase 4) keeps us out of
 * Ingress controller territory; Phase 6 may add an opt-in ingress for
 * users who want a stable URL.
 */
async function installHeadlamp(tools: Tooling, clusterName: string): Promise<void> {
  // Ensure the headlamp namespace exists. `helm install --create-namespace`
  // can't be used because we use template-and-apply to keep diagnostics clean.
  try {
    await execa(tools.kubectl, [
      "--context",
      `kind-${clusterName}`,
      "create",
      "namespace",
      "headlamp",
    ]);
  } catch {
    // already exists — fine
  }

  // Add the Headlamp Helm repo (idempotent).
  try {
    await execa(tools.helm, [
      "repo",
      "add",
      "headlamp",
      "https://kubernetes-sigs.github.io/headlamp/",
    ]);
  } catch {
    // already added — fine
  }
  await execa(tools.helm, ["repo", "update", "headlamp"]);

  // Render-and-apply (consistent with how we apply the kars chart).
  //
  // NOTE: the chart version is pinned. The kars Headlamp plugin
  // (tools/headlamp-plugin) is built against @kinvolk/headlamp-plugin
  // ^0.13.0 and depends on a specific `pluginLib` API surface
  // (K8s.cluster.KubeObject, CommonComponents.SimpleTable/SectionBox/Link).
  // Newer chart releases (0.42+) ship Headlamp images whose runtime API
  // has drifted enough that our plugin fails to mount its sidebar entries
  // or crashes in the list view. Pinning keeps `kars dev` reproducible
  // until we re-test against a newer version and bump intentionally.
  const HEADLAMP_CHART_VERSION = "0.41.0";
  const { stdout } = await execa(tools.helm, [
    "template",
    "headlamp",
    "headlamp/headlamp",
    "--version",
    HEADLAMP_CHART_VERSION,
    "--namespace",
    "headlamp",
    "--set",
    "config.useNodeInternalDNS=false",
  ]);
  await execa(
    tools.kubectl,
    [
      "--context",
      `kind-${clusterName}`,
      "apply",
      "-f",
      "-",
      "--server-side",
      "--force-conflicts",
    ],
    {
      input: stdout,
      stdio: ["pipe", "inherit", "inherit"],
    },
  );

  // Wait for headlamp to be ready (best-effort 90s).
  try {
    await execa(
      tools.kubectl,
      [
        "--context",
        `kind-${clusterName}`,
        "rollout",
        "status",
        "deployment/headlamp",
        "-n",
        "headlamp",
        "--timeout=90s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow(
        "  ⚠ Headlamp deployment did not become ready within 90s — check 'kubectl get pods -n headlamp'.",
      ),
    );
  }

  // Headlamp's Helm chart already creates a ClusterRoleBinding 'headlamp-admin'
  // that binds the 'headlamp' ServiceAccount to cluster-admin, so we don't
  // need to create our own. We just need to mint tokens against that SA.
}

/**
 * Install kube-prometheus-stack + apply our PodMonitor/json-exporter/dashboard
 * manifests so observability is wired end-to-end on first run. Mirrors the
 * Headlamp pattern (helm template → kubectl apply, idempotent).
 *
 * Defaults are tuned for a kind cluster:
 *   - AlertManager off (no UI, no PVC)
 *   - 2d retention (kind disks are ephemeral anyway)
 *   - Grafana: anonymous Viewer + allow_embedding so the Headlamp iframe
 *     works without auth
 *   - podMonitorSelectorNilUsesHelmValues=false so our PodMonitor is
 *     discovered without label gymnastics
 *
 * The chart version is pinned for reproducibility. The matching Headlamp
 * plugin (tools/headlamp-plugin) reads
 *   kars_mesh_messages_{sent,received}_total
 *   kars_tokens_total
 *   kars_agt_policy_evaluations_total
 *   agentmesh_relay_*
 * which all light up once the PodMonitor + json-exporter manifests below
 * are applied.
 */
async function installPrometheus(tools: Tooling, clusterName: string): Promise<void> {
  const ctx = `kind-${clusterName}`;

  // Ensure namespace exists + has the labels the sandbox NetworkPolicy
  // ingress allows scraping from (app.kubernetes.io/name=kars,
  // component=system). Without these labels kindnet would block the
  // PodMonitor scrape on :8443.
  try {
    await execa(tools.kubectl, ["--context", ctx, "create", "namespace", "monitoring"]);
  } catch {
    // already exists — fine
  }
  await execa(tools.kubectl, [
    "--context",
    ctx,
    "label",
    "namespace",
    "monitoring",
    "app.kubernetes.io/name=kars",
    "app.kubernetes.io/component=system",
    "--overwrite",
  ]);

  // Add the prometheus-community helm repo (idempotent).
  try {
    await execa(tools.helm, [
      "repo",
      "add",
      "prometheus-community",
      "https://prometheus-community.github.io/helm-charts",
    ]);
  } catch {
    // already added — fine
  }
  await execa(tools.helm, ["repo", "update", "prometheus-community"]);

  // Write a tmp values file — kube-prometheus-stack has nested keys with
  // dots ("grafana.ini") that are painful to express via --set. Yaml is
  // clearer and survives chart upgrades.
  const valuesYaml = `
alertmanager:
  enabled: false

prometheus:
  prometheusSpec:
    retention: 2d
    # Pick up PodMonitors created elsewhere in the cluster (our deploy/monitoring/*.yaml)
    podMonitorSelectorNilUsesHelmValues: false
    serviceMonitorSelectorNilUsesHelmValues: false
    probeSelectorNilUsesHelmValues: false
    ruleSelectorNilUsesHelmValues: false
    resources:
      requests:
        cpu: 100m
        memory: 256Mi
      limits:
        memory: 1Gi

grafana:
  adminPassword: admin
  defaultDashboardsTimezone: browser
  service:
    type: ClusterIP
  grafana.ini:
    auth.anonymous:
      enabled: true
      org_role: Viewer
    security:
      allow_embedding: true
      cookie_samesite: none
  sidecar:
    dashboards:
      enabled: true
      label: grafana_dashboard
      labelValue: "1"
      searchNamespace: ALL
  resources:
    requests:
      cpu: 50m
      memory: 128Mi

# Trim down for kind — full kube-state-metrics + node-exporter are kept
# because the in-image Grafana dashboards reference them.
defaultRules:
  create: false

kubeApiServer:
  enabled: true
kubelet:
  enabled: true
kubeControllerManager:
  enabled: false
coreDns:
  enabled: false
kubeEtcd:
  enabled: false
kubeScheduler:
  enabled: false
kubeProxy:
  enabled: false
`.trimStart();

  const KPS_CHART_VERSION = "85.3.3";
  const tmpdir = mkdtempSync(path.join(os.tmpdir(), "kars-kps-"));
  const valuesPath = path.join(tmpdir, "values.yaml");
  writeFileSync(valuesPath, valuesYaml);

  try {
    const { stdout } = await execa(tools.helm, [
      "template",
      "kps",
      "prometheus-community/kube-prometheus-stack",
      "--version",
      KPS_CHART_VERSION,
      "--namespace",
      "monitoring",
      "--include-crds",
      "--values",
      valuesPath,
    ]);

    // Split CRDs from CRs: kubectl --server-side races otherwise, which
    // surfaces on AKS as "no matches for kind Prometheus/ServiceMonitor".
    // Apply CRDs, wait for Established, then apply the rest.
    const docs = stdout.split(/^---\s*$/m).filter((d) => d.trim().length > 0);
    const crdDocs: string[] = [];
    const otherDocs: string[] = [];
    const crdNames: string[] = [];
    for (const doc of docs) {
      if (/^kind:\s*CustomResourceDefinition\s*$/m.test(doc)) {
        crdDocs.push(doc);
        const m = doc.match(/^\s*name:\s*([^\s]+)/m);
        if (m) crdNames.push(m[1]);
      } else {
        otherDocs.push(doc);
      }
    }

    if (crdDocs.length > 0) {
      await execa(
        tools.kubectl,
        ["--context", ctx, "apply", "-f", "-", "--server-side", "--force-conflicts"],
        { input: crdDocs.join("\n---\n"), stdio: ["pipe", "inherit", "inherit"] },
      );
      for (const name of crdNames) {
        try {
          await execa(
            tools.kubectl,
            ["--context", ctx, "wait", "--for=condition=Established", `crd/${name}`, "--timeout=60s"],
            { stdio: "pipe" },
          );
        } catch {
          /* best-effort */
        }
      }
    }

    if (otherDocs.length > 0) {
      await execa(
        tools.kubectl,
        ["--context", ctx, "apply", "-f", "-", "--server-side", "--force-conflicts"],
        { input: otherDocs.join("\n---\n"), stdio: ["pipe", "inherit", "inherit"] },
      );
    }
  } finally {
    try {
      rmSync(tmpdir, { recursive: true, force: true });
    } catch {
      // best-effort cleanup
    }
  }

  // Wait for Grafana to be ready (best-effort 180s — kube-prometheus-stack
  // pulls several images on first install). Don't fail the whole dev
  // command if it overruns; the Operator may still be reconciling Prometheus.
  try {
    await execa(
      tools.kubectl,
      [
        "--context",
        ctx,
        "rollout",
        "status",
        "deployment/kps-grafana",
        "-n",
        "monitoring",
        "--timeout=180s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow(
        "  ⚠ Grafana deployment did not become ready within 180s — observability panels may be empty until 'kubectl get pods -n monitoring' shows kps-grafana Ready.",
      ),
    );
  }

  // Apply our kars-specific monitoring manifests: PodMonitor for
  // every sandbox router, prometheus-json-exporter for the AGT relay
  // /health endpoint, and the two Grafana dashboards (fleet + ops).
  const repoRoot = findRepoRoot(process.cwd());
  const monitoringDir = path.join(repoRoot, "deploy", "monitoring");
  for (const f of [
    "podmonitor-sandbox-router.yaml",
    "agentmesh-json-exporter.yaml",
    "grafana-dashboard-configmap.yaml",
  ]) {
    const p = path.join(monitoringDir, f);
    if (!existsSync(p)) {
      console.warn(chalk.yellow(`  ⚠ missing ${f} — skipping`));
      continue;
    }
    await execa(
      tools.kubectl,
      [
        "--context",
        ctx,
        "apply",
        "-f",
        p,
        "--server-side",
        "--force-conflicts",
      ],
      { stdio: "inherit" },
    );
  }
}

/**
 * Start detached `kubectl port-forward` for Grafana (3000) and Prometheus
 * (19091) so the Headlamp plugin's iframe + ad-hoc PromQL just work
 * after `kars dev`. Same kill-existing-then-spawn pattern as the
 * Headlamp port-forward.
 */
async function startMonitoringPortForwards(
  tools: Tooling,
  grafanaLocalPort: number,
  prometheusLocalPort: number,
): Promise<void> {
  const { spawn } = await import("node:child_process");
  const startOne = async (
    localPort: number,
    target: string,
    targetPort: number,
  ): Promise<void> => {
    try {
      const { stdout } = await execa("lsof", ["-ti", `:${localPort}`]);
      for (const pid of stdout.trim().split(/\s+/).filter(Boolean)) {
        try {
          await execa("kill", [pid]);
        } catch {
          // process already gone
        }
      }
    } catch {
      // lsof returns non-zero when nothing matches — fine
    }
    const child = spawn(
      tools.kubectl,
      [
        "port-forward",
        "-n",
        "monitoring",
        target,
        `${localPort}:${targetPort}`,
      ],
      {
        detached: true,
        stdio: "ignore",
      },
    );
    child.unref();
  };

  // kps-grafana service is on :80, kps-kube-prometheus-stack-prometheus on :9090.
  await startOne(grafanaLocalPort, "service/kps-grafana", 80);
  await startOne(prometheusLocalPort, "service/kps-kube-prometheus-stack-prometheus", 9090);

  // Give the forwards a moment to bind.
  await new Promise((r) => setTimeout(r, 1500));
}

/**
 * Side-load the kars Headlamp plugin.
 *
 * Strategy: package `tools/headlamp-plugin/dist/main.js` + `package.json`
 * into a ConfigMap, then patch the Headlamp deployment to mount the
 * ConfigMap at `/headlamp/plugins/kars`. This survives pod restarts
 * (`kubectl cp` would not — it writes to ephemeral container fs).
 *
 * If the plugin hasn't been built yet we fall back to building it on
 * demand via `npm run build` so first-time devs don't have to remember
 * an extra step. If the build fails (no `node_modules`) we print a
 * helpful warning and skip — the dashboard still works for built-in
 * resources.
 */
async function installAzureclawPlugin(
  tools: Tooling,
  clusterName: string,
): Promise<void> {
  const repoRoot = findRepoRoot(process.cwd());
  const pluginDir = path.join(repoRoot, "tools", "headlamp-plugin");
  const distDir = path.join(pluginDir, "dist");
  const mainJs = path.join(distDir, "main.js");
  const pkgJson = path.join(pluginDir, "package.json");

  if (!existsSync(mainJs)) {
    console.log(
      chalk.dim("    plugin not built yet — running 'npm run build' in tools/headlamp-plugin…"),
    );
    if (!existsSync(path.join(pluginDir, "node_modules"))) {
      try {
        await execa("npm", ["install", "--no-audit", "--no-fund"], {
          cwd: pluginDir,
          stdio: "inherit",
        });
      } catch (err) {
        console.warn(
          chalk.yellow(
            `    ⚠ npm install failed (${(err as Error).message}); skipping plugin install. ` +
              "Run 'cd tools/headlamp-plugin && npm install && npm run build' manually then re-run this command.",
          ),
        );
        return;
      }
    }
    try {
      await execa("npm", ["run", "build"], { cwd: pluginDir, stdio: "inherit" });
    } catch (err) {
      console.warn(
        chalk.yellow(
          `    ⚠ plugin build failed (${(err as Error).message}); skipping plugin install.`,
        ),
      );
      return;
    }
  }

  // Build the ConfigMap. Headlamp expects each plugin to be a sub-dir
  // under /headlamp/plugins/<name>/ containing main.js + package.json.
  const ctx = `kind-${clusterName}`;
  const mainContent = readFileSync(mainJs, "utf8");
  const pkgContent = readFileSync(pkgJson, "utf8");

  const cmYaml = `apiVersion: v1
kind: ConfigMap
metadata:
  name: kars-headlamp-plugin
  namespace: headlamp
data:
  main.js: |
${indent(mainContent, 4)}
  package.json: |
${indent(pkgContent, 4)}
`;
  // server-side apply is idempotent and handles "binaryData: {}" reliably
  // — the previous client-side apply path occasionally fell back to
  // create on re-run because empty-map fields confuse the merger.
  await execa(
    tools.kubectl,
    [
      "--context",
      ctx,
      "apply",
      "--server-side",
      "--force-conflicts",
      "--field-manager=kars-cli",
      "-f",
      "-",
    ],
    { input: cmYaml, stdio: ["pipe", "inherit", "inherit"] },
  );

  // Patch the Headlamp Deployment to add the ConfigMap as a volume +
  // mount it at /headlamp/plugins/kars. Use strategic merge patch
  // so we don't clobber existing volumes/mounts.
  //
  // NB: 'plugins' on the chart is at /build/plugins (the in-image
  // shipped plugins dir). User plugins go to /headlamp-plugins —
  // discoverable via the chart's --plugins-dir arg. To stay
  // compatible with both layouts we patch the container's args
  // explicitly to point at our mount, then mount our CM on top.
  //
  // Simpler approach: mount the CM at /headlamp-plugins/kars
  // and rewrite the -plugins-dir arg to /headlamp-plugins.
  const patch = JSON.stringify({
    spec: {
      template: {
        spec: {
          volumes: [
            {
              name: "kars-plugin",
              configMap: { name: "kars-headlamp-plugin" },
            },
          ],
          containers: [
            {
              name: "headlamp",
              args: [
                "-in-cluster",
                "-in-cluster-context-name=main",
                "-plugins-dir=/headlamp-plugins",
                "-session-ttl=86400",
              ],
              volumeMounts: [
                {
                  name: "kars-plugin",
                  mountPath: "/headlamp-plugins/kars",
                },
              ],
            },
          ],
        },
      },
    },
  });

  await execa(tools.kubectl, [
    "--context",
    ctx,
    "patch",
    "deployment",
    "headlamp",
    "-n",
    "headlamp",
    "--type=strategic",
    "-p",
    patch,
  ]);

  // Wait for the new pod to come up.
  try {
    await execa(
      tools.kubectl,
      [
        "--context",
        ctx,
        "rollout",
        "status",
        "deployment/headlamp",
        "-n",
        "headlamp",
        "--timeout=90s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow(
        "    ⚠ Headlamp rollout did not complete in 90s after plugin patch — check 'kubectl get pods -n headlamp'.",
      ),
    );
  }
}

function indent(s: string, n: number): string {
  const pad = " ".repeat(n);
  return s
    .split("\n")
    .map((l) => pad + l)
    .join("\n");
}


/**
 * Start `kubectl port-forward` for Headlamp in the background. We
 * detach via 'spawn' (not execa.{detached}) so the CLI process can
 * exit while leaving the forward running. The user kills it with
 * `kars dev down --target local-k8s` (Phase 6).
 */
async function startHeadlampPortForward(
  tools: Tooling,
  localPort: number,
): Promise<void> {
  // Best-effort: kill any existing port-forward on the same port to avoid
  // EADDRINUSE on re-runs. We don't fail if there's nothing to kill.
  try {
    const { stdout } = await execa("lsof", ["-ti", `:${localPort}`]);
    const pids = stdout.trim().split(/\s+/).filter(Boolean);
    for (const pid of pids) {
      try {
        await execa("kill", [pid]);
      } catch {
        // process already gone
      }
    }
  } catch {
    // lsof returns non-zero when nothing matches — fine
  }

  const { spawn } = await import("node:child_process");
  const child = spawn(
    tools.kubectl,
    [
      "port-forward",
      "-n",
      "headlamp",
      "service/headlamp",
      `${localPort}:80`,
    ],
    {
      detached: true,
      stdio: "ignore",
    },
  );
  child.unref();

  // Give the forward ~1.5s to bind so the browser open below doesn't
  // race the listener.
  await new Promise((r) => setTimeout(r, 1500));
}

/**
 * Cross-platform `open <url>`. Best-effort — if it fails the URL is
 * already printed to stdout for the user to click manually.
 */
async function openBrowser(url: string): Promise<void> {
  const cmd =
    process.platform === "darwin"
      ? "open"
      : process.platform === "win32"
        ? "start"
        : "xdg-open";
  try {
    await execa(cmd, [url], { stdio: "ignore" });
  } catch {
    // user can click the printed URL
  }
}

/**
 * Print the Headlamp service-account token. Headlamp's auth is a plain
 * Bearer token — we mint one for the cluster-admin SA we bound above
 * and dump it for the user to paste into the Headlamp login screen.
 */
async function printHeadlampToken(tools: Tooling): Promise<void> {
  try {
    const { stdout } = await execa(tools.kubectl, [
      "create",
      "token",
      "headlamp",
      "-n",
      "headlamp",
      "--duration=24h",
    ]);
    console.log(chalk.bold("  Headlamp login token:"));
    console.log(`    ${chalk.dim(stdout.trim())}`);
  } catch (err) {
    console.warn(
      chalk.yellow(
        `  ⚠ could not mint Headlamp token (${(err as Error).message}); ` +
          "run 'kubectl create token headlamp -n headlamp --duration=24h' manually.",
      ),
    );
  }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 9: auto-create sandbox + WebUI port-forward
// ─────────────────────────────────────────────────────────────────────

/**
 * Resolve a comma-separated channel string ("telegram,slack.dev") to a
 * map of channel→token. Mirrors the docker-mode resolver in dev.ts so
 * users get the same dot-suffix variant semantics on both targets.
 *
 * Returns only channels that have a saved token; missing channels are
 * silently skipped (the user-facing prompt already filters to channels
 * with tokens, so a missing one means the user typed --channels
 * manually).
 */
function resolveChannelTokens(
  channels: string | undefined,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (!channels) return out;
  const parts = String(channels)
    .split(",")
    .map(s => s.trim().toLowerCase())
    .filter(Boolean);
  for (const ch of parts) {
    const dotIdx = ch.indexOf(".");
    const base = dotIdx > 0 ? ch.slice(0, dotIdx) : ch;
    const suffix = dotIdx > 0 ? ch.slice(dotIdx) : "";
    if (base !== "telegram" && base !== "slack" && base !== "discord") continue;
    const baseKey = `${base}-token`;
    const token =
      (suffix ? getSecret(baseKey + suffix) : undefined) ?? getSecret(baseKey);
    if (token) out[base] = token;
  }
  return out;
}

/**
 * Look up the saved Telegram allow-from list (comma-separated numeric
 * user IDs) from `kars credentials`. Mirrors how docker mode
 * resolves it (see `cli/src/commands/dev.ts` ~line 1260). Without
 * this, local-k8s sandboxes start the Telegram channel unrestricted
 * (any chat can DM the bot) while docker mode honours the allow-list,
 * causing a confusing inconsistency between the two `dev --target`
 * profiles.
 */
function resolveTelegramAllowFrom(channels: string | undefined): string | undefined {
  if (!channels) return undefined;
  const parts = String(channels)
    .split(",")
    .map(s => s.trim().toLowerCase())
    .filter(Boolean);
  for (const ch of parts) {
    const dotIdx = ch.indexOf(".");
    const base = dotIdx > 0 ? ch.slice(0, dotIdx) : ch;
    const suffix = dotIdx > 0 ? ch.slice(dotIdx) : "";
    if (base !== "telegram") continue;
    const baseKey = "telegram-allow-from";
    const v =
      (suffix ? getSecret(baseKey + suffix) : undefined) ?? getSecret(baseKey);
    if (v) return v;
  }
  return undefined;
}

/**
 * Auto-create the sandbox in the cluster: a one-shot YAML bundle with
 * the namespace, optional credentials Secret (telegram/slack/discord
 * tokens), the InferencePolicy CR, and the KarsSandbox CR. Server-side
 * apply so re-running `kars dev` is idempotent.
 *
 * The InferencePolicy `provider` field is just a tag — the actual
 * upstream is governed by the controller env (set by the per-run
 * dynamic overlay in `provisionDevCreds`). All upstream auth flows
 * (Foundry / GitHub Models / GitHub Copilot) end up in the same
 * `azure-openai` provider tag here.
 */
async function autoCreateSandbox(
  tools: Tooling,
  opts: LocalK8sOptions,
  creds: KarsConfig,
  mcpGithub: GithubMcpDecision = { enabled: false, envVarName: "COPILOT_GITHUB_TOKEN" },
): Promise<void> {
  const ns = `kars-${opts.name}`;
  const policyName = `${opts.name}-inference`;

  // Channels: convert tokens to a base64-encoded Secret block. The
  // controller mounts `<name>-credentials` via `envFrom: secretRef`
  // when present (see reconciler/mod.rs ~line 1170), so TELEGRAM_BOT_TOKEN
  // / SLACK_BOT_TOKEN / DISCORD_BOT_TOKEN flow into the sandbox the
  // same way docker mode passes them via `-e`.
  const channelTokens = resolveChannelTokens(opts.channels);
  const telegramAllowFrom = resolveTelegramAllowFrom(opts.channels);
  const credsBlock =
    Object.keys(channelTokens).length > 0
      ? [
          "---",
          "apiVersion: v1",
          "kind: Secret",
          "metadata:",
          `  name: ${opts.name}-credentials`,
          `  namespace: ${ns}`,
          "type: Opaque",
          "stringData:",
          ...(channelTokens.telegram
            ? [`  TELEGRAM_BOT_TOKEN: "${channelTokens.telegram}"`]
            : []),
          ...(telegramAllowFrom
            ? [`  TELEGRAM_ALLOW_FROM: "${telegramAllowFrom}"`]
            : []),
          ...(channelTokens.slack
            ? [`  SLACK_BOT_TOKEN: "${channelTokens.slack}"`]
            : []),
          ...(channelTokens.discord
            ? [`  DISCORD_BOT_TOKEN: "${channelTokens.discord}"`]
            : []),
          "",
        ].join("\n")
      : "";

  const toolPolicyName = `${opts.name}-toolpolicy`;

  // Slice "well-oiled-machine" — auto-emit a default KarsMemory binding
  // for the Foundry provider path. github-copilot / github-models don't
  // have a Foundry Memory Store, so we skip the CR there (and the router
  // simply omits the memory tools, gracefully). The Foundry path lets the
  // router auto-provision the store on first 404 (see
  // kars_memory_reconciler.rs + Slice 6.5 follow-up docs).
  //
  // storeName must be a DNS-label (≤63 chars). Sandbox name is already
  // DNS-1123 (≤63 chars max in practice), but truncate defensively so
  // `<name>-mem` always fits. CR `metadata.name` has the 253-char budget,
  // so we use the more readable `<name>-memory` there.
  const isCopilot = creds.provider === "github-copilot";
  const isGithubModels = creds.provider === "github-models";
  const wantMemoryCr = !isCopilot && !isGithubModels;
  // storeName must match the OpenClaw plugin's hardcoded convention
  // `memory-${SANDBOX_NAME}` (see runtimes/openclaw/src/core/agt-tools/
  // foundry.ts:634, agt-task-loop.ts:608, agt-handoff.ts:144, etc.) —
  // the plugin builds /memory_stores/${store} URLs and the router
  // proxies them through unchanged, so if the CR and plugin disagree
  // the plugin auto-creates a second store and the binding goes
  // unused. Cap at 63 chars (DNS-label). 7-char prefix budget leaves
  // 56 chars for the sandbox name.
  const memoryStoreName = `memory-${opts.name.substring(0, 56)}`;

  const yaml = [
    "---",
    "apiVersion: v1",
    "kind: Namespace",
    "metadata:",
    `  name: ${ns}`,
    "  labels:",
    `    kars.azure.com/sandbox: ${opts.name}`,
    credsBlock,
    "---",
    "apiVersion: kars.azure.com/v1alpha1",
    "kind: InferencePolicy",
    "metadata:",
    `  name: ${policyName}`,
    "  namespace: kars-system",
    "  labels:",
    `    kars.azure.com/sandbox: ${opts.name}`,
    "spec:",
    "  appliesTo:",
    `    sandboxName: ${opts.name}`,
    "  modelPreference:",
    "    primary:",
    "      provider: azure-openai",
    `      deployment: ${creds.model || "gpt-4.1"}`,
    "  contentSafety:",
    "    requirePromptShields: false",
    "  tokenBudget:",
    "    dailyTokens: 500000",
    "    perRequestTokens: 128000",
    "---",
    // Default ToolPolicy stub. The inference router unconditionally
    // injects `governance.toolPolicyRef = "<parent>-toolpolicy"` into
    // every spawned sub-agent CR (see inference-router/src/spawn/mod.rs
    // build_sub_agent_crd). Without this stub, every spawn lands in
    // Degraded with `ToolPolicyNotFound`. Permissive default — selector
    // matches the parent's sandbox label only, no rate-limit, no
    // approval, no commerce. Operators tighten via `kars toolpolicy`.
    //
    // `agtProfile.inline` is mandatory post-Slice-1e phase 2 (the bundled
    // sandbox-side fallback at /opt/kars-plugin/policies/ is gone),
    // so we inline the same default profile that `kars add` uses.
    "apiVersion: kars.azure.com/v1alpha1",
    "kind: ToolPolicy",
    "metadata:",
    `  name: ${toolPolicyName}`,
    "  namespace: kars-system",
    "  labels:",
    `    kars.azure.com/sandbox: ${opts.name}`,
    "spec:",
    "  appliesTo:",
    '    tool: "*"',
    "    sandboxMatchLabels:",
    `      kars.azure.com/sandbox: ${opts.name}`,
    "  agtProfile:",
    "    inline: |",
    ...loadAgtProfile("default")
      .replace(/\r\n/g, "\n")
      .replace(/\n+$/, "")
      .split("\n")
      .map((line) => `      ${line}`),
    "---",
    // Slice 4d.4.1 — McpServer/github (only emitted when the user opted
    // in during the dev-flow prompt). `bearerFromEnv=COPILOT_GITHUB_TOKEN`
    // tells the inference router to read that env var (controller wired
    // it via secretKeyRef in provisionDevCreds) and attach
    // `Authorization: Bearer <value>` to every outbound tools/list +
    // tools/call. allowedTools is intentionally read-only — we never
    // ship "*" because the bearer inherits Copilot/PAT scope which can
    // include write access.
    ...(mcpGithub.enabled
      ? [
          "apiVersion: kars.azure.com/v1alpha1",
          "kind: McpServer",
          "metadata:",
          "  name: github",
          "  namespace: kars-system",
          "spec:",
          "  url: https://api.githubcopilot.com/mcp",
          `  bearerFromEnv: ${mcpGithub.envVarName}`,
          "  allowedSandboxes:",
          "    matchLabels:",
          "      mcp-github: allow",
          "  allowedTools:",
          "    - list_pull_requests",
          "    - get_pull_request",
          "    - get_repository",
          "    - search_code",
          "    - search_issues",
          "---",
        ]
      : []),
    // well-oiled-machine — Foundry-only KarsMemory binding.
    // Auto-emits a default scope so the openclaw plugin's memory
    // tools (search_memories / update_memories / delete_scope in
    // runtimes/openclaw/src/core/agt-tools/foundry.ts) have a
    // resolved Memory Store from first boot. The router auto-
    // provisions the store on first 404 (kars_memory_reconciler.rs +
    // Slice 6.5 follow-up docs). Skipped for github-copilot /
    // github-models — no Foundry Memory Store on those paths.
    ...(wantMemoryCr
      ? [
          "apiVersion: kars.azure.com/v1alpha1",
          "kind: KarsMemory",
          "metadata:",
          `  name: ${opts.name}-memory`,
          "  namespace: kars-system",
          "  labels:",
          `    kars.azure.com/sandbox: ${opts.name}`,
          "spec:",
          "  sandboxRef:",
          `    name: ${opts.name}`,
          `  storeName: ${memoryStoreName}`,
          `  scope: "agent:${opts.name}"`,
          "  retentionDays: 30",
          "  deleteOnSandboxDelete: true",
          `  displayName: "Default memory for ${opts.name}"`,
          "---",
        ]
      : []),
    "apiVersion: kars.azure.com/v1alpha1",
    "kind: KarsSandbox",
    "metadata:",
    `  name: ${opts.name}`,
    "  namespace: kars-system",
    ...(mcpGithub.enabled
      ? ["  labels:", "    mcp-github: allow"]
      : []),
    "spec:",
    "  runtime:",
    "    kind: OpenClaw",
    "    openclaw:",
    '      version: "2026.3.13"',
    `      image: ${opts.image}`,
    "      config:",
    "        agent:",
    `          model: "azure/${creds.model || "gpt-4.1"}"`,
    "  sandbox:",
    '    isolation: "enhanced"',
    '    seccompProfile: "kars-strict"',
    "    readOnlyRootFilesystem: true",
    "    runAsNonRoot: true",
    "    allowPrivilegeEscalation: false",
    "    writablePaths:",
    "      - /sandbox",
    "      - /tmp",
    "  inferenceRef:",
    `    name: ${policyName}`,
    // well-oiled-machine — wire KarsMemory back-ref so the controller
    // mounts /etc/kars/memory/binding.json on the router and the
    // KarsMemory CR promotes from Compiled → Ready (router echo). Same
    // namespace as the sandbox (kars-system).
    ...(wantMemoryCr
      ? ["  memoryRef:", `    name: ${opts.name}-memory`]
      : []),
    // Enable AGT governance on the parent so the controller injects
    // AGT_RELAY_URL / AGT_REGISTRY_URL / AGT_GOVERNANCE_ENABLED into both
    // the openclaw + inference-router containers (controller/src/reconciler/
    // mod.rs:1111). Without this the parent never joins the mesh and
    // mesh sends from sub-agents back to the parent (and parent → child
    // discovery) fail. Sub-agents are auto-enabled by the router spawn
    // helper (inference-router/src/spawn/mod.rs:673); the parent must be
    // enabled here because the dev YAML is the source of truth for it.
    "  governance:",
    "    enabled: true",
    "    toolPolicyRef:",
    `      name: ${toolPolicyName}`,
    "    trustThreshold: 500",
    "    registryMode: local",
    // Slice 4d.4.1 — wire the GitHub MCP server when the user opted
    // in. The CR itself is emitted separately below; here we just
    // attach it via mcpServerRefs so the controller materializes
    // /etc/kars/mcp/github/meta.json into the router pod.
    ...(mcpGithub.enabled
      ? ["    mcpServerRefs:", "      - name: github"]
      : []),
      // Dev profile: run egress in learn mode so the forward proxy
    // (inference-router/src/forward_proxy.rs) logs new domains instead
    // of blocking them. Without this, channel integrations (Telegram,
    // Slack, Discord) fail at first run with
    //   `Network request for 'deleteMyCommands' failed`
    // because api.telegram.org / slack.com / discord.com aren't on the
    // allowlist. Operators promote learned domains to a strict
    // allowlist via `kars policy allow` once they're happy.
    "  networkPolicy:",
    "    defaultDeny: true",
    "    egressMode: Learn",
    "    allowedEndpoints: []",
    "",
  ].join("\n");

  await execa(
    tools.kubectl,
    [
      "--context",
      `kind-${opts.clusterName}`,
      "apply",
      "--server-side",
      "--force-conflicts",
      "--field-manager=kars-cli",
      "-f",
      "-",
    ],
    { input: yaml, stdio: ["pipe", "inherit", "inherit"] },
  );
}

/**
 * Wait for the controller to materialise the per-sandbox deployment and
 * for that deployment's pod to become Ready. Two-phase poll: first the
 * deployment object has to exist (the controller needs to reconcile the
 * CR we just applied), then `rollout status` blocks until pods Ready.
 *
 * On a fresh kind cluster this typically takes 20-60s — the controller
 * has to build the namespace, NetworkPolicy, ConfigMap, Deployment,
 * Service, and the seccomp installer DaemonSet on the node has to
 * project the profile before the sandbox pod can mount it.
 */
async function waitForSandboxReady(
  tools: Tooling,
  name: string,
): Promise<void> {
  const ns = `kars-${name}`;
  const ctx = ["--context", `kind-${ /* clusterName isn't on Tooling */ ""}`];
  // Strip the empty context arg if cluster name isn't tracked here —
  // current-context is set during `ensureCluster` to kind-<clusterName>
  // already, so we don't actually need --context for these calls.
  void ctx;

  // Phase 1: poll until the deployment object exists.
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      await execa(tools.kubectl, [
        "get",
        "deployment",
        name,
        "-n",
        ns,
      ]);
      break;
    } catch {
      await new Promise(r => setTimeout(r, 2000));
    }
  }
  if (Date.now() >= deadline) {
    throw new Error(
      `controller did not create deployment '${name}' in namespace '${ns}' within 120s. ` +
        `Check 'kubectl logs -n kars-system deploy/kars-controller'.`,
    );
  }

  // Phase 2: rollout status (blocks until pods Ready, has its own timeout).
  await execa(
    tools.kubectl,
    [
      "rollout",
      "status",
      `deployment/${name}`,
      "-n",
      ns,
      "--timeout=180s",
    ],
    { stdio: "inherit" },
  );
}

/**
 * Read the gateway token from the running sandbox pod and start a
 * background port-forward to its OpenClaw gateway (18789). Mirrors the
 * docker-mode UX where `dev` returns a clickable URL.
 *
 * The gateway token is written by entrypoint.sh to /tmp/gateway-token
 * inside the openclaw container after plugin init. Without it the WebUI
 * loads but rejects every request with 401.
 *
 * The port-forward is spawned **detached** so it survives the CLI
 * exiting; teardown is handled by `kars dev down` (which kills
 * any port-forward processes targeting the kind cluster).
 */
async function startSandboxConnect(
  tools: Tooling,
  name: string,
): Promise<{ url: string; token: string }> {
  const ns = `kars-${name}`;
  const localPort = 18789;

  // The token is provisioned by the controller as a K8s Secret named
  // `gateway-token` in the sandbox namespace. The openclaw container reads
  // it via the OPENCLAW_GATEWAY_TOKEN env var. We must NOT `kubectl exec
  // cat /tmp/gateway-token` here — that path is blocked by the
  // ValidatingAdmissionPolicy `kars-sandbox-exec-ban` and silently
  // 403s, causing this loop to time out after ~3 minutes even though the
  // gateway is up.
  //
  // Reading the Secret matches `kars connect`'s behavior (see
  // cli/src/commands/connect.ts ~line 143) and is gated by namespaced
  // RBAC on the Secret instead of a cluster-wide exec capability.
  let token = "";
  const maxAttempts = 60;
  for (let i = 0; i < maxAttempts; i++) {
    try {
      const { stdout: tokenB64 } = await execa(tools.kubectl, [
        "get",
        "secret",
        "-n",
        ns,
        "gateway-token",
        "-o",
        "jsonpath={.data.token}",
      ]);
      const trimmed = tokenB64.trim();
      if (trimmed) {
        token = Buffer.from(trimmed, "base64").toString("utf-8").trim();
        if (token) {
          break;
        }
      }
    } catch {
      // Secret may not exist yet — controller writes it as part of reconcile.
    }
    await new Promise(r => setTimeout(r, 1000));
  }

  // Detached port-forward. Match the headlamp port-forward pattern so
  // `kars dev down` can clean it up uniformly.
  const { spawn } = await import("node:child_process");
  const pf = spawn(
    tools.kubectl,
    [
      "port-forward",
      "-n",
      ns,
      `deploy/${name}`,
      `${localPort}:18789`,
    ],
    {
      detached: true,
      stdio: "ignore",
      env: tools.env,
    },
  );
  pf.unref();

  // Give kubectl a moment to bind the port before printing the URL.
  await new Promise(r => setTimeout(r, 1500));

  const url = token
    ? `http://localhost:${localPort}/#token=${token}`
    : `http://localhost:${localPort}/`;
  return { url, token };
}
