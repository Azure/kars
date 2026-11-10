// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { rootCertificates } from "node:tls";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { PRIVATE_PREFIX as P, type Execute } from "./private-activation.js";
import { rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import {
  finalizeRootMigration, prepareRootMigration, retireRootMigration, type RootMigrationReceipt,
} from "./private-root-migration.js";
import { setup, ORIGINAL, TARGET, GRANTS, FIELD } from "./private-root-migration.test-support.js";

describe("single-root image migration", () => {


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
