// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { rootCertificates } from "node:tls";
import { createHash, randomUUID } from "node:crypto";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { gunzipSync, gzipSync } from "node:zlib";
import { publishPrivateCore } from "./private-core-publication.js";
import { historicalHooks } from "./private-core-history.test-support.js";
import { HISTORICAL_HOOK_RETIREMENT } from "./private-core-history.js";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { crd, schemaFixture } from "./schema-stage.test-support.js";
import { schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION, CORE_PUBLICATION_FENCE } from "./private-activation.js";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import {
  at, canonical, digest, kinds, PRIVATE_PREFIX as P, previewPrivateActivation, templateDigest, validateQualifiedActivation, type Execute,
} from "./private-activation.js";
import { continuityFixture, rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import { assertRootImageDelta } from "./private-root-image-delta.js";
import {
  finalizeRootMigration, prepareRootBudgetTlsRotation, prepareRootMigration, prepareRouterBaselineRestart, retireRootMigration, reviewCompletedRootMigration, type RootMigrationReceipt,
} from "./private-root-migration.js";

const GRANTS = "karscredentialgrants.kars.azure.com";
const FIELD = `${P}root-retirement`;
const ORIGINAL = `example.test/controller:latest@sha256:${"1".repeat(64)}`;
const TARGET = `example.test/controller:latest@sha256:${"2".repeat(64)}`;

async function setup(budget = false, configure?: (root: any) => void, companionReplicas?: number, foreignReplicas?: number, lateForeign = false,
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

describe("sealed router baseline restart", () => {
  async function failed() {
    const f = await setup(true, root => root.spec.template.spec.containers[0].env.push(
      { name: "INFERENCE_ROUTER_IMAGE", value: ORIGINAL },
      { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"1".repeat(64)}` },
    ), 1, 1);
    const prepared = await f.prepare();
    // Historical reviews admitted this exact omission; new previews now reject it.
    const env = (prepared.plan.targetSpec as any).template.spec.containers[0].env;
    env.find((entry: any) => entry.name === "INFERENCE_ROUTER_IMAGE").value = TARGET;
    env.find((entry: any) => entry.name === "KARS_INFERENCE_BUDGET_ROUTER_DIGEST").value = `sha256:${"2".repeat(64)}`;
    prepared.plan.review.target = { specDigest: digest(prepared.plan.targetSpec), templateDigest: templateDigest({ spec: prepared.plan.targetSpec }) };
    prepared.planDigest = digest(prepared.plan);
    f.rendered.spec = structuredClone(prepared.plan.targetSpec);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    const stop: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "deployment" && ["sre", "kars-bridge-bff"].includes(args[2]!)) throw new Error("stopped before companion restore");
      return f.execute(args, input);
    };
    await expect(finalizeRootMigration(stop, retired, f.journal, applyReviewedGrant)).rejects.toThrow("stopped before companion restore");
    expect(f.last().phase).toBe("qualifying");
    expect(JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version).toBe(2);
    return f;
  }

  it("re-retires a sealed failed target, retains original intent and key exposure, then requires another rotation", async () => {
    const f = await failed(), source = f.last(), snapshot = canonical(source), writes = f.mutations.length;
    const restart = await prepareRouterBaselineRestart(f.execute, source, f.journal);
    expect(f.mutations).toHaveLength(writes);
    expect(canonical(source)).toBe(snapshot);
    expect(restart.plan.grant).toEqual(source.plan.grant);
    expect(restart.plan.restartFrom).toEqual(source);
    expect((restart.plan.targetSpec as any).template.spec.containers[0].env).toContainEqual({
      name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"1".repeat(64)}`,
    });
    const oldPods = [...f.pods.get("core")!].map(pod => pod.metadata.uid);
    f.rendered.spec = structuredClone(restart.plan.targetSpec);
    const retired = await retireRootMigration(f.execute, restart, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(retired.retirement!.attempt).not.toBe(source.retirement!.attempt);
    expect(retired.retirement!.captured[restart.plan.review.activation.root.namespace.uid]).toEqual(expect.arrayContaining(oldPods));
    expect(retired.retirement!.exposedKeys).toEqual(expect.arrayContaining(source.retirement!.exposedKeys));
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    f.rotate(3);
    const completed = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(completed.phase).toBe("complete");
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    expect(f.companion.spec.replicas).toBe(1);
    expect(f.foreign.spec.replicas).toBe(1);
    expect((await reviewCompletedRootMigration(f.execute, completed)).length).toBe(3);
  });

  it.each(["root", "companion", "writer", "seal", "tls", "projection", "superseded"])("rejects %s drift without a write", async fault => {
    const f = await failed(), source = f.last();
    if (fault === "root") f.root.spec.template.spec.containers[0].command = ["changed"];
    if (fault === "companion") f.companion.spec.replicas = 1;
    if (fault === "writer") f.grant("core").spec.writers = f.grantBefore.spec.writers;
    if (fault === "seal") {
      const seal = JSON.parse(f.namespace("core").metadata.annotations[FIELD]);
      seal.retirementDigest = "changed";
      f.namespace("core").metadata.annotations[FIELD] = canonical(seal);
    }
    if (fault === "tls") f.rotate(3);
    if (fault === "superseded") source.supersededBy = "a".repeat(64);
    const writes = f.mutations.length;
    const run: Execute = async (args, input) => {
      if (fault === "projection" && args[0] === "apply") {
        const projected = JSON.parse(input!);
        projected.spec.template.spec.containers[0].env.pop();
        return JSON.stringify(projected);
      }
      return f.execute(args, input);
    };
    await expect(prepareRouterBaselineRestart(run, source, f.journal)).rejects.toThrow();
    expect(f.mutations).toHaveLength(writes);
  });

  it.each(["retiring", "cutover"])("resumes a successor after a lost %s acknowledgement", async phase => {
    const f = await failed(), restart = await prepareRouterBaselineRestart(f.execute, f.last(), f.journal);
    f.rendered.spec = structuredClone(restart.plan.targetSpec);
    const run: Execute = async (args, input) => {
      const response = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployment" && args[2] === "kars-controller" && f.last().phase === phase) throw new Error("restart acknowledgement lost");
      return response;
    };
    await expect(retireRootMigration(run, restart, f.journal)).rejects.toThrow("restart acknowledgement lost");
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(retired.plan.restartFrom).toEqual(restart.plan.restartFrom);
    expect(f.grant("core").spec.writers).toEqual([]);
  });

  it.each(["target", "grant", "guard", "companion", "keys"])("rejects a rehashed successor with changed %s", async fault => {
    const f = await failed(), restart = await prepareRouterBaselineRestart(f.execute, f.last(), f.journal);
    if (fault === "target") (restart.plan.targetSpec as any).template.spec.containers[0].env.at(-1).value = `sha256:${"2".repeat(64)}`;
    if (fault === "grant") (restart.plan.grant.spec as any).writers = [];
    if (fault === "guard") (restart.plan.guard as any).changed = true;
    if (fault === "companion") (restart.plan.companions![0]!.spec as any).replicas = 0;
    if (fault === "keys") restart.plan.restartFrom!.retirement!.exposedKeys = [];
    restart.plan.review.target = { specDigest: digest(restart.plan.targetSpec), templateDigest: templateDigest({ spec: restart.plan.targetSpec }) };
    restart.planDigest = digest(restart.plan);
    const writes = f.mutations.length;
    await expect(retireRootMigration(f.execute, restart, f.journal)).rejects.toThrow();
    expect(f.mutations).toHaveLength(writes);
  });

  it("does not finalize a superseded source", async () => {
    const f = await failed(), source = { ...f.last(), supersededBy: "a".repeat(64) }, writes = f.mutations.length;
    await expect(finalizeRootMigration(f.execute, source, f.journal, applyReviewedGrant)).rejects.toThrow("superseded");
    expect(f.mutations).toHaveLength(writes);
  });
});

describe("foreign budget retirement ordering", () => {
  it("qualifies a governed companion at zero and waits for its controller instead of writing nonzero replicas", async () => {
    const f = await setup(true, undefined, 1, 1);
    f.namespace("kars-sre").metadata.labels = { "kars.azure.com/inference-budget": "v1" };
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    let controllerResumed = false, operatorEpochOnly = false;
    const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "deployment" && args[2] === "sre" && args.includes("--type=json")) {
        const spec = JSON.parse(input!).find((entry: any) => entry.path === "/spec" && entry.op === "replace")?.value;
        expect(spec.replicas).toBe(0);
        if (!args.includes("--dry-run=server")) operatorEpochOnly = true;
      }
      const response = await f.execute(args, input);
      if (!controllerResumed && operatorEpochOnly && args[0] === "get" && args[1] === "deployment" && args[2] === "sre") {
        controllerResumed = true;
        expect(f.namespace("kars-sre").metadata.annotations[`${P}state`]).toBe("Qualified");
        f.foreign.spec.replicas = 1;
        f.foreign.metadata.generation++;
        f.foreign.status = { observedGeneration: f.foreign.metadata.generation, updatedReplicas: 1, availableReplicas: 1 };
        f.pods.set("kars-sre", [f.foreignPod()]);
        return JSON.stringify(f.foreign);
      }
      return response;
    };
    const complete = await finalizeRootMigration(run, retired, f.journal, applyReviewedGrant);
    expect(operatorEpochOnly).toBe(true);
    expect(controllerResumed).toBe(true);
    expect(complete.phase).toBe("complete");
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it.each([false, true])("resumes after Pending is persisted before pausing a foreign scope (late=%s)", async late => {
    const f = await setup(true, undefined, 1, 1, late);
    const prepared = await f.prepare();
    const before = structuredClone(f.namespace("kars-sre").metadata.annotations);
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-sre" && !args.includes("--dry-run=server")) {
        expect(f.grant("core").spec.writers ?? []).toEqual([]);
        throw new Error("interrupted after namespace pause fence");
      }
      return result;
    };
    await expect(retireRootMigration(execute, prepared, f.journal)).rejects.toThrow("interrupted after namespace pause fence");
    expect(f.last().phase).toBe("retiring");
    expect(f.foreign.spec.replicas).toBe(1);
    expect(f.namespace("kars-sre").metadata.annotations).toEqual({ ...before, [`${P}state`]: "Pending" });
    await expect(validateQualifiedActivation(f.execute, f.grantBefore.spec.privateActivation)).rejects.toThrow();
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.foreign.spec.replicas).toBe(0);
    f.rotate(2);
    expect((await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).phase).toBe("complete");
  });

  it.each(["epoch", "bundle-revision", "parent-sre-deployment"])("rejects altered %s on a Pending retirement scope", async field => {
    const f = await setup(true, undefined, 1, 1);
    const prepared = await f.prepare();
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-sre" && !args.includes("--dry-run=server")) {
        throw new Error("interrupted after namespace pause fence");
      }
      return result;
    };
    await expect(retireRootMigration(execute, prepared, f.journal)).rejects.toThrow("interrupted after namespace pause fence");
    f.namespace("kars-sre").metadata.annotations[`${P}${field}`] = "changed";
    const writes = f.mutations.length;
    await expect(retireRootMigration(f.execute, f.last(), f.journal)).rejects.toThrow();
    expect(f.mutations).toHaveLength(writes);
    expect(f.foreign.spec.replicas).toBe(1);
  });
});

describe("controller reconciliation during migration restoration", () => {
  async function restoring() {
    const f = await setup(true, undefined, 1, 1);
    f.objects.get(f.key("karssandbox", "sre", "core")).metadata.generation = 1;
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, async () => {
      throw new Error("interrupted before writer restoration");
    })).rejects.toThrow("interrupted before writer restoration");
    const scope = JSON.parse(f.namespace("core").metadata.annotations[FIELD]).activation.namespaces
      .find((scope: any) => scope.namespace.name === "kars-sre");
    const receipt = { epoch: scope.epoch, baseline: scope.consumers[0].templateDigest,
      structure: templateDigest(f.foreign), template: templateDigest(f.foreign), sandbox_uid: "sre-sandbox", generation: 1 };
    f.namespace("kars-sre").metadata.annotations[`${P}reconciled-sre-deployment`] = JSON.stringify(receipt);
    return { ...f, receipt };
  }

  it("resumes with a verified controller receipt and retains completed migration review", async () => {
    const f = await restoring();
    const completed = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
    expect(completed.phase).toBe("complete");
    await expect(reviewCompletedRootMigration(f.execute, completed)).resolves.toHaveLength(3);
    expect(f.namespace("kars-sre").metadata.annotations[`${P}reconciled-sre-deployment`]).toBe(JSON.stringify(f.receipt));
  });

  it.each(["epoch", "baseline", "structure", "template", "sandbox_uid", "generation", "extra", "unknown-consumer"])(
    "rejects an invalid or unreviewed reconciliation receipt before restoring writers: %s", async field => {
      const f = await restoring();
      const fields = f.namespace("kars-sre").metadata.annotations;
      if (field === "unknown-consumer") fields[`${P}reconciled-unreviewed`] = JSON.stringify(f.receipt);
      else fields[`${P}reconciled-sre-deployment`] = JSON.stringify({ ...f.receipt, [field]: "changed" });
      const writes = f.mutations.length;
      await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
      expect(f.mutations).toHaveLength(writes);
      expect(f.grant("core").spec.writers ?? []).toEqual([]);
    });
});

describe("controller credential rollout migration review", () => {
  async function rollout() {
    const f = await setup(true, undefined, 1, 1);
    f.objects.get(f.key("karssandbox", "sre", "core")).metadata.generation = 1;
    const baseline = structuredClone(f.objects.get(f.key("replicasets.apps", "sre-rs", "kars-sre")));
    baseline.apiVersion = "apps/v1";
    baseline.spec.template.metadata.labels["pod-template-hash"] = "retained";
    f.foreign.spec.template.metadata.annotations = {
      ...f.foreign.spec.template.metadata.annotations,
      [`${P}epoch`]: f.namespace("kars-sre").metadata.annotations[`${P}epoch`],
      "kars.azure.com/inference-providers-version": "new-provider-version",
    };
    f.pods.set("kars-sre", [f.foreignPod()]);
    const history = { metadata: {}, items: [baseline] };
    const execute: Execute = (args, input) => args[0] === "get" && args[1] === "replicasets.apps" && args[2] === "-n"
      ? Promise.resolve(JSON.stringify(history)) : f.execute(args, input);
    return { ...f, execute, baseline, history };
  }

  it("reviews and retires an annotation-only rollout with its exact owned template witness", async () => {
    const f = await rollout();
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    const consumer = review.activation.namespaces.find(s => s.namespace.name === "kars-sre")!.consumers[0]!;
    expect(consumer.templateDigest).toBe(templateDigest(f.foreign));
    expect(f.namespace("core").metadata.annotations[FIELD]).toBe(f.seal);
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    f.rotate(2);
    const completed = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(completed.phase).toBe("complete");
  });

  it.each(["baseline", "template", "namespace", "deployment", "duplicate", "missing"])(
    "rejects edited rollout review before retirement: %s", async fault => {
      const f = await rollout();
      const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
      const writes = f.mutations.length, entries = f.saved.length;
      const transition = review.credentialRollouts![0]!;
      if (fault === "baseline") transition.baseline = "0".repeat(64);
      if (fault === "template") transition.template = "0".repeat(64);
      if (fault === "namespace") transition.namespaceUid = "replacement";
      if (fault === "deployment") transition.deploymentUid = review.activation.root.deployment.uid;
      if (fault === "duplicate") review.credentialRollouts!.push(structuredClone(transition));
      if (fault === "missing") delete review.credentialRollouts;
      await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow();
      expect(f.mutations).toHaveLength(writes);
      expect(f.saved).toHaveLength(entries);
    },
  );

  it.each(["image", "label", "unrelated-annotation", "namespace", "sandbox", "generation", "owner", "baseline", "missing", "page", "epoch"])(
    "rejects invalid rollover history without writes: %s", async fault => {
      const f = await rollout(), writes = f.mutations.length;
      if (fault === "image") f.foreign.spec.template.spec.containers[0].image = TARGET;
      if (fault === "label") f.foreign.spec.template.metadata.labels.app = "changed";
      if (fault === "unrelated-annotation") f.foreign.spec.template.metadata.annotations.other = "changed";
      if (fault === "namespace") f.namespace("kars-sre").metadata.uid = "replaced";
      if (fault === "sandbox") f.objects.get(f.key("karssandbox", "sre", "core")).metadata.uid = "replaced";
      if (fault === "generation") f.objects.get(f.key("karssandbox", "sre", "core")).metadata.generation = 0;
      if (fault === "owner") f.baseline.metadata.ownerReferences[0].uid = "foreign";
      if (fault === "baseline") f.baseline.spec.template.spec.containers[0].image = TARGET;
      if (fault === "missing") f.history.items = [];
      if (fault === "page") Object.assign(f.history.metadata, { continue: "next-page" });
      if (fault === "epoch") f.foreign.spec.template.metadata.annotations[`${P}epoch`] = "0".repeat(64);
      await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered])).rejects.toThrow();
      expect(f.mutations).toHaveLength(writes);
    },
  );
});

describe("completed migration no-op review", () => {
  it.each(["valid", "replicas", "image", "uid", "journal", "incomplete", "writers"])("reviews retained intent without writes: %s", async fault => {
    const f = await setup(true, undefined, 1, 0);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    const receipt = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    const saved = canonical(receipt), mutations = f.mutations.length, entries = f.saved.length;
    if (fault === "replicas") f.companion.spec.replicas = 0;
    if (fault === "image") f.companion.spec.template.spec.containers[0].image = TARGET;
    if (fault === "uid") f.companion.metadata.uid = "replacement";
    if (fault === "journal") receipt.planDigest = "0".repeat(64);
    if (fault === "incomplete") receipt.phase = "qualifying";
    if (fault === "writers") f.grant("core").spec.writers = [];
    if (fault === "valid") {
      const deployments = await reviewCompletedRootMigration(f.execute, receipt);
      expect(deployments.map(value => [at(value, "metadata", "uid"), at(value, "spec", "replicas")])).toEqual([
        [f.root.metadata.uid, 2], [f.companion.metadata.uid, 1], [f.foreign.metadata.uid, 0],
      ]);
      expect(canonical(receipt)).toBe(saved);
    } else await expect(reviewCompletedRootMigration(f.execute, receipt)).rejects.toThrow();
    expect(f.mutations).toHaveLength(mutations);
    expect(f.saved).toHaveLength(entries);
  });
});

describe("multi-scope root migration", () => {
  async function migration(replicas = 1, configureForeign?: (deployment: any) => void, configureRoot?: (root: any) => void) {
    const f = await setup(true, configureRoot, 1, replicas, false, configureForeign);
    const target = structuredClone(f.companion);
    target.spec.template.spec.containers[0].image = TARGET;
    const documents = [f.rendered, target];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, [target.metadata.name]);
    const prepared = await prepareRootMigration(f.execute, review, documents, f.journal);
    return { ...f, target, documents, prepared };
  }

  it("withholds writers and refuses resume after fresh-epoch control material changes the sealed SRE template", async () => {
    const version = "kars.azure.com/services-credential-version";
    const before = `control-uid:control-v1:${"a".repeat(64)}`;
    const after = `control-uid:control-v1:${"b".repeat(64)}`;
    const f = await migration(0, deployment => {
      deployment.spec.template.metadata.annotations = { [version]: before };
      deployment.spec.template.spec.serviceAccountName = "sre-api-router";
    });
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    f.rotate(2);
    let changed = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!changed && args[0] === "patch" && args[1] === "namespace" && args[2] === "core"
        && JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version === 2) {
        changed = true;
        f.foreign.spec.template.metadata.annotations[version] = after;
        f.foreign.metadata.resourceVersion = String(Number(f.foreign.metadata.resourceVersion) + 1);
      }
      return result;
    };
    await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant))
      .rejects.toThrow("fixture object unavailable");
    expect(changed).toBe(true);
    const sealed = f.namespace("core").metadata.annotations[FIELD];
    const scope = JSON.parse(sealed).activation.namespaces.find((value: any) => value.namespace.name === "kars-sre");
    expect(scope.epoch).not.toBe(f.review.activation.namespaces.find(value => value.namespace.name === "kars-sre")!.epoch);
    expect(f.foreign.spec.template.metadata.annotations[`${P}epoch`]).toBe(scope.epoch);
    expect(scope.consumers[0].templateDigest).not.toBe(templateDigest(f.foreign));
    expect(f.foreign.metadata.uid).toBe(scope.consumers[0].object.uid);
    expect(f.foreign.spec.replicas).toBe(0);
    expect(f.pods.get("kars-sre")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    expect(f.last().phase).toBe("qualifying");
    expect(f.last().retirement).toEqual(retired.retirement);
    const writes = f.mutations.length, journalEntries = f.saved.length;
    await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant))
      .rejects.toThrow("fixture object unavailable");
    expect(f.mutations).toHaveLength(writes);
    expect(f.saved).toHaveLength(journalEntries);
    expect(f.namespace("core").metadata.annotations[FIELD]).toBe(sealed);
    expect(f.grant("core").spec.writers).toEqual([]);
  });

  it.each(["valid", "pre-seal", "pre-seal-inline", "seal-ack", "journal-ack", "secret-uid", "secret-epoch", "secret-owner", "hash", "namespace-claim", "source-uid", "template", "pods", "writers", "extra-material"])(
    "verifies fresh control material and preserves retired evidence: %s", async fault => {
      const marker = "kars.azure.com/services-credential-version";
      const before = `control-uid:control-v1:${digest({ "control-token": "b2xk" })}`;
      const data: Record<string, string> = { "builtin-memory.json": "e30=", "control-token": "bmV3" };
      const after = `control-uid:control-v1:${digest({ "control-token": data["control-token"] })}`;
      const f = await migration(0, deployment => {
        deployment.spec.template.metadata.annotations = { [marker]: before };
        deployment.spec.template.spec.serviceAccountName = "sre-api-router";
      });
      const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
      f.rotate(2);
      let rotated = false;
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        const state = JSON.parse(f.namespace("core").metadata.annotations[FIELD]);
        const boundary = fault.startsWith("pre-seal")
          ? args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller"
            && state.version === 1 && state.phase === "restoring" && f.root.spec.replicas > 0
          : args[0] === "patch" && args[1] === "namespace" && args[2] === "core" && state.version === 2;
        if (!rotated && boundary) {
          rotated = true;
          f.foreign.spec.template.metadata.annotations[marker] = after;
          f.foreign.metadata.resourceVersion = String(Number(f.foreign.metadata.resourceVersion) + 1);
          f.objects.set(f.key("secret", "router-services-admin", "kars-sre"), {
            type: "Opaque", metadata: { name: "router-services-admin", namespace: "kars-sre", uid: "control-uid", resourceVersion: "2",
              labels: { "app.kubernetes.io/managed-by": "kars-controller" }, annotations: {
                "kars.azure.com/sandbox-uid": "sre-sandbox", "kars.azure.com/namespace-uid": "sre-namespace",
                [`${P}epoch`]: f.namespace("kars-sre").metadata.annotations[`${P}epoch`],
              } }, data,
          });
          if (fault !== "pre-seal-inline") throw new Error("qualification acknowledgement lost after control rotation");
        }
        return result;
      };
      if (fault === "pre-seal-inline") {
        const complete = await finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant);
        expect(complete.phase).toBe("complete");
        expect(complete.controlTransition?.qualification?.phase).toBe("qualified");
        expect(complete.retirement).toEqual(retired.retirement);
        await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
        return;
      }
      await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("qualification acknowledgement lost");
      const sealed = f.namespace("core").metadata.annotations[FIELD];
      const material = f.objects.get(f.key("secret", "router-services-admin", "kars-sre")) as any;
      if (fault === "secret-uid") material.metadata.uid = "replacement";
      if (fault === "secret-epoch") material.metadata.annotations[`${P}epoch`] = "old-epoch";
      if (fault === "secret-owner") material.metadata.annotations["kars.azure.com/sandbox-uid"] = "foreign-source";
      if (fault === "hash") material.data["control-token"] = "d3Jvbmc=";
      if (fault === "namespace-claim") delete f.namespace("kars-sre").metadata.annotations["kars.azure.com/namespace-claim-version"];
      if (fault === "source-uid") (f.objects.get(f.key("karssandbox", "sre", "core")) as any).metadata.uid = "replacement";
      if (fault === "template") f.foreign.spec.template.spec.containers[0].command = ["unexpected"];
      if (fault === "pods") f.pods.set("kars-sre", [f.foreignPod()]);
      if (fault === "writers") f.grant("core").spec.writers = f.grantBefore.spec.writers;
      if (fault === "extra-material") material.data["unexpected"] = "dW5leHBlY3RlZA==";
      if (!["valid", "pre-seal", "seal-ack", "journal-ack"].includes(fault)) {
        const writes = f.mutations.length;
        await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
        expect(f.mutations).toHaveLength(writes);
        expect(f.namespace("core").metadata.annotations[FIELD]).toBe(sealed);
        expect(f.last().retirement).toEqual(retired.retirement);
        return;
      }
      let interrupted = false;
      const resume: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (fault === "seal-ack" && !interrupted && args[0] === "patch" && args[1] === "namespace" && args[2] === "core") {
          interrupted = true;
          throw new Error("transition seal acknowledgement lost");
        }
        return result;
      };
      const journal = { save: async (receipt: RootMigrationReceipt) => {
        await f.journal.save(receipt);
        if (fault === "journal-ack" && !interrupted && receipt.controlTransition?.applied) {
          interrupted = true;
          throw new Error("transition journal acknowledgement lost");
        }
      } };
      if (["seal-ack", "journal-ack"].includes(fault)) await expect(finalizeRootMigration(resume, f.last(), journal, applyReviewedGrant)).rejects.toThrow("acknowledgement lost");
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(complete.retirement).toEqual(retired.retirement);
      expect(complete.controlTransition?.before).toBe(sealed);
      expect(complete.controlTransition?.applied).toBe(true);
      expect(f.foreign.spec.replicas).toBe(0);
      expect(f.foreign.spec.template.metadata.annotations[marker]).toBe(after);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
      expect((await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant)).phase).toBe("complete");
    },
  );

  it.each(["before-prepare", "prepared", "sealed", "applied", "suspended-recovery", "held-recovery", "empty-providers", "null-providers",
    "provider-hash", "ca-hash", "ca-contract", "tls-owner", "tls-epoch", "tls-uid", "tls-expired", "source-owner", "source-generation",
    "runtime-owner", "registration-not-ready", "registration-stale", "registration-generation", "registration-legacy-access",
    "suspended", "held", "extra-annotation", "command", "stale-pod", "foreign-pod",
    "image-hermes", "image-openclaw", "image-pinned", "image-paused", "image-agent", "image-guard", "image-controller",
    "image-mutable", "image-kind", "image-command", "image-duplicate", "image-router", "image-pod", "image-rs",
    "image-prepared", "image-sealed", "image-applied", "image-router-refresh", "image-router-only",
    "image-router-openclaw", "image-router-controller", "image-router-mutable", "image-router-duplicate"])(
    "qualifies only source-derived SRE recovery without losing the retired transition: %s", async fault => {
      const imageChange = fault.startsWith("image-");
      const routerChange = fault.startsWith("image-router-");
      const routerOnly = ["image-router-only", "image-router-openclaw"].includes(fault);
      const timing = imageChange ? fault.slice("image-".length) : fault;
      const hermesImage = `example.test/hermes:latest@sha256:${"3".repeat(64)}`;
      const valid = ["before-prepare", "prepared", "sealed", "applied", "suspended-recovery", "held-recovery", "empty-providers", "null-providers",
        "image-hermes", "image-openclaw", "image-pinned", "image-paused", "image-prepared", "image-sealed", "image-applied",
        "image-router-refresh", "image-router-only", "image-router-openclaw"].includes(fault);
      const paused = ["suspended-recovery", "held-recovery", "image-paused"].includes(fault);
      const marker = "kars.azure.com/services-credential-version";
      const before = `control-uid:control-v1:${digest({ "control-token": "b2xk" })}`;
      const data = { "builtin-memory.json": "e30=", "control-token": "bmV3" };
      const after = `control-uid:control-v1:${digest({ "control-token": data["control-token"] })}`;
      const derived = ["inference-budget-ca-version", "inference-providers-version", "sre-private-credential-uid", "sre-tls-expiry"];
      const f = await migration(0, deployment => {
        deployment.spec.template.metadata.annotations = { [marker]: before,
          "kars.azure.com/sre-registration-uid": "registration-uid", "kars.azure.com/sre-privacy-epoch": "sre-privacy",
          ...Object.fromEntries(derived.map(key => [`kars.azure.com/${key}`, "old"])) };
        deployment.spec.template.spec.serviceAccountName = "sre-api-router";
        if (imageChange) {
          deployment.spec.template.spec.containers[0].name = "agent";
          deployment.spec.template.spec.containers.push({ name: "inference-router", image: ORIGINAL });
          deployment.spec.template.spec.initContainers = [{ name: "egress-guard", image: ORIGINAL, command: ["guard"] }];
        }
      }, imageChange ? root => {
        root.spec.template.spec.containers[0].env.push(
          { name: "SANDBOX_IMAGE", value: routerOnly ? ORIGINAL : TARGET },
          { name: "HERMES_RUNTIME_IMAGE", value: routerOnly ? ORIGINAL : hermesImage },
          { name: "INFERENCE_ROUTER_IMAGE", value: routerChange ? TARGET : ORIGINAL });
      } : undefined);
      const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
      f.rotate(2);
      let rotated = false;
      const rotate: Execute = async (args, input) => {
        const response = await f.execute(args, input);
        const state = JSON.parse(f.namespace("core").metadata.annotations[FIELD]);
        if (!rotated && args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller"
          && state.version === 1 && state.phase === "restoring" && f.root.spec.replicas > 0) {
          rotated = true;
          f.foreign.spec.template.metadata.annotations[marker] = after;
          f.objects.set(f.key("secret", "router-services-admin", "kars-sre"), {
            type: "Opaque", metadata: { name: "router-services-admin", namespace: "kars-sre", uid: "control-uid", resourceVersion: "2",
              labels: { "app.kubernetes.io/managed-by": "kars-controller" }, annotations: {
                "kars.azure.com/sandbox-uid": "sre-sandbox", "kars.azure.com/namespace-uid": "sre-namespace",
                [`${P}epoch`]: f.namespace("kars-sre").metadata.annotations[`${P}epoch`],
              } }, data,
          });
          throw new Error("rotated before root seal");
        }
        return response;
      };
      await expect(finalizeRootMigration(rotate, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotated before root seal");
      const originalSeal = f.namespace("core").metadata.annotations[FIELD];
      const source = f.objects.get(f.key("karssandbox", "sre", "core")) as any;
      source.metadata.generation = 2;
      source.spec = { suspended: false };
      if (imageChange) source.spec.runtime = {
        kind: ["image-openclaw", "image-pinned", "image-router-openclaw"].includes(fault) ? "OpenClaw" : "Hermes",
        ...(fault === "image-pinned" ? { openclaw: { image: ORIGINAL } } : {}),
      };
      const registration = { metadata: { name: "canonical", uid: "registration-uid", resourceVersion: "1", generation: 1 },
        spec: { enabled: true, sandbox: { name: "sre", namespace: "core", uid: "sre-sandbox" },
          runtimeNamespace: { name: "kars-sre", uid: "sre-namespace" } },
        status: { phase: "Ready", observedGeneration: 1, legacySecretAccessDenied: true, privacyEpoch: "sre-privacy" } };
      f.objects.set(f.key("karssreregistrations.kars.azure.com", "canonical"), registration);
      const catalog = f.objects.get(f.key("configmap", "kars-inference-budget-contracts", "core")) as any;
      const expires = String(Math.floor(Date.now() / 1000) + 3600);
      const tls = { metadata: { name: "sre-api-router-identity", namespace: "kars-sre", uid: "tls-uid", resourceVersion: "2",
        annotations: { "kars.azure.com/sre-registration-uid": "registration-uid", "kars.azure.com/sre-privacy-epoch": "sre-privacy",
          "kars.azure.com/sre-tls-expiry": expires } } };
      f.objects.set(f.key("secret", "sre-api-router-identity", "kars-sre"), tls);
      const providers = { metadata: { name: "kars-inference-providers", namespace: "core", uid: "provider-uid", resourceVersion: "3" },
        data: { API_KEY: "cHJvdmlkZXI=" } };
      if (fault === "empty-providers") Reflect.deleteProperty(providers, "data");
      if (fault === "null-providers") Reflect.set(providers, "data", null);
      f.objects.set(f.key("secret", "kars-inference-providers", "core"), providers);
      let recovered = false;
      const recover = () => {
        recovered = true;
        if (imageChange) {
          f.foreign.spec.template.spec.containers[0].image = routerOnly || fault === "image-pinned" ? ORIGINAL : fault === "image-openclaw" ? TARGET : hermesImage;
          f.foreign.spec.template.spec.initContainers[0].image = routerOnly ? ORIGINAL : TARGET;
          if (routerChange) f.foreign.spec.template.spec.containers[1].image = TARGET;
        }
        Object.assign(f.foreign.spec.template.metadata.annotations, {
          "kars.azure.com/inference-budget-ca-version": createHash("sha256").update(rootCertificates[2]!).digest("hex"),
          "kars.azure.com/inference-providers-version": digest([providers.metadata.uid, providers.data ?? null]),
          "kars.azure.com/sre-private-credential-uid": tls.metadata.uid, "kars.azure.com/sre-tls-expiry": expires,
        });
        f.foreign.spec.replicas = 1;
        f.foreign.status.updatedReplicas = 1;
        f.foreign.status.availableReplicas = 1;
        f.pods.set("kars-sre", [f.foreignPod()]);
      };
      if (!["prepared", "sealed", "applied"].includes(timing)) recover();
      const journal = { save: async (receipt: RootMigrationReceipt) => {
        await f.journal.save(receipt);
        if (!recovered && receipt.controlTransition && ((timing === "prepared" && !receipt.controlTransition.applied)
          || (timing === "applied" && receipt.controlTransition.applied))) {
          recover();
          throw new Error("SRE recovered across acknowledgement");
        }
      } };
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!recovered && timing === "sealed" && args[0] === "patch" && args[1] === "namespace" && args[2] === "core"
          && JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version === 2) {
          recover();
          throw new Error("SRE recovered across acknowledgement");
        }
        return result;
      };
      if (["prepared", "sealed", "applied"].includes(timing)) {
        await expect(finalizeRootMigration(execute, f.last(), journal, applyReviewedGrant)).rejects.toThrow("SRE recovered across acknowledgement");
      }
      if (fault === "provider-hash") f.foreign.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"] = "wrong";
      if (fault === "ca-hash") f.foreign.spec.template.metadata.annotations["kars.azure.com/inference-budget-ca-version"] = "wrong";
      if (fault === "ca-contract") catalog.metadata.annotations["kars.azure.com/inference-budget-contracts"] = "wrong";
      if (fault === "tls-owner") tls.metadata.annotations["kars.azure.com/sre-registration-uid"] = "foreign";
      if (fault === "tls-epoch") tls.metadata.annotations["kars.azure.com/sre-privacy-epoch"] = "foreign";
      if (fault === "tls-uid") tls.metadata.uid = "replaced";
      if (fault === "tls-expired") tls.metadata.annotations["kars.azure.com/sre-tls-expiry"] = "1";
      if (fault === "source-owner") registration.spec.sandbox.uid = "foreign";
      if (fault === "source-generation") source.metadata.generation = 0;
      if (fault === "runtime-owner") registration.spec.runtimeNamespace.uid = "foreign";
      if (fault === "registration-not-ready") registration.status.phase = "Migrating";
      if (fault === "registration-stale") registration.status.observedGeneration = 0;
      if (fault === "registration-generation") registration.status.observedGeneration = registration.metadata.generation = 0;
      if (fault === "registration-legacy-access") registration.status.legacySecretAccessDenied = false;
      if (fault === "image-agent") f.foreign.spec.template.spec.containers[0].image = ORIGINAL;
      if (fault === "image-guard") f.foreign.spec.template.spec.initContainers[0].image = hermesImage;
      if (fault === "image-controller") f.root.spec.template.spec.containers[0].env.find((entry: any) => entry.name === "HERMES_RUNTIME_IMAGE").value = ORIGINAL;
      if (fault === "image-mutable") f.foreign.spec.template.spec.containers[0].image = "example.test/hermes:latest";
      if (fault === "image-kind") source.spec.runtime.kind = "Byo";
      if (fault === "image-command") f.foreign.spec.template.spec.initContainers[0].command = ["changed"];
      if (fault === "image-duplicate") f.foreign.spec.template.spec.containers.push(structuredClone(f.foreign.spec.template.spec.containers[0]));
      if (fault === "image-router") f.foreign.spec.template.spec.containers[1].image = TARGET;
      if (fault === "image-router-controller") f.root.spec.template.spec.containers[0].env.find((entry: any) => entry.name === "INFERENCE_ROUTER_IMAGE").value = ORIGINAL;
      if (fault === "image-router-mutable") f.foreign.spec.template.spec.containers[1].image = "example.test/router:latest";
      if (fault === "image-router-duplicate") f.foreign.spec.template.spec.containers.push(structuredClone(f.foreign.spec.template.spec.containers[1]));
      if (fault === "image-pod") f.pods.get("kars-sre")![0].spec.containers[0].image = ORIGINAL;
      if (fault === "image-rs") (f.objects.get(f.key("replicasets.apps", "sre-rs", "kars-sre")) as any).spec.template.spec.containers[0].image = ORIGINAL;
      if (["suspended", "suspended-recovery", "image-paused"].includes(fault)) source.spec.suspended = true;
      if (["held", "held-recovery"].includes(fault)) source.metadata.annotations["kars.azure.com/credential-rebind-task-uid"] = "held";
      if (paused) {
        f.foreign.spec.replicas = 0;
        f.foreign.status.updatedReplicas = f.foreign.status.availableReplicas = 0;
        f.pods.set("kars-sre", []);
      }
      if (fault === "extra-annotation") f.foreign.spec.template.metadata.annotations.unexpected = "changed";
      if (fault === "command") f.foreign.spec.template.spec.containers[0].command = ["changed"];
      if (fault === "stale-pod") f.pods.get("kars-sre")![0].metadata.annotations[`${P}epoch`] = "old";
      if (fault === "foreign-pod") f.pods.get("kars-sre")![0].metadata.ownerReferences[0].uid = "foreign";
      if (!valid) {
        const writes = f.mutations.length;
        await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
        expect(f.mutations).toHaveLength(writes);
        expect(f.grant("core").spec.writers).toEqual([]);
        expect(f.namespace("core").metadata.annotations[FIELD]).toBe(originalSeal);
        return;
      }
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(complete.retirement).toEqual(retired.retirement);
      expect(complete.controlTransition?.before).toBe(originalSeal);
      expect(complete.controlTransition?.consumers[0]?.reconciliation).toEqual({ sourceUid: "sre-sandbox", sourceGeneration: 2, replicas: paused ? 0 : 1,
        ...(imageChange ? { runtime: expect.objectContaining({ kind: source.spec.runtime.kind }) } : {}) });
      expect(!!complete.controlTransition?.previous).toBe(["prepared", "sealed", "applied"].includes(timing));
      expect(f.foreign.spec.replicas).toBe(paused ? 0 : 1);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
      expect((await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant)).phase).toBe("complete");
      const writes = f.mutations.length, retained = canonical(complete);
      const reviewedDeployments = await reviewCompletedRootMigration(f.execute, complete);
      expect(reviewedDeployments).toContainEqual(expect.objectContaining({
        metadata: expect.objectContaining({ uid: f.foreign.metadata.uid }),
        spec: expect.objectContaining({ replicas: paused ? 0 : 1 }),
      }));
      expect(canonical(complete)).toBe(retained);
      expect(f.mutations).toHaveLength(writes);
      f.foreign.spec.replicas = paused ? 1 : 0;
      await expect(reviewCompletedRootMigration(f.execute, complete)).rejects.toThrow();
      expect(f.mutations).toHaveLength(writes);
      expect(canonical(complete)).toBe(retained);
    },
  );

  it("retains a qualified SRE scope enrolled after the original root seal", async () => {
    const f = await setup(true, undefined, 1, 0, true);
    expect(JSON.parse(f.seal).activation.namespaces).toHaveLength(1);
    expect(f.review.activation.namespaces).toHaveLength(2);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    const complete = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    expect(JSON.parse(f.namespace("core").metadata.annotations[FIELD]).activation.namespaces).toHaveLength(2);
    expect(f.foreign.spec.replicas).toBe(0);
    expect(f.grant("core").spec.privateActivation.namespaces).toHaveLength(2);
  });

  it.each(["qualification", "namespace-drift", "consumer-drift"])("resumes qualification safely after foreign %s interruption", async fault => {
    const f = await migration();
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    f.rotate(2);
    let interrupted = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-sre"
        && f.namespace("kars-sre").metadata.annotations[`${P}state`] === "Qualified") {
        interrupted = true;
        throw new Error("foreign qualification acknowledgement lost");
      }
      return result;
    };
    await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("foreign qualification");
    expect(interrupted).toBe(true);
    if (fault === "namespace-drift") f.namespace("kars-sre").metadata.annotations[`${P}root-uid`] = "unreviewed-root";
    if (fault === "consumer-drift") f.foreign.spec.template.spec.containers[0].image = TARGET;
    if (fault !== "qualification") {
      await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
      expect(f.grant("core").spec.writers).toEqual([]);
    } else {
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(f.foreign.spec.replicas).toBe(1);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    }
  });

  it.each([0, 1, 2])("upgrades Core and BFF while preserving enrolled SRE intent %s", async replicas => {
    const f = await migration(replicas), foreign = structuredClone(f.foreign);
    const oldEpochs = f.review.activation.namespaces.map(scope => scope.epoch);
    const oldPods = f.pods.get("kars-sre")!.map(pod => pod.metadata.uid);
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.foreign.spec).toEqual({ ...foreign.spec, replicas: 0 });
    expect(retired.retirement!.captured["sre-namespace"]).toEqual(oldPods);
    expect(f.namespace("kars-sre").metadata.annotations[`${P}state`]).toBe("Pending");
    expect(f.pods.get("kars-sre")).toEqual([]);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    const complete = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    const activation = f.grant("core").spec.privateActivation;
    expect(activation.namespaces.map((scope: any) => scope.namespace.uid).sort()).toEqual(["core-uid", "sre-namespace"]);
    expect(activation.namespaces.every((scope: any) => !oldEpochs.includes(scope.epoch))).toBe(true);
    const scope = activation.namespaces.find((scope: any) => scope.namespace.name === "kars-sre");
    expect(scope.consumers[0].object.uid).toBe(foreign.metadata.uid);
    expect(scope.consumers[0].templateDigest).toBe(templateDigest(foreign));
    expect(f.foreign.metadata.uid).toBe(foreign.metadata.uid);
    expect(f.foreign.spec).toEqual({ ...foreign.spec, template: { ...foreign.spec.template,
      metadata: { ...foreign.spec.template.metadata, annotations: { ...foreign.spec.template.metadata.annotations, [`${P}epoch`]: scope.epoch } } } });
    expect(f.pods.get("kars-sre")).toHaveLength(replicas);
    expect(f.pods.get("kars-sre")!.every(pod => !oldPods.includes(pod.metadata.uid))).toBe(true);
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    await validateQualifiedActivation(f.execute, activation);
    await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant);
  });

  it.each(["omitted", "duplicate", "uid", "revision"])("rejects %s foreign namespace receipt snapshots", async fault => {
    const f = await migration(0);
    const receipt = await retireRootMigration(f.execute, f.prepared, f.journal);
    if (fault === "omitted") delete receipt.namespaces;
    if (fault === "duplicate") receipt.namespaces!.push(structuredClone(receipt.namespaces![0]!));
    if (fault === "uid") (receipt.namespaces![0]!.metadata as any).uid = "unreviewed";
    if (fault === "revision") (receipt.namespaces![0]!.metadata as any).annotations[`${P}bundle-revision`] = "unreviewed";
    f.rotate(2);
    await expect(finalizeRootMigration(f.execute, receipt, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
    expect(f.root.spec.replicas).toBe(0);
  });

  it.each(["uid", "template", "replicas", "namespace"])("rejects foreign %s drift before retirement", async fault => {
    const f = await migration();
    if (fault === "uid") f.foreign.metadata.uid = "replaced";
    if (fault === "template") f.foreign.spec.template.spec.containers[0].image = TARGET;
    if (fault === "replicas") f.foreign.spec.replicas = 4;
    if (fault === "namespace") f.namespace("kars-sre").metadata.annotations[`${P}root-uid`] = "other";
    await expect(retireRootMigration(f.execute, f.prepared, f.journal)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it("refuses foreign companion image selection and unselected rendered image changes", async () => {
    const f = await migration(), target = structuredClone(f.foreign);
    target.spec.template.spec.containers[0].image = TARGET;
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered], undefined, ["sre"])).rejects.toThrow("same-root");
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered, target])).rejects.toThrow("silently replace");
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it.each(["pause", "pending", "restoration"])("resumes a lost foreign %s acknowledgement", async boundary => {
    const f = await migration();
    let interrupted = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && !args.includes("--dry-run=server")
        && ((boundary === "pending" && args[1] === "namespace" && args[2] === "kars-sre")
          || (args[2] === "sre" && args.includes("--type=json")
            && f.foreign.spec.replicas === (boundary === "pause" ? 0 : boundary === "restoration" ? 1 : -1)))) {
        interrupted = true;
        throw new Error("lost foreign acknowledgement");
      }
      return result;
    };
    if (boundary !== "restoration") await expect(retireRootMigration(execute, f.prepared, f.journal)).rejects.toThrow("lost foreign");
    const retired = await retireRootMigration(f.execute, boundary === "restoration" ? f.prepared : f.last(), f.journal);
    f.rotate(2);
    if (boundary === "restoration") await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("lost foreign");
    const complete = await finalizeRootMigration(f.execute, boundary === "restoration" ? f.last() : retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    expect(f.foreign.spec.replicas).toBe(1);
    expect(f.grant("core").spec.privateActivation.namespaces).toHaveLength(2);
  });
});

describe("explicit companion image refresh", () => {
  it("requires explicit repeatable companion image review selection in the CLI", () => {
    const review = credentialGrantsCommand().commands.find(value => value.name() === "review-root-migration")!;
    expect(review.opts().companionImage).toEqual([]);
    review.parseOptions(["--companion-image", "kars-mission-dispatcher", "--companion-image", "kars-bridge-bff"]);
    expect(review.opts().companionImage).toEqual(["kars-mission-dispatcher", "kars-bridge-bff"]);
  });

  async function refresh(rootChanged = false) {
    const f = await setup(true, undefined, 1);
    if (!rootChanged) f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
    const target = structuredClone(f.companion);
    target.spec.template.spec.containers[0].image = TARGET;
    const documents = [f.rendered, target];
    const names = [f.companion.metadata.name];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, names);
    const prepared = await prepareRootMigration(f.execute, review, documents, f.journal);
    return { ...f, target, documents, names, imageReview: review, prepared };
  }

  it.each([false, true])("retires and freshly qualifies reviewed companion images with root change=%s", async rootChanged => {
    const f = await refresh(rootChanged), old = structuredClone(f.companion), oldEpoch = f.namespace("core").metadata.annotations[`${P}epoch`];
    const oldPods = new Set(f.pods.get("core")!.map(pod => pod.metadata.uid));
    expect(f.imageReview.companionImages).toEqual([{
      identity: { name: old.metadata.name, uid: old.metadata.uid, resourceVersion: old.metadata.resourceVersion },
      original: { templateDigest: templateDigest(old), specDigest: digest(old.spec) },
      target: { templateDigest: templateDigest(f.target), specDigest: digest(f.target.spec) },
      images: [{ group: "containers", name: "bff", before: ORIGINAL, after: TARGET }],
    }]);
    expect(f.prepared.plan.companions![0]!.spec).toEqual(old.spec);
    expect(f.prepared.plan.companionTargets).toEqual([{ uid: old.metadata.uid, spec: f.target.spec }]);
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(retired.companionUpdates).toEqual([{ uid: old.metadata.uid, phase: "updated" }]);
    expect(retired.activation!.namespaces[0]!.consumers.find(c => c.object.uid === old.metadata.uid)!.templateDigest).toBe(templateDigest(f.target));
    expect(f.companion.spec).toEqual({ ...f.target.spec, replicas: 0 });
    expect(f.root.spec.replicas).toBe(0);
    expect(f.pods.get("core")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
    expect(f.companion.metadata.uid).toBe(old.metadata.uid);
    expect(templateDigest(f.companion)).toBe(templateDigest(f.target));
    expect(f.companion.spec.replicas).toBe(old.spec.replicas);
    expect(f.root.spec.template.spec.containers[0].image).toBe(rootChanged ? TARGET : ORIGINAL);
    expect(f.namespace("core").metadata.annotations[`${P}epoch`]).not.toBe(oldEpoch);
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    expect(f.grant("core").spec.agentKeys).toEqual(f.grantBefore.spec.agentKeys);
    expect(f.grant("core").spec.privateActivation.namespaces[0].consumers.find((c: any) => c.object.uid === old.metadata.uid).templateDigest).toBe(templateDigest(f.target));
    expect(f.pods.get("core")!.every(pod => !oldPods.has(pod.metadata.uid))).toBe(true);
    await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
  });

  it.each(["not-selected", "unknown", "root", "duplicate", "missing-render", "unchanged", "uid", "namespace", "hook", "command", "env", "account", "volumes", "replicas", "mutable"])(
    "refuses %s companion review before any mutation", async fault => {
      const f = await setup(true, undefined, 1), target = structuredClone(f.companion);
      target.spec.template.spec.containers[0].image = TARGET;
      let names = [target.metadata.name], documents = [f.rendered, target];
      if (fault === "not-selected") names = [];
      if (fault === "unknown") names = ["unreviewed-dispatcher"];
      if (fault === "root") names = ["kars-controller"];
      if (fault === "duplicate") names.push(names[0]!);
      if (fault === "missing-render") documents = [f.rendered];
      if (fault === "unchanged") target.spec.template.spec.containers[0].image = ORIGINAL;
      if (fault === "uid") target.metadata.uid = "replacement";
      if (fault === "namespace") target.metadata.namespace = "other";
      if (fault === "hook") target.metadata.annotations = { "helm.sh/hook": "pre-upgrade" };
      if (fault === "command") target.spec.template.spec.containers[0].command = ["changed"];
      if (fault === "env") target.spec.template.spec.containers[0].env = [{ name: "MODE", value: "changed" }];
      if (fault === "account") target.spec.template.spec.serviceAccountName = "other";
      if (fault === "volumes") target.spec.template.spec.volumes = [];
      if (fault === "replicas") target.spec.replicas = 2;
      if (fault === "mutable") target.spec.template.spec.containers[0].image = "dispatcher:latest";
      await expect(previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, names)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("rejects an unselected companion replica-only render delta", async () => {
    const f = await setup(true, undefined, 1), target = structuredClone(f.companion);
    target.spec.replicas = 2;
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered, target])).rejects.toThrow("silently");
    expect(f.mutations).toEqual([]);
  });

  it("rejects no-op root migration without a reviewed companion delta", async () => {
    const f = await setup(true, undefined, 1);
    f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow("changed immutable");
    expect(f.mutations).toEqual([]);
  });

  it.each(["missing-target", "target-command", "target-image", "review-images", "original-spec", "unreviewed-target", "early-progress"])(
    "rejects digest-consistent %s receipt tampering before retirement", async fault => {
      const f = await refresh(), receipt = structuredClone(f.prepared);
      const target = receipt.plan.companionTargets![0]!;
      if (fault === "missing-target") delete receipt.plan.companionTargets;
      if (fault === "target-command") (target.spec as any).template.spec.containers[0].command = ["changed"];
      if (fault === "target-image") (target.spec as any).template.spec.containers[0].image = ORIGINAL;
      if (fault === "review-images") receipt.plan.review.companionImages![0]!.images[0]!.after = ORIGINAL;
      if (fault === "original-spec") (receipt.plan.companions![0]!.spec as any).replicas = 2;
      if (fault === "unreviewed-target") target.uid = "other";
      if (fault === "early-progress") receipt.companionUpdates = [{ uid: target.uid, phase: "updated" }];
      receipt.planDigest = digest(receipt.plan);
      await expect(retireRootMigration(f.execute, receipt, f.journal)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("does not accept a target image installed before its journaled cutover", async () => {
    const f = await refresh();
    f.companion.spec.template.spec.containers[0].image = TARGET;
    await expect(retireRootMigration(f.execute, f.prepared, f.journal)).rejects.toThrow("companion Deployment changed");
    expect(f.mutations).toEqual([]);
  });

  it("resumes after durable companion completion without repeating its image CAS", async () => {
    const f = await refresh();
    const journal = { save: async (receipt: RootMigrationReceipt) => {
      await f.journal.save(receipt);
      if (receipt.phase === "cutover" && receipt.companionUpdates?.[0]?.phase === "updated") throw new Error("Stopped after durable image completion");
    } };
    await expect(retireRootMigration(f.execute, f.prepared, journal)).rejects.toThrow("Stopped after durable image completion");
    const before = f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name).length;
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name)).toHaveLength(before);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
  });

  it("resumes a lost companion image acknowledgement from durable updating state", async () => {
    const f = await refresh();
    let lost = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!lost && args[0] === "patch" && args[2] === f.companion.metadata.name && !args.includes("--dry-run=server")
        && f.companion.spec.template.spec.containers[0].image === TARGET) {
        lost = true;
        expect(f.last().companionUpdates).toEqual([{ uid: f.companion.metadata.uid, phase: "updating" }]);
        expect(f.pods.get("core")).toEqual([]);
        throw new Error("Lost image acknowledgement");
      }
      return result;
    };
    await expect(retireRootMigration(run, f.prepared, f.journal)).rejects.toThrow("Lost image acknowledgement");
    const imageWrites = () => f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name
      && call.input?.includes(TARGET)).length;
    expect(imageWrites()).toBe(1);
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(imageWrites()).toBe(1);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
  });

  it.each(["command", "uid", "replicas", "version", "root", "namespace", "tls", "after-write"])(
    "fences %s drift at companion image CAS and withholds writers", async fault => {
      const f = await refresh();
      let raced = false;
      const run: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!raced && args[0] === "patch" && args[2] === f.companion.metadata.name && input?.includes(TARGET)
          && args.includes("--dry-run=server") === (fault !== "after-write")) {
          raced = true;
          if (fault === "command") f.companion.spec.template.spec.containers[0].command = ["changed"];
          if (fault === "uid") f.companion.metadata.uid = "replacement";
          if (fault === "replicas") f.companion.spec.replicas = 1;
          if (fault === "version") f.companion.metadata.resourceVersion = "999";
          if (fault === "root") f.root.spec.template.spec.containers[0].command = ["changed"];
          if (fault === "namespace") f.namespace("core").metadata.annotations[`${P}state`] = "Qualified";
          if (fault === "tls") f.rotate(2);
          if (fault === "after-write") f.companion.spec.template.spec.containers[0].image = ORIGINAL;
        }
        return result;
      };
      await expect(retireRootMigration(run, f.prepared, f.journal)).rejects.toThrow();
      expect(raced).toBe(true);
      expect(f.grant("core").spec.writers).toEqual([]);
      expect(f.last().phase).toBe("cutover");
      expect(f.last().companionUpdates![0]!.phase).toBe("updating");
    },
  );
});

describe("same-scope protected Deployment companions", () => {
  it.each([0, 1, 3])("retires and restores %i companion replicas with fresh epoch and retained grant intent", async replicas => {
    const f = await setup(true, undefined, replicas);
    expect(f.review.activation.namespaces[0]!.consumers[0]!.object.name).toBe("kars-bridge-bff");
    const originalSpec = structuredClone(f.companion.spec);
    const prepared = await f.prepare();
    const oldPods = new Set(prepared.plan.pods.map(pod => (pod.identity as { uid: string }).uid));
    expect(prepared.plan.companions).toHaveLength(1);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(f.root.spec.replicas).toBe(0);
    expect(f.companion.spec.replicas).toBe(0);
    expect(f.pods.get("core")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
    expect(f.root.spec.replicas).toBe(2);
    expect(f.companion.spec.replicas).toBe(replicas);
    expect(templateDigest(f.companion)).toBe(templateDigest({ spec: originalSpec }));
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    expect(f.grant("core").spec.agentKeys).toEqual(f.grantBefore.spec.agentKeys);
    expect(f.pods.get("core")).toHaveLength(2 + replicas);
    for (const pod of f.pods.get("core")!) {
      expect(oldPods.has(pod.metadata.uid)).toBe(false);
      expect(pod.metadata.annotations[`${P}epoch`]).toBe(f.namespace("core").metadata.annotations[`${P}epoch`]);
    }
    await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
  });

  it.each(["uid", "image", "command", "selector", "replicas", "epoch", "deleted"])("refuses companion %s drift before mutation", async fault => {
    const f = await setup(true, undefined, 2), prepared = await f.prepare();
    if (fault === "uid") f.companion.metadata.uid = "replacement";
    if (fault === "image") f.companion.spec.template.spec.containers[0].image = TARGET;
    if (fault === "command") f.companion.spec.template.spec.containers[0].command = ["changed"];
    if (fault === "selector") f.companion.spec.selector.matchLabels.app = "changed";
    if (fault === "replicas") f.companion.spec.replicas = 3;
    if (fault === "epoch") f.companion.spec.template.metadata.annotations = { [`${P}epoch`]: "changed" };
    if (fault === "deleted") f.companion.metadata.deletionTimestamp = "2026-10-26T00:00:00Z";
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["omitted", "duplicate", "namespace", "template", "kind", "replicas"])("refuses a digest-consistent %s companion capture", async fault => {
    const f = await setup(false, undefined, 1), prepared = await f.prepare();
    const captured = prepared.plan.companions![0]! as any;
    if (fault === "omitted") delete prepared.plan.companions;
    if (fault === "duplicate") prepared.plan.companions!.push(captured);
    if (fault === "namespace") captured.metadata.namespace = "other";
    if (fault === "template") captured.spec.template.spec.containers[0].image = TARGET;
    if (fault === "kind") captured.kind = "StatefulSet";
    if (fault === "replicas") captured.spec.replicas = -1;
    prepared.planDigest = digest(prepared.plan);
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["pause", "restore"])("resumes lost companion %s acknowledgment without repeating rotation", async point => {
    const f = await setup(true, undefined, 2), prepared = await f.prepare();
    let interrupted = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[2] === "kars-bridge-bff"
        && args.includes("--type=json") && !args.includes("--dry-run=server")
        && f.companion.spec.replicas === (point === "pause" ? 0 : 2)) {
        interrupted = true;
        throw new Error("Lost companion acknowledgment");
      }
      return result;
    };
    if (point === "pause") await expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("Lost companion");
    const retired = await retireRootMigration(f.execute, point === "pause" ? f.last() : prepared, f.journal);
    f.rotate(2);
    const keyVersion = f.secret.metadata.resourceVersion;
    if (point === "restore") {
      await expect(finalizeRootMigration(run, retired, f.journal, applyReviewedGrant)).rejects.toThrow("Lost companion");
      expect(f.grant("core").spec.writers).toEqual([]);
    }
    await finalizeRootMigration(f.execute, point === "restore" ? f.last() : retired, f.journal, applyReviewedGrant);
    expect(interrupted).toBe(true);
    expect(f.last().phase).toBe("complete");
    expect(f.companion.spec.replicas).toBe(2);
    expect(f.secret.metadata.resourceVersion).toBe(keyVersion);
  });

  it("does not declare retirement while companion Pods remain", async () => {
    const f = await setup(false, undefined, 1), prepared = await f.prepare();
    const old = f.pods.get("core")!.filter(pod => pod.metadata.uid.startsWith("bridge-pod"));
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "get" && args[1] === "pods") {
        const inventory = JSON.parse(result);
        inventory.items = [...inventory.items.filter((pod: any) => !pod.metadata.uid.startsWith("bridge-pod")), ...old];
        return JSON.stringify(inventory);
      }
      return result;
    };
    vi.useFakeTimers();
    try {
      const result = expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("timed out");
      await vi.runAllTimersAsync();
      await result;
      expect(f.last().phase).toBe("retiring");
      expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
      expect(f.grant("core").spec.writers).toEqual([]);
    } finally { vi.useRealTimers(); }
  });

  it.each(["old-pod", "wrong-epoch", "unknown-owner", "unready"])("withholds writers for %s companion execution", async fault => {
    const f = await setup(true, undefined, 1), prepared = await f.prepare();
    const old = f.pods.get("core")!.find(pod => pod.metadata.uid.startsWith("bridge-pod"));
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[2] === "kars-bridge-bff" && !args.includes("--dry-run=server") && f.companion.spec.replicas === 1) {
        const pod = f.pods.get("core")!.find(pod => pod.metadata.uid.startsWith("bridge-pod"));
        if (fault === "old-pod") pod.metadata.uid = old.metadata.uid;
        if (fault === "wrong-epoch") pod.metadata.annotations[`${P}epoch`] = "stale";
        if (fault === "unknown-owner") pod.metadata.ownerReferences[0].uid = "unreviewed";
        if (fault === "unready") pod.status.conditions[0].status = "False";
      }
      return result;
    };
    await expect(finalizeRootMigration(run, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
  });
});

async function alreadyRolled(selection = "combined") {
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

describe("retired legacy budget TLS ownership", () => {
  const data = (() => {
    const directory = mkdtempSync(join(tmpdir(), "kars-root-rotation-"));
    try {
      const certificate = join(directory, "tls.crt"), key = join(directory, "tls.key");
      execFileSync("openssl", ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
        "-nodes", "-keyout", key, "-out", certificate, "-subj", "/CN=rotation.test", "-days", "2",
        "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign"], { stdio: ["ignore", "pipe", "pipe"] });
      return {
        "tls.crt": readFileSync(certificate).toString("base64"),
        "tls.key": readFileSync(key).toString("base64"),
      };
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  })();
  async function fixture() {
    const f = await setup(true);
    Object.assign(f.root.metadata, { labels: { ...f.root.metadata.labels, "app.kubernetes.io/managed-by": "Helm" } });
    Object.assign(f.ca.metadata, { namespace: "core", labels: { "app.kubernetes.io/managed-by": "Helm" },
      annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" } });
    Object.assign(f.secret.data, { "tls.key": "old-private-fixture", "retained": "unchanged" });
    Object.assign(f.secret.metadata.annotations, { "customer.example/retain": "yes" });
    Object.assign(f.secret.metadata, { labels: { "customer.example/retain": "yes" }, finalizers: ["customer.example/retain"],
      managedFields: [{ manager: "legacy-issuer", operation: "Update", apiVersion: "v1", fieldsType: "FieldsV1", fieldsV1: { "f:data": {} } }] });
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    return { ...f, retired, expected: structuredClone(f.secret), next: { ...f.secret.data, ...data } };
  }
  it("prepares exact atomic ownership/rotation CAS without writes and finalizes the new revision", async () => {
    const f = await fixture(), count = f.mutations.length;
    const reads: string[][] = [];
    const patch = await prepareRootBudgetTlsRotation(async (args, input) => {
      reads.push(args);
      return f.execute(args, input);
    }, f.retired, f.expected, f.next);
    expect(reads.some(args => args.includes("--show-managed-fields=true"))).toBe(true);
    expect(patch[2]!.value).toEqual(f.secret.metadata);
    expect(f.mutations).toHaveLength(count);
    expect(patch.filter(p => p.op === "test").map(p => p.path)).toEqual(["/metadata/uid", "/metadata/resourceVersion", "/metadata", "/type", "/data"]);
    expect(patch.find(p => p.path === "/metadata/annotations" && p.op === "add")?.value).toEqual({
      ...f.secret.metadata.annotations, "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
    expect(patch.find(p => p.path === "/metadata/labels" && p.op === "add")?.value).toEqual({ "customer.example/retain": "yes", "app.kubernetes.io/managed-by": "Helm" });
    Object.assign(f.secret.metadata, { annotations: patch[5]!.value, labels: patch[6]!.value });
    f.secret.data = f.next;
    f.secret.metadata.resourceVersion = String(Number(f.secret.metadata.resourceVersion) + 1);
    f.ca.data["ca.crt"] = Buffer.from(data["tls.crt"], "base64").toString();
    f.ca.metadata.resourceVersion = String(Number(f.ca.metadata.resourceVersion) + 1);
    await expect(prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next)).rejects.toThrow();
    await finalizeRootMigration(f.execute, f.retired, f.journal, applyReviewedGrant);
    expect(f.grant("core").spec.privateActivation.root.budgetTls.secret.resourceVersion).toBe(f.secret.metadata.resourceVersion);
    expect((f.secret.metadata as any).finalizers).toEqual(["customer.example/retain"]);
  });
  it("preserves compatible existing release ownership", async () => {
    const f = await fixture();
    Object.assign(f.secret.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
    Object.assign((f.secret.metadata as any).labels, { "app.kubernetes.io/managed-by": "Helm" });
    f.expected = structuredClone(f.secret);
    const count = f.mutations.length;
    const patch = await prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next);
    expect(patch[5]!.value).toEqual(f.secret.metadata.annotations);
    expect(patch[6]!.value).toEqual((f.secret.metadata as any).labels);
    expect(f.mutations).toHaveLength(count);
  });
  it.each(["uid", "version", "data", "metadata", "owner", "manager", "owner-reference", "missing-managed-fields", "deleting", "immutable", "invalid-key", "qualified", "replicas", "pods", "catalog", "stale-key", "unrelated-data", "plan"])(
    "refuses %s without writes", async fault => {
      const f = await fixture();
      if (fault === "uid") f.secret.metadata.uid = "different";
      if (fault === "version") f.secret.metadata.resourceVersion = "999";
      if (fault === "data") Object.assign(f.secret.data, { retained: "changed" });
      if (fault === "metadata") Object.assign(f.secret.metadata.annotations, { extra: "changed" });
      if (fault === "owner") Object.assign(f.secret.metadata.annotations, { "meta.helm.sh/release-name": "other" });
      if (fault === "manager") Object.assign((f.secret.metadata as any).labels, { "app.kubernetes.io/managed-by": "other" });
      if (fault === "owner-reference") Object.assign(f.secret.metadata, { ownerReferences: [{ uid: "another-owner" }] });
      if (fault === "missing-managed-fields") delete (f.expected.metadata as any).managedFields;
      if (fault === "deleting") Object.assign(f.secret.metadata, { deletionTimestamp: new Date().toISOString() });
      if (fault === "immutable") Object.assign(f.secret, { immutable: true });
      if (fault === "invalid-key") f.next["tls.key"] = Buffer.from("invalid private key").toString("base64");
      if (["owner", "manager", "owner-reference", "deleting", "immutable"].includes(fault)) f.expected = structuredClone(f.secret);
      if (fault === "qualified") f.namespace("core").metadata.annotations[`${P}state`] = "Qualified";
      if (fault === "replicas") f.root.spec.replicas = 1;
      if (fault === "pods") f.pods.set("core", [rootPod(f, "unexpected")]);
      if (fault === "catalog") Object.assign((f.ca.metadata as any).annotations, { "meta.helm.sh/release-name": "other" });
      if (fault === "stale-key") f.next = { ...f.secret.data } as typeof f.next;
      if (fault === "unrelated-data") Object.assign(f.next, { retained: "changed" });
      if (fault === "plan") f.retired.planDigest = "0".repeat(64);
      const count = f.mutations.length;
      await expect(prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next)).rejects.toThrow();
      expect(f.mutations).toHaveLength(count);
    });
});

describe("single-root image migration", () => {
  it.each(["controller", "runtime", "runner", "combined"].flatMap(selection => ["Qualified", "Pending"].map(state => [selection, state])))
  ("recovers already-rolled %s images from %s without rollback or reused qualification", async (selection, state) => {
    const f = await alreadyRolled(selection);
    f.namespace("core").metadata.annotations[`${P}state`] = state;
    if (state === "Pending") {
      const metadata = f.namespace("core").metadata;
      const guard = `kars.azure.com/credential-reader-${f.grant("core").metadata.uid}`;
      delete metadata.labels[guard];
      delete metadata.annotations[guard];
      metadata.finalizers = metadata.finalizers.filter((value: string) => value !== guard);
    }
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered])).rejects.toThrow();
    const review = await f.preview();
    expect(f.mutations).toEqual([]);
    expect(review.activation.root.templateDigest).toBe(templateDigest(f.historicalRoot));
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(f.root.spec).toEqual({ ...f.rendered.spec, replicas: 0 });
    expect(f.pods.get("core")).toEqual([]);
    expect(retired.retirement?.captured["core-uid"]).toContain("rolled-root");
    expect(f.grant("core").spec.writers).toEqual([]);
    const writes = f.mutations.filter(call => call.args[1] === "deployment");
    expect(writes).toHaveLength(1);
    expect(JSON.parse(writes[0]!.input!).at(-1).value.template).toEqual(f.rendered.spec.template);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(templateDigest(f.root)).toBe(templateDigest(f.rendered));
    expect(f.root.spec.replicas).toBe(2);
    expect(f.grant("core").spec.privateActivation.namespaces[0].epoch).not.toBe(f.grantBefore.spec.privateActivation.namespaces[0].epoch);
    expect(f.grant("core").spec).toEqual({ ...f.grantBefore.spec, privateActivation: f.grant("core").spec.privateActivation });
    expect(f.ca.data["contracts.json"]).toBe('{"unchanged":"accounts-charges-caps"}');
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
  });

  it("recovers an already-rolled root when the grant recorded a newer budget namespace revision", async () => {
    const f = await alreadyRolled();
    f.grant("core").spec.privateActivation.root.budgetTls.namespace.resourceVersion = "999";
    const grantBefore = structuredClone(f.grant("core").spec);
    const review = await f.preview();
    expect(f.mutations).toEqual([]);
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.grant("core").spec).toEqual({ ...grantBefore,
      privateActivation: f.grant("core").spec.privateActivation });
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
  });

  it.each(["namespace-name", "namespace-uid", "secret-name", "secret-uid", "secret-version", "key", "missing"])(
    "rejects changed grant budget TLS %s despite namespace revision churn", async fault => {
      const f = await alreadyRolled();
      const activation = f.grant("core").spec.privateActivation;
      const budget = activation.root.budgetTls;
      budget.namespace.resourceVersion = "999";
      if (fault === "namespace-name") budget.namespace.name = "other";
      if (fault === "namespace-uid") budget.namespace.uid = "other";
      if (fault === "secret-name") budget.secret.name = "other";
      if (fault === "secret-uid") budget.secret.uid = "other";
      if (fault === "secret-version") budget.secret.resourceVersion = "999";
      if (fault === "key") budget.keyDigest = "f".repeat(64);
      if (fault === "missing") delete activation.root.budgetTls;
      await expect(f.preview()).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("prepares Pending recovery across version-only namespace reconciliation without weakening write CAS", async () => {
    const f = await alreadyRolled();
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    const review = await f.preview();
    const run: Execute = async (args, input) => {
      if (args[0] === "get") {
        const metadata = f.namespace("core").metadata;
        metadata.resourceVersion = String(Number(metadata.resourceVersion) + 1);
      }
      return f.execute(args, input);
    };
    const prepared = await prepareRootMigration(run, review, [f.rendered], f.journal);
    expect(prepared.phase).toBe("prepared");
    expect(f.mutations).toEqual([]);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.root.spec).toEqual({ ...f.rendered.spec, replicas: 0 });
  });

  it.each(["historical-template", "historical-uid", "live-uid", "mutable-image", "unrelated-env", "next-image", "key", "grant-epoch"])(
    "refuses already-rolled recovery with %s before mutation", async fault => {
      const f = await alreadyRolled();
      if (fault === "historical-template") f.historicalRoot.spec.template.spec.containers[0].image = TARGET;
      if (fault === "historical-uid") f.historicalRoot.metadata.uid = "replaced";
      if (fault === "live-uid") f.root.metadata.uid = "replaced";
      if (fault === "mutable-image") f.root.spec.template.spec.containers[0].image = "example.test/controller:latest";
      if (fault === "unrelated-env") f.root.spec.template.spec.containers[0].env.push({ name: "EXTRA", value: "changed" });
      if (fault === "next-image") f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
      if (fault === "key") f.rotate(2);
      if (fault === "grant-epoch") f.grant("core").spec.privateActivation.namespaces[0].epoch = "a".repeat(64);
      await expect(f.preview()).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it.each(["state", "epoch", "bundle-revision"])("refuses changed %s metadata during Pending authority recovery", async field => {
    const f = await alreadyRolled();
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    f.namespace("core").metadata.annotations[`${P}${field}`] = "unreviewed";
    await expect(f.preview()).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it("refuses stale recovery reviews and unreviewed actual Pod descendants", async () => {
    const f = await alreadyRolled(), review = await f.preview();
    f.root.spec.template.spec.containers[0].env.at(-1).value = ORIGINAL;
    await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow();
    f.root.spec = structuredClone(f.rendered.spec);
    f.pods.get("core")![0]!.spec.containers[0].image = ORIGINAL;
    await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["image", "namespace", "grant", "key"])("fences %s changes after recovery preparation before withdrawal", async fault => {
    const f = await alreadyRolled(), review = await f.preview();
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    if (fault === "image") f.root.spec.template.spec.containers[0].image = ORIGINAL;
    if (fault === "namespace") f.namespace("core").metadata.annotations[`${P}epoch`] = "b".repeat(64);
    if (fault === "grant") f.grant("core").spec.agentKeys.push("EXTRA_TOKEN");
    if (fault === "key") f.rotate(2);
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it("resumes already-rolled recovery after Pending without another template write", async () => {
    const f = await alreadyRolled(), review = await f.preview();
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    let interrupted = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[1] === "namespace" && args.includes("--type=json")) {
        interrupted = true;
        throw new Error("Pending acknowledgement lost");
      }
      return result;
    };
    await expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("acknowledgement lost");
    expect(f.last().phase).toBe("cutover");
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.mutations.filter(call => call.args[1] === "deployment")).toHaveLength(1);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
  });

  it("retains router-only migration without a governed broker or digest pin", async () => {
    const f = await setup(false, root => {
      root.spec.template.spec.containers[0].env = [{ name: "INFERENCE_ROUTER_IMAGE", value: ORIGINAL }];
    });
    const container = f.rendered.spec.template.spec.containers[0];
    container.image = ORIGINAL;
    container.env[0].value = TARGET;
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.root.spec.template.spec.containers[0].env).toEqual([{ name: "INFERENCE_ROUTER_IMAGE", value: TARGET }]);
    expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
  });

  it.each(["HERMES_RUNTIME_IMAGE", "SANDBOX_IMAGE", "INFERENCE_ROUTER_IMAGE", "KARS_CONFORMANCE_RUNNER_IMAGE", "combined"])(
    "qualifies %s changes through genuine retirement and rotation, including image-only cutover", async selection => {
      const names = selection === "combined"
        ? ["HERMES_RUNTIME_IMAGE", "SANDBOX_IMAGE", "INFERENCE_ROUTER_IMAGE", "KARS_CONFORMANCE_RUNNER_IMAGE"] : [selection];
      const f = await setup(true, root => {
        root.spec.template.spec.containers[0].env.push(...names.map(name => ({ name, value: ORIGINAL })));
        if (names.includes("INFERENCE_ROUTER_IMAGE")) {
          root.spec.template.spec.containers[0].env.push({
            name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"1".repeat(64)}`,
          });
        }
      });
      f.rendered.spec.template.spec.containers[0].image = selection === "combined" ? TARGET : ORIGINAL;
      for (const entry of f.rendered.spec.template.spec.containers[0].env) {
        if (names.includes(entry.name)) entry.value = TARGET;
        if (entry.name === "KARS_INFERENCE_BUDGET_ROUTER_DIGEST") entry.value = `sha256:${"2".repeat(64)}`;
      }
      if (names.includes("INFERENCE_ROUTER_IMAGE")) {
        f.rendered.spec.template.spec.containers[0].env.push({
          name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"1".repeat(64)}`,
        });
      }
      const originalEpoch = f.grantBefore.spec.privateActivation.namespaces[0].epoch;
      const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
      const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      const retired = await retireRootMigration(f.execute, prepared, f.journal);
      expect(f.root.spec.replicas).toBe(0);
      expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
      expect(retired.retirement?.captured["core-uid"]).toContain("new-root");
      expect(f.grant("core").spec.writers).toEqual([]);
      const deploymentWrites = f.mutations.filter(call => call.args[1] === "deployment");
      expect(deploymentWrites).toHaveLength(2);
      for (const write of deploymentWrites) {
        expect(write.args).toContain("--type=json");
        expect(JSON.parse(write.input!).slice(0, 3).map((op: any) => [op.op, op.path])).toEqual([
          ["test", "/metadata/uid"], ["test", "/metadata/resourceVersion"], ["test", "/spec"],
        ]);
      }
      await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
      f.rotate(2);
      await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
      expect(f.root.spec.replicas).toBe(2);
      expect(f.root.spec.template.spec.containers[0].image).toBe(selection === "combined" ? TARGET : ORIGINAL);
      for (const name of names) {
        expect(f.root.spec.template.spec.containers[0].env.find((entry: any) => entry.name === name).value).toBe(TARGET);
      }
      if (names.includes("INFERENCE_ROUTER_IMAGE")) {
        expect(f.root.spec.template.spec.containers[0].env.find(
          (entry: any) => entry.name === "KARS_INFERENCE_BUDGET_ROUTER_DIGEST",
        ).value).toBe(`sha256:${"2".repeat(64)}`);
      }
      expect(f.grant("core").spec.privateActivation.namespaces[0].epoch).not.toBe(originalEpoch);
      expect(f.grant("core").spec).toEqual({ ...f.grantBefore.spec, privateActivation: f.grant("core").spec.privateActivation });
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
    },
  );

  it.each(["old-pin", "target-pin", "unchanged-pin", "pin-only", "missing-pin",
    "new-pin", "removed-pin", "old-valueFrom", "target-valueFrom", "duplicate-pin", "disabled-broker"])(
    "refuses a governed router image migration with %s before writes", async fault => {
      const f = await setup(true, root => {
        const env = root.spec.template.spec.containers[0].env;
        env.push({ name: "INFERENCE_ROUTER_IMAGE", value: ORIGINAL });
        if (fault !== "missing-pin" && fault !== "new-pin") {
          env.push({
            name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST",
            ...(fault === "old-valueFrom"
              ? { valueFrom: { configMapKeyRef: { name: "images", key: "router" } } }
              : { value: `sha256:${(fault === "old-pin" ? "3" : "1").repeat(64)}` }),
          });
        }
        if (fault === "duplicate-pin") env.push({
          name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"1".repeat(64)}`,
        });
      });
      const container = f.rendered.spec.template.spec.containers[0];
      container.image = ORIGINAL;
      container.env.find((entry: any) => entry.name === "INFERENCE_ROUTER_IMAGE").value =
        fault === "pin-only" ? ORIGINAL : TARGET;
      const pin = container.env.find((entry: any) => entry.name === "KARS_INFERENCE_BUDGET_ROUTER_DIGEST");
      if (pin) {
        delete pin.valueFrom;
        pin.value = `sha256:${(fault === "target-pin" ? "3" : fault === "unchanged-pin" ? "1" : "2").repeat(64)}`;
        if (fault === "target-valueFrom") {
          delete pin.value;
          pin.valueFrom = { configMapKeyRef: { name: "images", key: "router" } };
        }
      }
      if (fault === "new-pin") container.env.push({
        name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"2".repeat(64)}`,
      });
      if (fault === "removed-pin") container.env = container.env.filter(
        (entry: any) => entry.name !== "KARS_INFERENCE_BUDGET_ROUTER_DIGEST",
      );
      if (fault === "disabled-broker") {
        container.env.find((entry: any) => entry.name === "KARS_INFERENCE_BUDGET_ENABLED").value = "false";
      }
      await expect((async () => {
        const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
        await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      })()).rejects.toThrow();
      expect(f.mutations).toEqual([]);
      expect(f.grant("core")).toEqual(f.grantBefore);
    },
  );

  describe("router adoption baseline", () => {
    const router = "INFERENCE_ROUTER_IMAGE";
    const pin = "KARS_INFERENCE_BUDGET_ROUTER_DIGEST";
    const baseline = "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST";
    const enabled = "KARS_INFERENCE_BUDGET_ENABLED";
    const originalDigest = `sha256:${"1".repeat(64)}`;
    const selectedDigest = `sha256:${"2".repeat(64)}`;

    it.each(["absent", "removed", "projection-removed"])("refuses a new review with an %s first-install baseline before retirement", async mode => {
      const f = await setup(true, root => {
        root.spec.template.spec.containers[0].env.push(
          { name: router, value: ORIGINAL }, { name: pin, value: originalDigest },
          ...(mode === "removed" ? [{ name: baseline, value: originalDigest }] : []),
        );
      });
      const container = f.rendered.spec.template.spec.containers[0];
      container.env.find((entry: any) => entry.name === router).value = TARGET;
      container.env.find((entry: any) => entry.name === pin).value = selectedDigest;
      container.env = container.env.filter((entry: any) => entry.name !== baseline);
      if (mode === "projection-removed") container.env.push({ name: baseline, value: originalDigest });
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (mode !== "projection-removed" || args[0] !== "apply") return result;
        const projected = JSON.parse(result);
        projected.spec.template.spec.containers[0].env = projected.spec.template.spec.containers[0].env
          .filter((entry: any) => entry.name !== baseline);
        return JSON.stringify(projected);
      };
      if (mode === "absent") {
        // Historical failed receipts remain readable; new preparation must reject this delta.
        expect(() => assertRootImageDelta(f.root.spec, f.rendered.spec)).not.toThrow();
      }
      const before = structuredClone(f.root.spec), namespace = structuredClone(f.namespace("core"));
      await expect(previewPrivateRootMigration(execute, "core", "kars", [f.rendered]))
        .rejects.toThrow(`must retain ${baseline}=${originalDigest} before retirement`);
      expect(f.mutations).toEqual([]);
      expect(f.root.spec).toEqual(before);
      expect(f.namespace("core")).toEqual(namespace);
      expect(f.grant("core")).toEqual(f.grantBefore);
    });

    it.each(["introduced", "retained"])("qualifies the %s original verifier only through retirement and rotation", async mode => {
      const f = await setup(true, root => {
        root.spec.template.spec.containers[0].env.push(
          { name: router, value: mode === "retained" ? TARGET : ORIGINAL },
          { name: pin, value: mode === "retained" ? selectedDigest : originalDigest },
          ...(mode === "retained" ? [{ name: baseline, value: originalDigest }] : []),
          { name: "UNCHANGED", value: "keep" },
        );
      });
      const container = f.rendered.spec.template.spec.containers[0];
      container.image = ORIGINAL;
      const nextDigest = `sha256:${"3".repeat(64)}`;
      container.env.find((entry: any) => entry.name === router).value = `example.test/router:latest@${nextDigest}`;
      container.env.find((entry: any) => entry.name === pin).value = nextDigest;
      if (mode === "introduced") container.env.splice(container.env.length - 1, 0, { name: baseline, value: originalDigest });
      const before = structuredClone(f.root.spec), target = structuredClone(f.rendered.spec);
      const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
      const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      expect(f.root.spec).toEqual(before);
      expect(f.rendered.spec).toEqual(target);
      expect(f.mutations).toEqual([]);
      const retired = await retireRootMigration(f.execute, prepared, f.journal);
      expect(f.root.spec).toEqual({ ...target, replicas: 0 });
      expect(f.pods.get("core")).toEqual([]);
      expect(f.grant("core").spec.writers).toEqual([]);
      await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
      f.rotate(2);
      await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
      expect(f.root.spec.template.spec.containers[0].env).toEqual(container.env);
      expect(f.grant("core").spec.privateActivation.namespaces[0].epoch)
        .not.toBe(f.grantBefore.spec.privateActivation.namespaces[0].epoch);
      expect(f.grant("core").spec).toEqual({ ...f.grantBefore.spec, privateActivation: f.grant("core").spec.privateActivation });
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
    });

    it.each(["missing", "indirect", "duplicate"])("rejects %s original governance at the delta guard", fault => {
      const env: any[] = [{ name: router, value: ORIGINAL }, { name: pin, value: originalDigest }];
      if (fault !== "missing") env.push({ name: enabled, value: "true" });
      if (fault === "indirect") env.at(-1).valueFrom = { configMapKeyRef: { name: "settings", key: "enabled" } };
      if (fault === "duplicate") env.push({ name: enabled, value: "true" });
      const original = { template: { spec: { containers: [{ name: "controller", image: ORIGINAL, env }] } } };
      const target = structuredClone(original);
      target.template.spec.containers[0]!.env[0].value = TARGET;
      target.template.spec.containers[0]!.env[1].value = selectedDigest;
      target.template.spec.containers[0]!.env.push({ name: baseline, value: originalDigest });
      expect(() => assertRootImageDelta(original, target)).toThrow();
    });

    it.each([
      "selected-baseline", "foreign-baseline", "malformed-baseline", "indirect-baseline", "extra-baseline-field", "duplicate-baseline",
      "old-pin-missing", "old-pin-malformed", "old-pin-mismatch", "old-pin-indirect", "old-pin-duplicate",
      "target-pin-missing", "target-pin-malformed", "target-pin-mismatch", "target-pin-indirect", "target-pin-duplicate",
      "old-router-missing", "old-router-malformed", "old-router-indirect", "old-router-duplicate",
      "target-router-missing", "target-router-malformed", "target-router-indirect", "target-router-duplicate",
      "ungoverned", "newly-governed", "disabled-governance",
      "replaced-baseline", "removed-baseline", "rebound-baseline", "added-env", "changed-env", "removed-env", "reordered-env",
      "changed-authority", "baseline-only",
    ])("rejects %s before any writes or authority changes", async fault => {
      const corrupt = (env: any[], name: string, issue: string) => {
        const entry = env.find(value => value.name === name);
        if (issue === "missing") env.splice(env.indexOf(entry), 1);
        if (issue === "malformed") entry.value = "not-an-immutable-pin";
        if (issue === "mismatch") entry.value = `sha256:${"4".repeat(64)}`;
        if (issue === "indirect") {
          delete entry.value;
          entry.valueFrom = { configMapKeyRef: { name: "images", key: "router" } };
        }
        if (issue === "duplicate") env.push(structuredClone(entry));
      };
      const f = await setup(!["ungoverned", "newly-governed"].includes(fault), root => {
        const env = root.spec.template.spec.containers[0].env ??= [];
        env.push({ name: router, value: ORIGINAL }, { name: pin, value: originalDigest },
          { name: "KARS_INFERENCE_BUDGET_CATALOG", value: "kars-inference-budget-contracts" },
          { name: "UNCHANGED", value: "keep" });
        if (fault.startsWith("old-")) corrupt(env, fault.includes("-pin-") ? pin : router, fault.split("-")[2]!);
        if (["replaced-baseline", "removed-baseline", "rebound-baseline"].includes(fault)) env.push({ name: baseline, value: originalDigest });
        if (fault === "ungoverned") env.push({ name: enabled, value: "false" });
      });
      const container = f.rendered.spec.template.spec.containers[0], env = container.env;
      container.image = ORIGINAL;
      for (const entry of env) {
        if (entry.name === router || entry.name === pin) {
          delete entry.valueFrom;
          entry.value = entry.name === router ? TARGET : selectedDigest;
        }
      }
      if (!env.some((entry: any) => entry.name === baseline)) env.splice(1, 0, { name: baseline, value: originalDigest });
      const declaration = env.find((entry: any) => entry.name === baseline);
      if (fault === "selected-baseline" || fault === "replaced-baseline") declaration.value = selectedDigest;
      if (fault === "foreign-baseline") declaration.value = `sha256:${"4".repeat(64)}`;
      if (fault === "extra-baseline-field") declaration.extra = "unreviewed";
      if (fault === "malformed-baseline") corrupt(env, baseline, "malformed");
      if (fault === "indirect-baseline" || fault === "rebound-baseline") corrupt(env, baseline, "indirect");
      if (fault === "duplicate-baseline") corrupt(env, baseline, "duplicate");
      if (fault === "removed-baseline") corrupt(env, baseline, "missing");
      if (fault.startsWith("target-")) corrupt(env, fault.includes("-pin-") ? pin : router, fault.split("-")[2]!);
      if (fault === "newly-governed") env.push({ name: enabled, value: "true" });
      if (fault === "disabled-governance") env.find((entry: any) => entry.name === enabled).value = "false";
      if (fault === "added-env") env.push({ name: "UNREVIEWED", value: "changed" });
      if (fault === "changed-env") env.find((entry: any) => entry.name === "UNCHANGED").value = "changed";
      if (fault === "removed-env") corrupt(env, "UNCHANGED", "missing");
      if (fault === "reordered-env") env.reverse();
      if (fault === "changed-authority") env.find((entry: any) => entry.name === "KARS_INFERENCE_BUDGET_CATALOG").value = "other-authority";
      if (fault === "baseline-only") {
        env.find((entry: any) => entry.name === router).value = ORIGINAL;
        env.find((entry: any) => entry.name === pin).value = originalDigest;
      }
      await expect((async () => {
        const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
        await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      })()).rejects.toThrow();
      expect(f.mutations).toEqual([]);
      expect(f.grant("core")).toEqual(f.grantBefore);
      expect(f.namespace("core").metadata.annotations[FIELD]).toBe(f.seal);
    });
  });

  it.each([
    { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: "ghcr.io/azure/kars-conformance-runner:latest" },
    { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: ORIGINAL },
    { name: "KARS_CONFORMANCE_RUNNER_IMAGE", valueFrom: { configMapKeyRef: { name: "images", key: "runner" } } },
  ])("preserves an unchanged runner entry during controller migration: %j", async entry => {
    const f = await setup(true, root => root.spec.template.spec.containers[0].env.push(structuredClone(entry)));
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.root.spec.template.spec.containers[0].image).toBe(TARGET);
    expect(f.root.spec.template.spec.containers[0].env.find((value: any) => value.name === entry.name)).toEqual(entry);
    expect(f.grant("core").metadata.uid).toBe(f.grantBefore.metadata.uid);
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
  });

  it.each(["HERMES_RUNTIME_IMAGE", "KARS_CONFORMANCE_RUNNER_IMAGE"].flatMap(name => [
    "new-entry", "deleted-entry", "valueFrom", "duplicate", "renamed", "reordered",
    "old-mutable", "target-mutable", "old-valueFrom", "old-duplicate", "malformed-digest", "unrelated-env",
    "command", "funding", "replicas", "checksum", "unchanged",
  ].map(fault => [name, fault])))(
    "refuses %s migration with %s without any writes", async (name, fault) => {
      const f = await setup(false, root => {
        root.spec.template.spec.containers[0].env = [
          { name, value: fault === "old-mutable" ? "runtime:latest" : ORIGINAL },
          { name: "UNCHANGED", value: "keep" },
          { name: "KARS_INFERENCE_BUDGET_MAX_USD", value: "100" },
        ];
        if (fault === "new-entry") root.spec.template.spec.containers[0].env.shift();
        if (fault === "old-valueFrom") root.spec.template.spec.containers[0].env[0] = {
          name, valueFrom: { configMapKeyRef: { name: "images", key: "image" } },
        };
        if (fault === "old-duplicate") root.spec.template.spec.containers[0].env.push({ name, value: ORIGINAL });
      });
      const container = f.rendered.spec.template.spec.containers[0];
      container.image = ORIGINAL;
      if (fault === "new-entry") container.env.unshift({ name, value: TARGET });
      else container.env[0] = { name, value: fault === "unchanged" ? ORIGINAL : TARGET };
      if (fault === "deleted-entry") container.env.shift();
      if (fault === "valueFrom") container.env[0] = {
        name, valueFrom: { configMapKeyRef: { name: "images", key: "image" } },
      };
      if (fault === "duplicate") container.env.push({ name, value: TARGET });
      if (fault === "renamed") container.env[0].name = "SANDBOX_IMAGE";
      if (fault === "reordered") container.env.reverse();
      if (fault === "target-mutable") container.env[0].value = "runtime:latest";
      if (fault === "malformed-digest") container.env[0].value += "bad";
      if (fault === "unrelated-env") container.env[1].value = "changed";
      if (fault === "command") container.command = ["other"];
      if (fault === "funding") container.env[2].value = "100000";
      if (fault === "replicas") f.rendered.spec.replicas = 3;
      if (fault === "checksum") f.rendered.spec.template.metadata.annotations["checksum/inference-budget-tls"] = "a".repeat(64);
      await expect((async () => {
        const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
        await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      })()).rejects.toThrow();
      expect(f.mutations).toEqual([]);
      expect(f.grant("core")).toEqual(f.grantBefore);
    },
  );

  it("repeats read-only forced projection after image recovery and keeps every live image write CAS-fenced", async () => {
    const f = await setup();
    f.root.metadata.managedFields = [{
      manager: "kubectl-patch", operation: "Update", apiVersion: "apps/v1", fieldsType: "FieldsV1",
      fieldsV1: { "f:spec": { "f:template": { "f:spec": { "f:containers": {
        'k:{"name":"controller"}': { "f:image": {} },
      } } } } },
    }];
    const ownership = canonical(f.root.metadata.managedFields);
    let projections = 0, liveDeploymentWrites = 0;
    const run: Execute = async (args, input) => {
      if (args[0] === "apply") {
        if (!args.includes("--force-conflicts")) {
          throw new Error('Apply conflict with "kubectl-patch": .spec.template.spec.containers[name="controller"].image');
        }
        expect(args).toContain("--server-side");
        expect(args).toContain("--dry-run=server");
        projections++;
      } else expect(args).not.toContain("--force-conflicts");
      if (args[0] === "patch" && args[1] === "deployment" && !args.includes("--dry-run=server")) {
        expect(args).toContain("--type=json");
        expect(args).toContain("--patch-file=/dev/stdin");
        expect(JSON.parse(input!).slice(0, 3)).toEqual([
          { op: "test", path: "/metadata/uid", value: f.root.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: f.root.metadata.resourceVersion },
          { op: "test", path: "/spec", value: f.root.spec },
        ]);
        liveDeploymentWrites++;
      }
      return f.execute(args, input);
    };
    const before = canonical([...f.objects.entries()]);
    const review = await previewPrivateRootMigration(run, "core", "kars", [f.rendered]);
    const prepared = await prepareRootMigration(run, review, [f.rendered], f.journal);
    expect(projections).toBe(4);
    expect(canonical([...f.objects.entries()])).toBe(before);
    await retireRootMigration(run, prepared, f.journal);
    expect(liveDeploymentWrites).toBe(2);
    expect(canonical(f.root.metadata.managedFields)).toBe(ownership);
    expect(f.root.spec.template.spec.containers[0].image).toBe(TARGET);
    expect(f.root.spec.replicas).toBe(0);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
  });

  it("persists real source evidence, retires at zero, then uses ordinary activation and exact retained grant intent", async () => {
    const f = await setup();
    const prepared = await f.prepare();
    expect(f.mutations).toEqual([]);
    expect(prepared.plan.seal).toBe(f.seal);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.root.spec.replicas).toBe(0);
    expect(f.root.spec.template.spec.containers[0].image).toBe(TARGET);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
    expect(JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version).toBe(1);
    expect(retired.retirement?.captured["core-uid"]).toContain("new-root");
    expect(retired.retirement?.captured["core-uid"]).toContain("old-root");
    expect(f.grant("core").spec).toEqual({ ...f.grantBefore.spec, writers: [] });
    const result = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(result.phase).toBe("complete");
    expect(f.root.spec.replicas).toBe(2);
    const activation = f.grant("core").spec.privateActivation;
    expect(f.grant("core").spec).toEqual({ ...f.grantBefore.spec, privateActivation: activation });
    expect(activation.namespaces[0].epoch).not.toBe(f.grantBefore.spec.privateActivation.namespaces[0].epoch);
    await validateQualifiedActivation(f.execute, activation);
    expect(JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version).toBe(2);
    const mutations = f.mutations.length;
    await finalizeRootMigration(f.execute, result, f.journal, applyReviewedGrant);
    expect(f.mutations).toHaveLength(mutations);
  });

  it("resumes the same journal after an unrelated terminal namespace finishes deletion during qualification", async () => {
    const f = await setup(true);
    const prepared = await f.prepare();
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    let interrupted = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[2] === "kars-controller" && f.root.spec.replicas > 0) {
        interrupted = true;
        f.namespace("third").metadata.deletionTimestamp = new Date().toISOString();
      }
      return result;
    };
    await expect(finalizeRootMigration(run, retired, f.journal, applyReviewedGrant)).rejects.toThrow("live API");
    const pending = f.last();
    expect(pending.phase).toBe("qualifying");
    expect(f.grant("core").spec.writers).toEqual([]);
    f.objects.delete(f.key("namespace", "third"));
    const result = await finalizeRootMigration(f.execute, pending, f.journal, applyReviewedGrant);
    expect(result.phase).toBe("complete");
    expect(result.planDigest).toBe(prepared.planDigest);
    expect(result.plan.inventory).toEqual(prepared.plan.inventory);
    expect(f.secret.data["tls.crt"]).toBe(Buffer.from(rootCertificates[2]!).toString("base64"));
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
    const mutations = f.mutations.length;
    await finalizeRootMigration(f.execute, result, f.journal, applyReviewedGrant);
    expect(f.mutations).toHaveLength(mutations);
  });

  it.each(["private-label", "private-annotation", "reader-guard", "finalizer", "grant-reference", "replacement", "addition", "changed"])(
    "rejects %s namespace inventory changes when resuming qualification", async fault => {
      const f = await setup();
      const namespace = f.namespace("third");
      if (fault === "private-label") namespace.metadata.labels = { [`${P}reserved`]: "true" };
      if (fault === "private-annotation") namespace.metadata.annotations[`${P}reserved`] = "true";
      if (fault === "reader-guard") namespace.metadata.labels = { "kars.azure.com/credential-reader-other": "third-uid" };
      if (fault === "finalizer") namespace.metadata.finalizers = ["example.test/protected"];
      if (fault === "grant-reference") f.grant("core").spec.agentKeys.push("third-uid");
      if (fault === "private-annotation") {
        await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered])).rejects.toThrow();
        expect(f.mutations).toEqual([]);
        return;
      }
      const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
      const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
      const retired = await retireRootMigration(f.execute, prepared, f.journal);
      const stop: Execute = async (args, input) => {
        if (args[0] === "patch" && args[2] === "kars-controller" && !args.includes("--type=json")) throw new Error("interrupted qualification");
        return f.execute(args, input);
      };
      await expect(finalizeRootMigration(stop, retired, f.journal, applyReviewedGrant)).rejects.toThrow("interrupted qualification");
      expect(f.last().phase).toBe("qualifying");
      if (fault === "replacement") namespace.metadata.uid = "replacement-uid";
      else if (fault === "addition") f.objects.set(f.key("namespace", "added"), {
        kind: "Namespace", metadata: { name: "added", uid: "added-uid", resourceVersion: "1" },
      });
      else if (fault === "changed") namespace.metadata.annotations.unknown = "changed";
      else f.objects.delete(f.key("namespace", "third"));
      const mutations = f.mutations.length;
      await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
      expect(f.mutations).toHaveLength(mutations);
      expect(f.grant("core").spec.writers).toEqual([]);
    },
  );

  it("requires an actual new post-retirement key and public CA update without changing contracts", async () => {
    const f = await setup(true);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    const contracts = f.ca.data["contracts.json"];
    const mutations = f.mutations.length;
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
    f.rotate(0);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
    f.rotate(2, false);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("CA");
    expect(f.mutations).toHaveLength(mutations);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.ca.data["contracts.json"]).toBe(contracts);
    expect(f.root.spec.replicas).toBe(2);
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it("cannot withdraw writers when durable preparation fails", async () => {
    const f = await setup();
    await expect(prepareRootMigration(f.execute, f.review, [f.rendered], {
      save: async () => { throw new Error("fsync refused"); },
    })).rejects.toThrow("fsync");
    expect(f.mutations).toEqual([]);
    expect(f.grant("core")).toEqual(f.grantBefore);
  });

  it.each(["grant", "namespace", "image", "command", "scope", "receipt"])("rejects stale or unreviewed %s before writes", async fault => {
    const f = await setup();
    const prepared = await f.prepare();
    if (fault === "grant") f.grant("core").spec.agentKeys.push("UNREVIEWED_TOKEN");
    if (fault === "namespace") f.namespace("third").metadata.annotations.unknown = "changed";
    if (fault === "image") f.root.spec.template.spec.containers[0].image = ORIGINAL.replaceAll("1", "3");
    if (fault === "command") f.root.spec.template.spec.containers[0].command = ["other"];
    if (fault === "scope") f.namespace("third").metadata.annotations[`${P}root-namespace`] = "core";
    if (fault === "receipt") prepared.plan.targetSpec.replicas = 3;
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.root.spec.template.spec.containers[0].image).not.toBe(TARGET);
    // A root execution mismatch is detected by ordinary writer retirement validation,
    // possibly after the intended writer withdrawal, never after a target install.
    expect(f.mutations.every(call => call.args[1] === GRANTS)).toBe(true);
  });

  it("does not silently omit another protected namespace or affiliated grant", async () => {
    const f = await setup();
    for (const review of [
      { ...f.review, grants: [...f.review.grants, { ...f.review.grants[0]!, namespace: "third" }] },
      { ...f.review, activation: { ...f.review.activation, namespaces: [...f.review.activation.namespaces,
        { namespace: { name: "third", uid: "third-uid", resourceVersion: "1" }, consumers: [] }] } },
    ]) {
      await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow();
    }
    expect(f.mutations).toEqual([]);
  });

  it("refuses image plus another target spec change", async () => {
    const f = await setup();
    f.rendered.spec.template.spec.containers[0].command = ["unreviewed"];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["writer-patch", "zero-patch", "pending-patch", "target-patch", "v1-patch", "baseline", "receipt"])(
    "resumes a lost acknowledgement at %s without synthetic retirement", async fault => {
      const f = await setup();
      const prepared = await f.prepare();
      let interrupted = false;
      const run: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (args[0] !== "patch" || args.includes("--dry-run=server") || interrupted) return result;
        const ops = args.includes("--type=json") ? JSON.parse(input!) : [];
        const fields = ops.find((op: any) => op.path === "/metadata/annotations" && op.op === "replace")?.value;
        const merge = args.includes("-p") ? JSON.parse(args[args.indexOf("-p") + 1]!) : {};
        const match = (fault === "writer-patch" && args[1] === GRANTS)
          || (fault === "zero-patch" && args[1] === "deployment" && f.root.spec.template.spec.containers[0].image === ORIGINAL)
          || (fault === "pending-patch" && fields?.[FIELD] === f.seal)
          || (fault === "target-patch" && args[1] === "deployment" && f.root.spec.template.spec.containers[0].image === TARGET)
          || (fault === "v1-patch" && fields && JSON.parse(fields[FIELD]).version === 1)
          || (fault === "baseline" && merge.metadata?.annotations?.[FIELD] && JSON.parse(merge.metadata.annotations[FIELD]).phase === "retired");
        if (match) { interrupted = true; throw new Error("Lost acknowledgement"); }
        return result;
      };
      const journal = { save: async (receipt: RootMigrationReceipt) => {
        if (fault === "receipt" && receipt.phase === "rotation-required" && !interrupted) {
          interrupted = true;
          throw new Error("Lost acknowledgement");
        }
        await f.journal.save(receipt);
      } };
      await expect(retireRootMigration(run, prepared, journal)).rejects.toThrow("Lost acknowledgement");
      expect(interrupted).toBe(true);
      await retireRootMigration(f.execute, f.last(), f.journal);
      expect(f.root.spec.replicas).toBe(0);
      expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
      expect(f.last().phase).toBe("rotation-required");
      await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
    },
  );

  it.each(["namespace", "grant", "key", "pod", "projection", "cas"])("refuses %s races at target projection", async fault => {
    const f = await setup(true);
    const prepared = await f.prepare();
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployment" && args.includes("--dry-run=server")
        && input?.includes(TARGET)) {
        if (fault === "namespace") f.namespace("third").metadata.annotations[`${P}root-namespace`] = "core";
        if (fault === "grant") f.grant("core").spec.agentKeys.push("RACE_TOKEN");
        if (fault === "key") f.rotate(2);
        if (fault === "pod") f.pods.set("core", [rootPod(f, "racing-root")]);
        if (fault === "cas") f.root.metadata.resourceVersion = "9999";
        if (fault === "projection") return JSON.stringify({ ...JSON.parse(result), spec: { ...JSON.parse(result).spec, replicas: 1 } });
      }
      return result;
    };
    await expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow();
    expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
    expect(f.root.spec.replicas).toBe(0);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
  });

  it("does not retire unreviewed private Pods", async () => {
    const f = await setup();
    const prepared = await f.prepare();
    f.pods.get("core")![0].metadata.ownerReferences[0].uid = "other-rs";
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.root.spec.replicas).toBe(2);
    expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
  });

  it("bounds waits for actual Pod deletion and never installs a target on timeout", async () => {
    vi.useFakeTimers();
    try {
      const f = await setup();
      const prepared = await f.prepare();
      f.controls.holdPods = true;
      const result = expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow("timed out");
      await vi.runAllTimersAsync();
      await result;
      expect(f.root.spec.replicas).toBe(0);
      expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
    } finally { vi.useRealTimers(); }
  });

  it("retains writer quiescence until owned read authority is actually absent", async () => {
    vi.useFakeTimers();
    try {
      const f = await setup(), prepared = await f.prepare();
      const run: Execute = async (args, input) => args[1]?.startsWith("roles,")
        ? JSON.stringify({ metadata: {}, items: [{ metadata: { annotations: {
          "kars.azure.com/credential-grant-owner": f.grantBefore.metadata.uid,
        } } }] }) : f.execute(args, input);
      const result = expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("still pending");
      await vi.runAllTimersAsync();
      await result;
      expect(f.root.spec.replicas).toBe(2);
      expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
    } finally { vi.useRealTimers(); }
  });

  it.each(["seal", "publication", "complete-receipt"])("resumes interrupted finalization at %s without rotating twice", async fault => {
    const f = await setup(true);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    let interrupted = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && !interrupted) {
        const patch = args.includes("-p") ? JSON.parse(args[args.indexOf("-p") + 1]!) : {};
        const raw = patch.metadata?.annotations?.[FIELD];
        if ((fault === "seal" && raw && JSON.parse(raw).version === 2)
          || (fault === "publication" && args[1] === GRANTS)) {
          interrupted = true;
          throw new Error("Finalization interrupted");
        }
      }
      return result;
    };
    const journal = { save: async (receipt: RootMigrationReceipt) => {
      if (fault === "complete-receipt" && receipt.phase === "complete" && !interrupted) {
        interrupted = true;
        throw new Error("Finalization interrupted");
      }
      await f.journal.save(receipt);
    } };
    await expect(finalizeRootMigration(run, retired, journal, applyReviewedGrant)).rejects.toThrow("Finalization interrupted");
    expect(interrupted).toBe(true);
    await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
    expect(f.secret.metadata.resourceVersion).toBe("3");
  });

  it.each(["grant", "scope", "contracts", "ca", "template"])("refuses changed %s at finalize without granting writers", async fault => {
    const f = await setup(true);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    if (fault === "grant") f.grant("core").spec.enabled = false;
    if (fault === "scope") f.namespace("third").metadata.annotations[`${P}root-namespace`] = "core";
    if (fault === "contracts") f.ca.data["contracts.json"] = '{"other":"caps"}';
    if (fault === "ca") f.ca.data["ca.crt"] = rootCertificates[3]!;
    if (fault === "template") f.root.spec.template.spec.containers[0].command = ["other"];
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
    expect(f.root.spec.replicas).toBe(0);
  });

  it("registers bounded phases separately from ordinary upgrade and exact-image recovery", () => {
    const command = credentialGrantsCommand().commands.find(value => value.name() === "migrate-root-image");
    expect(command?.commands.map(value => value.name())).toEqual(["retire", "restart-router-baseline", "resume-retirement", "publish-core", "finalize"]);
    const restart = command?.commands.find(value => value.name() === "restart-router-baseline");
    expect(restart?.options.some(option => option.long === "--receipt" && option.required)).toBe(true);
    expect(command?.description()).toContain("same-namespace Deployment companions");
    const review = credentialGrantsCommand().commands.find(value => value.name() === "review-root-migration");
    expect(review?.options.some(option => option.long === "--sealed-root" && option.required)).toBe(true);
  });
});

async function publicationFixture(policyDocument?: any, extraDocuments: any[] = [], runnerOnly = false, companionReplicas?: number, fullBundle = false, foreignReplicas?: number) {
  const f = await setup(true, root => {
    root.spec.progressDeadlineSeconds = 600;
    if (runnerOnly) root.spec.template.spec.containers[0].env.push({ name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: ORIGINAL });
  }, companionReplicas, foreignReplicas), schema = schemaFixture([crd()]);
  if (runnerOnly) {
    const container = f.rendered.spec.template.spec.containers[0];
    container.image = ORIGINAL;
    container.env.find((entry: any) => entry.name === "KARS_CONFORMANCE_RUNNER_IMAGE").value = TARGET;
  }
  schema.owner.namespace = "core";
  const definition = schema.install(crd());
  const policy: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "ToolPolicy", metadata: {
    name: "kars-default", namespace: "core", uid: "tool-policy", resourceVersion: "1", labels: { "app.kubernetes.io/managed-by": "Helm" },
    annotations: { "helm.sh/hook": "post-install,post-upgrade", "helm.sh/hook-weight": "-5" } }, spec: { allowed: "retained-policy" } };
  if (policyDocument) Object.assign(policy, structuredClone(policyDocument), {
    metadata: { ...policyDocument.metadata, uid: "tool-policy", resourceVersion: "1" },
  });
  const objects: any[] = [f.root, f.secret, f.ca, definition, f.namespace("core"), policy,
    ...(fullBundle ? [...f.objects.values()].filter(value => ["ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"].includes(value.kind)) : []),
    ...extraDocuments.map((object, index) => ({ ...structuredClone(object), metadata: {
      ...structuredClone(object.metadata), uid: `extra-${index}`, resourceVersion: "1", generation: 1,
    }, ...(object.kind === "ValidatingAdmissionPolicy" ? { status: { observedGeneration: 1, typeChecking: { expressionWarnings: [] } } } : {}) }))];
  f.namespace("core").kind = "Namespace";
  for (const object of objects) {
    object.apiVersion ??= "v1";
    object.kind ??= object === f.secret ? "Secret" : "Deployment";
    object.metadata.labels = { ...object.metadata.labels, "app.kubernetes.io/managed-by": "Helm" };
    if (object !== policy) object.metadata.annotations = { ...object.metadata.annotations,
      "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" };
  }
  const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
  const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
  const retired = await retireRootMigration(f.execute, prepared, f.journal);
  f.rotate(2);
  const directory = `.core-publication-test-${randomUUID()}`;
  mkdirSync(directory, { mode: 0o700 });
  const file = join(directory, "receipt.jsonl");
  await withRootMigrationJournal(file, true, async journal => journal.save(retired));
  const request = { receipt: file, context: "kind-owned", release: "kars", namespace: "core", chart: "charts/kars", values: [], set: [] };
  const documents = () => objects.map(object => {
    const copy = structuredClone(object);
    delete copy.status;
    for (const field of ["uid", "resourceVersion", "generation", "managedFields"]) delete copy.metadata[field];
    return copy;
  });
  const manifestObjects = (items: any[]) => items.map(object =>
    `${object.metadata.labels?.["app.kubernetes.io/part-of"] === "monitoring"
      ? `# Source: kars/charts/monitoring/templates/${object.kind}.yaml\n` : ""}${JSON.stringify(object)}`).join("\n---\n");
  const manifest = () => manifestObjects(documents().filter(object => object.kind !== "ToolPolicy"));
  const policyHook = () => ({ name: "kars-default", manifest: JSON.stringify(documents().find(object => object.kind === "ToolPolicy")),
    last_run: { phase: "Succeeded" } });
  const encode = (release: any) => Buffer.from(gzipSync(Buffer.from(JSON.stringify(release))).toString("base64")).toString("base64");
  const original: any = { name: "kars", namespace: "core", version: 1, apply_method: "ssa",
    info: { status: "deployed" }, manifest: manifest(), hooks: [policyHook()] };
  const secret = (release: any) => ({ apiVersion: "v1", kind: "Secret", type: "helm.sh/release.v1",
    metadata: { name: `sh.helm.release.v1.kars.v${release.version}`, namespace: "core", uid: `release-${release.version}`,
      resourceVersion: "1", labels: { owner: "helm", name: "kars", version: String(release.version), status: release.info.status } },
    data: { release: encode(release) } });
  const history: any[] = [secret(original)];
  const controls = { launchFailure: false, failedPublication: false, preflightFailure: false, afterSuccessFailure: false, lostReleaseAck: false,
    fenceFailure: false, immutableAckFailure: false, claimAckFailure: false, preparedInterruption: false,
    hooks: false, changedTls: false, failedHistory: false, omittedNamespace: false, launches: 0, fences: 0,
    adoptionBeforePatchFailure: false, adoptionAckFailure: false, adoptionAfterSaveFailure: false, adoptionBadAck: false,
    realHelmRender: false, manifestMutation: "", liveOwnershipChecks: 0, adoptions: 0, writes: 0,
    monitoringHooks: [] as any[], policyHook: undefined as Record<string, any> | undefined,
    config: {} as Record<string, any>, previewMutate: undefined as ((items: any[]) => void) | undefined,
    pendingRootReplicaSet: false, readinessFailure: "", readinessCalls: [] as string[][] };
  const current = async () => {
    let receipt: RootMigrationReceipt | undefined;
    await withRootMigrationJournal(file, false, async (_journal, value) => { receipt = value; });
    return receipt!;
  };
  const savedPublication = () => JSON.parse(readFileSync(file, "utf8").trim().split("\n").at(-1)!).receipt.publication;
  const phase = () => savedPublication()?.phase;
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && ["patch", "create", "replace", "delete"].includes(args[0]!)) controls.writes++;
    if (controls.adoptionAfterSaveFailure && savedPublication()?.adoption?.appliedResourceVersion
      && tool === "kubectl" && args[0] === "apply") {
      controls.adoptionAfterSaveFailure = false;
      throw new Error("interrupted after adoption journal acknowledgement");
    }
    if (controls.preparedInterruption && phase() === "prepared") {
      controls.preparedInterruption = false;
      throw new Error("interrupted before launch");
    }
    if (tool === "helm") {
      expect(settings.env?.HELM_DRIVER).toBe("secret");
      if (args[0] === "version") return { stdout: "v4.1.3" };
      if (args[0] === "list") return { stdout: JSON.stringify([{ name: "kars", namespace: "core" }]) };
      if (args[0] === "history") return { stdout: JSON.stringify([{ revision: 1, status: "deployed" }]) };
      if (args[0] === "get") return { stdout: original.manifest };
      if (args[0] === "template") {
        if (controls.preflightFailure && args.includes("--dry-run=server")) {
          controls.preflightFailure = false;
          throw new Error("server preflight interrupted");
        }
        return { stdout: manifest() };
      }
      if (args[0] === "upgrade" && args.includes("--dry-run=server")) {
        const desired = documents().filter(object => object.kind !== "ToolPolicy");
        if (controls.changedTls) desired.find(value => value.kind === "Secret")!.data["tls.crt"] = "changed";
        if (controls.omittedNamespace) delete desired.find(value => value.kind === "Deployment")!.metadata.namespace;
        controls.previewMutate?.(desired);
        return { stdout: JSON.stringify({ ...original, version: history.length + 1, manifest: manifestObjects(desired),
          hooks: controls.hooks ? [{ name: "writer" }] : [controls.policyHook ?? policyHook(), ...controls.monitoringHooks],
          chart: { metadata: { name: "kars", version: "0.1.0" } }, config: controls.config }) };
      }
      if (args[0] === "upgrade") {
        controls.launches++;
        expect(phase()).toBe("publishing");
        expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
        expect(args).toContain("--server-side=true");
        expect(args).toContain("--wait=hookOnly");
        expect(args.slice(args.indexOf("--history-max"), args.indexOf("--history-max") + 2)).toEqual(["--history-max", "0"]);
        if (controls.launchFailure) throw new Error("Helm resource writer outlived cancellation");
        const frozen = readFileSync(join(args[2]!, "files/publication.yaml"), "utf8");
        const resourceKey = (value: any) => canonical([value.apiVersion, value.kind, value.metadata.namespace ?? "core", value.metadata.name]);
        const existing = new Set(schemaDocuments(original.manifest).map(resourceKey));
        // Helm existingResourceConflict inspects LIVE ownership for resources absent
        // from the predecessor manifest, before it can apply desired annotations.
        for (const object of schemaDocuments(frozen).filter(value => !existing.has(resourceKey(value)))) {
          controls.liveOwnershipChecks++;
          const live = objects.find(value => resourceKey(value) === resourceKey(object))!;
          if (live.metadata.labels?.["app.kubernetes.io/managed-by"] !== "Helm"
            || live.metadata.annotations?.["meta.helm.sh/release-name"] !== "kars"
            || live.metadata.annotations?.["meta.helm.sh/release-namespace"] !== "core") {
            throw new Error("existingResourceConflict: live resource lacks Helm ownership");
          }
        }
        if (original.apply_method !== "ssa" && f.root.metadata.managedFields?.some((field: any) => field.manager === "helm" && field.operation === "Update")) {
          // Helm's CSA field-manager upgrade modifies the original separately from
          // its pinned target. The protected publisher must reject before this.
          f.root.metadata.resourceVersion = String(Number(f.root.metadata.resourceVersion) + 1);
        }
        let publishedManifest = frozen;
        if (controls.realHelmRender) {
          publishedManifest = execFileSync("helm", ["template", "kars", args[2]!, "--namespace", "core"],
            { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] });
          expect(canonical(schemaDocuments(publishedManifest))).not.toBe(canonical(schemaDocuments(frozen)));
          const sortedKinds = schemaDocuments(publishedManifest).map(value => value.kind);
          expect(sortedKinds.indexOf("ToolPolicy")).toBeLessThan(sortedKinds.indexOf("ValidatingAdmissionPolicy"));
          expect(sortedKinds.indexOf("CustomResourceDefinition")).toBeLessThan(sortedKinds.indexOf("Deployment"));
        }
        const released = schemaDocuments(publishedManifest);
        if (controls.manifestMutation === "duplicate") released.push(structuredClone(released[0]!));
        if (controls.manifestMutation === "missing") released.pop();
        if (controls.manifestMutation === "resource-version") released[0]!.metadata.resourceVersion = "stale-version";
        if (controls.manifestMutation === "body") released[0]!.metadata.labels.unreviewed = "true";
        if (controls.manifestMutation === "namespace") released.find(value => value.kind === "ToolPolicy")!.metadata.namespace = "other";
        for (const object of schemaDocuments(frozen)) {
          if (controls.failedPublication && object.kind === "ConfigMap") continue;
          const live = objects.find(value => value.kind === object.kind && value.metadata.name === object.metadata.name)!;
          expect(object.metadata.uid).toBe(live.metadata.uid);
          expect(object.metadata.resourceVersion).toBe(live.metadata.resourceVersion);
          if (live === f.root) object.spec.progressDeadlineSeconds ??= 600;
          Object.assign(live, object, { metadata: { ...live.metadata, ...object.metadata, resourceVersion: String(Number(live.metadata.resourceVersion) + 1) } });
        }
        const predecessor = history.at(-1)!;
        if (!predecessor.immutable && !controls.failedPublication) {
          const previous = JSON.parse(gunzipSync(Buffer.from(Buffer.from(predecessor.data.release, "base64").toString(), "base64")).toString());
          previous.info.status = "superseded";
          predecessor.metadata.labels.status = "superseded";
          predecessor.data.release = encode(previous);
          predecessor.metadata.resourceVersion = String(Number(predecessor.metadata.resourceVersion) + 1);
        }
        history.push(secret({ ...original, version: history.length + 1, apply_method: "ssa", info: { status: controls.failedPublication ? "failed" : "deployed" },
          config: {}, chart: {
            metadata: JSON.parse(readFileSync(join(args[2]!, "Chart.yaml"), "utf8")),
            values: JSON.parse(readFileSync(join(args[2]!, "values.yaml"), "utf8")),
            templates: [{ name: "templates/publication.yaml", data: readFileSync(join(args[2]!, "templates/publication.yaml")).toString("base64") }],
            files: [{ name: "files/publication.yaml", data: Buffer.from(frozen).toString("base64") }],
          }, manifest: released.map(value => JSON.stringify(value)).join("\n---\n"), hooks: [] }));
        if (controls.pendingRootReplicaSet) {
          expect(f.root.spec.replicas).toBe(0);
          f.root.status.observedGeneration = f.root.metadata.generation - 1;
          f.root.status.conditions = [{ type: "Progressing", status: "False", reason: "ReplicaSetCreateError",
            message: "Private capability consumption requires qualified actor authority; an epoch alone grants none" }];
          if (args.includes("--wait=legacy")) throw new Error("context deadline exceeded");
        }
        if (controls.failedPublication) throw new Error("Helm failed after partially applying the frozen snapshot");
        return { stdout: "" };
      }
      throw new Error(`Unexpected Helm command ${args.join(" ")}`);
    }
    if (controls.afterSuccessFailure && phase() === "succeeded") {
      controls.afterSuccessFailure = false;
      throw new Error("interrupted after durable completion");
    }
    if (["rollout", "wait"].includes(args[0]!) && phase() === "succeeded") {
      controls.readinessCalls.push([...args]);
      expect(history.every(entry => entry.immutable === true)).toBe(true);
      if (controls.readinessFailure && args.includes(controls.readinessFailure)) throw new Error("resource readiness timeout");
      return { stdout: "" };
    }
    if (args[0] === "get" && args[1] === "secrets") {
      if (controls.failedHistory) {
        history[0]!.metadata.labels.status = "failed";
        history[0]!.data.release = encode({ ...original, info: { status: "failed" } });
      }
      return { stdout: JSON.stringify({ metadata: {}, items: history }) };
    }
    if (args[0] === "get" && args.includes("--raw")) return schema.execute(tool, args, settings);
    const live = [...objects, ...history].find(value => value.kind.toLowerCase() === args[1]?.toLowerCase()
      && value.metadata.name === args[2]);
    if (args[0] === "get" && live && args.includes("json")) return { stdout: JSON.stringify(live) };
    if (args[0] === "get" && args[1] === "customresourcedefinition") return schema.execute(tool, args, settings);
    if (args[0] === "apply") {
      expect(args).toContain("--dry-run=server");
      const desired = JSON.parse(settings.input!);
      const actual = objects.find(value => value.kind === desired.kind && value.metadata.name === desired.metadata.name)!;
      if (desired.metadata.namespace && args.includes("--namespace")) {
        expect(args[args.indexOf("--namespace") + 1]).toBe(desired.metadata.namespace);
      }
      if (actual === f.root) desired.spec.progressDeadlineSeconds ??= 600;
      return { stdout: JSON.stringify({ ...actual, ...desired, metadata: { ...actual.metadata, ...desired.metadata } }) };
    }
    if (args[0] === "patch" && args.includes("--type=json")) {
      const patch = JSON.parse(settings.input!);
      if (args[1] === "ToolPolicy" && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.value?.["meta.helm.sh/release-name"] === "kars" && !operation.value?.[CORE_PUBLICATION_FENCE])) {
        const publication = JSON.parse(readFileSync(file, "utf8").trim().split("\n").at(-1)!).receipt.publication;
        expect(publication.phase).toBe("preparing");
        expect(publication.adoption.uid).toBe(live.metadata.uid);
        expect(publication.adoption.resourceVersion).toBe(live.metadata.resourceVersion);
        expect(publication.artifact).toBeUndefined();
        if (controls.adoptionBeforePatchFailure) {
          controls.adoptionBeforePatchFailure = false;
          throw new Error("interrupted after journal intent, before adoption CAS");
        }
        expect(patch[0]).toEqual({ op: "test", path: "/metadata/uid", value: live.metadata.uid });
        expect(patch[1]).toEqual({ op: "test", path: "/metadata/resourceVersion", value: live.metadata.resourceVersion });
        expect(patch[2]).toEqual({ op: "test", path: "/metadata/annotations", value: live.metadata.annotations });
        live.metadata.annotations = patch[3].value;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        controls.adoptions++;
        if (controls.adoptionAckFailure) {
          controls.adoptionAckFailure = false;
          throw new Error("adoption acknowledgement lost");
        }
        if (controls.adoptionBadAck) {
          controls.adoptionBadAck = false;
          const result = structuredClone(live);
          delete result.metadata.annotations["meta.helm.sh/release-name"];
          return { stdout: JSON.stringify(result) };
        }
        return { stdout: JSON.stringify(live) };
      }
      if (patch.some((operation: any) => operation.op === "add" && operation.value?.[CORE_PUBLICATION_FENCE])) {
        controls.fences++;
        if (controls.fenceFailure) throw new Error("fence acknowledgement lost");
        expect(patch[0].value).toBe(live.metadata.uid);
        expect(patch[1].value).toBe(live.metadata.resourceVersion);
        live.metadata.annotations = patch[2].value;
        if (patch.some((operation: any) => operation.path === "/immutable" && operation.value === true)) live.immutable = true;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        if (controls.immutableAckFailure && live.type === "helm.sh/release.v1") {
          return { stdout: JSON.stringify({ ...live, immutable: false }) };
        }
        return { stdout: JSON.stringify(live) };
      }
      if (controls.claimAckFailure && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.op === "replace" && JSON.parse(operation.value?.[CORE_PUBLICATION] ?? "{}").phase === "publishing")) {
        await f.execute([...args], settings.input);
        throw new Error("launch ownership acknowledgement lost");
      }
      if (controls.lostReleaseAck && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.op === "replace" && !operation.value[CORE_PUBLICATION])) {
        controls.lostReleaseAck = false;
        await f.execute([...args], settings.input);
        throw new Error("ownership release acknowledgement lost");
      }
    }
    return { stdout: await f.execute([...args], settings.input) };
  };
  const cleanup = () => {
    for (const item of readdirSync(directory, { withFileTypes: true })) if (item.isDirectory()) {
      for (const child of [".", "templates", "files"]) chmodSync(join(directory, item.name, child), 0o700);
    }
    rmSync(directory, { recursive: true, force: true });
  };
  return { ...f, request, execute, current, controls, history, retired, cleanup, policy, original, encode, objects,
    migrationExecute: f.execute, rootPod: () => rootPod(f), publish: () => publishPrivateCore(execute, request) };
}

async function witnessPublicationFixture(replicas = 1, fullBundle = false) {
  const f = await publicationFixture(undefined, [
    { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "kars-datapath-witness", namespace: "core",
      labels: { "kars.azure.com/witness-addon": "true" } }, data: { "witness.json": "retained runtime evidence" } },
    { apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "aggregator", namespace: "custom-witness",
      labels: { "kars.azure.com/witness-addon": "true" } }, spec: { replicas,
      selector: { matchLabels: { "app.kubernetes.io/component": "aggregator" } },
      template: { metadata: { labels: { "app.kubernetes.io/component": "aggregator" } },
        spec: { containers: [{ name: "aggregator", image: "witness:original" }] } } },
    },
  ], false, undefined, fullBundle);
  const writer = f.objects.find(value => value.metadata.name === "aggregator")!;
  const evidence = f.objects.find(value => value.metadata.name === "kars-datapath-witness")!;
  writer.status = { observedGeneration: writer.metadata.generation, replicas };
  const staleWitnessVersion = evidence.metadata.resourceVersion;
  const state = { restoreAckLost: false, rescaleBeforeLaunch: false, pauseAckLost: false, evidenceAckLost: false,
    afterLaunch: undefined as (() => void) | undefined, writes: [] as number[] };
  f.controls.previewMutate = documents => {
    const desired = documents.find(value => value.metadata.name === "aggregator")!;
    desired.spec.replicas = 1;
    desired.spec.template.spec.containers[0].image = "witness:updated";
  };
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && args[0] === "get" && args.includes("custom-witness")
      && ["pods", "replicasets", "horizontalpodautoscalers"].includes(args[1]!)) return { stdout: JSON.stringify({ metadata: {}, items: [] }) };
    if (tool === "kubectl" && args[0] === "patch" && args[1] === "Deployment" && args[2] === "aggregator") {
      const patch = JSON.parse(settings.input!);
      const scale = patch.find((op: any) => op.path === "/spec/replicas");
      if (scale) {
        expect(patch.slice(0, 3)).toEqual([
          { op: "test", path: "/metadata/uid", value: writer.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: writer.metadata.resourceVersion },
          { op: "test", path: "/spec", value: writer.spec },
        ]);
        const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
        expect(receipt.publication.phase).toBe(scale.value === 0 ? "preparing" : "verified");
        expect(receipt.publication.witnessWriter.deployment.spec.replicas).toBe(replicas);
        writer.spec.replicas = scale.value;
        writer.metadata.resourceVersion = String(Number(writer.metadata.resourceVersion) + 1);
        writer.metadata.generation++;
        writer.status = { observedGeneration: writer.metadata.generation, replicas: scale.value };
        state.writes.push(scale.value);
        if (state.restoreAckLost && scale.value === 1) { state.restoreAckLost = false; throw new Error("restore acknowledgement lost"); }
        if (state.pauseAckLost && scale.value === 0) { state.pauseAckLost = false; throw new Error("pause acknowledgement lost"); }
        return { stdout: JSON.stringify(writer) };
      }
    }
    if (tool === "kubectl" && args[0] === "get" && args[1]?.toLowerCase() === "deployment" && args[2] === "aggregator"
      && state.rescaleBeforeLaunch) {
      const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
      if (receipt.publication?.phase === "prepared") writer.spec.replicas = 1;
    }
    if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
      expect(writer.spec.replicas).toBe(0);
      expect(evidence.metadata.resourceVersion).not.toBe(staleWitnessVersion);
      expect(evidence.metadata.annotations[CORE_PUBLICATION_FENCE]).toBeDefined();
      expect(evidence.data["witness.json"]).toBe("retained runtime evidence");
    }
    if (tool === "kubectl" && args[0] === "patch" && args[2] === "kars-datapath-witness") {
      const annotations = JSON.parse(settings.input!).find((op: any) => op.path === "/metadata/annotations").value;
      expect(canonical(annotations)).not.toBe(canonical(evidence.metadata.annotations));
    }
    const result = await f.execute(tool, args, settings);
    if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) state.afterLaunch?.();
    if (state.evidenceAckLost && tool === "kubectl" && args[0] === "patch" && args[2] === "kars-datapath-witness") {
      state.evidenceAckLost = false;
      throw new Error("evidence fence acknowledgement lost");
    }
    return result;
  };
  return { ...f, execute, writer, evidence, state, publish: () => publishPrivateCore(execute, f.request) };
}

describe("protected publication with a live datapath writer", () => {
  it.each([undefined, "4"].flatMap(before => [false, true].map(omitted => ({ before, omitted }))))(
    "accepts only the witness controller revision advancing from $before (omitted from chart: $omitted)", async ({ before, omitted }) => {
    const f = await witnessPublicationFixture();
    try {
      const key = "deployment.kubernetes.io/revision";
      if (before) f.writer.metadata.annotations[key] = before;
      const preview = f.controls.previewMutate;
      f.controls.previewMutate = documents => {
        preview?.(documents);
        if (omitted) delete documents.find(value => value.metadata.name === "aggregator")!.metadata.annotations[key];
      };
      const execute: SchemaExecute = async (tool, args, settings) => {
        const result = await f.execute(tool, args, settings);
        if (omitted && before && tool === "kubectl" && args[0] === "apply") {
          const projected = JSON.parse(result.stdout);
          if (projected.metadata.uid === f.writer.metadata.uid) {
            projected.metadata.annotations[key] = before;
            return { stdout: JSON.stringify(projected) };
          }
        }
        return result;
      };
      f.state.afterLaunch = () => { f.writer.metadata.annotations[key] = "5"; };
      await publishPrivateCore(execute, f.request);
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.writer.metadata.annotations[key]).toBe("5");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["backwards", "invalid", "label", "annotation", "uid", "spec", "other-deployment"])(
    "does not hide %s drift behind a witness revision change", async mode => {
      const f = await witnessPublicationFixture();
      try {
        const key = "deployment.kubernetes.io/revision";
        f.writer.metadata.annotations[key] = "4";
        f.state.afterLaunch = () => {
          f.writer.metadata.annotations[key] = mode === "backwards" ? "3" : mode === "invalid" ? "invalid" : "5";
          if (mode === "label") f.writer.metadata.labels.unreviewed = "true";
          if (mode === "annotation") f.writer.metadata.annotations.unreviewed = "true";
          if (mode === "uid") f.writer.metadata.uid = "replacement";
          if (mode === "spec") f.writer.spec.template.spec.containers[0].image = "unreviewed";
          if (mode === "other-deployment") f.root.metadata.annotations[key] = "5";
        };
        await expect(f.publish()).rejects.toThrow();
        expect((await f.current()).publication?.phase).toBe("publishing");
        expect(f.writer.spec.replicas).toBe(0);
        expect(f.controls.launches).toBe(1);
      } finally { f.cleanup(); }
    });

  it.each([0, 1])("keeps replica intent %s paused through the frozen launch and fences, then restores", async replicas => {
    const f = await witnessPublicationFixture(replicas);
    try {
      await f.publish();
      expect(f.writer.spec.replicas).toBe(replicas);
      expect(f.writer.spec.template.spec.containers[0].image).toBe("witness:updated");
      expect(f.evidence.data["witness.json"]).toBe("retained runtime evidence");
      expect((await f.current()).publication?.witnessWriter?.restored).toBe(true);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["pauseAckLost", "evidenceAckLost", "restoreAckLost"] as const)("resumes %s without replaying the Helm launch", async mode => {
    const f = await witnessPublicationFixture();
    try {
      f.state[mode] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe(mode === "restoreAckLost" ? "verified" : "preparing");
      if (mode === "restoreAckLost") {
        const run: Execute = async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", ...(input ? { input } : {}) })).stdout;
        await expect(finalizeRootMigration(run, await f.current(), f.journal, async () => {
          throw new Error("Unexpected grant publication");
        })).rejects.toThrow("datapath writer is not restored");
      }
      await f.publish();
      expect(f.controls.launches).toBe(1);
      expect(f.state.writes).toEqual([0, 1]);
      expect((await f.current()).publication?.witnessWriter?.restored).toBe(true);
    } finally { f.cleanup(); }
  });

  it("refuses a writer that scales up after preparation", async () => {
    const f = await witnessPublicationFixture();
    try {
      f.state.rescaleBeforeLaunch = true;
      await expect(f.publish()).rejects.toThrow("writer changed or has not stopped");
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it("does not restore or finalize an unknown Helm outcome", async () => {
    const f = await witnessPublicationFixture();
    try {
      f.controls.launchFailure = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      expect(f.writer.spec.replicas).toBe(0);
      expect((await f.current()).publication?.witnessWriter?.restored).toBeUndefined();
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      const run: Execute = async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", ...(input ? { input } : {}) })).stdout;
      await expect(finalizeRootMigration(run, await f.current(), f.journal, async () => {
        throw new Error("Unexpected grant publication");
      })).rejects.toThrow("publication is owned or its outcome is unknown");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });
});

async function failedPublicationFixture(fullBundle = false) {
  const f = await publicationFixture(undefined, [], false, undefined, fullBundle);
  f.controls.policyHook = structuredClone(f.original.hooks[0]);
  f.controls.failedPublication = true;
  await expect(f.publish()).rejects.toThrow("outcome is unknown");
  f.controls.failedPublication = false;
  const failed = (await f.current()).publication!;
  return { ...f, failed, recover: (execute = f.execute) => publishPrivateCore(execute, { ...f.request, recoverFailedPublication: true }) };
}

describe("protected publication admission continuity", () => {
  it.each([0, 1].flatMap(replicas => ["core", "kars-sre"].map(namespace => ({ replicas, namespace }))))(
    "publishes multi-scope admission and resumes $namespace bundle CAS with replicas=$replicas", async ({ replicas, namespace }) => {
    const f = await publicationFixture(undefined, [], false, 1, true, replicas);
    try {
      let interrupted = false;
      const before = await f.current();
      const execute: SchemaExecute = async (tool, args, settings) => {
        const result = await f.execute(tool, args, settings);
        if (!interrupted && tool === "kubectl" && args[0] === "patch" && args[1] === "namespace" && args[2] === namespace
          && f.namespace(namespace).metadata.annotations[`${P}bundle-revision`] !== before.activation!.bundleRevision) {
          interrupted = true;
          throw new Error("foreign bundle acknowledgement lost");
        }
        return result;
      };
      await expect(publishPrivateCore(execute, f.request)).rejects.toThrow();
      expect(interrupted).toBe(true);
      await f.publish();
      const published = await f.current();
      expect(published.publication?.phase).toBe("verified");
      expect(published.retirement!.captured).toEqual(before.retirement!.captured);
      expect(f.namespace("kars-sre").metadata.annotations[`${P}bundle-revision`]).toBe(published.activation!.bundleRevision);
      const result = await finalizeRootMigration(f.migrationExecute, published, f.journal, applyReviewedGrant);
      expect(result.phase).toBe("complete");
      expect(f.foreign.spec.replicas).toBe(replicas);
      expect(f.grant("core").spec.privateActivation.namespaces).toHaveLength(2);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each([false, true].flatMap(recovery => ["before-cas", "lost-cas-ack", "after-save"].map(point => ({ recovery, point }))))(
    "resumes the same admission transition after $point (recovery=$recovery)", async ({ recovery, point }) => {
      const f = recovery ? await failedPublicationFixture(true) : await publicationFixture(undefined, [], false, undefined, true);
      try {
        let interrupted = false;
        const execute: SchemaExecute = async (tool, args, settings) => {
          const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
          const patch = tool === "kubectl" && args[0] === "patch" && args[1] === "namespace" && settings.input ? JSON.parse(settings.input) : [];
          const transition = patch.some((op: any) => op.op === "replace" && op.path === "/metadata/annotations"
            && op.value[`${P}bundle-revision`] !== f.namespace("core").metadata.annotations[`${P}bundle-revision`]);
          if (!interrupted && ((point === "after-save" && receipt.bundleTransitions?.at(-1)?.applied) || (transition && point !== "after-save"))) {
            interrupted = true;
            if (point === "lost-cas-ack") await f.execute(tool, args, settings);
            throw new Error("admission transition interrupted");
          }
          return f.execute(tool, args, settings);
        };
        const request = { ...f.request, recoverFailedPublication: recovery };
        await expect(publishPrivateCore(execute, request)).rejects.toThrow();
        expect(interrupted).toBe(true);
        const reserved = (await f.current()).bundleTransitions![0]!;
        await publishPrivateCore(f.execute, request);
        const after = await f.current();
        expect(after.publication?.phase).toBe("verified");
        expect(after.bundleTransitions![0]).toEqual({ ...reserved, applied: true });
        expect(f.controls.launches).toBe(recovery ? 2 : 1);
      } finally { f.cleanup(); }
    });

  it("resumes already-fenced recovery without a prior admission-transition journal", async () => {
    const f = await failedPublicationFixture(true);
    try {
      let interrupted = false;
      await expect(f.recover(async (tool, args, settings) => {
        const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
        const marker = `${receipt.publication.recovery?.attempt}:failed-publication`;
        if (!interrupted && f.objects.every(value => value.metadata.annotations?.[CORE_PUBLICATION_FENCE] === marker)
          && f.history.every(value => value.immutable === true && value.metadata.annotations?.[CORE_PUBLICATION_FENCE] === marker)) {
          interrupted = true;
          throw new Error("old publisher stopped after fencing");
        }
        return f.execute(tool, args, settings);
      })).rejects.toThrow();
      expect(interrupted).toBe(true);
      expect((await f.current()).bundleTransitions).toBeUndefined();
      await f.recover();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "spec", "annotation", "unobserved", "type-warning", "foreign-fence"])("refuses changed admission %s before recovery ownership", async mutation => {
    const f = await failedPublicationFixture(true);
    try {
      const policy = f.objects.find(value => value.kind === "ValidatingAdmissionPolicy")!;
      if (mutation === "uid") policy.metadata.uid += "-replaced";
      if (mutation === "spec") policy.spec.failurePolicy = "Ignore";
      if (mutation === "annotation") policy.metadata.annotations.unreviewed = "changed";
      if (mutation === "unobserved") policy.status.observedGeneration = 0;
      if (mutation === "type-warning") policy.status.typeChecking.expressionWarnings = [{ warning: "invalid" }];
      if (mutation === "foreign-fence") policy.metadata.annotations[CORE_PUBLICATION_FENCE] = "foreign";
      const owner = f.namespace("core").metadata.annotations[CORE_PUBLICATION];
      const writes = f.controls.writes;
      await expect(f.recover()).rejects.toThrow();
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBe(owner);
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it("finalizes the current admission revision without losing companion or retirement intent", async () => {
    const f = await publicationFixture(undefined, [], false, 2, true);
    try {
      await f.publish();
      const published = await f.current();
      const result = await finalizeRootMigration(f.migrationExecute, published, f.journal, applyReviewedGrant);
      expect(result.phase).toBe("complete");
      expect(result.retirement).toEqual(published.retirement);
      expect(f.companion.spec.replicas).toBe(2);
      expect(f.root.spec.replicas).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each([false, true])("preserves retired authority across full admission bundle fences (recovery=%s)", async recovery => {
    const f = recovery ? await failedPublicationFixture(true) : await publicationFixture(undefined, [], false, undefined, true);
    try {
      const before = await f.current();
      await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: recovery });
      const after = await f.current();
      expect(after.publication?.phase).toBe("verified");
      expect(after.activation!.bundleRevision).not.toBe(before.activation!.bundleRevision);
      expect(after.retirement).toEqual({ ...before.retirement, binding: after.retirement!.binding });
      expect(after.retirement!.attempt).toBe(before.retirement!.attempt);
      expect(f.root.spec.replicas).toBe(0);
      expect(f.controls.launches).toBe(recovery ? 2 : 1);
    } finally { f.cleanup(); }
  });
});

describe("failed protected publication recovery", () => {
  it.each(["publishing", "succeeded"].flatMap(phase => ["none", "claim", "resource", "storage"].map(boundary => [phase, boundary])))(
    "fences a fully projected deployed revision from %s before a fresh successor, resuming %s", async (phase, boundary) => {
      const f = await witnessPublicationFixture(1, true);
      try {
        const key = "deployment.kubernetes.io/revision";
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.writer.metadata.annotations[key] = "4";
        f.state.afterLaunch = () => {
          f.writer.metadata.annotations[key] = "5";
          f.controls.fenceFailure = phase === "publishing";
          f.controls.readinessFailure = phase === "succeeded" ? "Deployment/aggregator" : "";
        };
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        const original = structuredClone((await f.current()).publication!);
        const releaseData = f.history[1].data.release;
        expect(f.history[1].metadata.labels.status).toBe("deployed");
        expect(original.phase).toBe(phase);
        f.state.afterLaunch = undefined;
        f.controls.fenceFailure = false;
        f.controls.readinessFailure = "";
        let interrupted = false;
        const execute: SchemaExecute = async (tool, args, options) => {
          const patch = tool === "kubectl" && args[0] === "patch" ? JSON.parse(options.input!) : [];
          const claim = patch.some((op: any) => op.path === "/metadata/annotations" && op.op === "replace"
            && JSON.parse(op.value?.[CORE_PUBLICATION] ?? "{}").phase === "preparing");
          const fence = patch.some((op: any) => String(op.value?.[CORE_PUBLICATION_FENCE] ?? "").endsWith(":failed-publication"));
          const result = await f.execute(tool, args, options);
          if (!interrupted && ((boundary === "claim" && claim)
            || (fence && (boundary === "storage" ? args[1] === "secret" : boundary === "resource" && args[1] !== "secret")))) {
            interrupted = true;
            throw new Error("lost recovery acknowledgement");
          }
          return result;
        };
        const request = { ...f.request, recoverFailedPublication: true };
        if (boundary !== "none") {
          await expect(publishPrivateCore(execute, request)).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
          expect(interrupted).toBe(true);
          expect(f.controls.launches).toBe(1);
        }
        await publishPrivateCore(f.execute, request);
        const current = await f.current();
        expect(current.publication?.phase).toBe("verified");
        expect(current.publication?.attempt).not.toBe(original.attempt);
        expect(current.publication?.revision).toBe(3);
        expect(current.failedPublications?.[0].recovery?.fenced).toBe(true);
        expect(f.history[1].data.release).toBe(releaseData);
        expect(f.history[1].metadata.labels.status).toBe("deployed");
        expect(f.history[1].immutable).toBe(true);
        expect(f.writer.spec.replicas).toBe(1);
        expect(f.root.spec.replicas).toBe(0);
        expect(f.controls.launches).toBe(2);
        await publishPrivateCore(f.execute, request);
        expect(f.controls.launches).toBe(2);
      } finally { f.cleanup(); }
    });

  it.each(["publishing", "succeeded"].flatMap(phase => ["resource-body", "writer-running", "writer-uid"].map(mode => [phase, mode])))(
    "refuses deployed recovery from %s with %s drift before ownership or fencing writes", async (phase, mode) => {
      const f = await witnessPublicationFixture();
      try {
        f.state.afterLaunch = () => {
          f.controls.fenceFailure = phase === "publishing";
          f.controls.readinessFailure = phase === "succeeded" ? "Deployment/aggregator" : "";
        };
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        expect((await f.current()).publication?.phase).toBe(phase);
        f.state.afterLaunch = undefined;
        f.controls.fenceFailure = false;
        f.controls.readinessFailure = "";
        if (mode === "resource-body") f.evidence.data["witness.json"] = "changed";
        if (mode === "writer-running") f.writer.spec.replicas = 1;
        if (mode === "writer-uid") f.writer.metadata.uid = "replacement";
        const writes = f.controls.writes;
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.controls.writes).toBe(writes);
        expect(f.controls.launches).toBe(1);
        expect((await f.current()).publication?.recovery).toBeUndefined();
      } finally { f.cleanup(); }
    });

  it.each(["none", "server-defaulted-revision", "spec", "uid", "annotation", "revision-backward", "revision-invalid"])(
    "publishes a bounded monitoring readiness repair with only legitimate Deployment revision drift (%s)", async mode => {
    const f = await monitoringCapacityFixture();
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      const desired = f.controls.previewMutate;
      f.controls.config.monitoring["kube-state-metrics"].resources = structuredClone(f.original.config.monitoring["kube-state-metrics"].resources);
      f.controls.previewMutate = () => {};
      f.metrics.metadata.annotations["deployment.kubernetes.io/revision"] = "2";
      f.controls.readinessFailure = "Deployment/custom-kube-state-metrics";
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      expect((await f.current()).publication?.phase).toBe("succeeded");
      const original = f.history[1].data.release;
      f.controls.config.monitoring.kubeStateMetricsResourcesBaseline = structuredClone(f.original.config.monitoring["kube-state-metrics"].resources);
      f.controls.config.monitoring["kube-state-metrics"].resources = structuredClone(f.resources);
      f.controls.previewMutate = documents => {
        desired?.(documents);
        if (mode === "server-defaulted-revision") delete documents.find(value => value.metadata.name === f.metrics.metadata.name)!
          .metadata.annotations["deployment.kubernetes.io/revision"];
      };
      f.controls.readinessFailure = "";
      const execute: SchemaExecute = async (tool, args, settings) => {
        const result = await f.execute(tool, args, settings);
        if (mode === "server-defaulted-revision" && tool === "kubectl" && args[0] === "apply") {
          const projected = JSON.parse(result.stdout);
          if (projected.metadata.uid === f.metrics.metadata.uid) {
            projected.metadata.annotations["deployment.kubernetes.io/revision"] = "2";
            return { stdout: JSON.stringify(projected) };
          }
        }
        if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          f.metrics.metadata.annotations["deployment.kubernetes.io/revision"] = mode === "revision-backward" ? "1"
            : mode === "revision-invalid" ? "invalid" : "3";
          if (mode === "spec") f.metrics.spec.template.spec.containers[0].image = "unreviewed";
          if (mode === "uid") f.metrics.metadata.uid = "replacement";
          if (mode === "annotation") f.metrics.metadata.annotations.unreviewed = "changed";
        }
        return result;
      };
      const result = publishPrivateCore(execute, { ...f.request, recoverFailedPublication: true });
      if (!["none", "server-defaulted-revision"].includes(mode)) {
        await expect(result).rejects.toThrow();
        expect((await f.current()).publication?.phase).toBe("publishing");
        expect(f.history[1].data.release).toBe(original);
        expect(f.controls.launches).toBe(2);
        expect(f.root.spec.replicas).toBe(0);
        return;
      }
      await result;
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.metrics.spec.template.spec.containers[0].resources).toEqual(f.resources);
      expect(f.history[1].data.release).toBe(original);
      expect(f.history[1].immutable).toBe(true);
      expect(f.controls.launches).toBe(2);
      expect(f.root.spec.replicas).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "spec", "annotation", "unobserved", "type-warning", "foreign-fence"])(
    "refuses succeeded recovery with changed admission %s before ownership", async mutation => {
      const f = await witnessPublicationFixture(1, true);
      try {
        f.controls.readinessFailure = "Deployment/aggregator";
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        expect((await f.current()).publication?.phase).toBe("succeeded");
        const policy = f.objects.find(value => value.kind === "ValidatingAdmissionPolicy")!;
        if (mutation === "uid") policy.metadata.uid += "-replaced";
        if (mutation === "spec") policy.spec.failurePolicy = "Ignore";
        if (mutation === "annotation") policy.metadata.annotations.unreviewed = "changed";
        if (mutation === "unobserved") policy.status.observedGeneration = 0;
        if (mutation === "type-warning") policy.status.typeChecking.expressionWarnings = [{ warning: "invalid" }];
        if (mutation === "foreign-fence") policy.metadata.annotations[CORE_PUBLICATION_FENCE] = "foreign";
        const owner = f.namespace("core").metadata.annotations[CORE_PUBLICATION];
        const writes = f.controls.writes;
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBe(owner);
        expect(f.controls.writes).toBe(writes);
        expect(f.controls.launches).toBe(1);
      } finally { f.cleanup(); }
    });

  it.each(["mutable-storage", "failed-status"])("refuses succeeded recovery with %s before writes", async mode => {
    const f = await witnessPublicationFixture();
    try {
      f.controls.readinessFailure = "Deployment/aggregator";
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      expect((await f.current()).publication?.phase).toBe("succeeded");
      if (mode === "mutable-storage") delete f.history[1].immutable;
      else {
        const release = JSON.parse(gunzipSync(Buffer.from(Buffer.from(f.history[1].data.release, "base64").toString(), "base64")).toString());
        release.info.status = "failed";
        f.history[1].metadata.labels.status = "failed";
        f.history[1].data.release = f.encode(release);
      }
      const writes = f.controls.writes;
      await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.recovery).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it("refuses an uninspected Helm version before claiming or fencing recovery", async () => {
    const f = await failedPublicationFixture();
    try {
      const before = structuredClone([f.objects, f.history, await f.current()]);
      await expect(f.recover(async (tool, args, settings) => tool === "helm" && args[0] === "version"
        ? { stdout: "v4.2.0" } : f.execute(tool, args, settings))).rejects.toThrow("inspected Helm 4.1.3");
      expect([f.objects, f.history, await f.current()]).toEqual(before);
    } finally { f.cleanup(); }
  });

  it("retains completed monitoring bootstrap provenance through failed recovery", async () => {
    const f = await monitoringPublicationFixture();
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["changed-defaults", "missing-defaults", "explicit-override", "unexpected-override", "changed-template",
    "extra-template", "renamed-file", "extra-file", "changed-file", "dependencies", "schema", "wrong-chart"])(
    "refuses unproven frozen monitoring configuration: %s", async mode => {
      const f = await monitoringPublicationFixture();
      try {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.controls.failedPublication = true;
        await expect(f.publish()).rejects.toThrow("outcome is unknown");
        f.controls.failedPublication = false;
        const entry = f.history.at(-1)!;
        const release = JSON.parse(gunzipSync(Buffer.from(Buffer.from(entry.data.release, "base64").toString(), "base64")).toString());
        expect(release.config).toEqual({});
        expect(release.chart.values.monitoring).toEqual(f.controls.config.monitoring);
        if (mode === "changed-defaults") release.chart.values.monitoring.enabled = false;
        if (mode === "missing-defaults") delete release.chart.values.monitoring;
        if (mode === "explicit-override") release.config.monitoring = { enabled: false };
        if (mode === "unexpected-override") release.config.unreviewed = true;
        if (mode === "changed-template") release.chart.templates[0].data = Buffer.from("{{ .Values.unreviewed }}").toString("base64");
        if (mode === "extra-template") release.chart.templates.push(structuredClone(release.chart.templates[0]));
        if (mode === "renamed-file") release.chart.files[0].name = "files/unreviewed.yaml";
        if (mode === "extra-file") release.chart.files.push(structuredClone(release.chart.files[0]));
        if (mode === "changed-file") {
          const documents = schemaDocuments(Buffer.from(release.chart.files[0].data, "base64").toString());
          documents[0]!.metadata.labels.unreviewed = "true";
          release.chart.files[0].data = Buffer.from(documents.map(value => JSON.stringify(value)).join("\n---\n")).toString("base64");
        }
        if (mode === "dependencies") release.chart.metadata.dependencies = [{ name: "unreviewed" }];
        if (mode === "schema") release.chart.schema = Buffer.from("{}").toString("base64");
        if (mode === "wrong-chart") release.chart.metadata.name = "unreviewed";
        entry.data.release = f.encode(release);
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.controls.launches).toBe(1);
        expect(f.root.spec.replicas).toBe(0);
      } finally { f.cleanup(); }
    });

  it.each(["unchanged", "changed-scope", "future-revision", "invalid-revision"])("handles settings left by a failed publication: %s", async mode => {
    const config = { schema_version: 1, release_revision: 1, scope: "all", sandboxes: [], enabled: true };
    const f = await publicationFixture(undefined, [{ apiVersion: "v1", kind: "ConfigMap", metadata: {
      name: "kars-datapath-witness-settings", namespace: "core", labels: { "kars.azure.com/witness-addon": "true" },
    }, data: { "settings.json": JSON.stringify(config) } }]);
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.previewMutate = documents => {
        documents.find(value => value.metadata.name === "kars-datapath-witness-settings")!.data["settings.json"] =
          JSON.stringify({ ...config, release_revision: f.history.length + 1 });
      };
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      const settings = f.objects.find(value => value.metadata.name === "kars-datapath-witness-settings")!;
      expect(JSON.parse(settings.data["settings.json"]).release_revision).toBe(1);
      if (mode !== "unchanged") settings.data["settings.json"] = JSON.stringify({ ...config,
        ...(mode === "changed-scope" ? { scope: "selected" } : { release_revision: mode === "future-revision" ? 99 : "1" }) });
      const recover = () => publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      if (mode === "unchanged") {
        await recover();
        expect(JSON.parse(settings.data["settings.json"]).release_revision).toBe(3);
        expect((await f.current()).publication?.phase).toBe("verified");
      } else {
        await expect(recover()).rejects.toThrow("must remain semantically unchanged");
        expect(f.controls.launches).toBe(1);
      }
    } finally { f.cleanup(); }
  });

  it.each([0, 1])("carries original witness replica intent %s through partial failure and fresh publication", async replicas => {
    const f = await witnessPublicationFixture(replicas);
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      expect(f.writer.spec.replicas).toBe(0);
      f.controls.failedPublication = false;
      await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      expect(f.writer.spec.replicas).toBe(replicas);
      expect(f.writer.spec.template.spec.containers[0].image).toBe("witness:updated");
      expect(f.evidence.data["witness.json"]).toBe("retained runtime evidence");
      expect((await f.current()).publication?.witnessWriter?.restored).toBe(true);
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("resumes after the fenced successor is journaled without reconstructing the failed attempt", async () => {
    const f = await failedPublicationFixture();
    try {
      f.controls.preflightFailure = true;
      await expect(f.recover()).rejects.toThrow();
      const current = await f.current();
      expect(current.publication?.phase).toBe("preparing");
      expect(current.failedPublications?.[0].recovery?.fenced).toBe(true);
      const attempt = current.publication?.attempt;
      await f.recover();
      expect((await f.current()).publication?.attempt).toBe(attempt);
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("retains each failed attempt when a fresh successor also fails", async () => {
    const f = await failedPublicationFixture();
    try {
      f.controls.failedPublication = true;
      await expect(f.recover()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      await f.recover();
      const current = await f.current();
      expect(current.publication?.revision).toBe(4);
      expect(current.failedPublications?.map(value => value.revision)).toEqual([2, 3]);
      expect(new Set([...current.failedPublications!, current.publication!].map(value => value.attempt)).size).toBe(3);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "failed", "failed", "deployed"]);
      expect(f.controls.launches).toBe(3);
    } finally { f.cleanup(); }
  });

  it.each(["bad-resource-ack", "bad-immutable-ack", "storage-drift"])("refuses %s before a successor can launch", async mutation => {
    const f = await failedPublicationFixture();
    try {
      if (mutation === "bad-immutable-ack") f.controls.immutableAckFailure = true;
      let changed = false;
      const execute: SchemaExecute = async (tool, args, options) => {
        const before = tool === "kubectl" && args[0] === "patch" && args[2] === f.policy.metadata.name
          ? structuredClone(f.policy) : undefined;
        const result = await f.execute(tool, args, options);
        if (!changed && before) {
          changed = true;
          if (mutation === "bad-resource-ack") return { stdout: JSON.stringify(before) };
          if (mutation === "storage-drift") f.history[1].data.release = f.encode({ ...f.original, version: 2, info: { status: "failed" }, manifest: "changed" });
        }
        return result;
      };
      await expect(f.recover(execute)).rejects.toThrow();
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).failedPublications).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it("invalidates every old request, retains immutable failed history and launches only a fresh attempt", async () => {
    const f = await failedPublicationFixture();
    try {
      const failedData = f.history[1].data.release;
      const frozen = schemaDocuments(readFileSync(join(f.failed.artifact!, "files/publication.yaml"), "utf8"));
      const execute: SchemaExecute = async (tool, args, options) => {
        if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          expect(f.history.every(value => value.immutable === true)).toBe(true);
          for (const object of frozen) {
            const live = f.objects.find(value => value.kind === object.kind && value.metadata.name === object.metadata.name)!;
            expect(live.metadata.uid).toBe(object.metadata.uid);
            expect(live.metadata.resourceVersion).not.toBe(object.metadata.resourceVersion);
          }
          expect(JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt.publication.attempt).not.toBe(f.failed.attempt);
        }
        return f.execute(tool, args, options);
      };
      await f.recover(execute);
      const current = await f.current();
      expect(current.publication?.phase).toBe("verified");
      expect(current.publication?.revision).toBe(3);
      expect(current.failedPublications).toHaveLength(1);
      expect(current.failedPublications![0].artifact).toBe(f.failed.artifact);
      expect(current.failedPublications![0].recovery?.fenced).toBe(true);
      expect(f.history[1].data.release).toBe(failedData);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "failed", "deployed"]);
      expect(f.controls.launches).toBe(2);
      await f.recover();
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["pending-upgrade", "deployed-resource-drift", "absent", "manifest", "hooks", "csa", "prior-data", "prior-uid", "gap"])("refuses %s history before recovery ownership or writes", async mutation => {
    const f = await failedPublicationFixture();
    try {
      const release: any = JSON.parse(gunzipSync(Buffer.from(Buffer.from(f.history[1].data.release, "base64").toString(), "base64")).toString());
      if (["pending-upgrade", "deployed-resource-drift"].includes(mutation)) {
        release.info.status = mutation === "deployed-resource-drift" ? "deployed" : mutation;
        f.history[1].metadata.labels.status = release.info.status;
        if (mutation === "deployed-resource-drift") f.root.metadata.labels.unreviewed = "true";
      }
      if (mutation === "manifest") release.manifest += `\n---\n${JSON.stringify(schemaDocuments(release.manifest)[0])}`;
      if (mutation === "hooks") release.hooks = [{ kind: "Job", manifest: "unreviewed writer" }];
      if (mutation === "csa") release.apply_method = "csa";
      f.history[1].data.release = f.encode(release);
      if (mutation === "prior-data") f.history[0].data.release = f.encode({ ...f.original, manifest: "changed" });
      if (mutation === "prior-uid") f.history[0].metadata.uid = "replaced-history";
      if (mutation === "absent") f.history.pop();
      if (mutation === "gap") f.history.shift();
      const writes = f.controls.writes;
      await expect(f.recover()).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.recovery).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it.each(["claim", "resource", "storage"])("resumes a lost %s acknowledgement without reusing the failed launch", async boundary => {
    const f = await failedPublicationFixture();
    try {
      let interrupted = false;
      const execute: SchemaExecute = async (tool, args, options) => {
        const patch = tool === "kubectl" && args[0] === "patch" ? JSON.parse(options.input!) : [];
        const recoveryFence = patch.some((op: any) => String(op.value?.[CORE_PUBLICATION_FENCE] ?? "").endsWith(":failed-publication"));
        const recoveryClaim = patch.some((op: any) => op.path === "/metadata/annotations" && op.op === "replace"
          && JSON.parse(op.value?.[CORE_PUBLICATION] ?? "{}").phase === "preparing");
        const result = await f.execute(tool, args, options);
        if (!interrupted && ((boundary === "claim" && recoveryClaim)
          || (recoveryFence && (boundary === "storage" ? args[1] === "secret" : boundary === "resource" && args[1] !== "secret")))) {
          interrupted = true;
          throw new Error("lost recovery acknowledgement");
        }
        return result;
      };
      await expect(f.recover(execute)).rejects.toThrow();
      expect(interrupted).toBe(true);
      expect((await f.current()).publication?.phase).toBe("publishing");
      await f.recover();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("refuses copied receipts once another recovery owns the namespace", async () => {
    const f = await failedPublicationFixture();
    try {
      const copy = join(dirname(f.request.receipt), "copied.jsonl");
      writeFileSync(copy, readFileSync(f.request.receipt), { mode: 0o600 });
      f.controls.fenceFailure = true;
      await expect(f.recover()).rejects.toThrow();
      f.controls.fenceFailure = false;
      const writes = f.controls.writes;
      await expect(publishPrivateCore(f.execute, { ...f.request, receipt: copy, recoverFailedPublication: true })).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      await f.recover();
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "owner", "missing"])("refuses %s resource proof and leaves recovery owned", async mutation => {
    const f = await failedPublicationFixture();
    try {
      if (mutation === "uid") f.policy.metadata.uid = "replaced-policy";
      if (mutation === "owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "another-release";
      if (mutation === "missing") f.objects.splice(f.objects.indexOf(f.policy), 1);
      await expect(f.recover()).rejects.toThrow();
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).failedPublications).toBeUndefined();
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });
});

async function monitoringPublicationFixture(extraDocuments: any[] = []) {
  const f = await publicationFixture(undefined, [{ apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "custom-monitoring-operator", namespace: "observability", labels: { "app.kubernetes.io/part-of": "monitoring" },
  }, spec: { replicas: 1, template: { spec: { containers: [{ name: "operator", image: "operator:retained" }] } } } }, ...extraDocuments]);
  const definitions: Record<string, string> = { "serviceaccount.yaml": "ServiceAccount", "clusterrole.yaml": "ClusterRole",
    "clusterrolebinding.yaml": "ClusterRoleBinding", "role.yaml": "Role", "rolebinding.yaml": "RoleBinding",
    "job-createSecret.yaml": "Job", "job-patchWebhook.yaml": "Job" };
  f.controls.monitoringHooks = Object.entries(definitions).map(([file, kind]) => ({
    name: `custom-${file}`, kind, path: `kars/charts/monitoring/templates/prometheus-operator/admission-webhooks/job-patch/${file}`,
    events: ["pre-install", "pre-upgrade"], delete_policies: ["before-hook-creation", "hook-succeeded"],
    manifest: JSON.stringify({ apiVersion: kind === "Job" ? "batch/v1" : "v1", kind,
      metadata: { name: `custom-${file}`, namespace: "observability" } }),
  }));
  f.controls.config = { monitoring: { enabled: true, fullnameOverride: "custom-monitoring" } };
  f.original.config = structuredClone(f.controls.config);
  f.original.hooks.push(...f.controls.monitoringHooks.map(hook => ({ ...structuredClone(hook), last_run: { phase: "Succeeded" } })));
  f.history[0]!.data.release = f.encode(f.original);
  return f;
}

async function monitoringCapacityFixture() {
  const before = { requests: { cpu: "25m", memory: "32Mi" }, limits: { cpu: "250m", memory: "128Mi" } };
  const resources = { requests: { cpu: "25m", memory: "64Mi" }, limits: { cpu: "500m", memory: "256Mi" } };
  const labels = { "app.kubernetes.io/name": "kube-state-metrics", "app.kubernetes.io/part-of": "monitoring" };
  const f = await monitoringPublicationFixture([{ apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "custom-kube-state-metrics", namespace: "observability", labels,
  }, spec: { replicas: 1, selector: { matchLabels: { "app.kubernetes.io/name": "kube-state-metrics" } }, template: {
    metadata: { labels }, spec: { serviceAccountName: "metrics-account", containers: [{ name: "kube-state-metrics", image: "metrics:retained",
      resources: before, livenessProbe: { httpGet: { path: "/livez", port: 8080 } }, readinessProbe: { httpGet: { path: "/readyz", port: 8081 } },
    }] },
  } } }]);
  f.original.config.monitoring["kube-state-metrics"] = { resources: before };
  f.history[0].data.release = f.encode(f.original);
  f.controls.config.monitoring["kube-state-metrics"] = { resources };
  f.controls.previewMutate = items => {
    items.find(object => object.metadata.name === "custom-kube-state-metrics").spec.template.spec.containers[0].resources = structuredClone(resources);
  };
  return { ...f, resources, metrics: f.objects.find(object => object.metadata.name === "custom-kube-state-metrics")! };
}

async function historicalPublicationFixture() {
  const f = await publicationFixture();
  const hooks = historicalHooks();
  hooks[7] = { ...f.original.hooks[0], kind: "ToolPolicy", path: "kars/templates/toolpolicy-default.yaml" };
  f.original.hooks = hooks;
  const failed = { ...structuredClone(f.original), info: { status: "failed" } };
  failed.hooks[6].last_run.phase = "";
  f.history[0].metadata.labels.status = "failed";
  f.history[0].data.release = f.encode(failed);
  f.original.version = 2;
  f.history.push({ ...structuredClone(f.history[0]), metadata: { ...structuredClone(f.history[0].metadata),
    name: "sh.helm.release.v1.kars.v2", uid: "release-2", labels: { owner: "helm", name: "kars", version: "2", status: "deployed" } },
    data: { release: f.encode(f.original) } });
  const state = { interrupt: "", live: "", freezes: 0 };
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && args[0] === "get" && args.includes("--ignore-not-found") && args[2]?.startsWith("monitoring-admission")) {
      return { stdout: args[1] === state.live ? "{}" : "" };
    }
    if (tool === "kubectl" && args[0] === "get" && args[1] === "pods" && args.includes("monitoring")) {
      return { stdout: JSON.stringify({ metadata: {}, items: state.live === "pod" ? [{ spec: { serviceAccountName: "monitoring-admission" } }] : [] }) };
    }
    if (tool === "kubectl" && args[0] === "patch" && settings.input) {
      const patch = JSON.parse(settings.input), marker = patch.find((op: any) => op.value?.[HISTORICAL_HOOK_RETIREMENT]);
      if (marker && !marker.value[CORE_PUBLICATION_FENCE]) {
        const journal = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
        expect(journal.historicalHookRetirement.phase).toBe("prepared");
        expect(journal.historicalHookRetirement.proofs[0].failed.uid).toBe(f.history[0].metadata.uid);
        expect(journal.publication).toBeUndefined();
        if (state.interrupt === "before") { state.interrupt = ""; throw new Error("before historical freeze"); }
        const live = f.history[0];
        expect(patch.slice(0, 2)).toEqual([{ op: "test", path: "/metadata/uid", value: live.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: live.metadata.resourceVersion }]);
        live.metadata.annotations = marker.value;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        live.immutable = true;
        state.freezes++;
        if (state.interrupt === "after") { state.interrupt = ""; throw new Error("historical freeze acknowledgement lost"); }
        return { stdout: JSON.stringify(live) };
      }
    }
    return f.execute(tool, args, settings);
  };
  return { ...f, state, execute, retireHooks: () => publishPrivateCore(execute, { ...f.request, retireHistoricalMonitoringHooks: true }),
    publish: () => publishPrivateCore(execute, f.request) };
}

describe("owned monitoring capacity publication", () => {
  it.each(["cpu", "memory", "both", "tebibyte", "changed-cpu", "changed-memory", "extra-resource", "invalid-unit"])(
    "compares API-normalized monitoring quantities without accepting capacity drift (%s)", async mode => {
      const f = await monitoringCapacityFixture();
      try {
        f.resources.limits = { cpu: "1000m", memory: mode === "tebibyte" ? "1048576Mi" : "1024Mi" };
        const normalized = structuredClone(f.resources);
        if (mode !== "memory" && mode !== "tebibyte") normalized.limits.cpu = mode === "changed-cpu" ? "2" : "1";
        if (mode !== "cpu") normalized.limits.memory = mode === "changed-memory" ? "2Gi"
          : mode === "tebibyte" ? "1Ti" : mode === "invalid-unit" ? "1G" : "1Gi";
        if (mode === "extra-resource") (normalized.limits as any)["ephemeral-storage"] = "1Gi";
        const execute: SchemaExecute = async (tool, args, settings) => {
          const result = await f.execute(tool, args, settings);
          if (tool === "kubectl" && args[0] === "apply") {
            const projected = JSON.parse(result.stdout);
            if (projected.metadata.uid === f.metrics.metadata.uid) {
              projected.spec.template.spec.containers[0].resources = structuredClone(normalized);
              return { stdout: JSON.stringify(projected) };
            }
          }
          if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
            f.metrics.spec.template.spec.containers[0].resources = structuredClone(normalized);
          }
          return result;
        };
        const result = publishPrivateCore(execute, f.request);
        if (["changed-cpu", "changed-memory", "extra-resource", "invalid-unit"].includes(mode)) {
          await expect(result).rejects.toThrow("must remain semantically unchanged");
          expect(f.controls.launches).toBe(0);
        } else {
          await result;
          expect((await f.current()).publication?.phase).toBe("verified");
          expect(f.metrics.spec.template.spec.containers[0].resources).toEqual(normalized);
          expect(f.controls.launches).toBe(1);
        }
      } finally { f.cleanup(); }
    });

  it.each([false, true])("publishes only configured metrics resources and retains frozen provenance; recovery=%s", async recovery => {
    const f = await monitoringCapacityFixture();
    try {
      const before = structuredClone(f.metrics);
      if (recovery) {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.controls.failedPublication = true;
        await expect(f.publish()).rejects.toThrow("outcome is unknown");
        f.controls.failedPublication = false;
        await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      } else await f.publish();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(recovery ? 2 : 1);
      expect(f.metrics.metadata.uid).toBe(before.metadata.uid);
      before.spec.template.spec.containers[0].resources = f.resources;
      expect(f.metrics.spec).toEqual(before.spec);
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.some(object => object.kind === "Job")).toBe(false);
      expect(frozen.find(object => object.metadata.name === f.metrics.metadata.name)!.spec).toEqual(before.spec);
    } finally { f.cleanup(); }
  });

  it.each(["image", "probe", "account", "replicas", "extra-container", "other-workload", "unrequested-resources",
    "missing-limits", "zero-limit", "extra-resource", "other-config", "wrong-identity", "server-rejection"])(
    "refuses monitoring changes beyond bounded configured capacity: %s", async mode => {
      const f = await monitoringCapacityFixture();
      try {
        const change = f.controls.previewMutate!;
        f.controls.previewMutate = items => {
          change(items);
          const object = items.find(value => value.metadata.name === f.metrics.metadata.name), pod = object.spec.template.spec;
          if (mode === "image") pod.containers[0].image = "metrics:changed";
          if (mode === "probe") pod.containers[0].readinessProbe.httpGet.path = "/";
          if (mode === "account") pod.serviceAccountName = "other";
          if (mode === "replicas") object.spec.replicas = 2;
          if (mode === "extra-container") pod.containers.push({ name: "other", image: "other:latest" });
          if (mode === "other-workload") items.find(value => value.metadata.name === "custom-monitoring-operator").spec.template.spec.containers[0].resources = f.resources;
          if (mode === "unrequested-resources") pod.containers[0].resources.limits.cpu = "1";
        };
        if (mode === "missing-limits") delete (f.resources as any).limits;
        if (mode === "zero-limit") f.resources.limits.memory = "0";
        if (mode === "extra-resource") (f.resources.limits as any)["ephemeral-storage"] = "1Gi";
        if (mode === "other-config") f.controls.config.monitoring["kube-state-metrics"].extraArgs = ["--unreviewed"];
        if (mode === "wrong-identity") f.metrics.metadata.labels["app.kubernetes.io/name"] = "another-metric-service";
        const execute: SchemaExecute = async (tool, args, settings) => {
          if (mode === "server-rejection" && tool === "kubectl" && args[0] === "apply"
            && JSON.parse(settings.input!).metadata.name === f.metrics.metadata.name) throw new Error("API rejected resource quantity");
          return f.execute(tool, args, settings);
        };
        await expect(publishPrivateCore(execute, f.request)).rejects.toThrow();
        expect(f.controls.launches).toBe(0);
        expect(f.metrics.spec.template.spec.containers[0].resources.limits).toEqual({ cpu: "250m", memory: "128Mi" });
      } finally { f.cleanup(); }
    });
});

describe("owned historical monitoring hook retirement", () => {
  it("requires explicit opt-in and then preserves failed bytes through a hook-free publication", async () => {
    const f = await historicalPublicationFixture();
    try {
      const failed = structuredClone(f.history[0]);
      await expect(f.publish()).rejects.toThrow("failed/pending");
      expect(f.controls.writes).toBe(0);
      await f.retireHooks();
      const receipt = await f.current();
      expect(receipt.historicalHookRetirement?.phase).toBe("fenced");
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.history[0].data).toEqual(failed.data);
      expect(f.history[0].metadata.labels.status).toBe("failed");
      expect(f.history[0].metadata.uid).toBe(failed.metadata.uid);
      expect(f.history[0].immutable).toBe(true);
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["before", "after"])("resumes %s freeze interruption without replaying mutation or Helm", async boundary => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = boundary;
      await expect(f.retireHooks()).rejects.toThrow();
      expect((await f.current()).historicalHookRetirement?.phase).toBe("prepared");
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("Resume historical");
      await expect(finalizeRootMigration(f.migrationExecute, await f.current(), f.journal, async () => {})).rejects.toThrow("Resume historical");
      await f.retireHooks();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["Job", "ServiceAccount", "RoleBinding", "pod"])("refuses remaining %s authority without a freeze or launch", async kind => {
    const f = await historicalPublicationFixture();
    try {
      f.state.live = kind;
      await expect(f.retireHooks()).rejects.toThrow();
      expect(f.state.freezes).toBe(0);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it("verifies a persisted fence without importing the previous retirement journal", async () => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = "after";
      await expect(f.retireHooks()).rejects.toThrow();
      await withRootMigrationJournal(f.request.receipt, false, async journal => journal.save(f.retired));
      await f.publish();
      expect((await f.current()).historicalHookRetirement).toBeUndefined();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.state.freezes).toBe(1);
    } finally { f.cleanup(); }
  });

  it("refuses changed successor history after a lost acknowledgement", async () => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = "after";
      await expect(f.retireHooks()).rejects.toThrow();
      f.history[1].metadata.uid = "replacement-successor";
      await expect(f.retireHooks()).rejects.toThrow();
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it("rejects a different requested release before historical storage mutation", async () => {
    const f = await historicalPublicationFixture();
    try {
      await expect(publishPrivateCore(f.execute, { ...f.request, release: "another", retireHistoricalMonitoringHooks: true })).rejects.toThrow("reviewed release");
      expect(f.controls.writes).toBe(0);
      expect(f.state.freezes).toBe(0);
    } finally { f.cleanup(); }
  });
});

describe("owned protected full-Core publication", () => {
  it("resumes retired root readiness against server defaults without relaunching Helm", async () => {
    const f = await publicationFixture();
    try {
      f.controls.previewMutate = items => {
        delete items.find(object => object.kind === "Deployment" && object.metadata.name === f.root.metadata.name)!.spec.progressDeadlineSeconds;
      };
      f.controls.pendingRootReplicaSet = true;
      f.controls.afterSuccessFailure = true;
      await expect(f.publish()).rejects.toThrow();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("succeeded");
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.find(object => object.kind === "Deployment" && object.metadata.name === f.root.metadata.name)!.spec.progressDeadlineSeconds).toBeUndefined();
      expect(f.root.spec.progressDeadlineSeconds).toBe(600);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.controls.readinessCalls.some(args => args.includes(`Deployment/${f.root.metadata.name}`))).toBe(false);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "spec", "replicas"])("rejects retired root %s drift when resuming readiness", async mutation => {
    const f = await publicationFixture();
    try {
      f.controls.afterSuccessFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (mutation === "uid") f.root.metadata.uid = "replaced-root";
      if (mutation === "spec") f.root.spec.progressDeadlineSeconds = 601;
      if (mutation === "replicas") f.root.spec.replicas = 1;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("succeeded");
      expect(f.controls.launches).toBe(1);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });

  it("publishes with a retired Pending root whose new ReplicaSet is denied", async () => {
    const f = await publicationFixture();
    try {
      f.controls.pendingRootReplicaSet = true;
      await f.publish();
      expect(f.root.status.observedGeneration).toBeLessThan(f.root.metadata.generation);
      expect(f.root.spec.replicas).toBe(0);
      expect(f.controls.readinessCalls.some(args => args.includes(`Deployment/${f.root.metadata.name}`))).toBe(false);
      expect((await f.current()).publication?.phase).toBe("verified");
      expect((await f.current()).phase).not.toBe("complete");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each([
    ["Deployment", { replicas: 1 }], ["DaemonSet", {}], ["StatefulSet", { replicas: 1 }],
    ["DaemonSet", { updateStrategy: { type: "OnDelete" } }],
    ["StatefulSet", { updateStrategy: { type: "OnDelete" }, replicas: 1 }],
    ["PersistentVolumeClaim", {}], ["Service", { type: "ClusterIP" }],
    ["Service", { type: "LoadBalancer" }], ["ReplicaSet", { replicas: 1 }],
    ["ReplicationController", { replicas: 1 }],
  ] as const)("retains ownership and resumes %s readiness without replaying Helm (%j)", async (kind, spec) => {
    const f = await publicationFixture(undefined, [{ apiVersion: kind === "Deployment" || kind === "DaemonSet"
      || kind === "StatefulSet" || kind === "ReplicaSet" ? "apps/v1" : "v1", kind,
      metadata: { name: "supporting", namespace: "supporting" }, spec }]);
    try {
      f.controls.readinessFailure = `${kind}/supporting`;
      await expect(f.publish()).rejects.toThrow();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("succeeded");
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
      expect(f.root.spec.replicas).toBe(0);
      expect(f.controls.readinessCalls.some(args => args.includes(`${kind}/supporting`))).toBe(true);
      f.controls.readinessFailure = "";
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      if (kind === "Service" && "type" in spec && spec.type === "LoadBalancer") {
        expect(f.controls.readinessCalls.some(args => args.includes("--for=jsonpath={.status.loadBalancer.ingress}"))).toBe(true);
      }
    } finally { f.cleanup(); }
  });

  it("retains ownership while a published CRD is not Established", async () => {
    const f = await publicationFixture();
    try {
      const crd = f.objects.find(value => value.kind === "CustomResourceDefinition")!;
      f.controls.readinessFailure = `CustomResourceDefinition/${crd.metadata.name}`;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("succeeded");
      expect(f.controls.readinessCalls.some(args => args.includes("--for=condition=Established"))).toBe(true);
      f.controls.readinessFailure = "";
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it("omits completed unchanged admission maintenance without running jobs or changing monitoring", async () => {
    const f = await monitoringPublicationFixture();
    try {
      await f.publish();
      expect(f.controls.launches).toBe(1);
      const receipt = await f.current();
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.some(value => value.kind === "Job")).toBe(false);
      expect(frozen.find(value => value.metadata.name === "custom-monitoring-operator")?.metadata.namespace).toBe("observability");
      expect(receipt.publication!.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it.each(["missing", "duplicate", "unknown", "changed-manifest", "changed-events", "changed-policy", "wrong-kind",
    "failed", "no-predecessor", "extra-predecessor", "changed-values", "missing-provenance", "changed-workload"])(
    "refuses unsafe admission bootstrap omission: %s", async mode => {
      const f = await monitoringPublicationFixture();
      try {
        const hooks = f.controls.monitoringHooks;
        if (mode === "missing") hooks.pop();
        if (mode === "duplicate") hooks[1] = structuredClone(hooks[0]);
        if (mode === "unknown") hooks[0].path += ".unknown";
        if (mode === "changed-manifest") hooks[0].manifest += "\n---\n{}";
        if (mode === "changed-events") hooks[0].events = ["post-delete"];
        if (mode === "changed-policy") hooks[0].delete_policies = [];
        if (mode === "wrong-kind") hooks[0].kind = "Job";
        if (mode === "failed") f.original.hooks[1].last_run.phase = "Failed";
        if (mode === "no-predecessor") f.original.hooks = f.original.hooks.slice(0, 1);
        if (mode === "extra-predecessor") f.original.hooks.push({ ...structuredClone(f.original.hooks[1]), path: `${hooks[0].path}.extra` });
        if (mode === "changed-values") f.controls.config.monitoring.fullnameOverride = "different";
        if (mode === "missing-provenance") f.original.manifest = f.original.manifest.replaceAll("# Source:", "# Removed:");
        if (mode === "changed-workload") f.controls.previewMutate = items => {
          items.find(value => value.metadata.name === "custom-monitoring-operator").spec.template.spec.containers[0].image = "operator:changed";
        };
        f.history[0]!.data.release = f.encode(f.original);
        await expect(f.publish()).rejects.toThrow();
        expect(f.controls.launches).toBe(0);
        expect((await f.current()).publication?.phase).toBe("preparing");
      } finally { f.cleanup(); }
    });

  it.each(["revision-only", "scope", "digest", "stale-revision", "skipped-revision", "other-data"])(
    "allows only the current datapath witness revision transition: %s", async mode => {
      const settings = { schema_version: 1, enabled: true, release_revision: 1, config_digest: "retained", scope: "all", sandboxes: [] };
      const f = await publicationFixture(undefined, [{ apiVersion: "v1", kind: "ConfigMap", metadata: {
        name: "kars-datapath-witness-settings", namespace: "core", labels: { "kars.azure.com/witness-addon": "true" },
      }, data: { "settings.json": JSON.stringify(settings) } }]);
      try {
        f.controls.previewMutate = items => {
          const object = items.find(value => value.metadata.name === "kars-datapath-witness-settings");
          const after = { ...settings, release_revision: 2 };
          if (mode === "scope") after.scope = "selected";
          if (mode === "digest") after.config_digest = "changed";
          if (mode === "stale-revision") after.release_revision = 1;
          if (mode === "skipped-revision") after.release_revision = 3;
          if (mode === "other-data") object.data.unreviewed = "changed";
          object.data["settings.json"] = JSON.stringify(after);
        };
        if (mode === "revision-only") {
          await f.publish();
          expect((await f.current()).publication?.phase).toBe("verified");
        } else {
          await expect(f.publish()).rejects.toThrow(/semantically unchanged/);
          expect(f.controls.launches).toBe(0);
        }
      } finally { f.cleanup(); }
    });

  it.each(["--replace-preparing-request", "--recover-failed-publication", "--retire-historical-monitoring-hooks"])(
    "exposes %s only as an explicit publication option", name => {
    const command = credentialGrantsCommand().commands.find(value => value.name() === "migrate-root-image")
      ?.commands.find(value => value.name() === "publish-core");
    const option = command?.options.find(value => value.long === name);
    expect(option).toBeDefined();
    expect(option?.defaultValue).toBeUndefined();
  });

  it("publishes Core while retaining the enrolled Bridge companion and then restores both", async () => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      const before = structuredClone(f.companion);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.companion).toEqual(before);
      expect(f.companion.spec.replicas).toBe(0);
      const run: Execute = async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout;
      await finalizeRootMigration(run, await f.current(), f.journal, applyReviewedGrant);
      expect(f.companion.spec.replicas).toBe(2);
      expect(f.root.spec.replicas).toBe(2);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    } finally { f.cleanup(); }
  });

  it.each(["csa", undefined, null, "unknown", "wrong-field"])("rejects predecessor apply method %s before any publication mutation", async method => {
    const f = await publicationFixture();
    try {
      f.original.apply_method = method === "wrong-field" ? undefined : method;
      if (method === "wrong-field") f.original.applyMethod = "ssa";
      f.history[0].data.release = f.encode(f.original);
      f.root.metadata.managedFields = [{ manager: "helm", operation: "Update", apiVersion: "apps/v1",
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:replicas": {} } } }];
      const before = structuredClone([f.root, f.policy, f.secret, f.history]);
      await expect(f.publish()).rejects.toThrow(/SSA predecessor/);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeUndefined();
      expect((await f.current()).publication).toBeUndefined();
      expect([f.root, f.policy, f.secret, f.history]).toEqual(before);
    } finally { f.cleanup(); }
  });

  it.each(["adoptionBeforePatchFailure", "adoptionAckFailure", "adoptionAfterSaveFailure", "adoptionBadAck"] as const)("recovers journaled static policy %s before freezing or launching", async failure => {
    const f = await publicationFixture();
    try {
      const policy = structuredClone(f.policy.spec), history = structuredClone(f.history);
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("preparing");
      expect(f.controls.launches).toBe(0);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.adoptions).toBe(1);
      expect(f.controls.liveOwnershipChecks).toBe(1);
      expect(f.policy.metadata.uid).toBe("tool-policy");
      expect(f.policy.spec).toEqual(policy);
      expect(f.history[0].metadata.uid).toBe(history[0].metadata.uid);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "body", "owner", "version"])("refuses changed %s when resuming static policy adoption", async change => {
    const f = await publicationFixture();
    try {
      f.controls.adoptionBeforePatchFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (change === "uid") f.policy.metadata.uid = "replacement-policy";
      if (change === "body") f.policy.spec = { changed: true };
      if (change === "owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
      if (change === "version") f.policy.metadata.resourceVersion = "999";
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.adoptions).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("preparing");
    } finally { f.cleanup(); }
  });

  it("keeps SSA predecessors eligible with Helm-owned Update fields without running CSA conversion", async () => {
    const f = await publicationFixture();
    try {
      f.root.metadata.managedFields = [{ manager: "helm", operation: "Update", apiVersion: "apps/v1",
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:replicas": {} } } }];
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("rechecks the stored apply method on prepared resume before any new mutation", async () => {
    const f = await publicationFixture();
    try {
      f.controls.preparedInterruption = true;
      await expect(f.publish()).rejects.toThrow();
      f.original.apply_method = "csa";
      f.history[0].data.release = f.encode(f.original);
      f.controls.writes = 0;
      await expect(f.publish()).rejects.toThrow(/SSA predecessor/);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("prepared");
    } finally { f.cleanup(); }
  });

  it.each(["duplicate", "missing", "resource-version", "body", "namespace"])("rejects a %s published inventory without weakening exact object binding", async mutation => {
    const f = await publicationFixture();
    try {
      f.controls.manifestMutation = mutation;
      await expect(f.publish()).rejects.toThrow("Published Helm manifest differs");
      expect((await f.current()).publication?.phase).toBe("publishing");
      expect(f.controls.fences).toBe(0);
      await expect(f.publish()).rejects.toThrow("unknown");
    } finally { f.cleanup(); }
  });

  it("accepts real Helm sorting of the maintained admission policies, static ToolPolicy and CRDs", async () => {
    const directory = `.core-sorting-render-${randomUUID()}`;
    mkdirSync(join(directory, "templates"), { recursive: true, mode: 0o700 });
    mkdirSync(join(directory, "files"), { mode: 0o700 });
    try {
      expect(execFileSync("helm", ["version", "--template", "{{.Version}}"], { encoding: "utf8" }).trim()).toMatch(/^v4\.1\.3(?:\+|$)/);
      writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: kars\nversion: 0.1.0\n");
      for (const file of ["toolpolicy-default.yaml", "admission-no-public-router-exposure.yaml"]) {
        copyFileSync(new URL(`../../../deploy/helm/kars/templates/${file}`, import.meta.url), join(directory, "templates", file));
      }
      copyFileSync(new URL("../../../deploy/helm/kars/files/kars-default-agt-profile.yaml", import.meta.url), join(directory, "files/kars-default-agt-profile.yaml"));
      const documents = schemaDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "core"],
        { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] }));
      const policy = documents.find(value => value.kind === "ToolPolicy")!;
      const admission = documents.filter(value => value.kind.startsWith("ValidatingAdmissionPolicy"));
      expect(admission.map(value => value.kind)).toEqual(["ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"]);
      const f = await publicationFixture(policy, admission);
      try {
        Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
        f.controls.realHelmRender = true;
        await f.publish();
        expect(f.controls.liveOwnershipChecks).toBe(1);
        expect(f.controls.adoptions).toBe(0);
        expect((await f.current()).publication?.phase).toBe("verified");
      } finally { f.cleanup(); }
    } finally { rmSync(directory, { recursive: true, force: true }); }
  });

  it("publishes the maintained chart's default ToolPolicy without running its delete/recreate hook lifecycle", async () => {
    const directory = `.core-policy-render-${randomUUID()}`;
    mkdirSync(join(directory, "templates"), { recursive: true, mode: 0o700 });
    mkdirSync(join(directory, "files"), { mode: 0o700 });
    try {
      writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: kars\nversion: 0.1.0\n");
      copyFileSync(new URL("../../../deploy/helm/kars/templates/toolpolicy-default.yaml", import.meta.url), join(directory, "templates/toolpolicy-default.yaml"));
      copyFileSync(new URL("../../../deploy/helm/kars/files/kars-default-agt-profile.yaml", import.meta.url), join(directory, "files/kars-default-agt-profile.yaml"));
      const [policy] = schemaDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "core"],
        { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] }));
      policy!.spec.agtProfile.inline = policy!.spec.agtProfile.inline.replace(/\n$/, "");
      const f = await publicationFixture(policy);
      try {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.policy.metadata.finalizers = ["kars.azure.com/toolpolicy-cleanup"];
        f.policy.spec.agtProfile.inline += "\n";
        const liveSpec = structuredClone(f.policy.spec);
        await f.publish();
        expect(f.policy.metadata.uid).toBe("tool-policy");
        expect(f.policy.spec).toEqual(liveSpec);
        expect(f.policy.metadata.finalizers).toEqual(["kars.azure.com/toolpolicy-cleanup"]);
        const frozen = schemaDocuments(readFileSync(join((await f.current()).publication!.artifact!, "files/publication.yaml"), "utf8"))
          .find(value => value.kind === "ToolPolicy")!;
        expect(frozen.spec).toEqual(liveSpec);
        expect(frozen.metadata.finalizers).toEqual(f.policy.metadata.finalizers);
        expect(f.policy.metadata.annotations["helm.sh/hook"]).toBeUndefined();
        expect(f.policy.metadata.annotations["meta.helm.sh/release-name"]).toBe("kars");
        expect((await f.current()).publication?.phase).toBe("verified");
      } finally { f.cleanup(); }
    } finally { rmSync(directory, { recursive: true, force: true }); }
  });

  it.each(["unknown-finalizer", "extra-finalizer", "policy-change", "extra-newlines", "scalar-newline", "invalid-yaml", "multiple-documents", "changed-label"] as const)(
    "refuses static policy compatibility with %s before adoption", async failure => {
      const f = await publicationFixture();
      try {
        const inline = failure === "scalar-newline" ? "description: |+\n  retained\n"
          : failure === "invalid-yaml" ? "rules: ["
            : failure === "multiple-documents" ? "rules: []\n---\nrules: []" : "rules: []";
        const rendered = JSON.parse(f.original.hooks[0].manifest);
        rendered.spec.agtProfile = { inline };
        f.original.hooks[0].manifest = JSON.stringify(rendered);
        f.history[0].data.release = f.encode(f.original);
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.policy.spec = structuredClone(rendered.spec);
        f.policy.spec.agtProfile.inline += failure === "extra-newlines" ? "\n\n" : "\n";
        f.policy.metadata.finalizers = ["kars.azure.com/toolpolicy-cleanup"];
        if (failure === "unknown-finalizer") f.policy.metadata.finalizers = ["another.example/cleanup"];
        if (failure === "extra-finalizer") f.policy.metadata.finalizers.push("another.example/cleanup");
        if (failure === "policy-change") f.policy.spec.allowed = "changed";
        if (failure === "changed-label") f.policy.metadata.labels.changed = "true";
        const before = structuredClone(f.policy);
        await expect(f.publish()).rejects.toThrow("unchanged live predecessor");
        expect(f.controls.adoptions).toBe(0);
        expect(f.controls.launches).toBe(0);
        expect(f.policy).toEqual(before);
      } finally { f.cleanup(); }
    });

  it.each(["changed-policy", "foreign-owner", "missing-history"] as const)("refuses static hook conversion with %s", async failure => {
    const f = await publicationFixture();
    try {
      if (failure === "changed-policy") f.policy.spec = { allowed: "unreviewed-policy" };
      else if (failure === "foreign-owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "someone-else";
      else f.history[0].data.release = Buffer.from(gzipSync(Buffer.from(JSON.stringify({ name: "kars", namespace: "core", version: 1,
        info: { status: "deployed" }, manifest: "", hooks: [] }))).toString("base64")).toString("base64");
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each([false, true])("publishes through retirement and requalifies the retained grant (runner-only: %s)", async runnerOnly => {
    const f = await publicationFixture(undefined, [], runnerOnly);
    try {
      await f.publish();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.controls.fences).toBe(8);
      expect(f.history.every(value => value.immutable === true)).toBe(true);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeUndefined();
      expect(receipt.retirement).toEqual(f.retired.retirement);
      expect(f.history.map(value => value.metadata.uid)).toEqual(["release-1", "release-2"]);
      const completed = await finalizeRootMigration(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        receipt, f.journal, applyReviewedGrant);
      expect(completed.phase).toBe("complete");
      expect(f.grant("core").metadata.uid).toBe(f.grantBefore.metadata.uid);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      expect(f.ca.data["contracts.json"]).toBe('{"unchanged":"accounts-charges-caps"}');
      expect(f.ca.metadata.uid).toBe("contracts-uid");
      const originalMetadata = receipt.plan.original.metadata;
      if (originalMetadata === null || typeof originalMetadata !== "object" || Array.isArray(originalMetadata)) {
        throw new Error("Expected original root metadata to be a JSON object");
      }
      expect(f.root.metadata.uid).toBe(originalMetadata.uid);
      expect(f.root.spec.replicas).toBe(2);
      expect(f.root.spec.template.spec.containers[0].image).toBe(runnerOnly ? ORIGINAL : TARGET);
      if (runnerOnly) expect(f.root.spec.template.spec.containers[0].env.find(
        (entry: any) => entry.name === "KARS_CONFORMANCE_RUNNER_IMAGE",
      ).value).toBe(TARGET);
      expect(templateDigest(f.root)).toBe(receipt.plan.review.target.templateDigest);
      expect(f.grant("core").spec.privateActivation.namespaces[0].epoch)
        .not.toBe(f.grantBefore.spec.privateActivation.namespaces[0].epoch);
      await validateQualifiedActivation(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        f.grant("core").spec.privateActivation);
    } finally { f.cleanup(); }
  });

  it("resolves omitted resource namespaces against the release, not the operator's default namespace", async () => {
    const f = await publicationFixture();
    try {
      f.controls.omittedNamespace = true;
      await f.publish();
      const receipt = await f.current();
      const objects = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(objects.find(object => object.kind === "Deployment")!.metadata.namespace).toBe("core");
      expect(receipt.publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("preserves sanitized Helm 4 release-list fallback with an enrolled companion", async () => {
    const f = await publicationFixture(undefined, [], false, 1);
    try {
      const marker = "PRIVATE-KEY-MUST-NOT-BE-DISCLOSED";
      let fallbacks = 0;
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "list" && args.includes("--all")) {
          fallbacks += 1;
          throw Object.assign(new Error(marker), { stderr: "Error: unknown flag: --all\n", stdout: marker, exitCode: 1 });
        }
        return f.execute(file, args, options);
      };
      await publishPrivateCore(execute, f.request);
      expect(fallbacks).toBeGreaterThan(0);
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.companion.spec.replicas).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain(marker);
    } finally { f.cleanup(); }
  });

  it.each([
    { stderr: "Error: unknown flag: --all\nPRIVATE-KEY-MUST-NOT-BE-DISCLOSED", exitCode: 1 },
    { stderr: "Error: unknown flag: --all", exitCode: 2 },
  ])("withholds unexpected Helm inventory errors instead of treating them as syntax fallback: %j", async error => {
    const f = await publicationFixture();
    try {
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "list") throw Object.assign(new Error("PRIVATE-KEY-MUST-NOT-BE-DISCLOSED"), error);
        return f.execute(file, args, options);
      };
      await expect(publishPrivateCore(execute, f.request)).rejects.toThrow("output is withheld");
      expect(f.controls.launches).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain("PRIVATE-KEY-MUST-NOT-BE-DISCLOSED");
    } finally { f.cleanup(); }
  });

  it("does not disclose credentials in failed Helm commands or render output", async () => {
    const f = await publicationFixture();
    try {
      const marker = "PRIVATE-KEY-MUST-NOT-BE-DISCLOSED";
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "template") throw Object.assign(new Error(`helm --set key=${marker}`), { stderr: marker, exitCode: 1 });
        return f.execute(file, args, options);
      };
      await expect(publishPrivateCore(execute, f.request)).rejects.toThrow("output is withheld");
      expect(f.controls.launches).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain(marker);
    } finally { f.cleanup(); }
  });

  it("retains previously immutable deployed history on the next protected publication", async () => {
    const f = await publicationFixture();
    try {
      f.history[0].immutable = true;
      const retained = structuredClone(f.history[0].data);
      await f.publish();
      expect(f.history[0].data).toEqual(retained);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "deployed"]);
      expect(f.history.every(value => value.immutable)).toBe(true);
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it.each(["preflightFailure", "preparedInterruption", "afterSuccessFailure", "lostReleaseAck"] as const)("resumes %s without duplicating a Helm launch", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe(failure === "preflightFailure" ? "preparing" : failure === "preparedInterruption" ? "prepared" : "succeeded");
      expect(f.controls.launches).toBe(failure === "preflightFailure" || failure === "preparedInterruption" ? 0 : 1);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it("explicitly replaces only a preparing request, preserving its attempt, ownership and original journal evidence", async () => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      f.controls.preflightFailure = true;
      await expect(f.publish()).rejects.toThrow();
      const before = await f.current(), evidence = readFileSync(f.request.receipt, "utf8");
      const namespace = structuredClone(f.namespace("core")), companion = structuredClone(f.companion);
      const replacement = { ...f.request, chart: "charts/repaired-immutable-core" };
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, replacement)).rejects.toThrow("original Core publication request");
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      f.controls.preflightFailure = true;
      await expect(publishPrivateCore(f.execute, { ...replacement, replacePreparingRequest: true })).rejects.toThrow();
      const recovered = await f.current(), updated = readFileSync(f.request.receipt, "utf8");
      expect(updated.startsWith(evidence)).toBe(true);
      expect(recovered.publication!.requestDigest).not.toBe(before.publication!.requestDigest);
      expect(recovered).toEqual({ ...before, publication: { ...before.publication, requestDigest: recovered.publication!.requestDigest } });
      expect(JSON.parse(updated.slice(evidence.length).split("\n")[0]!).receipt).toEqual(recovered);
      expect(f.namespace("core")).toEqual(namespace);
      expect(f.companion).toEqual(companion);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("original Core publication request");
      await publishPrivateCore(f.execute, replacement);
      expect((await f.current()).publication).toMatchObject({ phase: "verified", attempt: before.publication!.attempt,
        requestDigest: recovered.publication!.requestDigest });
      expect(f.controls.launches).toBe(1);
      expect(f.companion).toEqual(companion);
    } finally { f.cleanup(); }
  });

  it.each([0, 1])("replaces a preparing request while preserving fenced witness replica intent %s", async replicas => {
    const f = await witnessPublicationFixture(replicas);
    try {
      Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
      f.state.evidenceAckLost = true;
      await expect(f.publish()).rejects.toThrow();
      const before = await f.current(), evidence = readFileSync(f.request.receipt, "utf8");
      expect(before.publication?.phase).toBe("preparing");
      expect(before.publication?.witnessWriter?.evidenceFence).toBeDefined();
      const writer = structuredClone(f.writer), config = structuredClone(f.evidence), scales = [...f.state.writes];
      const replacement = { ...f.request, values: ["repaired-model-values.yaml"] };
      f.controls.writes = 0;
      f.controls.preflightFailure = true;
      await expect(publishPrivateCore(f.execute, { ...replacement, replacePreparingRequest: true })).rejects.toThrow("Command output is withheld");
      const replaced = await f.current();
      expect(replaced.publication!.requestDigest).not.toBe(before.publication!.requestDigest);
      expect(replaced).toEqual({ ...before, publication: { ...before.publication, requestDigest: replaced.publication!.requestDigest } });
      expect(readFileSync(f.request.receipt, "utf8").startsWith(evidence)).toBe(true);
      expect(f.writer).toEqual(writer);
      expect(f.evidence).toEqual(config);
      expect(f.state.writes).toEqual(scales);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("original Core publication request");
      await publishPrivateCore(f.execute, replacement);
      expect((await f.current()).publication).toMatchObject({ phase: "verified", attempt: before.publication!.attempt,
        witnessWriter: { restored: true, deployment: before.publication!.witnessWriter!.deployment } });
      expect(f.writer.spec.replicas).toBe(replicas);
      expect(f.evidence.data).toEqual(config.data);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["writer-uid", "writer-spec", "writer-replicas", "writer-owner", "writer-pods", "writer-hpa",
    "evidence-uid", "evidence-fence", "evidence-version", "evidence-owner", "missing-fence", "publishedSpec", "restored", "future-writer"] as const)(
    "refuses preparing request replacement with %s witness drift before journaling", async fault => {
      const f = await witnessPublicationFixture();
      try {
        Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
        f.state.evidenceAckLost = true;
        await expect(f.publish()).rejects.toThrow();
        const before = await f.current();
        if (fault === "writer-uid") f.writer.metadata.uid = "replacement";
        if (fault === "writer-spec") f.writer.spec.template.spec.containers[0].image = "changed";
        if (fault === "writer-replicas") f.writer.spec.replicas = 1;
        if (fault === "writer-owner") f.writer.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
        if (fault === "evidence-uid") f.evidence.metadata.uid = "replacement";
        if (fault === "evidence-fence") f.evidence.metadata.annotations[CORE_PUBLICATION_FENCE] = "foreign:witness-paused";
        if (fault === "evidence-version") f.evidence.metadata.resourceVersion = before.publication!.witnessWriter!.evidenceFence!.resourceVersion;
        if (fault === "evidence-owner") f.evidence.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
        if (["missing-fence", "publishedSpec", "restored", "future-writer"].includes(fault)) {
          await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
            if (fault === "missing-fence") delete receipt!.publication!.witnessWriter!.evidenceFence;
            else Object.assign(receipt!.publication!.witnessWriter!, { [fault]: null });
            await journal.save(receipt!);
          });
        }
        const execute: SchemaExecute = async (tool, args, settings) => {
          if (tool === "kubectl" && args[0] === "get" && args.includes("custom-witness")) {
            if (fault === "writer-pods" && args[1] === "pods") return { stdout: JSON.stringify({ metadata: {}, items: [{ metadata: { name: "still-running" } }] }) };
            if (fault === "writer-hpa" && args[1] === "horizontalpodautoscalers") return { stdout: JSON.stringify({ metadata: {}, items: [{ spec: { scaleTargetRef: { kind: "Deployment", name: "aggregator" } } }] }) };
          }
          return f.execute(tool, args, settings);
        };
        const evidence = readFileSync(f.request.receipt, "utf8"), scales = [...f.state.writes];
        f.controls.writes = 0;
        await expect(publishPrivateCore(execute, { ...f.request, values: ["repaired-model-values.yaml"], replacePreparingRequest: true })).rejects.toThrow();
        expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
        expect(f.state.writes).toEqual(scales);
        expect(f.controls.writes).toBe(0);
        expect(f.controls.launches).toBe(0);
      } finally { f.cleanup(); }
    });

  it.each(["first", "unchanged", "context", "release", "namespace", "rotation", "storage-missing",
    "artifact", "digest", "resources", "revision", "adoption", "future-outcome"] as const)("refuses preparing request replacement with %s before journaling", async fault => {
    const f = await publicationFixture();
    try {
      if (fault !== "first") {
        f.controls.preflightFailure = true;
        await expect(f.publish()).rejects.toThrow();
      }
      const request = { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true };
      if (fault === "unchanged") request.chart = f.request.chart;
      if (["context", "release", "namespace"].includes(fault)) Object.assign(request, { [fault]: "other" });
      if (["rotation", "storage-missing", "artifact", "digest", "resources", "revision", "adoption", "future-outcome"].includes(fault)) {
        await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
          if (fault === "rotation") receipt!.phase = "complete";
          else if (fault === "storage-missing") delete receipt!.publication!.storage;
          else Object.assign(receipt!.publication!, { [fault]: fault === "resources" ? [] : null });
          await journal.save(receipt!);
        });
      }
      const evidence = readFileSync(f.request.receipt, "utf8");
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, request)).rejects.toThrow("Request replacement requires");
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["prepared", "publishing", "succeeded", "verified"] as const)("refuses preparing request replacement after %s", async phase => {
    const f = await publicationFixture();
    try {
      if (phase === "prepared") f.controls.preparedInterruption = true;
      if (phase === "publishing") f.controls.launchFailure = true;
      if (phase === "succeeded") f.controls.afterSuccessFailure = true;
      if (phase === "verified") await f.publish();
      else await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe(phase);
      const evidence = readFileSync(f.request.receipt, "utf8"), launches = f.controls.launches;
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true })).rejects.toThrow();
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(launches);
    } finally { f.cleanup(); }
  });

  it.each(["storage-uid", "storage-content", "storage-status", "recorded-marker", "missing-marker", "foreign-marker", "launch-marker",
    "root", "companion", "root-pod", "ca", "unrotated", "writers"] as const)("refuses preparing request replacement with stale %s fencing", async fault => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      f.controls.preflightFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (fault === "storage-uid") f.history[0].metadata.uid = "replaced";
      if (fault === "storage-content") f.history[0].data.release = f.encode({ ...f.original, config: { changed: true } });
      if (fault === "storage-status") f.controls.failedHistory = true;
      if (fault === "recorded-marker") await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
        delete (receipt!.namespace!.metadata as any).annotations[CORE_PUBLICATION];
        await journal.save(receipt!);
      });
      if (fault === "missing-marker") delete f.namespace("core").metadata.annotations[CORE_PUBLICATION];
      if (fault === "foreign-marker" || fault === "launch-marker") {
        const marker = JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]);
        if (fault === "foreign-marker") marker.attempt = "a".repeat(64);
        else marker.phase = "publishing";
        f.namespace("core").metadata.annotations[CORE_PUBLICATION] = canonical(marker);
      }
      if (fault === "root") f.root.spec.replicas = 1;
      if (fault === "companion") f.companion.spec.replicas = 1;
      if (fault === "root-pod") f.pods.set("core", [f.rootPod()]);
      if (fault === "ca") f.ca.data["ca.crt"] = rootCertificates[3]!;
      if (fault === "unrotated") f.rotate(1);
      if (fault === "writers") f.grant("core").spec.writers = f.grantBefore.spec.writers;
      const evidence = readFileSync(f.request.receipt, "utf8"), namespace = structuredClone(f.namespace("core"));
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true })).rejects.toThrow();
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.namespace("core")).toEqual(namespace);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["extra-template", "changed-content", "stale-version", "changed-context"] as const)("rejects %s on prepared resume before launching", async change => {
    const f = await publicationFixture();
    try {
      f.controls.preparedInterruption = true;
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      const receipt = await f.current(), artifact = receipt.publication!.artifact!;
      if (change === "extra-template") {
        chmodSync(join(artifact, "templates"), 0o700);
        writeFileSync(join(artifact, "templates", "unreviewed.yaml"), "kind: Secret", { mode: 0o400 });
        chmodSync(join(artifact, "templates"), 0o500);
      } else if (change === "changed-content") {
        chmodSync(join(artifact, "values.yaml"), 0o600);
        writeFileSync(join(artifact, "values.yaml"), '{"changed":true}');
        chmodSync(join(artifact, "values.yaml"), 0o400);
      } else if (change === "stale-version") f.root.metadata.resourceVersion = "999999";
      else f.request.context = "kind-other";
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("prepared");
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });

  it.each(["preparing", "prepared"] as const)("prevents a copied %s receipt from sharing the launch ownership", async phase => {
    const f = await publicationFixture();
    try {
      f.controls[phase === "preparing" ? "preflightFailure" : "preparedInterruption"] = true;
      await expect(f.publish()).rejects.toThrow();
      const copy = `${f.request.receipt}.copy`;
      copyFileSync(f.request.receipt, copy);
      let blocked = false;
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          expect(JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).phase).toBe("publishing");
          await expect(publishPrivateCore(f.execute, { ...f.request, receipt: copy })).rejects.toThrow();
          blocked = true;
        }
        return f.execute(file, args, options);
      };
      await publishPrivateCore(execute, f.request);
      expect(blocked).toBe(true);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("retains ambiguous launch ownership without starting Helm after a lost claim acknowledgement", async () => {
    const f = await publicationFixture();
    try {
      f.controls.claimAckFailure = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("publishing");
      expect(JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).phase).toBe("publishing");
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("unknown");
    } finally { f.cleanup(); }
  });

  it.each(["launchFailure", "fenceFailure", "immutableAckFailure"] as const)("refuses ambiguous %s including copied pre-publication qualification", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("publishing");
      await expect(f.publish()).rejects.toThrow("unknown");
      await expect(finalizeRootMigration(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        f.retired, f.journal, applyReviewedGrant)).rejects.toThrow(/publication/i);
      expect(f.controls.launches).toBe(1);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
      expect(f.root.spec.replicas).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["hooks", "changedTls", "failedHistory"] as const)("refuses %s before publication", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe(failure === "failedHistory" ? undefined : "preparing");
    } finally { f.cleanup(); }
  });
});
