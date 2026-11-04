// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { PRIVATE_PREFIX as P, templateDigest, type Execute } from "./private-activation.js";
import { reviewedControllerRollout } from "./private-activation-reconciliation.js";

function fixture() {
  const epoch = "a".repeat(64);
  const identity = { name: "sre", uid: "deployment-uid", resourceVersion: "1" };
  const current = { kind: "Deployment", apiVersion: "apps/v1", metadata: { ...identity, namespace: "kars-sre" },
    spec: { template: { metadata: { labels: { app: "sre" }, annotations: {
      [`${P}epoch`]: epoch, "kars.azure.com/inference-providers-version": "one",
    } }, spec: { containers: [{ name: "router", image: "example.test/router:latest" }] } } } };
  const structure = structuredClone(current);
  delete (structure.spec.template.metadata.annotations as Record<string, string>)["kars.azure.com/inference-providers-version"];
  const consumer = { kind: "Deployment", object: identity, templateDigest: "b".repeat(64) };
  const scope = { namespace: { name: "kars-sre", uid: "namespace-uid", resourceVersion: "1" }, epoch, consumers: [consumer] };
  const receipt = { epoch, baseline: consumer.templateDigest, structure: templateDigest(structure), template: templateDigest(current),
    sandbox_uid: "sandbox-uid", generation: 2 };
  const namespace = { metadata: { ...scope.namespace, annotations: {
    [`${P}epoch`]: epoch, [`${P}parent-${identity.uid}`]: epoch,
    "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/sandbox-name": "sre",
    "kars.azure.com/sandbox-namespace": "core", "kars.azure.com/sandbox-uid": "sandbox-uid",
    [`${P}reconciled-${identity.uid}`]: JSON.stringify(receipt),
  } } };
  const sandbox = { metadata: { name: "sre", namespace: "core", uid: "sandbox-uid", resourceVersion: "1", generation: 2,
    annotations: { "kars.azure.com/namespace-uid": "namespace-uid" } } };
  const calls: string[][] = [];
  const execute: Execute = async args => {
    calls.push(args);
    if (args[0] !== "get") throw new Error("Read-only review required");
    if (args[1] === "namespace") return JSON.stringify(namespace);
    if (args[1] === "karssandbox") return JSON.stringify(sandbox);
    throw new Error("No retained ReplicaSets");
  };
  const verify = () => reviewedControllerRollout(execute, scope, consumer, current);
  return { current, consumer, scope, receipt, namespace, sandbox, calls, verify };
}

describe("protected controller rollout receipts", () => {
  it("accepts repeated rotations after ReplicaSets are collected without changing the original qualification", async () => {
    const f = fixture();
    for (let n = 0; n < 15; n++) {
      f.current.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"] = `rotation-${n}`;
      f.receipt.template = templateDigest(f.current);
      f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
      expect(await f.verify()).toBe(true);
    }
    expect(f.consumer.templateDigest).toBe("b".repeat(64));
    expect(f.calls.every(args => ["namespace", "karssandbox"].includes(args[1]!))).toBe(true);
  });

  it.each(["agent", "openclaw"])("verifies extended %s model receipts and rejects malformed projections", async runtime => {
    for (const fault of ["none", "digest", "null", "duplicate", "valueFrom", "extra-field", "other-env"]) {
      const f = fixture();
      const containers = [
        { name: runtime, image: "reviewed-agent", env: [{ name: "KARS_MODEL", value: "new" }] },
        { name: "inference-router", image: "reviewed-router", env: [{ name: "AZURE_OPENAI_DEPLOYMENT", value: "new" }, { name: "DEFAULT_MODEL", value: "fixed" }] },
      ];
      if (runtime === "openclaw") containers[0]!.env.push({ name: "OPENCLAW_MODEL", value: "new" });
      Object.assign(f.current.spec.template.spec, { containers });
      const normalized = structuredClone(f.current);
      const projected = structuredClone(containers);
      for (const container of projected) for (const entry of container.env) {
        if (entry.name !== "DEFAULT_MODEL") entry.value = "controller-policy-model";
      }
      Object.assign(normalized.spec.template.spec, { containers: projected });
      delete (normalized.spec.template.metadata.annotations as Record<string, string>)["kars.azure.com/inference-providers-version"];
      Object.assign(f.receipt, { model_structure: templateDigest(normalized) });
      if (fault === "digest") Object.assign(f.receipt, { model_structure: "changed" });
      if (fault === "null") Object.assign(f.receipt, { model_structure: null });
      if (fault === "duplicate") containers[0]!.env.push({ ...containers[0]!.env[0]! });
      if (fault === "valueFrom") Object.assign(containers[0]!.env[0]!, { valueFrom: { fieldRef: { fieldPath: "metadata.name" } } });
      if (fault === "extra-field") Object.assign(containers[0]!.env[0]!, { unexpected: true });
      if (fault === "other-env") containers[1]!.env[1]!.value = "changed";
      const structure = structuredClone(f.current);
      delete (structure.spec.template.metadata.annotations as Record<string, string>)["kars.azure.com/inference-providers-version"];
      f.receipt.structure = templateDigest(structure);
      f.receipt.template = templateDigest(f.current);
      f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
      if (["duplicate", "valueFrom"].includes(fault)) await expect(f.verify()).rejects.toThrow("Malformed model rollout");
      else expect(await f.verify(), `${runtime}/${fault}`).toBe(fault === "none");
    }
  });

  it.each(["baseline", "structure", "template", "epoch", "sandbox_uid", "generation", "unknown", "missing", "image", "namespace", "sandbox", "source-generation", "parent", "deployment"])(
    "rejects stale or altered authority: %s", async fault => {
      const f = fixture();
      if (["baseline", "structure", "template", "epoch", "sandbox_uid"].includes(fault)) Object.assign(f.receipt, { [fault]: "changed" });
      if (fault === "generation") f.receipt.generation = 1;
      if (fault === "unknown") Object.assign(f.receipt, { ignored: true });
      if (fault === "image") f.current.spec.template.spec.containers[0]!.image = "changed";
      if (fault === "namespace") f.namespace.metadata.uid = "replaced";
      if (fault === "sandbox") f.sandbox.metadata.uid = "replaced";
      if (fault === "source-generation") f.sandbox.metadata.generation++;
      if (fault === "parent") f.namespace.metadata.annotations[`${P}parent-${f.consumer.object.uid}`] = "changed";
      if (fault === "deployment") f.current.metadata.uid = "replaced";
      f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
      if (fault === "missing") delete f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`];
      expect(await f.verify()).toBe(false);
    },
  );
});
