// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { rootCertificates } from "node:tls";
import { chmodSync, copyFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { publishPrivateCore } from "./private-core-publication.js";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION, CORE_PUBLICATION_FENCE } from "./private-activation.js";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { canonical } from "./private-activation.js";
import { finalizeRootMigration } from "./private-root-migration.js";
import { witnessPublicationFixture, publicationFixture } from "./private-root-migration.publication.test-support.js";

describe("owned protected full-Core publication", () => {


  it.each(["writer-uid", "writer-spec", "writer-replicas", "writer-owner", "writer-pods", "writer-hpa",
    "evidence-uid", "evidence-fence", "evidence-version", "evidence-owner", "missing-fence", "publishedSpec", "restored", "future-writer"] as const)(
    "refuses preparing request replacement with %s witness drift before journaling", async fault => {
      const f = await witnessPublicationFixture();
      try {
        Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
        f.state.evidenceAckLost = true;
        await expect(f.publish()).rejects.toThrow();
        const before = await f.current();
        if (fault === "writer-uid") f.writer.metadata.uid = "replacement";
        if (fault === "writer-spec") f.writer.spec.template.spec.containers[0].image = "changed";
        if (fault === "writer-replicas") f.writer.spec.replicas = 1;
        if (fault === "writer-owner") f.writer.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
        if (fault === "evidence-uid") f.evidence.metadata.uid = "replacement";
        if (fault === "evidence-fence") f.evidence.metadata.annotations[CORE_PUBLICATION_FENCE] = "foreign:witness-paused";
        if (fault === "evidence-version") f.evidence.metadata.resourceVersion = before.publication!.witnessWriter!.evidenceFence!.resourceVersion;
        if (fault === "evidence-owner") f.evidence.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
        if (["missing-fence", "publishedSpec", "restored", "future-writer"].includes(fault)) {
          await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
            if (fault === "missing-fence") delete receipt!.publication!.witnessWriter!.evidenceFence;
            else Object.assign(receipt!.publication!.witnessWriter!, { [fault]: null });
            await journal.save(receipt!);
          });
        }
        const execute: SchemaExecute = async (tool, args, settings) => {
          if (tool === "kubectl" && args[0] === "get" && args.includes("custom-witness")) {
            if (fault === "writer-pods" && args[1] === "pods") return { stdout: JSON.stringify({ metadata: {}, items: [{ metadata: { name: "still-running" } }] }) };
            if (fault === "writer-hpa" && args[1] === "horizontalpodautoscalers") return { stdout: JSON.stringify({ metadata: {}, items: [{ spec: { scaleTargetRef: { kind: "Deployment", name: "aggregator" } } }] }) };
          }
          return f.execute(tool, args, settings);
        };
        const evidence = readFileSync(f.request.receipt, "utf8"), scales = [...f.state.writes];
        f.controls.writes = 0;
        await expect(publishPrivateCore(execute, { ...f.request, values: ["repaired-model-values.yaml"], replacePreparingRequest: true })).rejects.toThrow();
        expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
        expect(f.state.writes).toEqual(scales);
        expect(f.controls.writes).toBe(0);
        expect(f.controls.launches).toBe(0);
      } finally { f.cleanup(); }
    });

  it.each(["first", "unchanged", "context", "release", "namespace", "rotation", "storage-missing",
    "artifact", "digest", "resources", "revision", "adoption", "future-outcome"] as const)("refuses preparing request replacement with %s before journaling", async fault => {
    const f = await publicationFixture();
    try {
      if (fault !== "first") {
        f.controls.preflightFailure = true;
        await expect(f.publish()).rejects.toThrow();
      }
      const request = { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true };
      if (fault === "unchanged") request.chart = f.request.chart;
      if (["context", "release", "namespace"].includes(fault)) Object.assign(request, { [fault]: "other" });
      if (["rotation", "storage-missing", "artifact", "digest", "resources", "revision", "adoption", "future-outcome"].includes(fault)) {
        await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
          if (fault === "rotation") receipt!.phase = "complete";
          else if (fault === "storage-missing") delete receipt!.publication!.storage;
          else Object.assign(receipt!.publication!, { [fault]: fault === "resources" ? [] : null });
          await journal.save(receipt!);
        });
      }
      const evidence = readFileSync(f.request.receipt, "utf8");
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, request)).rejects.toThrow("Request replacement requires");
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["prepared", "publishing", "succeeded", "verified"] as const)("refuses preparing request replacement after %s", async phase => {
    const f = await publicationFixture();
    try {
      if (phase === "prepared") f.controls.preparedInterruption = true;
      if (phase === "publishing") f.controls.launchFailure = true;
      if (phase === "succeeded") f.controls.afterSuccessFailure = true;
      if (phase === "verified") await f.publish();
      else await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe(phase);
      const evidence = readFileSync(f.request.receipt, "utf8"), launches = f.controls.launches;
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true })).rejects.toThrow();
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(launches);
    } finally { f.cleanup(); }
  });

  it.each(["storage-uid", "storage-content", "storage-status", "recorded-marker", "missing-marker", "foreign-marker", "launch-marker",
    "root", "companion", "root-pod", "ca", "unrotated", "writers"] as const)("refuses preparing request replacement with stale %s fencing", async fault => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      f.controls.preflightFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (fault === "storage-uid") f.history[0].metadata.uid = "replaced";
      if (fault === "storage-content") f.history[0].data.release = f.encode({ ...f.original, config: { changed: true } });
      if (fault === "storage-status") f.controls.failedHistory = true;
      if (fault === "recorded-marker") await withRootMigrationJournal(f.request.receipt, false, async (journal, receipt) => {
        delete (receipt!.namespace!.metadata as any).annotations[CORE_PUBLICATION];
        await journal.save(receipt!);
      });
      if (fault === "missing-marker") delete f.namespace("core").metadata.annotations[CORE_PUBLICATION];
      if (fault === "foreign-marker" || fault === "launch-marker") {
        const marker = JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]);
        if (fault === "foreign-marker") marker.attempt = "a".repeat(64);
        else marker.phase = "publishing";
        f.namespace("core").metadata.annotations[CORE_PUBLICATION] = canonical(marker);
      }
      if (fault === "root") f.root.spec.replicas = 1;
      if (fault === "companion") f.companion.spec.replicas = 1;
      if (fault === "root-pod") f.pods.set("core", [f.rootPod()]);
      if (fault === "ca") f.ca.data["ca.crt"] = rootCertificates[3]!;
      if (fault === "unrotated") f.rotate(1);
      if (fault === "writers") f.grant("core").spec.writers = f.grantBefore.spec.writers;
      const evidence = readFileSync(f.request.receipt, "utf8"), namespace = structuredClone(f.namespace("core"));
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, { ...f.request, chart: "charts/repaired-immutable-core", replacePreparingRequest: true })).rejects.toThrow();
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      expect(f.namespace("core")).toEqual(namespace);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["extra-template", "changed-content", "stale-version", "changed-context"] as const)("rejects %s on prepared resume before launching", async change => {
    const f = await publicationFixture();
    try {
      f.controls.preparedInterruption = true;
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      const receipt = await f.current(), artifact = receipt.publication!.artifact!;
      if (change === "extra-template") {
        chmodSync(join(artifact, "templates"), 0o700);
        writeFileSync(join(artifact, "templates", "unreviewed.yaml"), "kind: Secret", { mode: 0o400 });
        chmodSync(join(artifact, "templates"), 0o500);
      } else if (change === "changed-content") {
        chmodSync(join(artifact, "values.yaml"), 0o600);
        writeFileSync(join(artifact, "values.yaml"), '{"changed":true}');
        chmodSync(join(artifact, "values.yaml"), 0o400);
      } else if (change === "stale-version") f.root.metadata.resourceVersion = "999999";
      else f.request.context = "kind-other";
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("prepared");
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });

  it.each(["preparing", "prepared"] as const)("prevents a copied %s receipt from sharing the launch ownership", async phase => {
    const f = await publicationFixture();
    try {
      f.controls[phase === "preparing" ? "preflightFailure" : "preparedInterruption"] = true;
      await expect(f.publish()).rejects.toThrow();
      const copy = `${f.request.receipt}.copy`;
      copyFileSync(f.request.receipt, copy);
      let blocked = false;
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          expect(JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).phase).toBe("publishing");
          await expect(publishPrivateCore(f.execute, { ...f.request, receipt: copy })).rejects.toThrow();
          blocked = true;
        }
        return f.execute(file, args, options);
      };
      await publishPrivateCore(execute, f.request);
      expect(blocked).toBe(true);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("retains ambiguous launch ownership without starting Helm after a lost claim acknowledgement", async () => {
    const f = await publicationFixture();
    try {
      f.controls.claimAckFailure = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("publishing");
      expect(JSON.parse(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).phase).toBe("publishing");
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("unknown");
    } finally { f.cleanup(); }
  });

  it.each(["launchFailure", "fenceFailure", "immutableAckFailure"] as const)("refuses ambiguous %s including copied pre-publication qualification", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("publishing");
      await expect(f.publish()).rejects.toThrow("unknown");
      await expect(finalizeRootMigration(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        f.retired, f.journal, applyReviewedGrant)).rejects.toThrow(/publication/i);
      expect(f.controls.launches).toBe(1);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
      expect(f.root.spec.replicas).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["hooks", "changedTls", "failedHistory"] as const)("refuses %s before publication", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe(failure === "failedHistory" ? undefined : "preparing");
    } finally { f.cleanup(); }
  });
});
