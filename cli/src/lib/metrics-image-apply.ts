// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";
import {
  inspectCoreInstallation, readDeployment, requireHealthyDeployment, valueAt,
  type CoreInstallation, type DeploymentRecord,
} from "./core-image-apply.js";

const NAME = "kars-metrics-server";
const OWNER = "kars.azure.com/metrics-owner";
export type MetricsInstallation =
  | { kind: "unmanaged"; reason: string }
  | { kind: "absent" }
  | { kind: "owned"; owner: string; deployment: DeploymentRecord };

export async function inspectMetricsInstallation(execute: Execute, core: CoreInstallation): Promise<MetricsInstallation> {
  if (core.kind === "helm" && valueAt(core.values, "metricsServer.enabled") === false) {
    return { kind: "unmanaged", reason: "metrics-server is disabled in the owning release" };
  }
  const { stdout } = await execute("kubectl", ["get", "apiservice", "v1beta1.metrics.k8s.io",
    "--ignore-not-found", "-o", "json"], { stdio: "pipe" });
  if (!String(stdout).trim()) return { kind: "absent" };
  const api = JSON.parse(String(stdout)) as {
    metadata?: { name?: string; annotations?: Record<string, string> };
    spec?: { service?: { name?: string; namespace?: string } };
  };
  if (api.metadata?.name !== "v1beta1.metrics.k8s.io") throw new Error("Invalid Metrics API ownership evidence");
  const owner = api.metadata.annotations?.[OWNER];
  const namespace = core.deployment.metadata.namespace;
  if (!owner || !owner.startsWith(`${namespace}/`) || owner === `${namespace}/`
    || api.spec?.service?.name !== NAME || api.spec.service.namespace !== namespace) {
    return { kind: "unmanaged", reason: "the cluster Metrics API is managed outside this Core installation" };
  }
  if (core.kind === "helm" && owner !== `${core.releaseNamespace}/${core.release}`) {
    throw new Error("Metrics API belongs to a different release");
  }
  function verifyHelmOwner(annotations: Record<string, string> = {}) {
    const name = annotations["meta.helm.sh/release-name"];
    const ns = annotations["meta.helm.sh/release-namespace"];
    if ((name || ns) && (core.kind !== "helm" || name !== core.release || ns !== core.releaseNamespace)) {
      throw new Error("Metrics resources have conflicting Helm ownership");
    }
  }
  verifyHelmOwner(api.metadata.annotations);
  const deployment = await readDeployment(execute, NAME, namespace);
  if (deployment.metadata.namespace !== namespace || deployment.metadata.ownerReferences?.length || deployment.metadata.annotations?.[OWNER] !== owner) {
    throw new Error("Metrics deployment ownership does not match its API registration");
  }
  verifyHelmOwner(deployment.metadata.annotations);
  return { kind: "owned", owner, deployment };
}

export async function updateLegacyMetricsImage(execute: Execute, core: CoreInstallation, image: string): Promise<void> {
  if (core.kind !== "legacy") throw new Error("Direct metrics updates require template-owned Core");
  const current = await inspectMetricsInstallation(execute, core);
  if (current.kind !== "owned") throw new Error("Install the Core metrics prerequisite before applying its image");
  const { deployment } = current;
  const version = deployment.metadata.resourceVersion;
  const index = deployment.spec.template.spec.containers.findIndex(item => item.name === "metrics-server");
  if (!version || index < 0) throw new Error("Metrics deployment version or container is missing");
  const operations = [
    { op: "test", path: "/metadata/resourceVersion", value: version },
    { op: "replace", path: `/spec/template/spec/containers/${index}/image`, value: image },
    { op: "add", path: `/spec/template/spec/containers/${index}/imagePullPolicy`, value: "Always" },
  ];
  await execute("kubectl", ["patch", `deployment/${NAME}`, "-n", deployment.metadata.namespace,
    "--type=json", "-p", JSON.stringify(operations)], { stdio: "pipe" });
  await execute("kubectl", ["rollout", "status", `deployment/${NAME}`, "-n", deployment.metadata.namespace,
    "--timeout=120s"], { stdio: "pipe" });
}

export async function verifyMetricsImage(execute: Execute, core: CoreInstallation, image: string): Promise<void> {
  const beforeWait = await inspectMetricsInstallation(execute, core);
  if (beforeWait.kind !== "owned") throw new Error("Selected metrics-server image has no owned installation");
  await execute("kubectl", ["wait", "apiservice/v1beta1.metrics.k8s.io", "--for=condition=Available",
    "--timeout=120s"], { stdio: "pipe" });
  const current = await inspectMetricsInstallation(execute, core);
  if (current.kind !== "owned") throw new Error("Selected metrics-server image has no owned installation");
  const container = current.deployment.spec.template.spec.containers.find(item => item.name === "metrics-server");
  if (container?.image !== image) throw new Error("Metrics-server does not use the selected artifact");
  requireHealthyDeployment(current.deployment);
}

/** Reacquire floating tags after an upgrade without touching external Metrics APIs. */
export async function restartOwnedMetrics(execute: Execute): Promise<void> {
  const core = await inspectCoreInstallation(execute);
  const metrics = await inspectMetricsInstallation(execute, core);
  if (metrics.kind !== "owned") return;
  const image = metrics.deployment.spec.template.spec.containers.find(item => item.name === "metrics-server")?.image;
  if (!image) throw new Error("Owned metrics deployment has no metrics-server image");
  const namespace = metrics.deployment.metadata.namespace;
  await execute("kubectl", ["rollout", "restart", `deployment/${NAME}`, "-n", namespace], { stdio: "pipe" });
  await execute("kubectl", ["rollout", "status", `deployment/${NAME}`, "-n", namespace, "--timeout=120s"], { stdio: "pipe" });
  await verifyMetricsImage(execute, core, image);
}
