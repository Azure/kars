// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { digest } from "./private-activation.js";
import type { HistoricalCoreRelease } from "./private-core-history.js";

export function historicalHooks(namespace = "monitoring", coreNamespace = "core") {
  const account = "monitoring-admission";
  const definitions: Record<string, string> = { "serviceaccount.yaml": "ServiceAccount", "clusterrole.yaml": "ClusterRole",
    "clusterrolebinding.yaml": "ClusterRoleBinding", "role.yaml": "Role", "rolebinding.yaml": "RoleBinding",
    "job-createSecret.yaml": "Job", "job-patchWebhook.yaml": "Job" };
  return [...Object.entries(definitions).map(([file, kind]) => {
    const name = kind === "Job" ? `${account}-${file === "job-createSecret.yaml" ? "create" : "patch"}` : account;
    const manifest: Record<string, any> = { apiVersion: kind === "Job" ? "batch/v1" : kind === "ServiceAccount" ? "v1" : "rbac.authorization.k8s.io/v1",
      kind, metadata: { name, ...(["ClusterRole", "ClusterRoleBinding"].includes(kind) ? {} : { namespace }) } };
    if (kind.endsWith("Binding")) {
      manifest.subjects = [{ kind: "ServiceAccount", name: account, namespace }];
      manifest.roleRef = { apiGroup: "rbac.authorization.k8s.io", kind: kind === "RoleBinding" ? "Role" : "ClusterRole", name: account };
    }
    if (kind === "Job") manifest.spec = { template: { spec: { serviceAccountName: account, restartPolicy: "OnFailure", containers: [{
      name: "certgen", image: `example.test/certgen:latest@sha256:${"a".repeat(64)}`, args: [file === "job-createSecret.yaml" ? "create" : "patch"],
      securityContext: { allowPrivilegeEscalation: false, capabilities: { drop: ["ALL"] } },
    }] } } };
    return { name, kind, path: `kars/charts/monitoring/templates/prometheus-operator/admission-webhooks/job-patch/${file}`,
      manifest: JSON.stringify(manifest), last_run: { phase: "Succeeded" } };
  }), { name: "kars-default", kind: "ToolPolicy", path: "kars/templates/toolpolicy-default.yaml", last_run: { phase: "Succeeded" },
    manifest: JSON.stringify({ apiVersion: "kars.azure.com/v1alpha1", kind: "ToolPolicy", metadata: { name: "kars-default", namespace: coreNamespace } }) }];
}

export function historicalRelease(revision: number, status: string): HistoricalCoreRelease {
  const release = { name: "kars", namespace: "core", version: revision, apply_method: "ssa", info: { status }, hooks: historicalHooks() };
  const copy = structuredClone(release) as Record<string, any>;
  delete copy.info.status;
  return { name: `sh.helm.release.v1.kars.v${revision}`, uid: `release-${revision}`, revision, digest: digest(copy), release,
    object: { metadata: { name: `sh.helm.release.v1.kars.v${revision}`, uid: `release-${revision}`, namespace: "core", resourceVersion: "1" },
      data: { release: "original-failure-bytes" } } };
}
