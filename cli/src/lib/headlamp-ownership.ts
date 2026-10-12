// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execa } from "execa";
function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export const HEADLAMP_OPERATOR_LOGIN =
  "Core-managed Headlamp requires your own Kubernetes login credential; " +
  "the dashboard ServiceAccount deliberately has no resource permissions.";

export async function hasCoreHeadlamp(
  context: string,
  namespace = "headlamp",
  kubectl = "kubectl",
): Promise<boolean> {
  const { stdout } = await execa(kubectl, [
    "--context", context, "get", "deployment", "headlamp",
    "-n", namespace, "--ignore-not-found", "-o", "json",
  ], { stdio: "pipe" });
  if (!stdout.trim()) return false;
  const deployment: unknown = JSON.parse(stdout);
  if (!isObject(deployment) || deployment.kind !== "Deployment" || !isObject(deployment.metadata)) {
    throw new Error("Invalid Headlamp Deployment response; refusing to infer ownership");
  }
  const annotations = deployment.metadata.annotations;
  if (annotations === undefined) return false;
  if (!isObject(annotations) || Object.values(annotations).some(value => typeof value !== "string")) {
    throw new Error("Invalid Headlamp annotations; refusing to infer ownership");
  }
  if (annotations["kars.azure.com/headlamp-plugin"] !== "bundled") return false;
  const owner = annotations["kars.azure.com/headlamp-owner"];
  if (owner !== undefined) {
    if (typeof owner !== "string" || !/^[^/\s]+\/[^/\s]+$/.test(owner)) {
      throw new Error("Invalid Core Headlamp owner; refusing to replace it");
    }
    if ((annotations["meta.helm.sh/release-name"] || annotations["meta.helm.sh/release-namespace"])
      && owner !== `${annotations["meta.helm.sh/release-namespace"]}/${annotations["meta.helm.sh/release-name"]}`) {
      throw new Error("Conflicting Core Headlamp ownership; refusing to replace it");
    }
    return true;
  }
  return !!annotations["meta.helm.sh/release-name"];
}
