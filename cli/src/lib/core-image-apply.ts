// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";
import { controllerEnv, coreImageValues, type PushedImage } from "./image-targets.js";
import { readReleaseValues } from "./mesh-release.js";

export interface DeploymentRecord {
  metadata: { name: string; namespace: string; resourceVersion?: string; generation?: number; labels?: Record<string, string>; annotations?: Record<string, string>; ownerReferences?: unknown[] };
  spec: { replicas?: number; template: { metadata?: { labels?: Record<string, string> }; spec: {
    containers: Array<{ name: string; image: string; imagePullPolicy?: string; env?: Array<{ name: string; value?: string; valueFrom?: unknown }> }>;
    initContainers?: Array<{ name: string; image: string; imagePullPolicy?: string }>;
  } } };
  status?: { observedGeneration?: number; readyReplicas?: number; updatedReplicas?: number; conditions?: Array<{ type: string; status: string }> };
}
export type CoreInstallation = { kind: "legacy"; deployment: DeploymentRecord; container: string }
  | { kind: "helm"; deployment: DeploymentRecord; container: string; release: string; releaseNamespace: string; values: Record<string, unknown> };

export async function readDeployment(execute: Execute, name: string, namespace: string): Promise<DeploymentRecord> {
  const { stdout } = await execute("kubectl", ["get", "deployment", name, "-n", namespace, "-o", "json"], { stdio: "pipe" });
  const deployment = JSON.parse(String(stdout)) as DeploymentRecord;
  if (deployment?.metadata?.name !== name || !Array.isArray(deployment.spec?.template?.spec?.containers)) {
    throw new Error(`Invalid deployment evidence for ${namespace}/${name}`);
  }
  return deployment;
}

export async function inspectCoreInstallation(execute: Execute): Promise<CoreInstallation> {
  const deployment = await readDeployment(execute, "kars-controller", "kars-system");
  const containers = deployment.spec.template.spec.containers;
  const container = containers.find(item => item.name === "controller" || item.name === "kars-controller")
    ?? (containers.length === 1 ? containers[0] : undefined);
  if (!container) throw new Error("Cannot identify the Kars controller container");
  const metadata = deployment.metadata;
  if (metadata.ownerReferences?.length) throw new Error("Kars controller has an external Kubernetes owner");
  const release = metadata.annotations?.["meta.helm.sh/release-name"];
  const releaseNamespace = metadata.annotations?.["meta.helm.sh/release-namespace"];
  const manager = metadata.labels?.["app.kubernetes.io/managed-by"];
  if (!release && !releaseNamespace && !manager) return { kind: "legacy", deployment, container: container.name };
  if (!release || !releaseNamespace || manager !== "Helm") throw new Error("Kars controller ownership is ambiguous");
  const { stdout } = await execute("helm", ["list", "-n", releaseNamespace, "-o", "json"], { stdio: "pipe" });
  const releases: unknown = JSON.parse(String(stdout));
  if (!Array.isArray(releases) || !releases.some(item => item?.name === release
    && typeof item.chart === "string" && item.chart.startsWith("kars-"))) throw new Error("Controller is not owned by a verified Kars release");
  return { kind: "helm", deployment, container: container.name, release, releaseNamespace,
    values: await readReleaseValues(execute, release, releaseNamespace) };
}

export async function recheckCoreOwnership(execute: Execute, core: CoreInstallation): Promise<void> {
  const current = await inspectCoreInstallation(execute);
  if (current.kind !== core.kind || current.container !== core.container
    || (core.kind === "helm" && (current.kind !== "helm" || current.release !== core.release || current.releaseNamespace !== core.releaseNamespace))) {
    throw new Error("Controller ownership changed during image preparation");
  }
}

export async function updateLegacyCore(execute: Execute, core: CoreInstallation, images: PushedImage[]): Promise<void> {
  if (core.kind !== "legacy") throw new Error("Direct controller updates require unmanaged ownership");
  const verified = await inspectCoreInstallation(execute);
  if (verified.kind !== "legacy") throw new Error("Controller ownership changed before its legacy update");
  const current = verified.deployment;
  if (!current.metadata.resourceVersion) throw new Error("Controller version is missing; refusing an unguarded update");
  const index = current.spec.template.spec.containers.findIndex(item => item.name === core.container);
  if (index < 0) throw new Error("Controller container changed");
  const container = current.spec.template.spec.containers[index];
  const env = [...(container.env ?? [])];
  const operations: unknown[] = [{ op: "test", path: "/metadata/resourceVersion", value: current.metadata.resourceVersion }];
  for (const item of images) {
    if (item.name === "controller") {
      operations.push({ op: "add", path: `/spec/template/spec/containers/${index}/image`, value: item.image },
        { op: "add", path: `/spec/template/spec/containers/${index}/imagePullPolicy`, value: "Always" });
    }
    const name = controllerEnv(item.name);
    if (name) {
      if (env.filter(value => value.name === name).length > 1) throw new Error(`Ambiguous controller environment ${name}`);
      const entry = env.findIndex(value => value.name === name);
      if (entry < 0) env.push({ name, value: item.image });
      else env[entry] = { name, value: item.image };
    }
  }
  if (images.some(item => controllerEnv(item.name))) operations.push({ op: "add", path: `/spec/template/spec/containers/${index}/env`, value: env });
  await execute("kubectl", ["patch", "deployment/kars-controller", "-n", "kars-system", "--type=json", "-p", JSON.stringify(operations)], { stdio: "pipe" });
}

export function valueAt(values: Record<string, unknown>, path: string): unknown {
  return path.split(".").reduce<unknown>((value, key) =>
    value && typeof value === "object" ? (value as Record<string, unknown>)[key] : undefined, values);
}

export async function verifyCoreConfiguration(execute: Execute, core: CoreInstallation, images: PushedImage[]): Promise<DeploymentRecord> {
  if (core.kind === "helm") {
    const values = await readReleaseValues(execute, core.release, core.releaseNamespace);
    for (const [key, value] of Object.entries(coreImageValues(images))) {
      if (valueAt(values, key) !== value) throw new Error(`Helm did not retain selected image configuration '${key}'`);
    }
  }
  const deployment = await readDeployment(execute, "kars-controller", "kars-system");
  const container = deployment.spec.template.spec.containers.find(item => item.name === core.container);
  if (!container) throw new Error("Controller container missing after update");
  for (const item of images) {
    if (item.name === "controller" && container.image !== item.image) throw new Error("Controller is not configured with the selected artifact");
    const envName = controllerEnv(item.name);
    if (envName) {
      const entries = container.env?.filter(entry => entry.name === envName) ?? [];
      if (entries.length !== 1 || entries[0].value !== item.image) throw new Error(`Controller default '${envName}' does not match the selected artifact`);
    }
  }
  return deployment;
}

export function requireHealthyDeployment(deployment: DeploymentRecord): void {
  const desired = deployment.spec.replicas ?? 1;
  if (desired < 1 || (deployment.status?.updatedReplicas ?? 0) < desired
    || (deployment.status?.readyReplicas ?? 0) < desired
    || (deployment.status?.observedGeneration ?? -1) < (deployment.metadata.generation ?? 0)
    || !deployment.status?.conditions?.some(condition => condition.type === "Available" && condition.status === "True")) {
    throw new Error(`Deployment ${deployment.metadata.namespace}/${deployment.metadata.name} is not healthy on its selected configuration`);
  }
}
