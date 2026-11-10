// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { digest, PRIVATE_PREFIX as P, templateDigest, type Execute } from "./private-activation.js";
import { rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import {
  finalizeRootMigration, prepareRootBudgetTlsRotation, prepareRootMigration, retireRootMigration,
  type RootMigrationReceipt,
} from "./private-root-migration.js";
import { setup, ORIGINAL, TARGET } from "./private-root-migration.test-support.js";

describe("explicit companion image refresh", () => {
  it("requires explicit repeatable companion image review selection in the CLI", () => {
    const review = credentialGrantsCommand().commands.find(value => value.name() === "review-root-migration")!;
    expect(review.opts().companionImage).toEqual([]);
    review.parseOptions(["--companion-image", "kars-mission-dispatcher", "--companion-image", "kars-bridge-bff"]);
    expect(review.opts().companionImage).toEqual(["kars-mission-dispatcher", "kars-bridge-bff"]);
  });

  async function refresh(rootChanged = false) {
    const f = await setup(true, undefined, 1);
    if (!rootChanged) f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
    const target = structuredClone(f.companion);
    target.spec.template.spec.containers[0].image = TARGET;
    const documents = [f.rendered, target];
    const names = [f.companion.metadata.name];
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, names);
    const prepared = await prepareRootMigration(f.execute, review, documents, f.journal);
    return { ...f, target, documents, names, imageReview: review, prepared };
  }

  it.each([false, true])("retires and freshly qualifies reviewed companion images with root change=%s", async rootChanged => {
    const f = await refresh(rootChanged), old = structuredClone(f.companion), oldEpoch = f.namespace("core").metadata.annotations[`${P}epoch`];
    const oldPods = new Set(f.pods.get("core")!.map(pod => pod.metadata.uid));
    expect(f.imageReview.companionImages).toEqual([{
      identity: { name: old.metadata.name, uid: old.metadata.uid, resourceVersion: old.metadata.resourceVersion },
      original: { templateDigest: templateDigest(old), specDigest: digest(old.spec) },
      target: { templateDigest: templateDigest(f.target), specDigest: digest(f.target.spec) },
      images: [{ group: "containers", name: "bff", before: ORIGINAL, after: TARGET }],
    }]);
    expect(f.prepared.plan.companions![0]!.spec).toEqual(old.spec);
    expect(f.prepared.plan.companionTargets).toEqual([{ uid: old.metadata.uid, spec: f.target.spec }]);
    const retired = await retireRootMigration(f.execute, f.prepared, f.journal);
    expect(retired.phase).toBe("rotation-required");
    expect(retired.companionUpdates).toEqual([{ uid: old.metadata.uid, phase: "updated" }]);
    expect(retired.activation!.namespaces[0]!.consumers.find(c => c.object.uid === old.metadata.uid)!.templateDigest).toBe(templateDigest(f.target));
    expect(f.companion.spec).toEqual({ ...f.target.spec, replicas: 0 });
    expect(f.root.spec.replicas).toBe(0);
    expect(f.pods.get("core")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    await expect(finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
    expect(f.companion.metadata.uid).toBe(old.metadata.uid);
    expect(templateDigest(f.companion)).toBe(templateDigest(f.target));
    expect(f.companion.spec.replicas).toBe(old.spec.replicas);
    expect(f.root.spec.template.spec.containers[0].image).toBe(rootChanged ? TARGET : ORIGINAL);
    expect(f.namespace("core").metadata.annotations[`${P}epoch`]).not.toBe(oldEpoch);
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    expect(f.grant("core").spec.agentKeys).toEqual(f.grantBefore.spec.agentKeys);
    expect(f.grant("core").spec.privateActivation.namespaces[0].consumers.find((c: any) => c.object.uid === old.metadata.uid).templateDigest).toBe(templateDigest(f.target));
    expect(f.pods.get("core")!.every(pod => !oldPods.has(pod.metadata.uid))).toBe(true);
    await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
  });

  it.each(["not-selected", "unknown", "root", "duplicate", "missing-render", "unchanged", "uid", "namespace", "hook", "command", "env", "account", "volumes", "replicas", "mutable"])(
    "refuses %s companion review before any mutation", async fault => {
      const f = await setup(true, undefined, 1), target = structuredClone(f.companion);
      target.spec.template.spec.containers[0].image = TARGET;
      let names = [target.metadata.name], documents = [f.rendered, target];
      if (fault === "not-selected") names = [];
      if (fault === "unknown") names = ["unreviewed-dispatcher"];
      if (fault === "root") names = ["kars-controller"];
      if (fault === "duplicate") names.push(names[0]!);
      if (fault === "missing-render") documents = [f.rendered];
      if (fault === "unchanged") target.spec.template.spec.containers[0].image = ORIGINAL;
      if (fault === "uid") target.metadata.uid = "replacement";
      if (fault === "namespace") target.metadata.namespace = "other";
      if (fault === "hook") target.metadata.annotations = { "helm.sh/hook": "pre-upgrade" };
      if (fault === "command") target.spec.template.spec.containers[0].command = ["changed"];
      if (fault === "env") target.spec.template.spec.containers[0].env = [{ name: "MODE", value: "changed" }];
      if (fault === "account") target.spec.template.spec.serviceAccountName = "other";
      if (fault === "volumes") target.spec.template.spec.volumes = [];
      if (fault === "replicas") target.spec.replicas = 2;
      if (fault === "mutable") target.spec.template.spec.containers[0].image = "dispatcher:latest";
      await expect(previewPrivateRootMigration(f.execute, "core", "kars", documents, undefined, names)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("rejects an unselected companion replica-only render delta", async () => {
    const f = await setup(true, undefined, 1), target = structuredClone(f.companion);
    target.spec.replicas = 2;
    await expect(previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered, target])).rejects.toThrow("silently");
    expect(f.mutations).toEqual([]);
  });

  it("rejects no-op root migration without a reviewed companion delta", async () => {
    const f = await setup(true, undefined, 1);
    f.rendered.spec.template.spec.containers[0].image = ORIGINAL;
    const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
    await expect(prepareRootMigration(f.execute, review, [f.rendered], f.journal)).rejects.toThrow("changed immutable");
    expect(f.mutations).toEqual([]);
  });

  it.each(["missing-target", "target-command", "target-image", "review-images", "original-spec", "unreviewed-target", "early-progress"])(
    "rejects digest-consistent %s receipt tampering before retirement", async fault => {
      const f = await refresh(), receipt = structuredClone(f.prepared);
      const target = receipt.plan.companionTargets![0]!;
      if (fault === "missing-target") delete receipt.plan.companionTargets;
      if (fault === "target-command") (target.spec as any).template.spec.containers[0].command = ["changed"];
      if (fault === "target-image") (target.spec as any).template.spec.containers[0].image = ORIGINAL;
      if (fault === "review-images") receipt.plan.review.companionImages![0]!.images[0]!.after = ORIGINAL;
      if (fault === "original-spec") (receipt.plan.companions![0]!.spec as any).replicas = 2;
      if (fault === "unreviewed-target") target.uid = "other";
      if (fault === "early-progress") receipt.companionUpdates = [{ uid: target.uid, phase: "updated" }];
      receipt.planDigest = digest(receipt.plan);
      await expect(retireRootMigration(f.execute, receipt, f.journal)).rejects.toThrow();
      expect(f.mutations).toEqual([]);
    },
  );

  it("does not accept a target image installed before its journaled cutover", async () => {
    const f = await refresh();
    f.companion.spec.template.spec.containers[0].image = TARGET;
    await expect(retireRootMigration(f.execute, f.prepared, f.journal)).rejects.toThrow("companion Deployment changed");
    expect(f.mutations).toEqual([]);
  });

  it("resumes after durable companion completion without repeating its image CAS", async () => {
    const f = await refresh();
    const journal = { save: async (receipt: RootMigrationReceipt) => {
      await f.journal.save(receipt);
      if (receipt.phase === "cutover" && receipt.companionUpdates?.[0]?.phase === "updated") throw new Error("Stopped after durable image completion");
    } };
    await expect(retireRootMigration(f.execute, f.prepared, journal)).rejects.toThrow("Stopped after durable image completion");
    const before = f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name).length;
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name)).toHaveLength(before);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
  });

  it("resumes a lost companion image acknowledgement from durable updating state", async () => {
    const f = await refresh();
    let lost = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!lost && args[0] === "patch" && args[2] === f.companion.metadata.name && !args.includes("--dry-run=server")
        && f.companion.spec.template.spec.containers[0].image === TARGET) {
        lost = true;
        expect(f.last().companionUpdates).toEqual([{ uid: f.companion.metadata.uid, phase: "updating" }]);
        expect(f.pods.get("core")).toEqual([]);
        throw new Error("Lost image acknowledgement");
      }
      return result;
    };
    await expect(retireRootMigration(run, f.prepared, f.journal)).rejects.toThrow("Lost image acknowledgement");
    const imageWrites = () => f.mutations.filter(call => call.args[0] === "patch" && call.args[2] === f.companion.metadata.name
      && call.input?.includes(TARGET)).length;
    expect(imageWrites()).toBe(1);
    const retired = await retireRootMigration(f.execute, f.last(), f.journal);
    expect(imageWrites()).toBe(1);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
  });

  it.each(["command", "uid", "replicas", "version", "root", "namespace", "tls", "after-write"])(
    "fences %s drift at companion image CAS and withholds writers", async fault => {
      const f = await refresh();
      let raced = false;
      const run: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!raced && args[0] === "patch" && args[2] === f.companion.metadata.name && input?.includes(TARGET)
          && args.includes("--dry-run=server") === (fault !== "after-write")) {
          raced = true;
          if (fault === "command") f.companion.spec.template.spec.containers[0].command = ["changed"];
          if (fault === "uid") f.companion.metadata.uid = "replacement";
          if (fault === "replicas") f.companion.spec.replicas = 1;
          if (fault === "version") f.companion.metadata.resourceVersion = "999";
          if (fault === "root") f.root.spec.template.spec.containers[0].command = ["changed"];
          if (fault === "namespace") f.namespace("core").metadata.annotations[`${P}state`] = "Qualified";
          if (fault === "tls") f.rotate(2);
          if (fault === "after-write") f.companion.spec.template.spec.containers[0].image = ORIGINAL;
        }
        return result;
      };
      await expect(retireRootMigration(run, f.prepared, f.journal)).rejects.toThrow();
      expect(raced).toBe(true);
      expect(f.grant("core").spec.writers).toEqual([]);
      expect(f.last().phase).toBe("cutover");
      expect(f.last().companionUpdates![0]!.phase).toBe("updating");
    },
  );
});

describe("same-scope protected Deployment companions", () => {
  it.each([0, 1, 3])("retires and restores %i companion replicas with fresh epoch and retained grant intent", async replicas => {
    const f = await setup(true, undefined, replicas);
    expect(f.review.activation.namespaces[0]!.consumers[0]!.object.name).toBe("kars-bridge-bff");
    const originalSpec = structuredClone(f.companion.spec);
    const prepared = await f.prepare();
    const oldPods = new Set(prepared.plan.pods.map(pod => (pod.identity as { uid: string }).uid));
    expect(prepared.plan.companions).toHaveLength(1);
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    expect(f.root.spec.replicas).toBe(0);
    expect(f.companion.spec.replicas).toBe(0);
    expect(f.pods.get("core")).toEqual([]);
    expect(f.grant("core").spec.writers).toEqual([]);
    f.rotate(2);
    await finalizeRootMigration(f.execute, retired, f.journal, applyReviewedGrant);
    expect(f.last().phase).toBe("complete");
    expect(f.root.spec.replicas).toBe(2);
    expect(f.companion.spec.replicas).toBe(replicas);
    expect(templateDigest(f.companion)).toBe(templateDigest({ spec: originalSpec }));
    expect(f.grant("core").spec.writers).toEqual(f.grantBefore.spec.writers);
    expect(f.grant("core").spec.agentKeys).toEqual(f.grantBefore.spec.agentKeys);
    expect(f.pods.get("core")).toHaveLength(2 + replicas);
    for (const pod of f.pods.get("core")!) {
      expect(oldPods.has(pod.metadata.uid)).toBe(false);
      expect(pod.metadata.annotations[`${P}epoch`]).toBe(f.namespace("core").metadata.annotations[`${P}epoch`]);
    }
    await finalizeRootMigration(f.execute, f.last(), f.journal, applyReviewedGrant);
  });

  it.each(["uid", "image", "command", "selector", "replicas", "epoch", "deleted"])("refuses companion %s drift before mutation", async fault => {
    const f = await setup(true, undefined, 2), prepared = await f.prepare();
    if (fault === "uid") f.companion.metadata.uid = "replacement";
    if (fault === "image") f.companion.spec.template.spec.containers[0].image = TARGET;
    if (fault === "command") f.companion.spec.template.spec.containers[0].command = ["changed"];
    if (fault === "selector") f.companion.spec.selector.matchLabels.app = "changed";
    if (fault === "replicas") f.companion.spec.replicas = 3;
    if (fault === "epoch") f.companion.spec.template.metadata.annotations = { [`${P}epoch`]: "changed" };
    if (fault === "deleted") f.companion.metadata.deletionTimestamp = "2026-10-26T00:00:00Z";
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["omitted", "duplicate", "namespace", "template", "kind", "replicas"])("refuses a digest-consistent %s companion capture", async fault => {
    const f = await setup(false, undefined, 1), prepared = await f.prepare();
    const captured = prepared.plan.companions![0]! as any;
    if (fault === "omitted") delete prepared.plan.companions;
    if (fault === "duplicate") prepared.plan.companions!.push(captured);
    if (fault === "namespace") captured.metadata.namespace = "other";
    if (fault === "template") captured.spec.template.spec.containers[0].image = TARGET;
    if (fault === "kind") captured.kind = "StatefulSet";
    if (fault === "replicas") captured.spec.replicas = -1;
    prepared.planDigest = digest(prepared.plan);
    await expect(retireRootMigration(f.execute, prepared, f.journal)).rejects.toThrow();
    expect(f.mutations).toEqual([]);
  });

  it.each(["pause", "restore"])("resumes lost companion %s acknowledgment without repeating rotation", async point => {
    const f = await setup(true, undefined, 2), prepared = await f.prepare();
    let interrupted = false;
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!interrupted && args[0] === "patch" && args[2] === "kars-bridge-bff"
        && args.includes("--type=json") && !args.includes("--dry-run=server")
        && f.companion.spec.replicas === (point === "pause" ? 0 : 2)) {
        interrupted = true;
        throw new Error("Lost companion acknowledgment");
      }
      return result;
    };
    if (point === "pause") await expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("Lost companion");
    const retired = await retireRootMigration(f.execute, point === "pause" ? f.last() : prepared, f.journal);
    f.rotate(2);
    const keyVersion = f.secret.metadata.resourceVersion;
    if (point === "restore") {
      await expect(finalizeRootMigration(run, retired, f.journal, applyReviewedGrant)).rejects.toThrow("Lost companion");
      expect(f.grant("core").spec.writers).toEqual([]);
    }
    await finalizeRootMigration(f.execute, point === "restore" ? f.last() : retired, f.journal, applyReviewedGrant);
    expect(interrupted).toBe(true);
    expect(f.last().phase).toBe("complete");
    expect(f.companion.spec.replicas).toBe(2);
    expect(f.secret.metadata.resourceVersion).toBe(keyVersion);
  });

  it("does not declare retirement while companion Pods remain", async () => {
    const f = await setup(false, undefined, 1), prepared = await f.prepare();
    const old = f.pods.get("core")!.filter(pod => pod.metadata.uid.startsWith("bridge-pod"));
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "get" && args[1] === "pods") {
        const inventory = JSON.parse(result);
        inventory.items = [...inventory.items.filter((pod: any) => !pod.metadata.uid.startsWith("bridge-pod")), ...old];
        return JSON.stringify(inventory);
      }
      return result;
    };
    vi.useFakeTimers();
    try {
      const result = expect(retireRootMigration(run, prepared, f.journal)).rejects.toThrow("timed out");
      await vi.runAllTimersAsync();
      await result;
      expect(f.last().phase).toBe("retiring");
      expect(f.root.spec.template.spec.containers[0].image).toBe(ORIGINAL);
      expect(f.grant("core").spec.writers).toEqual([]);
    } finally { vi.useRealTimers(); }
  });

  it.each(["old-pod", "wrong-epoch", "unknown-owner", "unready"])("withholds writers for %s companion execution", async fault => {
    const f = await setup(true, undefined, 1), prepared = await f.prepare();
    const old = f.pods.get("core")!.find(pod => pod.metadata.uid.startsWith("bridge-pod"));
    const retired = await retireRootMigration(f.execute, prepared, f.journal);
    f.rotate(2);
    const run: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[2] === "kars-bridge-bff" && !args.includes("--dry-run=server") && f.companion.spec.replicas === 1) {
        const pod = f.pods.get("core")!.find(pod => pod.metadata.uid.startsWith("bridge-pod"));
        if (fault === "old-pod") pod.metadata.uid = old.metadata.uid;
        if (fault === "wrong-epoch") pod.metadata.annotations[`${P}epoch`] = "stale";
        if (fault === "unknown-owner") pod.metadata.ownerReferences[0].uid = "unreviewed";
        if (fault === "unready") pod.status.conditions[0].status = "False";
      }
      return result;
    };
    await expect(finalizeRootMigration(run, retired, f.journal, applyReviewedGrant)).rejects.toThrow();
    expect(f.grant("core").spec.writers).toEqual([]);
  });
});

describe("retired legacy budget TLS ownership", () => {
  const data = (() => {
    const directory = mkdtempSync(join(tmpdir(), "kars-root-rotation-"));
    try {
      const certificate = join(directory, "tls.crt"), key = join(directory, "tls.key");
      execFileSync("openssl", ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
        "-nodes", "-keyout", key, "-out", certificate, "-subj", "/CN=rotation.test", "-days", "2",
        "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign"], { stdio: ["ignore", "pipe", "pipe"] });
      return {
        "tls.crt": readFileSync(certificate).toString("base64"),
        "tls.key": readFileSync(key).toString("base64"),
      };
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  })();
  async function fixture() {
    const f = await setup(true);
    Object.assign(f.root.metadata, { labels: { ...f.root.metadata.labels, "app.kubernetes.io/managed-by": "Helm" } });
    Object.assign(f.ca.metadata, { namespace: "core", labels: { "app.kubernetes.io/managed-by": "Helm" },
      annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" } });
    Object.assign(f.secret.data, { "tls.key": "old-private-fixture", "retained": "unchanged" });
    Object.assign(f.secret.metadata.annotations, { "customer.example/retain": "yes" });
    Object.assign(f.secret.metadata, { labels: { "customer.example/retain": "yes" }, finalizers: ["customer.example/retain"],
      managedFields: [{ manager: "legacy-issuer", operation: "Update", apiVersion: "v1", fieldsType: "FieldsV1", fieldsV1: { "f:data": {} } }] });
    const retired = await retireRootMigration(f.execute, await f.prepare(), f.journal);
    return { ...f, retired, expected: structuredClone(f.secret), next: { ...f.secret.data, ...data } };
  }
  it("prepares exact atomic ownership/rotation CAS without writes and finalizes the new revision", async () => {
    const f = await fixture(), count = f.mutations.length;
    const reads: string[][] = [];
    const patch = await prepareRootBudgetTlsRotation(async (args, input) => {
      reads.push(args);
      return f.execute(args, input);
    }, f.retired, f.expected, f.next);
    expect(reads.some(args => args.includes("--show-managed-fields=true"))).toBe(true);
    expect(patch[2]!.value).toEqual(f.secret.metadata);
    expect(f.mutations).toHaveLength(count);
    expect(patch.filter(p => p.op === "test").map(p => p.path)).toEqual(["/metadata/uid", "/metadata/resourceVersion", "/metadata", "/type", "/data"]);
    expect(patch.find(p => p.path === "/metadata/annotations" && p.op === "add")?.value).toEqual({
      ...f.secret.metadata.annotations, "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
    expect(patch.find(p => p.path === "/metadata/labels" && p.op === "add")?.value).toEqual({ "customer.example/retain": "yes", "app.kubernetes.io/managed-by": "Helm" });
    Object.assign(f.secret.metadata, { annotations: patch[5]!.value, labels: patch[6]!.value });
    f.secret.data = f.next;
    f.secret.metadata.resourceVersion = String(Number(f.secret.metadata.resourceVersion) + 1);
    f.ca.data["ca.crt"] = Buffer.from(data["tls.crt"], "base64").toString();
    f.ca.metadata.resourceVersion = String(Number(f.ca.metadata.resourceVersion) + 1);
    await expect(prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next)).rejects.toThrow();
    await finalizeRootMigration(f.execute, f.retired, f.journal, applyReviewedGrant);
    expect(f.grant("core").spec.privateActivation.root.budgetTls.secret.resourceVersion).toBe(f.secret.metadata.resourceVersion);
    expect((f.secret.metadata as any).finalizers).toEqual(["customer.example/retain"]);
  });
  it("preserves compatible existing release ownership", async () => {
    const f = await fixture();
    Object.assign(f.secret.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" });
    Object.assign((f.secret.metadata as any).labels, { "app.kubernetes.io/managed-by": "Helm" });
    f.expected = structuredClone(f.secret);
    const count = f.mutations.length;
    const patch = await prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next);
    expect(patch[5]!.value).toEqual(f.secret.metadata.annotations);
    expect(patch[6]!.value).toEqual((f.secret.metadata as any).labels);
    expect(f.mutations).toHaveLength(count);
  });
  it.each(["uid", "version", "data", "metadata", "owner", "manager", "owner-reference", "missing-managed-fields", "deleting", "immutable", "invalid-key", "qualified", "replicas", "pods", "catalog", "stale-key", "unrelated-data", "plan"])(
    "refuses %s without writes", async fault => {
      const f = await fixture();
      if (fault === "uid") f.secret.metadata.uid = "different";
      if (fault === "version") f.secret.metadata.resourceVersion = "999";
      if (fault === "data") Object.assign(f.secret.data, { retained: "changed" });
      if (fault === "metadata") Object.assign(f.secret.metadata.annotations, { extra: "changed" });
      if (fault === "owner") Object.assign(f.secret.metadata.annotations, { "meta.helm.sh/release-name": "other" });
      if (fault === "manager") Object.assign((f.secret.metadata as any).labels, { "app.kubernetes.io/managed-by": "other" });
      if (fault === "owner-reference") Object.assign(f.secret.metadata, { ownerReferences: [{ uid: "another-owner" }] });
      if (fault === "missing-managed-fields") delete (f.expected.metadata as any).managedFields;
      if (fault === "deleting") Object.assign(f.secret.metadata, { deletionTimestamp: new Date().toISOString() });
      if (fault === "immutable") Object.assign(f.secret, { immutable: true });
      if (fault === "invalid-key") f.next["tls.key"] = Buffer.from("invalid private key").toString("base64");
      if (["owner", "manager", "owner-reference", "deleting", "immutable"].includes(fault)) f.expected = structuredClone(f.secret);
      if (fault === "qualified") f.namespace("core").metadata.annotations[`${P}state`] = "Qualified";
      if (fault === "replicas") f.root.spec.replicas = 1;
      if (fault === "pods") f.pods.set("core", [rootPod(f, "unexpected")]);
      if (fault === "catalog") Object.assign((f.ca.metadata as any).annotations, { "meta.helm.sh/release-name": "other" });
      if (fault === "stale-key") f.next = { ...f.secret.data } as typeof f.next;
      if (fault === "unrelated-data") Object.assign(f.next, { retained: "changed" });
      if (fault === "plan") f.retired.planDigest = "0".repeat(64);
      const count = f.mutations.length;
      await expect(prepareRootBudgetTlsRotation(f.execute, f.retired, f.expected, f.next)).rejects.toThrow();
      expect(f.mutations).toHaveLength(count);
    });
});
