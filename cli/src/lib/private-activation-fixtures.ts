// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { expect } from "vitest";
import { bundleDefinition } from "./private-activation.js";

export function fixture() {
  const objects = new Map<string, any>();
  const calls: string[][] = [];
  const key = (kind: string, name: string, namespace = "") => `${kind}/${namespace}/${name}`;
  for (const name of ["work", "core", "reader"]) objects.set(key("namespace", name), {
    kind: "Namespace", metadata: { name, uid: `${name}-uid`, resourceVersion: "1", annotations: {} },
  });
  objects.set(key("serviceaccount", "kars-controller", "core"), {
    metadata: { name: "kars-controller", namespace: "core", uid: "controller-sa", resourceVersion: "1" },
  });
  objects.set(key("serviceaccount", "bff", "reader"), {
    metadata: { name: "bff", namespace: "reader", uid: "reader-sa", resourceVersion: "1" },
  });
  for (const name of bundleDefinition().controllers as string[]) objects.set(key("serviceaccount", name, "kube-system"), {
    metadata: { name, namespace: "kube-system", uid: `${name}-uid`, resourceVersion: "1" },
  });
  const deployment = {
    kind: "Deployment", metadata: { name: "kars-controller", namespace: "core", uid: "deployment", resourceVersion: "1", generation: 1 },
    spec: { replicas: 1, template: { metadata: {}, spec: { serviceAccountName: "kars-controller",
      containers: [{ name: "controller", image: "fixture", command: ["controller"] }] } } },
    status: { observedGeneration: 1, updatedReplicas: 1, availableReplicas: 1 },
  };
  objects.set(key("deployment", "kars-controller", "core"), deployment);
  objects.set(key("deployments.apps", "kars-controller", "core"), deployment);
  for (const [index, entry] of (bundleDefinition().objects as any[]).entries()) {
    const value = structuredClone(entry);
    value.metadata = { ...value.metadata, uid: `policy-${index}`, resourceVersion: "1", generation: 1 };
    if (value.kind === "ValidatingAdmissionPolicy") value.status = { observedGeneration: 1, typeChecking: {} };
    objects.set(key(value.kind.toLowerCase(), value.metadata.name), value);
  }
  const pods = new Map<string, any[]>([["work", []], ["core", []], ["reader", []]]);
  const merge = (value: any, patch: any) => {
    for (const [name, entry] of Object.entries(patch)) {
      if (name === "__proto__" || name === "constructor" || name === "prototype") {
        throw new Error("Unsafe fixture patch property");
      }
      if (entry && typeof entry === "object" && !Array.isArray(entry)) {
        value[name] ??= {};
        merge(value[name], entry);
      } else value[name] = entry;
    }
  };
  const execute = async (args: string[], input?: string) => {
    calls.push(args);
    if (args[0] === "auth") return "yes";
    if (args[0] === "create") {
      const value = JSON.parse(input!);
      const path = key("karscredentialgrants.kars.azure.com", value.metadata.name, value.metadata.namespace);
      if (objects.has(path)) throw new Error("fixture create conflict");
      value.metadata.uid = `created-grant-${value.metadata.namespace}`;
      value.metadata.resourceVersion = "1";
      value.metadata.generation = 1;
      objects.set(path, value);
      return JSON.stringify(value);
    }
    const namespace = args.includes("-n") ? args[args.indexOf("-n") + 1]! : "";
    if (args[0] === "get" && args[1] === "pods") return JSON.stringify({ metadata: {}, items: pods.get(namespace) ?? [] });
    if (args[0] === "get" && args[1] === "karscredentialgrants.kars.azure.com" && args.includes("--all-namespaces")) {
      return JSON.stringify({ metadata: {}, items: [...objects.entries()].filter(([path]) =>
        path.startsWith("karscredentialgrants.kars.azure.com/")).map(([, object]) => object) });
    }
    const value = objects.get(key(args[1]!, args[2]!, namespace));
    if (!value && args.includes("--ignore-not-found")) return "";
    if (!value) throw new Error("fixture object unavailable");
    if (args[0] === "get" && args[1] === "secret") {
      const format = args[args.indexOf("-o") + 1];
      if (format === "go-template={{json .metadata}}") return JSON.stringify(value.metadata);
      if (format === "go-template={{.type}}") return value.type;
      if (format === 'go-template={{index .data "tls.crt"}}') return value.data["tls.crt"];
    }
    if (args[0] === "get") return JSON.stringify(value);
    if (args[0] !== "patch") throw new Error("Unexpected fixture mutation");
    const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
    expect(patch.metadata.uid).toBe(value.metadata.uid);
    expect(patch.metadata.resourceVersion).toBe(value.metadata.resourceVersion);
    merge(value, patch);
    value.metadata.resourceVersion = String(Number(value.metadata.resourceVersion) + 1);
    if (value.kind === "Deployment" && patch.spec) {
      value.metadata.generation = Number(value.metadata.generation) + 1;
      value.status = { observedGeneration: value.metadata.generation,
        updatedReplicas: value.spec.replicas, availableReplicas: value.spec.replicas };
    }
    return JSON.stringify(value);
  };
  return { objects, pods, calls, execute, key, deployment };
}

export function rootPod(f: ReturnType<typeof fixture>, uid = "old-root") {
  const root = f.objects.get(f.key("deployment", "kars-controller", "core"));
  f.objects.set(f.key("replicasets.apps", "root-rs", "core"), {
    kind: "ReplicaSet", metadata: { name: "root-rs", namespace: "core", uid: "root-rs-uid", resourceVersion: "1",
      ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "kars-controller", uid: "deployment", controller: true }] },
    spec: { template: structuredClone(root.spec.template) },
  });
  return {
    kind: "Pod", metadata: { name: uid, namespace: "core", uid, resourceVersion: "1",
      annotations: structuredClone(root.spec.template.metadata.annotations ?? {}),
      ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "root-rs", uid: "root-rs-uid", controller: true }] },
    spec: structuredClone(root.spec.template.spec),
  };
}
