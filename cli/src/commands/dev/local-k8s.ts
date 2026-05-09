// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `azureclaw dev --target local-k8s` — runs a sandbox in a local kind
 * cluster instead of plain Docker. Pairs with a Headlamp dashboard
 * (added in a later phase) so developers get a real K8s view of their
 * agents without needing AKS.
 *
 * Phase 1: skeleton only.
 *   - Detects/creates a kind cluster (default name: azureclaw-dev).
 *   - Loads the locally-built azureclaw images into kind.
 *   - Helm-installs the existing chart in a local-friendly way.
 *   - Prints a `kubectl exec` recipe.
 *
 * Later phases add: values-local-dev overlay, fake-router, Headlamp,
 * AzureClaw Headlamp plugin, hot-reload, and lifecycle commands.
 */

import { execa } from "execa";
import chalk from "chalk";
import * as path from "node:path";
import * as os from "node:os";
import { existsSync, writeFileSync, mkdtempSync, rmSync, readFileSync } from "node:fs";
import { Stepper } from "../../stepper.js";
import { loadConfig, type AzureClawConfig } from "../../config.js";

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
  const override = process.env.AZURECLAW_DEV_RUNTIME?.toLowerCase();
  if (
    override === "docker" ||
    override === "podman" ||
    override === "nerdctl"
  ) {
    const p = await whichOptional(override);
    if (!p) {
      throw new Error(
        `AZURECLAW_DEV_RUNTIME=${override} but the '${override}' binary is not on PATH.`,
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
      "Set AZURECLAW_DEV_RUNTIME=docker|podman|nerdctl to override " +
      "autodetection.",
  );
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
 * Public helper used by `azureclaw dev down` so it can issue
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
  // The default kindest/node image is fine for our use case; pinning is
  // a Phase-2 hardening concern alongside the values-local-dev overlay.
  //
  // We pass an inline kind config that pre-clears the control-plane
  // NoSchedule taint. Without this, kubelet starts the node tainted and
  // CoreDNS / local-path-provisioner / Headlamp all emit "FailedScheduling
  // — untolerated taint" events at startup before the chart's untaint
  // step lands. The pods recover within ~30 s but the noisy red events
  // sit in Headlamp's event view forever, scaring first-time users.
  // Pre-clearing at node creation eliminates the race entirely.
  const kindConfig = `kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
    kubeadmConfigPatches:
      - |
        kind: InitConfiguration
        nodeRegistration:
          taints: []
`;
  await execa(kind, ["create", "cluster", "--name", name, "--config", "-"], {
    stdio: ["pipe", "inherit", "inherit"],
    input: kindConfig,
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
      "Could not locate repo root (Cargo.toml). Run from inside the azureclaw checkout.",
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
    "azureclaw-system",
    "--include-crds",
  ];
  for (const overlay of valuesOverlays) {
    args.push("-f", overlay);
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
 * / `AZURECLAW_PROVIDER` / `COPILOT_GITHUB_TOKEN` to every spawned
 * router sidecar (see `controller/src/reconciler/mod.rs`). The router
 * auto-detects API-key auth when those env vars are present and
 * short-circuits the workload-identity / IMDS path used in AKS.
 *
 * The API key itself lives in a K8s Secret (`azureclaw-dev-creds` in
 * `azureclaw-system`) so it never lands in a values file or in
 * `kubectl describe` output. The overlay only references it via
 * `valueFrom.secretKeyRef`.
 *
 * Returns the absolute path to the rendered overlay; caller owns
 * cleanup. Pure dev creds — never used in AKS production where
 * workload identity handles auth.
 */
async function provisionDevCreds(
  kubectl: string,
  creds: AzureClawConfig,
): Promise<string> {
  const SECRET_NAME = "azureclaw-dev-creds";
  const NS = "azureclaw-system";

  // Materialize the Secret idempotently. Using `apply` instead of `create`
  // so re-running `azureclaw dev` after rotating creds picks up the new
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

  // Build the values fragment. We always set AZURE_OPENAI_ENDPOINT (the
  // controller forwards it to both the OpenClaw container and the router
  // sidecar — see `controller/src/reconciler/mod.rs:1015,1223`). We
  // reference the API key via secretKeyRef so it never leaks into a
  // values file. AZURECLAW_PROVIDER + COPILOT_GITHUB_TOKEN are only set
  // for non-Foundry providers — same flag set the docker dev path uses.
  const isCopilot = creds.provider === "github-copilot";
  const isGithubModels = creds.provider === "github-models";
  const providerEnv =
    isCopilot || isGithubModels
      ? `        - name: AZURECLAW_PROVIDER\n          value: "${creds.provider}"\n`
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
    "# Auto-generated per-run dev overlay. Rewritten on every `azureclaw dev` invocation.",
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
      ? ["    - name: AZURECLAW_PROVIDER", `      value: "${creds.provider}"`]
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
    ...(creds.foundryProjectEndpoint
      ? ["    - name: FOUNDRY_PROJECT_ENDPOINT", `      value: "${creds.foundryProjectEndpoint}"`]
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

  const tmpDir = mkdtempSync(path.join(os.tmpdir(), "azureclaw-dev-"));
  const overlayPath = path.join(tmpDir, "values-local-dev-creds.yaml");
  writeFileSync(overlayPath, overlay, { mode: 0o600 });
  return overlayPath;
}

export async function runLocalK8s(opts: LocalK8sOptions): Promise<void> {
  const stepper = new Stepper({ totalSteps: 8 });

  stepper.step("Checking local tooling (kind / kubectl / helm / container runtime)…");
  const tools = await ensureTooling();
  stepper.done(
    `tooling ready: ${path.basename(tools.kind)}, ${path.basename(tools.kubectl)}, ${path.basename(tools.helm)}, ${tools.runtimeName}`,
  );

  // Load creds up-front so we fail fast (and with a friendly pointer to
  // `azureclaw credentials`) before paying the cost of cluster bringup
  // and image loading.
  stepper.step("Loading inference credentials…");
  const creds = loadConfig();
  if (!creds || !creds.apiKey || !creds.endpoint) {
    stepper.stop();
    throw new Error(
      "no inference credentials found. Run `azureclaw credentials` (or `azureclaw dev` once " +
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

  stepper.step(`Ensuring kind cluster '${opts.clusterName}' exists…`);
  await ensureCluster(tools.kind, opts.clusterName, tools.env);
  stepper.done(`kind cluster '${opts.clusterName}' is ready`);

  // The values-local-dev overlay pins all images to local "dev" tags
  // with imagePullPolicy=Never, so we MUST load all three images that
  // the chart references — sandbox, controller, inference-router.
  // Missing any of them turns the helm install into an ErrImageNeverPull
  // loop with no useful diagnostics.
  stepper.step("Loading AzureClaw images into the kind cluster…");
  if (opts.noBuild) {
    stepper.done("skipped image load (--no-build)");
  } else {
    const images: { target: string; aliases: string[] }[] = [
      {
        target: opts.image,
        aliases: [
          "azureclawacr.azurecr.io/openclaw-sandbox:latest",
          "azureclaw.azurecr.io/openclaw-sandbox:latest",
        ],
      },
      {
        target: "azureclaw-controller:dev",
        aliases: [
          "azureclawacr.azurecr.io/azureclaw-controller:latest",
          "azureclaw.azurecr.io/azureclaw-controller:latest",
        ],
      },
      {
        target: "azureclaw-inference-router:dev",
        aliases: [
          "azureclawacr.azurecr.io/azureclaw-inference-router:latest",
          "azureclaw.azurecr.io/azureclaw-inference-router:latest",
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

  // Sandboxes are scheduled with `nodeSelector: azureclaw.azure.com/pool=sandbox`.
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
      "azureclaw.azure.com/pool=sandbox",
      "--overwrite",
    ]);
    // Best-effort: if a previous run added the NoSchedule taint, remove it
    // so Headlamp/controller can still schedule.
    try {
      await execa(tools.kubectl, [
        "taint",
        "node",
        node,
        "azureclaw.azure.com/sandbox-",
      ]);
    } catch {
      // taint not present — fine
    }
  } catch {
    // Best-effort: if the node naming differs the user can fix manually.
  }

  stepper.step("Helm-installing the AzureClaw chart (with local-dev overlay)…");
  const repoRoot = findRepoRoot(process.cwd());
  const chartDir = path.join(repoRoot, "deploy", "helm", "azureclaw");
  if (!existsSync(chartDir)) {
    throw new Error(`AzureClaw helm chart not found at ${chartDir}`);
  }
  const valuesOverlay = path.join(chartDir, "values-local-dev.yaml");
  if (!existsSync(valuesOverlay)) {
    throw new Error(
      `Expected local-dev overlay at ${valuesOverlay} — your checkout is incomplete.`,
    );
  }
  // Ensure the namespace exists before applying namespaced resources.
  try {
    await execa(tools.kubectl, ["create", "namespace", "azureclaw-system"]);
  } catch {
    // Namespace already exists — proceed.
  }
  // Provision the dev-creds Secret + per-run overlay BEFORE helm-applying,
  // so the controller deployment picks up the secretKeyRef on its first
  // rollout (no second restart needed).
  const credsOverlay = await provisionDevCreds(tools.kubectl, creds);
  try {
    await helmInstall(tools.helm, tools.kubectl, opts.name, chartDir, [
      valuesOverlay,
      credsOverlay,
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

  stepper.step("Verifying controller deployment is rolling out…");
  // Force a rollout restart in case the deployment already existed (e.g.
  // user re-ran `azureclaw dev` after rotating creds). Helm's apply
  // doesn't trigger a restart when only a referenced Secret changes;
  // explicitly restarting catches that case.
  try {
    await execa(tools.kubectl, [
      "rollout",
      "restart",
      "deployment/azureclaw-controller",
      "-n",
      "azureclaw-system",
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
        "deployment/azureclaw-controller",
        "-n",
        "azureclaw-system",
        "--timeout=120s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    console.warn(
      chalk.yellow(
        "  ⚠ controller deployment did not become ready within 120s — check 'kubectl describe deployment/azureclaw-controller -n azureclaw-system'.",
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

  // Phase 5: side-load the AzureClaw Headlamp plugin (CRD views).
  // Built into a ConfigMap and volume-mounted at /headlamp/plugins/azureclaw
  // so it survives pod restarts.
  stepper.step("Installing AzureClaw Headlamp plugin…");
  await installAzureclawPlugin(tools, opts.clusterName);
  stepper.done("AzureClaw plugin installed");

  // Open Headlamp in the user's browser. Port-forward runs detached so
  // it survives the CLI command exiting; user kills it via `azureclaw dev down`
  // (Phase 6) or `pkill -f 'port-forward.*headlamp'`.
  const headlampPort = 4466;
  const headlampUrl = `http://localhost:${headlampPort}/`;
  await startHeadlampPortForward(tools, headlampPort);
  await openBrowser(headlampUrl);

  console.log("");
  console.log(chalk.green("  ✓ Local-k8s dev environment is ready."));
  console.log("");
  console.log(chalk.bold("  Headlamp dashboard:"));
  console.log(`    ${chalk.cyan(headlampUrl)}  (token printed below)`);
  console.log("");
  await printHeadlampToken(tools);
  console.log("");
  console.log(chalk.bold("  Next steps:"));
  console.log(
    `    kubectl get pods -A --context kind-${opts.clusterName}`,
  );
  console.log(
    `    kubectl apply -f examples/basic-agent/clawsandbox.yaml -n azureclaw-system`,
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

  // Render-and-apply (consistent with how we apply the azureclaw chart).
  const { stdout } = await execa(tools.helm, [
    "template",
    "headlamp",
    "headlamp/headlamp",
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
 * Side-load the AzureClaw Headlamp plugin.
 *
 * Strategy: package `tools/headlamp-plugin/dist/main.js` + `package.json`
 * into a ConfigMap, then patch the Headlamp deployment to mount the
 * ConfigMap at `/headlamp/plugins/azureclaw`. This survives pod restarts
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
  name: azureclaw-headlamp-plugin
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
      "--field-manager=azureclaw-cli",
      "-f",
      "-",
    ],
    { input: cmYaml, stdio: ["pipe", "inherit", "inherit"] },
  );

  // Patch the Headlamp Deployment to add the ConfigMap as a volume +
  // mount it at /headlamp/plugins/azureclaw. Use strategic merge patch
  // so we don't clobber existing volumes/mounts.
  //
  // NB: 'plugins' on the chart is at /build/plugins (the in-image
  // shipped plugins dir). User plugins go to /headlamp-plugins —
  // discoverable via the chart's --plugins-dir arg. To stay
  // compatible with both layouts we patch the container's args
  // explicitly to point at our mount, then mount our CM on top.
  //
  // Simpler approach: mount the CM at /headlamp-plugins/azureclaw
  // and rewrite the -plugins-dir arg to /headlamp-plugins.
  const patch = JSON.stringify({
    spec: {
      template: {
        spec: {
          volumes: [
            {
              name: "azureclaw-plugin",
              configMap: { name: "azureclaw-headlamp-plugin" },
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
                  name: "azureclaw-plugin",
                  mountPath: "/headlamp-plugins/azureclaw",
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
 * `azureclaw dev down --target local-k8s` (Phase 6).
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
