// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { parseAllDocuments } from "yaml";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { authorityCommand } from "./sre-authority.js";

const { execute } = vi.hoisted(() => ({ execute: vi.fn() }));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../lib/repo-assets.js", () => ({
  requireBundledAsset: () => fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url)),
}));

beforeEach(() => {
  execute.mockReset();
  vi.spyOn(console, "log").mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

function fixture(authorityStage?: boolean) {
  const state = {
    inventory: JSON.stringify([{ name: "kars", namespace: "kars-system" }]),
    values: JSON.stringify({ controller: { inferenceDefaults: { provider: "azure-openai", model: "gpt-4.1" } },
      sre: { enabled: false, ...(authorityStage === undefined ? {} : { authorityStage }),
        dailyTokens: 12345, tokenBudget: 6789, requirePromptShields: true } }),
    readError: false,
    foreign: true,
    source: undefined as any,
    rendered: [] as any[],
    writes: [] as any[],
  };
  execute.mockImplementation(async (file: string, original: string[], options: { input?: string }) => {
    expect(original.slice(0, 2)).toEqual([file === "helm" ? "--kube-context" : "--context", "kind-kars-e2e"]);
    const args = original.slice(2);
    if (file === "helm") {
      if (args[0] === "list") return { stdout: state.inventory };
      if (args[0] === "get") {
        expect(args).toEqual(["get", "values", "kars", "--namespace", "kars-system", "--all", "-o", "json"]);
        if (state.readError) throw new Error("stored values read failed");
        return { stdout: state.values };
      }
      if (args[0] === "template") {
        if (options.input) expect(JSON.parse(options.input)).toEqual(JSON.parse(state.values));
        // Render the maintained chart, not a mock that hides missing model configuration.
        const stdout = execFileSync("helm", args, { input: options.input, encoding: "utf8", stdio: "pipe" });
        state.rendered = parseAllDocuments(stdout).map(document => document.toJSON()).filter(Boolean);
        return { stdout };
      }
    }
    if (file === "kubectl") {
      if (args[0] === "auth") return { stdout: "yes" };
      if (args[0] === "get") {
        if (["clusterrolebindings", "rolebindings"].includes(args[1])) return { stdout: '{"items":[]}' };
        if (args[1] === "karssandbox") return { stdout: state.source ? JSON.stringify(state.source) : "" };
        if (args[1] === "namespace" && (state.foreign || state.source)) return { stdout: JSON.stringify({
          metadata: { name: "kars-sre", resourceVersion: "1", uid: state.foreign ? "foreign-uid" : "created-namespace-uid",
            ...(state.foreign ? {} : { annotations: {
              "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/sandbox-namespace": "kars-system",
              "kars.azure.com/sandbox-name": "sre", "kars.azure.com/sandbox-uid": "created-source-uid",
            } }) }, status: { phase: "Active" },
        }) };
        return { stdout: "" };
      }
      if (args[0] === "create") {
        const object = JSON.parse(options.input!);
        state.writes.push(object);
        if (object.kind === "KarsSandbox") state.source = { ...object, metadata: {
          ...object.metadata, uid: "created-source-uid", resourceVersion: "1", annotations: {
            ...object.metadata.annotations, "kars.azure.com/namespace-uid": "created-namespace-uid",
          },
        } };
        return { stdout: JSON.stringify(state.source ?? object) };
      }
    }
    throw new Error(`Unexpected ${file} operation ${args[0]}`);
  });
  return state;
}

const stage = (...args: string[]) => authorityCommand().parseAsync(["node", "authority", "stage-source",
  "--context", "kind-kars-e2e", ...args]);
const helmOperations = () => execute.mock.calls.filter(([file]) => file === "helm").map(([, args]) => args[2]);

describe("authority stage-source release configuration", () => {
  it.each([undefined, false, true])("reaches the exact foreign-occupant denial before writes (installed authorityStage=%s)", async authorityStage => {
    const state = fixture(authorityStage);
    await expect(stage()).rejects.toThrow("Existing kars-sre namespace requires explicit ownership review");
    expect(helmOperations()).toEqual(["list", "get", "template"]);
    expect(state.rendered.some(object => object.kind === "KarsSandbox")).toBe(true);
    expect(state.writes).toEqual([]);
  });

  it.each([undefined, false, true])("creates fresh source/supports with installed route and budgets (installed authorityStage=%s)", async authorityStage => {
    const state = fixture(authorityStage);
    state.foreign = false;
    await stage();
    expect(helmOperations()).toEqual(["list", "get", "template"]);
    const policy = state.writes.find(object => object.kind === "InferencePolicy");
    expect(policy.spec.modelPreference.primary).toEqual({ provider: "azure-openai", deployment: "gpt-4.1" });
    expect(policy.spec.tokenBudget).toEqual({ dailyTokens: 12345, perRequestTokens: 6789 });
    expect(policy.spec.contentSafety.requirePromptShields).toBe(true);
    expect(state.writes.every(object => ["KarsSandbox", "InferencePolicy", "ToolPolicy", "ClusterRole"].includes(object.kind))).toBe(true);
    expect(console.log).toHaveBeenCalledWith("Created and claimed source: --sandbox-uid created-source-uid --namespace-uid created-namespace-uid");
    await expect(stage()).rejects.toThrow("SRE source already exists");
    expect(state.writes.filter(object => object.kind === "KarsSandbox")).toHaveLength(1);
  });

  it.each([undefined, false, true])("honors explicit selection without changing budgets (installed authorityStage=%s)", async authorityStage => {
    const state = fixture(authorityStage);
    state.foreign = false;
    await stage("--provider", "local", "--model", "org/model:small");
    const policy = state.writes.find(object => object.kind === "InferencePolicy");
    expect(policy.spec.modelPreference.primary).toEqual({ provider: "local", deployment: "org/model:small" });
    expect(policy.spec.tokenBudget).toEqual({ dailyTokens: 12345, perRequestTokens: 6789 });
    expect(policy.spec.contentSafety.requirePromptShields).toBe(true);
  });

  it("keeps explicit template-managed staging without requiring a Helm release", async () => {
    const state = fixture();
    state.inventory = "[]";
    await expect(stage("--provider", "local", "--model", "org/model:small")).rejects.toThrow("Existing kars-sre namespace");
    expect(helmOperations()).toEqual(["list", "template"]);
    expect(state.writes).toEqual([]);
  });

  it.each(["null", "[]", '"invalid"', "{"])("rejects invalid stored values %s before rendering or writes", async value => {
    const state = fixture();
    state.values = value;
    await expect(stage()).rejects.toThrow();
    expect(helmOperations()).toEqual(["list", "get"]);
    expect(state.writes).toEqual([]);
  });

  it("does not fall back to chart defaults on a stored-values read failure", async () => {
    const state = fixture();
    state.readError = true;
    await expect(stage()).rejects.toThrow("stored values read failed");
    expect(helmOperations()).toEqual(["list", "get"]);
    expect(state.writes).toEqual([]);
  });

  it("keeps the missing-model render safeguard rather than accepting it as an occupancy denial", async () => {
    const state = fixture();
    state.values = "{}";
    await expect(stage()).rejects.toThrow("Configure controller.inferenceDefaults");
    expect(state.writes).toEqual([]);
  });

  it.each(["null", "{}", '[{"name":"kars","namespace":"other"}]'])("rejects invalid release inventory %s", async inventory => {
    const state = fixture();
    state.inventory = inventory;
    await expect(stage()).rejects.toThrow("Helm ownership inventory is invalid");
    expect(helmOperations()).toEqual(["list"]);
    expect(state.writes).toEqual([]);
  });
});
