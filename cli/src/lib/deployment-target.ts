// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { DeploymentContext } from "../config.js";
import { createSubscriptionPinnedExeca } from "./azure-subscription.js";

export type Execute = typeof import("execa").execa;
type ClusterContext = Pick<DeploymentContext, "subscription" | "resourceGroup" | "aksCluster">;

function isAksNotFound(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const detail = error as Record<string, unknown>;
  const text = ["stderr", "stdout", "shortMessage", "message"]
    .map(key => detail[key]).filter(value => typeof value === "string").join("\n");
  return /\((?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)\)/i.test(text)
    || /["']code["']\s*:\s*["'](?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)["']/i.test(text);
}

/** #531 compatibility rule: a legacy RG/AKS pair must resolve in exactly one
 * enabled subscription. Authorization, transport and malformed-response errors
 * are never interpreted as absence. */
export async function resolveDeploymentSubscription(execute: Execute, ctx: ClusterContext): Promise<string> {
  const cached = ctx.subscription?.trim();
  if (cached) return cached;
  if (!ctx.resourceGroup || !ctx.aksCluster) {
    throw new Error("Deployment context lacks a subscription and an AKS identity; specify --subscription or adopt the intended cluster.");
  }
  const { stdout } = await execute("az", [
    "account", "list", "--query", "[?state=='Enabled'].id", "--output", "json",
  ], { stdio: "pipe", timeout: 15000 });
  const candidates: unknown = JSON.parse(String(stdout));
  if (!Array.isArray(candidates) || candidates.some(value => typeof value !== "string" || !value.trim())) {
    throw new Error("Azure returned an invalid enabled subscription list");
  }
  const matches = new Set<string>();
  for (const subscription of new Set(candidates.map(value => (value as string).trim()))) {
    let output: string;
    try {
      const result = await execute("az", [
        "aks", "show", "--resource-group", ctx.resourceGroup, "--name", ctx.aksCluster,
        "--query", "{name:name,resourceGroup:resourceGroup}", "--output", "json", "--subscription", subscription,
      ], { stdio: "pipe", timeout: 30000 });
      output = String(result.stdout);
    } catch (error) {
      if (isAksNotFound(error)) continue;
      throw error;
    }
    const cluster: unknown = JSON.parse(output);
    if (!cluster || typeof cluster !== "object"
      || typeof (cluster as Record<string, unknown>).name !== "string"
      || typeof (cluster as Record<string, unknown>).resourceGroup !== "string") {
      throw new Error(`Looking up cached AKS cluster in '${subscription}' returned an invalid cluster`);
    }
    const fields = cluster as { name: string; resourceGroup: string };
    if (fields.name.toLowerCase() === ctx.aksCluster.toLowerCase()
      && fields.resourceGroup.toLowerCase() === ctx.resourceGroup.toLowerCase()) matches.add(subscription);
  }
  if (matches.size === 1) return [...matches][0];
  throw new Error(matches.size
    ? "Cached AKS cluster exists in multiple enabled Azure subscriptions; adopt the intended subscription before updating."
    : "No enabled Azure subscription contains the cached AKS cluster; update stopped before making changes.");
}

function pinOption(args: readonly string[], option: string, value: string): string[] {
  const indexes = args.flatMap((arg, index) => arg === option || arg.startsWith(`${option}=`) ? [index] : []);
  if (!indexes.length) return [...args, option, value];
  const index = indexes[0];
  const selected = args[index] === option ? args[index + 1] : args[index].slice(option.length + 1);
  if (indexes.length !== 1 || selected !== value) throw new Error(`Command targets a different ${option}; deployment target is '${value}'`);
  return [...args];
}

export function createDeploymentExecutor(execute: Execute, subscription: string, kubeContext: string): Execute {
  const azure = createSubscriptionPinnedExeca(execute, subscription);
  return ((file: string, args: readonly string[] = [], options?: unknown) =>
    azure(file, file === "kubectl" ? pinOption(args, "--context", kubeContext)
      : file === "helm" ? pinOption(args, "--kube-context", kubeContext) : args, options as never)
  ) as unknown as Execute;
}

async function clusterIdentity(execute: Execute, ctx: ClusterContext, subscription: string): Promise<string[]> {
  if (!ctx.resourceGroup?.trim() || !ctx.aksCluster?.trim()) throw new Error("Deployment context is missing resource group or AKS cluster");
  const { stdout } = await createSubscriptionPinnedExeca(execute, subscription)("az", [
    "aks", "show", "--resource-group", ctx.resourceGroup, "--name", ctx.aksCluster,
    "--query", "{id:id,name:name,resourceGroup:resourceGroup,fqdn:fqdn,privateFqdn:privateFqdn}",
    "--output", "json",
  ], { stdio: "pipe", timeout: 30000 });
  const cluster = JSON.parse(String(stdout)) as Record<string, unknown>;
  const expectedId = `/subscriptions/${subscription}/resourceGroups/${ctx.resourceGroup}/providers/Microsoft.ContainerService/managedClusters/${ctx.aksCluster}`;
  if (typeof cluster?.id !== "string" || cluster.id.toLowerCase() !== expectedId.toLowerCase()) {
    throw new Error("Azure AKS identity does not match the adopted subscription/resource group/cluster; no updates were made.");
  }
  const hosts = [cluster.fqdn, cluster.privateFqdn]
    .filter((value): value is string => typeof value === "string" && !!value.trim())
    .map(value => value.trim().toLowerCase());
  if (!hosts.length) throw new Error("Azure AKS lookup returned no API-server hostname; cannot verify Kubernetes target.");
  return hosts;
}

async function verifyKubeTarget(execute: Execute, hosts: string[]): Promise<void> {
  const { stdout } = await execute("kubectl", [
    "config", "view", "--minify", "--output", "jsonpath={.clusters[0].cluster.server}",
  ], { stdio: "pipe", timeout: 15000 });
  const server = new URL(String(stdout).trim());
  if (server.protocol !== "https:" || server.username || server.password
    || (server.port && server.port !== "443") || server.pathname !== "/" || server.search || server.hash
    || !hosts.includes(server.hostname.toLowerCase())) {
    throw new Error("Selected Kubernetes context does not point to the adopted AKS API server; no updates were made.");
  }
}

export async function connectDeploymentTarget(execute: Execute, ctx: ClusterContext): Promise<{
  execute: Execute; subscription: string; kubeContext: string;
}> {
  const subscription = await resolveDeploymentSubscription(execute, ctx);
  const hosts = await clusterIdentity(execute, ctx, subscription);
  const kubeContext = ["kars", subscription, ctx.resourceGroup!, ctx.aksCluster!].map(encodeURIComponent).join("/");
  const azure = createSubscriptionPinnedExeca(execute, subscription);
  await azure("az", [
    "aks", "get-credentials", "--name", ctx.aksCluster!, "--resource-group", ctx.resourceGroup!,
    "--context", kubeContext, "--overwrite-existing", "--output", "none",
  ], { stdio: "pipe", timeout: 30000 });
  const scoped = createDeploymentExecutor(execute, subscription, kubeContext);
  await verifyKubeTarget(scoped, hosts);
  return { execute: scoped, subscription, kubeContext };
}

/** Adoption only inspects the existing Kubernetes context; it never replaces it. */
export async function verifyAdoptedTarget(execute: Execute, ctx: ClusterContext, requestedContext?: string): Promise<Execute> {
  const subscription = await resolveDeploymentSubscription(execute, ctx);
  const hosts = await clusterIdentity(execute, ctx, subscription);
  const kubeContext = requestedContext?.trim()
    || String((await execute("kubectl", ["config", "current-context"], { stdio: "pipe" })).stdout).trim();
  if (!kubeContext) throw new Error("No Kubernetes context selected for AKS adoption");
  const scoped = createDeploymentExecutor(execute, subscription, kubeContext);
  await verifyKubeTarget(scoped, hosts);
  return scoped;
}

export async function preparePushTarget(execute: Execute, ctx: DeploymentContext | null, options: {
  apply?: boolean; subscription?: string;
}): Promise<Execute> {
  if (options.subscription && ctx?.subscription && options.subscription.trim() !== ctx.subscription.trim()) {
    throw new Error("--subscription differs from the saved deployment subscription; adopt the intended deployment first.");
  }
  const target = { ...ctx, subscription: options.subscription || ctx?.subscription };
  if (options.apply) return (await connectDeploymentTarget(execute, target)).execute;
  return createSubscriptionPinnedExeca(execute, await resolveDeploymentSubscription(execute, target));
}
