// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { coreWitnessUpgradeFlags } from "./core-helm-witness.js";
import type { SchemaExecute } from "./schema-documents.js";

vi.mock("node:fs", async importOriginal => ({ ...await importOriginal<typeof import("node:fs")>(), readFileSync: vi.fn() }));
afterEach(() => vi.clearAllMocks());

function fixture() {
  const adoption: any = { immutable: true, metadata: { name: "kars-sre-helm-adoption", namespace: "kars-system",
    uid: "receipt-uid", resourceVersion: "1", annotations: { "meta.helm.sh/release-name": "kars",
      "meta.helm.sh/release-namespace": "kars-system" } }, data: { intentUID: "intent-uid", valuesDigest: "a".repeat(64) } };
  const witness = { enabled: true, managed: true, sandboxes: ["sre"], nodes: ["approved-node"],
    aggregator: { image: `registry.example/witness:latest@sha256:${"a".repeat(64)}` },
    gadget: { image: `registry.example/witness:latest@sha256:${"a".repeat(64)}`, hookMode: "podinformer" } };
  const saved: any = { datapathWitness: witness, sre: {} };
  const execute = vi.fn<SchemaExecute>(async (file, args) => {
    if (file === "kubectl") {
      expect(args).toEqual(["get", "configmap", "kars-sre-helm-adoption", "-n", "kars-system", "--ignore-not-found", "-o", "json"]);
      return { stdout: JSON.stringify(adoption) };
    }
    expect(args).toEqual(["get", "values", "kars", "-n", "kars-system", "--all", "-o", "json"]);
    return { stdout: JSON.stringify(saved) };
  });
  const run = (overlays: string[] = [], assignments: string[] = []) => coreWitnessUpgradeFlags(execute, "kars", "kars-system", overlays, assignments);
  return { adoption, saved, execute, run };
}

describe("Core Helm original witness baseline capture", () => {
  it("captures effective original values without inventing an original scope", async () => {
    const f = fixture();
    expect(await f.run()).toEqual(["--set-json", `sre.datapathWitnessBaseline=${JSON.stringify(f.saved.datapathWitness)}`,
      "--set-json", 'datapathWitness.scope="selected"']);
    expect(f.saved.datapathWitness).not.toHaveProperty("scope");
  });

  it("captures the stored parent's inherited profile, never a new overlay or a recomputed existing baseline", async () => {
    const f = fixture();
    f.saved.global = { installationProfile: "kind-beta" };
    f.saved.datapathWitness.global = {};
    vi.mocked(readFileSync).mockReturnValue("global:\n  installationProfile: changed\n");
    const flags = await f.run(["reviewed.yaml"]);
    const baseline = JSON.parse(flags[1].slice("sre.datapathWitnessBaseline=".length));
    expect(baseline.global).toEqual({ installationProfile: "kind-beta" });
    expect(f.saved.datapathWitness.global).toEqual({});
    f.saved.sre.datapathWitnessBaseline = baseline;
    f.saved.global.installationProfile = "changed";
    expect((await f.run())[1]).toBe(flags[1]);
  });

  it.each(["overlay", "assignment", "combined assignment"])("honors explicit fleet scope through %s", async source => {
    const f = fixture();
    vi.mocked(readFileSync).mockReturnValue("datapathWitness:\n  scope: all\n  sandboxes: []\n");
    const flags = await f.run(source === "overlay" ? ["reviewed.yaml"] : [], source === "assignment" ? ["datapathWitness.scope=all"]
      : source === "combined assignment" ? ["sre.enabled=true,datapathWitness.scope=all"] : []);
    expect(flags).toEqual(["--set-json", `sre.datapathWitnessBaseline=${JSON.stringify(f.saved.datapathWitness)}`]);
  });

  it("retains the ORIGINAL baseline across fleet and subsequent image refreshes", async () => {
    const f = fixture();
    const original = structuredClone(f.saved.datapathWitness);
    f.saved.sre.datapathWitnessBaseline = original;
    Object.assign(f.saved.datapathWitness, { scope: "all", sandboxes: [] });
    for (const image of [`mirror.example/reviewed/witness:latest@sha256:${"b".repeat(64)}`,
      `another.example/collector:reviewed@sha256:${"c".repeat(64)}`]) {
      f.saved.datapathWitness.aggregator.image = image;
      f.saved.datapathWitness.gadget.image = image;
      expect(await f.run()).toEqual(["--set-json", `sre.datapathWitnessBaseline=${JSON.stringify(original)}`,
        "--set-json", 'datapathWitness.scope="all"']);
    }
    vi.mocked(readFileSync).mockReturnValue(JSON.stringify({ sre: { datapathWitnessBaseline: original } }));
    expect(await f.run(["reviewed.yaml"])).toContain(`sre.datapathWitnessBaseline=${JSON.stringify(original)}`);
  });

  it("rejects overriding the saved original baseline with a changed overlay or --set", async () => {
    const f = fixture();
    vi.mocked(readFileSync).mockReturnValue(JSON.stringify({ sre: { datapathWitnessBaseline: { forged: true } } }));
    await expect(f.run(["reviewed.yaml"])).rejects.toThrow("original witness baseline");
    for (const assignment of ["sre.datapathWitnessBaseline={}", "sre.datapathWitnessBaseline.scope=all",
      "sre.enabled=true,sre.datapathWitnessBaseline.nodes[0]=other"]) {
      await expect(f.run([], [assignment])).rejects.toThrow("do not override");
    }
  });

  it("does not capture a disabled, unmanaged or unadopted witness", async () => {
    const f = fixture();
    f.saved.datapathWitness.enabled = false;
    expect(await f.run()).toEqual([]);
    f.saved.datapathWitness.enabled = true;
    f.saved.datapathWitness.managed = false;
    expect(await f.run()).toEqual([]);
    f.execute.mockResolvedValueOnce({ stdout: "" });
    expect(await f.run()).toEqual([]);
  });

  it("fails closed on read failures, malformed overlays and foreign receipts", async () => {
    const f = fixture();
    f.execute.mockRejectedValueOnce(new Error("forbidden"));
    await expect(f.run()).rejects.toThrow("forbidden");
    vi.mocked(readFileSync).mockReturnValue("invalid: [\n");
    await expect(f.run(["reviewed.yaml"])).rejects.toThrow("Invalid Core values overlay");
    f.adoption.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    await expect(f.run()).rejects.toThrow("original immutable Core adoption receipt");
  });
});
