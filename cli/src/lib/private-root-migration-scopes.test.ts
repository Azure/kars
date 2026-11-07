// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { rootCertificates } from "node:tls";
import { createHash } from "node:crypto";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  canonical, digest, PRIVATE_PREFIX as P, templateDigest, validateQualifiedActivation, type Execute,
} from "./private-activation.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import {
  finalizeRootMigration, prepareRootMigration, retireRootMigration, reviewCompletedRootMigration,
  type RootMigrationReceipt,
} from "./private-root-migration.js";
import { setup, TARGET, FIELD, ORIGINAL } from "./private-root-migration.test-support.js";

describe("multi-scope root migration", () => {
  async function migration(replicas = 1, configureForeign?: (deployment: any) => void, configureRoot?: (root: any) => void) {
    const f = await setup(true, configureRoot, 1, replicas, false, configureForeign);
    const target = structuredClone(f.companion);
    target.spec.template.spec.containers[0].image = TARGET;
    const documents = [f.rendered, target];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, [target.metadata.name]);
    const prepared = await prepareRootMigration(f.execute, review, documents, f.journal);
    return { ...f, target, documents, prepared };
  }

  it("withholds writers and refuses resume after fresh-epoch control material changes the sealed SRE template", async () => {
    const version = "kars.azure.com/services-credential-version";
    const before = `control-uid:control-v1:${"a".repeat(64)}`;
    const after = `control-uid:control-v1:${"b".repeat(64)}`;
    const f = await migration(0, deployment => {
      deployment.spec.template.metadata.annotations = { [version]: before };
      deployment.spec.template.spec.serviceAccountName = "sre-api-router";
    });
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    f.rotate(2);
    let changed = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!changed && args[0] === "patch" && args[1] === "namespace" && args[2] === "core"
        && JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version === 2) {
        changed = true;
        f.foreign.spec.template.metadata.annotations[version] = after;
        f.foreign.metadata.resourceVersion = String(Number(f.foreign.metadata.resourceVersion) + 1);
      }
      return result;
    };
    await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant))
      .rejects.toThrow("fixture object unavailable");
    expect(changed).toBe(true);
    const sealed = f.namespace("core").metadata.annotations[FIELD];
    const scope = JSON.parse(sealed).activation.namespaces.find((value: any) => value.namespace.name === "kars-sre");
    expect(scope.epoch).not.toBe(f.review.activation.namespaces.find(value => value.namespace.name === "kars-sre")!.epoch);
    expect(f.foreign.spec.template.metadata.annotations[`${P}epoch`]).toBe(scope.epoch);
    expect(scope.consumers[0].templateDigest).not.toBe(templateDigest(f.foreign));
    expect(f.foreign.metadata.uid).toBe(scope.consumers[0].object.uid);
    expect(f.foreign.spec.replicas).toBe(0);
    expect(f.pods.get("kars-sre")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    expect(f.last().phase).toBe("qualifying");
    expect(f.last().retirement).toEqual(retired.retirement);
    const writes = f.mutations.length, journalEntries = f.saved.length;
    await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant))
      .rejects.toThrow("fixture object unavailable");
    expect(f.mutations).toHaveLength(writes);
    expect(f.saved).toHaveLength(journalEntries);
    expect(f.namespace("core").metadata.annotations[FIELD]).toBe(sealed);
    expect(f.grant("core").spec.writers).toEqual([]);
  });

  it.each(["valid", "pre-seal", "pre-seal-inline", "seal-ack", "journal-ack", "secret-uid", "secret-epoch", "secret-owner", "hash", "namespace-claim", "source-uid", "template", "pods", "writers", "extra-material"])(
    "verifies fresh control material and preserves retired evidence: %s", async fault => {
      const marker = "kars.azure.com/services-credential-version";
      const before = `control-uid:control-v1:${digest({ "control-token": "b2xk" })}`;
      const data: Record<string, string> = { "builtin-memory.json": "e30=", "control-token": "bmV3" };
      const after = `control-uid:control-v1:${digest({ "control-token": data["control-token"] })}`;
      const f = await migration(0, deployment => {
        deployment.spec.template.metadata.annotations = { [marker]: before };
        deployment.spec.template.spec.serviceAccountName = "sre-api-router";
      });
      const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
      f.rotate(2);
      let rotated = false;
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        const state = JSON.parse(f.namespace("core").metadata.annotations[FIELD]);
        const boundary = fault.startsWith("pre-seal")
          ? args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller"
            && state.version === 1 && state.phase === "restoring" && f.root.spec.replicas > 0
          : args[0] === "patch" && args[1] === "namespace" && args[2] === "core" && state.version === 2;
        if (!rotated && boundary) {
          rotated = true;
          f.foreign.spec.template.metadata.annotations[marker] = after;
          f.foreign.metadata.resourceVersion = String(Number(f.foreign.metadata.resourceVersion) + 1);
          f.objects.set(f.key("secret", "router-services-admin", "kars-sre"), {
            type: "Opaque", metadata: { name: "router-services-admin", namespace: "kars-sre", uid: "control-uid", resourceVersion: "2",
              labels: { "app.kubernetes.io/managed-by": "kars-controller" }, annotations: {
                "kars.azure.com/sandbox-uid": "sre-sandbox", "kars.azure.com/namespace-uid": "sre-namespace",
                [`${P}epoch`]: f.namespace("kars-sre").metadata.annotations[`${P}epoch`],
              } }, data,
          });
          if (fault !== "pre-seal-inline") throw new Error("qualification acknowledgement lost after control rotation");
        }
        return result;
      };
      if (fault === "pre-seal-inline") {
        const complete = await finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant);
        expect(complete.phase).toBe("complete");
        expect(complete.controlTransition?.qualification?.phase).toBe("qualified");
        expect(complete.retirement).toEqual(retired.retirement);
        await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
        return;
      }
      await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("qualification acknowledgement lost");
      const sealed = f.namespace("core").metadata.annotations[FIELD];
      const material = f.objects.get(f.key("secret", "router-services-admin", "kars-sre")) as any;
      if (fault === "secret-uid") material.metadata.uid = "replacement";
      if (fault === "secret-epoch") material.metadata.annotations[`${P}epoch`] = "old-epoch";
      if (fault === "secret-owner") material.metadata.annotations["kars.azure.com/sandbox-uid"] = "foreign-source";
      if (fault === "hash") material.data["control-token"] = "d3Jvbmc=";
      if (fault === "namespace-claim") delete f.namespace("kars-sre").metadata.annotations["kars.azure.com/namespace-claim-version"];
      if (fault === "source-uid") (f.objects.get(f.key("karssandbox", "sre", "core")) as any).metadata.uid = "replacement";
      if (fault === "template") f.foreign.spec.template.spec.containers[0].command = ["unexpected"];
      if (fault === "pods") f.pods.set("kars-sre", [f.foreignPod()]);
      if (fault === "writers") f.grant("core").spec.writers = f.grantBefore.spec.writers;
      if (fault === "extra-material") material.data["unexpected"] = "dW5leHBlY3RlZA==";
      if (!["valid", "pre-seal", "seal-ack", "journal-ack"].includes(fault)) {
        const writes = f.mutations.length;
        await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
        expect(f.mutations).toHaveLength(writes);
        expect(f.namespace("core").metadata.annotations[FIELD]).toBe(sealed);
        expect(f.last().retirement).toEqual(retired.retirement);
        return;
      }
      let interrupted = false;
      const resume: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (fault === "seal-ack" && !interrupted && args[0] === "patch" && args[1] === "namespace" && args[2] === "core") {
          interrupted = true;
          throw new Error("transition seal acknowledgement lost");
        }
        return result;
      };
      const journal = { save: async (receipt: RootMigrationReceipt) => {
        await f.journal.save(receipt);
        if (fault === "journal-ack" && !interrupted && receipt.controlTransition?.applied) {
          interrupted = true;
          throw new Error("transition journal acknowledgement lost");
        }
      } };
      if (["seal-ack", "journal-ack"].includes(fault)) await expect(finalizeRootMigration(resume, f.last(), journal, applyReviewedGrant)).rejects.toThrow("acknowledgement lost");
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(complete.retirement).toEqual(retired.retirement);
      expect(complete.controlTransition?.before).toBe(sealed);
      expect(complete.controlTransition?.applied).toBe(true);
      expect(f.foreign.spec.replicas).toBe(0);
      expect(f.foreign.spec.template.metadata.annotations[marker]).toBe(after);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
      expect((await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant)).phase).toBe("complete");
    },
  );

  it.each(["before-prepare", "prepared", "sealed", "applied", "suspended-recovery", "held-recovery", "empty-providers", "null-providers",
    "provider-hash", "ca-hash", "ca-contract", "tls-owner", "tls-epoch", "tls-uid", "tls-expired", "source-owner", "source-generation",
    "runtime-owner", "registration-not-ready", "registration-stale", "registration-generation", "registration-legacy-access",
    "suspended", "held", "extra-annotation", "command", "stale-pod", "foreign-pod",
    "image-hermes", "image-openclaw", "image-pinned", "image-paused", "image-agent", "image-guard", "image-controller",
    "image-mutable", "image-kind", "image-command", "image-duplicate", "image-router", "image-pod", "image-rs",
    "image-prepared", "image-sealed", "image-applied", "image-router-refresh", "image-router-only",
    "image-router-openclaw", "image-router-controller", "image-router-mutable", "image-router-duplicate"])(
    "qualifies only source-derived SRE recovery without losing the retired transition: %s", async fault => {
      const imageChange = fault.startsWith("image-");
      const routerChange = fault.startsWith("image-router-");
      const routerOnly = ["image-router-only", "image-router-openclaw"].includes(fault);
      const timing = imageChange ? fault.slice("image-".length) : fault;
      const hermesImage = `example.test/hermes:latest@sha256:${"3".repeat(64)}`;
      const valid = ["before-prepare", "prepared", "sealed", "applied", "suspended-recovery", "held-recovery", "empty-providers", "null-providers",
        "image-hermes", "image-openclaw", "image-pinned", "image-paused", "image-prepared", "image-sealed", "image-applied",
        "image-router-refresh", "image-router-only", "image-router-openclaw"].includes(fault);
      const paused = ["suspended-recovery", "held-recovery", "image-paused"].includes(fault);
      const marker = "kars.azure.com/services-credential-version";
      const before = `control-uid:control-v1:${digest({ "control-token": "b2xk" })}`;
      const data = { "builtin-memory.json": "e30=", "control-token": "bmV3" };
      const after = `control-uid:control-v1:${digest({ "control-token": data["control-token"] })}`;
      const derived = ["inference-budget-ca-version", "inference-providers-version", "sre-private-credential-uid", "sre-tls-expiry"];
      const f = await migration(0, deployment => {
        deployment.spec.template.metadata.annotations = { [marker]: before,
          "kars.azure.com/sre-registration-uid": "registration-uid", "kars.azure.com/sre-privacy-epoch": "sre-privacy",
          ...Object.fromEntries(derived.map(key => [`kars.azure.com/${key}`, "old"])) };
        deployment.spec.template.spec.serviceAccountName = "sre-api-router";
        if (imageChange) {
          deployment.spec.template.spec.containers[0].name = "agent";
          deployment.spec.template.spec.containers.push({ name: "inference-router", image: ORIGINAL });
          deployment.spec.template.spec.initContainers = [{ name: "egress-guard", image: ORIGINAL, command: ["guard"] }];
        }
      }, imageChange ? root => {
        root.spec.template.spec.containers[0].env.push(
          { name: "SANDBOX_IMAGE", value: routerOnly ? ORIGINAL : TARGET },
          { name: "HERMES_RUNTIME_IMAGE", value: routerOnly ? ORIGINAL : hermesImage },
          { name: "INFERENCE_ROUTER_IMAGE", value: routerChange ? TARGET : ORIGINAL });
      } : undefined);
      const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
      f.rotate(2);
      let rotated = false;
      const rotate: Execute = async (args, input) => {
        const response = await f.execute(args, input);
        const state = JSON.parse(f.namespace("core").metadata.annotations[FIELD]);
        if (!rotated && args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller"
          && state.version === 1 && state.phase === "restoring" && f.root.spec.replicas > 0) {
          rotated = true;
          f.foreign.spec.template.metadata.annotations[marker] = after;
          f.objects.set(f.key("secret", "router-services-admin", "kars-sre"), {
            type: "Opaque", metadata: { name: "router-services-admin", namespace: "kars-sre", uid: "control-uid", resourceVersion: "2",
              labels: { "app.kubernetes.io/managed-by": "kars-controller" }, annotations: {
                "kars.azure.com/sandbox-uid": "sre-sandbox", "kars.azure.com/namespace-uid": "sre-namespace",
                [`${P}epoch`]: f.namespace("kars-sre").metadata.annotations[`${P}epoch`],
              } }, data,
          });
          throw new Error("rotated before root seal");
        }
        return response;
      };
      await expect(finalizeRootMigration(rotate, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotated before root seal");
      const originalSeal = f.namespace("core").metadata.annotations[FIELD];
      const source = f.objects.get(f.key("karssandbox", "sre", "core")) as any;
      source.metadata.generation = 2;
      source.spec = { suspended: false };
      if (imageChange) source.spec.runtime = {
        kind: ["image-openclaw", "image-pinned", "image-router-openclaw"].includes(fault) ? "OpenClaw" : "Hermes",
        ...(fault === "image-pinned" ? { openclaw: { image: ORIGINAL } } : {}),
      };
      const registration = { metadata: { name: "canonical", uid: "registration-uid", resourceVersion: "1", generation: 1 },
        spec: { enabled: true, sandbox: { name: "sre", namespace: "core", uid: "sre-sandbox" },
          runtimeNamespace: { name: "kars-sre", uid: "sre-namespace" } },
        status: { phase: "Ready", observedGeneration: 1, legacySecretAccessDenied: true, privacyEpoch: "sre-privacy" } };
      f.objects.set(f.key("karssreregistrations.kars.azure.com", "canonical"), registration);
      const catalog = f.objects.get(f.key("configmap", "kars-inference-budget-contracts", "core")) as any;
      const expires = String(Math.floor(Date.now() / 1000) + 3600);
      const tls = { metadata: { name: "sre-api-router-identity", namespace: "kars-sre", uid: "tls-uid", resourceVersion: "2",
        annotations: { "kars.azure.com/sre-registration-uid": "registration-uid", "kars.azure.com/sre-privacy-epoch": "sre-privacy",
          "kars.azure.com/sre-tls-expiry": expires } } };
      f.objects.set(f.key("secret", "sre-api-router-identity", "kars-sre"), tls);
      const providers = { metadata: { name: "kars-inference-providers", namespace: "core", uid: "provider-uid", resourceVersion: "3" },
        data: { API_KEY: "cHJvdmlkZXI=" } };
      if (fault === "empty-providers") Reflect.deleteProperty(providers, "data");
      if (fault === "null-providers") Reflect.set(providers, "data", null);
      f.objects.set(f.key("secret", "kars-inference-providers", "core"), providers);
      let recovered = false;
      const recover = () => {
        recovered = true;
        if (imageChange) {
          f.foreign.spec.template.spec.containers[0].image = routerOnly || fault === "image-pinned" ? ORIGINAL : fault === "image-openclaw" ? TARGET : hermesImage;
          f.foreign.spec.template.spec.initContainers[0].image = routerOnly ? ORIGINAL : TARGET;
          if (routerChange) f.foreign.spec.template.spec.containers[1].image = TARGET;
        }
        Object.assign(f.foreign.spec.template.metadata.annotations, {
          "kars.azure.com/inference-budget-ca-version": createHash("sha256").update(rootCertificates[2]!).digest("hex"),
          "kars.azure.com/inference-providers-version": digest([providers.metadata.uid, providers.data ?? null]),
          "kars.azure.com/sre-private-credential-uid": tls.metadata.uid, "kars.azure.com/sre-tls-expiry": expires,
        });
        f.foreign.spec.replicas = 1;
        f.foreign.status.updatedReplicas = 1;
        f.foreign.status.availableReplicas = 1;
        f.pods.set("kars-sre", [f.foreignPod()]);
      };
      if (!["prepared", "sealed", "applied"].includes(timing)) recover();
      const journal = { save: async (receipt: RootMigrationReceipt) => {
        await f.journal.save(receipt);
        if (!recovered && receipt.controlTransition && ((timing === "prepared" && !receipt.controlTransition.applied)
          || (timing === "applied" && receipt.controlTransition.applied))) {
          recover();
          throw new Error("SRE recovered across acknowledgement");
        }
      } };
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!recovered && timing === "sealed" && args[0] === "patch" && args[1] === "namespace" && args[2] === "core"
          && JSON.parse(f.namespace("core").metadata.annotations[FIELD]).version === 2) {
          recover();
          throw new Error("SRE recovered across acknowledgement");
        }
        return result;
      };
      if (["prepared", "sealed", "applied"].includes(timing)) {
        await expect(finalizeRootMigration(execute, f.last(), journal, applyReviewedGrant)).rejects.toThrow("SRE recovered across acknowledgement");
      }
      if (fault === "provider-hash") f.foreign.spec.template.metadata.annotations["kars.azure.com/inference-providers-version"] = "wrong";
      if (fault === "ca-hash") f.foreign.spec.template.metadata.annotations["kars.azure.com/inference-budget-ca-version"] = "wrong";
      if (fault === "ca-contract") catalog.metadata.annotations["kars.azure.com/inference-budget-contracts"] = "wrong";
      if (fault === "tls-owner") tls.metadata.annotations["kars.azure.com/sre-registration-uid"] = "foreign";
      if (fault === "tls-epoch") tls.metadata.annotations["kars.azure.com/sre-privacy-epoch"] = "foreign";
      if (fault === "tls-uid") tls.metadata.uid = "replaced";
      if (fault === "tls-expired") tls.metadata.annotations["kars.azure.com/sre-tls-expiry"] = "1";
      if (fault === "source-owner") registration.spec.sandbox.uid = "foreign";
      if (fault === "source-generation") source.metadata.generation = 0;
      if (fault === "runtime-owner") registration.spec.runtimeNamespace.uid = "foreign";
      if (fault === "registration-not-ready") registration.status.phase = "Migrating";
      if (fault === "registration-stale") registration.status.observedGeneration = 0;
      if (fault === "registration-generation") registration.status.observedGeneration = registration.metadata.generation = 0;
      if (fault === "registration-legacy-access") registration.status.legacySecretAccessDenied = false;
      if (fault === "image-agent") f.foreign.spec.template.spec.containers[0].image = ORIGINAL;
      if (fault === "image-guard") f.foreign.spec.template.spec.initContainers[0].image = hermesImage;
      if (fault === "image-controller") f.root.spec.template.spec.containers[0].env.find((entry: any) => entry.name === "HERMES_RUNTIME_IMAGE").value = ORIGINAL;
      if (fault === "image-mutable") f.foreign.spec.template.spec.containers[0].image = "example.test/hermes:latest";
      if (fault === "image-kind") source.spec.runtime.kind = "Byo";
      if (fault === "image-command") f.foreign.spec.template.spec.initContainers[0].command = ["changed"];
      if (fault === "image-duplicate") f.foreign.spec.template.spec.containers.push(structuredClone(f.foreign.spec.template.spec.containers[0]));
      if (fault === "image-router") f.foreign.spec.template.spec.containers[1].image = TARGET;
      if (fault === "image-router-controller") f.root.spec.template.spec.containers[0].env.find((entry: any) => entry.name === "INFERENCE_ROUTER_IMAGE").value = ORIGINAL;
      if (fault === "image-router-mutable") f.foreign.spec.template.spec.containers[1].image = "example.test/router:latest";
      if (fault === "image-router-duplicate") f.foreign.spec.template.spec.containers.push(structuredClone(f.foreign.spec.template.spec.containers[1]));
      if (fault === "image-pod") f.pods.get("kars-sre")![0].spec.containers[0].image = ORIGINAL;
      if (fault === "image-rs") (f.objects.get(f.key("replicasets.apps", "sre-rs", "kars-sre")) as any).spec.template.spec.containers[0].image = ORIGINAL;
      if (["suspended", "suspended-recovery", "image-paused"].includes(fault)) source.spec.suspended = true;
      if (["held", "held-recovery"].includes(fault)) source.metadata.annotations["kars.azure.com/credential-rebind-task-uid"] = "held";
      if (paused) {
        f.foreign.spec.replicas = 0;
        f.foreign.status.updatedReplicas = f.foreign.status.availableReplicas = 0;
        f.pods.set("kars-sre", []);
      }
      if (fault === "extra-annotation") f.foreign.spec.template.metadata.annotations.unexpected = "changed";
      if (fault === "command") f.foreign.spec.template.spec.containers[0].command = ["changed"];
      if (fault === "stale-pod") f.pods.get("kars-sre")![0].metadata.annotations[`${P}epoch`] = "old";
      if (fault === "foreign-pod") f.pods.get("kars-sre")![0].metadata.ownerReferences[0].uid = "foreign";
      if (!valid) {
        const writes = f.mutations.length;
        await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
        expect(f.mutations).toHaveLength(writes);
        expect(f.grant("core").spec.writers).toEqual([]);
        expect(f.namespace("core").metadata.annotations[FIELD]).toBe(originalSeal);
        return;
      }
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(complete.retirement).toEqual(retired.retirement);
      expect(complete.controlTransition?.before).toBe(originalSeal);
      expect(complete.controlTransition?.consumers[0]?.reconciliation).toEqual({ sourceUid: "sre-sandbox", sourceGeneration: 2, replicas: paused ? 0 : 1,
        ...(imageChange ? { runtime: expect.objectContaining({ kind: source.spec.runtime.kind }) } : {}) });
      expect(!!complete.controlTransition?.previous).toBe(["prepared", "sealed", "applied"].includes(timing));
      expect(f.foreign.spec.replicas).toBe(paused ? 0 : 1);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
      await validateQualifiedActivation(f.execute, f.grant("core").spec.privateActivation);
      expect((await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant)).phase).toBe("complete");
      const writes = f.mutations.length, retained = canonical(complete);
      const reviewedDeployments = await reviewCompletedRootMigration(f.execute, complete);
      expect(reviewedDeployments).toContainEqual(expect.objectContaining({
        metadata: expect.objectContaining({ uid: f.foreign.metadata.uid }),
        spec: expect.objectContaining({ replicas: paused ? 0 : 1 }),
      }));
      expect(canonical(complete)).toBe(retained);
      expect(f.mutations).toHaveLength(writes);
      f.foreign.spec.replicas = paused ? 1 : 0;
      await expect(reviewCompletedRootMigration(f.execute, complete)).rejects.toThrow();
      expect(f.mutations).toHaveLength(writes);
      expect(canonical(complete)).toBe(retained);
    },
  );

  it("retains a qualified SRE scope enrolled after the original root seal", async () => {
    const f = await setup(true, undefined, 1, 0, true);
    expect(JSON.parse(f.seal).activation.namespaces).toHaveLength(1);
    expect(f.review.activation.namespaces).toHaveLength(2);
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    f.rotate(2);
    const complete = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    expect(JSON.parse(f.namespace("core").metadata.annotations[FIELD]).activation.namespaces).toHaveLength(2);
    expect(f.foreign.spec.replicas).toBe(0);
    expect(f.grant("core").spec.privateActivation.namespaces).toHaveLength(2);
  });

  it.each(["qualification", "namespace-drift", "consumer-drift"])("resumes qualification safely after foreign %s interruption", async fault => {
    const f = await migration();
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    f.rotate(2);
    let interrupted = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-sre"
        && f.namespace("kars-sre").metadata.annotations[`${P}state`] === "Qualified") {
        interrupted = true;
        throw new Error("foreign qualification acknowledgement lost");
      }
      return result;
    };
    await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("foreign qualification");
    expect(interrupted).toBe(true);
    if (fault === "namespace-drift") f.namespace("kars-sre").metadata.annotations[`${P}root-uid`] = "unreviewed-root";
    if (fault === "consumer-drift") f.foreign.spec.template.spec.containers[0].image = TARGET;
    if (fault !== "qualification") {
      await expect(finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant)).rejects.toThrow();
      expect(f.grant("core").spec.writers).toEqual([]);
    } else {
      const complete = await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
      expect(complete.phase).toBe("complete");
      expect(f.foreign.spec.replicas).toBe(1);
      expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    }
  });

  it.each([0, 1, 2])("upgrades Core and BFF while preserving enrolled SRE intent %s", async replicas => {
    const f = await migration(replicas), foreign = structuredClone(f.foreign);
    const oldEpochs = f.review.activation.namespaces.map(scope => scope.epoch);
    const oldPods = f.pods.get("kars-sre")!.map(pod => pod.metadata.uid);
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(f.foreign.spec).toEqual({ ...foreign.spec, replicas: 0 });
    expect(retired.retirement!.captured["sre-namespace"]).toEqual(oldPods);
    expect(f.namespace("kars-sre").metadata.annotations[`${P}state`]).toBe("Pending");
    expect(f.pods.get("kars-sre")).toEqual([]);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("rotation");
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    const complete = await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    const activation = f.grant("core").spec.privateActivation;
    expect(activation.namespaces.map((scope: any) => scope.namespace.uid).sort()).toEqual(["core-uid", "sre-namespace"]);
    expect(activation.namespaces.every((scope: any) => !oldEpochs.includes(scope.epoch))).toBe(true);
    const scope = activation.namespaces.find((scope: any) => scope.namespace.name === "kars-sre");
    expect(scope.consumers[0].object.uid).toBe(foreign.metadata.uid);
    expect(scope.consumers[0].templateDigest).toBe(templateDigest(foreign));
    expect(f.foreign.metadata.uid).toBe(foreign.metadata.uid);
    expect(f.foreign.spec).toEqual({ ...foreign.spec, template: { ...foreign.spec.template,
      metadata: { ...foreign.spec.template.metadata, annotations: { ...foreign.spec.template.metadata.annotations, [`${P}epoch`]: scope.epoch } } } });
    expect(f.pods.get("kars-sre")).toHaveLength(replicas);
    expect(f.pods.get("kars-sre")!.every(pod => !oldPods.includes(pod.metadata.uid))).toBe(true);
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    await validateQualifiedActivation(f.execute, activation);
    await finalizeRootMigration(f.execute, complete, f.journal, applyReviewedGrant);
  });

  it.each(["omitted", "duplicate", "uid", "revision"])("rejects %s foreign namespace receipt snapshots", async fault => {
    const f = await migration(0);
    const receipt = await retireRootMigration(f.execute, f.prepared, f.journal);
    if (fault === "omitted") delete receipt.namespaces;
    if (fault === "duplicate") receipt.namespaces!.push(structuredClone(receipt.namespaces![0]!));
    if (fault === "uid") (receipt.namespaces![0]!.metadata as any).uid = "unreviewed";
    if (fault === "revision") (receipt.namespaces![0]!.metadata as any).annotations[`${P}bundle-revision`] = "unreviewed";
    f.rotate(2);
    await expect(finalizeRootMigration(f.execute, receipt, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
    expect(f.root.spec.replicas).toBe(0);
  });

  it.each(["uid", "template", "replicas", "namespace"])("rejects foreign %s drift before retirement", async fault => {
    const f = await migration();
    if (fault === "uid") f.foreign.metadata.uid = "replaced";
    if (fault === "template") f.foreign.spec.template.spec.containers[0].image = TARGET;
    if (fault === "replicas") f.foreign.spec.replicas = 4;
    if (fault === "namespace") f.namespace("kars-sre").metadata.annotations[`${P}root-uid`] = "other";
    await expect(retireRootMigration(f.execute, f.prepared, f.journal)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it("refuses foreign companion image selection and unselected rendered image changes", async () => {
    const f = await migration(), target = structuredClone(f.foreign);
    target.spec.template.spec.containers[0].image = TARGET;
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered], undefined, ["sre"])).rejects.toThrow("same-root");
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered, target])).rejects.toThrow("silently replace");
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
  });

  it.each(["pause", "pending", "restoration"])("resumes a lost foreign %s acknowledgement", async boundary => {
    const f = await migration();
    let interrupted = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && !args.includes("--dry-run=server")
        && ((boundary === "pending" && args[1] === "namespace" && args[2] === "kars-sre")
          || (args[2] === "sre" && args.includes("--type=json")
            && f.foreign.spec.replicas === (boundary === "pause" ? 0 : boundary === "restoration" ? 1 : -1)))) {
        interrupted = true;
        throw new Error("lost foreign acknowledgement");
      }
      return result;
    };
    if (boundary !== "restoration") await expect(retireRootMigration(execute, f.prepared, f.journal)).rejects.toThrow("lost foreign");
    const retired = await retireRootMigration(f.execute, boundary === "restoration" ? f.prepared : f.last(), f.journal);
    f.rotate(2);
    if (boundary === "restoration") await expect(finalizeRootMigration(execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow("lost foreign");
    const complete = await finalizeRootMigration(f.execute, boundary === "restoration" ? f.last() : retired, f.journal, applyReviewedGrant);
    expect(complete.phase).toBe("complete");
    expect(f.foreign.spec.replicas).toBe(1);
    expect(f.grant("core").spec.privateActivation.namespaces).toHaveLength(2);
  });
});
