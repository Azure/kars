// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { KarsConfig } from "../../config.js";
import { prepareKindBetaInference, type KindBetaProviderConnection } from "./kind-beta-inference.js";

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

const connections: KindBetaProviderConnection[] = [
  { kind: "openai", endpoint: "https://api.openai.com/v1", model: "gpt-5", apiKey: "openai-fixture" },
  { kind: "anthropic", endpoint: "https://api.anthropic.com/v1", model: "claude-sonnet-4-5", apiKey: "anthropic-fixture" },
  { kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-5", apiKey: "copilot-fixture" },
  { kind: "openai-compatible", endpoint: "https://models.example.com/v1", model: "org/model:latest", apiKey: "server-fixture" },
];
const namedDeps = () => ({ ...dependencies(null, false), resolveAddresses: vi.fn(async () => ["8.8.8.8"]) });
describe("explicit Kind provider connections", () => {
  it.each(connections)("binds $kind to the governed source store without fake Azure values", async connection => {
    const resources: any[] = [], deps = namedDeps();
    const result = await prepareKindBetaInference({}, resources, deps, false, connection);
    expect(result).toEqual({ controller: { inferenceDefaults: { provider: connection.kind, model: connection.model } }, kindBeta: { providerEndpoint: connection.endpoint } });
    expect(resources).toHaveLength(1);
    expect(resources[0]).toMatchObject({ apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: "kars-inference-providers", namespace: "kars-system" } });
    const prefix = `KARS_PROVIDER_${connection.kind.toUpperCase().replaceAll("-", "_")}_`;
    expect(resources[0].stringData).toEqual(connection.kind === "github-copilot"
      ? { [`${prefix}ENDPOINT`]: connection.endpoint, [`${prefix}TOKEN`]: connection.apiKey, COPILOT_GITHUB_TOKEN: connection.apiKey }
      : { [`${prefix}ENDPOINT`]: connection.endpoint, [`${prefix}API_KEY`]: connection.apiKey });
    expect(JSON.stringify(result)).not.toContain(connection.apiKey);
    expect(deps.load).not.toHaveBeenCalled(); expect(deps.prompt).not.toHaveBeenCalled();
  });
  it("retains an exactly matching prepared store and its unrelated provider entries", async () => {
    const resources: any[] = [], connection = connections[0];
    await prepareKindBetaInference({}, resources, namedDeps(), false, connection);
    resources[0].stringData.KARS_PROVIDER_OTHER_ENDPOINT = "https://other.example.com/v1";
    const before = structuredClone(resources);
    await prepareKindBetaInference({}, resources, namedDeps(), false, connection);
    expect(resources).toEqual(before);
  });
  it.each([
    { controller: { inferenceDefaults: { provider: "azure-openai" } } },
    { controller: { inferenceDefaults: { model: "different" } } },
    { sre: { provider: "foundry" } },
    { foundry: { endpoint: "https://existing.services.ai.azure.com" } },
    { controller: { extraEnv: [{ name: "KARS_PROVIDER_OPENAI_API_KEY", value: "different" }] } },
  ])("preserves incompatible reviewed Core settings", async core => {
    const resources: any[] = [], before = structuredClone(core);
    await expect(prepareKindBetaInference(core, resources, namedDeps(), false, connections[0])).rejects.toThrow();
    expect(core).toEqual(before); expect(resources).toEqual([]);
  });
  it.each(["uid", "resourceVersion", "ownerReferences"])("refuses to adopt a store carrying %s", async field => {
    const resources: any[] = [];
    await prepareKindBetaInference({}, resources, namedDeps(), false, connections[0]);
    resources[0].metadata[field] = "existing";
    const before = structuredClone(resources);
    await expect(prepareKindBetaInference({}, resources, namedDeps(), false, connections[0])).rejects.toThrow("source identity");
    expect(resources).toEqual(before);
  });
  it("refuses mismatched credentials and duplicate source stores", async () => {
    const resources: any[] = [];
    await prepareKindBetaInference({}, resources, namedDeps(), false, connections[0]);
    const before = structuredClone(resources);
    await expect(prepareKindBetaInference({}, resources, namedDeps(), false, { ...connections[0], apiKey: "different" })).rejects.toThrow("exactly");
    expect(resources).toEqual(before);
    await expect(prepareKindBetaInference({}, [...resources, ...resources], namedDeps(), false, connections[0])).rejects.toThrow("Duplicate");
  });
  it.each(["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fd00::1", "::ffff:127.0.0.1"])("rejects private DNS answer %s before creating a credential", async address => {
    const resources: any[] = [];
    await expect(prepareKindBetaInference({}, resources, { ...namedDeps(), resolveAddresses: async () => [address] }, false, connections[3])).rejects.toThrow("public remote");
    expect(resources).toEqual([]);
  });
  it("requires a credential and fixed endpoint for hosted connections", async () => {
    await expect(prepareKindBetaInference({}, [], namedDeps(), false, { ...connections[0], apiKey: undefined })).rejects.toThrow("credential");
    await expect(prepareKindBetaInference({}, [], namedDeps(), false, { ...connections[0], endpoint: "https://other.example.com/v1" })).rejects.toThrow("official");
  });
});
