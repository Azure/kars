// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { PRIVATE_PREFIX as P, record, templateDigest, type Execute } from "./private-activation.js";
import { projectObserver } from "./private-activation-observer.js";
import { reviewedControllerRollout } from "./private-activation-reconciliation.js";

function fixture(rolloutField = "kars.azure.com/inference-providers-version", legacyProjection = false) {
  const epoch = "a".repeat(64);
  const identity = { name: "sre", uid: "deployment-uid", resourceVersion: "1" };
  const current = { kind: "Deployment", apiVersion: "apps/v1", metadata: { ...identity, namespace: "kars-sre" },
    spec: { template: { metadata: { labels: { app: "sre" }, annotations: {
      [`${P}epoch`]: epoch, [rolloutField]: "one",
    } }, spec: { containers: [{ name: "router", image: "example.test/router:latest" }] } } } };
  const structure = structuredClone(current);
  if (!legacyProjection) delete structure.spec.template.metadata.annotations[rolloutField];
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

const OBSERVER = "kars.azure.com/services-observer-version";

function observerFixture() {
  const f = fixture();
  const volumes = [{ name: "existing", emptyDir: {} }];
  const env = [{ name: "EXISTING", value: "fixed" }];
  const volumeMounts = [{ name: "existing", mountPath: "/existing" }];
  Object.assign(f.current.spec.template.spec, { volumes, containers: [
    { name: "agent", image: "reviewed-agent" }, { name: "inference-router", image: "reviewed-router", env, volumeMounts },
  ] });
  const baseline = structuredClone(f.current);
  f.consumer.templateDigest = templateDigest(baseline);
  f.receipt.baseline = f.consumer.templateDigest;
  const normalized = structuredClone(baseline);
  delete normalized.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"];
  f.receipt.structure = templateDigest(normalized);
  f.current.spec.template.metadata.annotations[OBSERVER] = "observer:1";
  Object.assign(volumes, [volumes[0],
    { name: "service-observations", secret: { secretName: "router-services-observer", defaultMode: 420,
      items: [{ key: "observation-token", path: "observation-token" }, { key: "config.json", path: "config.json" }] } },
    { name: "service-observation-identity", secret: { secretName: "router-services-observer-identity", defaultMode: 420,
      items: [{ key: "config.json", path: "config.json" }] } },
  ]);
  env.push({ name: "KARS_SERVICE_OBSERVATION_VERSION", value: "observer:1" });
  Object.assign(volumeMounts, [volumeMounts[0],
    { name: "service-observations", mountPath: "/etc/kars/observations", readOnly: true },
    { name: "service-observation-identity", mountPath: "/etc/kars/observation-identity", readOnly: true },
  ]);
  const publish = () => {
    f.receipt.template = templateDigest(f.current);
    f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
  };
  return { ...f, baseline, volumes, env, volumeMounts, publish };
}

describe("protected controller rollout receipts", () => {
  it("requires an exact receipt for observer enable, rotation and removal", async () => {
    const f = observerFixture();
    for (const version of ["observer:1", "observer:2"]) {
      f.current.spec.template.metadata.annotations[OBSERVER] = version;
      f.env[1]!.value = version;
      expect(await f.verify()).toBe(false);
      f.publish();
      expect(await f.verify()).toBe(true);
      const projected = structuredClone(f.current.spec.template);
      expect(projectObserver(projected)).toBe(version);
      expect(projected).toEqual(f.baseline.spec.template);
    }
    Object.assign(f.current, structuredClone(f.baseline));
    expect(await f.verify()).toBe(false);
    f.publish();
    expect(await f.verify()).toBe(true);
    expect(f.calls.every(args => ["namespace", "karssandbox"].includes(args[1]!))).toBe(true);
  });

  it.each([true, false])("reads observer-pinned legacy receipts with credential projection %s", async legacyProjection => {
    const f = observerFixture();
    const pinned = structuredClone(f.current);
    if (!legacyProjection) delete pinned.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"];
    else {
      // Legacy projection receipts still remove provider-version, but retain credential-projection-version.
      pinned.spec.template.metadata.annotations["kars.azure.com/credential-projection-version"] = "projection:1";
      f.current.spec.template.metadata.annotations["kars.azure.com/credential-projection-version"] = "projection:1";
      delete pinned.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"];
    }
    f.receipt.structure = templateDigest(pinned);
    Object.assign(f.receipt, { model_structure: f.receipt.structure });
    f.publish();
    expect(await f.verify()).toBe(true);
    f.current.spec.template.metadata.annotations[OBSERVER] = "observer:2";
    f.env[1]!.value = "observer:2";
    f.publish();
    expect(await f.verify()).toBe(false);
  });

  it.each(["secret", "items", "mode", "mount", "env", "duplicate", "optional", "null", "image", "agent-mount"])(
    "rejects observer-looking structural drift even with updated full digest: %s", async fault => {
      const f = observerFixture();
      if (fault === "secret") record(record(f.volumes[1]).secret).secretName = "other";
      if (fault === "items") record(record(f.volumes[1]).secret).items = [];
      if (fault === "mode") record(record(f.volumes[1]).secret).defaultMode = 511;
      if (fault === "optional") record(record(f.volumes[1]).secret).optional = true;
      if (fault === "mount") Object.assign(f.volumeMounts[1]!, { readOnly: false });
      if (fault === "env") Object.assign(f.env[1]!, { valueFrom: { fieldRef: { fieldPath: "metadata.name" } } });
      if (fault === "duplicate") f.volumes.push(f.volumes[1]!);
      if (fault === "null") Object.assign(f.current.spec.template.metadata.annotations, { [OBSERVER]: null });
      if (fault === "image") f.current.spec.template.spec.containers[0]!.image = "changed";
      if (fault === "agent-mount") Object.assign(f.current.spec.template.spec.containers[0]!, { volumeMounts: [f.volumeMounts[1]] });
      f.publish();
      expect(await f.verify().catch(() => false)).toBe(false);
      if (!["image", "agent-mount"].includes(fault)) {
        expect(() => projectObserver(structuredClone(f.current.spec.template))).toThrow("Malformed observer projection");
      }
    },
  );

  it("never treats retained review history as observer issuance authority", async () => {
    const f = observerFixture();
    delete f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`];
    const replica = { ...f.baseline, kind: "ReplicaSet", metadata: { namespace: "kars-sre", ownerReferences: [
      { apiVersion: "apps/v1", kind: "Deployment", name: "sre", uid: f.consumer.object.uid, controller: true },
    ] } };
    const execute: Execute = async args => JSON.stringify(args[1] === "namespace" ? f.namespace
      : args[1] === "karssandbox" ? f.sandbox : { items: [replica] });
    expect(await reviewedControllerRollout(execute, f.scope, f.consumer, f.current, true)).toBe(false);
    Object.assign(f.current, structuredClone(f.baseline));
    expect(await reviewedControllerRollout(execute, f.scope, f.consumer, f.current, true)).toBe(true);
  });
  it.each(["kars.azure.com/inference-providers-version", "kars.azure.com/credential-projection-version"])(
    "accepts repeated %s rotations only with a current receipt after ReplicaSets are collected", async field => {
      const f = fixture(field);
      for (let n = 0; n < 15; n++) {
        f.current.spec.template.metadata.annotations[field] = `projection-uid:${n + 2}`;
        expect(await f.verify()).toBe(false);
        f.receipt.template = templateDigest(f.current);
        f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
        expect(await f.verify()).toBe(true);
      }
      expect(f.consumer.templateDigest).toBe("b".repeat(64));
      expect(f.calls.every(args => ["namespace", "karssandbox"].includes(args[1]!))).toBe(true);
      Object.assign(f.current.spec.template.metadata.annotations, { [field]: { invalid: true } });
      await expect(f.verify()).rejects.toThrow("Malformed credential rollout annotation");
    },
  );

  it.each([false, true])("preserves existing projection-pinned receipts with model binding: %s", async modelBinding => {
    const field = "kars.azure.com/credential-projection-version";
    const f = fixture(field, true);
    if (modelBinding) Object.assign(f.receipt, { model_structure: f.receipt.structure });
    f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
    expect(await f.verify()).toBe(true);
    f.current.spec.template.metadata.annotations[field] = "projection-uid:2";
    f.receipt.template = templateDigest(f.current);
    f.namespace.metadata.annotations[`${P}reconciled-${f.consumer.object.uid}`] = JSON.stringify(f.receipt);
    expect(await f.verify()).toBe(false);
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
