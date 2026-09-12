// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { continuityFixture, privateAuthoritySnapshot } from "./private-activation-fixtures.js";
import { PRIVATE_PREFIX as P, canonical, type Execute } from "./private-activation.js";

const HISTORY = `${P}root-retirement`;
const VERSION = "kars.azure.com/services-credential-version";
const ADMIN = "router-services-admin";
const SOURCE = "kars.azure.com/sandbox-uid";
const NS = "kars.azure.com/namespace-uid";
const consumer = "kars-late/Deployment/late";
const AUTHORIZATION = `sha256:${"a".repeat(64)}`;
const SNAPSHOT_MARKER = "KARS_PRIVATE_LATE_SANDBOX_CHECKS ";
const cliProcess = vi.hoisted(() => ({ execute: vi.fn() }));
vi.mock("execa", () => ({ execa: cliProcess.execute }));

async function setup(suspended: boolean | null = null) {
  const f = continuityFixture();
  await applyReviewedGrant(f.execute, await f.document());
  await applyReviewedGrant(f.execute, await f.document("second"));
  const namespace: any = { kind: "Namespace", metadata: {
    name: "kars-late", uid: "runtime-ns", resourceVersion: "1", annotations: {
      "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/sandbox-name": "late",
      "kars.azure.com/sandbox-namespace": "work", [SOURCE]: "sandbox",
    } } };
  const task: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: {
    name: "task", namespace: "work", uid: "task-uid", resourceVersion: "1", generation: 1,
  }, spec: { execution: { launch: true }, objective: "Retain real Task authority" },
  status: { phase: "Ready", sandboxRef: { name: "late" }, observedGeneration: 1,
    envelopeDigest: AUTHORIZATION, conditions: [{ type: "Ready", status: "True" }] } };
  const sandbox: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox", metadata: {
    name: "late", namespace: "work", uid: "sandbox", resourceVersion: "1", generation: 1,
    annotations: { [NS]: "runtime-ns" }, ownerReferences: [{
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: "task", uid: "task-uid", controller: true,
    }] }, spec: { credentialsRef: { name: "kars-credential-bundle-source", uid: "bundle-uid" },
    ...(suspended === null ? {} : { suspended }) }, status: { phase: "Running", observedGeneration: 1,
      conditions: [{ type: "Ready", status: "True", observedGeneration: 1 }] } };
  const secret: any = { kind: "Secret", type: "Opaque", metadata: {
    name: ADMIN, namespace: "kars-late", uid: "admin-secret", resourceVersion: "1",
    labels: { "app.kubernetes.io/managed-by": "kars-controller" },
    annotations: { [SOURCE]: "sandbox", [NS]: "runtime-ns" },
  }, data: { "control-token": Buffer.from("A".repeat(64)).toString("base64") } };
  const deployment: any = { apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "late", namespace: "kars-late", uid: "late-deployment", resourceVersion: "1", generation: 1,
    labels: { "kars.azure.com/sandbox": "late", "kars.azure.com/component": "sandbox" },
    annotations: { "kars.azure.com/credential-sandbox-uid": "sandbox", "kars.azure.com/credential-namespace-uid": "runtime-ns" },
  }, spec: { replicas: suspended ? 0 : 1, selector: { matchLabels: { app: "late" } }, template: {
    metadata: { labels: { app: "late" }, annotations: { [VERSION]: "admin-secret:1" } },
    spec: { automountServiceAccountToken: false, volumes: [
      { name: "governed-services-control", secret: { secretName: ADMIN, items: [{ key: "control-token", path: "control-token" }] } },
      { name: "optional-app", secret: { secretName: "router-github-app", optional: true } },
    ], containers: [{ name: "inference-router", image: "fixture", env: [
      { name: "KARS_SERVICE_IDENTITY_JSON", value: JSON.stringify({
        task: { uid: "task-uid", namespace: "work", name: "task" },
        task_authorization: AUTHORIZATION, task_generation: 1,
      }) },
    ], volumeMounts: [{ name: "governed-services-control", mountPath: "/etc/kars/services", readOnly: true }] }] } } },
  status: { observedGeneration: 1, updatedReplicas: suspended ? 0 : 1, availableReplicas: suspended ? 0 : 1 } };
  const projection = { kind: "Secret", metadata: { name: "projection", uid: "projection-uid", resourceVersion: "8" },
    data: { CUSTOMER_KEY: "preserved-value" } };
  const source = { kind: "Secret", metadata: { name: "source", uid: "source-uid", resourceVersion: "9" },
    data: { CUSTOMER_KEY: "preserved-source" } };
  for (const [kind, object, ns] of [
    ["namespace", namespace, ""], ["karstask", task, "work"], ["karssandbox", sandbox, "work"],
    ["deployments.apps", deployment, "kars-late"], ["secret", secret, "kars-late"],
    ["secret", projection, "kars-late"], ["secret", source, "work"],
  ] as const) f.objects.set(f.key(kind, object.metadata.name, ns), object);
  const pod = (uid: string) => {
    f.objects.set(f.key("replicasets.apps", "late-rs", "kars-late"), {
      kind: "ReplicaSet", metadata: { name: "late-rs", namespace: "kars-late", uid: "late-rs-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "late", uid: "late-deployment", controller: true }] },
      spec: { template: structuredClone(deployment.spec.template) },
    });
    return { kind: "Pod", metadata: { name: uid, namespace: "kars-late", uid, resourceVersion: "1",
      annotations: structuredClone(deployment.spec.template.metadata.annotations),
      ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "late-rs", uid: "late-rs-uid", controller: true }] },
    spec: structuredClone(deployment.spec.template.spec) };
  };
  f.pods.set("kars-late", suspended ? [] : [pod("old-running"), {
    ...pod("old-terminating"), metadata: { ...pod("old-terminating").metadata, deletionTimestamp: "2026-09-12T01:00:00Z" },
  }]);
  let rotate = true;
  let keepPods = false;
  const updateDeployment = () => {
    deployment.metadata.resourceVersion = String(Number(deployment.metadata.resourceVersion) + 1);
    deployment.metadata.generation++;
    deployment.status = { observedGeneration: deployment.metadata.generation,
      updatedReplicas: deployment.spec.replicas, availableReplicas: deployment.spec.replicas };
  };
  const execute: Execute = async (args, input) => {
    const result = await f.execute(args, input);
    if (args[0] !== "patch") return result;
    const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
    if (args[1] === "karssandbox" && patch.spec) {
      sandbox.metadata.generation++;
      sandbox.status.observedGeneration = sandbox.metadata.generation;
      if (patch.spec.suspended === null) delete sandbox.spec.suspended;
      if (sandbox.spec.suspended !== true) {
        deployment.spec.replicas = 1;
        updateDeployment();
        f.pods.set("kars-late", [pod("new-current")]);
      }
    }
    if (args[1] === "deployments.apps" && patch.spec?.replicas === 0 && !keepPods) f.pods.set("kars-late", []);
    if (args[1] === "namespace" && args[2] === "kars-late"
      && JSON.parse(namespace.metadata.annotations[HISTORY]).phase === "Rotating") {
      expect(sandbox.spec.suspended).toBe(true);
      expect(f.pods.get("kars-late")).toEqual([]);
      expect(deployment.spec.replicas).toBe(0);
      if (rotate) secret.data["control-token"] = Buffer.from("B".repeat(64)).toString("base64");
      secret.metadata.resourceVersion = "2";
      secret.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      deployment.spec.template.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      deployment.spec.template.metadata.annotations[VERSION] = "admin-secret:2";
      updateDeployment();
    }
    return result;
  };
  const document = (run = execute) => f.document("work", [consumer], run);
  const preserved = () => structuredClone({ root: f.namespace("core"), reader: privateAuthoritySnapshot(f.namespace("reader")),
    otherGrant: f.grant("second"), otherAuthority: f.authority.get(f.grant("second").metadata.uid),
    rootDeployment: f.deployment, rootPods: f.pods.get("core"), source, projection, task });
  f.calls.length = 0;
  return { ...f, namespace, task, sandbox, secret, deployment, document, execute, preserved,
    refuseRotation: () => { rotate = false; }, keepPods: () => { keepPods = true; } };
}

describe("reviewed late runtime private enrollment", () => {
  beforeEach(() => {
    cliProcess.execute.mockReset();
    vi.spyOn(console, "error").mockImplementation(() => {});
  });
  afterEach(() => { vi.restoreAllMocks(); });

  it("accepts an identical current Sandbox after an intervening status PATCH advances only resourceVersion", async () => {
    const f = await setup();
    let updated = false;
    const statusUpdate: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!updated && args[0] === "get" && args[1] === "karstask") {
        updated = true;
        f.sandbox.status = structuredClone(f.sandbox.status);
        f.sandbox.metadata.resourceVersion = "2";
      }
      return result;
    };
    const before = f.preserved();
    const review = await f.document(statusUpdate);
    expect(updated).toBe(true);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "karssandbox")).toHaveLength(3);
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "karstask")).toHaveLength(2);
    expect(vi.mocked(console.error).mock.calls.some(([value]) => String(value).startsWith(SNAPSHOT_MARKER))).toBe(false);
    await applyReviewedGrant(f.execute, review);
    expect(f.preserved()).toEqual(before);
    expect(f.sandbox.spec.suspended).toBeUndefined();
  });

  it.each(["same-status", "changed-spec", "stale-ready"])(
    "exercises the shipped --observe preview and validation command with concurrent %s", async fault => {
      const f = await setup();
      const output = vi.spyOn(console, "log").mockImplementation(() => {});
      let updated = false;
      cliProcess.execute.mockImplementation(async (program: string, args: string[], options: { input?: string }) => {
        expect(program).toBe("kubectl");
        const stdout = await f.execute(args, options.input);
        if (!updated && args[0] === "get" && args[1] === "karstask") {
          updated = true;
          f.sandbox.metadata.resourceVersion = "2";
          if (fault === "same-status") f.sandbox.status = structuredClone(f.sandbox.status);
          if (fault === "changed-spec") f.sandbox.spec.credentialsRef.uid = "unreviewed-source";
          if (fault === "stale-ready") f.sandbox.status.conditions[0].observedGeneration = 0;
        }
        return { stdout };
      });
      const preview = credentialGrantsCommand().parseAsync([
        "preview", "--namespace", "work", "--writer", "reader/bff", "--observe", "late",
        "--private-root", "core", "--private-controller-profile", "kcm-certificate",
        "--private-consumer", consumer,
      ], { from: "user" });
      if (fault === "same-status") {
        await preview;
        expect(output).toHaveBeenCalledTimes(1);
        const review = JSON.parse(String(output.mock.calls[0]![0]));
        expect(review.spec.observationTargets).toEqual([{ kind: "KarsSandbox", namespace: "work", name: "late", uid: "sandbox" }]);
        expect(review.spec.privateActivation.phase).toBe("reviewed");
        expect(JSON.stringify(review)).not.toContain("control-token");
        expect(JSON.stringify(review)).not.toContain("conditions");
      } else {
        await expect(preview).rejects.toThrow();
        expect(output).not.toHaveBeenCalled();
      }
      expect(updated).toBe(true);
      expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    });

  it.each(["uid", "spec", "generation", "owner", "labels", "annotations", "finalizers", "managed-fields",
    "namespace-binding", "observation-authority", "status-content"])(
    "does not treat concurrent Sandbox %s drift as a harmless resourceVersion update", async fault => {
      const f = await setup();
      let changed = false;
      const race: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!changed && args[0] === "get" && args[1] === "karstask") {
          changed = true;
          f.sandbox.metadata.resourceVersion = "2";
          if (fault === "uid") f.sandbox.metadata.uid = "must-not-be-logged";
          if (fault === "spec") f.sandbox.spec.credentialsRef.uid = "changed-source";
          if (fault === "generation") f.sandbox.metadata.generation = 2;
          if (fault === "owner") f.sandbox.metadata.ownerReferences[0].uid = "changed-task";
          if (fault === "labels") f.sandbox.metadata.labels = { changed: "must-not-be-logged" };
          if (fault === "annotations") f.sandbox.metadata.annotations.unreviewed = "must-not-be-logged";
          if (fault === "finalizers") f.sandbox.metadata.finalizers = ["changed-finalizer"];
          if (fault === "managed-fields") f.sandbox.metadata.managedFields = [{ manager: "changed-manager" }];
          if (fault === "namespace-binding") f.sandbox.metadata.annotations[NS] = "changed-namespace";
          if (fault === "observation-authority") f.sandbox.status.serviceObservation = { phase: "Prepared" };
          if (fault === "status-content") f.sandbox.status.conditions[0].message = "changed-status";
        }
        return result;
      };
      await expect(f.document(race)).rejects.toThrow();
      expect(changed).toBe(true);
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      const markers = vi.mocked(console.error).mock.calls.map(([value]) => String(value))
        .filter(value => value.startsWith(SNAPSHOT_MARKER));
      expect(markers).toEqual([`${SNAPSHOT_MARKER}{"resourceVersionMatch":false,"observedGenerationMatch":true,"phaseRunningMatch":true,"readyConditionMatch":true}`]);
      expect(markers.join("")).not.toContain("must-not-be-logged");
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    });

  it.each(["observed-generation", "phase", "missing-ready", "false-ready", "stale-ready", "missing-ready-generation", "malformed-conditions"])(
    "reports only fixed readiness match booleans for %s without a fallback", async fault => {
      const f = await setup();
      if (fault === "observed-generation") f.sandbox.status.observedGeneration = 0;
      if (fault === "phase") f.sandbox.status.phase = "Degraded";
      if (fault === "missing-ready") f.sandbox.status.conditions = [];
      if (fault === "false-ready") f.sandbox.status.conditions[0].status = "False";
      if (fault === "stale-ready") f.sandbox.status.conditions[0].observedGeneration = 0;
      if (fault === "missing-ready-generation") delete f.sandbox.status.conditions[0].observedGeneration;
      if (fault === "malformed-conditions") f.sandbox.status.conditions = {};
      await expect(f.document()).rejects.toThrow();
      const markers = vi.mocked(console.error).mock.calls.map(([value]) => String(value))
        .filter(value => value.startsWith(SNAPSHOT_MARKER));
      expect(markers).toHaveLength(1);
      expect(JSON.parse(markers[0]!.slice(SNAPSHOT_MARKER.length))).toEqual({
        resourceVersionMatch: true, observedGenerationMatch: fault !== "observed-generation",
        phaseRunningMatch: fault !== "phase",
        readyConditionMatch: ["observed-generation", "phase"].includes(fault),
      });
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it.each(["task-spec", "task-owner", "task-uid", "task-authorization", "task-metadata",
    "deployment-env", "deployment-metadata", "namespace"])(
    "rechecks the bounded read set and preserves authority on concurrent %s drift", async fault => {
      const f = await setup(true);
      let changed = false;
      const race: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!changed && args[0] === "get" && args[1] === "karstask") {
          changed = true;
          if (fault === "task-spec") f.task.spec.objective = "changed";
          if (fault === "task-owner") f.task.metadata.ownerReferences = [{ uid: "changed-team" }];
          if (fault === "task-uid") f.task.metadata.uid = "changed-task";
          if (fault === "task-authorization") f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
          if (fault === "task-metadata") f.task.metadata.labels = { changed: "authority" };
          if (fault === "deployment-env") f.deployment.spec.template.spec.containers[0].env[0].value = "{}";
          if (fault === "deployment-metadata") f.deployment.metadata.labels.changed = "authority";
          if (fault === "namespace") f.namespace.metadata.annotations[SOURCE] = "changed-sandbox";
        }
        return result;
      };
      await expect(f.document(race)).rejects.toThrow();
      expect(changed).toBe(true);
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
      expect(f.sandbox.spec.suspended).toBe(true);
    });

  it("accepts an identical Task status snapshot without rebasing its authority", async () => {
    const f = await setup();
    let updated = false;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!updated && args[0] === "get" && args[1] === "karstask") {
        updated = true;
        f.task.metadata.resourceVersion = "2";
      }
      return result;
    };
    const review = await f.document(race);
    expect(updated).toBe(true);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    await applyReviewedGrant(f.execute, review);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).runtime.task.authorization).toBe(AUTHORIZATION);
  });

  it("does not promote a first-read stale Ready condition when the second read becomes current", async () => {
    const f = await setup();
    f.sandbox.status.conditions[0].observedGeneration = 0;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "get" && args[1] === "karstask") {
        f.sandbox.status.conditions[0].observedGeneration = 1;
        f.sandbox.metadata.resourceVersion = "2";
      }
      return result;
    };
    await expect(f.document(race)).rejects.toThrow();
    expect(console.error).toHaveBeenCalledWith(`${SNAPSHOT_MARKER}{"resourceVersionMatch":false,"observedGenerationMatch":true,"phaseRunningMatch":true,"readyConditionMatch":true}`);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("revalidates the same snapshot constraints during apply before any mutation", async () => {
    const f = await setup();
    const review = await f.document();
    f.calls.length = 0;
    let changed = false;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!changed && args[0] === "get" && args[1] === "karstask") {
        changed = true;
        f.sandbox.metadata.resourceVersion = "2";
        f.sandbox.spec.credentialsRef.uid = "changed-before-apply";
      }
      return result;
    };
    await expect(applyReviewedGrant(race, review)).rejects.toThrow();
    expect(changed).toBe(true);
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
    expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
  });

  it("still rejects changed shared-root authority after preview before any mutation", async () => {
    const f = await setup();
    const review = await f.document();
    f.objects.get(f.key("deployment", "kars-controller", "core")).spec.template.spec.containers[0].image = "changed-root";
    f.calls.length = 0;
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
    expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
  });

  it.each([null, false, true])("retires real owned Pod UIDs, verifies token rotation and restores suspension %s without touching shared authority", async original => {
    const f = await setup(original);
    const before = f.preserved();
    const review = await f.document();
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("controller admin-key rotation"));
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.calls.filter(args => args[1] === "secret").every(args => args.includes("jsonpath-as-json={.metadata}"))).toBe(true);
    const oldKey = f.secret.data["control-token"];
    await applyReviewedGrant(f.execute, review);
    expect(f.preserved()).toEqual(before);
    expect(f.secret.metadata.uid).toBe("admin-secret");
    expect(f.secret.data["control-token"]).not.toBe(oldKey);
    expect(f.sandbox.metadata.uid).toBe("sandbox");
    expect(f.sandbox.spec.suspended ?? null).toBe(original);
    expect(f.deployment.metadata.uid).toBe("late-deployment");
    expect(f.deployment.spec.replicas).toBe(original ? 0 : 1);
    const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(state.version).toBe(4);
    expect(state.phase).toBe("Qualified");
    expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
    expect(state.captured).toEqual(original ? [] : ["old-running", "old-terminating"]);
    expect(state.baseline.key).not.toBe(state.qualified.material.key);
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
    const activation = f.grant().spec.privateActivation;
    expect(activation.namespaces.find((s: any) => s.namespace.name === "core").epoch)
      .toBe(f.grant("second").spec.privateActivation.namespaces.find((s: any) => s.namespace.name === "core").epoch);
    const qualified = await f.document();
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, qualified);
    expect(f.calls.some(args => args[0] === "patch" && args[1] !== "karscredentialgrants.kars.azure.com")).toBe(false);
  });

  it.each(["sandbox-uid", "namespace-uid", "claim", "deployment-owner", "task-owner", "task-spec", "unobserved",
    "host", "budget-token", "other-private", "pod-owner", "pod-template", "job", "secret-provenance", "missing-secret"])(
    "refuses unsupported %s without any mutation", async fault => {
      const f = await setup();
      if (fault === "sandbox-uid") f.sandbox.metadata.uid = "replaced";
      if (fault === "namespace-uid") f.namespace.metadata.uid = "replaced";
      if (fault === "claim") delete f.namespace.metadata.annotations["kars.azure.com/namespace-claim-version"];
      if (fault === "deployment-owner") f.deployment.metadata.ownerReferences = [{ kind: "KarsSandbox" }];
      if (fault === "task-owner") f.sandbox.metadata.ownerReferences[0].uid = "replaced";
      if (fault === "task-spec") { f.task.metadata.generation++; f.task.status.observedGeneration++; }
      if (fault === "unobserved") f.sandbox.metadata.generation++;
      if (fault === "host") f.deployment.spec.template.spec.hostPID = true;
      if (fault === "budget-token") f.deployment.spec.template.spec.volumes.push({
        name: "budget", projected: { sources: [{ serviceAccountToken: { audience: "kars.azure.com/governed-inference-budget" } }] },
      });
      if (fault === "other-private") f.objects.set(f.key("secret", "router-github-app", "kars-late"), {
        metadata: { name: "router-github-app", uid: "app", resourceVersion: "1" },
      });
      if (fault === "pod-owner") f.pods.get("kars-late")![0].metadata.ownerReferences[0].uid = "replaced";
      if (fault === "pod-template") f.pods.get("kars-late")![0].spec.hostNetwork = true;
      if (fault === "job") f.pods.get("kars-late")![0].metadata.ownerReferences[0].kind = "Job";
      if (fault === "secret-provenance") delete f.secret.metadata.annotations[SOURCE];
      if (fault === "missing-secret") f.objects.delete(f.key("secret", ADMIN, "kars-late"));
      await expect(f.document()).rejects.toThrow();
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it.each(["template", "deployment-rv", "namespace-rv", "sandbox-spec"])("rejects changed %s after preview", async fault => {
    const f = await setup();
    const review = await f.document();
    if (fault === "template") f.deployment.spec.template.spec.containers[0].image = "changed";
    if (fault === "deployment-rv") f.deployment.metadata.resourceVersion = "2";
    if (fault === "namespace-rv") f.namespace.metadata.resourceVersion = "2";
    if (fault === "sandbox-spec") { f.sandbox.spec.isolation = "changed"; f.sandbox.metadata.generation++; }
    f.calls.length = 0;
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["Pausing", "suspend", "Retired", "Rotating", "Restoring", "resume", "Qualified"])(
    "resumes lost acknowledgement at %s with original identity, attempt, captured intent and epoch", async boundary => {
      const f = await setup(false);
      const before = f.preserved();
      let interrupted = false;
      const lost: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (args[0] !== "patch" || interrupted) return result;
        const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
        const phase = args[1] === "namespace" && args[2] === "kars-late"
          ? JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase : undefined;
        if (phase === boundary || (args[1] === "karssandbox"
          && ((boundary === "suspend" && patch.spec.suspended === true) || (boundary === "resume" && patch.spec.suspended === false)))) {
          interrupted = true;
          throw new Error("lost acknowledgement");
        }
        return result;
      };
      await expect(applyReviewedGrant(lost, await f.document(lost))).rejects.toThrow("lost acknowledgement");
      expect(interrupted).toBe(true);
      const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      const review = await f.document();
      await applyReviewedGrant(f.execute, review);
      const finished = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
      expect(finished.runtime.task.authorization).toBe(AUTHORIZATION);
      expect(finished.attempt).toBe(state.attempt);
      expect(finished.epoch).toBe(state.epoch ?? finished.epoch);
      expect(finished.phase).toBe("Qualified");
      expect(finished.captured).toEqual(["old-running", "old-terminating"]);
      expect(f.sandbox.spec.suspended).toBe(false);
      expect(f.preserved()).toEqual(before);
    });

  it("rejects public epoch and version changes when actual old authentication bytes were reused", async () => {
    const f = await setup();
    f.refuseRotation();
    const before = f.preserved();
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow("key was not rotated");
    expect(f.preserved()).toEqual(before);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.deployment.spec.replicas).toBe(0);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Rotating");
  });

  it("does not adopt a custom token payload or extra keys", async () => {
    for (const data of [{ "control-token": Buffer.from("custom").toString("base64") },
      { "control-token": Buffer.alloc(64, 193).toString("base64") },
      { "control-token": Buffer.from("A".repeat(64)).toString("base64"), other: "private" }]) {
      const f = await setup();
      f.secret.data = data;
      await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow("Customized or missing");
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    }
  });

  it.each(["a".repeat(64), `SHA256:${"a".repeat(64)}`, `sha256:${"A".repeat(64)}`, "sha256:",
    `sha256:${"a".repeat(63)}`, `sha256:${"a".repeat(65)}`, `${AUTHORIZATION}\n`])(
    "rejects malformed Task authorization %s even if configured identity repeats it", async authorization => {
      const f = await setup();
      f.task.status.envelopeDigest = authorization;
      const env = f.deployment.spec.template.spec.containers[0].env[0];
      env.value = JSON.stringify({ ...JSON.parse(env.value), task_authorization: authorization });
      await expect(f.document()).rejects.toThrow("Task authorization");
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it("rejects a different valid prefixed Task digest than the reviewed configured identity", async () => {
    const f = await setup();
    f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
    await expect(f.document()).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it.each(["a".repeat(64), `sha256:${"b".repeat(64)}`])(
    "does not normalize or replace a changed recovery Task authorization %s", async authorization => {
      const f = await setup();
      const stop: Execute = async (args, input) => {
        const value = await f.execute(args, input);
        if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late") throw new Error("interrupted");
        return value;
      };
      await expect(applyReviewedGrant(stop, await f.document(stop))).rejects.toThrow("interrupted");
      const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
      state.runtime.task.authorization = authorization;
      f.namespace.metadata.annotations[HISTORY] = canonical(state);
      f.calls.length = 0;
      await expect(f.document()).rejects.toThrow();
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    });

  it("keeps terminating consumers suspended and does not request new authority until actual retirement", async () => {
    const f = await setup();
    f.keepPods();
    const token = f.secret.data["control-token"];
    const delayed: Execute = async (args, input) => {
      const value = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployments.apps") {
        const elapsed = Date.now() + 121_000;
        vi.spyOn(Date, "now").mockReturnValue(elapsed);
      }
      return value;
    };
    await expect(applyReviewedGrant(delayed, await f.document(delayed))).rejects.toThrow("including terminating UIDs");
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.secret.data["control-token"]).toBe(token);
    expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    expect(f.pods.get("kars-late")).toHaveLength(2);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).captured).toEqual(["old-running", "old-terminating"]);
    vi.mocked(Date.now).mockRestore();
    f.pods.set("kars-late", []);
    await applyReviewedGrant(f.execute, await f.document());
    expect(f.secret.data["control-token"]).not.toBe(token);
  });

  it.each(["karssandbox", "deployments.apps"])("does not overwrite a concurrent %s resourceVersion and safely resumes its original receipt", async kind => {
    const f = await setup();
    const value = kind === "karssandbox" ? f.sandbox : f.deployment;
    let conflicted = false;
    const conflict: Execute = async (args, input) => {
      if (!conflicted && args[0] === "patch" && args[1] === kind) {
        conflicted = true;
        value.metadata.resourceVersion = String(Number(value.metadata.resourceVersion) + 1);
      }
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(conflict, await f.document(conflict))).rejects.toThrow();
    expect(conflicted).toBe(true);
    const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(state.phase).toBe("Pausing");
    expect(f.secret.data["control-token"]).toBe(Buffer.from("A".repeat(64)).toString("base64"));
    await applyReviewedGrant(f.execute, await f.document());
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).attempt).toBe(state.attempt);
  });

  it("rejects a key minted before the recorded retirement boundary instead of blessing its bytes", async () => {
    const f = await setup();
    const premature: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployments.apps") {
        f.secret.data["control-token"] = Buffer.from("C".repeat(64)).toString("base64");
        f.secret.metadata.resourceVersion = "2";
      }
      return result;
    };
    await expect(applyReviewedGrant(premature, await f.document(premature))).rejects.toThrow("key changed before retirement");
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Pausing");
  });

  it.each(["spec", "task", "uid", "receipt", "provenance", "new-pod"])("preserves suspension on changed recovery %s", async fault => {
    const f = await setup();
    const stop: Execute = async (args, input) => {
      const value = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late"
        && JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase === "Rotating") throw new Error("interrupted");
      return value;
    };
    await expect(applyReviewedGrant(stop, await f.document(stop))).rejects.toThrow("interrupted");
    if (fault === "spec") f.sandbox.spec.isolation = "changed";
    if (fault === "task") f.task.spec.objective = "changed";
    if (fault === "uid") f.sandbox.metadata.uid = "changed";
    if (fault === "receipt") delete f.namespace.metadata.annotations[HISTORY];
    if (fault === "provenance") delete f.secret.metadata.annotations[SOURCE];
    if (fault === "new-pod") f.pods.set("kars-late", [{ kind: "Pod", metadata: { name: "new", uid: "new", resourceVersion: "1" }, spec: { containers: [] } }]);
    f.calls.length = 0;
    await expect(f.document()).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.deployment.spec.replicas).toBe(0);
  });
});
