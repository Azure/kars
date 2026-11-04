// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import {
  adoptedFixture, fixture, render, routerFixture, dailyLimitFixture, hash, contractRenewalFixture, topUp,
  root,
} from "./sre-first-install-transition.test-support.js";

describe("SRE calendar accounting policy continuity", () => {
  it.each([false, true])("accepts the strict default for older sealed values (adopted: %s)", adopted => {
    const f = adopted ? adoptedFixture() : fixture();
    f.root.Values.inferenceBudget.calendarPolicy = "enforce";
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    if (adopted) expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]).inferenceBudget.calendarPolicy).toBe("enforce");
  });
  it("requires adoption before changing the original calendar policy", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.calendarPolicy = "record-only";
    expect(() => render(f)).toThrow();
  });
  it("preserves selected record-only policy and sealed receipt during a router upgrade", () => {
    const f = routerFixture();
    f.root.Values.inferenceBudget.calendarPolicy = "record-only";
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]).inferenceBudget).toEqual(before.root.Values.inferenceBudget);
    expect(f).toEqual(before);
  });
  it.each(["", "disabled", false, {}, []])("rejects invalid calendar policy %j", policy => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.calendarPolicy = policy;
    expect(() => render(f)).toThrow();
  });
});

describe("adopted SRE model selection", () => {
  function modelFixture(inherited = false) {
    const f = dailyLimitFixture();
    f.root.Values.sre.dailyTokens = 1500000;
    f.resources[1].spec.tokenBudget.dailyTokens = 1500000;
    const primary = { provider: "azure-openai", deployment: "original-model" };
    f.root.Values.controller.inferenceDefaults = { provider: primary.provider, model: primary.deployment };
    f.root.Values.sre.provider = inherited ? "" : primary.provider;
    f.root.Values.sre.model = inherited ? "" : primary.deployment;
    f.resources[1].spec.modelPreference = { primary, fallback: [] };
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.resources = structuredClone(f.resources);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.sre.modelRouteBaseline = { provider: f.root.Values.sre.provider, model: f.root.Values.sre.model };
    f.root.Values.sre.provider = "github-copilot";
    f.root.Values.sre.model = "claude-opus-5.5";
    f.resources[1].spec.modelPreference.primary = { provider: "github-copilot", deployment: "claude-opus-5.5" };
    return f;
  }
  it.each([false, true])("preserves the sealed %s baseline, funding and selected route", inherited => {
    const f = modelFixture(inherited);
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.kind === "InferencePolicy").spec).toEqual(f.resources[1].spec);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const selected = JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]);
    expect(selected.sre).toEqual(f.root.Values.sre);
    expect(selected.inferenceBudget).toEqual(f.root.Values.inferenceBudget);
    expect(f).toEqual(before);
  });
  it("composes a model selection with the existing finite daily increase", () => {
    const f = modelFixture();
    f.root.Values.sre.dailyTokens = 5000000;
    f.resources[1].spec.tokenBudget.dailyTokens = 5000000;
    expect(render(f)).toContain("claude-opus-5.5");
  });
  it.each([
    (f: any) => { f.adoptionMap = {}; },
    (f: any) => { f.adoptionMap.data.valuesDigest = "forged"; },
    (f: any) => { f.root.Values.sre.modelRouteBaseline.model = "forged"; },
    (f: any) => { f.root.Values.sre.modelRouteBaseline = { provider: "azure-openai" }; },
    (f: any) => { f.root.Values.sre.modelRouteBaseline = null; },
    (f: any) => { f.root.Values.sre.modelRouteBaseline.extra = true; },
    (f: any) => { f.root.Values.sre.provider = "bad/tag"; },
    (f: any) => { f.root.Values.sre.model = " "; },
    (f: any) => { f.resources[1].spec.modelPreference.primary.deployment = "different"; },
    (f: any) => { f.resources[1].spec.modelPreference.fallback.push({ provider: "other", deployment: "other" }); },
    (f: any) => { f.resources[1].spec.contentSafety.requirePromptShields = false; },
    (f: any) => { f.resources[1].spec.tokenBudget.perRequestTokens = 0; },
    (f: any) => { f.resources[1].spec.appliesTo.sandboxName = "other"; },
    (f: any) => { f.root.Values.sre.firstInstallFunding.tokens = 5000000; },
    (f: any) => { f.root.Values.controller.extraEnv.push({ name: "KEY", value: "changed" }); },
  ])("rejects unauthenticated baselines and unrelated policy/configuration changes", mutate => {
    const f = modelFixture(); mutate(f);
    expect(() => render(f)).toThrow();
  });
});

describe("capless Kind SRE policy continuity", () => {
  function caplessFixture(originalCapless = false, adopted = true, kind = true, unbounded = true) {
    const f = fixture(1000, undefined, undefined, unbounded);
    f.root.Values.global = { installationProfile: kind ? "kind-beta" : "production" };
    Object.assign(f.root.Values.sre, { tokenBudget: 528000, dailyTokens: 1500000,
      provider: "azure-openai", model: "original-model" });
    f.resources.push({ apiVersion: "kars.azure.com/v1alpha1", kind: "InferencePolicy",
      metadata: { name: "sre-inference", namespace: "kars-system" }, spec: {
        appliesTo: { sandboxName: "sre" }, contentSafety: { requirePromptShields: true },
        modelPreference: { primary: { provider: "azure-openai", deployment: "original-model" }, fallback: [] },
        ...(originalCapless ? {} : { tokenBudget: { dailyTokens: 1500000, perRequestTokens: 528000 } }),
      } });
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.resources = structuredClone(f.resources);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    if (adopted) {
      const baseline = structuredClone(f.root.Values);
      delete baseline.controller.image;
      f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
      f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    }
    delete f.resources[1].spec.tokenBudget;
    return f;
  }
  it.each([false, true])("preserves capless output and sealed receipt (original capless: %s)", originalCapless => {
    const f = caplessFixture(originalCapless);
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.kind === "InferencePolicy").spec).toEqual(f.resources[1].spec);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(f.root.Values);
    expect(f).toEqual(before);
  });
  it("adopts an originally capless install but requires adoption before removing sealed ceilings", () => {
    expect(() => render(caplessFixture(true, false))).not.toThrow();
    expect(() => render(caplessFixture(false, false))).toThrow(/completed adoption/);
  });
  it("composes cap removal with independently verified model selection", () => {
    const f = caplessFixture();
    f.root.Values.sre.modelRouteBaseline = { provider: "azure-openai", model: "original-model" };
    f.root.Values.sre.provider = "github-copilot";
    f.root.Values.sre.model = "claude-opus-5.5";
    f.resources[1].spec.modelPreference.primary = { provider: "github-copilot", deployment: "claude-opus-5.5" };
    const docs = parseAllDocuments(render(f)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.kind === "InferencePolicy").spec).toEqual(f.resources[1].spec);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
  });
  it.each([[false, true], [true, false], [false, false]])("does not exempt cap removal outside authorized Kind (kind=%s, unbounded=%s)", (kind, unbounded) => {
    expect(() => render(caplessFixture(false, true, kind, unbounded))).toThrow();
  });
  it.each([
    (f: any) => { f.adoptionMap.data.valuesDigest = "forged"; },
    (f: any) => { f.intentMap.immutable = false; },
    (f: any) => { f.registration.spec.firstInstallFunding.grant.unboundedTokens = false; },
    (f: any) => { f.registration.spec.firstInstallFunding.grant.root = { ...f.registration.spec.firstInstallFunding.grant.root,
      resource: { name: "sre", namespace: "kars-system", uid: "other" } }; },
    (f: any) => { f.root.Values.sre.firstInstallFunding.expiresAt = "2999-01-01T00:00:00Z"; },
    (f: any) => { f.root.Values.sre.tokenBudget = 0; },
    (f: any) => { f.resources[1].spec.tokenBudget = { perRequestTokens: 0 }; },
    (f: any) => { f.resources[1].spec.tokenBudget = { perRequestTokens: 528000 }; },
    (f: any) => { f.resources[1].spec.contentSafety.requirePromptShields = false; },
    (f: any) => { f.resources[1].spec.appliesTo.sandboxName = "other"; },
    (f: any) => { f.resources[1].spec.modelPreference.fallback.push({ provider: "other", deployment: "other" }); },
  ])("rejects authority, safeguards, target, retained ceilings and unrelated values drift", mutate => {
    const f = caplessFixture(); mutate(f);
    expect(() => render(f)).toThrow();
  });
});

describe("adopted SRE daily calendar limit", () => {
  it("emits the finite increase and preserves immutable funding and adoption", () => {
    const f = dailyLimitFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.kind === "InferencePolicy").spec.tokenBudget).toEqual({ dailyTokens: 5000000, perRequestTokens: 528000 });
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    const selected = JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]);
    expect(selected.sre.dailyTokens).toBe(5000000);
    expect(selected.inferenceBudget).toEqual(before.root.Values.inferenceBudget);
    expect(f).toEqual(before);
  });
  it("rejects changing the daily limit during initial adoption", () => {
    expect(() => render(dailyLimitFixture(false))).toThrow(/completed adoption/);
  });
  it.each([0, -1, 1499999, 1.5, "5000000", null, 9007199254740992])("rejects an invalid limit %j", value => {
    const f = dailyLimitFixture();
    f.root.Values.sre.dailyTokens = value;
    f.resources[1].spec.tokenBudget.dailyTokens = value;
    expect(() => render(f)).toThrow(/finite integer increase/);
  });
  it.each([
    (f: any) => { f.resources[1].spec.tokenBudget.perRequestTokens = 0; },
    (f: any) => { f.resources[1].spec.tokenBudget.monthlyTokens = 0; },
    (f: any) => { f.resources[1].spec.contentSafety.requirePromptShields = false; },
    (f: any) => { f.resources[1].spec.appliesTo.sandboxName = "other"; },
    (f: any) => { f.resources[1].spec.tokenBudget.dailyTokens = 6000000; },
    (f: any) => { f.root.Values.sre.firstInstallFunding.tokens = 5000000; },
    (f: any) => { f.adoptionMap.immutable = false; },
    (f: any) => { f.adoptionMap.data.intentUID = "other"; },
  ])("does not exempt other policy, funding or receipt changes", mutate => {
    const f = dailyLimitFixture(); mutate(f);
    expect(() => render(f)).toThrow();
  });
});

describe("adopted operator non-inference classifications", () => {
  it.each([false, true])("preserves funding and receipts with concurrent model renewal %s", renewal => {
    const f = contractRenewalFixture();
    if (!renewal) {
      const budget = JSON.parse(f.intentMap.data["intent.json"]).budget;
      f.root.Values.inferenceBudget.catalogVersion = budget.catalogVersion;
      f.root.Values.inferenceBudget.contracts = budget.contracts;
    }
    const receipt = structuredClone(f.adoptionMap.data);
    f.root.Values.inferenceBudget.nonInferenceEgressHosts = ["www.zendesk.com", "www.intercom.com"];
    const documents = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(documents.find(doc => doc.metadata.name === "verified").data).toEqual(receipt);
    const selected = JSON.parse(documents.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(selected.inferenceBudget.nonInferenceEgressHosts).toEqual(["www.zendesk.com", "www.intercom.com"]);
    expect(selected.sre.firstInstallFunding).toEqual(f.root.Values.sre.firstInstallFunding);
  });
  it.each(["before-adoption", "model-host", "wildcard", "uppercase", "url", "port", "empty", "too-many", "model", "funding", "receipt"])("rejects %s changes", fault => {
    const f = contractRenewalFixture(fault !== "before-adoption");
    const budget = f.root.Values.inferenceBudget;
    budget.nonInferenceEgressHosts = ["www.zendesk.com"];
    if (fault === "model-host") budget.nonInferenceEgressHosts = ["provider.example"];
    if (fault === "wildcard") budget.nonInferenceEgressHosts = ["*.example"];
    if (fault === "uppercase") budget.nonInferenceEgressHosts = ["UPPER.example"];
    if (fault === "url") budget.nonInferenceEgressHosts = ["https://example.com"];
    if (fault === "port") budget.nonInferenceEgressHosts = ["example.com:443"];
    if (fault === "empty") budget.nonInferenceEgressHosts = [""];
    if (fault === "too-many") budget.nonInferenceEgressHosts = Array(65).fill("example.com");
    if (fault === "model") budget.contracts[0].model = "unreviewed";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "0".repeat(64);
    expect(() => render(f)).toThrow();
  });
});

describe("adopted model contract validity renewal", () => {
  it.each(["UTC", "Etc/GMT-2", "Etc/GMT+7"])("emits renewed contracts without altering sealed funding in %s", timeZone => {
    const f = contractRenewalFixture();
    f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
    f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true, timeZone)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    const selected = JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]);
    expect(selected.inferenceBudget).toEqual(before.root.Values.inferenceBudget);
    expect(f).toEqual(before);
  });
  it("forbids renewal before original adoption", () => {
    expect(() => render(contractRenewalFixture(false))).toThrow(/completed SRE adoption/);
  });
  it.each([
    ["endpoint", "https://different.example"], ["providerId", "another-provider"], ["model", "different-model"],
    ["operation", "Responses"], ["maximumInputTokens", 500000], ["maximumOutputTokens", 256000],
    ["maximumWireBytes", 2097152], ["outputBoundIncludesReasoning", false], ["outputField", "MaxTokens"],
    ["id", "another-contract"], ["maximumPrice", { input: 1 }], ["unexpected", true],
  ])("rejects contract-field changes: %s", (key, value) => {
    const f = contractRenewalFixture();
    f.root.Values.inferenceBudget.contracts[0][String(key)] = value;
    expect(() => render(f)).toThrow(/cannot change destinations/);
  });
  it.each(["", "2020-01-01T00:00:00Z", "2019-01-01T00:00:00Z", "2021-01-01T00:00:00Z", "2999-02-30T00:00:00Z", "2999-01-01T00:00:00+00:00"])("rejects invalid or non-renewing expiry %s", expiry => {
    const f = contractRenewalFixture();
    f.root.Values.inferenceBudget.contracts[0].validUntil = expiry;
    expect(() => render(f)).toThrow(/strictly later, unexpired UTC/);
  });
  it.each(["catalog", "contract", "catalog-only", "added", "removed", "receipt", "funding"])("rejects incomplete or unsealed transition %s", change => {
    const f = contractRenewalFixture();
    if (change === "catalog") f.root.Values.inferenceBudget.catalogVersion = "review-v1";
    if (change === "contract") f.root.Values.inferenceBudget.contracts[0].version = "bounds-v1";
    if (change === "catalog-only") f.root.Values.inferenceBudget.contracts = JSON.parse(f.intentMap.data["intent.json"]).budget.contracts;
    if (change === "added") f.root.Values.inferenceBudget.contracts.push(structuredClone(f.root.Values.inferenceBudget.contracts[0]));
    if (change === "removed") f.root.Values.inferenceBudget.contracts = [];
    if (change === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    if (change === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    expect(() => render(f)).toThrow();
  });
});

describe("additional model contracts after SRE adoption", () => {
  function additionalFixture(renewal = true, adopted = true): any {
    const f = contractRenewalFixture(adopted);
    const contract = { ...f.root.Values.inferenceBudget.contracts[0], id: "additional-chat", model: "additional-model", maximumInputTokens: 1050000 };
    if (!renewal) f.root.Values.inferenceBudget.contracts = JSON.parse(f.intentMap.data["intent.json"]).budget.contracts;
    f.root.Values.inferenceBudget.contracts.push(contract, { ...contract, id: "additional-responses", operation: "Responses", outputField: "MaxOutputTokens" });
    return f;
  }
  it.each([true, false])("adds model routes with renewal=%s without changing sealed SRE state", renewal => {
    const f = additionalFixture(renewal);
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]))
      .toEqual(before.root.Values);
    expect(f).toEqual(before);
  });
  it.each(["adoption", "duplicate-id", "duplicate-route", "original-model", "expired", "zero-bound", "missing-provider", "funding", "policy", "receipt"])("rejects additions hiding %s changes", fault => {
    const f = additionalFixture(true, fault !== "adoption");
    const extra = f.root.Values.inferenceBudget.contracts[1];
    if (fault === "duplicate-id") extra.id = "model-chat";
    if (fault === "duplicate-route") f.root.Values.inferenceBudget.contracts.push({ ...extra, id: "other-id" });
    if (fault === "original-model") extra.model = "reviewed-model";
    if (fault === "expired") extra.validUntil = "2020-01-01T00:00:00Z";
    if (fault === "zero-bound") extra.maximumInputTokens = 0;
    if (fault === "missing-provider") delete extra.providerId;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "policy") f.resources[0].spec = { changed: true };
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    expect(() => render(f)).toThrow();
  });
});

describe("controller catalogue from governed contracts", () => {
  function catalogue(extraEnv: unknown[] = [], enabled = true) {
    const contract = { providerId: "azure-openai", model: "gpt-5.4", operation: "ChatCompletions" };
    const values = { schemaHook: { enabled: false }, sre: { enabled: false }, agentMesh: { enabled: false },
      controller: { inferenceDefaults: { provider: "azure-openai", model: "gpt-5.4-mini" }, extraEnv },
      inferenceBudget: { enabled, catalogVersion: "test-v1", caBundle: "test-public-ca", tlsSecretName: "budget-tls", routerImageDigest: `sha256:${"a".repeat(64)}`,
        contracts: [contract, { ...contract, operation: "Responses" },
          { ...contract, model: "other-provider", providerId: "another-provider" },
          { ...contract, model: "embedding", operation: "Embeddings" }] } };
    const output = execFileSync("helm", ["template", "kars", `${root}deploy/helm/kars`, "--namespace", "kars-system", "--show-only", "templates/controller-deployment.yaml", "-f", "-"],
      { input: JSON.stringify(values), encoding: "utf8", timeout: 10000, stdio: ["pipe", "pipe", "pipe"] });
    const controller = parseAllDocuments(output).map(d => d.toJSON()).find(d => d?.kind === "Deployment");
    return controller.spec.template.spec.containers.find((c: any) => c.name === "controller").env;
  }
  it("exposes deduplicated conversational models on the default provider without changing the default", () => {
    const env = catalogue();
    expect(env.find((e: any) => e.name === "KARS_MODEL_CATALOG").value).toBe('["gpt-5.4"]');
    expect(env.find((e: any) => e.name === "KARS_TASK_DEFAULT_MODEL").value).toBe("gpt-5.4-mini");
  });
  it("preserves an explicit operator catalogue", () => {
    expect(catalogue([{ name: "KARS_MODEL_CATALOG", value: "operator-model" }]).filter((e: any) => e.name === "KARS_MODEL_CATALOG"))
      .toEqual([{ name: "KARS_MODEL_CATALOG", value: "operator-model" }]);
  });
  it("does not infer routes from a disabled broker", () => {
    expect(catalogue([], false).some((e: any) => e.name === "KARS_MODEL_CATALOG")).toBe(false);
  });
});
