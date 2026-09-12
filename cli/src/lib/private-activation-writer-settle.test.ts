// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { continuityFixture, privateAuthoritySnapshot } from "./private-activation-fixtures.js";
import { canonical, PRIVATE_PREFIX as P, type Execute } from "./private-activation.js";
import { captureGuardRetirement, refreshGuardRetirement } from "./private-activation-guard-retirement.js";
import { captureWriterSettlement } from "./private-activation-writer-settle.js";

const RESOURCE = "karscredentialgrants.kars.azure.com";
const C = "kars.azure.com/credential-";
const VERSION = `${C}projection-version`;
const INPUTS = `${C}input-state`;
const REVISION = "deployment.kubernetes.io/revision";
const AUTH = `sha256:${"a".repeat(64)}`;
const consumer = "kars-late/Deployment/late";
const data = { SLACK_BOT_TOKEN: Buffer.from("original-customer-token").toString("base64") };

async function setup(originalRuntime = false) {
  const f = continuityFixture();
  if (originalRuntime) {
    await applyReviewedGrant(f.execute, { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: "work" },
      spec: { workspaceUid: "work-uid", enabled: true, writers: [] } });
  } else {
    await applyReviewedGrant(f.execute, await f.document());
    await applyReviewedGrant(f.execute, await f.document("second"));
  }
  f.grant().status.phase = "Ready";
  const grantUid = f.grant().metadata.uid;
  const owner = (kind: string, name: string, uid: string) => ({
    apiVersion: kind === "Namespace" ? "v1" : "kars.azure.com/v1alpha1",
    kind, name, uid, controller: true, blockOwnerDeletion: false,
  });
  const bindings = { grant: { name: "workspace", uid: grantUid }, sources: [
    { scope: "workspace", source: { name: "kars-credential-input-workspace", uid: "input-uid" }, keys: ["SLACK_BOT_TOKEN"] },
  ] };
  const task: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask",
    metadata: { name: "late", namespace: "work", uid: "task-uid", resourceVersion: "1", generation: 1,
      annotations: { [`${C}bundle-uid`]: "bundle-uid" } },
    spec: { objective: "Existing native observer", execution: { launch: true }, blueprint: { credentialBindings: bindings } },
    status: { phase: "Ready", observedGeneration: 1, envelopeDigest: AUTH, sandboxRef: { name: "late" },
      conditions: [{ type: "Ready", status: "True", observedGeneration: 1, reason: "Reconciled" }] } };
  const sandbox: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
    metadata: { name: "late", namespace: "work", uid: "sandbox-uid", resourceVersion: "1", generation: 1,
      annotations: { "kars.azure.com/namespace-uid": "runtime-uid" }, ownerReferences: [owner("KarsTask", "late", "task-uid")] },
    spec: { credentialBindings: bindings },
    status: { phase: "Running", observedGeneration: 1, conditions: [{ type: "Ready", status: "True", observedGeneration: 1 }] } };
  const namespace: any = { kind: "Namespace", metadata: { name: "kars-late", uid: "runtime-uid", resourceVersion: "1",
    annotations: { "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/sandbox-name": "late",
      "kars.azure.com/sandbox-namespace": "work", "kars.azure.com/sandbox-uid": "sandbox-uid" } } };
  const input: any = { kind: "Secret", type: "Opaque", metadata: { name: "kars-credential-input-workspace", namespace: "work",
    uid: "input-uid", resourceVersion: "1", annotations: { [`${C}purpose`]: "agent-input-v2", [`${C}grant-uid`]: grantUid } }, data };
  const inputs = { grantUid, grantGeneration: f.grant().metadata.generation,
    target: { kind: "KarsTask", namespace: "work", name: "late", uid: "task-uid" }, bindings,
    sources: [{ name: input.metadata.name, uid: "input-uid", resourceVersion: "1", keys: ["SLACK_BOT_TOKEN"], scope: "workspace" }] };
  const bundle: any = { kind: "Secret", type: "Opaque", metadata: { name: "kars-credential-bundle-karstask-late", namespace: "work",
    uid: "bundle-uid", resourceVersion: "1", ownerReferences: [owner("KarsTask", "late", "task-uid")],
    annotations: { [`${C}purpose`]: "agent-bundle-v2", [`${C}grant-uid`]: grantUid, [`${C}target-uid`]: "task-uid", [INPUTS]: JSON.stringify(inputs) } }, data };
  const projection: any = { kind: "Secret", type: "Opaque", metadata: { name: "late-credential-projection", namespace: "kars-late",
    uid: "projection-uid", resourceVersion: "1", ownerReferences: [owner("Namespace", "kars-late", "runtime-uid")],
    annotations: { [`${C}purpose`]: "agent-projection-v1", [`${C}sandbox-uid`]: "sandbox-uid",
      [`${C}namespace-uid`]: "runtime-uid", [`${C}projection-uid`]: "projection-uid", [`${C}source-uid`]: "bundle-uid" } }, data };
  const admin: any = { kind: "Secret", type: "Opaque", metadata: { name: "router-services-admin", namespace: "kars-late",
    uid: "admin-uid", resourceVersion: "1", labels: { "app.kubernetes.io/managed-by": "kars-controller" },
    annotations: { "kars.azure.com/sandbox-uid": "sandbox-uid", "kars.azure.com/namespace-uid": "runtime-uid" } },
    data: { "control-token": Buffer.from("A".repeat(64)).toString("base64") } };
  const deployment: any = { kind: "Deployment", metadata: { name: "late", namespace: "kars-late", uid: "deployment-uid",
    resourceVersion: "1", generation: 1, labels: { "kars.azure.com/sandbox": "late", "kars.azure.com/component": "sandbox" },
    annotations: { [`${C}sandbox-uid`]: "sandbox-uid", [`${C}namespace-uid`]: "runtime-uid", [REVISION]: "1" } },
    spec: { replicas: 1, strategy: { type: "Recreate" }, selector: { matchLabels: { app: "late" } },
      template: { metadata: { annotations: { [VERSION]: "projection-uid:1", "kars.azure.com/services-credential-version": "admin-uid:1" } },
        spec: { automountServiceAccountToken: false, volumes: [{ name: "governed-services-control",
          secret: { secretName: "router-services-admin", items: [{ key: "control-token", path: "control-token" }] } }],
        containers: [{ name: "inference-router", image: "fixture", volumeMounts: [
          { name: "governed-services-control", mountPath: "/etc/kars/services", readOnly: true }],
          env: [{ name: "KARS_SERVICE_IDENTITY_JSON", value: JSON.stringify({
            task: { uid: "task-uid", name: "late", namespace: "work" }, task_authorization: AUTH, task_generation: 1,
          }) }] }] } } },
    status: { observedGeneration: 1, updatedReplicas: 1, availableReplicas: 1 } };
  for (const [kind, object, ns] of [
    ["namespace", namespace, ""], ["karstask", task, "work"], ["karssandbox", sandbox, "work"],
    ["deployments.apps", deployment, "kars-late"], ["secret", input, "work"], ["secret", bundle, "work"],
    ["secret", projection, "kars-late"], ["secret", admin, "kars-late"],
  ] as const) f.objects.set(f.key(kind, object.metadata.name, ns), object);
  const pod = (uid: string) => {
    f.objects.set(f.key("replicasets.apps", "rs", "kars-late"), { kind: "ReplicaSet",
      metadata: { name: "rs", uid: "rs-uid", resourceVersion: "1", ownerReferences: [
        { apiVersion: "apps/v1", kind: "Deployment", name: "late", uid: "deployment-uid", controller: true }] },
      spec: { template: structuredClone(deployment.spec.template) } });
    return { kind: "Pod", metadata: { name: uid, uid, resourceVersion: "1",
      annotations: structuredClone(deployment.spec.template.metadata.annotations), ownerReferences: [
        { apiVersion: "apps/v1", kind: "ReplicaSet", name: "rs", uid: "rs-uid", controller: true }] },
    spec: structuredClone(deployment.spec.template.spec) };
  };
  f.pods.set("kars-late", [pod("original-pod")]);
  const bump = (object: any) => { object.metadata.resourceVersion = String(Number(object.metadata.resourceVersion) + 1); };
  const deploymentStatus = () => { deployment.status = { observedGeneration: deployment.metadata.generation,
    updatedReplicas: deployment.spec.replicas, availableReplicas: deployment.spec.replicas }; };
  let retired = false;
  let restored = false;
  let allowRestore = true;
  let restoreAt = 2;
  let emptyReads = 0;
  let fault: ((stage: string) => void) | undefined;
  const restore = () => {
    restored = true;
    task.status = { phase: "Ready", observedGeneration: 1, envelopeDigest: AUTH, sandboxRef: { name: "late" },
      conditions: [{ type: "Ready", status: "True", observedGeneration: 1, reason: "Reconciled" }] };
    bump(task);
    sandbox.status = { phase: "Running", observedGeneration: 1, conditions: [{ type: "Ready", status: "True", observedGeneration: 1 }] };
    bump(sandbox);
    bundle.metadata.annotations[INPUTS] = JSON.stringify({ ...inputs, grantGeneration: f.grant().metadata.generation });
    bump(bundle);
    projection.data = structuredClone(data); bump(projection);
    deployment.spec.replicas = 1;
    deployment.spec.template.metadata.annotations[VERSION] = `projection-uid:${projection.metadata.resourceVersion}`;
    deployment.metadata.annotations[REVISION] = "2";
    deployment.metadata.generation++; bump(deployment); deploymentStatus();
    f.pods.set("kars-late", [pod("reattested-pod")]);
    fault?.("restored");
  };
  const execute: Execute = async (args, inputValue) => {
    const result = await f.execute(args, inputValue);
    if (allowRestore && retired && !restored && args[0] === "get" && args[1] === "secret" && args[2] === projection.metadata.name) {
      if (++emptyReads === restoreAt) restore();
    }
    if (args[0] !== "patch") return result;
    const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
    if (args[1] === RESOURCE && patch.spec.writers.length === 0) {
      retired = true;
      f.grant().status.phase = "Ready";
      task.status = { phase: "Degraded", observedGeneration: 1, envelopeDigest: null, sandboxRef: { name: "late" },
        conditions: [{ type: "Ready", status: "False", observedGeneration: 1, reason: "CredentialAuthorityUnavailable" }] };
      bump(task);
      sandbox.status = { phase: "Degraded", observedGeneration: 1, conditions: [
        { type: "Ready", status: "False", observedGeneration: 1, reason: "CredentialSourceUnavailable" }] };
      bump(sandbox);
      deployment.spec.replicas = 0; deployment.metadata.generation++; bump(deployment); deploymentStatus();
      projection.data = {}; bump(projection);
      f.pods.set("kars-late", []);
      fault?.("retired");
    }
    if (args[1] === "karssandbox") {
      sandbox.metadata.generation++;
      if (patch.spec.suspended === null) delete sandbox.spec.suspended;
      if (sandbox.spec.suspended !== true) {
        deployment.spec.replicas = 1; deployment.metadata.generation++; bump(deployment); deploymentStatus();
        f.pods.set("kars-late", [pod("qualified-pod")]);
      }
    }
    if (args[1] === "deployments.apps" && patch.spec.replicas === 0) f.pods.set("kars-late", []);
    if (args[1] === "namespace" && args[2] === "kars-late"
      && namespace.metadata.annotations[`${P}root-retirement`]
      && JSON.parse(namespace.metadata.annotations[`${P}root-retirement`]).phase === "Rotating") {
      admin.data["control-token"] = Buffer.from("B".repeat(64)).toString("base64"); bump(admin);
      admin.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      deployment.spec.template.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      deployment.spec.template.metadata.annotations["kars.azure.com/services-credential-version"] = `admin-uid:${admin.metadata.resourceVersion}`;
      deployment.metadata.generation++; bump(deployment); deploymentStatus();
    }
    return result;
  };
  const document = () => f.document("work", [consumer], execute);
  if (originalRuntime) {
    await applyReviewedGrant(execute, await document());
    await f.execute(["patch", "deployments.apps", "late", "-n", "kars-late", "--type=merge", "-p", JSON.stringify({
      metadata: { uid: deployment.metadata.uid, resourceVersion: deployment.metadata.resourceVersion }, spec: { replicas: 1 },
    })]);
    f.pods.set("kars-late", [pod("original-qualified-pod")]);
    await applyReviewedGrant(f.execute, await f.document("second"));
  }
  const preserved = () => structuredClone({ root: f.objects.get(f.key("namespace", "core")),
    otherGrant: f.grant("second"), reader: privateAuthoritySnapshot(f.objects.get(f.key("namespace", "reader"))),
    rootDeployment: f.deployment, rootPods: f.pods.get("core"), input, taskSpec: task.spec, sandboxSpec: sandbox.spec });
  return { ...f, execute, document, passiveExecute: f.execute, preserved, task, sandbox, namespace, deployment, bundle, projection, input, admin,
    fault: (callback: (stage: string) => void) => { fault = callback; }, restore, wasRestored: () => restored,
    neverRestore: () => { allowRestore = false; }, delayRestore: () => { restoreAt = 8; } };
}

describe("late runtime authority across selected writer retirement", () => {
  beforeEach(() => { vi.spyOn(console, "error").mockImplementation(() => {}); });
  afterEach(() => { vi.restoreAllMocks(); });

  it("reproduces the rejected null attestation in the original immediate post-retirement validation", async () => {
    const f = await setup();
    const review = await f.document();
    const grant = structuredClone(f.grant());
    const guard = await captureGuardRetirement(f.execute, review.spec.privateActivation, grant);
    await f.execute(["patch", RESOURCE, "workspace", "-n", "work", "--type=merge", "-p", JSON.stringify({
      metadata: { uid: grant.metadata.uid, resourceVersion: grant.metadata.resourceVersion },
      spec: { ...grant.spec, writers: [] },
    })]);
    expect(f.task.status.envelopeDigest).toBeNull();
    await expect(refreshGuardRetirement(f.execute, guard)).rejects.toThrow("Task authorization");
    expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
  });

  it("updates an active grant whose runtime was qualified in the shared v2 proof, without a local scope receipt", async () => {
    const f = await setup(true);
    const root = f.objects.get(f.key("namespace", "core"));
    const proof = JSON.parse(root.metadata.annotations[`${P}root-retirement`]);
    expect(proof.version).toBe(2);
    expect(proof.activation.namespaces.some((scope: any) => scope.namespace.uid === f.namespace.metadata.uid)).toBe(true);
    expect(f.namespace.metadata.annotations[`${P}state`]).toBe("Qualified");
    expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
    expect(f.deployment.spec.replicas).toBe(1);
    expect(f.pods.get("kars-late")).toHaveLength(1);
    const review = await f.document();
    const before = structuredClone({ root, namespace: f.namespace, deployment: f.deployment, otherGrant: f.grant("second") });
    expect(await captureWriterSettlement(f.passiveExecute, review.spec.privateActivation, f.grant())).toBeUndefined();
    f.calls.length = 0;
    await applyReviewedGrant(f.passiveExecute, review);
    expect({ root, namespace: f.namespace, deployment: f.deployment, otherGrant: f.grant("second") }).toEqual(before);
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === RESOURCE)).toBe(true);
  });

  it.each(["epoch", "state"])("does not skip an unproven private %s marker during settlement classification", async marker => {
    const f = await setup();
    const review = await f.document();
    f.namespace.metadata.annotations[`${P}${marker}`] = marker === "epoch" ? "a".repeat(64) : "Qualified";
    f.calls.length = 0;
    await expect(captureWriterSettlement(f.execute, review.spec.privateActivation, f.grant())).rejects.toThrow("unproven private lifecycle");
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("waits through real withdrawal, owned pause and exact projection refill before private qualification", async () => {
    const f = await setup();
    const review = await f.document();
    const original = structuredClone(review);
    const before = f.preserved();
    f.delayRestore();
    const waits = vi.spyOn(globalThis, "setTimeout");
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, review);
    expect(f.wasRestored()).toBe(true);
    expect(review).toEqual(original);
    expect(f.preserved()).toEqual(before);
    expect(f.task.status.envelopeDigest).toBe(AUTH);
    expect(f.projection.data).toEqual(data);
    expect(f.bundle.data).toEqual(data);
    expect(f.admin.data["control-token"]).toBe(Buffer.from("B".repeat(64)).toString("base64"));
    expect(f.namespace.metadata.annotations[`${P}state`]).toBe("Qualified");
    const recorded = JSON.parse(f.namespace.metadata.annotations[`${P}root-retirement`]);
    expect(recorded.captured).toEqual(["reattested-pod"]);
    expect(recorded.deployment.uid).toBe("deployment-uid");
    expect(recorded.runtime.task.authorization).toBe(AUTH);
    expect(f.calls.filter(args => args[0] === "patch" && args[1] === RESOURCE)).toHaveLength(2);
    expect(waits.mock.calls.some(([, delay]) => delay === 500)).toBe(true);
  });

  it.each(["disabled", "keys", "grant-uid", "source", "source-uid", "task-spec", "task-uid", "task-owner",
    "task-generation", "sandbox-spec", "sandbox-uid", "sandbox-owner", "template", "private-key", "projection-key", "bundle-anchor",
    "additional-private", "projection-uid", "bundle-data", "namespace", "deployment-uid", "deployment-generation"])(
    "does not settle changed %s authority", async fault => {
      const f = await setup();
      const review = await f.document();
      f.neverRestore();
      f.fault(stage => {
        if (stage !== "retired") return;
        if (fault === "disabled") f.grant().spec.enabled = false;
        if (fault === "keys") f.grant().spec.agentKeys = ["UNREVIEWED_TOKEN"];
        if (fault === "grant-uid") f.grant().metadata.uid = "different";
        if (fault === "source") f.input.data = { SLACK_BOT_TOKEN: "changed" };
        if (fault === "source-uid") f.input.metadata.uid = "different";
        if (fault === "task-spec") f.task.spec.objective = "different";
        if (fault === "task-uid") f.task.metadata.uid = "different";
        if (fault === "task-owner") f.task.metadata.ownerReferences = [{ uid: "different" }];
        if (fault === "task-generation") f.task.metadata.generation++;
        if (fault === "sandbox-spec") f.sandbox.spec.suspended = true;
        if (fault === "sandbox-uid") f.sandbox.metadata.uid = "different";
        if (fault === "sandbox-owner") f.sandbox.metadata.ownerReferences[0].uid = "different";
        if (fault === "template") f.deployment.spec.template.spec.containers[0].image = "different";
        if (fault === "private-key") f.admin.data["control-token"] = Buffer.from("C".repeat(64)).toString("base64");
        if (fault === "projection-key") f.projection.data = { SLACK_BOT_TOKEN: "different" };
        if (fault === "bundle-anchor") f.task.metadata.annotations[`${C}bundle-uid`] = "different";
        if (fault === "additional-private") f.objects.set(f.key("secret", "router-services-observer", "kars-late"),
          { metadata: { name: "router-services-observer", uid: "foreign", resourceVersion: "1" } });
        if (fault === "projection-uid") f.projection.metadata.uid = "different";
        if (fault === "bundle-data") f.bundle.data = { SLACK_BOT_TOKEN: "new-key" };
        if (fault === "namespace") f.namespace.metadata.annotations.unreviewed = "changed";
        if (fault === "deployment-uid") f.deployment.metadata.uid = "different";
        if (fault === "deployment-generation") f.deployment.metadata.generation += 4;
      });
      f.calls.length = 0;
      await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
      expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
      expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === RESOURCE)).toBe(true);
    });

  it("times out without substituting the captured digest when fresh authority never returns", async () => {
    const f = await setup();
    const review = await f.document();
    f.neverRestore();
    f.fault(stage => {
      if (stage === "retired") {
        const elapsed = Date.now() + 121_000;
        vi.spyOn(Date, "now").mockReturnValue(elapsed);
      }
    });
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow("awaiting fresh Task attestation");
    expect(f.task.status.envelopeDigest).toBeNull();
    expect(f.grant().spec.writers).toEqual([]);
    expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
  });

  it("rejects extra template changes even when a fresh current Task re-attests", async () => {
    const f = await setup();
    const review = await f.document();
    f.fault(stage => {
      if (stage === "restored") f.deployment.spec.template.spec.containers[0].env.push({ name: "UNREVIEWED", value: "changed" });
    });
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
    expect(f.wasRestored()).toBe(true);
    expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
    expect(canonical(f.grant().spec.writers)).toBe("[]");
  });

  it.each(["stale-task-version", "changed-task-digest", "stale-projection-version", "original-projection-version", "unconsumed-refill"])(
    "does not accept %s as authentic restoration", async fault => {
      const f = await setup();
      const review = await f.document();
      f.fault(stage => {
        if (stage !== "restored") return;
        if (fault === "stale-task-version") f.task.metadata.resourceVersion = "1";
        if (fault === "changed-task-digest") f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
        if (fault === "stale-projection-version") {
          f.projection.metadata.resourceVersion = "2";
          f.deployment.spec.template.metadata.annotations[VERSION] = "projection-uid:2";
        }
        if (fault === "original-projection-version") {
          f.projection.metadata.resourceVersion = "1";
          f.deployment.spec.template.metadata.annotations[VERSION] = "projection-uid:1";
          f.deployment.metadata.annotations[REVISION] = "1";
          for (const pod of f.pods.get("kars-late")!) pod.metadata.annotations[VERSION] = "projection-uid:1";
        }
        if (fault === "unconsumed-refill") {
          f.deployment.spec.template.metadata.annotations[VERSION] = "projection-uid:1";
          f.deployment.metadata.annotations[REVISION] = "1";
          const elapsed = Date.now() + 121_000;
          vi.spyOn(Date, "now").mockReturnValue(elapsed);
        }
      });
      const result = applyReviewedGrant(f.execute, review);
      if (["stale-projection-version", "original-projection-version"].includes(fault)) {
        await expect(result).rejects.toThrow("witnessed fresh revoke/refill");
      } else {
        await expect(result).rejects.toThrow();
      }
      expect(f.namespace.metadata.annotations[`${P}root-retirement`]).toBeUndefined();
    });
});
