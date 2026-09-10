// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { withAzureDestroyTarget } from "./azure-destroy-target.js";
import type { Execute } from "./sre-authority.js";

const subscription = "11111111-1111-1111-1111-111111111111";
const otherSubscription = "22222222-2222-2222-2222-222222222222";
const group = "target-B";
const groupId = `/subscriptions/${subscription}/resourceGroups/${group}`;
const ca = readFileSync(new URL("../../../a2a-gateway/testdata/test-cert.pem", import.meta.url));
const endpoint = { server: "https://aks.example.test/", "certificate-authority-data": ca.toString("base64") };
const metadata = (name: string) => ({ name, uid: name, resourceVersion: "1" });

function cluster(name: string) {
  return { id: `${groupId}/providers/Microsoft.ContainerService/managedClusters/${name}`,
    name, resourceGroup: group, resourceUid: `${name}-immutable-resource-uid` };
}
function credentials(name: string) {
  return JSON.stringify({ apiVersion: "v1", kind: "Config", "current-context": name,
    contexts: [{ name, context: { cluster: name, user: name } }],
    clusters: [{ name, cluster: endpoint }], users: [{ name, user: { token: "synthetic-test-token" } }] });
}
function fixture(names = ["cluster-b"]) {
  const state = {
    clusters: names.map(cluster), inventoryCalls: 0, active: new Set<string>(), unavailable: "",
    selectedUid: "cluster-b-kube-system", selectedEndpoint: { ...endpoint },
    changedInventory: undefined as undefined | ReturnType<typeof cluster>[],
  };
  const files = new Set<string>();
  const execute = vi.fn<Execute>(async (file, args) => {
    if (file === "az") {
      if (args[0] === "account") return { stdout: subscription };
      expect(args.slice(-2)).toEqual(["--subscription", subscription]);
      if (args[0] === "group" && args[1] === "show") return { stdout: groupId };
      if (args[0] === "aks" && args[1] === "list") {
        state.inventoryCalls++;
        return { stdout: JSON.stringify(state.inventoryCalls > 1 ? state.changedInventory ?? state.clusters : state.clusters) };
      }
      if (args[0] === "rest") {
        expect(args.slice(0, 3)).toEqual(["rest", "--method", "post"]);
        const url = args[args.indexOf("--url") + 1];
        const target = state.clusters.find(cluster =>
          url === `${cluster.id.toLowerCase()}/listClusterUserCredential?api-version=2024-10-01`);
        expect(target).toBeDefined();
        const name = target!.name;
        if (state.unavailable === name) throw new Error("AKS credentials Forbidden");
        return { stdout: JSON.stringify({ kubeconfigs: [{ name: "clusterUser", value: Buffer.from(credentials(name)).toString("base64") }] }) };
      }
      if (args[0] === "group" && args[1] === "delete") return { stdout: "" };
    }
    if (file === "kubectl") {
      const context = args[args.indexOf("--context") + 1];
      if (args.includes("config")) return { stdout: JSON.stringify(state.selectedEndpoint) };
      if (args.includes("--kubeconfig")) {
        const path = args[args.indexOf("--kubeconfig") + 1];
        files.add(path);
        expect(statSync(path).mode & 0o777).toBe(0o600);
        expect(statSync(dirname(path)).mode & 0o777).toBe(0o700);
        expect(JSON.parse(readFileSync(path, "utf8"))["current-context"]).toBe(context);
      }
      const start = args.indexOf("get");
      const [kind, name] = args.slice(start + 1, start + 3);
      if (kind === "namespace") return { stdout: name === "kube-system" ? JSON.stringify({
        metadata: { ...metadata(name), uid: context === "selected" ? state.selectedUid : `${context}-kube-system` },
      }) : "" };
      if (kind === "crd") return { stdout: state.active.has(context) ? JSON.stringify({ metadata: metadata(name) }) : "" };
      if (kind === "karssreregistrations.kars.azure.com") return { stdout: JSON.stringify({
        metadata: { ...metadata("canonical"), generation: 1 }, spec: { enabled: true },
        status: { phase: "Ready", observedGeneration: 1 },
      }) };
    }
    throw new Error(`Unexpected fixture command: ${file} ${args.join(" ")}`);
  });
  const remove = vi.fn(async (azure: Execute) => {
    await azure("az", ["group", "delete", "--name", group, "--yes", "--no-wait", "--output", "none"], { stdio: "pipe" });
  });
  const run = (context?: string, exec: Execute = execute, sub?: string) => withAzureDestroyTarget(exec, group, sub, context, remove);
  const cleaned = () => {
    for (const path of files) {
      expect(existsSync(path)).toBe(false);
      expect(existsSync(dirname(path))).toBe(false);
    }
  };
  return { state, execute, files, remove, run, cleaned };
}

describe("Azure teardown retirement target binding", () => {
  it("checks ALL resource-group AKS APIs, ignores global current-context and pins subscription through deletion", async () => {
    const f = fixture(["cluster-b", "cluster-c"]);
    await f.run();
    expect(f.remove).toHaveBeenCalledOnce();
    expect(f.state.inventoryCalls).toBe(2);
    expect(f.files.size).toBe(2);
    expect(f.execute.mock.calls.some(([file, args]) => file === "kubectl" && !args.includes("--kubeconfig"))).toBe(false);
    expect(f.execute.mock.calls.some(([, args]) => args.includes("current-context") || args.includes("--overwrite-existing"))).toBe(false);
    expect(f.execute.mock.calls.some(([, args]) => args.includes("get-credentials") || args.includes("convert-kubeconfig"))).toBe(false);
    const deletion = f.execute.mock.calls.find(([, args]) => args[0] === "group" && args[1] === "delete")!;
    expect(deletion[1].slice(-2)).toEqual(["--subscription", subscription]);
    f.cleaned();
  });

  it("resolves an explicit subscription once and never follows a later selected-account switch", async () => {
    const f = fixture();
    await f.run(undefined, f.execute, subscription);
    expect(f.execute.mock.calls[0][1]).toEqual(["account", "show", "--subscription", subscription, "--query", "id", "--output", "tsv"]);
    expect(f.execute.mock.calls.filter(([, args]) => args[0] === "account")).toHaveLength(1);
    f.cleaned();
  });

  it("accepts an explicit context only with the ARM credential TLS endpoint AND real cluster UID", async () => {
    const f = fixture(["cluster-b", "cluster-c"]);
    await f.run("selected");
    expect(f.remove).toHaveBeenCalledOnce();
    expect(f.files.size).toBe(2);
    f.cleaned();
  });

  it.each(["uid", "ca", "server"])("rejects context A versus deletion target B (%s mismatch)", async field => {
    const f = fixture();
    if (field === "uid") f.state.selectedUid = "cluster-a-kube-system";
    if (field === "ca") f.state.selectedEndpoint["certificate-authority-data"] = Buffer.concat([ca, Buffer.from("\n")]).toString("base64");
    if (field === "server") f.state.selectedEndpoint.server = "https://different.example.test/";
    await expect(f.run("selected")).rejects.toThrow("--context does not match");
    expect(f.remove).not.toHaveBeenCalled();
    f.cleaned();
  });

  it("blocks an active registration in a second cluster even when the selected context is retired/empty", async () => {
    const f = fixture(["cluster-b", "cluster-c"]);
    f.state.active.add("cluster-c");
    await expect(f.run("selected")).rejects.toThrow("Retire");
    expect(f.remove).not.toHaveBeenCalled();
    f.cleaned();
  });

  it("fails closed when any cluster's credentials/API are inaccessible", async () => {
    const f = fixture(["cluster-b", "cluster-c"]);
    f.state.unavailable = "cluster-c";
    await expect(f.run()).rejects.toThrow("Cannot obtain AKS user credentials");
    expect(f.remove).not.toHaveBeenCalled();
    f.cleaned();
  });

  it.each(["new-cluster", "removed-cluster", "replaced-cluster"])("blocks a %s inventory race before deletion", async change => {
    const f = fixture();
    f.state.changedInventory = change === "new-cluster" ? [...f.state.clusters, cluster("new")]
      : change === "removed-cluster" ? [] : [{ ...f.state.clusters[0], resourceUid: "replacement-uid" }];
    await expect(f.run()).rejects.toThrow("inventory changed");
    expect(f.remove).not.toHaveBeenCalled();
    f.cleaned();
  });

  it.each(["id", "name", "resourceUid", "resourceGroup"])("rejects ambiguous or foreign ARM inventory %s", async key => {
    const f = fixture();
    (f.state.clusters[0] as any)[key] = key === "id" ? f.state.clusters[0].id.replace(subscription, otherSubscription) : "";
    await expect(f.run()).rejects.toThrow("exact ARM/resourceUid identity");
    expect(f.remove).not.toHaveBeenCalled();
  });

  it("rejects duplicate clusters rather than treating repeated names as distinct proof", async () => {
    const f = fixture();
    f.state.clusters.push({ ...f.state.clusters[0] });
    await expect(f.run()).rejects.toThrow("exact ARM/resourceUid identity");
    expect(f.remove).not.toHaveBeenCalled();
  });

  it.each(["empty", "duplicate", "bad-base64", "bad-yaml", "insecure-tls"])(
    "rejects %s ARM credentials without leaking their response", async kind => {
      const f = fixture();
      const marker = "credential-response-must-not-be-printed";
      let value: any = { kubeconfigs: [] };
      if (kind === "duplicate") value.kubeconfigs = [{ value: marker }, { value: marker }];
      if (kind === "bad-base64") value.kubeconfigs = [{ value: marker }];
      if (kind === "bad-yaml") value.kubeconfigs = [{ value: Buffer.from(`secret: [${marker}`).toString("base64") }];
      if (kind === "insecure-tls") {
        const config = JSON.parse(credentials("cluster-b"));
        config.clusters[0].cluster["insecure-skip-tls-verify"] = true;
        value.kubeconfigs = [{ value: Buffer.from(JSON.stringify(config)).toString("base64") }];
      }
      const execute: Execute = (file, args, options) => args[0] === "rest"
        ? Promise.resolve({ stdout: JSON.stringify(value) }) : f.execute(file, args, options);
      let error: unknown;
      try { await f.run(undefined, execute); } catch (caught) { error = caught; }
      expect(error).toBeInstanceOf(Error);
      expect(String(error)).not.toContain(marker);
      expect(f.remove).not.toHaveBeenCalled();
      f.cleaned();
    },
  );

  it.each(["account", "group", "list", "api"])("does not interpret a %s failure as cluster absence", async point => {
    const f = fixture();
    const execute: Execute = (file, args, options) => (point === "account" && args[0] === "account")
      || (point === "group" && args[0] === "group" && args[1] === "show")
      || (point === "list" && args[1] === "list") || (point === "api" && file === "kubectl")
      ? Promise.reject(new Error("Forbidden target proof")) : f.execute(file, args, options);
    await expect(f.run(undefined, execute)).rejects.toThrow("Forbidden target proof");
    expect(f.remove).not.toHaveBeenCalled();
    f.cleaned();
  });

  it("permits an authoritatively empty AKS inventory, but cannot claim an explicit unrelated context", async () => {
    const f = fixture([]);
    await f.run();
    expect(f.remove).toHaveBeenCalledOnce();
    expect(f.state.inventoryCalls).toBe(2);
    f.remove.mockClear();
    await expect(f.run("selected")).rejects.toThrow("--context does not match");
    expect(f.remove).not.toHaveBeenCalled();
  });

  it("rejects subscription retargeting even inside the destructive callback", async () => {
    const f = fixture();
    await expect(withAzureDestroyTarget(f.execute, group, undefined, undefined, async azure => {
      await azure("az", ["group", "delete", "--name", group, "--subscription", otherSubscription], { stdio: "pipe" });
    })).rejects.toThrow("not the deployment subscription");
    expect(f.execute.mock.calls.some(([, args]) => args[1] === "delete")).toBe(false);
    f.cleaned();
  });
});
