// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { publishPrivateCore } from "./private-core-publication.js";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { finalizeRootMigration } from "./private-root-migration.js";
import {
  monitoringCapacityFixture, historicalPublicationFixture,
} from "./private-root-migration.publication.test-support.js";

describe("owned monitoring capacity publication", () => {
  it.each(["cpu", "memory", "both", "tebibyte", "changed-cpu", "changed-memory", "extra-resource", "invalid-unit"])(
    "compares API-normalized monitoring quantities without accepting capacity drift (%s)", async mode => {
      const f = await monitoringCapacityFixture();
      try {
        f.resources.limits = { cpu: "1000m", memory: mode === "tebibyte" ? "1048576Mi" : "1024Mi" };
        const normalized = structuredClone(f.resources);
        if (mode !== "memory" && mode !== "tebibyte") normalized.limits.cpu = mode === "changed-cpu" ? "2" : "1";
        if (mode !== "cpu") normalized.limits.memory = mode === "changed-memory" ? "2Gi"
          : mode === "tebibyte" ? "1Ti" : mode === "invalid-unit" ? "1G" : "1Gi";
        if (mode === "extra-resource") (normalized.limits as any)["ephemeral-storage"] = "1Gi";
        const execute: SchemaExecute = async (tool, args, settings) => {
          const result = await f.execute(tool, args, settings);
          if (tool === "kubectl" && args[0] === "apply") {
            const projected = JSON.parse(result.stdout);
            if (projected.metadata.uid === f.metrics.metadata.uid) {
              projected.spec.template.spec.containers[0].resources = structuredClone(normalized);
              return { stdout: JSON.stringify(projected) };
            }
          }
          if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
            f.metrics.spec.template.spec.containers[0].resources = structuredClone(normalized);
          }
          return result;
        };
        const result = publishPrivateCore(execute, f.request);
        if (["changed-cpu", "changed-memory", "extra-resource", "invalid-unit"].includes(mode)) {
          await expect(result).rejects.toThrow("must remain semantically unchanged");
          expect(f.controls.launches).toBe(0);
        } else {
          await result;
          expect((await f.current()).publication?.phase).toBe("verified");
          expect(f.metrics.spec.template.spec.containers[0].resources).toEqual(normalized);
          expect(f.controls.launches).toBe(1);
        }
      } finally { f.cleanup(); }
    });

  it.each([false, true])("publishes only configured metrics resources and retains frozen provenance; recovery=%s", async recovery => {
    const f = await monitoringCapacityFixture();
    try {
      const before = structuredClone(f.metrics);
      if (recovery) {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.controls.failedPublication = true;
        await expect(f.publish()).rejects.toThrow("outcome is unknown");
        f.controls.failedPublication = false;
        await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      } else await f.publish();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(recovery ? 2 : 1);
      expect(f.metrics.metadata.uid).toBe(before.metadata.uid);
      before.spec.template.spec.containers[0].resources = f.resources;
      expect(f.metrics.spec).toEqual(before.spec);
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.some(object => object.kind === "Job")).toBe(false);
      expect(frozen.find(object => object.metadata.name === f.metrics.metadata.name)!.spec).toEqual(before.spec);
    } finally { f.cleanup(); }
  });

  it.each(["image", "probe", "account", "replicas", "extra-container", "other-workload", "unrequested-resources",
    "missing-limits", "zero-limit", "extra-resource", "other-config", "wrong-identity", "server-rejection"])(
    "refuses monitoring changes beyond bounded configured capacity: %s", async mode => {
      const f = await monitoringCapacityFixture();
      try {
        const change = f.controls.previewMutate!;
        f.controls.previewMutate = items => {
          change(items);
          const object = items.find(value => value.metadata.name === f.metrics.metadata.name), pod = object.spec.template.spec;
          if (mode === "image") pod.containers[0].image = "metrics:changed";
          if (mode === "probe") pod.containers[0].readinessProbe.httpGet.path = "/";
          if (mode === "account") pod.serviceAccountName = "other";
          if (mode === "replicas") object.spec.replicas = 2;
          if (mode === "extra-container") pod.containers.push({ name: "other", image: "other:latest" });
          if (mode === "other-workload") items.find(value => value.metadata.name === "custom-monitoring-operator").spec.template.spec.containers[0].resources = f.resources;
          if (mode === "unrequested-resources") pod.containers[0].resources.limits.cpu = "1";
        };
        if (mode === "missing-limits") delete (f.resources as any).limits;
        if (mode === "zero-limit") f.resources.limits.memory = "0";
        if (mode === "extra-resource") (f.resources.limits as any)["ephemeral-storage"] = "1Gi";
        if (mode === "other-config") f.controls.config.monitoring["kube-state-metrics"].extraArgs = ["--unreviewed"];
        if (mode === "wrong-identity") f.metrics.metadata.labels["app.kubernetes.io/name"] = "another-metric-service";
        const execute: SchemaExecute = async (tool, args, settings) => {
          if (mode === "server-rejection" && tool === "kubectl" && args[0] === "apply"
            && JSON.parse(settings.input!).metadata.name === f.metrics.metadata.name) throw new Error("API rejected resource quantity");
          return f.execute(tool, args, settings);
        };
        await expect(publishPrivateCore(execute, f.request)).rejects.toThrow();
        expect(f.controls.launches).toBe(0);
        expect(f.metrics.spec.template.spec.containers[0].resources.limits).toEqual({ cpu: "250m", memory: "128Mi" });
      } finally { f.cleanup(); }
    });
});

describe("owned historical monitoring hook retirement", () => {
  it("requires explicit opt-in and then preserves failed bytes through a hook-free publication", async () => {
    const f = await historicalPublicationFixture();
    try {
      const failed = structuredClone(f.history[0]);
      await expect(f.publish()).rejects.toThrow("failed/pending");
      expect(f.controls.writes).toBe(0);
      await f.retireHooks();
      const receipt = await f.current();
      expect(receipt.historicalHookRetirement?.phase).toBe("fenced");
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.history[0].data).toEqual(failed.data);
      expect(f.history[0].metadata.labels.status).toBe("failed");
      expect(f.history[0].metadata.uid).toBe(failed.metadata.uid);
      expect(f.history[0].immutable).toBe(true);
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["before", "after"])("resumes %s freeze interruption without replaying mutation or Helm", async boundary => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = boundary;
      await expect(f.retireHooks()).rejects.toThrow();
      expect((await f.current()).historicalHookRetirement?.phase).toBe("prepared");
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("Resume historical");
      await expect(finalizeRootMigration(f.migrationExecute, await f.current(), f.journal, async () => {})).rejects.toThrow("Resume historical");
      await f.retireHooks();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each(["Job", "ServiceAccount", "RoleBinding", "pod"])("refuses remaining %s authority without a freeze or launch", async kind => {
    const f = await historicalPublicationFixture();
    try {
      f.state.live = kind;
      await expect(f.retireHooks()).rejects.toThrow();
      expect(f.state.freezes).toBe(0);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it("verifies a persisted fence without importing the previous retirement journal", async () => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = "after";
      await expect(f.retireHooks()).rejects.toThrow();
      await withRootMigrationJournal(f.request.receipt, false, async journal => journal.save(f.retired));
      await f.publish();
      expect((await f.current()).historicalHookRetirement).toBeUndefined();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.state.freezes).toBe(1);
    } finally { f.cleanup(); }
  });

  it("refuses changed successor history after a lost acknowledgement", async () => {
    const f = await historicalPublicationFixture();
    try {
      f.state.interrupt = "after";
      await expect(f.retireHooks()).rejects.toThrow();
      f.history[1].metadata.uid = "replacement-successor";
      await expect(f.retireHooks()).rejects.toThrow();
      expect(f.state.freezes).toBe(1);
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it("rejects a different requested release before historical storage mutation", async () => {
    const f = await historicalPublicationFixture();
    try {
      await expect(publishPrivateCore(f.execute, { ...f.request, release: "another", retireHistoricalMonitoringHooks: true })).rejects.toThrow("reviewed release");
      expect(f.controls.writes).toBe(0);
      expect(f.state.freezes).toBe(0);
    } finally { f.cleanup(); }
  });
});
