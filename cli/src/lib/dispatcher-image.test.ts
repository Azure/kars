// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { applyDispatcherImage, reviewDispatcherImage, verifyDispatcherImage, type DispatcherImageReceipt } from "./dispatcher-image.js";
import { canonical, digest, PRIVATE_PREFIX as P, type Execute } from "./private-activation.js";
import { privateCommandFailure } from "./private-activation-command-diagnostics.js";

const NAME = "kars-mission-dispatcher", NS = "kars-system";
const OLD = `localhost:5077/dispatcher:latest@sha256:${"1".repeat(64)}`;
const NEW = `localhost:5077/dispatcher:latest@sha256:${"2".repeat(64)}`;
const metadata = (name: string, uid = name, namespace?: string) => ({ name, uid, resourceVersion: "1", ...(namespace ? { namespace } : {}) });
function fixture() {
  const deployment: any = {
    apiVersion: "apps/v1", kind: "Deployment",
    metadata: { ...metadata(NAME, "dispatcher-uid", NS), generation: 1,
      managedFields: [{ manager: "helm", operation: "Update", apiVersion: "apps/v1", fieldsType: "FieldsV1",
        fieldsV1: { "f:spec": { "f:template": { "f:spec": { "f:containers": {} } } } } }],
      labels: { "app.kubernetes.io/managed-by": "Helm", "app.kubernetes.io/component": "mission-dispatcher" },
      annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": NS, "kars.azure.com/mission-root-uid": "independent-root" } },
    spec: { replicas: 1, strategy: { type: "Recreate" }, progressDeadlineSeconds: 600,
      selector: { matchLabels: { app: NAME } },
      template: { metadata: { labels: { app: NAME } }, spec: { serviceAccountName: NAME,
        containers: [{ name: "mission-dispatcher", image: OLD, imagePullPolicy: "Always",
          env: [ { name: "KARS_MISSION_DISPATCHER_DEPLOYMENT", value: NAME },
            { name: "KARS_MISSION_DISPATCHER_RELEASE", value: "kars" },
            { name: "KARS_MISSION_IDENTITY_ROOT", valueFrom: { secretKeyRef: { name: `${NAME}-identity`, key: "root", optional: false } } } ],
          securityContext: { readOnlyRootFilesystem: true }, resources: { limits: { memory: "512Mi" } },
          readinessProbe: { httpGet: { path: "/readyz", port: 8080 } } }],
        volumes: [{ name: "work", emptyDir: {} }], terminationGracePeriodSeconds: 35 } } },
    status: { observedGeneration: 1, replicas: 1, updatedReplicas: 1, readyReplicas: 1, availableReplicas: 1 },
  };
  const namespace: any = { kind: "Namespace", metadata: { ...metadata(NS), annotations: {
    [`${P}state`]: "Qualified", [`${P}parent-controller-uid`]: "epoch", [`${P}root-deployment-uid`]: "controller-uid",
    [`${P}root-retirement`]: JSON.stringify({ activation: { root: { deployment: { uid: "controller-uid" } } } }),
  } } };
  const root: any = { apiVersion: "apps/v1", kind: "Deployment", metadata: metadata("kars-controller", "controller-uid", NS),
    spec: { template: { metadata: {}, spec: { containers: [{ name: "controller", image: OLD }] } } } };
  const cluster: any = { kind: "Namespace", metadata: metadata("kube-system", "cluster-uid") };
  const account: any = { metadata: metadata(NAME, "account-uid", NS) };
  const secret: any = metadata(`${NAME}-identity`, "identity-secret-uid", NS);
  const grants: any[] = [{ metadata: metadata("workspace", "grant-uid", NS), spec: { writers: [], controller: { tokenAllowance: "unbounded" } } }];
  const records: DispatcherImageReceipt[] = [], calls: string[][] = [];
  let patchHook: (() => void) | undefined, rolloutHook: (() => void) | undefined;
  let loseAck = false, incomplete = false;
  const execute: Execute = async (args, input) => {
    calls.push(args);
    if (args[0] === "get") {
      if (args[1] === "deployment") {
        const printed = structuredClone(deployment);
        if (!args.includes("--show-managed-fields=true")) delete printed.metadata.managedFields;
        return JSON.stringify(printed);
      }
      if (args[1] === "serviceaccount") return JSON.stringify(account);
      if (args[1] === "secret") {
        expect(args.at(-1)).toBe("jsonpath-as-json={.metadata}");
        return JSON.stringify([secret]);
      }
      const items = args[1] === "namespaces" ? [namespace, cluster]
        : args[1] === "karscredentialgrants.kars.azure.com" ? grants : [root, deployment];
      return JSON.stringify({ metadata: incomplete ? { continue: "next-page" } : {}, items });
    }
    if (args[0] === "patch") {
      patchHook?.();
      const patch = JSON.parse(input!);
      expect(patch.map((p: any) => p.op)).toEqual(["test", "test", "test", "test", "replace"]);
      expect(patch.at(-1).path).toBe("/spec/template/spec/containers/0/image");
      expect(records.at(-1)?.phase).toBe("attempting");
      for (const test of patch.filter((p: any) => p.op === "test")) {
        const actual = test.path.split("/").slice(1).reduce((v: any, key: string) => v[key], deployment);
        if (canonical(actual) !== canonical(test.value)) throw new Error("CAS conflict");
      }
      deployment.spec.template.spec.containers[0].image = patch.at(-1).value;
      deployment.metadata.resourceVersion = "2";
      deployment.metadata.generation++;
      if (loseAck) throw new Error("lost acknowledgement");
      return JSON.stringify(deployment);
    }
    if (args[0] === "rollout") {
      rolloutHook?.();
      deployment.status.observedGeneration = deployment.metadata.generation;
      return "successfully rolled out";
    }
    throw new Error(`Unexpected command ${args.join(" ")}`);
  };
  return { deployment, namespace, root, cluster, account, secret, grants, records, calls, execute,
    journal: { save: async (receipt: DispatcherImageReceipt) => { records.push(structuredClone(receipt)); } },
    review: () => reviewDispatcherImage(execute, "kind-beta", NS, "kars", NEW),
    setPatchHook: (hook: () => void) => { patchHook = hook; },
    setRolloutHook: (hook: () => void) => { rolloutHook = hook; },
    loseAck: () => { loseAck = true; }, incomplete: () => { incomplete = true; },
  };
}

describe("non-private dispatcher image operator", () => {
  it("performs read-only review then only the exact image CAS, preserving all other state", async () => {
    const f = fixture(), before = structuredClone(f.deployment);
    const privateBefore = structuredClone([f.namespace, f.root, f.account, f.secret, f.grants]);
    const receipt = await f.review();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(receipt.review.targetSpec.template).not.toEqual(before.spec.template);
    const result = await applyDispatcherImage(f.execute, receipt, f.journal);
    expect(result.phase).toBe("verified");
    before.spec.template.spec.containers[0].image = NEW;
    expect(f.deployment.spec).toEqual(before.spec);
    expect([f.namespace, f.root, f.account, f.secret, f.grants]).toEqual(privateBefore);
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "applied", "verified"]);
    expect(f.calls.filter(args => !["get", "rollout"].includes(args[0]!)).map(args => args[0])).toEqual(["patch"]);
    await expect(applyDispatcherImage(f.execute, result, f.journal)).rejects.toThrow("consumed");
  });

  it("retains server managedFields for the complete metadata CAS despite default kubectl filtering", async () => {
    const f = fixture();
    const printed = JSON.parse(await f.execute(["get", "deployment", NAME, "-n", NS, "-o", "json"]));
    expect(printed.metadata).not.toHaveProperty("managedFields");
    const receipt = await f.review();
    expect(receipt.review.original.metadata).toEqual(f.deployment.metadata);
    expect((await applyDispatcherImage(f.execute, receipt, f.journal)).phase).toBe("verified");
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "deployment").slice(1)
      .every(args => args.includes("--show-managed-fields=true"))).toBe(true);
  });

  it("refuses an older filtered review before patch rather than silently weakening or upgrading its CAS", async () => {
    const f = fixture(), receipt = await f.review();
    delete (receipt.review.original.metadata as any).managedFields;
    receipt.reviewDigest = digest(receipt.review);
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow();
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "failed"]);
  });

  it.each(["missing", "null"])("preserves an exactly reviewed %s template creationTimestamp", async representation => {
    const f = fixture();
    if (representation === "null") f.deployment.spec.template.metadata.creationTimestamp = null;
    const receipt = await f.review();
    const expected = structuredClone(f.deployment.spec);
    expected.template.spec.containers[0].image = NEW;
    expect((await applyDispatcherImage(f.execute, receipt, f.journal)).phase).toBe("verified");
    expect(f.deployment.spec).toEqual(expected);
  });

  it.each(["Conflict", "Invalid", "Forbidden", "Unknown"])("retains sanitized %s diagnostics without allowing replay", async serverReason => {
    const f = fixture(), receipt = await f.review();
    const failure = privateCommandFailure({ exitCode: 1, stderr: `Error from server (${serverReason}): SECRET_CANARY`, stdout: "SECRET_CANARY" }, ["patch", "deployment"]);
    f.setPatchHook(() => { throw failure; });
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toBe(failure);
    const failed = f.records.at(-1)!;
    expect(failed.commandFailure).toEqual({ version: 1, phase: "Unscoped", operation: "patch", resourceKind: "Deployment", serverReason, exitCode: 1 });
    expect(failed.failure).toContain("image CAS failed");
    expect(JSON.stringify(f.records)).not.toContain("SECRET_CANARY");
    expect(f.deployment.spec.template.spec.containers[0].image).toBe(OLD);
    await expect(applyDispatcherImage(f.execute, failed, f.journal)).rejects.toThrow("consumed");
    const callCount = f.calls.length;
    await expect(verifyDispatcherImage(f.execute, failed, f.journal)).rejects.toThrow();
    expect(f.records.at(-1)!.commandFailure).toEqual(failed.commandFailure);
    expect(f.calls.slice(callCount).every(args => args[0] === "get")).toBe(true);
    expect(f.calls.filter(args => args[0] === "patch")).toHaveLength(1);
  });

  it("records sanitized read-only subprocess failures while preserving previous records", async () => {
    const f = fixture(), receipt = await f.review();
    const applied = await applyDispatcherImage(f.execute, receipt, f.journal);
    const failure = privateCommandFailure({ exitCode: 1, stderr: "Error from server (Forbidden): SECRET_CANARY" }, ["get", "deployment"]);
    await expect(verifyDispatcherImage(async () => { throw failure; }, applied, f.journal)).rejects.toBe(failure);
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "applied", "verified", "failed"]);
    expect(f.records.at(-1)!.commandFailure).toEqual(failure.facts);
    expect(JSON.stringify(f.records)).not.toContain("SECRET_CANARY");
  });

  it.each(["parent", "retained-parent", "epoch", "metadata", "seal", "grant", "writer", "account", "root-account", "wrong-owner", "sidecar", "init", "strategy", "replicas", "identity", "mutable-old", "unchanged", "mutable-new", "incomplete"])("refuses %s during read-only review", async fault => {
    const f = fixture();
    if (fault === "parent" || fault === "retained-parent") f.namespace.metadata.annotations[`${P}parent-dispatcher-uid`] = fault === "parent" ? "epoch" : "old-epoch";
    if (fault === "epoch") f.deployment.spec.template.metadata.annotations = { [`${P}epoch`]: "epoch" };
    if (fault === "metadata") f.deployment.metadata.annotations[`${P}state`] = "Pending";
    if (fault === "seal") f.namespace.metadata.annotations[`${P}root-retirement`] = JSON.stringify({ consumers: [{ object: { uid: "dispatcher-uid" } }] });
    if (fault === "grant") f.grants[0].spec.privateActivation = { namespaces: [{ consumers: [{ object: { uid: "dispatcher-uid" } }] }] };
    if (fault === "writer") f.grants[0].spec.writers = [{ name: NAME, namespace: NS, uid: "account-uid" }];
    if (fault === "account") f.account.metadata.annotations = { [`${P}enabled`]: "true" };
    if (fault === "root-account") f.namespace.metadata.annotations[`${P}root-uid`] = "account-uid";
    if (fault === "wrong-owner") f.deployment.metadata.annotations["meta.helm.sh/release-name"] = "other";
    if (fault === "sidecar") f.deployment.spec.template.spec.containers.push({ name: "extra", image: OLD });
    if (fault === "init") f.deployment.spec.template.spec.initContainers = [{ name: "init", image: OLD }];
    if (fault === "strategy") f.deployment.spec.strategy.type = "RollingUpdate";
    if (fault === "replicas") f.deployment.spec.replicas = 2;
    if (fault === "identity") f.deployment.spec.template.spec.containers[0].env[2].valueFrom.secretKeyRef.name = "other";
    if (fault === "mutable-old") f.deployment.spec.template.spec.containers[0].image = "dispatcher:latest";
    if (fault === "incomplete") f.incomplete();
    await expect(reviewDispatcherImage(f.execute, "kind-beta", NS, "kars", fault === "unchanged" ? OLD : fault === "mutable-new" ? "dispatcher:latest" : NEW)).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it.each(["uid", "version", "env", "metadata", "replicas", "enrollment", "private-root", "grants", "secret", "account", "cluster"])("refuses live %s drift before update and retains failure", async fault => {
    const f = fixture(), receipt = await f.review();
    if (fault === "uid") f.deployment.metadata.uid = "replacement";
    if (fault === "version") f.deployment.metadata.resourceVersion = "9";
    if (fault === "env") f.deployment.spec.template.spec.containers[0].env.push({ name: "NEW", value: "changed" });
    if (fault === "metadata") f.deployment.metadata.labels.changed = "yes";
    if (fault === "replicas") f.deployment.spec.replicas = 0;
    if (fault === "enrollment") f.namespace.metadata.annotations[`${P}parent-dispatcher-uid`] = "epoch";
    if (fault === "private-root") f.root.spec.template.spec.containers[0].image = NEW;
    if (fault === "grants") f.grants[0].spec.controller.changed = true;
    if (fault === "secret") f.secret.resourceVersion = "2";
    if (fault === "account") f.account.metadata.uid = "replacement-account";
    if (fault === "cluster") f.cluster.metadata.uid = "replacement-cluster";
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow();
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "failed"]);
  });

  it.each(["uid", "version", "metadata", "managed-fields", "spec", "null-timestamp", "absent-timestamp"])("atomically rejects last-instant %s CAS drift", async fault => {
    const f = fixture();
    if (fault === "absent-timestamp") f.deployment.spec.template.metadata.creationTimestamp = null;
    const receipt = await f.review();
    f.setPatchHook(() => {
      if (fault === "uid") f.deployment.metadata.uid = "replacement";
      if (fault === "version") f.deployment.metadata.resourceVersion = "2";
      if (fault === "metadata") f.deployment.metadata.labels.changed = "yes";
      if (fault === "managed-fields") f.deployment.metadata.managedFields[0].manager = "other";
      if (fault === "spec") f.deployment.spec.template.spec.volumes = [];
      if (fault === "null-timestamp") f.deployment.spec.template.metadata.creationTimestamp = null;
      if (fault === "absent-timestamp") delete f.deployment.spec.template.metadata.creationTimestamp;
    });
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow("CAS conflict");
    expect(f.deployment.spec.template.spec.containers[0].image).toBe(OLD);
  });

  it("never patches if durable attempt persistence fails", async () => {
    const f = fixture(), receipt = await f.review();
    await expect(applyDispatcherImage(f.execute, receipt, { save: async () => { throw new Error("fsync failed"); } })).rejects.toThrow("fsync failed");
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
  });

  it("recovers an unknown acknowledgement only by read-only live verification, preserving failure", async () => {
    const f = fixture(), receipt = await f.review();
    f.loseAck();
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow("lost acknowledgement");
    const failed = f.records.at(-1)!;
    await expect(applyDispatcherImage(f.execute, failed, f.journal)).rejects.toThrow("consumed");
    f.deployment.status.observedGeneration = 2;
    const calls = f.calls.length;
    expect((await verifyDispatcherImage(f.execute, failed, f.journal)).phase).toBe("verified");
    expect(f.calls.slice(calls).every(args => args[0] === "get")).toBe(true);
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "failed", "verified"]);
    expect(f.records.at(-1)!.failure).toBeDefined();
  });

  it.each(["image", "non-image", "identity", "private-inventory", "not-ready"])("does not claim verification after %s diverges", async fault => {
    const f = fixture(), receipt = await f.review();
    f.setRolloutHook(() => {
      if (fault === "image") f.deployment.spec.template.spec.containers[0].image = OLD;
      if (fault === "non-image") f.deployment.spec.template.spec.volumes = [];
      if (fault === "identity") f.deployment.metadata.annotations["kars.azure.com/mission-root-uid"] = "changed";
      if (fault === "private-inventory") f.namespace.metadata.annotations[`${P}parent-dispatcher-uid`] = "epoch";
      if (fault === "not-ready") f.deployment.status.readyReplicas = 0;
    });
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow();
    expect(f.records.map(r => r.phase)).toEqual(["attempting", "applied", "failed"]);
    expect(f.calls.filter(args => args[0] === "patch")).toHaveLength(1);
  });

  it("preserves stopped replica intent rather than starting the dispatcher", async () => {
    const f = fixture();
    f.deployment.spec.replicas = 0;
    f.deployment.status = { observedGeneration: 1 };
    const receipt = await f.review();
    expect((await applyDispatcherImage(f.execute, receipt, f.journal)).phase).toBe("verified");
    expect(f.deployment.spec.replicas).toBe(0);
  });

  it("records a post-read cross-resource enrollment race as failure, not successful migration", async () => {
    const f = fixture(), receipt = await f.review();
    f.setPatchHook(() => { f.namespace.metadata.annotations[`${P}parent-dispatcher-uid`] = "new-epoch"; });
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow();
    expect(f.deployment.spec.template.spec.containers[0].image).toBe(NEW);
    expect(f.records.at(-1)?.phase).toBe("failed");
    expect(f.calls.filter(args => args[0] === "patch")).toHaveLength(1);
  });

  it.each(["name-in-seal", "name-in-grant", "missing-parent", "malformed-seal", "incomplete-grant"])("refuses ambiguous private evidence: %s", async fault => {
    const f = fixture();
    if (fault === "name-in-seal") f.namespace.metadata.annotations[`${P}root-retirement`] = JSON.stringify({ consumers: [{ object: { name: NAME, uid: "old-dispatcher" } }] });
    if (fault === "name-in-grant") f.grants[0].spec.privateActivation = { consumer: { name: NAME, uid: "old-dispatcher" } };
    if (fault === "missing-parent") f.namespace.metadata.annotations[`${P}parent-missing-uid`] = "old-epoch";
    if (fault === "malformed-seal") f.namespace.metadata.annotations[`${P}root-retirement`] = "{bad";
    if (fault === "incomplete-grant") f.grants[0].spec.privateActivation = "opaque unknown proof";
    await expect(f.review()).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("rejects a target spec widened beyond the one selected image even with a recomputed digest", async () => {
    const f = fixture(), receipt = await f.review();
    receipt.review.targetSpec.replicas = 0;
    receipt.reviewDigest = digest(receipt.review);
    await expect(applyDispatcherImage(f.execute, receipt, f.journal)).rejects.toThrow();
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
  });
});
