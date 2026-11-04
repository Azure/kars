// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import { coreWitnessUpgradeFlags } from "../lib/core-helm-witness.js";
import {
  witnessFixture, hash, render, originalQualifiedWitnessImage, refreshedQualifiedWitnessImage,
  adoptedFixture, routerFixture, fixture,
} from "./sre-first-install-transition.test-support.js";

describe("sealed original datapath witness transitions", () => {
  function inheritedProfileFixture() {
    const f = witnessFixture("all");
    f.root.Values.global = { installationProfile: "kind-beta" };
    f.root.Values.datapathWitness.global = { installationProfile: "kind-beta" };
    f.root.Values.sre.datapathWitnessBaseline.global = { installationProfile: "kind-beta" };
    const original = structuredClone(f.root.Values);
    delete original.controller.image;
    delete original.sre.datapathWitnessBaseline;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(original));
    return f;
  }
  it("captures the original rendered profile from stored Helm values and reproduces the sealed receipt", async () => {
    const f = inheritedProfileFixture();
    f.root.Values.datapathWitness.global = {};
    delete f.root.Values.sre.datapathWitnessBaseline;
    Object.assign(f.adoptionMap.metadata, { name: "kars-sre-helm-adoption", namespace: "kars-system", resourceVersion: "1" });
    const flags = await coreWitnessUpgradeFlags(async file => ({ stdout: JSON.stringify(file === "kubectl" ? f.adoptionMap : f.root.Values) }), "kars", "kars-system", [], []);
    f.root.Values.sre.datapathWitnessBaseline = JSON.parse(flags[1].slice("sre.datapathWitnessBaseline=".length));
    const documents = parseAllDocuments(render(f, true, undefined, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(documents.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(documents.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.datapathWitness.global.installationProfile).toBe("kind-beta");
    expect(values.sre.datapathWitnessBaseline.global).toEqual({ installationProfile: "kind-beta" });
  });
  it.each(["parent", "child", "other-global", "baseline"])("rejects unsealed %s profile configuration", change => {
    const f = inheritedProfileFixture();
    if (change === "parent") f.root.Values.global.installationProfile = "aks-production";
    if (change === "child") f.root.Values.datapathWitness.global.installationProfile = "aks-production";
    if (change === "other-global") f.root.Values.datapathWitness.global.extra = true;
    if (change === "baseline") f.root.Values.sre.datapathWitnessBaseline.global.installationProfile = "changed";
    expect(() => render(f, true, undefined, change !== "child")).toThrow();
  });
  it("preserves an unchanged legacy selected upgrade and the exact receipt", () => {
    const f = witnessFixture();
    f.root.Values.datapathWitness.scope = "selected";
    const receipt = structuredClone(f.adoptionMap.data);
    const documents = parseAllDocuments(render(f, true, undefined, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(documents.find(doc => doc.metadata.name === "verified").data).toEqual(receipt);
    const values = JSON.parse(documents.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.datapathWitness.scope).toBe("selected");
    expect(values.sre.datapathWitnessBaseline).not.toHaveProperty("scope");
  });

  it.each([undefined, "selected", "all"] as const)("allows fleet scope and repeated image refreshes from original scope %s", scope => {
    const f = witnessFixture(scope);
    const baseline = structuredClone(f.root.Values.sre.datapathWitnessBaseline);
    const receipt = structuredClone(f.adoptionMap.data);
    const source = structuredClone(f.source);
    const funding = structuredClone(f.registration.spec.firstInstallFunding);
    Object.assign(f.root.Values.datapathWitness, { scope: "all", sandboxes: [] });
    for (const digest of ["b", "c"]) {
      f.root.Values.datapathWitness.aggregator.image = `registry.example/witness:latest@sha256:${digest.repeat(64)}`;
      f.root.Values.datapathWitness.gadget.image = f.root.Values.datapathWitness.aggregator.image;
      const documents = parseAllDocuments(render(f, true, undefined, true)).map(doc => doc.toJSON()).filter(Boolean);
      expect(documents.find(doc => doc.metadata.name === "verified").data).toEqual(receipt);
      const values = JSON.parse(documents.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
      expect(values.datapathWitness).toEqual(f.root.Values.datapathWitness);
      expect(values.sre.datapathWitnessBaseline).toEqual(baseline);
    }
    expect(f.source).toEqual(source);
    expect(f.registration.spec.firstInstallFunding).toEqual(funding);
  });

  it.each(["aggregator", "gadget", "both"])("allows qualified repository refresh of %s, then a tag refresh with the ORIGINAL baseline", field => {
    const f = witnessFixture(undefined, originalQualifiedWitnessImage);
    const baseline = structuredClone(f.root.Values.sre.datapathWitnessBaseline);
    const receipt = structuredClone(f.adoptionMap.data);
    const source = structuredClone(f.source);
    const registration = structuredClone(f.registration);
    Object.assign(f.root.Values.datapathWitness, { scope: "all", sandboxes: [] });
    for (const image of [refreshedQualifiedWitnessImage, `mirror.example:5443/reviewed/collector:reviewed@sha256:${"d".repeat(64)}`]) {
      for (const target of field === "both" ? ["aggregator", "gadget"] : [field]) f.root.Values.datapathWitness[target].image = image;
      const documents = parseAllDocuments(render(f, true, undefined, true)).map(doc => doc.toJSON()).filter(Boolean);
      expect(documents.find(doc => doc.metadata.name === "verified").data).toEqual(receipt);
      const values = JSON.parse(documents.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
      expect(values.datapathWitness).toEqual(f.root.Values.datapathWitness);
      expect(values.sre.datapathWitnessBaseline).toEqual(baseline);
      const images = documents.filter(doc => ["Deployment", "DaemonSet"].includes(doc.kind))
        .flatMap(doc => doc.spec.template.spec.containers.map((container: any) => container.image));
      expect(images).toContain(f.root.Values.datapathWitness.aggregator.image);
      expect(images).toContain(f.root.Values.datapathWitness.gadget.image);
    }
    expect(f.source).toEqual(source);
    expect(f.registration).toEqual(registration);
    // Even an authentic last deployment cannot replace the original sealed values.
    f.root.Values.sre.datapathWitnessBaseline = structuredClone(f.root.Values.datapathWitness);
    expect(() => render(f)).toThrow();
  });

  it.each(["aggregator", "gadget"])("rejects unpinned and invalid original/selected %s image references", field => {
    const invalid = ["", "registry.example/witness:latest", `registry.example/witness@sha256:${"a".repeat(63)}`,
      `registry.example/witness@sha256:${"a".repeat(65)}`, `registry.example/witness@sha256:${"g".repeat(64)}`,
      `registry.example/witness@sha256:${"A".repeat(64)}`, `registry.example/witness@sha512:${"a".repeat(64)}`,
      `registry.example/witness @sha256:${"a".repeat(64)}`, `https://registry.example/witness@sha256:${"a".repeat(64)}`,
      `registry.example//witness@sha256:${"a".repeat(64)}`, `registry.example/witness:@sha256:${"a".repeat(64)}`,
      `registry.example/../witness@sha256:${"a".repeat(64)}`, `registry.example/witness@sha256:${"a".repeat(64)}?query=1`,
      `registry.example/witness@sha256:${"a".repeat(64)}\n`, 42];
    for (const image of invalid) {
      for (const original of [false, true]) {
        const f = witnessFixture();
        Object.assign(f.root.Values.datapathWitness, { scope: "all", sandboxes: [] });
        const config = original ? f.root.Values.sre.datapathWitnessBaseline : f.root.Values.datapathWitness;
        config[field].image = image;
        expect(() => render(f)).toThrow(/Datapath witness (refresh requires valid immutable|images must be strings)/);
      }
    }
  });

  it("does not silently interpret the new dependency default as legacy selected scope", () => {
    const f = witnessFixture();
    expect(() => render(f, false, undefined, true)).toThrow();
  });

  it("requires adoption, and rejects a forged or most-recent rather than original baseline", () => {
    for (const change of ["unadopted", "forged-image", "latest-scope", "forged-node"]) {
      const f = witnessFixture();
      const baseline = f.root.Values.sre.datapathWitnessBaseline;
      if (change === "unadopted") f.adoptionMap = {};
      if (change === "forged-image") baseline.aggregator.image = `registry.example/witness:latest@sha256:${"d".repeat(64)}`;
      if (change === "latest-scope") Object.assign(baseline, { scope: "all", sandboxes: [] });
      if (change === "forged-node") baseline.nodes = ["another-node"];
      Object.assign(f.root.Values.datapathWitness, { scope: "all", sandboxes: [] });
      expect(() => render(f)).toThrow();
    }
  });

  it.each([
    ["other", []], ["", []], [false, []], ["all", ["sre"]], ["selected", []],
    ["selected", ["sre", "sre"]], ["selected", ["../sre"]], ["selected", [1]], ["all", null],
  ])("rejects invalid scope/list %j / %j without relying on dependency validation", (scope, sandboxes) => {
    const f = witnessFixture();
    Object.assign(f.root.Values.datapathWitness, { scope, sandboxes });
    expect(() => render(f)).toThrow();
  });

  it.each([
    (f: any) => { f.root.Values.datapathWitness.nodes = []; },
    (f: any) => { f.root.Values.datapathWitness.nodes.push("unapproved-node"); },
    (f: any) => { f.root.Values.datapathWitness.gadget.hookMode = "fanotify+ebpf"; },
    (f: any) => { f.root.Values.datapathWitness.gadget.dnsImage = `other@sha256:${"a".repeat(64)}`; },
    (f: any) => { f.root.Values.datapathWitness.gadget.tcpImage = `other@sha256:${"a".repeat(64)}`; },
    (f: any) => { f.root.Values.datapathWitness.aggregator.imagePullSecrets = [{ name: "other" }]; },
    (f: any) => { f.root.Values.datapathWitness.aggregator.windowSeconds++; },
    (f: any) => { f.root.Values.datapathWitness.aggregator.resources = { limits: { memory: "8Gi" } }; },
    (f: any) => { f.root.Values.datapathWitness.managed = false; },
    (f: any) => { f.root.Values.datapathWitness.enabled = false; },
    (f: any) => { f.root.Values.datapathWitness.rbac = { clusterAdmin: true }; },
    (f: any) => { f.root.Values.datapathWitness.gadget.privileged = false; },
    (f: any) => { f.root.Values.datapathWitness.aggregator.image = "registry.example/witness:latest"; },
    (f: any) => { f.root.Values.datapathWitness.gadget.image = `other/witness:latest@sha256:invalid`; },
    (f: any) => { f.root.Values.sre.firstInstallFunding.tokens++; },
    (f: any) => { f.registration.spec.sandbox.uid = "replacement"; },
    (f: any) => { f.root.Values.controller.serviceAccount = "other"; },
  ])("rejects node, resource, authority, safety or unrelated change %#", change => {
    const f = witnessFixture();
    Object.assign(f.root.Values.datapathWitness, { scope: "all", sandboxes: [] });
    f.root.Values.datapathWitness.aggregator.image = refreshedQualifiedWitnessImage;
    f.root.Values.datapathWitness.gadget.image = refreshedQualifiedWitnessImage;
    change(f);
    expect(() => render(f)).toThrow();
  });
});

describe("separately governed Bridge orchestrator funding", () => {
  const grant = () => ({ root: { kind: "KarsSandbox", resource: { namespace: "kars-system", name: "bridge-orchestrator", uid: "fresh-orchestrator" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" }, limits: { tokens: 1200000 }, expiresAt: "2999-01-01T00:00:00Z" });
  function sealFunding(f: any, funding: unknown[] | undefined) {
    if (funding !== undefined) f.root.Values.inferenceBudget.builtInFunding = funding;
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
  }
  it.each([undefined, []])("adds a Bridge grant without rewriting sealed SRE receipts (baseline %j)", funding => {
    const f = adoptedFixture();
    sealFunding(f, funding);
    const receipt = structuredClone(f.adoptionMap.data);
    f.root.Values.inferenceBudget.builtInFunding = [grant()];
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(receipt);
    expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"]).inferenceBudget.builtInFunding).toEqual([grant()]);
  });
  it("composes with supported runtime, router, dispatcher and top-up changes", () => {
    const f = routerFixture(true);
    f.root.Values.inferenceBudget.builtInFunding = [grant()];
    f.root.Values.inferenceBudget.tokenTopUps = [{ independentlyAuthorized: true }];
    expect(() => render(f)).not.toThrow();
  });
  it("retains an originally sealed Bridge grant during a supported pull-coverage change", () => {
    const f = adoptedFixture();
    sealFunding(f, [grant()]);
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    expect(() => render(f)).not.toThrow();
  });
  it("preserves every other sealed actor grant", () => {
    const f = adoptedFixture();
    const original = { ...grant(), root: { ...grant().root, resource: { ...grant().root.resource, name: "sre" } } };
    sealFunding(f, [original]);
    f.root.Values.inferenceBudget.builtInFunding = [original, grant()];
    expect(() => render(f)).not.toThrow();
    f.root.Values.inferenceBudget.builtInFunding[0].limits.tokens++;
    expect(() => render(f)).toThrow();
  });
  it("does not permit funding during initial SRE adoption", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.builtInFunding = [grant()];
    expect(() => render(f)).toThrow();
  });
  it.each(["sre", "unknown-actor", "wrong-namespace", "wrong-kind", "duplicate", "original-limit", "original-expiry", "catalog", "credential", "receipt"])("does not hide %s changes behind a Bridge grant", fault => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.builtInFunding = [grant()];
    const selected = f.root.Values.inferenceBudget.builtInFunding[0];
    if (fault === "sre" || fault === "unknown-actor") selected.root.resource.name = fault;
    if (fault === "wrong-namespace") selected.root.resource.namespace = "another-workspace";
    if (fault === "wrong-kind") selected.root.kind = "KarsTask";
    if (fault === "duplicate") f.root.Values.inferenceBudget.builtInFunding.push(grant());
    if (fault === "original-limit") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "original-expiry") f.root.Values.sre.firstInstallFunding.expiresAt = "2998-01-01T00:00:00Z";
    if (fault === "catalog") f.root.Values.inferenceBudget.caBundle = "changed";
    if (fault === "credential") f.root.Values.controller.extraEnv.push({ name: "AZURE_OPENAI_API_KEY", value: "changed" });
    if (fault === "receipt") f.adoptionMap.data.intentUID = "changed";
    expect(() => render(f)).toThrow();
  });
});

describe("adopted Foundry project configuration", () => {
  function projectFixture(composed = false, original = "") {
    const f = composed ? routerFixture(true) : adoptedFixture();
    f.root.Values.foundry = { endpoint: "https://inference.example", projectEndpoint: original, deployments: "[]" };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    if (composed) {
      baseline.sandbox.image.tag = `latest@sha256:${"a".repeat(64)}`;
      baseline.runtimes.hermes.image = `registry.example/hermes:latest@sha256:${"a".repeat(64)}`;
      delete baseline.sre.runtimeImageBaseline;
      baseline.inferenceRouter.image.tag = `latest@sha256:${"a".repeat(64)}`;
      baseline.inferenceBudget.routerImageDigest = `sha256:${"a".repeat(64)}`;
      delete baseline.sre.routerImageBaseline;
      baseline.missionDispatcher = { enabled: false, authMode: "", image: {
        repository: "karsacr.azurecr.io/kars-mission-dispatcher", digest: "" }, relayUrl: "", registryUrl: "" };
    }
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.foundry.projectEndpointBaseline = original;
    f.root.Values.foundry.projectEndpoint = "https://account.services.ai.azure.com/api/projects/project";
    return f;
  }
  it.each([false, true])("changes only the project URL, composing with image/dispatcher transitions: %s", composed => {
    const f = projectFixture(composed);
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    const selected = JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]);
    expect(selected.foundry).toEqual(before.root.Values.foundry);
    expect(selected.inferenceBudget).toEqual(before.root.Values.inferenceBudget);
    expect(f.source).toEqual(before.source);
    expect(f.registration).toEqual(before.registration);
    expect(f.intentMap).toEqual(before.intentMap);
  });
  it("retains the original project baseline across subsequent edits", () => {
    const original = "https://old.services.ai.azure.com/api/projects/old-project";
    const f = projectFixture(false, original);
    expect(render(f)).toContain("name: verified");
    f.root.Values.foundry.projectEndpoint = "https://new.services.ai.azure.com/api/projects/new-project";
    expect(render(f)).toContain("name: verified");
    f.root.Values.foundry.projectEndpointBaseline = "https://account.services.ai.azure.com/api/projects/project";
    expect(() => render(f)).toThrow();
  });
  it.each(["initial-adoption", "missing-baseline", "forged-baseline", "baseline-type", "selected-type", "inference-endpoint",
    "deployments", "credentials", "funding", "source", "receipt"])("rejects unrelated or unbound change: %s", fault => {
    const f = projectFixture();
    if (fault === "initial-adoption") f.adoptionMap = {};
    if (fault === "missing-baseline") delete f.root.Values.foundry.projectEndpointBaseline;
    if (fault === "forged-baseline") f.root.Values.foundry.projectEndpointBaseline = "forged";
    if (fault === "baseline-type") f.root.Values.foundry.projectEndpointBaseline = {};
    if (fault === "selected-type") f.root.Values.foundry.projectEndpoint = {};
    if (fault === "inference-endpoint") f.root.Values.foundry.endpoint = "https://replacement.example";
    if (fault === "deployments") f.root.Values.foundry.deployments = '["replacement"]';
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "AZURE_OPENAI_API_KEY", value: "replacement" });
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.spec.unreviewed = true;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    expect(() => render(f)).toThrow();
  });
  it.each(["", "http://account.services.ai.azure.com/api/projects/project", "https://foreign.example/api/projects/project",
    "https://account.services.ai.azure.com.evil.example/api/projects/project", "https://user:password@account.services.ai.azure.com/api/projects/project",
    "https://account.services.ai.azure.com:443/api/projects/project", "https://account.services.ai.azure.com/api/projects/../other",
    "https://account.services.ai.azure.com/api/projects/project?query=1", "https://account.services.ai.azure.com/api/projects/project#fragment"])(
    "rejects noncanonical selected endpoint %s", endpoint => {
      const f = projectFixture();
      f.root.Values.foundry.projectEndpoint = endpoint;
      expect(() => render(f)).toThrow();
    });
});

describe("adopted evaluation runner refresh", () => {
  function initialRunnerFixture() {
    const f = fixture();
    f.root.Values.evals = {
      runnerImageBaseline: "ghcr.io/azure/kars-conformance-runner:latest",
      runnerImage: `registry.example/runner:latest@sha256:${"b".repeat(64)}`,
    };
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    return f;
  }
  it("adopts an already sealed runner baseline and retains that receipt for subsequent upgrades", () => {
    const f = initialRunnerFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    const receipt = docs.find(d => d.metadata.name === "verified").data;
    const projected = structuredClone(f.root.Values);
    delete projected.controller.image;
    projected.evals.runnerImage = projected.evals.runnerImageBaseline;
    delete projected.evals.runnerImageBaseline;
    expect(receipt).toEqual({ intentUID: "intent-uid", valuesDigest: hash(canonicalSchema(projected)) });
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
    expect(f).toEqual(before);
    f.adoptionMap = { ...structuredClone(adoptedFixture().adoptionMap), data: receipt };
    f.root.Values.evals.runnerImage = `registry.example/runner:latest@sha256:${"c".repeat(64)}`;
    const upgraded = parseAllDocuments(render(f)).map(d => d.toJSON()).filter(Boolean);
    expect(upgraded.find(d => d.metadata.name === "verified").data).toEqual(receipt);
  });
  it.each(["baseline", "selected", "other-value", "source", "funding", "blocked", "controller-uid"])(
    "rejects initial sealed-runner adoption with changed %s", fault => {
      const f = initialRunnerFixture();
      if (fault === "baseline") f.root.Values.evals.runnerImageBaseline = "foreign.example/runner:latest";
      if (fault === "selected") f.root.Values.evals.runnerImage = `registry.example/runner:latest@sha256:${"c".repeat(64)}`;
      if (fault === "other-value") f.root.Values.controller.extraEnv.push({ name: "UNREVIEWED", value: "true" });
      if (fault === "source") f.source.spec.unreviewed = true;
      if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
      if (fault === "blocked") f.registration.status.phase = "Blocked";
      if (fault === "controller-uid") f.controller.metadata.uid = "different-controller";
      expect(() => render(f)).toThrow();
    });
  function runnerFixture(original = "ghcr.io/azure/kars-conformance-runner:latest") {
    const f = adoptedFixture();
    f.root.Values.evals = { runnerImage: original };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.evals.runnerImageBaseline = original;
    f.root.Values.evals.runnerImage = `localhost:5077/kars-conformance-runner:latest@sha256:${"b".repeat(64)}`;
    return f;
  }
  it.each(["ghcr.io/azure/kars-conformance-runner:latest", `registry.example/runner:latest@sha256:${"a".repeat(64)}`])(
    "refreshes %s against its sealed original without changing funding or resources", original => {
      const f = runnerFixture(original);
      const before = structuredClone(f);
      for (const digest of ["b", "c"]) {
        f.root.Values.evals.runnerImage = `localhost:5077/kars-conformance-runner:latest@sha256:${digest.repeat(64)}`;
        const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
        expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
        expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
        const selected = JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]);
        expect(selected.evals).toEqual(f.root.Values.evals);
        expect(selected.inferenceBudget).toEqual(before.root.Values.inferenceBudget);
        expect(selected.sre).toEqual(before.root.Values.sre);
      }
    });
  it.each(["initial-adoption", "missing-baseline", "forged-baseline", "empty-baseline", "baseline-type", "selected-type",
    "mutable-selected", "malformed-digest", "other-eval-setting", "controller-env", "funding", "source", "receipt"])(
    "rejects runner refresh with changed %s", fault => {
      const f = runnerFixture();
      if (fault === "initial-adoption") f.adoptionMap = {};
      if (fault === "missing-baseline") delete f.root.Values.evals.runnerImageBaseline;
      if (fault === "forged-baseline") f.root.Values.evals.runnerImageBaseline = "foreign.example/runner:latest";
      if (fault === "empty-baseline") f.root.Values.evals.runnerImageBaseline = "";
      if (fault === "baseline-type") f.root.Values.evals.runnerImageBaseline = {};
      if (fault === "selected-type") f.root.Values.evals.runnerImage = {};
      if (fault === "mutable-selected") f.root.Values.evals.runnerImage = "localhost:5077/runner:latest";
      if (fault === "malformed-digest") f.root.Values.evals.runnerImage += "bad";
      if (fault === "other-eval-setting") f.root.Values.evals.disablePolicies = true;
      if (fault === "controller-env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
      if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
      if (fault === "source") f.source.spec.unreviewed = true;
      if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
      expect(() => render(f)).toThrow();
    });
});
