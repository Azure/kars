// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash, randomUUID, X509Certificate } from "node:crypto";
import { mkdir, open, unlink, rmdir } from "node:fs/promises";
import { resolve } from "node:path";
import { parse } from "yaml";
import { pinAzureSubscription } from "./azure-subscription.js";
import { assertDestroySafe, get, type Execute } from "./sre-authority.js";

interface Cluster { id: string; name: string; resourceGroup: string; resourceUid: string }

function trustedEndpoint(cluster: any): string {
  if (!cluster || typeof cluster.server !== "string"
    || typeof cluster["certificate-authority-data"] !== "string"
    || cluster["insecure-skip-tls-verify"] || cluster["proxy-url"] || cluster["tls-server-name"]
    || cluster["certificate-authority"]) throw new Error("AKS target lacks an unambiguous TLS-authenticated API endpoint");
  const url = new URL(cluster.server);
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash || url.pathname !== "/") {
    throw new Error("Invalid AKS API endpoint");
  }
  const ca = Buffer.from(cluster["certificate-authority-data"], "base64");
  new X509Certificate(ca);
  return `${url.href}\n${createHash("sha256").update(ca).digest("hex")}`;
}

function credentialIdentity(stdout: string): { context: string; endpoint: string } {
  let config;
  try { config = parse(stdout); } catch {
    throw new Error("Azure returned malformed AKS credentials; deletion is blocked");
  }
  const context = config?.["current-context"];
  if (typeof context !== "string" || !context || !Array.isArray(config.contexts) || !Array.isArray(config.clusters)) {
    throw new Error("Azure returned an invalid AKS kubeconfig");
  }
  const contexts = config.contexts.filter((item: any) => item.name === context);
  if (contexts.length !== 1) throw new Error("Azure returned an ambiguous AKS context");
  const clusters = config.clusters.filter((item: any) => item.name === contexts[0].context?.cluster);
  if (clusters.length !== 1) throw new Error("Azure returned an ambiguous AKS cluster");
  return { context, endpoint: trustedEndpoint(clusters[0].cluster) };
}

async function userCredentials(azure: Execute, id: string): Promise<string> {
  // Some az aks get-credentials versions run kubelogin conversion even with
  // --file -, potentially touching the global kubeconfig. Read ARM directly.
  let result;
  try {
    const response = await azure("az", ["rest", "--method", "post", "--url",
      `${id}/listClusterUserCredential?api-version=2024-10-01`, "--output", "json"], { stdio: "pipe" });
    result = JSON.parse(response.stdout);
  } catch {
    // Command/YAML errors must never print credential-bearing response bodies.
    throw new Error("Cannot obtain AKS user credentials from the selected ARM resource; deletion is blocked");
  }
  const configs = result?.kubeconfigs;
  if (!Array.isArray(configs) || configs.length !== 1 || typeof configs[0]?.value !== "string"
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(configs[0].value)
    || !configs[0].value) throw new Error("Azure returned ambiguous or invalid AKS credentials; deletion is blocked");
  return Buffer.from(configs[0].value, "base64").toString("utf8");
}

/** Bind every retirement check to ARM-issued credentials for every AKS resource
 * in the deletion target. Neither current-context nor an AKS name is proof.
 * The callback receives the same subscription pin used by the entire preflight. */
export async function withAzureDestroyTarget(
  execute: Execute, resourceGroup: string, requestedSubscription: string | undefined,
  requestedContext: string | undefined, destroy: (azure: Execute) => Promise<void>,
): Promise<void> {
  const subscription = (await execute("az", ["account", "show",
    ...(requestedSubscription ? ["--subscription", requestedSubscription] : []),
    "--query", "id", "--output", "tsv"], { stdio: "pipe" })).stdout.trim();
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(subscription)) {
    throw new Error("Azure did not resolve a subscription ID; resource-group deletion is blocked");
  }
  const azure: Execute = (file, args, options) => {
    if (file !== "az") throw new Error("Azure teardown executor cannot use an unbound Kubernetes context");
    return execute(file, pinAzureSubscription(args, subscription), options);
  };
  const groupId = `/subscriptions/${subscription}/resourceGroups/${resourceGroup}`;
  async function inventory(): Promise<Cluster[]> {
    const group = (await azure("az", ["group", "show", "--name", resourceGroup,
      "--query", "id", "--output", "tsv"], { stdio: "pipe" })).stdout.trim();
    if (group.toLowerCase() !== groupId.toLowerCase()) throw new Error("Azure resource-group target identity mismatch");
    const result: unknown = JSON.parse((await azure("az", ["aks", "list", "--resource-group", resourceGroup,
      "--output", "json"], { stdio: "pipe" })).stdout);
    if (!Array.isArray(result)) throw new Error("Azure AKS inventory is malformed; deletion is blocked");
    const seen = new Set<string>();
    const clusters: Cluster[] = [];
    for (const cluster of result) {
      if (!cluster || typeof cluster.name !== "string" || !cluster.name
        || typeof cluster.id !== "string" || typeof cluster.resourceGroup !== "string"
        || typeof cluster.resourceUid !== "string" || !cluster.resourceUid.trim()
        || cluster.resourceGroup.toLowerCase() !== resourceGroup.toLowerCase()
        || cluster.id.toLowerCase() !== `${groupId}/providers/Microsoft.ContainerService/managedClusters/${cluster.name}`.toLowerCase()
        || seen.has(cluster.id.toLowerCase())) {
        throw new Error("AKS inventory lacks exact ARM/resourceUid identity; update Azure CLI or review the target before deletion");
      }
      seen.add(cluster.id.toLowerCase());
      clusters.push({ id: cluster.id.toLowerCase(), name: cluster.name, resourceGroup: cluster.resourceGroup,
        resourceUid: cluster.resourceUid });
    }
    return clusters.sort((a, b) => a.id.localeCompare(b.id));
  }
  const clusters = await inventory();
  let selected: { endpoint: string; uid: string } | undefined;
  if (requestedContext) {
    const endpoint = trustedEndpoint(JSON.parse((await execute("kubectl", ["--context", requestedContext,
      "config", "view", "--minify", "--raw", "-o", "jsonpath={.clusters[0].cluster}"], { stdio: "pipe" })).stdout));
    const ns = await get((file, args, options) =>
      execute(file, ["--context", requestedContext, ...args], options), "namespace", "kube-system");
    if (!ns) throw new Error("Selected context lacks a live kube-system identity");
    selected = { endpoint, uid: ns.metadata.uid! };
  }
  let contextMatched = !selected;
  const directory = resolve(`.kars-destroy-${randomUUID()}`);
  await mkdir(directory, { mode: 0o700 });
  const owned: string[] = [];
  try {
    for (const [index, cluster] of clusters.entries()) {
      const credentials = await userCredentials(azure, cluster.id);
      const identity = credentialIdentity(credentials);
      const path = resolve(directory, `cluster-${index}.yaml`);
      const file = await open(path, "wx", 0o600);
      owned.push(path);
      try { await file.writeFile(credentials); } finally { await file.close(); }
      const bound: Execute = (file, args, options) => {
        if (file !== "kubectl") throw new Error("Retirement preflight requires the bound AKS API");
        return execute(file, ["--kubeconfig", path, "--context", identity.context, ...args], options);
      };
      const before = await get(bound, "namespace", "kube-system");
      if (!before) throw new Error("AKS API lacks its live kube-system identity");
      if (selected?.endpoint === identity.endpoint && selected.uid === before.metadata.uid) contextMatched = true;
      await assertDestroySafe(bound);
      const after = await get(bound, "namespace", "kube-system");
      if (after?.metadata.uid !== before.metadata.uid) throw new Error("AKS API identity changed during retirement preflight");
    }
    if (!contextMatched) throw new Error("--context does not match any TLS/UID-verified AKS cluster in the selected resource group");
    if (JSON.stringify(await inventory()) !== JSON.stringify(clusters)) {
      throw new Error("Azure AKS inventory changed during retirement preflight; deletion is blocked");
    }
    await destroy(azure);
  } finally {
    for (const path of owned) await unlink(path);
    // No recursive deletion: foreign files are never removed.
    await rmdir(directory);
  }
}
