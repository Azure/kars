// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";
import { coreImageValues, dockerPushDigest, imageValueArgs, RUNTIME_IMAGE_TARGETS, splitImage } from "./image-targets.js";
import { meshImageValueArgs } from "./mesh-release.js";

const digest = `sha256:${"b".repeat(64)}`;
const helmAvailable = spawnSync("helm", ["version", "--short"], { stdio: "pipe" }).status === 0;

describe("immutable pushed artifact configuration", () => {
  it("binds application to Docker's pushed manifest instead of a later concurrent latest tag", () => {
    expect(dockerPushDigest(`latest: digest: ${digest} size: 1234`)).toBe(digest);
    expect(dockerPushDigest("Pushed successfully without digest evidence")).toBeUndefined();
    expect(dockerPushDigest(`digest: ${digest}\ndigest: sha256:${"c".repeat(64)}`)).toBeUndefined();
  });

  it("retains latest while binding it to a valid digest", () => {
    expect(splitImage(`mirror.azurecr.io/agent:latest@${digest}`)).toEqual({
      repository: "mirror.azurecr.io/agent", tag: `latest@${digest}`,
    });
    expect(() => splitImage("mirror.azurecr.io/agent:latest@sha256:invalid")).toThrow();
    expect(() => splitImage("mirror.azurecr.io/agent:latest,other=value")).toThrow();
  });

  it.skipIf(!helmAvailable)("renders every selected core/runtime/mesh artifact through the actual chart", () => {
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const images = [
      { name: "controller", image: `mirror.azurecr.io/kars-controller:latest@${digest}` },
      { name: "router", image: `mirror.azurecr.io/kars-inference-router:latest@${digest}` },
      { name: "sandbox", image: `mirror.azurecr.io/openclaw-sandbox:latest@${digest}` },
      ...RUNTIME_IMAGE_TARGETS.map(runtime => ({ name: runtime.name, image: `mirror.azurecr.io/${runtime.repo}:latest@${digest}` })),
    ];
    const mesh = { registry: `mirror.azurecr.io/agentmesh-registry-agt:latest@${digest}`,
      relay: `mirror.azurecr.io/agentmesh-relay-agt:latest@${digest}` };
    const rendered = execFileSync("helm", ["template", "kars", chart, "--namespace", "kars-system",
      "--set", "agentMesh.enabled=true", ...imageValueArgs(coreImageValues(images)), ...meshImageValueArgs(mesh)], { encoding: "utf8" });
    const resources = parseAllDocuments(rendered).map(document => document.toJSON()) as Array<{
      kind: string; metadata: { name: string };
      spec?: { template?: { spec?: { containers?: Array<{ image: string; env?: Array<{ name: string; value?: string }> }> } } };
    }>;
    const controller = resources.find(item => item.kind === "Deployment" && item.metadata.name === "kars-controller")!
      .spec!.template!.spec!.containers![0];
    expect(controller.image).toBe(images[0].image);
    expect(controller.env).toContainEqual(expect.objectContaining({ name: "INFERENCE_ROUTER_IMAGE", value: images[1].image }));
    expect(controller.env).toContainEqual(expect.objectContaining({ name: "SANDBOX_IMAGE", value: images[2].image }));
    for (const runtime of RUNTIME_IMAGE_TARGETS) {
      expect(controller.env).toContainEqual(expect.objectContaining({
        name: runtime.env, value: images.find(item => item.name === runtime.name)!.image,
      }));
    }
    for (const name of ["registry", "relay"] as const) {
      expect(resources.find(item => item.kind === "Deployment" && item.metadata.name === name)!
        .spec!.template!.spec!.containers![0].image).toBe(mesh[name]);
    }
  });
});
