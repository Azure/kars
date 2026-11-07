// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { gunzipSync } from "node:zlib";
import { publishPrivateCore } from "./private-core-publication.js";
import { schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION, CORE_PUBLICATION_FENCE } from "./private-activation.js";
import {
  witnessPublicationFixture, monitoringCapacityFixture, failedPublicationFixture,
  monitoringPublicationFixture, publicationFixture,
} from "./private-root-migration.publication.test-support.js";

describe("failed protected publication recovery", () => {
  it.each(["publishing", "succeeded"].flatMap(phase => ["none", "claim", "resource", "storage"].map(boundary => [phase, boundary])))(
    "fences a fully projected deployed revision from %s before a fresh successor, resuming %s", async (phase, boundary) => {
      const f = await witnessPublicationFixture(1, true);
      try {
        const key = "deployment.kubernetes.io/revision";
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.writer.metadata.annotations[key] = "4";
        f.state.afterLaunch = () => {
          f.writer.metadata.annotations[key] = "5";
          f.controls.fenceFailure = phase === "publishing";
          f.controls.readinessFailure = phase === "succeeded" ? "Deployment/aggregator" : "";
        };
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        const original = structuredClone((await f.current()).publication!);
        const releaseData = f.history[1].data.release;
        expect(f.history[1].metadata.labels.status).toBe("deployed");
        expect(original.phase).toBe(phase);
        f.state.afterLaunch = undefined;
        f.controls.fenceFailure = false;
        f.controls.readinessFailure = "";
        let interrupted = false;
        const execute: SchemaExecute = async (tool, args, options) => {
          const patch = tool === "kubectl" && args[0] === "patch" ? JSON.parse(options.input!) : [];
          const claim = patch.some((op: any) => op.path === "/metadata/annotations" && op.op === "replace"
            && JSON.parse(op.value?.[CORE_PUBLICATION] ?? "{}").phase === "preparing");
          const fence = patch.some((op: any) => String(op.value?.[CORE_PUBLICATION_FENCE] ?? "").endsWith(":failed-publication"));
          const result = await f.execute(tool, args, options);
          if (!interrupted && ((boundary === "claim" && claim)
            || (fence && (boundary === "storage" ? args[1] === "secret" : boundary === "resource" && args[1] !== "secret")))) {
            interrupted = true;
            throw new Error("lost recovery acknowledgement");
          }
          return result;
        };
        const request = { ...f.request, recoverFailedPublication: true };
        if (boundary !== "none") {
          await expect(publishPrivateCore(execute, request)).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
          expect(interrupted).toBe(true);
          expect(f.controls.launches).toBe(1);
        }
        await publishPrivateCore(f.execute, request);
        const current = await f.current();
        expect(current.publication?.phase).toBe("verified");
        expect(current.publication?.attempt).not.toBe(original.attempt);
        expect(current.publication?.revision).toBe(3);
        expect(current.failedPublications?.[0].recovery?.fenced).toBe(true);
        expect(f.history[1].data.release).toBe(releaseData);
        expect(f.history[1].metadata.labels.status).toBe("deployed");
        expect(f.history[1].immutable).toBe(true);
        expect(f.writer.spec.replicas).toBe(1);
        expect(f.root.spec.replicas).toBe(0);
        expect(f.controls.launches).toBe(2);
        await publishPrivateCore(f.execute, request);
        expect(f.controls.launches).toBe(2);
      } finally { f.cleanup(); }
    });

  it.each(["publishing", "succeeded"].flatMap(phase => ["resource-body", "writer-running", "writer-uid"].map(mode => [phase, mode])))(
    "refuses deployed recovery from %s with %s drift before ownership or fencing writes", async (phase, mode) => {
      const f = await witnessPublicationFixture();
      try {
        f.state.afterLaunch = () => {
          f.controls.fenceFailure = phase === "publishing";
          f.controls.readinessFailure = phase === "succeeded" ? "Deployment/aggregator" : "";
        };
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        expect((await f.current()).publication?.phase).toBe(phase);
        f.state.afterLaunch = undefined;
        f.controls.fenceFailure = false;
        f.controls.readinessFailure = "";
        if (mode === "resource-body") f.evidence.data["witness.json"] = "changed";
        if (mode === "writer-running") f.writer.spec.replicas = 1;
        if (mode === "writer-uid") f.writer.metadata.uid = "replacement";
        const writes = f.controls.writes;
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.controls.writes).toBe(writes);
        expect(f.controls.launches).toBe(1);
        expect((await f.current()).publication?.recovery).toBeUndefined();
      } finally { f.cleanup(); }
    });

  it.each(["none", "server-defaulted-revision", "spec", "uid", "annotation", "revision-backward", "revision-invalid"])(
    "publishes a bounded monitoring readiness repair with only legitimate Deployment revision drift (%s)", async mode => {
    const f = await monitoringCapacityFixture();
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      const desired = f.controls.previewMutate;
      f.controls.config.monitoring["kube-state-metrics"].resources = structuredClone(f.original.config.monitoring["kube-state-metrics"].resources);
      f.controls.previewMutate = () => {};
      f.metrics.metadata.annotations["deployment.kubernetes.io/revision"] = "2";
      f.controls.readinessFailure = "Deployment/custom-kube-state-metrics";
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      expect((await f.current()).publication?.phase).toBe("succeeded");
      const original = f.history[1].data.release;
      f.controls.config.monitoring.kubeStateMetricsResourcesBaseline = structuredClone(f.original.config.monitoring["kube-state-metrics"].resources);
      f.controls.config.monitoring["kube-state-metrics"].resources = structuredClone(f.resources);
      f.controls.previewMutate = documents => {
        desired?.(documents);
        if (mode === "server-defaulted-revision") delete documents.find(value => value.metadata.name === f.metrics.metadata.name)!
          .metadata.annotations["deployment.kubernetes.io/revision"];
      };
      f.controls.readinessFailure = "";
      const execute: SchemaExecute = async (tool, args, settings) => {
        const result = await f.execute(tool, args, settings);
        if (mode === "server-defaulted-revision" && tool === "kubectl" && args[0] === "apply") {
          const projected = JSON.parse(result.stdout);
          if (projected.metadata.uid === f.metrics.metadata.uid) {
            projected.metadata.annotations["deployment.kubernetes.io/revision"] = "2";
            return { stdout: JSON.stringify(projected) };
          }
        }
        if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          f.metrics.metadata.annotations["deployment.kubernetes.io/revision"] = mode === "revision-backward" ? "1"
            : mode === "revision-invalid" ? "invalid" : "3";
          if (mode === "spec") f.metrics.spec.template.spec.containers[0].image = "unreviewed";
          if (mode === "uid") f.metrics.metadata.uid = "replacement";
          if (mode === "annotation") f.metrics.metadata.annotations.unreviewed = "changed";
        }
        return result;
      };
      const result = publishPrivateCore(execute, { ...f.request, recoverFailedPublication: true });
      if (!["none", "server-defaulted-revision"].includes(mode)) {
        await expect(result).rejects.toThrow();
        expect((await f.current()).publication?.phase).toBe("publishing");
        expect(f.history[1].data.release).toBe(original);
        expect(f.controls.launches).toBe(2);
        expect(f.root.spec.replicas).toBe(0);
        return;
      }
      await result;
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.metrics.spec.template.spec.containers[0].resources).toEqual(f.resources);
      expect(f.history[1].data.release).toBe(original);
      expect(f.history[1].immutable).toBe(true);
      expect(f.controls.launches).toBe(2);
      expect(f.root.spec.replicas).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "spec", "annotation", "unobserved", "type-warning", "foreign-fence"])(
    "refuses succeeded recovery with changed admission %s before ownership", async mutation => {
      const f = await witnessPublicationFixture(1, true);
      try {
        f.controls.readinessFailure = "Deployment/aggregator";
        await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
        expect((await f.current()).publication?.phase).toBe("succeeded");
        const policy = f.objects.find(value => value.kind === "ValidatingAdmissionPolicy")!;
        if (mutation === "uid") policy.metadata.uid += "-replaced";
        if (mutation === "spec") policy.spec.failurePolicy = "Ignore";
        if (mutation === "annotation") policy.metadata.annotations.unreviewed = "changed";
        if (mutation === "unobserved") policy.status.observedGeneration = 0;
        if (mutation === "type-warning") policy.status.typeChecking.expressionWarnings = [{ warning: "invalid" }];
        if (mutation === "foreign-fence") policy.metadata.annotations[CORE_PUBLICATION_FENCE] = "foreign";
        const owner = f.namespace("core").metadata.annotations[CORE_PUBLICATION];
        const writes = f.controls.writes;
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBe(owner);
        expect(f.controls.writes).toBe(writes);
        expect(f.controls.launches).toBe(1);
      } finally { f.cleanup(); }
    });

  it.each(["mutable-storage", "failed-status"])("refuses succeeded recovery with %s before writes", async mode => {
    const f = await witnessPublicationFixture();
    try {
      f.controls.readinessFailure = "Deployment/aggregator";
      await expect(f.publish()).rejects.toThrow("KARS_PRIVATE_COMMAND_FAILURE");
      expect((await f.current()).publication?.phase).toBe("succeeded");
      if (mode === "mutable-storage") delete f.history[1].immutable;
      else {
        const release = JSON.parse(gunzipSync(Buffer.from(Buffer.from(f.history[1].data.release, "base64").toString(), "base64")).toString());
        release.info.status = "failed";
        f.history[1].metadata.labels.status = "failed";
        f.history[1].data.release = f.encode(release);
      }
      const writes = f.controls.writes;
      await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.recovery).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it("refuses an uninspected Helm version before claiming or fencing recovery", async () => {
    const f = await failedPublicationFixture();
    try {
      const before = structuredClone([f.objects, f.history, await f.current()]);
      await expect(f.recover(async (tool, args, settings) => tool === "helm" && args[0] === "version"
        ? { stdout: "v4.2.0" } : f.execute(tool, args, settings))).rejects.toThrow("inspected Helm 4.1.3");
      expect([f.objects, f.history, await f.current()]).toEqual(before);
    } finally { f.cleanup(); }
  });

  it("retains completed monitoring bootstrap provenance through failed recovery", async () => {
    const f = await monitoringPublicationFixture();
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["changed-defaults", "missing-defaults", "explicit-override", "unexpected-override", "changed-template",
    "extra-template", "renamed-file", "extra-file", "changed-file", "dependencies", "schema", "wrong-chart"])(
    "refuses unproven frozen monitoring configuration: %s", async mode => {
      const f = await monitoringPublicationFixture();
      try {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.controls.failedPublication = true;
        await expect(f.publish()).rejects.toThrow("outcome is unknown");
        f.controls.failedPublication = false;
        const entry = f.history.at(-1)!;
        const release = JSON.parse(gunzipSync(Buffer.from(Buffer.from(entry.data.release, "base64").toString(), "base64")).toString());
        expect(release.config).toEqual({});
        expect(release.chart.values.monitoring).toEqual(f.controls.config.monitoring);
        if (mode === "changed-defaults") release.chart.values.monitoring.enabled = false;
        if (mode === "missing-defaults") delete release.chart.values.monitoring;
        if (mode === "explicit-override") release.config.monitoring = { enabled: false };
        if (mode === "unexpected-override") release.config.unreviewed = true;
        if (mode === "changed-template") release.chart.templates[0].data = Buffer.from("{{ .Values.unreviewed }}").toString("base64");
        if (mode === "extra-template") release.chart.templates.push(structuredClone(release.chart.templates[0]));
        if (mode === "renamed-file") release.chart.files[0].name = "files/unreviewed.yaml";
        if (mode === "extra-file") release.chart.files.push(structuredClone(release.chart.files[0]));
        if (mode === "changed-file") {
          const documents = schemaDocuments(Buffer.from(release.chart.files[0].data, "base64").toString());
          documents[0]!.metadata.labels.unreviewed = "true";
          release.chart.files[0].data = Buffer.from(documents.map(value => JSON.stringify(value)).join("\n---\n")).toString("base64");
        }
        if (mode === "dependencies") release.chart.metadata.dependencies = [{ name: "unreviewed" }];
        if (mode === "schema") release.chart.schema = Buffer.from("{}").toString("base64");
        if (mode === "wrong-chart") release.chart.metadata.name = "unreviewed";
        entry.data.release = f.encode(release);
        await expect(publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true })).rejects.toThrow();
        expect(f.controls.launches).toBe(1);
        expect(f.root.spec.replicas).toBe(0);
      } finally { f.cleanup(); }
    });

  it.each(["unchanged", "changed-scope", "future-revision", "invalid-revision"])("handles settings left by a failed publication: %s", async mode => {
    const config = { schema_version: 1, release_revision: 1, scope: "all", sandboxes: [], enabled: true };
    const f = await publicationFixture(undefined, [{ apiVersion: "v1", kind: "ConfigMap", metadata: {
      name: "kars-datapath-witness-settings", namespace: "core", labels: { "kars.azure.com/witness-addon": "true" },
    }, data: { "settings.json": JSON.stringify(config) } }]);
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.previewMutate = documents => {
        documents.find(value => value.metadata.name === "kars-datapath-witness-settings")!.data["settings.json"] =
          JSON.stringify({ ...config, release_revision: f.history.length + 1 });
      };
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      const settings = f.objects.find(value => value.metadata.name === "kars-datapath-witness-settings")!;
      expect(JSON.parse(settings.data["settings.json"]).release_revision).toBe(1);
      if (mode !== "unchanged") settings.data["settings.json"] = JSON.stringify({ ...config,
        ...(mode === "changed-scope" ? { scope: "selected" } : { release_revision: mode === "future-revision" ? 99 : "1" }) });
      const recover = () => publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      if (mode === "unchanged") {
        await recover();
        expect(JSON.parse(settings.data["settings.json"]).release_revision).toBe(3);
        expect((await f.current()).publication?.phase).toBe("verified");
      } else {
        await expect(recover()).rejects.toThrow("must remain semantically unchanged");
        expect(f.controls.launches).toBe(1);
      }
    } finally { f.cleanup(); }
  });

  it.each([0, 1])("carries original witness replica intent %s through partial failure and fresh publication", async replicas => {
    const f = await witnessPublicationFixture(replicas);
    try {
      f.controls.policyHook = structuredClone(f.original.hooks[0]);
      f.controls.failedPublication = true;
      await expect(f.publish()).rejects.toThrow("outcome is unknown");
      expect(f.writer.spec.replicas).toBe(0);
      f.controls.failedPublication = false;
      await publishPrivateCore(f.execute, { ...f.request, recoverFailedPublication: true });
      expect(f.writer.spec.replicas).toBe(replicas);
      expect(f.writer.spec.template.spec.containers[0].image).toBe("witness:updated");
      expect(f.evidence.data["witness.json"]).toBe("retained runtime evidence");
      expect((await f.current()).publication?.witnessWriter?.restored).toBe(true);
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("resumes after the fenced successor is journaled without reconstructing the failed attempt", async () => {
    const f = await failedPublicationFixture();
    try {
      f.controls.preflightFailure = true;
      await expect(f.recover()).rejects.toThrow();
      const current = await f.current();
      expect(current.publication?.phase).toBe("preparing");
      expect(current.failedPublications?.[0].recovery?.fenced).toBe(true);
      const attempt = current.publication?.attempt;
      await f.recover();
      expect((await f.current()).publication?.attempt).toBe(attempt);
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("retains each failed attempt when a fresh successor also fails", async () => {
    const f = await failedPublicationFixture();
    try {
      f.controls.failedPublication = true;
      await expect(f.recover()).rejects.toThrow("outcome is unknown");
      f.controls.failedPublication = false;
      await f.recover();
      const current = await f.current();
      expect(current.publication?.revision).toBe(4);
      expect(current.failedPublications?.map(value => value.revision)).toEqual([2, 3]);
      expect(new Set([...current.failedPublications!, current.publication!].map(value => value.attempt)).size).toBe(3);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "failed", "failed", "deployed"]);
      expect(f.controls.launches).toBe(3);
    } finally { f.cleanup(); }
  });

  it.each(["bad-resource-ack", "bad-immutable-ack", "storage-drift"])("refuses %s before a successor can launch", async mutation => {
    const f = await failedPublicationFixture();
    try {
      if (mutation === "bad-immutable-ack") f.controls.immutableAckFailure = true;
      let changed = false;
      const execute: SchemaExecute = async (tool, args, options) => {
        const before = tool === "kubectl" && args[0] === "patch" && args[2] === f.policy.metadata.name
          ? structuredClone(f.policy) : undefined;
        const result = await f.execute(tool, args, options);
        if (!changed && before) {
          changed = true;
          if (mutation === "bad-resource-ack") return { stdout: JSON.stringify(before) };
          if (mutation === "storage-drift") f.history[1].data.release = f.encode({ ...f.original, version: 2, info: { status: "failed" }, manifest: "changed" });
        }
        return result;
      };
      await expect(f.recover(execute)).rejects.toThrow();
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).failedPublications).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it("invalidates every old request, retains immutable failed history and launches only a fresh attempt", async () => {
    const f = await failedPublicationFixture();
    try {
      const failedData = f.history[1].data.release;
      const frozen = schemaDocuments(readFileSync(join(f.failed.artifact!, "files/publication.yaml"), "utf8"));
      const execute: SchemaExecute = async (tool, args, options) => {
        if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
          expect(f.history.every(value => value.immutable === true)).toBe(true);
          for (const object of frozen) {
            const live = f.objects.find(value => value.kind === object.kind && value.metadata.name === object.metadata.name)!;
            expect(live.metadata.uid).toBe(object.metadata.uid);
            expect(live.metadata.resourceVersion).not.toBe(object.metadata.resourceVersion);
          }
          expect(JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt.publication.attempt).not.toBe(f.failed.attempt);
        }
        return f.execute(tool, args, options);
      };
      await f.recover(execute);
      const current = await f.current();
      expect(current.publication?.phase).toBe("verified");
      expect(current.publication?.revision).toBe(3);
      expect(current.failedPublications).toHaveLength(1);
      expect(current.failedPublications![0].artifact).toBe(f.failed.artifact);
      expect(current.failedPublications![0].recovery?.fenced).toBe(true);
      expect(f.history[1].data.release).toBe(failedData);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "failed", "deployed"]);
      expect(f.controls.launches).toBe(2);
      await f.recover();
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["pending-upgrade", "deployed-resource-drift", "absent", "manifest", "hooks", "csa", "prior-data", "prior-uid", "gap"])("refuses %s history before recovery ownership or writes", async mutation => {
    const f = await failedPublicationFixture();
    try {
      const release: any = JSON.parse(gunzipSync(Buffer.from(Buffer.from(f.history[1].data.release, "base64").toString(), "base64")).toString());
      if (["pending-upgrade", "deployed-resource-drift"].includes(mutation)) {
        release.info.status = mutation === "deployed-resource-drift" ? "deployed" : mutation;
        f.history[1].metadata.labels.status = release.info.status;
        if (mutation === "deployed-resource-drift") f.root.metadata.labels.unreviewed = "true";
      }
      if (mutation === "manifest") release.manifest += `\n---\n${JSON.stringify(schemaDocuments(release.manifest)[0])}`;
      if (mutation === "hooks") release.hooks = [{ kind: "Job", manifest: "unreviewed writer" }];
      if (mutation === "csa") release.apply_method = "csa";
      f.history[1].data.release = f.encode(release);
      if (mutation === "prior-data") f.history[0].data.release = f.encode({ ...f.original, manifest: "changed" });
      if (mutation === "prior-uid") f.history[0].metadata.uid = "replaced-history";
      if (mutation === "absent") f.history.pop();
      if (mutation === "gap") f.history.shift();
      const writes = f.controls.writes;
      await expect(f.recover()).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).publication?.recovery).toBeUndefined();
    } finally { f.cleanup(); }
  });

  it.each(["claim", "resource", "storage"])("resumes a lost %s acknowledgement without reusing the failed launch", async boundary => {
    const f = await failedPublicationFixture();
    try {
      let interrupted = false;
      const execute: SchemaExecute = async (tool, args, options) => {
        const patch = tool === "kubectl" && args[0] === "patch" ? JSON.parse(options.input!) : [];
        const recoveryFence = patch.some((op: any) => String(op.value?.[CORE_PUBLICATION_FENCE] ?? "").endsWith(":failed-publication"));
        const recoveryClaim = patch.some((op: any) => op.path === "/metadata/annotations" && op.op === "replace"
          && JSON.parse(op.value?.[CORE_PUBLICATION] ?? "{}").phase === "preparing");
        const result = await f.execute(tool, args, options);
        if (!interrupted && ((boundary === "claim" && recoveryClaim)
          || (recoveryFence && (boundary === "storage" ? args[1] === "secret" : boundary === "resource" && args[1] !== "secret")))) {
          interrupted = true;
          throw new Error("lost recovery acknowledgement");
        }
        return result;
      };
      await expect(f.recover(execute)).rejects.toThrow();
      expect(interrupted).toBe(true);
      expect((await f.current()).publication?.phase).toBe("publishing");
      await f.recover();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it("refuses copied receipts once another recovery owns the namespace", async () => {
    const f = await failedPublicationFixture();
    try {
      const copy = join(dirname(f.request.receipt), "copied.jsonl");
      writeFileSync(copy, readFileSync(f.request.receipt), { mode: 0o600 });
      f.controls.fenceFailure = true;
      await expect(f.recover()).rejects.toThrow();
      f.controls.fenceFailure = false;
      const writes = f.controls.writes;
      await expect(publishPrivateCore(f.execute, { ...f.request, receipt: copy, recoverFailedPublication: true })).rejects.toThrow();
      expect(f.controls.writes).toBe(writes);
      expect(f.controls.launches).toBe(1);
      await f.recover();
      expect(f.controls.launches).toBe(2);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "owner", "missing"])("refuses %s resource proof and leaves recovery owned", async mutation => {
    const f = await failedPublicationFixture();
    try {
      if (mutation === "uid") f.policy.metadata.uid = "replaced-policy";
      if (mutation === "owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "another-release";
      if (mutation === "missing") f.objects.splice(f.objects.indexOf(f.policy), 1);
      await expect(f.recover()).rejects.toThrow();
      expect(f.controls.launches).toBe(1);
      expect((await f.current()).failedPublications).toBeUndefined();
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });
});
