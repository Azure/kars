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
import { existsSync } from "node:fs";
import { Stepper } from "../../stepper.js";

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
  // `kind load docker-image` is slow but safe — it streams the image tar
  // into every kind node. For Phase 1 we accept the latency; Phase 6
  // will add `--image-archive` batching for the full set.
  await execa(
    kind,
    ["load", "docker-image", image, "--name", clusterName],
    { stdio: "inherit" },
  );
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
): Promise<void> {
  // We render-then-apply (rather than `helm install`) to keep failures
  // visible: `kubectl apply -f -` shows precisely which resources didn't
  // accept admission. Phase 4 may switch to `helm install --atomic` once
  // CRDs and the values overlay are stable.
  const { stdout } = await execa(helm, [
    "template",
    release,
    chartDir,
    "--namespace",
    "azureclaw-system",
    "--set",
    "controller.image.pullPolicy=Never",
    "--set",
    "router.image.pullPolicy=Never",
    "--include-crds",
  ]);
  await execa(
    kubectl,
    ["apply", "-f", "-", "--server-side", "--force-conflicts"],
    {
      input: stdout,
      stdio: ["pipe", "inherit", "inherit"],
    },
  );
}

export async function runLocalK8s(opts: LocalK8sOptions): Promise<void> {
  const stepper = new Stepper({ totalSteps: 5 });

  stepper.step("Checking local tooling (kind / kubectl / helm / docker)…");
  const tools = await ensureTooling();
  stepper.done(
    `tooling ready: ${path.basename(tools.kind)}, ${path.basename(tools.kubectl)}, ${path.basename(tools.helm)}`,
  );

  stepper.step(`Ensuring kind cluster '${opts.clusterName}' exists…`);
  await ensureCluster(tools.kind, opts.clusterName);
  stepper.done(`kind cluster '${opts.clusterName}' is ready`);

  stepper.step(`Loading sandbox image '${opts.image}' into the cluster…`);
  if (opts.noBuild) {
    stepper.done("skipped image load (--no-build)");
  } else {
    await loadImageIntoKind(tools.kind, opts.clusterName, opts.image);
    stepper.done(`image '${opts.image}' loaded into kind`);
  }

  stepper.step("Helm-installing the AzureClaw chart…");
  const repoRoot = findRepoRoot(process.cwd());
  const chartDir = path.join(repoRoot, "deploy", "helm", "azureclaw");
  if (!existsSync(chartDir)) {
    throw new Error(`AzureClaw helm chart not found at ${chartDir}`);
  }
  // Ensure the namespace exists before applying namespaced resources.
  try {
    await execa(tools.kubectl, ["create", "namespace", "azureclaw-system"]);
  } catch {
    // Namespace already exists — proceed.
  }
  await helmInstall(tools.helm, tools.kubectl, opts.name, chartDir);
  stepper.done("chart applied");

  stepper.step("Verifying controller deployment is rolling out…");
  // Best-effort: don't block forever if the controller image isn't on the
  // node yet (Phase 1 just verifies the apply path). The user-facing exec
  // recipe below works as soon as a sandbox CR is created.
  try {
    await execa(
      tools.kubectl,
      [
        "rollout",
        "status",
        "deployment/azureclaw-controller",
        "-n",
        "azureclaw-system",
        "--timeout=60s",
      ],
      { stdio: "inherit" },
    );
  } catch {
    // Phase 1: surface the warning rather than failing — the rest of the
    // bringup may still succeed once images are properly pinned in Phase 2.
    console.warn(
      chalk.yellow(
        "  ⚠ controller deployment did not become ready within 60s — check 'kubectl describe deployment/azureclaw-controller -n azureclaw-system'.",
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
        `  --ephemeral: cluster will NOT be destroyed automatically in Phase 1.\n  Run 'kind delete cluster --name ${opts.clusterName}' when finished.`,
      ),
    );
  }
}
