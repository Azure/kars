// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { agentContainerName, runtimeKindFromCr } from "../runtime.js";
import type { Execute } from "./deployment-target.js";
import { runtimeTarget, type PushedImage } from "./image-targets.js";
import { readDeployment, requireHealthyDeployment, type DeploymentRecord } from "./core-image-apply.js";

interface SandboxRecord {
  metadata: { name: string; namespace: string };
  spec?: {
    runtime?: Record<string, unknown> & { kind?: string };
    openclaw?: { image?: string };
    upstreamCompatibility?: { sigsAgentSandbox?: string };
    suspended?: boolean;
  };
}
export interface SandboxImagePlan {
  cr: SandboxRecord;
  deployment: DeploymentRecord;
  expected: Array<{ container: string; init: boolean; image: string }>;
  pinned: boolean;
}

export function planSandboxImages(cr: SandboxRecord, deployment: DeploymentRecord, images: PushedImage[]): SandboxImagePlan {
  const expected: SandboxImagePlan["expected"] = [];
  let pinned = false;
  if (cr.spec?.upstreamCompatibility?.sigsAgentSandbox === "overlay") return { cr, deployment, expected, pinned: true };
  const kind = runtimeKindFromCr(cr);
  for (const item of images) {
    if (item.name === "router") expected.push({ container: "inference-router", init: false, image: item.image });
    if (item.name === "sandbox") {
      if (deployment.spec.template.spec.initContainers?.some(container => container.name === "egress-guard")) {
        expected.push({ container: "egress-guard", init: true, image: item.image });
      }
      if (kind === "OpenClaw") {
        const config = cr.spec?.runtime?.openclaw as { image?: unknown } | undefined;
        if (config?.image != null || cr.spec?.openclaw?.image != null) pinned = true;
        else expected.push({ container: agentContainerName(kind), init: false, image: item.image });
      }
    }
    const runtime = runtimeTarget(item.name);
    if (runtime && runtime.kind === kind) {
      const config = cr.spec?.runtime?.[runtime.variant] as { image?: unknown; language?: string } | undefined;
      if (runtime.language && (config?.language ?? "python") !== runtime.language) continue;
      if (config?.image != null) pinned = true;
      else expected.push({ container: agentContainerName(kind), init: false, image: item.image });
    }
  }
  for (const item of expected) {
    const containers = item.init ? deployment.spec.template.spec.initContainers : deployment.spec.template.spec.containers;
    if (!containers?.some(container => container.name === item.container)) throw new Error(`Eligible workload lacks '${item.container}'`);
  }
  return { cr, deployment, expected, pinned };
}

export async function inspectSandboxPlans(execute: Execute, images: PushedImage[]): Promise<SandboxImagePlan[]> {
  if (!images.some(item => item.name === "router" || item.name === "sandbox" || runtimeTarget(item.name))) return [];
  const [deployments, sandboxes] = await Promise.all([
    execute("kubectl", ["get", "deployments", "-A", "-l", "kars.azure.com/component=sandbox", "-o", "json"], { stdio: "pipe" }),
    execute("kubectl", ["get", "karssandboxes", "-A", "-o", "json"], { stdio: "pipe" }),
  ]);
  const list = JSON.parse(String(deployments.stdout)) as { items?: DeploymentRecord[] };
  const crs = JSON.parse(String(sandboxes.stdout)) as { items?: SandboxRecord[] };
  if (!Array.isArray(list.items) || !Array.isArray(crs.items)) throw new Error("Cannot verify sandbox image overrides");
  return list.items.map(deployment => {
    const labels = deployment.metadata?.labels ?? {};
    const name = labels["kars.azure.com/sandbox"] || deployment.metadata?.name;
    const namespace = labels["kars.azure.com/parent-namespace"];
    const candidates = crs.items!.filter(cr => cr.metadata?.name === name && (!namespace || cr.metadata.namespace === namespace));
    if (candidates.length !== 1 || !deployment.metadata.namespace) throw new Error(`Cannot identify the owning CR for sandbox deployment ${name}`);
    return planSandboxImages(candidates[0], deployment, images);
  });
}

/** Let the controller materialize its new defaults from each unmodified CR.
 * Only metadata is touched, never a customer's explicit spec image pin. */
export async function refreshSandboxImages(execute: Execute, plans: SandboxImagePlan[]): Promise<number> {
  let updated = 0;
  for (const plan of plans.filter(item => item.expected.length > 0)) {
    const { cr, deployment } = plan;
    // Re-read authority before triggering reconciliation; a concurrently added
    // custom pin must be preserved, not patched back to a new default.
    const { stdout } = await execute("kubectl", ["get", "karssandbox", cr.metadata.name, "-n", cr.metadata.namespace, "-o", "json"], { stdio: "pipe" });
    const current = JSON.parse(String(stdout)) as SandboxRecord;
    if (JSON.stringify(current.spec) !== JSON.stringify(cr.spec)) throw new Error(`Sandbox ${cr.metadata.name} changed during image preparation; retry with its current overrides`);
    await execute("kubectl", ["annotate", "karssandbox", cr.metadata.name, "-n", cr.metadata.namespace,
      `kars.azure.com/image-refresh=${Date.now()}`, "--overwrite"], { stdio: "pipe" });
    for (const item of plan.expected) {
      const containers = item.init ? "initContainers" : "containers";
      await execute("kubectl", ["wait", `deployment/${deployment.metadata.name}`, "-n", deployment.metadata.namespace,
        `--for=jsonpath={.spec.template.spec.${containers}[?(@.name=="${item.container}")].image}=${item.image}`,
        "--timeout=180s"], { stdio: "pipe" });
    }
    if ((deployment.spec.replicas ?? 1) > 0) {
      await execute("kubectl", ["rollout", "restart", `deployment/${deployment.metadata.name}`, "-n", deployment.metadata.namespace], { stdio: "pipe" });
      await execute("kubectl", ["rollout", "status", `deployment/${deployment.metadata.name}`, "-n", deployment.metadata.namespace, "--timeout=300s"], { stdio: "pipe" });
    }
    const actual = await readDeployment(execute, deployment.metadata.name, deployment.metadata.namespace);
    for (const item of plan.expected) {
      const containers = item.init ? actual.spec.template.spec.initContainers : actual.spec.template.spec.containers;
      if (containers?.find(container => container.name === item.container)?.image !== item.image) {
        throw new Error(`Sandbox ${cr.metadata.name}/${item.container} did not converge to the selected artifact`);
      }
    }
    if ((deployment.spec.replicas ?? 1) > 0) requireHealthyDeployment(actual);
    updated++;
  }
  return updated;
}
