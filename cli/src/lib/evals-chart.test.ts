// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
async function runnerEnv(args: string[] = []) {
  const { stdout } = await execa("helm", ["template", "kars", chart, "-n", "kars-system",
    "--show-only", "templates/controller-deployment.yaml", ...args]);
  const deployment = parseAllDocuments(stdout).map(d => d.toJSON()).find(d => d.kind === "Deployment");
  return deployment.spec.template.spec.containers.flatMap((c: { env: { name: string; value: string }[] }) => c.env)
    .filter((e: { name: string }) => e.name === "KARS_CONFORMANCE_RUNNER_IMAGE");
}

describe("default safety evaluation runner", () => {
  it("uses the maintained public AL3 release image without an eval-specific image override", async () => {
    expect(await runnerEnv()).toEqual([
      { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: "ghcr.io/azure/kars-conformance-runner:latest" },
    ]);
    const workflow = readFileSync(new URL("../../../.github/workflows/release-public-interim.yml", import.meta.url), "utf8");
    const images = parseAllDocuments(workflow)[0].toJSON().jobs["build-images"].strategy.matrix.image;
    expect(images.find((image: { name: string }) => image.name === "conformance-runner")).toMatchObject({
      ghcr: "kars-conformance-runner", dockerfile: "sandbox-images/conformance-runner/Dockerfile",
      platforms: "linux/amd64,linux/arm64",
    });
  });

  it("uses the image loaded by the Kind installer", async () => {
    expect(await runnerEnv(["-f", join(chart, "values-local-dev.yaml")])).toEqual([
      { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: "kars-conformance-runner:latest" },
    ]);
  });

  it("preserves explicit image and legacy extraEnv overrides without duplicate environment entries", async () => {
    const image = `mirror.example/runner@sha256:${"a".repeat(64)}`;
    expect(await runnerEnv(["--set-string", `evals.runnerImage=${image}`])).toEqual([
      { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: image },
    ]);
    expect(await runnerEnv(["--set-string", "controller.extraEnv[0].name=KARS_CONFORMANCE_RUNNER_IMAGE",
      "--set-string", `controller.extraEnv[0].value=${image}`])).toEqual([
      { name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: image },
    ]);
  });
});
