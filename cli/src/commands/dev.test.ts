// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as path from "node:path";
import * as os from "node:os";
import * as fs from "node:fs";
import { preflightTools, requiredToolsFor, CONTAINER_RUNTIMES } from "./dev.js";

describe("preflightTools — AGT toolkit gating", () => {
  const origExit = process.exit;
  const origConsoleError = console.error;
  let exitCalls: number[];
  let errorCalls: string[];

  beforeEach(() => {
    exitCalls = [];
    errorCalls = [];
    process.exit = ((code?: number) => {
      exitCalls.push(code ?? 0);
      throw new Error(`__preflight_exit_${code ?? 0}__`);
    }) as never;
    console.error = (...args: unknown[]) => {
      errorCalls.push(args.map(String).join(" "));
    };
  });

  afterEach(() => {
    process.exit = origExit;
    console.error = origConsoleError;
  });

  function nonExistentAgtRepo(): string {
    return path.join(os.tmpdir(), `kars-no-agt-${Date.now()}-${Math.random()}`);
  }

  it("skips the AGT-toolkit existence check when build=false", async () => {
    // Even with a nonexistent AGT path, an overlay-refresh-only invocation
    // (no --build) must not bail out. This is the bug we fixed — re-running
    // `kars dev --target local-k8s` to pick up a new env var should not
    // require the AGT toolkit checkout.
    const agtRepo = nonExistentAgtRepo();
    expect(fs.existsSync(agtRepo)).toBe(false);

    await preflightTools("local-k8s", agtRepo, { build: false, noMesh: false });

    const agtErrors = errorCalls.filter((l) => l.includes("Agent Governance Toolkit"));
    expect(agtErrors).toHaveLength(0);
    expect(exitCalls).toHaveLength(0);
  });

  it("skips the AGT-toolkit check when noMesh=true even if build=true", async () => {
    // --no-mesh means "do not deploy the AGT relay/registry stack", so the
    // toolkit checkout is unused regardless of --build.
    const agtRepo = nonExistentAgtRepo();
    await preflightTools("local-k8s", agtRepo, { build: true, noMesh: true });

    const agtErrors = errorCalls.filter((l) => l.includes("Agent Governance Toolkit"));
    expect(agtErrors).toHaveLength(0);
    expect(exitCalls).toHaveLength(0);
  });

  it("does NOT require cargo in --release mode (no-compile promise)", async () => {
    // The README/quickstart headline is "kars dev --release — no Rust". That
    // path pulls pre-built, signed images from ghcr.io and compiles nothing,
    // so a missing Rust toolchain must not block it. We can't easily uninstall
    // cargo in CI, so assert on the required-tools list instead: in --release
    // mode, cargo must not be listed as required.
    const required = requiredToolsFor("docker", { build: false, noMesh: false, release: true });
    expect(required.map((t) => t.bin)).not.toContain("cargo");

    const requiredK8s = requiredToolsFor("local-k8s", { build: false, noMesh: false, release: true });
    expect(requiredK8s.map((t) => t.bin)).not.toContain("cargo");
    // local-k8s still needs the cluster tools.
    expect(requiredK8s.map((t) => t.bin)).toEqual(
      expect.arrayContaining(["kind", "kubectl", "helm"]),
    );
  });

  it("does NOT hard-require docker for the kind target (Podman/nerdctl work)", async () => {
    // The local-k8s runner drives docker/podman/nerdctl via
    // KIND_EXPERIMENTAL_PROVIDER, so the preflight must not pin `docker` in the
    // must-each-be-present list — the container runtime is an anyOf check.
    const requiredK8s = requiredToolsFor("local-k8s", { build: false, noMesh: false, release: true });
    expect(requiredK8s.map((t) => t.bin)).not.toContain("docker");
    expect(CONTAINER_RUNTIMES).toEqual(["docker", "podman", "nerdctl"]);
  });

  it("the single-container docker target DOES require the docker CLI", async () => {
    // The docker target shells out to `docker` directly, so it needs that binary.
    const required = requiredToolsFor("docker", { build: false, noMesh: false, release: true });
    expect(required.map((t) => t.bin)).toContain("docker");
  });

  it("DOES require cargo when building locally (no --release)", async () => {
    const required = requiredToolsFor("docker", { build: false, noMesh: false, release: false });
    expect(required.map((t) => t.bin)).toContain("cargo");
  });

  it("DOES require the AGT toolkit when --build is set and mesh is on", async () => {
    // The genuine build-from-source path is unchanged: missing toolkit
    // still fails loud with the copy-pasteable git clone command so the
    // operator isn't 5 minutes into a build before finding out.
    const agtRepo = nonExistentAgtRepo();
    let threw = false;
    try {
      await preflightTools("local-k8s", agtRepo, { build: true, noMesh: false });
    } catch (e) {
      threw = true;
      expect(String(e)).toContain("__preflight_exit_1__");
    }
    expect(threw).toBe(true);
    expect(exitCalls).toEqual([1]);
    const hasAgtError = errorCalls.some((l) => l.includes("Agent Governance Toolkit"));
    expect(hasAgtError).toBe(true);
  });
});
