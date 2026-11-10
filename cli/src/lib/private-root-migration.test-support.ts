// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { expect } from "vitest";
import { rootCertificates } from "node:tls";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  kinds, PRIVATE_PREFIX as P, previewPrivateActivation, templateDigest, type Execute,
} from "./private-activation.js";
import { continuityFixture, rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import { prepareRootMigration, type RootMigrationReceipt } from "./private-root-migration.js";

export const GRANTS = "karscredentialgrants.kars.azure.com";

export const FIELD = `${P}root-retirement`;

export const ORIGINAL = `example.test/controller:latest@sha256:${"1".repeat(64)}`;

export const TARGET = `example.test/controller:latest@sha256:${"2".repeat(64)}`;

export async function setup(budget = false, configure?: (root: any) => void, companionReplicas?: number, foreignReplicas?: number, lateForeign = false,
  configureForeign?: (deployment: any) => void) {
  const f = continuityFixture(), root: any = f.deployment;
  root.apiVersion = "apps/v1";
  root.metadata.annotations = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" };
  root.spec.selector = { matchLabels: { app: "controller" } };
  root.spec.template.spec.containers[0].image = ORIGINAL;
  root.spec.replicas = 2;
  const secret = { type: "kubernetes.io/tls", metadata: { name: "budget-tls", namespace: "core",
    uid: "budget-uid", resourceVersion: "1", annotations: { "kars.azure.com/inference-budget-tls": "v1" } },
    data: { "tls.crt": Buffer.from(rootCertificates[0]!).toString("base64") } };
  const ca = { kind: "ConfigMap", metadata: { name: "kars-inference-budget-contracts", namespace: "core",
    uid: "contracts-uid", resourceVersion: "1", annotations: { "kars.azure.com/inference-budget-contracts": "v1" } },
    data: { "contracts.json": '{"unchanged":"accounts-charges-caps"}', "ca.crt": rootCertificates[0]! } };
  const rotate = (index: number, publicCa = true) => {
    secret.data["tls.crt"] = Buffer.from(rootCertificates[index]!).toString("base64");
    secret.metadata.resourceVersion = String(Number(secret.metadata.resourceVersion) + 1);
    if (publicCa) {
      ca.data["ca.crt"] = rootCertificates[index]!;
      ca.metadata.resourceVersion = String(Number(ca.metadata.resourceVersion) + 1);
    }
  };
  if (budget) {
    root.spec.template.spec.containers[0].env = [
      { name: "KARS_INFERENCE_BUDGET_ENABLED", value: "true" },
      { name: "KARS_INFERENCE_BUDGET_TLS_SECRET", value: "budget-tls" },
      { name: "KARS_NAMESPACE", value: "core" },
    ];
    f.objects.set(f.key("secret", "budget-tls", "core"), secret);
    f.objects.set(f.key("configmap", ca.metadata.name, "core"), ca);
  }
  configure?.(root);
  f.objects.set(f.key("serviceaccount", "bff", "core"), {
    metadata: { name: "bff", namespace: "core", uid: "writer-uid", resourceVersion: "1" },
  });
  const companion: any = companionReplicas === undefined ? undefined : {
    apiVersion: "apps/v1", kind: "Deployment",
    metadata: { name: "kars-bridge-bff", namespace: "core", uid: "bridge-deployment", resourceVersion: "1", generation: 1 },
    spec: { replicas: companionReplicas, selector: { matchLabels: { app: "bff" } },
      template: { metadata: { labels: { app: "bff" } }, spec: { serviceAccountName: "bff",
        containers: [{ name: "bff", image: ORIGINAL, command: ["bff"] }] } } },
    status: { observedGeneration: 1, updatedReplicas: companionReplicas, availableReplicas: companionReplicas },
  };
  if (companion) for (const kind of ["deployment", "deployments.apps"]) {
    f.objects.set(f.key(kind, companion.metadata.name, "core"), companion);
  }
  const foreign: any = foreignReplicas === undefined ? undefined : {
    apiVersion: "apps/v1", kind: "Deployment",
    metadata: { name: "sre", namespace: "kars-sre", uid: "sre-deployment", resourceVersion: "1", generation: 1 },
    spec: { replicas: foreignReplicas, selector: { matchLabels: { app: "sre" } },
      template: { metadata: { labels: { app: "sre" } }, spec: { serviceAccountName: "sre",
        containers: [{ name: "sre", image: ORIGINAL, command: ["sre"] }] } } },
    status: { observedGeneration: 1, updatedReplicas: foreignReplicas, availableReplicas: foreignReplicas },
  };
  if (foreign) {
    configureForeign?.(foreign);
    f.objects.set(f.key("namespace", "kars-sre"), { kind: "Namespace",
      metadata: { name: "kars-sre", uid: "sre-namespace", resourceVersion: "1", annotations: {
        "kars.azure.com/namespace-claim-version": "v1",
        "kars.azure.com/sandbox-name": "sre", "kars.azure.com/sandbox-namespace": "core", "kars.azure.com/sandbox-uid": "sre-sandbox",
      } }, spec: { finalizers: ["kubernetes"] } });
    f.objects.set(f.key("karssandbox", "sre", "core"), { metadata: { name: "sre", namespace: "core", uid: "sre-sandbox",
      resourceVersion: "1", annotations: { "kars.azure.com/namespace-uid": "sre-namespace" } } });
    f.pods.set("kars-sre", []);
    for (const kind of ["deployment", "deployments.apps"]) f.objects.set(f.key(kind, "sre", "kars-sre"), foreign);
  }
  let foreignSequence = 0;
  const foreignPod = () => {
    const uid = `sre-pod-${++foreignSequence}`;
    f.objects.set(f.key("replicasets.apps", "sre-rs", "kars-sre"), {
      kind: "ReplicaSet", metadata: { name: "sre-rs", namespace: "kars-sre", uid: "sre-rs-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "sre", uid: foreign.metadata.uid, controller: true }] },
      spec: { template: structuredClone(foreign.spec.template) },
    });
    return { kind: "Pod", metadata: { name: uid, namespace: "kars-sre", uid, resourceVersion: "1",
      annotations: structuredClone(foreign.spec.template.metadata.annotations ?? {}),
      ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "sre-rs", uid: "sre-rs-uid", controller: true }] },
      spec: structuredClone(foreign.spec.template.spec), status: { phase: "Running", conditions: [{ type: "Ready", status: "True" }] } };
  };
  let companionSequence = 0;
  const companionPod = () => {
    const uid = `bridge-pod-${++companionSequence}`;
    f.objects.set(f.key("replicasets.apps", "bridge-rs", "core"), {
      kind: "ReplicaSet", metadata: { name: "bridge-rs", namespace: "core", uid: "bridge-rs-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: companion.metadata.name, uid: companion.metadata.uid, controller: true }] },
      spec: { template: structuredClone(companion.spec.template) },
    });
    return { kind: "Pod", metadata: { name: uid, namespace: "core", uid, resourceVersion: "1",
      annotations: structuredClone(companion.spec.template.metadata.annotations ?? {}),
      ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "bridge-rs", uid: "bridge-rs-uid", controller: true }] },
      spec: structuredClone(companion.spec.template.spec), status: { phase: "Running", conditions: [{ type: "Ready", status: "True" }] } };
  };
  const isCompanionPod = (pod: any) => pod.metadata.ownerReferences?.[0]?.uid === "bridge-rs-uid";
  f.pods.set("core", [rootPod(f)]);
  const doc = async (includeForeign = true) => ({
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
    metadata: { name: "workspace", namespace: "core" },
    spec: { workspaceUid: "core-uid", enabled: true, agentKeys: ["RETAINED_TOKEN"],
      writers: [{ namespace: "core", name: "bff", uid: "writer-uid" }],
      privateActivation: await previewPrivateActivation(f.execute, "core", [{ namespace: "core" }, ...(foreign && includeForeign ? [{ namespace: "kars-sre" }] : [])], [], "core", "kcm-certificate",
        [...(companion ? ["core/Deployment/kars-bridge-bff"] : []), ...(foreign && includeForeign ? ["kars-sre/Deployment/sre"] : [])]) },
  });
  if (budget) {
    await expect(applyReviewedGrant(f.execute, await doc(!lateForeign))).rejects.toThrow("operator rotation");
    rotate(1);
  }
  await applyReviewedGrant(f.execute, await doc(!lateForeign));
  if (lateForeign) {
    const review = await doc(), existing = f.grant("core");
    Object.assign(review.metadata, { uid: existing.metadata.uid, resourceVersion: existing.metadata.resourceVersion });
    await applyReviewedGrant(f.execute, review);
  }
  if (companion) {
    companion.spec.replicas = companionReplicas;
    companion.status = { observedGeneration: companion.metadata.generation, updatedReplicas: companionReplicas, availableReplicas: companionReplicas };
    f.pods.set("core", [...f.pods.get("core")!, ...Array.from({ length: companionReplicas! }, companionPod)]);
  }
  if (foreign) f.pods.set("kars-sre", Array.from({ length: foreignReplicas! }, foreignPod));
  const grant = structuredClone(f.grant("core")), seal = f.namespace("core").metadata.annotations[FIELD];
  const rendered = structuredClone(root);
  rendered.spec.template.spec.containers[0].image = TARGET;
  const mutations: { args: string[]; input?: string }[] = [];
  const controls = { holdPods: false };
  let rootSequence = 0;
  const execute: Execute = async (args, input) => {
    if (args[0] === "apply") {
      expect(args).toContain("--dry-run=server");
      return input!;
    }
    if (args[0] === "get" && args[1] === "namespaces") return JSON.stringify({ metadata: {},
      items: [...f.objects.entries()].filter(([key]) => key.startsWith("namespace/")).map(([, value]) => value) });
    if (args[0] === "get" && args[1] === Object.values(kinds).join(",")) {
      return JSON.stringify({ metadata: {}, items: [...new Set(f.objects.values())]
        .filter(value => Object.hasOwn(kinds, value.kind ?? "") && value.metadata.namespace === args[args.indexOf("-n") + 1]) });
    }
    if (args[0] === "patch" && args.includes("--type=json")) {
      const ns = args.includes("-n") ? args[args.indexOf("-n") + 1]! : "";
      const value = args[1] === "namespace" ? f.namespace(args[2]!) : f.objects.get(f.key("deployment", args[2]!, ns));
      const projected = structuredClone(value);
      for (const operation of JSON.parse(input!)) {
        const path = operation.path.slice(1).split("/").map((part: string) => part.replaceAll("~1", "/").replaceAll("~0", "~"));
        let parent = projected;
        for (const key of path.slice(0, -1)) parent = parent[key];
        const key = path.at(-1);
        if (operation.op === "test") expect(parent[key]).toEqual(operation.value);
        else {
          expect(operation.op).toBe("replace");
          parent[key] = operation.value;
        }
      }
      if (budget && value === foreign && projected.spec.replicas === 0 && value.spec.replicas !== 0
        && f.namespace("kars-sre").metadata.annotations[`${P}state`] !== "Pending") {
        throw new Error("Budget admission requires Pending before an operator pauses a foreign workload");
      }
      if (args.includes("--dry-run=server")) return JSON.stringify(projected);
      mutations.push({ args, input });
      if (args[1] === "deployment") {
        expect(projected.spec.replicas).toBe(value === foreign && projected.spec.replicas > 0 ? foreignReplicas
          : value === companion && projected.spec.replicas > 0 ? companionReplicas : 0);
        if (templateDigest(projected) !== templateDigest(value) && templateDigest(projected) === templateDigest(rendered)) {
          expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
          expect(f.pods.get("core")).toEqual([]);
        }
        value.spec = projected.spec;
        value.metadata.generation++;
        value.status = { observedGeneration: value.metadata.generation,
          availableReplicas: value.spec.replicas, updatedReplicas: value.spec.replicas };
        if (value === foreign && !controls.holdPods) f.pods.set("kars-sre", Array.from({ length: value.spec.replicas }, foreignPod));
        else if (!controls.holdPods) f.pods.set("core", [
          ...f.pods.get("core")!.filter(pod => isCompanionPod(pod) !== (value === companion)),
          ...(value === companion ? Array.from({ length: value.spec.replicas }, companionPod) : []),
        ]);
      } else value.metadata.annotations = projected.metadata.annotations;
      value.metadata.resourceVersion = String(Number(value.metadata.resourceVersion) + 1);
      return JSON.stringify(value);
    }
    if (!["get", "auth"].includes(args[0]!)) mutations.push({ args, ...(input ? { input } : {}) });
    const companionPods = f.pods.get("core")!.filter(isCompanionPod);
    const result = await f.execute(args, input);
    if (args[0] === "patch" && args[2] === "kars-controller") {
      f.pods.set("core", [...f.pods.get("core")!.filter(pod => !isCompanionPod(pod)), ...companionPods]);
      if (root.spec.replicas > 0 && templateDigest(root) === templateDigest(rendered)) {
        rootSequence++;
        f.pods.set("core", [...companionPods, ...Array.from({ length: root.spec.replicas }, (_, index) => ({
          ...rootPod(f, `migration-root-${rootSequence}-${index}`), status: { phase: "Running", conditions: [{ type: "Ready", status: "True" }] },
        }))]);
      }
    }
    if (companion && args[0] === "patch" && args[2] === companion.metadata.name) {
      f.pods.set("core", [...f.pods.get("core")!.filter(pod => !isCompanionPod(pod)),
        ...Array.from({ length: companion.spec.replicas }, companionPod)]);
    }
    return result;
  };
  const saved: RootMigrationReceipt[] = [];
  const journal = { save: async (receipt: RootMigrationReceipt) => { saved.push(structuredClone(receipt)); } };
  const review = await previewPrivateRootMigration(execute, "core", "kars", [rendered]);
  const prepare = () => prepareRootMigration(execute, review, [rendered], journal);
  const last = () => structuredClone(saved.at(-1)!);
  return { ...f, root, companion, companionPod, foreign, foreignPod, grantBefore: grant, seal, execute, review, rendered, mutations, saved, journal, prepare, last, rotate, secret, ca, controls };
}

export async function alreadyRolled(selection = "combined") {
  const f = await setup(true, root => {
    root.spec.template.spec.containers[0].env.push({
      name: selection === "runner" ? "KARS_CONFORMANCE_RUNNER_IMAGE" : "HERMES_RUNTIME_IMAGE", value: ORIGINAL,
    });
  });
  const historicalRoot = structuredClone(f.root);
  if (selection !== "controller") f.rendered.spec.template.spec.containers[0].env.at(-1).value = TARGET;
  if (selection === "runtime" || selection === "runner") f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
  f.root.spec = structuredClone(f.rendered.spec);
  f.root.metadata.resourceVersion = String(Number(f.root.metadata.resourceVersion) + 1);
  f.root.metadata.generation++;
  f.pods.set("core", [rootPod(f, "rolled-root")]);
  const preview = () => previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered], historicalRoot);
  return { ...f, historicalRoot, preview };
}
