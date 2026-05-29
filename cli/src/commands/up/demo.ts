// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `kars up --demo` — recording-friendly walkthrough mode.
 *
 * Walks through every phase a real `kars up` would execute, using
 * read-only queries against the current Azure subscription + kubeconfig
 * context. Stepper visuals are identical to a live run; nothing is
 * created, modified, or deleted.
 *
 * Designed for capturing demo videos without burning 20 min on a real
 * provision and without accumulating disposable Azure resources.
 *
 * Each step uses the SAME `Stepper` helpers a real run uses so the
 * recording matches what an operator would see during a normal
 * `kars up`. Steps that would mutate (`az group create`, helm install,
 * etc.) are replaced with a real read counterpart (`az group show`,
 * `helm list`) that produces a believable result line — present vs.
 * absent — and a small simulated pause so the recording doesn't
 * race through every step in milliseconds.
 *
 * Falls back to a synthetic "would-create" line when the real read
 * fails (e.g. resource group doesn't exist, helm release missing) so
 * the demo still has content to show.
 */

import { execa } from "execa";
import chalk from "chalk";
import { Stepper } from "../../stepper.js";

interface DemoOptions {
  name: string;
  model: string;
  region: string;
  clusterName: string;
  isolation: string;
  resourceGroup?: string;
  sourceAcr: string;
  meshTrust?: string;
  meshPeer?: boolean;
}

/**
 * Sleep helper with a default 600ms pause so the recording has a
 * natural rhythm between step transitions.
 */
async function pause(ms = 600): Promise<void> {
  await new Promise((r) => setTimeout(r, ms));
}

/** Run an az/kubectl read; return parsed JSON or `null` on failure. */
async function readJson<T>(cmd: string, args: string[]): Promise<T | null> {
  try {
    const { stdout } = await execa(cmd, args, { stdio: "pipe", timeout: 8000 });
    if (!stdout.trim() || stdout.trim() === "null") return null;
    return JSON.parse(stdout) as T;
  } catch {
    return null;
  }
}

/**
 * Print a phase header (italic, indented) before the next group of
 * stepper calls. Mirrors the visual rhythm of `kars up` where each
 * major phase has a banner above its detail rows.
 */
function phase(title: string): void {
  console.log();
  console.log(chalk.bold.cyan(`  ▸ ${title}`));
}

export async function runUpDemo(options: DemoOptions): Promise<void> {
  const baseName = options.clusterName.replace(/-aks$/, "");
  const rg = options.resourceGroup ?? `${baseName}-${options.region}`;

  console.log();
  console.log(chalk.bold.blue("  kars up — DEMO MODE"));
  console.log(chalk.dim("  Read-only walkthrough using your current Azure + kubeconfig context."));
  console.log(chalk.dim("  Nothing will be created, modified, or deleted.\n"));
  await pause(400);

  // 7 logical phases; if --mesh-trust=entra adds an extra check inside
  // phase 6 the count is the same.
  const stepper = new Stepper({ totalSteps: 7 });

  // ── Phase 1: preflight ───────────────────────────────────────────────
  phase("Preflight: tools + Azure auth");
  stepper.step("Checking required CLIs (az, kubectl, helm, docker)...");
  for (const tool of ["az", "kubectl", "helm", "docker"]) {
    const ok = await execa("which", [tool], { stdio: "pipe" }).then(() => true).catch(() => false);
    stepper.detail(ok ? "ok" : "skip", tool);
    await pause(120);
  }
  const account = await readJson<{ name: string; tenantId: string; id: string; user: { name: string } }>(
    "az", ["account", "show", "-o", "json"],
  );
  if (account) {
    stepper.detail("ok", `subscription: ${account.name} (${account.id.slice(0, 8)}…)`);
    stepper.detail("ok", `user: ${account.user.name}`);
    stepper.detail("ok", `tenant: ${account.tenantId.slice(0, 8)}…`);
  } else {
    stepper.detail("info", "not logged in — would prompt for `az login`");
  }
  stepper.done("Preflight passed");
  await pause(500);

  // ── Phase 2: resource group ──────────────────────────────────────────
  phase(`Resource group '${rg}'`);
  stepper.step(`Checking ${rg} in ${options.region}...`);
  const exists = await readJson<{ location: string; provisioningState: string }>(
    "az", ["group", "show", "-n", rg, "-o", "json"],
  );
  if (exists) {
    stepper.detail("ok", `exists (state=${exists.provisioningState}, region=${exists.location})`);
    stepper.done("Resource group ready");
  } else {
    stepper.detail("new", `would create in ${options.region}`);
    stepper.done("Resource group would be created");
  }
  await pause(500);

  // ── Phase 3: infrastructure ──────────────────────────────────────────
  phase("Infrastructure: AKS + ACR + KV + Foundry + Monitor + WI");
  stepper.step("Checking AKS cluster...");
  const aks = await readJson<{ name: string; provisioningState: string; agentPoolProfiles: { count: number }[] }>(
    "az", ["aks", "show", "-g", rg, "-n", options.clusterName, "-o", "json"],
  );
  if (aks) {
    const nodes = aks.agentPoolProfiles?.reduce((n, p) => n + (p.count || 0), 0) ?? 0;
    stepper.detail("ok", `${aks.name} (${aks.provisioningState}, ${nodes} nodes)`);
  } else {
    stepper.detail("new", `would deploy AKS '${options.clusterName}' via Bicep`);
  }
  await pause(400);

  const acr = await readJson<{ name: string; loginServer: string }[]>(
    "az", ["acr", "list", "-g", rg, "-o", "json"],
  );
  if (acr && acr[0]) {
    stepper.detail("ok", `ACR: ${acr[0].name}`);
  } else {
    stepper.detail("new", `would create ACR + push images from ${options.sourceAcr}`);
  }
  await pause(400);

  const foundry = await readJson<{ name: string; kind: string }[]>(
    "az", ["cognitiveservices", "account", "list", "-g", rg, "-o", "json"],
  );
  const fc = foundry?.find((c) => c?.kind === "AIServices");
  if (fc) {
    stepper.detail("ok", `Foundry: ${fc.name} (AI Services)`);
  } else {
    stepper.detail("new", `would create Foundry account + deploy ${options.model}`);
  }
  stepper.done("Infrastructure check complete");
  await pause(500);

  // ── Phase 4: cluster connectivity ────────────────────────────────────
  phase("Cluster: kubeconfig + Helm releases");
  stepper.step("Checking kubeconfig context...");
  const ctx = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" })
    .then((r) => r.stdout.trim()).catch(() => "");
  stepper.detail(ctx ? "ok" : "info", ctx || "no current-context — would set via `az aks get-credentials`");
  await pause(300);

  const releases = await readJson<{ name: string; status: string }[]>(
    "helm", ["list", "-n", "kars-system", "-o", "json"],
  );
  if (releases && releases.length > 0) {
    for (const r of releases) {
      stepper.detail("ok", `helm: ${r.name} (${r.status})`);
    }
  } else {
    stepper.detail("new", "would install kars Helm chart (CRDs + controller + RBAC)");
  }
  stepper.done("Cluster ready");
  await pause(500);

  // ── Phase 5: agentmesh ──────────────────────────────────────────────
  phase("AgentMesh relay + registry");
  stepper.step("Checking agentmesh namespace...");
  const pods = await readJson<{ items: { metadata: { name: string }; status: { phase: string } }[] }>(
    "kubectl", ["get", "pods", "-n", "agentmesh", "-o", "json"],
  );
  if (pods?.items?.length) {
    for (const p of pods.items) {
      stepper.detail("ok", `${p.metadata.name}: ${p.status.phase}`);
    }
  } else {
    stepper.detail("new", "would deploy relay + registry from agentmesh-agt.yaml");
  }
  stepper.done("AgentMesh ready");
  await pause(500);

  // ── Phase 6: mesh trust ──────────────────────────────────────────────
  if (options.meshTrust === "entra") {
    phase("Mesh trust: Entra Agent ID (--mesh-trust=entra)");
    stepper.step("Checking KarsAuthConfig/default...");
    const kac = await readJson<{ spec: { agentId: { blueprintClientId: string }; meshAuthBackend?: string; tenant: { tenantId: string } } }>(
      "kubectl", ["get", "karsauthconfig", "default", "-o", "json"],
    );
    if (kac) {
      stepper.detail("ok", `blueprint=${kac.spec.agentId.blueprintClientId.slice(0, 8)}…`);
      stepper.detail("ok", `tenant=${kac.spec.tenant.tenantId.slice(0, 8)}…`);
      stepper.detail("ok", `meshAuthBackend=${kac.spec.meshAuthBackend ?? "<unset, defaults to Anonymous>"}`);
    } else {
      stepper.detail("new", "would provision tenant-wide Entra Agent Identity blueprint");
      stepper.detail("new", "would create per-sandbox SP-as-FIC pattern");
      stepper.detail("new", "would write KarsAuthConfig/default with meshAuthBackend=EntraAgentIdentity");
    }
    await pause(400);

    const relay = await readJson<{ spec: { template: { spec: { containers: { env?: { name: string; value?: string }[] }[] } } } }>(
      "kubectl", ["get", "deploy", "relay", "-n", "agentmesh", "-o", "json"],
    );
    const relayEnv = relay?.spec.template.spec.containers[0]?.env ?? [];
    const aud = relayEnv.find((e) => e.name === "AGENTMESH_ENTRA_AUDIENCE")?.value;
    if (aud) {
      stepper.detail("ok", `relay AGENTMESH_ENTRA_AUDIENCE = ${aud.slice(0, 8)}…`);
    } else {
      stepper.detail("new", "would patch relay+registry with AGENTMESH_ENTRA_AUDIENCE/_TENANT_ID");
    }
    stepper.done("Entra Agent ID trust ready");
  } else {
    phase("Mesh trust: anonymous (default)");
    stepper.step("--mesh-trust=anonymous — skipping Entra Agent ID provisioning");
    stepper.detail("ok", "registry + relay run unverified");
    stepper.detail("ok", "sandbox uses shared cluster MI for Foundry");
    stepper.done("Anonymous-tier ready (zero Entra prerequisites)");
  }
  await pause(500);

  // ── Phase 7: sandbox ────────────────────────────────────────────────
  phase(`Sandbox '${options.name}'`);
  stepper.step(`Checking KarsSandbox/${options.name}...`);
  const sb = await readJson<{ spec: { runtime: { kind: string } }; status?: { phase?: string; agentIdentity?: { appId: string } } }>(
    "kubectl", ["get", "karssandbox", options.name, "-n", "kars-system", "-o", "json"],
  );
  if (sb) {
    stepper.detail("ok", `phase=${sb.status?.phase ?? "?"} runtime=${sb.spec.runtime.kind}`);
    if (sb.status?.agentIdentity?.appId) {
      stepper.detail("ok", `agentIdentity=${sb.status.agentIdentity.appId.slice(0, 8)}…`);
    }
  } else {
    stepper.detail("new", `would create KarsSandbox '${options.name}' (isolation=${options.isolation}, model=${options.model})`);
    stepper.detail("new", "controller would provision per-sandbox SA + Service + NetworkPolicy + Deployment");
    if (options.meshTrust === "entra") {
      stepper.detail("new", "controller would provision Entra Agent Identity SP + RBAC + federated credential");
    }
  }
  stepper.done("Sandbox provisioning complete");
  await pause(500);

  stepper.summary();

  console.log();
  console.log(chalk.bold.blue("  Demo walkthrough complete."));
  console.log(chalk.dim("  In a real `kars up` this would have taken ~10-20 minutes."));
  console.log(chalk.dim("  Drop --demo to execute for real.\n"));
}
