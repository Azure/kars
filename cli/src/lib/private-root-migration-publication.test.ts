// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { randomUUID } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { publishPrivateCore } from "./private-core-publication.js";
import { schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION } from "./private-activation.js";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { templateDigest, validateQualifiedActivation, type Execute } from "./private-activation.js";
import { finalizeRootMigration } from "./private-root-migration.js";
import { ORIGINAL, TARGET } from "./private-root-migration.test-support.js";
import {
  publicationFixture, monitoringPublicationFixture, witnessPublicationFixture,
} from "./private-root-migration.publication.test-support.js";

describe("owned protected full-Core publication", () => {

  it("resumes retired root readiness against server defaults without relaunching Helm", async () => {
    const f = await publicationFixture();
    try {
      f.controls.previewMutate = items => {
        delete items.find(object => object.kind === "Deployment" && object.metadata.name === f.root.metadata.name)!.spec.progressDeadlineSeconds;
      };
      f.controls.pendingRootReplicaSet = true;
      f.controls.afterSuccessFailure = true;
      await expect(f.publish()).rejects.toThrow();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("succeeded");
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.find(object => object.kind === "Deployment" && object.metadata.name === f.root.metadata.name)!.spec.progressDeadlineSeconds).toBeUndefined();
      expect(f.root.spec.progressDeadlineSeconds).toBe(600);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.controls.readinessCalls.some(args => args.includes(`Deployment/${f.root.metadata.name}`))).toBe(false);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "spec", "replicas"])("rejects retired root %s drift when resuming readiness", async mutation => {
    const f = await publicationFixture();
    try {
      f.controls.afterSuccessFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (mutation === "uid") f.root.metadata.uid = "replaced-root";
      if (mutation === "spec") f.root.spec.progressDeadlineSeconds = 601;
      if (mutation === "replicas") f.root.spec.replicas = 1;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("succeeded");
      expect(f.controls.launches).toBe(1);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
    } finally { f.cleanup(); }
  });

  it("publishes with a retired Pending root whose new ReplicaSet is denied", async () => {
    const f = await publicationFixture();
    try {
      f.controls.pendingRootReplicaSet = true;
      await f.publish();
      expect(f.root.status.observedGeneration).toBeLessThan(f.root.metadata.generation);
      expect(f.root.spec.replicas).toBe(0);
      expect(f.controls.readinessCalls.some(args => args.includes(`Deployment/${f.root.metadata.name}`))).toBe(false);
      expect((await f.current()).publication?.phase).toBe("verified");
      expect((await f.current()).phase).not.toBe("complete");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it.each([
    ["Deployment", { replicas: 1 }], ["DaemonSet", {}], ["StatefulSet", { replicas: 1 }],
    ["DaemonSet", { updateStrategy: { type: "OnDelete" } }],
    ["StatefulSet", { updateStrategy: { type: "OnDelete" }, replicas: 1 }],
    ["PersistentVolumeClaim", {}], ["Service", { type: "ClusterIP" }],
    ["Service", { type: "LoadBalancer" }], ["ReplicaSet", { replicas: 1 }],
    ["ReplicationController", { replicas: 1 }],
  ] as const)("retains ownership and resumes %s readiness without replaying Helm (%j)", async (kind, spec) => {
    const f = await publicationFixture(undefined, [{ apiVersion: kind === "Deployment" || kind === "DaemonSet"
      || kind === "StatefulSet" || kind === "ReplicaSet" ? "apps/v1" : "v1", kind,
      metadata: { name: "supporting", namespace: "supporting" }, spec }]);
    try {
      f.controls.readinessFailure = `${kind}/supporting`;
      await expect(f.publish()).rejects.toThrow();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("succeeded");
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
      expect(f.root.spec.replicas).toBe(0);
      expect(f.controls.readinessCalls.some(args => args.includes(`${kind}/supporting`))).toBe(true);
      f.controls.readinessFailure = "";
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      if (kind === "Service" && "type" in spec && spec.type === "LoadBalancer") {
        expect(f.controls.readinessCalls.some(args => args.includes("--for=jsonpath={.status.loadBalancer.ingress}"))).toBe(true);
      }
    } finally { f.cleanup(); }
  });

  it("retains ownership while a published CRD is not Established", async () => {
    const f = await publicationFixture();
    try {
      const crd = f.objects.find(value => value.kind === "CustomResourceDefinition")!;
      f.controls.readinessFailure = `CustomResourceDefinition/${crd.metadata.name}`;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("succeeded");
      expect(f.controls.readinessCalls.some(args => args.includes("--for=condition=Established"))).toBe(true);
      f.controls.readinessFailure = "";
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it("omits completed unchanged admission maintenance without running jobs or changing monitoring", async () => {
    const f = await monitoringPublicationFixture();
    try {
      await f.publish();
      expect(f.controls.launches).toBe(1);
      const receipt = await f.current();
      const frozen = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(frozen.some(value => value.kind === "Job")).toBe(false);
      expect(frozen.find(value => value.metadata.name === "custom-monitoring-operator")?.metadata.namespace).toBe("observability");
      expect(receipt.publication!.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it.each(["missing", "duplicate", "unknown", "changed-manifest", "changed-events", "changed-policy", "wrong-kind",
    "failed", "no-predecessor", "extra-predecessor", "changed-values", "missing-provenance", "changed-workload"])(
    "refuses unsafe admission bootstrap omission: %s", async mode => {
      const f = await monitoringPublicationFixture();
      try {
        const hooks = f.controls.monitoringHooks;
        if (mode === "missing") hooks.pop();
        if (mode === "duplicate") hooks[1] = structuredClone(hooks[0]);
        if (mode === "unknown") hooks[0].path += ".unknown";
        if (mode === "changed-manifest") hooks[0].manifest += "\n---\n{}";
        if (mode === "changed-events") hooks[0].events = ["post-delete"];
        if (mode === "changed-policy") hooks[0].delete_policies = [];
        if (mode === "wrong-kind") hooks[0].kind = "Job";
        if (mode === "failed") f.original.hooks[1].last_run.phase = "Failed";
        if (mode === "no-predecessor") f.original.hooks = f.original.hooks.slice(0, 1);
        if (mode === "extra-predecessor") f.original.hooks.push({ ...structuredClone(f.original.hooks[1]), path: `${hooks[0].path}.extra` });
        if (mode === "changed-values") f.controls.config.monitoring.fullnameOverride = "different";
        if (mode === "missing-provenance") f.original.manifest = f.original.manifest.replaceAll("# Source:", "# Removed:");
        if (mode === "changed-workload") f.controls.previewMutate = items => {
          items.find(value => value.metadata.name === "custom-monitoring-operator").spec.template.spec.containers[0].image = "operator:changed";
        };
        f.history[0]!.data.release = f.encode(f.original);
        await expect(f.publish()).rejects.toThrow();
        expect(f.controls.launches).toBe(0);
        expect((await f.current()).publication?.phase).toBe("preparing");
      } finally { f.cleanup(); }
    });

  it.each(["revision-only", "scope", "digest", "stale-revision", "skipped-revision", "other-data"])(
    "allows only the current datapath witness revision transition: %s", async mode => {
      const settings = { schema_version: 1, enabled: true, release_revision: 1, config_digest: "retained", scope: "all", sandboxes: [] };
      const f = await publicationFixture(undefined, [{ apiVersion: "v1", kind: "ConfigMap", metadata: {
        name: "kars-datapath-witness-settings", namespace: "core", labels: { "kars.azure.com/witness-addon": "true" },
      }, data: { "settings.json": JSON.stringify(settings) } }]);
      try {
        f.controls.previewMutate = items => {
          const object = items.find(value => value.metadata.name === "kars-datapath-witness-settings");
          const after = { ...settings, release_revision: 2 };
          if (mode === "scope") after.scope = "selected";
          if (mode === "digest") after.config_digest = "changed";
          if (mode === "stale-revision") after.release_revision = 1;
          if (mode === "skipped-revision") after.release_revision = 3;
          if (mode === "other-data") object.data.unreviewed = "changed";
          object.data["settings.json"] = JSON.stringify(after);
        };
        if (mode === "revision-only") {
          await f.publish();
          expect((await f.current()).publication?.phase).toBe("verified");
        } else {
          await expect(f.publish()).rejects.toThrow(/semantically unchanged/);
          expect(f.controls.launches).toBe(0);
        }
      } finally { f.cleanup(); }
    });

  it.each(["--replace-preparing-request", "--recover-failed-publication", "--retire-historical-monitoring-hooks"])(
    "exposes %s only as an explicit publication option", name => {
    const command = credentialGrantsCommand().commands.find(value => value.name() === "migrate-root-image")
      ?.commands.find(value => value.name() === "publish-core");
    const option = command?.options.find(value => value.long === name);
    expect(option).toBeDefined();
    expect(option?.defaultValue).toBeUndefined();
  });

  it("publishes Core while retaining the enrolled Bridge companion and then restores both", async () => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      const before = structuredClone(f.companion);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.companion).toEqual(before);
      expect(f.companion.spec.replicas).toBe(0);
      const run: Execute = async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout;
      await finalizeRootMigration(run, await f.current(), f.journal, applyReviewedGrant);
      expect(f.companion.spec.replicas).toBe(2);
      expect(f.root.spec.replicas).toBe(2);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    } finally { f.cleanup(); }
  });

  it.each(["csa", undefined, null, "unknown", "wrong-field"])("rejects predecessor apply method %s before any publication mutation", async method => {
    const f = await publicationFixture();
    try {
      f.original.apply_method = method === "wrong-field" ? undefined : method;
      if (method === "wrong-field") f.original.applyMethod = "ssa";
      f.history[0].data.release = f.encode(f.original);
      f.root.metadata.managedFields = [{ manager: "helm", operation: "Update", apiVersion: "apps/v1",
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:replicas": {} } } }];
      const before = structuredClone([f.root, f.policy, f.secret, f.history]);
      await expect(f.publish()).rejects.toThrow(/SSA predecessor/);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeUndefined();
      expect((await f.current()).publication).toBeUndefined();
      expect([f.root, f.policy, f.secret, f.history]).toEqual(before);
    } finally { f.cleanup(); }
  });

  it.each(["adoptionBeforePatchFailure", "adoptionAckFailure", "adoptionAfterSaveFailure", "adoptionBadAck"] as const)("recovers journaled static policy %s before freezing or launching", async failure => {
    const f = await publicationFixture();
    try {
      const policy = structuredClone(f.policy.spec), history = structuredClone(f.history);
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe("preparing");
      expect(f.controls.launches).toBe(0);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.adoptions).toBe(1);
      expect(f.controls.liveOwnershipChecks).toBe(1);
      expect(f.policy.metadata.uid).toBe("tool-policy");
      expect(f.policy.spec).toEqual(policy);
      expect(f.history[0].metadata.uid).toBe(history[0].metadata.uid);
    } finally { f.cleanup(); }
  });

  it.each(["uid", "body", "owner", "version"])("refuses changed %s when resuming static policy adoption", async change => {
    const f = await publicationFixture();
    try {
      f.controls.adoptionBeforePatchFailure = true;
      await expect(f.publish()).rejects.toThrow();
      if (change === "uid") f.policy.metadata.uid = "replacement-policy";
      if (change === "body") f.policy.spec = { changed: true };
      if (change === "owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
      if (change === "version") f.policy.metadata.resourceVersion = "999";
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.adoptions).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("preparing");
    } finally { f.cleanup(); }
  });

  it("keeps SSA predecessors eligible with Helm-owned Update fields without running CSA conversion", async () => {
    const f = await publicationFixture();
    try {
      f.root.metadata.managedFields = [{ manager: "helm", operation: "Update", apiVersion: "apps/v1",
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:replicas": {} } } }];
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("rechecks the stored apply method on prepared resume before any new mutation", async () => {
    const f = await publicationFixture();
    try {
      f.controls.preparedInterruption = true;
      await expect(f.publish()).rejects.toThrow();
      f.original.apply_method = "csa";
      f.history[0].data.release = f.encode(f.original);
      f.controls.writes = 0;
      await expect(f.publish()).rejects.toThrow(/SSA predecessor/);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      expect((await f.current()).publication?.phase).toBe("prepared");
    } finally { f.cleanup(); }
  });

  it.each(["duplicate", "missing", "resource-version", "body", "namespace"])("rejects a %s published inventory without weakening exact object binding", async mutation => {
    const f = await publicationFixture();
    try {
      f.controls.manifestMutation = mutation;
      await expect(f.publish()).rejects.toThrow("Published Helm manifest differs");
      expect((await f.current()).publication?.phase).toBe("publishing");
      expect(f.controls.fences).toBe(0);
      await expect(f.publish()).rejects.toThrow("unknown");
    } finally { f.cleanup(); }
  });

  it("accepts real Helm sorting of the maintained admission policies, static ToolPolicy and CRDs", async () => {
    const directory = `.core-sorting-render-${randomUUID()}`;
    mkdirSync(join(directory, "templates"), { recursive: true, mode: 0o700 });
    mkdirSync(join(directory, "files"), { mode: 0o700 });
    try {
      expect(execFileSync("helm", ["version", "--template", "{{.Version}}"], { encoding: "utf8" }).trim()).toMatch(/^v4\.1\.3(?:\+|$)/);
      writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: kars\nversion: 0.1.0\n");
      for (const file of ["toolpolicy-default.yaml", "admission-no-public-router-exposure.yaml"]) {
        copyFileSync(new URL(`../../../deploy/helm/kars/templates/${file}`, import.meta.url), join(directory, "templates", file));
      }
      copyFileSync(new URL("../../../deploy/helm/kars/files/kars-default-agt-profile.yaml", import.meta.url), join(directory, "files/kars-default-agt-profile.yaml"));
      const documents = schemaDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "core"],
        { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] }));
      const policy = documents.find(value => value.kind === "ToolPolicy")!;
      const admission = documents.filter(value => value.kind.startsWith("ValidatingAdmissionPolicy"));
      expect(admission.map(value => value.kind)).toEqual(["ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"]);
      const f = await publicationFixture(policy, admission);
      try {
        Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
        f.controls.realHelmRender = true;
        await f.publish();
        expect(f.controls.liveOwnershipChecks).toBe(1);
        expect(f.controls.adoptions).toBe(0);
        expect((await f.current()).publication?.phase).toBe("verified");
      } finally { f.cleanup(); }
    } finally { rmSync(directory, { recursive: true, force: true }); }
  });

  it("publishes the maintained chart's default ToolPolicy without running its delete/recreate hook lifecycle", async () => {
    const directory = `.core-policy-render-${randomUUID()}`;
    mkdirSync(join(directory, "templates"), { recursive: true, mode: 0o700 });
    mkdirSync(join(directory, "files"), { mode: 0o700 });
    try {
      writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: kars\nversion: 0.1.0\n");
      copyFileSync(new URL("../../../deploy/helm/kars/templates/toolpolicy-default.yaml", import.meta.url), join(directory, "templates/toolpolicy-default.yaml"));
      copyFileSync(new URL("../../../deploy/helm/kars/files/kars-default-agt-profile.yaml", import.meta.url), join(directory, "files/kars-default-agt-profile.yaml"));
      const [policy] = schemaDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "core"],
        { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] }));
      policy!.spec.agtProfile.inline = policy!.spec.agtProfile.inline.replace(/\n$/, "");
      const f = await publicationFixture(policy);
      try {
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.policy.metadata.finalizers = ["kars.azure.com/toolpolicy-cleanup"];
        f.policy.spec.agtProfile.inline += "\n";
        const liveSpec = structuredClone(f.policy.spec);
        await f.publish();
        expect(f.policy.metadata.uid).toBe("tool-policy");
        expect(f.policy.spec).toEqual(liveSpec);
        expect(f.policy.metadata.finalizers).toEqual(["kars.azure.com/toolpolicy-cleanup"]);
        const frozen = schemaDocuments(readFileSync(join((await f.current()).publication!.artifact!, "files/publication.yaml"), "utf8"))
          .find(value => value.kind === "ToolPolicy")!;
        expect(frozen.spec).toEqual(liveSpec);
        expect(frozen.metadata.finalizers).toEqual(f.policy.metadata.finalizers);
        expect(f.policy.metadata.annotations["helm.sh/hook"]).toBeUndefined();
        expect(f.policy.metadata.annotations["meta.helm.sh/release-name"]).toBe("kars");
        expect((await f.current()).publication?.phase).toBe("verified");
      } finally { f.cleanup(); }
    } finally { rmSync(directory, { recursive: true, force: true }); }
  });

  it.each(["unknown-finalizer", "extra-finalizer", "policy-change", "extra-newlines", "scalar-newline", "invalid-yaml", "multiple-documents", "changed-label"] as const)(
    "refuses static policy compatibility with %s before adoption", async failure => {
      const f = await publicationFixture();
      try {
        const inline = failure === "scalar-newline" ? "description: |+\n  retained\n"
          : failure === "invalid-yaml" ? "rules: ["
            : failure === "multiple-documents" ? "rules: []\n---\nrules: []" : "rules: []";
        const rendered = JSON.parse(f.original.hooks[0].manifest);
        rendered.spec.agtProfile = { inline };
        f.original.hooks[0].manifest = JSON.stringify(rendered);
        f.history[0].data.release = f.encode(f.original);
        f.controls.policyHook = structuredClone(f.original.hooks[0]);
        f.policy.spec = structuredClone(rendered.spec);
        f.policy.spec.agtProfile.inline += failure === "extra-newlines" ? "\n\n" : "\n";
        f.policy.metadata.finalizers = ["kars.azure.com/toolpolicy-cleanup"];
        if (failure === "unknown-finalizer") f.policy.metadata.finalizers = ["another.example/cleanup"];
        if (failure === "extra-finalizer") f.policy.metadata.finalizers.push("another.example/cleanup");
        if (failure === "policy-change") f.policy.spec.allowed = "changed";
        if (failure === "changed-label") f.policy.metadata.labels.changed = "true";
        const before = structuredClone(f.policy);
        await expect(f.publish()).rejects.toThrow("unchanged live predecessor");
        expect(f.controls.adoptions).toBe(0);
        expect(f.controls.launches).toBe(0);
        expect(f.policy).toEqual(before);
      } finally { f.cleanup(); }
    });

  it.each(["changed-policy", "foreign-owner", "missing-history"] as const)("refuses static hook conversion with %s", async failure => {
    const f = await publicationFixture();
    try {
      if (failure === "changed-policy") f.policy.spec = { allowed: "unreviewed-policy" };
      else if (failure === "foreign-owner") f.policy.metadata.annotations["meta.helm.sh/release-name"] = "someone-else";
      else f.history[0].data.release = Buffer.from(gzipSync(Buffer.from(JSON.stringify({ name: "kars", namespace: "core", version: 1,
        info: { status: "deployed" }, manifest: "", hooks: [] }))).toString("base64")).toString("base64");
      await expect(f.publish()).rejects.toThrow();
      expect(f.controls.launches).toBe(0);
    } finally { f.cleanup(); }
  });

  it.each([false, true])("publishes through retirement and requalifies the retained grant (runner-only: %s)", async runnerOnly => {
    const f = await publicationFixture(undefined, [], runnerOnly);
    try {
      await f.publish();
      const receipt = await f.current();
      expect(receipt.publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.controls.fences).toBe(8);
      expect(f.history.every(value => value.immutable === true)).toBe(true);
      expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeUndefined();
      expect(receipt.retirement).toEqual(f.retired.retirement);
      expect(f.history.map(value => value.metadata.uid)).toEqual(["release-1", "release-2"]);
      const completed = await finalizeRootMigration(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        receipt, f.journal, applyReviewedGrant);
      expect(completed.phase).toBe("complete");
      expect(f.grant("core").metadata.uid).toBe(f.grantBefore.metadata.uid);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      expect(f.ca.data["contracts.json"]).toBe('{"unchanged":"accounts-charges-caps"}');
      expect(f.ca.metadata.uid).toBe("contracts-uid");
      const originalMetadata = receipt.plan.original.metadata;
      if (originalMetadata === null || typeof originalMetadata !== "object" || Array.isArray(originalMetadata)) {
        throw new Error("Expected original root metadata to be a JSON object");
      }
      expect(f.root.metadata.uid).toBe(originalMetadata.uid);
      expect(f.root.spec.replicas).toBe(2);
      expect(f.root.spec.template.spec.containers[0].image).toBe(runnerOnly ? ORIGINAL : TARGET);
      if (runnerOnly) expect(f.root.spec.template.spec.containers[0].env.find(
        (entry: any) => entry.name === "KARS_CONFORMANCE_RUNNER_IMAGE",
      ).value).toBe(TARGET);
      expect(templateDigest(f.root)).toBe(receipt.plan.review.target.templateDigest);
      expect(f.grant("core").spec.privateActivation.namespaces[0].epoch)
        .not.toBe(f.grantBefore.spec.privateActivation.namespaces[0].epoch);
      await validateQualifiedActivation(async (args, input) => (await f.execute("kubectl", args, { stdio: "pipe", input })).stdout,
        f.grant("core").spec.privateActivation);
    } finally { f.cleanup(); }
  });

  it("resolves omitted resource namespaces against the release, not the operator's default namespace", async () => {
    const f = await publicationFixture();
    try {
      f.controls.omittedNamespace = true;
      await f.publish();
      const receipt = await f.current();
      const objects = schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8"));
      expect(objects.find(object => object.kind === "Deployment")!.metadata.namespace).toBe("core");
      expect(receipt.publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it("preserves sanitized Helm 4 release-list fallback with an enrolled companion", async () => {
    const f = await publicationFixture(undefined, [], false, 1);
    try {
      const marker = "PRIVATE-KEY-MUST-NOT-BE-DISCLOSED";
      let fallbacks = 0;
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "list" && args.includes("--all")) {
          fallbacks += 1;
          throw Object.assign(new Error(marker), { stderr: "Error: unknown flag: --all\n", stdout: marker, exitCode: 1 });
        }
        return f.execute(file, args, options);
      };
      await publishPrivateCore(execute, f.request);
      expect(fallbacks).toBeGreaterThan(0);
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
      expect(f.companion.spec.replicas).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain(marker);
    } finally { f.cleanup(); }
  });

  it.each([
    { stderr: "Error: unknown flag: --all\nPRIVATE-KEY-MUST-NOT-BE-DISCLOSED", exitCode: 1 },
    { stderr: "Error: unknown flag: --all", exitCode: 2 },
  ])("withholds unexpected Helm inventory errors instead of treating them as syntax fallback: %j", async error => {
    const f = await publicationFixture();
    try {
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "list") throw Object.assign(new Error("PRIVATE-KEY-MUST-NOT-BE-DISCLOSED"), error);
        return f.execute(file, args, options);
      };
      await expect(publishPrivateCore(execute, f.request)).rejects.toThrow("output is withheld");
      expect(f.controls.launches).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain("PRIVATE-KEY-MUST-NOT-BE-DISCLOSED");
    } finally { f.cleanup(); }
  });

  it("does not disclose credentials in failed Helm commands or render output", async () => {
    const f = await publicationFixture();
    try {
      const marker = "PRIVATE-KEY-MUST-NOT-BE-DISCLOSED";
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "template") throw Object.assign(new Error(`helm --set key=${marker}`), { stderr: marker, exitCode: 1 });
        return f.execute(file, args, options);
      };
      await expect(publishPrivateCore(execute, f.request)).rejects.toThrow("output is withheld");
      expect(f.controls.launches).toBe(0);
      expect(readFileSync(f.request.receipt, "utf8")).not.toContain(marker);
    } finally { f.cleanup(); }
  });

  it("retains previously immutable deployed history on the next protected publication", async () => {
    const f = await publicationFixture();
    try {
      f.history[0].immutable = true;
      const retained = structuredClone(f.history[0].data);
      await f.publish();
      expect(f.history[0].data).toEqual(retained);
      expect(f.history.map(value => value.metadata.labels.status)).toEqual(["deployed", "deployed"]);
      expect(f.history.every(value => value.immutable)).toBe(true);
      expect((await f.current()).publication?.phase).toBe("verified");
    } finally { f.cleanup(); }
  });

  it.each(["preflightFailure", "preparedInterruption", "afterSuccessFailure", "lostReleaseAck"] as const)("resumes %s without duplicating a Helm launch", async failure => {
    const f = await publicationFixture();
    try {
      f.controls[failure] = true;
      await expect(f.publish()).rejects.toThrow();
      expect((await f.current()).publication?.phase).toBe(failure === "preflightFailure" ? "preparing" : failure === "preparedInterruption" ? "prepared" : "succeeded");
      expect(f.controls.launches).toBe(failure === "preflightFailure" || failure === "preparedInterruption" ? 0 : 1);
      await f.publish();
      expect((await f.current()).publication?.phase).toBe("verified");
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });

  it("explicitly replaces only a preparing request, preserving its attempt, ownership and original journal evidence", async () => {
    const f = await publicationFixture(undefined, [], false, 2);
    try {
      f.controls.preflightFailure = true;
      await expect(f.publish()).rejects.toThrow();
      const before = await f.current(), evidence = readFileSync(f.request.receipt, "utf8");
      const namespace = structuredClone(f.namespace("core")), companion = structuredClone(f.companion);
      const replacement = { ...f.request, chart: "charts/repaired-immutable-core" };
      f.controls.writes = 0;
      await expect(publishPrivateCore(f.execute, replacement)).rejects.toThrow("original Core publication request");
      expect(readFileSync(f.request.receipt, "utf8")).toBe(evidence);
      f.controls.preflightFailure = true;
      await expect(publishPrivateCore(f.execute, { ...replacement, replacePreparingRequest: true })).rejects.toThrow();
      const recovered = await f.current(), updated = readFileSync(f.request.receipt, "utf8");
      expect(updated.startsWith(evidence)).toBe(true);
      expect(recovered.publication!.requestDigest).not.toBe(before.publication!.requestDigest);
      expect(recovered).toEqual({ ...before, publication: { ...before.publication, requestDigest: recovered.publication!.requestDigest } });
      expect(JSON.parse(updated.slice(evidence.length).split("\n")[0]!).receipt).toEqual(recovered);
      expect(f.namespace("core")).toEqual(namespace);
      expect(f.companion).toEqual(companion);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("original Core publication request");
      await publishPrivateCore(f.execute, replacement);
      expect((await f.current()).publication).toMatchObject({ phase: "verified", attempt: before.publication!.attempt,
        requestDigest: recovered.publication!.requestDigest });
      expect(f.controls.launches).toBe(1);
      expect(f.companion).toEqual(companion);
    } finally { f.cleanup(); }
  });

  it.each([0, 1])("replaces a preparing request while preserving fenced witness replica intent %s", async replicas => {
    const f = await witnessPublicationFixture(replicas);
    try {
      Object.assign(f.policy.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
      f.state.evidenceAckLost = true;
      await expect(f.publish()).rejects.toThrow();
      const before = await f.current(), evidence = readFileSync(f.request.receipt, "utf8");
      expect(before.publication?.phase).toBe("preparing");
      expect(before.publication?.witnessWriter?.evidenceFence).toBeDefined();
      const writer = structuredClone(f.writer), config = structuredClone(f.evidence), scales = [...f.state.writes];
      const replacement = { ...f.request, values: ["repaired-model-values.yaml"] };
      f.controls.writes = 0;
      f.controls.preflightFailure = true;
      await expect(publishPrivateCore(f.execute, { ...replacement, replacePreparingRequest: true })).rejects.toThrow("Command output is withheld");
      const replaced = await f.current();
      expect(replaced.publication!.requestDigest).not.toBe(before.publication!.requestDigest);
      expect(replaced).toEqual({ ...before, publication: { ...before.publication, requestDigest: replaced.publication!.requestDigest } });
      expect(readFileSync(f.request.receipt, "utf8").startsWith(evidence)).toBe(true);
      expect(f.writer).toEqual(writer);
      expect(f.evidence).toEqual(config);
      expect(f.state.writes).toEqual(scales);
      expect(f.controls.writes).toBe(0);
      expect(f.controls.launches).toBe(0);
      await expect(f.publish()).rejects.toThrow("original Core publication request");
      await publishPrivateCore(f.execute, replacement);
      expect((await f.current()).publication).toMatchObject({ phase: "verified", attempt: before.publication!.attempt,
        witnessWriter: { restored: true, deployment: before.publication!.witnessWriter!.deployment } });
      expect(f.writer.spec.replicas).toBe(replicas);
      expect(f.evidence.data).toEqual(config.data);
      expect(f.controls.launches).toBe(1);
    } finally { f.cleanup(); }
  });
});
