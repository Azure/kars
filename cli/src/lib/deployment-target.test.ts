// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { pinAzureSubscription, createSubscriptionPinnedExeca } from "./azure-subscription.js";
import {
  connectDeploymentTarget, createDeploymentExecutor, preparePushTarget,
  resolveDeploymentSubscription, verifyAdoptedTarget, type Execute,
} from "./deployment-target.js";

const context = { subscription: "sub-b", resourceGroup: "shared-rg", aksCluster: "shared-aks" };
const identity = {
  id: "/subscriptions/sub-b/resourceGroups/shared-rg/providers/Microsoft.ContainerService/managedClusters/shared-aks",
  fqdn: "cluster-b.example.test",
};

function executor(route: (bin: string, args: string[]) => string) {
  return vi.fn(async (bin: string, args: readonly string[]) => ({ stdout: route(bin, [...args]) })) as unknown as Execute;
}

function recorded(execute: Execute): Array<[string, string[]]> {
  return vi.mocked(execute).mock.calls as unknown as Array<[string, string[]]>;
}

function connectedRoute(bin: string, args: string[]): string {
  if (bin === "az" && args[0] === "aks" && args[1] === "show") return JSON.stringify(identity);
  if (bin === "kubectl" && args.includes("view")) return "https://cluster-b.example.test";
  if (bin === "kubectl" && args.includes("current-context")) return "adopted";
  return "";
}

describe("deployment subscription pinning (shared #531 pattern)", () => {
  it("does not use ambient account A when the cache selected B", async () => {
    const raw = executor(() => "");
    expect(await resolveDeploymentSubscription(raw, context)).toBe("sub-b");
    expect(raw).not.toHaveBeenCalled();
    await createSubscriptionPinnedExeca(raw, "sub-b")("az", ["acr", "login", "--name", "mirror"]);
    expect(raw).toHaveBeenCalledWith("az", ["acr", "login", "--name", "mirror", "--subscription", "sub-b"], undefined);
  });

  it("retains matching pins and rejects duplicate or conflicting explicit pins", () => {
    expect(pinAzureSubscription(["aks", "show", "--subscription=sub-b"], "sub-b"))
      .toEqual(["aks", "show", "--subscription=sub-b"]);
    expect(() => pinAzureSubscription(["aks", "show", "--subscription", "sub-a"], "sub-b")).toThrow("not the deployment");
    expect(() => pinAzureSubscription(["aks", "show", "--subscription", "sub-b", "--subscription=sub-b"], "sub-b")).toThrow("duplicate");
    expect(() => pinAzureSubscription(["aks", "show", "--subscription"], "sub-b")).toThrow("requires");
  });

  it.each(["ResourceNotFound", "ResourceGroupNotFound", "ManagedClusterNotFound", "ParentResourceNotFound"])(
    "legacy discovery skips only explicit %s results", async code => {
      const raw = executor((_bin, args) => {
        if (args[0] === "account") return JSON.stringify(["sub-a", "sub-b"]);
        if (args[args.indexOf("--subscription") + 1] === "sub-a") throw new Error(`(${code}) absent`);
        return JSON.stringify({ name: "shared-aks", resourceGroup: "shared-rg" });
      });
      expect(await resolveDeploymentSubscription(raw, { ...context, subscription: undefined })).toBe("sub-b");
      expect(recorded(raw).slice(1).every(([, args]) => args.includes("--subscription"))).toBe(true);
    },
  );

  it("rejects ambiguous legacy cluster names instead of choosing the ambient subscription", async () => {
    const raw = executor((_bin, args) => args[0] === "account"
      ? JSON.stringify(["sub-a", "sub-b"])
      : JSON.stringify({ name: "shared-aks", resourceGroup: "shared-rg" }));
    await expect(resolveDeploymentSubscription(raw, { ...context, subscription: undefined })).rejects.toThrow("multiple");
    expect(recorded(raw).every(([, args]) => args[1] === "list" || args[1] === "show")).toBe(true);
  });

  it.each(["(AuthorizationFailed) denied", "timed out"])("propagates %s rather than treating it as absence", async message => {
    const raw = executor((_bin, args) => {
      if (args[0] === "account") return JSON.stringify(["sub-a", "sub-b"]);
      throw new Error(message);
    });
    await expect(resolveDeploymentSubscription(raw, { ...context, subscription: undefined })).rejects.toThrow(message);
    expect(raw).toHaveBeenCalledTimes(2);
  });

  it.each(["garbage", "{}", "[null]"])("rejects malformed legacy discovery: %s", async response => {
    await expect(resolveDeploymentSubscription(executor(() => response), { ...context, subscription: undefined })).rejects.toThrow();
  });
});

describe("verified Kubernetes target", () => {
  it("pins credential retrieval, Azure mutation, Helm and kubectl to adopted B", async () => {
    const raw = executor(connectedRoute);
    const target = await connectDeploymentTarget(raw, context);
    await target.execute("az", ["acr", "import", "--name", "mirror", "--source", "ghcr.io/azure/example:latest"]);
    await target.execute("az", ["role", "assignment", "create", "--role", "reader"]);
    await target.execute("helm", ["upgrade", "kars", "chart"]);
    await target.execute("kubectl", ["rollout", "restart", "deployment/relay"]);
    const calls = recorded(raw);
    for (const [bin, argv] of calls) {
      const args = argv as string[];
      if (bin === "az") expect(args[args.indexOf("--subscription") + 1]).toBe("sub-b");
      if (bin === "helm") expect(args[args.indexOf("--kube-context") + 1]).toBe(target.kubeContext);
      if (bin === "kubectl") expect(args[args.indexOf("--context") + 1]).toBe(target.kubeContext);
    }
    expect(calls.some(([, args]) => args?.[0] === "account" && args?.[1] === "set")).toBe(false);
  });

  it("rejects Azure identity A before retrieving credentials or mutating anything", async () => {
    const raw = executor(() => JSON.stringify({ ...identity, id: identity.id.replace("sub-b", "sub-a") }));
    await expect(connectDeploymentTarget(raw, context)).rejects.toThrow("does not match");
    expect(raw).toHaveBeenCalledTimes(1);
  });

  it("rejects kubeconfig A before any Helm, ACR, or rollout mutation", async () => {
    const raw = executor((bin, args) => bin === "kubectl" ? "https://cluster-a.example.test" : connectedRoute(bin, args));
    await expect(connectDeploymentTarget(raw, context)).rejects.toThrow("does not point");
    expect(recorded(raw).some(([bin, args]) => bin === "helm" || args.includes("import") || args.includes("rollout"))).toBe(false);
  });

  it("rejects conflicting Kubernetes selections before subprocess execution", () => {
    const raw = executor(() => "");
    const pinned = createDeploymentExecutor(raw, "sub-b", "verified");
    expect(() => pinned("kubectl", ["apply", "--context", "wrong"])).toThrow("different");
    expect(() => pinned("helm", ["upgrade", "--kube-context=wrong"])).toThrow("different");
    expect(raw).not.toHaveBeenCalled();
  });

  it("adoption verifies the existing context without rewriting kubeconfig", async () => {
    const raw = executor(connectedRoute);
    const scoped = await verifyAdoptedTarget(raw, context, "existing-b");
    await scoped("helm", ["status", "kars"]);
    expect(recorded(raw).some(([, args]) => args.includes("get-credentials"))).toBe(false);
    expect(recorded(raw).at(-1)?.[1]).toEqual(["status", "kars", "--kube-context", "existing-b"]);
  });

  it("push without apply scopes ACR login but does not require Kubernetes", async () => {
    const raw = executor(() => "");
    const scoped = await preparePushTarget(raw, { subscription: "sub-b" }, {});
    await scoped("az", ["acr", "login", "--name", "mirror"]);
    expect(raw).toHaveBeenCalledTimes(1);
    expect(recorded(raw)[0][1]).toContain("sub-b");
  });

  it("push rejects explicit A against adopted B before any operation", async () => {
    const raw = executor(() => "");
    await expect(preparePushTarget(raw, context, { apply: true, subscription: "sub-a" })).rejects.toThrow("differs");
    expect(raw).not.toHaveBeenCalled();
  });
});
