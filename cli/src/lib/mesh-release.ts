// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";

export const MESH_NAMESPACE = "agentmesh";
export type MeshComponent = "registry" | "relay";
export type MeshImages = Partial<Record<MeshComponent, string>>;
interface MeshDeployment { component: MeshComponent; name: string; container: string; image: string }
export type MeshInstallation = {
  kind: "absent" | "legacy";
  namespace: string;
  deployments: MeshDeployment[];
} | {
  kind: "helm";
  namespace: string;
  deployments: MeshDeployment[];
  release: string;
  releaseNamespace: string;
  values: Record<string, unknown>;
};

interface Resource {
  kind?: string;
  metadata?: { name?: string; annotations?: Record<string, string>; labels?: Record<string, string>; generation?: number; ownerReferences?: unknown[] };
  spec?: { replicas?: number; template?: { spec?: { containers?: Array<{ name?: string; image?: string }> } } };
  status?: { observedGeneration?: number; readyReplicas?: number; updatedReplicas?: number; conditions?: Array<{ type?: string; status?: string }> };
}

function parseObject(raw: string, label: string): Record<string, unknown> {
  const parsed: unknown = JSON.parse(raw);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(`${label} returned an invalid object`);
  return parsed as Record<string, unknown>;
}

export async function readReleaseValues(execute: Execute, release = "kars", namespace = "kars-system"): Promise<Record<string, unknown>> {
  const { stdout } = await execute("helm", ["get", "values", release, "-n", namespace, "--all", "-o", "json"], { stdio: "pipe" });
  return String(stdout).trim() === "null" ? {} : parseObject(String(stdout), "Helm values");
}

export function helmMeshEnabled(values: Record<string, unknown>): boolean {
  if (values.agentMesh == null) return false;
  if (typeof values.agentMesh !== "object" || Array.isArray(values.agentMesh)) throw new Error("agentMesh values must be a map");
  const mesh = values.agentMesh as Record<string, unknown>;
  if (mesh.enabled != null && typeof mesh.enabled !== "boolean") throw new Error("agentMesh.enabled must be boolean");
  return mesh.enabled === true;
}

function componentOf(resource: Resource): MeshComponent | undefined {
  const name = resource.metadata?.name;
  for (const component of ["registry", "relay"] as const) {
    if (resource.kind === "Deployment" && (name === component || name === `agentmesh-${component}`)) return component;
    if (resource.kind === "Service" && name === `agentmesh-${component}`) return component;
  }
  return undefined;
}

/** Inspect Kubernetes ownership first. Existing unmanaged installations do not
 * require a Helm binary or a Helm release merely to refresh their images. */
export async function inspectMeshInstallation(execute: Execute): Promise<MeshInstallation> {
  let raw: string;
  try {
    raw = String((await execute("kubectl", [
      "get", "deployment,service", "-n", MESH_NAMESPACE, "-o", "json",
    ], { stdio: "pipe" })).stdout);
  } catch (error) {
    const text = `${(error as { stderr?: string }).stderr ?? ""}\n${String(error)}`;
    if (/\(NotFound\).*namespaces?\s+"agentmesh"\s+not found/.test(text)) {
      return { kind: "absent", namespace: MESH_NAMESPACE, deployments: [] };
    }
    throw error;
  }
  const list = parseObject(raw, "AgentMesh inventory");
  if (!Array.isArray(list.items)) throw new Error("AgentMesh inventory has no items array");
  const resources = (list.items as Resource[]).filter(resource => componentOf(resource));
  if (!resources.length) return { kind: "absent", namespace: MESH_NAMESPACE, deployments: [] };
  if (resources.some(resource => resource.metadata?.ownerReferences?.length)) {
    throw new Error("AgentMesh resources have another Kubernetes owner; refusing an unmanaged update.");
  }
  const deployments: MeshDeployment[] = [];
  for (const component of ["registry", "relay"] as const) {
    const matching = resources.filter(resource => componentOf(resource) === component);
    const workload = matching.filter(resource => resource.kind === "Deployment");
    const services = matching.filter(resource => resource.kind === "Service");
    if (workload.length !== 1 || services.length !== 1) {
      throw new Error(`AgentMesh ${component} has incomplete or ambiguous deployment/service ownership; refusing to adopt or replace selectors.`);
    }
    const containers = workload[0].spec?.template?.spec?.containers ?? [];
    const container = containers.find(item => item.name === component || item.name === `agentmesh-${component}`)
      ?? (containers.length === 1 ? containers[0] : undefined);
    if (!container?.name || !container.image) throw new Error(`Cannot identify AgentMesh ${component} container`);
    deployments.push({ component, name: workload[0].metadata!.name!, container: container.name, image: container.image });
  }
  const owners = resources.map(resource => ({
    release: resource.metadata?.annotations?.["meta.helm.sh/release-name"],
    namespace: resource.metadata?.annotations?.["meta.helm.sh/release-namespace"],
    manager: resource.metadata?.labels?.["app.kubernetes.io/managed-by"],
  }));
  if (owners.every(owner => !owner.release && !owner.namespace && !owner.manager)) {
    return { kind: "legacy", namespace: MESH_NAMESPACE, deployments };
  }
  const owner = owners[0];
  if (!owner.release || !owner.namespace || owners.some(item =>
    item.release !== owner.release || item.namespace !== owner.namespace || item.manager !== "Helm")) {
    throw new Error("AgentMesh has mixed or incomplete Helm ownership; refusing selector adoption.");
  }
  const { stdout } = await execute("helm", ["list", "-n", owner.namespace, "-o", "json"], { stdio: "pipe" });
  const releases: unknown = JSON.parse(String(stdout));
  if (!Array.isArray(releases) || !releases.some(item =>
    item?.name === owner.release && typeof item.chart === "string" && item.chart.startsWith("kars-"))) {
    throw new Error("AgentMesh is not owned by a verified Kars Helm release");
  }
  const values = await readReleaseValues(execute, owner.release, owner.namespace);
  if (!helmMeshEnabled(values)) throw new Error("AgentMesh Helm ownership conflicts with agentMesh.enabled; refusing an unsafe update");
  const namespace = (values.agentMesh as Record<string, unknown>).namespace;
  if (namespace != null && namespace !== MESH_NAMESPACE) throw new Error("Only the agentmesh namespace is supported");
  return { kind: "helm", namespace: MESH_NAMESPACE, deployments, release: owner.release, releaseNamespace: owner.namespace, values };
}

export function assertMeshReleaseConsistency(mesh: MeshInstallation, values: Record<string, unknown>): void {
  const enabled = helmMeshEnabled(values);
  if (enabled !== (mesh.kind === "helm")
    || (mesh.kind === "helm" && (mesh.release !== "kars" || mesh.releaseNamespace !== "kars-system"))) {
    throw new Error("Kars Helm mesh values do not match actual deployment ownership; refusing automatic adoption or ownership changes.");
  }
}

export async function recheckMeshOwnership(execute: Execute, expected: MeshInstallation): Promise<void> {
  const current = await inspectMeshInstallation(execute);
  const identity = (mesh: MeshInstallation) => JSON.stringify({
    kind: mesh.kind, namespace: mesh.namespace,
    release: mesh.kind === "helm" ? [mesh.releaseNamespace, mesh.release] : null,
    deployments: mesh.deployments.map(({ name, container }) => [name, container]),
  });
  if (identity(current) !== identity(expected)) {
    throw new Error("AgentMesh ownership changed during preparation; update stopped before modifying its resources.");
  }
}

export function meshImageValueArgs(images: MeshImages): string[] {
  const args: string[] = [];
  for (const component of ["registry", "relay"] as const) {
    const image = images[component];
    if (!image) continue;
    const colon = image.lastIndexOf(":");
    if (image.includes("@") || colon <= image.lastIndexOf("/") || colon === image.length - 1) {
      throw new Error(`AgentMesh ${component} requires the actual repository:tag artifact`);
    }
    args.push("--set-string", `agentMesh.${component}.image.repository=${image.slice(0, colon)}`,
      "--set-string", `agentMesh.${component}.image.tag=${image.slice(colon + 1)}`);
  }
  return args;
}

export function releaseMeshImages(registry: string, tag: string): MeshImages {
  return { registry: `${registry}/agentmesh-registry-agt:${tag}`, relay: `${registry}/agentmesh-relay-agt:${tag}` };
}

export async function restartMesh(execute: Execute, mesh: MeshInstallation, components?: MeshComponent[]): Promise<void> {
  for (const deployment of mesh.deployments.filter(item => !components || components.includes(item.component))) {
    await execute("kubectl", ["rollout", "restart", `deployment/${deployment.name}`, "-n", mesh.namespace], { stdio: "pipe" });
    await execute("kubectl", ["rollout", "status", `deployment/${deployment.name}`, "-n", mesh.namespace, "--timeout=180s"], { stdio: "pipe" });
  }
}

export async function verifyMeshHealth(execute: Execute, mesh: MeshInstallation, expected: MeshImages = {}): Promise<void> {
  for (const deployment of mesh.deployments) {
    const { stdout } = await execute("kubectl", ["get", "deployment", deployment.name, "-n", mesh.namespace, "-o", "json"], { stdio: "pipe" });
    const resource = parseObject(String(stdout), `AgentMesh ${deployment.name}`) as Resource;
    const replicas = resource.spec?.replicas ?? 1;
    if (replicas < 1 || (resource.status?.readyReplicas ?? 0) < replicas
      || (resource.status?.updatedReplicas ?? 0) < replicas
      || (resource.status?.observedGeneration ?? -1) < (resource.metadata?.generation ?? 0)
      || !resource.status?.conditions?.some(condition => condition.type === "Available" && condition.status === "True")) {
      throw new Error(`AgentMesh deployment '${deployment.name}' has not completed a healthy rollout`);
    }
    const image = resource.spec?.template?.spec?.containers?.find(item => item.name === deployment.container)?.image;
    if (expected[deployment.component] && image !== expected[deployment.component]) {
      throw new Error(`AgentMesh ${deployment.name} is not running the selected image artifact`);
    }
  }
}

export async function updateLegacyMeshImages(execute: Execute, mesh: MeshInstallation, images: MeshImages): Promise<void> {
  if (mesh.kind !== "legacy") throw new Error("Direct image updates require an existing unmanaged AgentMesh installation");
  for (const deployment of mesh.deployments) {
    if (images[deployment.component]) {
      await execute("kubectl", ["set", "image", `deployment/${deployment.name}`,
        `${deployment.container}=${images[deployment.component]}`, "-n", mesh.namespace], { stdio: "pipe" });
    }
  }
}

export async function applyMeshImages(execute: Execute, mesh: MeshInstallation, images: MeshImages, chart: string): Promise<void> {
  if (mesh.kind === "absent") throw new Error("AgentMesh is not installed; install it explicitly before pushing mesh updates");
  await recheckMeshOwnership(execute, mesh);
  if (mesh.kind === "helm") {
    await execute("helm", ["upgrade", mesh.release, chart, "--namespace", mesh.releaseNamespace,
      "--reuse-values", ...meshImageValueArgs(images), "--atomic", "--wait", "--timeout", "8m"], { stdio: "pipe" });
  } else {
    await updateLegacyMeshImages(execute, mesh, images);
  }
  const components = (["registry", "relay"] as const).filter(component => images[component]);
  await restartMesh(execute, mesh, [...components]);
  await verifyMeshHealth(execute, mesh, images);
}
