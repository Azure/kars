// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { canonical, PRIVATE_PREFIX as P, previewPrivateActivation, type Execute } from "./private-activation.js";
import { continuityFixture, rootPod } from "./private-activation-fixtures.js";
import { applyRootImageRecovery, previewRootImageRecovery } from "./private-root-image-recovery.js";

const ORIGINAL = `example.test/controller:latest@sha256:${"1".repeat(64)}`;
const CHANGED = `example.test/controller:latest@sha256:${"2".repeat(64)}`;

async function setup() {
  const f = continuityFixture();
  const root = f.deployment;
  root.spec.template.spec.containers[0]!.image = ORIGINAL;
  f.objects.set(f.key("serviceaccount", "bff", "core"), {
    metadata: { name: "bff", namespace: "core", uid: "core-writer", resourceVersion: "1" },
  });
  f.pods.set("core", [rootPod(f)]);
  const activation = await previewPrivateActivation(f.execute, "core", [{ namespace: "core" }], [],
    "core", "kcm-certificate", []);
  await applyReviewedGrant(f.execute, {
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
    metadata: { name: "workspace", namespace: "core" },
    spec: { workspaceUid: "core-uid", enabled: true,
      writers: [{ namespace: "core", name: "bff", uid: "core-writer" }], privateActivation: activation },
  });
  root.spec.template.spec.containers[0]!.image = CHANGED;
  f.pods.set("core", [rootPod(f, "changed-root")]);
  const mutations: { args: string[]; input?: string }[] = [];
  const execute: Execute = async (args, input) => {
    if (args[0] === "get" && args[1] === "namespaces") {
      return JSON.stringify({ metadata: {}, items: [...f.objects.entries()]
        .filter(([key]) => key.startsWith("namespace/")).map(([, value]) => value) });
    }
    if (args[0] === "patch" && args.includes("--type=json")) {
      mutations.push({ args, ...(input ? { input } : {}) });
      const patch = JSON.parse(input!);
      if (args[1] === "namespace") {
        const ns = f.namespace("core");
        expect(patch.slice(0, 3)).toEqual([
          { op: "test", path: "/metadata/uid", value: ns.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: ns.metadata.resourceVersion },
          { op: "test", path: "/metadata/annotations", value: ns.metadata.annotations },
        ]);
        expect(patch[3]).toEqual({
          op: "replace", path: "/metadata/annotations/kars.azure.com~1private-state", value: "Qualified",
        });
        ns.metadata.annotations[`${P}state`] = "Qualified";
        ns.metadata.resourceVersion = String(Number(ns.metadata.resourceVersion) + 1);
        return JSON.stringify(ns);
      }
      expect(patch.slice(0, 3)).toEqual([
        { op: "test", path: "/metadata/uid", value: root.metadata.uid },
        { op: "test", path: "/metadata/resourceVersion", value: root.metadata.resourceVersion },
        { op: "test", path: "/spec", value: root.spec },
      ]);
      const projected = structuredClone(root);
      for (const operation of patch.slice(3)) {
        if (operation.path === "/spec/template/spec/containers/0/image") {
          projected.spec.template.spec.containers[0]!.image = operation.value;
        } else {
          expect(operation.path).toBe("/spec/replicas");
          projected.spec.replicas = operation.value;
        }
      }
      if (args.includes("--dry-run=server")) return JSON.stringify(projected);
      root.spec = projected.spec;
      root.metadata.resourceVersion = String(Number(root.metadata.resourceVersion) + 1);
      root.metadata.generation++;
      root.status = { observedGeneration: root.metadata.generation,
        updatedReplicas: root.spec.replicas, availableReplicas: root.spec.replicas };
      return JSON.stringify(root);
    }
    if (args[0] === "rollout") {
      if (f.namespace("core").metadata.annotations[`${P}state`] === "Pending" && root.spec.replicas) {
        throw new Error("Native admission denies startup while Pending");
      }
      f.pods.set("core", root.spec.replicas ? [rootPod(f, "recovered-root")] : []);
      return "successfully rolled out";
    }
    if (args[0] === "wait") {
      if (!root.spec.replicas) f.pods.set("core", []);
      return "deleted";
    }
    if (!["get", "auth"].includes(args[0]!)) throw new Error("Unexpected recovery mutation");
    return f.execute(args, input);
  };
  f.calls.length = 0;
  return { ...f, root, execute, mutations };
}

describe("sealed root image-only recovery", () => {
  it("restores only the previously qualified image and verifies unchanged actual continuity", async () => {
    const f = await setup();
    const original = canonical({ namespace: f.namespace("core"), grant: f.grant("core") });
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    expect(f.mutations).toEqual([]);
    expect(review.observedImage).toBe(CHANGED);
    expect(review.image).toBe(ORIGINAL);
    await applyRootImageRecovery(f.execute, review);
    expect(f.root.spec.template.spec.containers[0]!.image).toBe(ORIGINAL);
    expect(canonical({ namespace: f.namespace("core"), grant: f.grant("core") })).toBe(original);
    expect(f.mutations).toHaveLength(2);
    expect(f.mutations[0]!.args).toContain("--dry-run=server");
    expect(f.mutations[1]!.args).not.toContain("--dry-run=server");
  });

  it.each(["controller:latest", CHANGED, `wrong@sha256:${"3".repeat(64)}`])(
    "refuses an image that cannot restore the sealed template: %s", async candidate => {
      const f = await setup();
      await expect(previewRootImageRecovery(f.execute, "core", candidate)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("retires Pending root execution before releasing startup and verifies actual restored continuity", async () => {
    const f = await setup();
    const fields = structuredClone(f.namespace("core").metadata.annotations);
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    await applyRootImageRecovery(f.execute, review);
    expect(f.namespace("core").metadata.annotations).toEqual(fields);
    expect(f.mutations).toHaveLength(4);
    expect(f.mutations[2]!.args[1]).toBe("namespace");
    expect(JSON.parse(f.mutations[1]!.input!)[4]).toEqual({ op: "replace", path: "/spec/replicas", value: 0 });
    expect(JSON.parse(f.mutations[3]!.input!)[3]).toEqual({ op: "replace", path: "/spec/replicas", value: 1 });
  });

  it("does not release Pending protection when old execution retirement or restored continuity fails", async () => {
    for (const fault of ["wait", "template", "grant", "late-scope", "seal", "state-cas"]) {
      const f = await setup();
      f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
      const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
      const execute: Execute = async (args, input) => {
        if (args[0] === "wait" && fault === "wait") throw new Error("Retirement timed out");
        if (args[0] === "patch" && args[1] === "namespace" && fault === "state-cas") throw new Error("State CAS refused");
        const result = await f.execute(args, input);
        if (args[0] === "wait") {
          if (fault === "template") f.root.spec.template.spec.containers[0]!.command = ["unexpected"];
          if (fault === "grant") f.grant("core").spec.agentKeys = ["CHANGED_TOKEN"];
          if (fault === "late-scope") f.namespace("second").metadata.annotations = { [`${P}root-namespace`]: "core" };
          if (fault === "seal") f.namespace("core").metadata.annotations[`${P}root-retirement`] = "{}";
        }
        return result;
      };
      await expect(applyRootImageRecovery(execute, review)).rejects.toThrow();
      expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
      expect(f.mutations).toHaveLength(2);
    }
  });

  it("can resume interrupted quarantine restoration without another image mutation", async () => {
    const f = await setup();
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    await expect(applyRootImageRecovery(async (args, input) => {
      if (args[0] === "patch" && args[1] === "namespace") throw new Error("State CAS refused");
      return f.execute(args, input);
    }, review)).rejects.toThrow("State CAS");
    expect(f.root.spec.replicas).toBe(0);
    const next = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    expect(next.observedImage).toBe(ORIGINAL);
    await applyRootImageRecovery(f.execute, next);
    expect(f.root.spec.replicas).toBe(1);
    expect(f.mutations).toHaveLength(4);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Qualified");
  });

  it("recovers a Pending interrupted image change with old image-only predecessor Pods still running", async () => {
    const f = await setup();
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    f.root.spec.template.spec.containers[0]!.image = ORIGINAL;
    const next = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    await applyRootImageRecovery(f.execute, next);
    expect(f.root.spec.replicas).toBe(1);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Qualified");
  });

  it("resumes after startup permission was restored but replica restoration was interrupted", async () => {
    const f = await setup();
    f.namespace("core").metadata.annotations[`${P}state`] = "Pending";
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    await expect(applyRootImageRecovery(async (args, input) => {
      if (args[0] === "patch" && args[1] === "deployment" && !args.includes("--dry-run=server")
        && JSON.parse(input!).at(-1).path === "/spec/replicas" && JSON.parse(input!).at(-1).value === 1) {
        throw new Error("Replica restore CAS refused");
      }
      return f.execute(args, input);
    }, review)).rejects.toThrow("Replica restore CAS");
    expect(f.root.spec.replicas).toBe(0);
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Qualified");
    await applyRootImageRecovery(f.execute, await previewRootImageRecovery(f.execute, "core", ORIGINAL));
    expect(f.root.spec.replicas).toBe(1);
    expect(f.mutations).toHaveLength(4);
  });

  it.each(["Retired", "Stamping", "", "unknown"])("refuses non-recoverable state %s", async state => {
    const f = await setup();
    f.namespace("core").metadata.annotations[`${P}state`] = state;
    await expect(previewRootImageRecovery(f.execute, "core", ORIGINAL)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });
  it.each(["root", "namespace", "account", "command", "replicas", "seal", "epoch", "pod", "late-scope"])(
    "refuses unrelated %s changes without modifying authority", async fault => {
      const f = await setup();
      if (fault === "root") f.root.metadata.uid = "replacement";
      if (fault === "namespace") f.namespace("core").metadata.uid = "replacement";
      if (fault === "account") f.objects.get(f.key("serviceaccount", "kars-controller", "core")).metadata.uid = "replacement";
      if (fault === "command") f.root.spec.template.spec.containers[0]!.command = ["different"];
      if (fault === "replicas") f.root.spec.replicas = 2;
      if (fault === "seal") f.namespace("core").metadata.annotations[`${P}root-retirement`] = "{}";
      if (fault === "epoch") f.namespace("core").metadata.annotations[`${P}epoch`] = "changed";
      if (fault === "pod") f.pods.get("core")![0].metadata.ownerReferences[0].uid = "foreign-rs";
      if (fault === "late-scope") f.namespace("second").metadata.annotations = {
        [`${P}root-namespace`]: "core", [`${P}root-namespace-uid`]: "core-uid",
      };
      await expect(previewRootImageRecovery(f.execute, "core", ORIGINAL)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it.each(["namespaces", "pods", "karscredentialgrants.kars.azure.com"])(
    "refuses truncated %s inventory", async resource => {
      const f = await setup();
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (args[0] === "get" && args[1] === resource && args.includes("--chunk-size=0")) {
          return JSON.stringify({ ...JSON.parse(result), metadata: { continue: "more" } });
        }
        return result;
      };
      await expect(previewRootImageRecovery(execute, "core", ORIGINAL)).rejects.toThrow("inventory");
      expect(f.mutations).toEqual([]);
    },
  );

  it("requires operator authority and a still-current exact review", async () => {
    const f = await setup();
    await expect(previewRootImageRecovery(async (args, input) =>
      args[0] === "auth" ? "no" : f.execute(args, input), "core", ORIGINAL)).rejects.toThrow("operator");
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    f.grant("core").spec.agentKeys = ["CHANGED_TOKEN"];
    await expect(applyRootImageRecovery(f.execute, review)).rejects.toThrow("changed");
    expect(f.mutations).toEqual([]);
  });

  it("rechecks grant authority after server projection before any real mutation", async () => {
    const f = await setup();
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args.includes("--dry-run=server")) f.grant("core").spec.agentKeys = ["CHANGED_TOKEN"];
      return result;
    };
    await expect(applyRootImageRecovery(execute, review)).rejects.toThrow("changed");
    expect(f.mutations).toHaveLength(1);
    expect(f.root.spec.template.spec.containers[0]!.image).toBe(CHANGED);
  });

  it("rejects unexpected server projection and propagates a compare-and-swap refusal", async () => {
    const f = await setup();
    const review = await previewRootImageRecovery(f.execute, "core", ORIGINAL);
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args.includes("--dry-run=server")) {
        const projected = JSON.parse(result);
        projected.spec.replicas = 2;
        return JSON.stringify(projected);
      }
      return result;
    };
    await expect(applyRootImageRecovery(execute, review)).rejects.toThrow("projection");
    await expect(applyRootImageRecovery(async (args, input) => {
      if (args[0] === "patch" && !args.includes("--dry-run=server")) throw new Error("Kubernetes status 409");
      return f.execute(args, input);
    }, review)).rejects.toThrow("409");
    expect(f.root.spec.template.spec.containers[0]!.image).toBe(CHANGED);
  });

  it("registers preview/apply without adding an ordinary upgrade bypass", () => {
    const recovery = credentialGrantsCommand().commands.find(command => command.name() === "recover-root-image");
    expect(recovery?.commands.map(command => command.name())).toEqual(["preview", "apply"]);
    expect(recovery?.commands[0]?.options.filter(option => option.mandatory).map(option => option.long))
      .toEqual(["--private-root", "--image"]);
  });
});
