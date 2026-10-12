// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Stepper } from "../../stepper.js";

const mocks = vi.hoisted(() => ({
  execute: vi.fn(), repo: vi.fn(), wheels: vi.fn(), done: vi.fn(), guard: vi.fn(),
}));
vi.mock("execa", () => ({ execa: mocks.execute }));
vi.mock("../../lib/agt-bootstrap.js", async original => ({
  ...await original<typeof import("../../lib/agt-bootstrap.js")>(),
  ensureAgtRepo: mocks.repo, ensureAgtWheels: mocks.wheels,
}));
vi.mock("../../lib/stage-rust-bin.js", () => ({ stageRustBinaries: vi.fn() }));
vi.mock("../../lib/private-root-upgrade-guard.js", () => ({ assertControllerMutationAllowed: mocks.guard }));
vi.mock("./resume.js", () => ({ isPhaseSkippable: () => false, markPhaseDone: mocks.done }));

import { acquireImages, type AcquireImagesContext } from "./images.js";

function context(options: AcquireImagesContext["options"]): AcquireImagesContext {
  const stepper = new Stepper({ totalSteps: 1 });
  for (const method of ["step", "update", "detail", "done"] as const) vi.spyOn(stepper, method).mockImplementation(() => {});
  return { stepper, options, acr: "mirror", acrLoginServer: "mirror.azurecr.io",
    repoRoot: "/kars", resumeFromPhase: null, resumeTopology: {} };
}

beforeEach(() => {
  vi.resetAllMocks();
  mocks.execute.mockResolvedValue({ stdout: "" });
  mocks.repo.mockResolvedValue("/agt");
  mocks.wheels.mockResolvedValue(undefined);
});

describe("shared AL3 AgentMesh image acquisition", () => {
  it.each([true, false])("builds both mesh components even when skipRuntimeImages=%s", async skipRuntimeImages => {
    await acquireImages(context({ build: true, sourceAcr: "old.invalid", skipRuntimeImages }));
    expect(mocks.wheels).toHaveBeenCalledWith("/agt", "/kars");
    const builds = mocks.execute.mock.calls.filter(([command, args]) => command === "docker" && args[0] === "build");
    for (const component of ["relay", "registry"]) {
      const target = `mirror.azurecr.io/agentmesh-${component}-agt:latest`;
      const build = builds.find(([, args]) => args.includes(target));
      expect(build?.[1]).toContain("/kars/sandbox-images/agentmesh/Dockerfile");
      expect(build?.[1]).toContain(`COMPONENT=${component}`);
      expect(build?.[1].at(-1)).toBe("/kars");
      expect(mocks.execute).toHaveBeenCalledWith("docker", ["push", target], { stdio: "pipe" });
    }
    expect(mocks.execute.mock.calls.some(([, args]) => args.includes("import"))).toBe(false);
    expect(mocks.done).toHaveBeenCalled();
  });

  it("fails before ACR/build operations when the wheel set is invalid", async () => {
    mocks.wheels.mockRejectedValue(new Error("native wheel provenance mismatch"));
    await expect(acquireImages(context({ build: true, sourceAcr: "old.invalid", skipRuntimeImages: true })))
      .rejects.toThrow("native wheel provenance mismatch");
    expect(mocks.execute).not.toHaveBeenCalled();
    expect(mocks.done).not.toHaveBeenCalled();
  });

  it("does not import an old mesh or mark success after a mesh build fails", async () => {
    mocks.execute.mockImplementation(async (command: string, args: string[]) => {
      if (command === "docker" && args[0] === "build" && args.includes("COMPONENT=relay")) {
        throw new Error("mesh build failed");
      }
      return { stdout: "" };
    });
    await expect(acquireImages(context({ build: true, sourceAcr: "old.invalid", skipRuntimeImages: true })))
      .rejects.toThrow("mesh build failed");
    expect(mocks.done).not.toHaveBeenCalled();
    expect(mocks.execute.mock.calls.some(([, args]) => args.includes("import"))).toBe(false);
  });

  it.each([undefined, true])("keeps non-build release/import acquisition separate (release=%s)", async release => {
    await acquireImages(context({ sourceAcr: "source.azurecr.io", release, skipRuntimeImages: true }));
    expect(mocks.repo).not.toHaveBeenCalled();
    expect(mocks.wheels).not.toHaveBeenCalled();
    expect(mocks.execute.mock.calls.some(([command]) => command === "docker")).toBe(false);
    for (const component of ["relay", "registry"]) {
      expect(mocks.execute.mock.calls.some(([, args]) => args.includes("import") &&
        args.includes(`agentmesh-${component}-agt:latest`))).toBe(true);
    }
  });

  it("uses the same recipe and verified wheel artifact for native release builds", () => {
    const root = fileURLToPath(new URL("../../../../", import.meta.url));
    const workflow = parse(readFileSync(`${root}.github/workflows/release-public-interim.yml`, "utf8"));
    const mesh = workflow.jobs["build-mesh"];
    expect(mesh.needs).toContain("build-agt-wheels");
    expect(mesh.steps.find((step: { uses?: string }) => step.uses?.startsWith("actions/download-artifact@")).with)
      .toMatchObject({ name: "agt-wheels-${{ env.VERSION }}", path: "runtimes/wheels/" });
    expect(mesh.steps.find((step: { uses?: string }) => step.uses?.startsWith("docker/build-push-action@")).with)
      .toMatchObject({ context: ".", file: "sandbox-images/agentmesh/Dockerfile" });
  });
});
