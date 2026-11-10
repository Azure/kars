// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { rootCertificates } from "node:tls";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  canonical, PRIVATE_PREFIX as P, templateDigest, validateQualifiedActivation, type Execute,
} from "./private-activation.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import { assertRootImageDelta } from "./private-root-image-delta.js";
import { finalizeRootMigration, prepareRootMigration, retireRootMigration } from "./private-root-migration.js";
import { alreadyRolled, TARGET, ORIGINAL, setup, FIELD } from "./private-root-migration.test-support.js";

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
});
