// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync, readFileSync, existsSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

// Redirect homedir() to a per-test tmp dir so context.json writes are
// isolated from the user's real ~/.azureclaw.
let tmpHome: string = mkdtempSync(join(tmpdir(), "azureclaw-resume-test-"));
vi.mock("os", async (importOriginal) => {
  const actual = await importOriginal<typeof import("os")>();
  return { ...actual, homedir: () => tmpHome };
});

beforeEach(() => {
  tmpHome = mkdtempSync(join(tmpdir(), "azureclaw-resume-test-"));
  mkdirSync(join(tmpHome, ".azureclaw"), { recursive: true });
});

afterEach(() => {
  rmSync(tmpHome, { recursive: true, force: true });
});

const CONTEXT_PATH = () => join(tmpHome, ".azureclaw", "context.json");

function writeContext(ctx: Record<string, unknown>): void {
  writeFileSync(CONTEXT_PATH(), JSON.stringify(ctx, null, 2), "utf-8");
}

describe("up/resume", () => {
  async function loadModules() {
    // Reset module cache so config.ts re-captures CONFIG_DIR from the
    // per-test tmpHome via the mocked homedir().
    vi.resetModules();
    const mod = await import("./resume.js");
    return mod;
  }

  it("returns null when no context.json exists", async () => {
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({}, { region: "eastus2" })).toBeNull();
  });

  it("returns null when phase is missing", async () => {
    writeContext({ region: "eastus2", savedAt: new Date().toISOString() });
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({}, { region: "eastus2" })).toBeNull();
  });

  it("returns null when phase is 'complete' (last run finished)", async () => {
    writeContext({ phase: "complete", region: "eastus2", savedAt: new Date().toISOString() });
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({}, { region: "eastus2" })).toBeNull();
  });

  it("returns null when --from-scratch is passed", async () => {
    writeContext({ phase: "network", region: "eastus2", savedAt: new Date().toISOString() });
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({ fromScratch: true }, { region: "eastus2" })).toBeNull();
  });

  it("returns null when topology mismatches (region change)", async () => {
    writeContext({ phase: "network", region: "eastus2", savedAt: new Date().toISOString() });
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({}, { region: "westus3" })).toBeNull();
  });

  it("returns null when context is older than 7 days", async () => {
    const tenDaysAgo = new Date(Date.now() - 10 * 24 * 60 * 60 * 1000).toISOString();
    writeContext({ phase: "network", region: "eastus2", savedAt: tenDaysAgo });
    const { loadResumeState } = await loadModules();
    expect(loadResumeState({}, { region: "eastus2" })).toBeNull();
  });

  it("returns resume state when phase is partial and topology matches", async () => {
    writeContext({
      phase: "network",
      region: "eastus2",
      resourceGroup: "azureclaw-eastus2",
      aksCluster: "azureclaw-aks",
      sandboxName: "my-assistant",
      savedAt: new Date().toISOString(),
    });
    const { loadResumeState } = await loadModules();
    const state = loadResumeState(
      {},
      { region: "eastus2", resourceGroup: "azureclaw-eastus2", aksCluster: "azureclaw-aks", sandboxName: "my-assistant" },
    );
    expect(state).not.toBeNull();
    expect(state?.resumeFromPhase).toBe("network");
    expect(state?.ageMs).toBeGreaterThanOrEqual(0);
  });

  it("isPhaseSkippable returns true only for SKIPPABLE phases at-or-before resumeFromPhase", async () => {
    const { isPhaseSkippable } = await loadModules();
    // network is skippable
    expect(isPhaseSkippable("network", "network")).toBe(true);
    expect(isPhaseSkippable("network", "images")).toBe(true);
    expect(isPhaseSkippable("network", "rg")).toBe(false); // not done yet
    // images is skippable
    expect(isPhaseSkippable("images", "images")).toBe(true);
    expect(isPhaseSkippable("images", "network")).toBe(false);
    // rg is NOT in SKIPPABLE
    expect(isPhaseSkippable("rg", "complete")).toBe(false);
    // helm is NOT in SKIPPABLE
    expect(isPhaseSkippable("helm", "helm")).toBe(false);
    // null resumeFromPhase: never skip
    expect(isPhaseSkippable("network", null)).toBe(false);
    expect(isPhaseSkippable("images", undefined)).toBe(false);
  });

  it("markPhaseDone writes phase + topology to context.json and merges with prior state", async () => {
    writeContext({ acrLoginServer: "test.azurecr.io" });
    const { markPhaseDone } = await loadModules();
    markPhaseDone(
      "infra",
      { keyVaultName: "kv-foo" },
      { region: "eastus2", resourceGroup: "rg-foo", aksCluster: "aks-foo", sandboxName: "s" },
    );
    const written = JSON.parse(readFileSync(CONTEXT_PATH(), "utf-8"));
    expect(written.phase).toBe("infra");
    expect(written.acrLoginServer).toBe("test.azurecr.io"); // preserved
    expect(written.keyVaultName).toBe("kv-foo");            // new
    expect(written.region).toBe("eastus2");                  // topology
    expect(written.resourceGroup).toBe("rg-foo");
    expect(written.savedAt).toBeTypeOf("string");
    expect(written.phaseStartedAt).toBeTypeOf("string");
  });

  it("markPhaseDone('complete') stamps complete and clears the resumable state", async () => {
    writeContext({ phase: "mesh", region: "eastus2", savedAt: new Date().toISOString() });
    const { markPhaseDone, loadResumeState } = await loadModules();
    markPhaseDone("complete", {}, { region: "eastus2" });
    expect(loadResumeState({}, { region: "eastus2" })).toBeNull();
    const written = JSON.parse(readFileSync(CONTEXT_PATH(), "utf-8"));
    expect(written.phase).toBe("complete");
  });

  it("formatAge produces compact strings", async () => {
    const { formatAge } = await loadModules();
    expect(formatAge(0)).toBe("<1m");
    expect(formatAge(30_000)).toBe("<1m");
    expect(formatAge(60_000)).toBe("1m");
    expect(formatAge(12 * 60_000)).toBe("12m");
    expect(formatAge(2 * 60 * 60_000)).toBe("2h");
    expect(formatAge(3 * 24 * 60 * 60_000)).toBe("3d");
    expect(formatAge(-1)).toBe("?");
  });

  it("file is created with expected location under $HOME", async () => {
    const { markPhaseDone } = await loadModules();
    markPhaseDone("rg", {}, { region: "eastus2" });
    expect(existsSync(CONTEXT_PATH())).toBe(true);
  });
});
