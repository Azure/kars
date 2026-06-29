// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  parseVersionTag,
  compareVersions,
  releaseImagePlan,
  releasesBetween,
  type ReleaseNote,
} from "./release.js";
import { buildHelmUpgradeArgs, summarizeChangelog } from "../commands/upgrade.js";

describe("parseVersionTag", () => {
  it("parses stable + prerelease tags (v optional)", () => {
    expect(parseVersionTag("v0.1.16")).toMatchObject({ major: 0, minor: 1, patch: 16, pre: [] });
    expect(parseVersionTag("0.1.16")).toMatchObject({ major: 0, minor: 1, patch: 16 });
    expect(parseVersionTag("v0.1.16-interim.3")).toMatchObject({ pre: ["interim", "3"] });
    expect(parseVersionTag("nonsense")).toBeNull();
  });
});

describe("compareVersions", () => {
  it("orders by major.minor.patch", () => {
    expect(compareVersions("v0.1.16", "v0.1.15")).toBeGreaterThan(0);
    expect(compareVersions("v0.1.15", "v0.1.16")).toBeLessThan(0);
    expect(compareVersions("v0.2.0", "v0.1.99")).toBeGreaterThan(0);
    expect(compareVersions("v1.0.0", "v0.9.9")).toBeGreaterThan(0);
  });
  it("treats equal versions as 0", () => {
    expect(compareVersions("v0.1.16", "0.1.16")).toBe(0);
  });
  it("ranks a stable release above its prerelease", () => {
    expect(compareVersions("v0.1.16", "v0.1.16-interim.3")).toBeGreaterThan(0);
    expect(compareVersions("v0.1.16-interim.3", "v0.1.16")).toBeLessThan(0);
  });
  it("orders prerelease identifiers numerically then lexically", () => {
    expect(compareVersions("v0.1.16-interim.2", "v0.1.16-interim.10")).toBeLessThan(0);
    expect(compareVersions("v0.1.16-alpha", "v0.1.16-beta")).toBeLessThan(0);
  });
});

describe("releaseImagePlan", () => {
  it("includes required core + mesh images with -agt mesh tags", () => {
    const plan = releaseImagePlan("v0.1.16");
    const targets = plan.map((p) => p.target);
    expect(targets).toContain("kars-controller:latest");
    expect(targets).toContain("kars-inference-router:latest");
    expect(targets).toContain("openclaw-sandbox:latest");
    expect(targets).toContain("agentmesh-relay-agt:latest");
    expect(targets).toContain("agentmesh-registry-agt:latest");
    // core + mesh are required
    for (const t of ["kars-controller:latest", "agentmesh-relay-agt:latest"]) {
      expect(plan.find((p) => p.target === t)?.required).toBe(true);
    }
    // version is threaded into the GHCR source
    expect(plan[0].src).toContain(":v0.1.16");
  });
  it("can skip runtime adapters", () => {
    const withRt = releaseImagePlan("v0.1.16", { includeRuntimes: true });
    const noRt = releaseImagePlan("v0.1.16", { includeRuntimes: false });
    expect(withRt.length).toBeGreaterThan(noRt.length);
    expect(noRt.every((p) => !p.target.startsWith("kars-runtime-"))).toBe(true);
  });
});

describe("buildHelmUpgradeArgs", () => {
  const ctx = {
    acrLoginServer: "karsacr.azurecr.io",
    aksCluster: "kars-aks",
    resourceGroup: "kars-eastus2",
    wiClientId: "wi-123",
    keyVaultName: "kars-kv",
    foundryEndpoint: "https://x.services.ai.azure.com/api/projects/p",
  };
  it("is atomic and pins :latest image tags", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart");
    expect(args).toContain("--atomic");
    expect(args).toContain("upgrade");
    expect(args).toContain("--install");
    expect(args.join(" ")).toContain("controller.image.repository=karsacr.azurecr.io/kars-controller");
    expect(args.join(" ")).toContain("controller.image.tag=latest");
    expect(args.join(" ")).toContain("inferenceRouter.azure.openai.endpoint=https://x.services.ai.azure.com/api/projects/p");
  });
  it("omits the foundry endpoint when absent", () => {
    const args = buildHelmUpgradeArgs({ ...ctx, foundryEndpoint: undefined }, "/chart");
    expect(args.join(" ")).not.toContain("inferenceRouter.azure.openai.endpoint");
  });
});

describe("releasesBetween", () => {
  const rels: ReleaseNote[] = [
    { tag: "v0.1.18", name: "v0.1.18", body: "" },
    { tag: "v0.1.17", name: "v0.1.17", body: "" },
    { tag: "v0.1.16", name: "v0.1.16", body: "" },
    { tag: "v0.1.15", name: "v0.1.15", body: "" },
    { tag: "v0.1.14", name: "v0.1.14", body: "" },
  ];
  it("returns releases newer than current up to target, oldest→newest", () => {
    expect(releasesBetween(rels, "v0.1.15", "v0.1.18").map((r) => r.tag))
      .toEqual(["v0.1.16", "v0.1.17", "v0.1.18"]);
  });
  it("excludes the current version and anything above target", () => {
    const got = releasesBetween(rels, "v0.1.16", "v0.1.17").map((r) => r.tag);
    expect(got).toEqual(["v0.1.17"]);
    expect(got).not.toContain("v0.1.16");
    expect(got).not.toContain("v0.1.18");
  });
  it("with no known current, includes everything up to target", () => {
    expect(releasesBetween(rels, "", "v0.1.16").map((r) => r.tag))
      .toEqual(["v0.1.14", "v0.1.15", "v0.1.16"]);
  });
});

describe("summarizeChangelog", () => {
  it("extracts bullet lines and skips the title + boilerplate", () => {
    const msg = [
      "kars v0.1.17",
      "",
      "- First feature",
      "* Second feature",
      "",
      "## Container images",
      "- ghcr.io/azure/kars-controller:v0.1.17",
    ].join("\n");
    const out = summarizeChangelog(msg);
    expect(out).toEqual(["• First feature", "• Second feature"]);
  });
  it("falls back to prose when there are no bullets", () => {
    const out = summarizeChangelog("kars v0.1.5\n\nJust a prose summary line.");
    expect(out).toEqual(["Just a prose summary line."]);
  });
  it("caps the number of bullet lines", () => {
    const many = ["kars v1.0.0", ...Array.from({ length: 20 }, (_, i) => `- item ${i}`)].join("\n");
    const out = summarizeChangelog(many, 8);
    expect(out.length).toBeLessThanOrEqual(9); // 8 + the "…" marker
    expect(out[out.length - 1]).toBe("…");
  });
});
