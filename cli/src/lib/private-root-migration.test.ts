// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  at, canonical, digest, PRIVATE_PREFIX as P, templateDigest, validateQualifiedActivation, type Execute,
} from "./private-activation.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import {
  finalizeRootMigration, prepareRootMigration, prepareRouterBaselineRestart, retireRootMigration,
  reviewCompletedRootMigration,
} from "./private-root-migration.js";
import { setup, ORIGINAL, TARGET, FIELD } from "./private-root-migration.test-support.js";

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
