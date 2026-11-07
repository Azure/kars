// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { KarsConfig } from "../../config.js";
import { prepareKindBetaInference } from "./kind-beta-inference.js";

const selected: KarsConfig = { provider: "foundry", endpoint: "https://models.example.test", model: "chosen-model", apiKey: "private-test-key" };
function dependencies(saved: KarsConfig | null = null, interactive = true) {
  return { load: vi.fn(() => saved), prompt: vi.fn(async () => selected), interactive };
}

describe("Kind Day-0 Core inference", () => {
  it("uses the existing prompt for missing inference and creates only a Core Secret reference/default pair", async () => {
    const deps = dependencies(), resources: any[] = [];
    const overlay = await prepareKindBetaInference({}, resources, deps);
    expect(deps.prompt).toHaveBeenCalledExactlyOnceWith({ provider: "foundry", heading: "Kind beta Core inference setup", foundry: { endpoint: undefined, model: undefined } });
    expect(overlay).toEqual({ controller: { inferenceDefaults: { provider: "azure-openai", model: "chosen-model" } },
      inferenceRouter: { azure: { openai: { endpoint: selected.endpoint, credentials: { existingSecret: "kars-kind-inference", key: "api-key" } } } } });
    expect(JSON.stringify(overlay)).not.toContain(selected.apiKey);
    expect(resources).toEqual([{ apiVersion: "v1", kind: "Secret", metadata: { name: "kars-kind-inference", namespace: "kars-system" }, type: "Opaque", stringData: { "api-key": selected.apiKey } }]);
    expect(overlay).not.toHaveProperty("sre");
    expect(overlay).not.toHaveProperty("bff");
    expect(overlay).not.toHaveProperty("web");
  });

  it("reuses compatible saved configuration without prompting, including noninteractive installs", async () => {
    const deps = dependencies(selected, false), resources: any[] = [];
    const overlay = await prepareKindBetaInference({}, resources, deps);
    expect(deps.prompt).not.toHaveBeenCalled();
    expect(overlay.controller.inferenceDefaults.model).toBe(selected.model);
  });

  it("preserves explicit defaults, project settings, SRE overrides, environment and supplied input identities", async () => {
    const core = { controller: { inferenceDefaults: { provider: "foundry", model: "reviewed-model" }, extraEnv: [{ name: "CUSTOM", value: "keep" }] },
      foundry: { endpoint: selected.endpoint, projectEndpoint: "" }, sre: { provider: "foundry", model: "sre-override" },
      inferenceRouter: { azure: { openai: { credentials: { key: "custom-key" } } } } };
    const before = structuredClone(core), resources: any[] = [{ kind: "Secret", metadata: { name: "bridge-principal" }, stringData: { signing: "retained-identity" } }];
    const overlay = await prepareKindBetaInference(core, resources, dependencies({ ...selected, foundryProjectEndpoint: "https://other.example.test/project" }));
    expect(core).toEqual(before);
    expect(overlay.controller.inferenceDefaults).toEqual(core.controller.inferenceDefaults);
    expect(overlay.foundry).toEqual({ endpoint: selected.endpoint });
    expect(overlay).not.toHaveProperty("sre");
    expect(resources[0].stringData.signing).toBe("retained-identity");
    expect(resources[1].stringData).toEqual({ "custom-key": selected.apiKey });
  });

  it("preserves prepared authentication without loading laptop credentials or prompting", async () => {
    const core = { controller: { inferenceDefaults: { provider: "foundry", model: "reviewed-model" } },
      foundry: { endpoint: selected.endpoint }, sre: { model: "independent-sre-model" },
      inferenceRouter: { azure: { openai: { credentials: { existingSecret: "operator-provider", key: "token" } } } } };
    const resources = [{ kind: "Secret", metadata: { name: "operator-provider" }, data: { token: "cHJlcGFyZWQ=" } }];
    const before = structuredClone({ core, resources }), deps = dependencies();
    expect(await prepareKindBetaInference(core, resources, deps)).toEqual({ controller: core.controller });
    expect({ core, resources }).toEqual(before);
    expect(deps.load).not.toHaveBeenCalled();
    expect(deps.prompt).not.toHaveBeenCalled();
  });

  it("retains older prepared SRE route selections as Core defaults when no explicit pair exists", async () => {
    const core = { sre: { provider: "foundry", model: "already-selected" }, foundry: { endpoint: selected.endpoint },
      inferenceRouter: { azure: { openai: { credentials: { existingSecret: "prepared" } } } } };
    const resources = [{ kind: "Secret", metadata: { name: "prepared" }, stringData: { "api-key": "existing" } }];
    expect((await prepareKindBetaInference(core, resources, dependencies())).controller.inferenceDefaults)
      .toEqual({ provider: "foundry", model: "already-selected" });
  });

  it("pins reviewed endpoint/model in the shared prompt rather than borrowing another endpoint's key", async () => {
    const deps = dependencies({ ...selected, endpoint: "https://different.example.test" });
    const core = { controller: { inferenceDefaults: { model: "reviewed" } }, inferenceRouter: { azure: { openai: { endpoint: selected.endpoint } } } };
    await prepareKindBetaInference(core, [], deps);
    expect(deps.prompt).toHaveBeenCalledWith(expect.objectContaining({ foundry: { endpoint: selected.endpoint, model: "reviewed" } }));
  });

  it.each([null, { ...selected, provider: "github-copilot" as const }])("fails clearly without interactive or supported saved credentials", async saved => {
    const deps = dependencies(saved, false);
    await expect(prepareKindBetaInference({}, [], deps)).rejects.toThrow(/Run interactively/);
    expect(deps.prompt).not.toHaveBeenCalled();
  });

  it.each(["github-models", "github-copilot", "local"])("does not invent bootstrap support for %s", async provider => {
    const deps = dependencies();
    await expect(prepareKindBetaInference({ controller: { inferenceDefaults: { provider } } }, [], deps)).rejects.toThrow(/separate qualification/);
    expect(deps.prompt).not.toHaveBeenCalled();
  });

  it("fails on incomplete prepared auth instead of replacing it with saved credentials", async () => {
    const core = { inferenceRouter: { azure: { openai: { credentials: { existingSecret: "prepared" } } } } };
    const deps = dependencies(selected);
    await expect(prepareKindBetaInference(core, [], deps)).rejects.toThrow(/must contain/);
    const resources = [{ kind: "Secret", metadata: { name: "prepared" }, stringData: { "api-key": "existing" } }];
    await expect(prepareKindBetaInference(core, resources, deps)).rejects.toThrow(/matching Core endpoint/);
    expect(deps.load).not.toHaveBeenCalled();
  });

  it("rejects inline keys and resource collisions without replacing identities", async () => {
    const deps = dependencies(selected);
    await expect(prepareKindBetaInference({ inferenceRouter: { azure: { openai: { credentials: { apiKey: "inline" } } } } }, [], deps)).rejects.toThrow(/inline key/);
    const resources = [{ kind: "Secret", metadata: { name: "kars-kind-inference" }, stringData: { "api-key": "keep" } }];
    await expect(prepareKindBetaInference({}, resources, deps)).rejects.toThrow(/reference it explicitly/);
    expect(resources[0].stringData["api-key"]).toBe("keep");
    expect(deps.prompt).not.toHaveBeenCalled();
  });

  it.each(["http://models.example.test", "https://models.example.test:8443", "https://user:password@models.example.test"])("rejects unsafe endpoint %s before credentials are created", async endpoint => {
    const resources: any[] = [];
    await expect(prepareKindBetaInference({}, resources, dependencies({ ...selected, endpoint }))).rejects.toThrow(/HTTPS/);
    expect(resources).toEqual([]);
  });
});
