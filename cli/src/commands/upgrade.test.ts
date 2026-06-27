// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  isFoundryProjectHost,
  resolveTargetVersion,
  buildHelmUpgradeArgs,
  detectCurrentVersion,
  rolloutRestartAll,
  verifyHealth,
} from "./upgrade.js";

describe("isFoundryProjectHost", () => {
  it("accepts a real Foundry project endpoint", () => {
    expect(isFoundryProjectHost("https://acct.services.ai.azure.com/api/projects/p")).toBe(true);
    expect(isFoundryProjectHost("https://services.ai.azure.com")).toBe(true);
  });

  it("rejects look-alike hosts that a substring match would accept", () => {
    // The classic incomplete-sanitization bypass.
    expect(isFoundryProjectHost("https://services.ai.azure.com.evil.com/api/projects/p")).toBe(false);
    expect(isFoundryProjectHost("https://evilservices.ai.azure.com/x")).toBe(false);
    expect(isFoundryProjectHost("https://attacker.com/?q=services.ai.azure.com")).toBe(false);
  });

  it("rejects plain Azure OpenAI and empty/garbage input", () => {
    expect(isFoundryProjectHost("https://my-aoai.openai.azure.com")).toBe(false);
    expect(isFoundryProjectHost("")).toBe(false);
    expect(isFoundryProjectHost("not a url")).toBe(false);
  });
});

describe("resolveTargetVersion", () => {
  const fetchLatest = async () => "v0.1.20";
  const fetchNone = async () => null;

  it("resolves no `--to` to the latest published release", async () => {
    expect(await resolveTargetVersion(undefined, fetchLatest)).toEqual({ target: "v0.1.20" });
  });

  it("resolves `latest` / `stable` (any case) to the latest release — the recovery path", async () => {
    expect(await resolveTargetVersion("latest", fetchLatest)).toEqual({ target: "v0.1.20" });
    expect(await resolveTargetVersion("LATEST", fetchLatest)).toEqual({ target: "v0.1.20" });
    expect(await resolveTargetVersion("stable", fetchLatest)).toEqual({ target: "v0.1.20" });
    expect(await resolveTargetVersion("  latest  ", fetchLatest)).toEqual({ target: "v0.1.20" });
  });

  it("passes a valid explicit tag through unchanged", async () => {
    expect(await resolveTargetVersion("v0.1.19", fetchLatest)).toEqual({ target: "v0.1.19" });
    expect(await resolveTargetVersion("0.1.19", fetchLatest)).toEqual({ target: "0.1.19" });
    expect(await resolveTargetVersion("v1.2.3-rc.1", fetchLatest)).toEqual({ target: "v1.2.3-rc.1" });
  });

  it("rejects a non-version `--to` instead of treating it as a downgrade", async () => {
    const r = await resolveTargetVersion("garbage", fetchLatest);
    expect(r.target).toBeUndefined();
    expect(r.error).toMatch(/not a valid release tag/);
  });

  it("errors clearly when the latest release can't be determined", async () => {
    const r = await resolveTargetVersion("latest", fetchNone);
    expect(r.target).toBeUndefined();
    expect(r.error).toMatch(/GitHub releases API|explicit tag/);
  });
});

describe("buildHelmUpgradeArgs", () => {
  const ctx = {
    acrLoginServer: "myacr.azurecr.io",
    aksCluster: "kars-aks",
    resourceGroup: "rg",
    wiClientId: "wi-123",
    keyVaultName: "kv-1",
  };

  it("version-pins the image tags to the target (NOT :latest) so rollback works", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20").join(" ");
    expect(args).toContain("controller.image.repository=myacr.azurecr.io/kars-controller");
    expect(args).toContain("inferenceRouter.image.repository=myacr.azurecr.io/kars-inference-router");
    expect(args).toContain("sandbox.image.repository=myacr.azurecr.io/openclaw-sandbox");
    // The crux of the rollback fix: tags are the version, not `latest`.
    expect(args).toContain("controller.image.tag=v0.1.20");
    expect(args).toContain("inferenceRouter.image.tag=v0.1.20");
    expect(args).toContain("sandbox.image.tag=v0.1.20");
    expect(args).not.toContain("image.tag=latest");
  });

  it("sets ALL runtime images explicitly (incl. langgraph-ts), pinned to target", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20").join(" ");
    for (const [key, repo] of [
      ["runtimes.openaiAgents.image", "kars-runtime-openai-agents"],
      ["runtimes.mafPython.image", "kars-runtime-maf-python"],
      ["runtimes.anthropic.image", "kars-runtime-anthropic"],
      ["runtimes.langgraph.image", "kars-runtime-langgraph"],
      ["runtimes.langgraphTs.image", "kars-runtime-langgraph-ts"],
      ["runtimes.pydanticAi.image", "kars-runtime-pydantic-ai"],
      ["runtimes.hermes.image", "kars-runtime-hermes"],
    ] as const) {
      expect(args).toContain(`${key}=myacr.azurecr.io/${repo}:v0.1.20`);
    }
  });

  it("omits runtime image overrides when --skip-runtime-images (preserve via reuse)", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20", { skipRuntimeImages: true }).join(" ");
    expect(args).not.toContain("runtimes.langgraphTs.image");
    expect(args).not.toContain("runtimes.hermes.image");
    // core images are still pinned
    expect(args).toContain("controller.image.tag=v0.1.20");
  });

  it("still preserves operator config via --reuse-values", () => {
    expect(buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20")).toContain("--reuse-values");
  });

  it("stamps the resolved release as karsRelease for later version detection", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20");
    const i = args.indexOf("karsRelease=v0.1.20");
    expect(i).toBeGreaterThan(-1);
    expect(args[i - 1]).toBe("--set");
  });

  it("is a safe atomic upgrade (auto-rollback + wait)", () => {
    const args = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20");
    expect(args).toContain("--atomic");
    expect(args).toContain("--wait");
    expect(args.slice(0, 4)).toEqual(["upgrade", "--install", "kars", "/chart"]);
  });

  it("only sets the Foundry endpoint when one is configured", () => {
    const withFoundry = buildHelmUpgradeArgs({ ...ctx, foundryEndpoint: "https://x.services.ai.azure.com" }, "/chart", "v0.1.20").join(" ");
    expect(withFoundry).toContain("inferenceRouter.azure.openai.endpoint=https://x.services.ai.azure.com");
    const without = buildHelmUpgradeArgs(ctx, "/chart", "v0.1.20").join(" ");
    expect(without).not.toContain("inferenceRouter.azure.openai.endpoint");
  });
});

// ── health / safety / recovery coverage ────────────────────────────

type Execa = typeof import("execa").execa;

/** Fake execa that records argv and returns scripted stdout per matcher. */
function fakeExeca(script: (bin: string, args: string[]) => string | undefined, calls: string[][]): Execa {
  return (async (bin: string, args: readonly string[]) => {
    calls.push([bin, ...args]);
    const out = script(bin, [...args]);
    if (out === "THROW") throw new Error("command failed");
    return { stdout: out ?? "" };
  }) as unknown as Execa;
}

describe("detectCurrentVersion — honest, un-spoofable version detection", () => {
  // The controller image is queried first; route by command.
  const withImage = (image: string, helmValues: string) =>
    (bin: string, args: string[]): string | undefined => {
      if (bin === "kubectl" && args.includes("kars-controller")) return image;
      if (bin === "helm") return helmValues;
      return "";
    };

  it("prefers the controller image TAG (what's actually running)", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(withImage("myacr.azurecr.io/kars-controller:v0.1.20", JSON.stringify({ karsRelease: "v0.1.18" })), calls);
    // Image tag wins over a (stale) stamped helm value.
    expect(await detectCurrentVersion(execa, "0.1.0")).toBe("v0.1.20");
  });

  it("falls back to the stamped karsRelease when the image tag is :latest", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(withImage("myacr.azurecr.io/kars-controller:latest", JSON.stringify({ karsRelease: "v0.1.19" })), calls);
    expect(await detectCurrentVersion(execa, "0.1.0")).toBe("v0.1.19");
  });

  it("treats the static 0.1.0 chart appVersion as unknown (no false downgrade guard)", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(withImage("myacr.azurecr.io/kars-controller:latest", "{}"), calls);
    expect(await detectCurrentVersion(execa, "0.1.0")).toBe("");
  });

  it("returns unknown when both kubectl and helm fail", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(() => "THROW", calls);
    expect(await detectCurrentVersion(execa, "0.1.0")).toBe("");
  });

  it("trusts a real, non-sentinel appVersion if the chart ever bumps it", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(withImage("myacr.azurecr.io/kars-controller:latest", "{}"), calls);
    expect(await detectCurrentVersion(execa, "0.2.5")).toBe("v0.2.5");
  });
});

describe("rolloutRestartAll — refreshes every workload, mesh first", () => {
  it("restarts agentmesh relay+registry, the controller, and every sandbox", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(() => "", calls);
    await rolloutRestartAll(execa);
    const restarts = calls
      .filter((c) => c[0] === "kubectl" && c[1] === "rollout" && c[2] === "restart")
      .map((c) => c.join(" "));
    // mesh relay + registry by name (narrow, not --all)
    expect(restarts.some((c) => c.includes("deployment/agentmesh-relay") && c.includes("-n agentmesh"))).toBe(true);
    expect(restarts.some((c) => c.includes("deployment/agentmesh-registry") && c.includes("-n agentmesh"))).toBe(true);
    expect(restarts.some((c) => c.includes("--all"))).toBe(false);
    // controller + sandboxes
    expect(restarts.some((c) => c.includes("-n kars-system") && c.includes("app.kubernetes.io/name=kars"))).toBe(true);
    expect(restarts.some((c) => c.includes("-A") && c.includes("kars.azure.com/component=sandbox"))).toBe(true);
  });

  it("restarts the mesh BEFORE the controller (so deps come up against new mesh)", async () => {
    const calls: string[][] = [];
    await rolloutRestartAll(fakeExeca(() => "", calls));
    const order = calls
      .filter((c) => c[0] === "kubectl" && c[1] === "rollout" && c[2] === "restart")
      .map((c) => c.join(" "));
    const meshIdx = order.findIndex((c) => c.includes("agentmesh-relay"));
    const ctrlIdx = order.findIndex((c) => c.includes("app.kubernetes.io/name=kars"));
    expect(meshIdx).toBeGreaterThanOrEqual(0);
    expect(ctrlIdx).toBeGreaterThan(meshIdx);
  });

  it("never throws even if every kubectl call fails (best-effort)", async () => {
    const calls: string[][] = [];
    const execa = fakeExeca(() => "THROW", calls);
    await expect(rolloutRestartAll(execa)).resolves.toBeUndefined();
  });
});

describe("verifyHealth — strong enough to gate success", () => {
  const route = (ctrlAvail: string, podReasons: Record<string, string>) =>
    (bin: string, args: string[]): string | undefined => {
      if (bin === "kubectl" && args.includes("deployment") && args.includes("kars-controller")) return ctrlAvail;
      if (bin === "kubectl" && args.includes("pods")) {
        const ns = args[args.indexOf("-n") + 1];
        return podReasons[ns] ?? "";
      }
      return "";
    };

  it("healthy when controller Available and no bad pods", async () => {
    const r = await verifyHealth(fakeExeca(route("True", {}), []));
    expect(r.healthy).toBe(true);
  });

  it("unhealthy when the controller is not Available", async () => {
    const r = await verifyHealth(fakeExeca(route("False", {}), []));
    expect(r.healthy).toBe(false);
    expect(r.reason).toMatch(/controller/i);
  });

  it("unhealthy on ImagePullBackOff (the bad-image symptom)", async () => {
    const r = await verifyHealth(fakeExeca(route("True", { "kars-system": "ImagePullBackOff " }), []));
    expect(r.healthy).toBe(false);
    expect(r.reason).toMatch(/ImagePullBackOff/);
  });

  it("unhealthy on CrashLoopBackOff in the agentmesh namespace", async () => {
    const r = await verifyHealth(fakeExeca(route("True", { agentmesh: "CrashLoopBackOff " }), []));
    expect(r.healthy).toBe(false);
    expect(r.reason).toMatch(/CrashLoopBackOff/);
  });

  it("unhealthy (not a false-positive) when the controller probe errors", async () => {
    const r = await verifyHealth(fakeExeca(() => "THROW", []));
    expect(r.healthy).toBe(false);
  });
});
