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
import { existsSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
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

interface Tooling {
  kind: string;
  kubectl: string;
  helm: string;
  docker: string;
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

async function ensureTooling(): Promise<Tooling> {
  // Resolved up front so we fail with one actionable error per missing
  // dependency, instead of an opaque ENOENT mid-bringup.
  const [kind, kubectl, helm, docker] = await Promise.all([
    which("kind"),
    which("kubectl"),
    which("helm"),
    which("docker"),
  ]);
  return { kind, kubectl, helm, docker };
}

async function clusterExists(kind: string, name: string): Promise<boolean> {
  const { stdout } = await execa(kind, ["get", "clusters"]);
  return stdout.split(/\r?\n/).map((s) => s.trim()).includes(name);
}

async function ensureCluster(kind: string, name: string): Promise<void> {
  if (await clusterExists(kind, name)) return;
  // The default kindest/node image is fine for our use case; pinning is
  // a Phase-2 hardening concern alongside the values-local-dev overlay.
  await execa(kind, ["create", "cluster", "--name", name], {
    stdio: "inherit",
  });
}

async function loadImageIntoKind(
  kind: string,
  clusterName: string,
  image: string,
): Promise<void> {
  // `kind load docker-image` has a known issue where it can silently fail
  // to surface an image into the node's containerd (the import succeeds at
  // the kind layer but `crictl images` doesn't show it — observed on
  // multiple OS/arch combos and tracked in kind#3795). We use it as the
  // primary path, then verify by piping a `docker save` straight into
  // the node's `ctr` as a fallback. The fallback is idempotent.
  try {
    await execa(
      kind,
      ["load", "docker-image", image, "--name", clusterName],
      { stdio: "inherit" },
    );
  } catch {
    // fall through to the ctr import path
  }

  // Verify the image is on the node; if not, push it via ctr.
  const node = `${clusterName}-control-plane`;
  const present = await execa("docker", [
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

  const save = execa("docker", ["save", image]);
  const importProc = execa(
    "docker",
    ["exec", "-i", node, "ctr", "-n=k8s.io", "images", "import", "-"],
    { stdio: ["pipe", "inherit", "inherit"] },
  );
  if (save.stdout && importProc.stdin) save.stdout.pipe(importProc.stdin);
  await Promise.all([save, importProc]);
}

async function dockerImageExists(docker: string, image: string): Promise<boolean> {
  try {
    await execa(docker, ["image", "inspect", image]);
    return true;
  } catch {
    return false;
  }
}

async function loadImageIfPresent(
  kind: string,
  docker: string,
  clusterName: string,
  /** Desired tag inside kind (matches values-local-dev.yaml). */
  targetImage: string,
  /** Fallback tags to retag-from if `targetImage` itself isn't local. */
  candidateAliases: string[] = [],
): Promise<{ loaded: boolean; reason?: string }> {
  const tryLoad = async (img: string): Promise<boolean> => {
    if (!(await dockerImageExists(docker, img))) return false;
    if (img !== targetImage) {
      // Retag to the canonical name so values-local-dev.yaml's
      // `imagePullPolicy: Never` finds it.
      await execa(docker, ["tag", img, targetImage]);
    }
    await loadImageIntoKind(kind, clusterName, targetImage);
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
  const stepper = new Stepper({ totalSteps: 6 });

  stepper.step("Checking local tooling (kind / kubectl / helm / docker)…");
  const tools = await ensureTooling();
  stepper.done(
    `tooling ready: ${path.basename(tools.kind)}, ${path.basename(tools.kubectl)}, ${path.basename(tools.helm)}`,
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
  await ensureCluster(tools.kind, opts.clusterName);
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
        tools.docker,
        opts.clusterName,
        img.target,
        img.aliases,
      );
      if (!result.loaded) {
        missing.push(result.reason ?? img.target);
      }
    }
    if (missing.length > 0) {
      console.warn(
        chalk.yellow(
          `  ⚠ some images missing from local docker; the deployment will fail until you build them:\n     - ${missing.join("\n     - ")}\n     Hint: 'make images' or 'make build && make images' from repo root.`,
        ),
      );
    }
    stepper.done(`loaded ${images.length - missing.length}/${images.length} images`);
  }

  // Sandboxes are scheduled with `nodeSelector: azureclaw.azure.com/pool=sandbox`
  // and a matching toleration. On a single-node kind cluster we apply the
  // label + taint to the control-plane node so the scheduler picks it.
  // (Production AKS uses a dedicated sandbox node pool — same selector,
  // different mechanism.)
  try {
    const node = `${opts.clusterName}-control-plane`;
    await execa(tools.kubectl, [
      "label",
      "node",
      node,
      "azureclaw.azure.com/pool=sandbox",
      "--overwrite",
    ]);
    await execa(tools.kubectl, [
      "taint",
      "node",
      node,
      "azureclaw.azure.com/sandbox=true:NoSchedule",
      "--overwrite",
    ]);
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

  console.log("");
  console.log(chalk.green("  ✓ Local-k8s dev environment is ready."));
  console.log("");
  console.log(chalk.bold("  Next steps:"));
  console.log(
    `    kubectl get pods -A --context kind-${opts.clusterName}`,
  );
  console.log(
    `    kubectl apply -f examples/basic-agent/clawsandbox.yaml -n azureclaw-${opts.name}`,
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
