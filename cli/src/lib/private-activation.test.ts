// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { rootCertificates } from "node:tls";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  bundleDefinition, previewPrivateActivation, stagePrivateActivation, validatePrivateActivation,
  validateQualifiedActivation, privateMaterial, PRIVATE_PREFIX,
} from "./private-activation.js";

function fixture() {
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
    kind: "Deployment", metadata: { name: "kars-controller", namespace: "core", uid: "deployment", resourceVersion: "1" },
    spec: { replicas: 1, template: { metadata: {}, spec: { serviceAccountName: "kars-controller",
      containers: [{ name: "controller", image: "fixture", command: ["controller"] }] } } },
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
      value.metadata.uid = "created-grant";
      value.metadata.resourceVersion = "1";
      objects.set(key("karscredentialgrants.kars.azure.com", value.metadata.name, value.metadata.namespace), value);
      return JSON.stringify(value);
    }
    const namespace = args.includes("-n") ? args[args.indexOf("-n") + 1]! : "";
    if (args[0] === "get" && args[1] === "pods") return JSON.stringify({ metadata: {}, items: pods.get(namespace) ?? [] });
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
    return JSON.stringify(value);
  };
  const preview = () => previewPrivateActivation(execute, "work", [{ namespace: "reader" }], [], "core", "kcm-certificate", []);
  return { objects, pods, calls, execute, preview, key, deployment };
}

describe("generic private activation staging", () => {
  it("reviews configurable budget TLS metadata and requires a genuinely different public key before private enrollment", async () => {
    const f = fixture();
    const root = f.objects.get(f.key("deployment", "kars-controller", "core"));
    root.spec.template.spec.containers[0].env = [
      { name: "KARS_INFERENCE_BUDGET_ENABLED", value: "true" },
      { name: "KARS_INFERENCE_BUDGET_TLS_SECRET", value: "operator-budget-tls" },
      { name: "POD_NAMESPACE", valueFrom: { fieldRef: { fieldPath: "metadata.namespace" } } },
    ];
    root.metadata.generation = 1;
    root.status = { observedGeneration: 1, updatedReplicas: 1, availableReplicas: 1 };
    const secret = { type: "kubernetes.io/tls",
      metadata: { name: "operator-budget-tls", namespace: "core", uid: "budget-key", resourceVersion: "1",
        annotations: { "kars.azure.com/inference-budget-tls": "v1" } },
      data: { "tls.crt": Buffer.from(rootCertificates[0]!).toString("base64") } };
    f.objects.set(f.key("secret", "operator-budget-tls", "core"), secret);
    const first = await f.preview();
    expect(first.root.budgetTls?.secret.uid).toBe("budget-key");
    await expect(stagePrivateActivation(f.execute, first)).rejects.toThrow("operator rotation");
    await expect(stagePrivateActivation(f.execute, await f.preview())).rejects.toThrow("public key is unchanged");
    secret.data["tls.crt"] = Buffer.from(rootCertificates[1]!).toString("base64");
    secret.metadata.resourceVersion = "2";
    const staged = await stagePrivateActivation(f.execute, await f.preview());
    expect(staged.root.budgetTls?.keyDigest).not.toBe(first.root.budgetTls?.keyDigest);
    await validateQualifiedActivation(f.execute, staged);
    expect(f.calls.some(args => args[0] === "patch" && args[1] === "secret")).toBe(false);
    expect(f.calls.some(args => args.some(arg => arg.includes("tls.key")))).toBe(false);
    secret.metadata.uid = "replacement-budget-key";
    secret.metadata.resourceVersion = "3";
    await expect(stagePrivateActivation(f.execute, await f.preview())).rejects.toThrow("operator rotation");
  });

  it("treats the governed budget audience as router-private while public CA projection remains non-secret", () => {
    expect(privateMaterial({ containers: [], volumes: [{ projected: { sources: [
      { serviceAccountToken: { audience: "kars.azure.com/governed-inference-budget", path: "token" } },
    ] } }] })).toBe(true);
    expect(privateMaterial({ containers: [], volumes: [{ configMap: { name: "kars-inference-budget-ca" } }] })).toBe(false);
  });

  it("applies a qualified receipt through the existing grant command rather than a separate activation command", async () => {
    const f = fixture();
    const review = await f.preview();
    await applyReviewedGrant(f.execute, {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: "work" },
      spec: { workspaceUid: "work-uid", writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }],
        enabled: true, privateActivation: review },
    });
    const stored = f.objects.get(f.key("karscredentialgrants.kars.azure.com", "workspace", "work"));
    expect(stored.spec.privateActivation.phase).toBe("qualified");
    expect(stored.spec.privateActivation.namespaces.every((scope: any) => scope.epoch.length === 64)).toBe(true);
    expect(f.calls.findIndex(args => args[0] === "create")).toBeGreaterThan(
      f.calls.findIndex(args => args[0] === "patch" && args[1] === "namespace"));
  });

  it("retires writers without removing namespace protection or requiring a new private bootstrap", async () => {
    const f = fixture();
    const existing = {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: "work", uid: "grant", resourceVersion: "1" },
      spec: { workspaceUid: "work-uid", writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }] },
    };
    f.objects.set(f.key("karscredentialgrants.kars.azure.com", "workspace", "work"), structuredClone(existing));
    await applyReviewedGrant(f.execute, { ...existing, spec: { ...existing.spec, writers: [] } });
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === "karscredentialgrants.kars.azure.com")).toBe(true);
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
  });

  it("previews without mutation then stages a namespace-UID-bound fence in the existing enrollment flow", async () => {
    const f = fixture();
    const review = await f.preview();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    const staged = await stagePrivateActivation(f.execute, review);
    expect(staged.phase).toBe("qualified");
    expect(new Set(staged.namespaces.map(scope => scope.epoch)).size).toBe(3);
    for (const scope of staged.namespaces) {
      expect(scope.epoch).toMatch(/^[a-f0-9]{64}$/);
      const current = f.objects.get(f.key("namespace", scope.namespace.name));
      expect(current.metadata.annotations[`${PRIVATE_PREFIX}namespace-uid`]).toBe(scope.namespace.uid);
      expect(current.metadata.annotations[`${PRIVATE_PREFIX}enabled`]).toBe("true");
    }
    await validateQualifiedActivation(f.execute, staged);
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
  });

  it.each(["policy", "binding", "root-uid", "template", "namespace"])("rejects changed %s before any staging mutation", async fault => {
    const f = fixture();
    const review = await f.preview();
    if (fault === "policy") f.objects.get(f.key("validatingadmissionpolicy", "kars-private-consumption")).spec.validations[0].expression = "true";
    if (fault === "binding") f.objects.get(f.key("validatingadmissionpolicybinding", "kars-private-consumption")).spec.matchResources = { namespaceSelector: { matchLabels: { bypass: "true" } } };
    if (fault === "root-uid") f.objects.get(f.key("serviceaccount", "kars-controller", "core")).metadata.uid = "replaced";
    if (fault === "template") f.objects.get(f.key("deployment", "kars-controller", "core")).spec.template.spec.containers[0].command = ["different"];
    if (fault === "namespace") f.objects.get(f.key("namespace", "work")).metadata.resourceVersion = "2";
    await expect(stagePrivateActivation(f.execute, review)).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
  });

  it("rejects old insufficient input and raw nested fields instead of inventing trust", async () => {
    const f = fixture();
    await expect(validatePrivateActivation(f.execute, undefined!)).rejects.toThrow("reviewed private activation");
    const review = await f.preview();
    await expect(validatePrivateActivation(f.execute, { ...review, token: "PRIVATE_VALUE" } as any)).rejects.toThrow("canonical reviewed metadata");
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
  });

  it("preserves unexplained unlabelled and terminating private consumers without issuing an epoch", async () => {
    const f = fixture();
    const review = await f.preview();
    f.pods.set("work", [{
      metadata: { name: "foreign", uid: "foreign", resourceVersion: "1", deletionTimestamp: "2026-01-01T00:00:00Z" },
      spec: { containers: [{ name: "unrelated", image: "fixture" }],
        volumes: [{ name: "identity", secret: { secretName: "router-services-observer-identity" } }] },
    }]);
    await expect(stagePrivateActivation(f.execute, review)).rejects.toThrow("Unexplained private consumer preserved");
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
    expect(f.objects.get(f.key("namespace", "work")).metadata.annotations[`${PRIVATE_PREFIX}epoch`]).toBeUndefined();
    expect(f.objects.get(f.key("namespace", "work")).metadata.annotations[`${PRIVATE_PREFIX}state`]).toBe("Pending");
  });

  it("does not accept a forged Pod execution merely because it names the reviewed ReplicaSet owner", async () => {
    const f = fixture();
    const review = await f.preview();
    f.objects.set(f.key("replicasets.apps", "root-rs", "core"), {
      kind: "ReplicaSet", metadata: { name: "root-rs", uid: "rs", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "kars-controller", uid: "deployment", controller: true }] },
      spec: { template: structuredClone(f.deployment.spec.template) },
    });
    f.pods.set("core", [{
      kind: "Pod", metadata: { name: "forged", uid: "forged", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "root-rs", uid: "rs", controller: true }] },
      spec: { serviceAccountName: "kars-controller", containers: [{ name: "controller", image: "different" }] },
    }]);
    await expect(stagePrivateActivation(f.execute, review)).rejects.toThrow("Consumer execution differs");
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
  });

  it("pins each actual ServiceAccount UID in the explicit service-account controller profile", async () => {
    const f = fixture();
    const review = await previewPrivateActivation(f.execute, "work", [{ namespace: "reader" }], [],
      "core", "service-accounts", []);
    f.objects.get(f.key("serviceaccount", "replicaset-controller", "kube-system")).metadata.uid = "replaced";
    await expect(stagePrivateActivation(f.execute, review)).rejects.toThrow("workload-controller UID changed");
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
  });

  it("does not advance past a writer-retirement acknowledgement while owned read roles still exist", async () => {
    const f = fixture();
    const review = await f.preview();
    const existing: any = {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: "work", uid: "grant", resourceVersion: "1", generation: 1 },
      spec: { workspaceUid: "work-uid", enabled: true, writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }] },
    };
    f.objects.set(f.key("karscredentialgrants.kars.azure.com", "workspace", "work"), existing);
    const execute = async (args: string[], input?: string) => {
      if (args[1]?.startsWith("roles,")) return JSON.stringify({ metadata: {}, items: [
        { metadata: { annotations: { "kars.azure.com/credential-grant-owner": "grant" } } },
      ] });
      const value = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "karscredentialgrants.kars.azure.com") {
        existing.metadata.generation = 2;
        existing.status = { observedGeneration: 2, conditions: [{ type: "WriterReady", status: "False" }] };
      }
      return value;
    };
    const now = vi.spyOn(Date, "now").mockReturnValueOnce(0).mockReturnValue(120_001);
    try {
      await expect(applyReviewedGrant(execute, { ...structuredClone(existing),
        spec: { ...structuredClone(existing.spec), privateActivation: review } })).rejects.toThrow("retirement is still pending");
    } finally { now.mockRestore(); }
    expect(existing.spec.writers).toEqual([]);
    expect(f.calls.some(args => args[0] === "patch" && args[1] === "namespace")).toBe(false);
  });

  it("does not touch an unrelated non-consuming Pod or treat the legacy agent token as private control authority", async () => {
    const f = fixture();
    f.pods.set("work", [{ metadata: { name: "ordinary", uid: "ordinary", resourceVersion: "1" },
      spec: { containers: [{ name: "agent", image: "fixture" }],
        volumes: [{ name: "agent", secret: { secretName: "router-admin-token" } }] } }]);
    const original = structuredClone(f.pods.get("work"));
    await stagePrivateActivation(f.execute, await f.preview());
    expect(f.pods.get("work")).toEqual(original);
    expect(privateMaterial(original![0].spec)).toBe(false);
  });
});
