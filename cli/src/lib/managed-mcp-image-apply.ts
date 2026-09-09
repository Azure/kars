// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";
import { MANAGED_MCP_IMAGE_TARGET, type PushedImage } from "./image-targets.js";

interface ObjectRecord {
  metadata: { name: string; namespace?: string; uid: string; resourceVersion: string; generation?: number;
    deletionTimestamp?: string; annotations?: Record<string, string>; labels?: Record<string, string>; ownerReferences?: unknown[] };
  spec?: Record<string, any>;
  status?: Record<string, any>;
}
export interface ManagedMcpImagePlan { source: ObjectRecord; image: string }

function checked(value: unknown): ObjectRecord {
  const object = value as ObjectRecord;
  if (!object?.metadata?.name || !object.metadata.uid || !object.metadata.resourceVersion
    || object.metadata.deletionTimestamp) throw new Error("Managed MCP object lacks a live exact API identity");
  return object;
}

async function read(execute: Execute, kind: string, name: string, namespace?: string): Promise<ObjectRecord> {
  const result = await execute("kubectl", ["get", kind, name, ...(namespace ? ["-n", namespace] : []), "-o", "json"], { stdio: "pipe" });
  const object = checked(JSON.parse(String(result.stdout)));
  if (object.metadata.name !== name || (namespace && object.metadata.namespace !== namespace)) {
    throw new Error("Managed MCP API returned another object");
  }
  return object;
}

export async function inspectManagedMcpPlans(execute: Execute, images: PushedImage[]): Promise<ManagedMcpImagePlan[]> {
  const image = images.find(image => image.name === MANAGED_MCP_IMAGE_TARGET.name)?.image;
  if (!image) return [];
  const result = await execute("kubectl", ["get", "mcpservers", "-A", "-o", "json"], { stdio: "pipe" });
  const list = JSON.parse(String(result.stdout));
  if (!Array.isArray(list.items)) throw new Error("Managed MCP inventory is invalid");
  return list.items.filter((item: ObjectRecord) => item.spec?.managed?.preset === "everything").map((item: unknown) => {
    const source = checked(item);
    if (!source.metadata.namespace || !source.metadata.generation) throw new Error("Managed MCP source lacks workspace/generation");
    return { source, image };
  });
}

function sourceUnchanged(current: ObjectRecord, original: ObjectRecord): void {
  if (current.metadata.uid !== original.metadata.uid || current.metadata.generation !== original.metadata.generation
    || JSON.stringify(current.spec) !== JSON.stringify(original.spec)) {
    throw new Error("Managed MCP source changed during image application; no CR spec was overwritten");
  }
}

function owned(resource: ObjectRecord, source: ObjectRecord, namespaceUid: string): void {
  const annotations = resource.metadata.annotations ?? {};
  if (resource.metadata.ownerReferences?.length
    || resource.metadata.labels?.["app.kubernetes.io/managed-by"] !== "kars-controller"
    || annotations["kars.azure.com/mcp-source-namespace"] !== source.metadata.namespace
    || annotations["kars.azure.com/mcp-source-name"] !== source.metadata.name
    || annotations["kars.azure.com/mcp-source-uid"] !== source.metadata.uid
    || annotations["kars.azure.com/mcp-namespace-uid"] !== namespaceUid) {
    throw new Error("Managed MCP workload ownership does not match its source/namespace UIDs");
  }
}

export async function refreshManagedMcpImages(
  execute: Execute, plans: ManagedMcpImagePlan[], controllerNamespace: string,
): Promise<number> {
  if (!plans.length) return 0;
  const controller = await read(execute, "namespace", controllerNamespace);
  for (const { source, image } of plans) {
    const current = await read(execute, "mcpserver", source.metadata.name, source.metadata.namespace);
    sourceUnchanged(current, source);
    await execute("kubectl", ["patch", "mcpserver", source.metadata.name, "-n", source.metadata.namespace!,
      "--type=merge", "-p", JSON.stringify({ metadata: { uid: current.metadata.uid,
        resourceVersion: current.metadata.resourceVersion, annotations: { "kars.azure.com/image-refresh": String(Date.now()) } } })],
    { stdio: "pipe" });
    await execute("kubectl", ["wait", `mcpserver/${source.metadata.name}`, "-n", source.metadata.namespace!,
      `--for=jsonpath={.status.workloadImage}=${image}`, "--timeout=300s"], { stdio: "pipe" });
    const verified = await read(execute, "mcpserver", source.metadata.name, source.metadata.namespace);
    sourceUnchanged(verified, source);
    const status = verified.status;
    if (status?.phase !== "Ready" || status.observedGeneration !== verified.metadata.generation
      || status.workloadImage !== image || !status.workloadRef || !status.managedNamespaceUid) {
      throw new Error("Managed MCP source did not qualify the selected image");
    }
    const parts = String(status.workloadRef).split("/");
    if (parts.length !== 2 || parts.some(part => !/^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(part))) {
      throw new Error("Managed MCP workload reference is invalid");
    }
    const [namespace, name] = parts;
    const ns = await read(execute, "namespace", namespace);
    if (ns.metadata.uid !== status.managedNamespaceUid
      || ns.metadata.annotations?.["kars.azure.com/mcp-namespace-claim"] !== "v1"
      || ns.metadata.annotations?.["kars.azure.com/mcp-controller-namespace"] !== controllerNamespace
      || ns.metadata.annotations?.["kars.azure.com/mcp-controller-namespace-uid"] !== controller.metadata.uid) {
      throw new Error("Managed MCP namespace ownership changed");
    }
    const deployment = await read(execute, "deployment", name, namespace);
    const service = await read(execute, "service", name, namespace);
    owned(deployment, source, ns.metadata.uid);
    owned(service, source, ns.metadata.uid);
    const selector = { "kars.azure.com/mcp-source-uid": source.metadata.uid };
    if (JSON.stringify(deployment.spec?.selector?.matchLabels) !== JSON.stringify(selector)
      || JSON.stringify(service.spec?.selector) !== JSON.stringify(selector)
      || deployment.spec?.template?.spec?.containers?.find((container: { name: string }) => container.name === "mcp")?.image !== image
      || deployment.status?.observedGeneration !== deployment.metadata.generation
      || deployment.status?.availableReplicas !== 1 || deployment.status?.updatedReplicas !== 1
      || status.workloadGeneration !== deployment.metadata.generation) {
      throw new Error("Managed MCP workload/Service did not converge to the qualified artifact and selector");
    }
  }
  return plans.length;
}
