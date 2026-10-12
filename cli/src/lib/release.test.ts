// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { parse } from "yaml";
import {
  parseVersionTag,
  compareVersions,
  releaseImagePlan,
  releasesBetween,
  type ReleaseNote,
} from "./release.js";
import { buildHelmUpgradeArgs, summarizeChangelog } from "../commands/upgrade.js";

describe("native Hermes release wiring", () => {
  interface Step {
    name?: string;
    uses?: string;
    run?: string;
    with?: Record<string, string | boolean>;
  }
  interface Job {
    "runs-on"?: string;
    needs?: string[];
    strategy?: { matrix: { runtime: Array<{ name: string; ghcr: string; arch?: string; runner?: string; suffix?: string }> } };
    steps: Step[];
  }
  const workflow: { jobs: Record<string, Job> } = parse(readFileSync(
    new URL("../../../.github/workflows/release-public-interim.yml", import.meta.url), "utf8",
  ));
  const build = workflow.jobs["build-runtimes"];
  const merge = workflow.jobs["merge-sandbox"];

  it("builds Hermes on both native runners without expanding other adapters", () => {
    const runtimes = build.strategy!.matrix.runtime;
    expect(runtimes.filter(runtime => runtime.name === "hermes")).toEqual([
      { name: "hermes", ghcr: "kars-runtime-hermes", arch: "amd64", runner: "ubuntu-22.04", suffix: "-amd64" },
      { name: "hermes", ghcr: "kars-runtime-hermes", arch: "arm64", runner: "ubuntu-24.04-arm", suffix: "-arm64" },
    ]);
    expect(build["runs-on"]).toBe("${{ matrix.runtime.runner || 'ubuntu-22.04' }}");
    expect(runtimes.filter(runtime => runtime.name !== "hermes").every(runtime => !runtime.arch && !runtime.suffix)).toBe(true);
    expect(build.steps.some(step => step.uses?.includes("setup-qemu"))).toBe(false);
    expect(build.steps.find(step => step.name?.startsWith("Guard"))?.run).toContain("platform.machine()");
  });

  it("keeps per-architecture pushes and SBOM artifacts separate until merging", () => {
    const push = build.steps.find(step => step.uses?.startsWith("docker/build-push-action"))!.with!;
    expect(push.platforms).toBe("linux/${{ matrix.runtime.arch || 'amd64' }}");
    expect(String(push.tags).trim().split("\n").every(tag => tag.endsWith("${{ matrix.runtime.suffix }}"))).toBe(true);
    const sbom = build.steps.find(step => step.name === "Generate SBOM (SPDX JSON)")!.with!;
    const upload = build.steps.find(step => step.name === "Upload SBOM")!.with!;
    expect(sbom.image).toBe("${{ env.REGISTRY }}/${{ matrix.runtime.ghcr }}:${{ env.VERSION }}${{ matrix.runtime.suffix }}");
    expect(sbom["output-file"]).toContain("${{ matrix.runtime.suffix }}");
    expect(upload.path).toBe(sbom["output-file"]);
    expect(upload.name).toContain("${{ matrix.runtime.suffix }}");
  });

  it("merges and signs Hermes only after both native builds", () => {
    expect(merge.needs).toContain("build-runtimes");
    const commands = merge.steps.find(step => step.name === "Create multi-arch manifests (:VERSION + :latest)")!.run!;
    expect(commands).toContain("for name in kars-sandbox-base openclaw-sandbox kars-runtime-hermes;");
    expect(commands).toContain('"${REGISTRY}/${name}:${VERSION}-amd64"');
    expect(commands).toContain('"${REGISTRY}/${name}:${VERSION}-arm64"');
    expect(commands).toContain('echo "hermes=${DIGEST}"');
    expect(merge.steps.find(step => step.name === "Attest Hermes manifest provenance")?.with).toMatchObject({
      "subject-name": "${{ env.REGISTRY }}/kars-runtime-hermes",
      "subject-digest": "${{ steps.digests.outputs.hermes }}",
      "push-to-registry": true,
    });
    expect(merge.steps.find(step => step.name === "Cosign keyless sign manifests")?.run)
      .toContain("kars-runtime-hermes@${{ steps.digests.outputs.hermes }}");
    expect(workflow.jobs.release.needs).toContain("merge-sandbox");
    expect(workflow.jobs.release.steps.some(step => step.run?.includes("${{ needs.merge-sandbox.outputs.hermes_digest }}"))).toBe(true);
  });
});

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
    expect(targets).toContain("kars-metrics-server:latest");
    expect(targets).toContain("kars-headlamp:latest");
    expect(targets).toContain("openclaw-sandbox:latest");
    expect(targets).toContain("agentmesh-relay-agt:latest");
    expect(targets).toContain("agentmesh-registry-agt:latest");
    // core + mesh are required
    for (const t of ["kars-controller:latest", "kars-metrics-server:latest", "kars-headlamp:latest", "agentmesh-relay-agt:latest"]) {
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
    expect(args.join(" ")).toContain("headlamp.image.repository=karsacr.azurecr.io/kars-headlamp");
    expect(args.join(" ")).toContain("headlamp.image.tag=latest");
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
