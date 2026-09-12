// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { expect } from "vitest";
import { bundleDefinition, previewPrivateActivation, type Execute } from "./private-activation.js";

export function fixture() {
  const objects = new Map<string, any>();
  const calls: string[][] = [];
  const key = (kind: string, name: string, namespace = "") => `${kind}/${namespace}/${name}`;
  for (const name of ["work", "core", "reader"]) objects.set(key("namespace", name), {
    kind: "Namespace", metadata: { name, uid: `${name}-uid`, resourceVersion: "1", annotations: {} },
    spec: { finalizers: ["kubernetes"] },
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
      if (format === "jsonpath-as-json={.metadata}") return JSON.stringify([value.metadata]);
      if (format === "go-template={{.type}}") return value.type;
      if (format === 'go-template={{index .data "tls.crt"}}') return value.data["tls.crt"];
      if (format !== "json") throw new Error("Unsupported fixture Secret printer");
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

const GUARD_PREFIX = "kars.azure.com/credential-reader-";

export function privateAuthoritySnapshot(namespace: any): any {
  const current = structuredClone(namespace);
  delete current.metadata.resourceVersion;
  for (const field of ["labels", "annotations"]) {
    for (const key of Object.keys(current.metadata[field] ?? {})) {
      if (key.startsWith(GUARD_PREFIX)) delete current.metadata[field][key];
    }
    if (current.metadata[field] && !Object.keys(current.metadata[field]).length) delete current.metadata[field];
  }
  if (current.metadata.finalizers) {
    current.metadata.finalizers = current.metadata.finalizers.filter((key: string) => !key.startsWith(GUARD_PREFIX));
    if (!current.metadata.finalizers.length) delete current.metadata.finalizers;
  }
  return current;
}

function synchronizeWriterGuards(f: ReturnType<typeof fixture>, grant: any, active: boolean): void {
  const key = `${GUARD_PREFIX}${grant.metadata.uid}`;
  const selected = new Map<string, string>();
  if (active) for (const writer of grant.spec.writers) {
    const namespace = f.objects.get(f.key("namespace", writer.namespace));
    const account = f.objects.get(f.key("serviceaccount", writer.name, writer.namespace));
    expect(namespace.spec.finalizers).toContain("kubernetes");
    expect(account.metadata.uid).toBe(writer.uid);
    selected.set(f.key("namespace", writer.namespace), namespace.metadata.uid);
    selected.set(f.key("serviceaccount", writer.name, writer.namespace), namespace.metadata.uid);
  }
  const controller = f.objects.get(f.key("serviceaccount", "kars-controller", "core")).metadata.uid;
  for (const [path, object] of f.objects) {
    if (!path.startsWith("namespace/") && !path.startsWith("serviceaccount/")) continue;
    const meta = object.metadata;
    const uid = selected.get(path);
    if (uid && meta.labels?.[key] === uid && meta.annotations?.[key] === controller && meta.finalizers?.includes(key)) continue;
    if (!uid && meta.labels?.[key] === undefined) continue;
    meta.finalizers = (meta.finalizers ?? []).filter((value: string) => value !== key);
    if (uid) {
      meta.finalizers.push(key);
      (meta.labels ??= {})[key] = uid;
      (meta.annotations ??= {})[key] = controller;
    } else {
      delete meta.labels[key];
      if (meta.annotations) delete meta.annotations[key];
      for (const field of ["labels", "annotations"]) if (meta[field] && !Object.keys(meta[field]).length) delete meta[field];
      if (!meta.finalizers.length) delete meta.finalizers;
    }
    meta.resourceVersion = String(Number(meta.resourceVersion) + 1);
  }
}

export function continuityFixture() {
  const f = fixture();
  const resource = "karscredentialgrants.kars.azure.com";
  const authority = new Map<string, { metadata: { annotations: Record<string, string> } }>();
  const namespace = (name: string) => f.objects.get(f.key("namespace", name));
  const grant = (name = "work") => f.objects.get(f.key(resource, "workspace", name));
  for (const name of ["second", "third"]) f.objects.set(f.key("namespace", name), {
    kind: "Namespace", metadata: { name, uid: `${name}-uid`, resourceVersion: "1", annotations: {} },
    spec: { finalizers: ["kubernetes"] },
  });
  f.pods.set("core", [rootPod(f)]);
  const execute: Execute = async (args, input) => {
    if (args[1]?.startsWith("roles,")) return JSON.stringify({ metadata: {}, items: [...authority.values()] });
    const result = await f.execute(args, input);
    if (args[0] === "patch" && args[2] === "kars-controller") {
      const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
      if (patch.spec?.replicas === 0) f.pods.set("core", []);
      if (patch.spec?.replicas > 0) f.pods.set("core", [rootPod(f, "new-root")]);
    }
    if (args[0] === "create" || (args[0] === "patch" && args[1] === resource)) {
      const stored = grant(args[0] === "create" ? JSON.parse(input!).metadata.namespace : args[args.indexOf("-n") + 1]);
      stored.metadata.generation = (stored.metadata.generation ?? 0) + 1;
      const active = stored.spec.enabled !== false && stored.spec.writers.length > 0;
      if (active) authority.set(stored.metadata.uid, { metadata: {
        annotations: { "kars.azure.com/credential-grant-owner": stored.metadata.uid },
      } });
      else authority.delete(stored.metadata.uid);
      synchronizeWriterGuards(f, stored, active);
      stored.status = { observedGeneration: stored.metadata.generation,
        conditions: [{ type: "WriterReady", status: active ? "True" : "False" }] };
    }
    return result;
  };
  const preview = (work = "work", consumers: string[] = [], run = execute, profile = "kcm-certificate") =>
    previewPrivateActivation(run, work, [{ namespace: "reader" }], [], "core", profile, consumers);
  const document = async (work = "work", consumers: string[] = [], run = execute) => {
    const existing = grant(work);
    return {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: work, ...(existing ? {
        uid: existing.metadata.uid, resourceVersion: existing.metadata.resourceVersion,
      } : {}) },
      spec: { workspaceUid: namespace(work).metadata.uid,
        enabled: true, writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }],
        privateActivation: await preview(work, consumers, run) },
    };
  };
  const preserved = () => structuredClone({
    root: namespace("core"), reader: privateAuthoritySnapshot(namespace("reader")), work: namespace("work"), deployment: f.deployment,
    grant: grant(), authority: authority.get(grant()?.metadata.uid), pods: f.pods.get("core"),
  });
  return { ...f, execute, preview, document, namespace, grant, authority, preserved };
}
