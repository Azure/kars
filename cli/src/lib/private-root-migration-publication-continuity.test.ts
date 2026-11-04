// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { publishPrivateCore } from "./private-core-publication.js";
import { type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION, CORE_PUBLICATION_FENCE } from "./private-activation.js";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { PRIVATE_PREFIX as P, type Execute } from "./private-activation.js";
import { finalizeRootMigration } from "./private-root-migration.js";
import {
  witnessPublicationFixture, publicationFixture, failedPublicationFixture,
} from "./private-root-migration.publication.test-support.js";

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
