// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { at, canonical, type Execute } from "./private-activation.js";
import { assertPublicationWitnessStopped, restorePublicationWitness, stopPublicationWitness } from "./private-publication-witness.js";
import type { RootMigrationJournal, RootMigrationReceipt } from "./private-root-migration.js";
import type { ObjectMap } from "./schema-documents.js";

function fixture(count = 1) {
  const live: ObjectMap = { apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "aggregator", namespace: "custom-witness", uid: "writer-uid", resourceVersion: "1", generation: 1,
    labels: { "app.kubernetes.io/managed-by": "Helm", "kars.azure.com/witness-addon": "true" },
    annotations: { "meta.helm.sh/release-name": "custom-release", "meta.helm.sh/release-namespace": "custom-core" },
  }, spec: { replicas: count, selector: { matchLabels: { "app.kubernetes.io/component": "aggregator" } },
    template: { metadata: { labels: { "app.kubernetes.io/component": "aggregator" } },
      spec: { containers: [{ name: "aggregator", image: "witness:original" }] } } },
  status: { observedGeneration: 1, replicas: count } };
  const desired = structuredClone(live);
  desired.spec.replicas = 1;
  desired.spec.template.spec.containers[0].image = "witness:updated";
  const documents = [{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "kars-datapath-witness",
    namespace: "custom-core", labels: { "kars.azure.com/witness-addon": "true" } }, data: {} }, desired];
  const receipt = { publication: { phase: "preparing", release: "custom-release", namespace: "custom-core" } } as RootMigrationReceipt;
  const saved: RootMigrationReceipt[] = [], writes: number[] = [];
  const controls = { loseAck: false, badAck: false, driftOnPause: false, observed: true, pods: [] as ObjectMap[],
    replicasets: [] as ObjectMap[], horizontalpodautoscalers: [] as ObjectMap[] };
  const journal = { save: async (value: RootMigrationReceipt) => { saved.push(structuredClone(value)); } } as RootMigrationJournal;
  const run: Execute = async (args, input) => {
    expect(args).toContain("custom-witness");
    if (args[0] === "get" && ["pods", "replicasets", "horizontalpodautoscalers"].includes(args[1]!)) {
      return JSON.stringify({ metadata: {}, items: controls[args[1] as "pods" | "replicasets" | "horizontalpodautoscalers"] });
    }
    if (args[0] === "get" && args[1]?.toLowerCase() === "deployment") return JSON.stringify(live);
    if (args[0] === "patch") {
      const patch = JSON.parse(input!);
      expect(saved.length).toBeGreaterThan(0);
      expect(patch.slice(0, 3)).toEqual([
        { op: "test", path: "/metadata/uid", value: live.metadata.uid },
        { op: "test", path: "/metadata/resourceVersion", value: live.metadata.resourceVersion },
        { op: "test", path: "/spec", value: live.spec },
      ]);
      const count = patch[3].value;
      writes.push(count);
      live.spec.replicas = count;
      live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
      live.metadata.generation++;
      if (controls.observed) live.status = { observedGeneration: live.metadata.generation, replicas: count };
      if (controls.driftOnPause) live.spec.template.spec.containers[0].image = "witness:drift";
      if (controls.loseAck) { controls.loseAck = false; throw new Error("acknowledgement lost"); }
      if (controls.badAck) return JSON.stringify({ ...live, spec: { ...live.spec, replicas: 8 } });
      return JSON.stringify(live);
    }
    throw new Error(`Unexpected request ${args.join(" ")}`);
  };
  const stop = () => stopPublicationWitness(run, receipt, journal, documents);
  const verified = () => {
    live.spec = structuredClone(desired.spec);
    receipt.publication!.witnessWriter!.publishedSpec = structuredClone(live.spec);
    receipt.publication!.phase = "verified";
  };
  return { live, desired, documents, receipt, saved, writes, controls, journal, run, stop, verified,
    restore: () => restorePublicationWitness(run, receipt, journal, documents) };
}

afterEach(() => vi.restoreAllMocks());

describe("protected Core datapath writer lifecycle", () => {
  it.each([0, 1])("journals and restores original replica intent %s without reverting the new template", async count => {
    const f = fixture(count);
    await f.stop();
    expect(at(f.saved[0], "publication", "witnessWriter", "deployment", "spec", "replicas")).toBe(count);
    expect(f.live.spec.replicas).toBe(0);
    expect(f.desired.spec.replicas).toBe(0);
    await assertPublicationWitnessStopped(f.run, f.receipt);
    f.verified();
    await f.restore();
    expect(f.live.spec.replicas).toBe(count);
    expect(f.live.spec.template.spec.containers[0].image).toBe("witness:updated");
    expect(f.receipt.publication!.witnessWriter!.restored).toBe(true);
    const writes = f.writes.length;
    await f.restore();
    expect(f.writes).toHaveLength(writes);
  });

  it("leaves publications without the maintained witness untouched", async () => {
    const f = fixture();
    await stopPublicationWitness(f.run, f.receipt, f.journal, []);
    expect(f.writes).toEqual([]);
    expect(f.receipt.publication!.witnessWriter).toBeUndefined();
  });

  it("preserves pause intent and resumes after lost acknowledgement", async () => {
    const f = fixture();
    f.controls.loseAck = true;
    await expect(f.stop()).rejects.toThrow("acknowledgement lost");
    expect(at(f.saved[0], "publication", "witnessWriter", "deployment", "spec", "replicas")).toBe(1);
    await f.stop();
    expect(f.writes).toEqual([0]);
  });

  it("resumes an acknowledged restore without a second scale mutation", async () => {
    const f = fixture();
    await f.stop();
    f.verified();
    f.controls.loseAck = true;
    await expect(f.restore()).rejects.toThrow("acknowledgement lost");
    expect(f.receipt.publication!.witnessWriter!.restored).toBeUndefined();
    await f.restore();
    expect(f.writes).toEqual([0, 1]);
  });

  it.each(["lingering pods", "unobserved generation", "active ReplicaSet", "foreign ReplicaSet", "autoscaler"])("refuses %s", async mode => {
    const f = fixture();
    if (mode === "lingering pods") f.controls.pods = [{ metadata: { name: "terminating" } }];
    if (mode === "unobserved generation") f.controls.observed = false;
    if (mode.includes("ReplicaSet")) f.controls.replicasets = [{ metadata: { generation: 1,
      ownerReferences: [{ kind: "Deployment", controller: true, uid: mode === "foreign ReplicaSet" ? "other" : "writer-uid" }] },
    spec: { replicas: 1 }, status: { observedGeneration: 1, replicas: 1 } }];
    if (mode === "autoscaler") f.controls.horizontalpodautoscalers = [{ spec: { scaleTargetRef: { kind: "Deployment", name: "aggregator" } } }];
    vi.spyOn(Date, "now").mockReturnValueOnce(0).mockReturnValue(120_001);
    await expect(f.stop()).rejects.toThrow("writer changed or has not stopped");
    expect(f.receipt.publication!.witnessWriter!.restored).toBeUndefined();
  });

  it.each(["foreign release", "missing writer", "duplicate writer", "bad acknowledgement", "spec drift"])("refuses %s", async mode => {
    const f = fixture();
    if (mode === "foreign release") f.live.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (mode === "missing writer") f.documents.pop();
    if (mode === "duplicate writer") f.documents.push(structuredClone(f.desired));
    if (mode === "bad acknowledgement") f.controls.badAck = true;
    if (mode === "spec drift") f.controls.driftOnPause = true;
    await expect(f.stop()).rejects.toThrow("writer changed or has not stopped");
  });

  it.each(["not verified", "replaced UID", "changed template", "unexpected replicas"])("refuses restoration when %s", async mode => {
    const f = fixture();
    await f.stop();
    f.verified();
    if (mode === "not verified") f.receipt.publication!.phase = "succeeded";
    if (mode === "replaced UID") f.live.metadata.uid = "replacement";
    if (mode === "changed template") f.live.spec.template.spec.containers[0].image = "unreviewed";
    if (mode === "unexpected replicas") f.live.spec.replicas = 2;
    const before = canonical(f.live);
    await expect(f.restore()).rejects.toThrow("writer changed or has not stopped");
    expect(canonical(f.live)).toBe(before);
  });
});
