// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parse, parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import { annotations, canonical, consumesPrivateAuthority, templateDigest } from "../lib/private-activation.js";
import { retirementBinding } from "../lib/private-activation-retirement.js";
import { coreWitnessUpgradeFlags } from "../lib/core-helm-witness.js";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const hash = (value: string) => createHash("sha256").update(value).digest("hex");
function fixture(tokens = 1000, tokenTopUps?: unknown[], expiresAt = "2999-01-01T00:00:00Z", unbounded = false): any {
  const owner = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
  const funding = unbounded ? { unboundedTokens: true } : { tokens, expiresAt };
  const budget = { enabled: true, routerImageDigest: `sha256:${"a".repeat(64)}`, ...(tokenTopUps ? { tokenTopUps } : {}) };
  const values = { schemaHook: { enabled: true }, sre: { enabled: true, firstInstallFunding: funding }, inferenceBudget: budget,
    controller: { extraEnv: [], image: { repository: "registry.example/controller", tag: `latest@sha256:${"a".repeat(64)}`, digest: `sha256:${"a".repeat(64)}` } },
    inferenceRouter: { image: { repository: "registry.example/router", tag: `latest@${budget.routerImageDigest}` } } };
  const resources = [{ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox", metadata: { name: "sre", namespace: "kars-system" }, spec: {} }];
  const encoded = JSON.stringify({ version: 1, release: "kars", namespace: "kars-system", resources, funding, budget,
    valuesDigest: hash(canonicalSchema(values)) });
  const grant = { root: { kind: "KarsSandbox", resource: { name: "sre", namespace: "kars-system", uid: "source-uid" }, workspaceUid: "workspace-uid", clusterUid: "cluster-uid" }, ...(unbounded ? { limits: {}, unboundedTokens: true } : { limits: { tokens }, expiresAt }) };
  return { root: { Release: { Name: "kars", Namespace: "kars-system" }, Values: { ...values, schemaHook: { enabled: false } } }, resources, receipt: {}, adoptionMap: {},
    grafanaDefaults: JSON.parse(readFileSync(`${root}deploy/helm/kars/files/monitoring/grafana-defaults.json`, "utf8")),
    intentMap: { immutable: true, metadata: { uid: "intent-uid", annotations: owner }, data: { "intent.json": encoded } },
    source: { metadata: { uid: "source-uid", annotations: { ...owner } }, spec: { inferenceBudgetRef: { root: grant.root, account: { uid: "account-uid" } } } },
    registration: { metadata: { generation: 1 }, spec: { enabled: true, sandbox: { uid: "source-uid" }, controller: { release: "kars", deployment: { uid: "controller-uid" }, namespace: { uid: "workspace-uid" } }, runtimeNamespace: { uid: "runtime-uid" }, firstInstallFunding: { intent: { uid: "intent-uid", digest: hash(encoded) }, grant } }, status: { phase: "Ready", privacyRevision: "kars.azure.com/sre-privacy/v2", observedGeneration: 1 } },
    controller: { metadata: { uid: "controller-uid" } }, runtime: { metadata: { uid: "runtime-uid" } }, workspace: { metadata: { uid: "workspace-uid" } } };
}
function render(input: any, exposeValues = false, timeZone?: string, witnessDependency = false) {
  const dir = `${root}.sre-transition-check-${randomUUID()}`;
  mkdirSync(`${dir}/templates`, { recursive: true });
  try {
    writeFileSync(`${dir}/Chart.yaml`, "apiVersion: v2\nname: transition-check\nversion: 0.1.0\n" + (witnessDependency
      ? "dependencies:\n  - name: kars-datapath-witness\n    alias: datapathWitness\n    version: 0.1.0\n" : ""));
    if (witnessDependency) {
      cpSync(`${root}deploy/helm/kars-datapath-witness`, `${dir}/charts/kars-datapath-witness`, { recursive: true });
      copyFileSync(`${root}deploy/helm/kars/values.schema.json`, `${dir}/values.schema.json`);
    }
    writeFileSync(`${dir}/values.yaml`, JSON.stringify(input.root.Values));
    writeFileSync(`${dir}/fixture.json`, JSON.stringify(input));
    copyFileSync(`${root}deploy/helm/kars/templates/_sre-first-install.tpl`, `${dir}/templates/_transition.tpl`);
    copyFileSync(`${root}deploy/helm/kars/templates/_sre-resources.tpl`, `${dir}/templates/_sre-resources.tpl`);
    copyFileSync(`${root}deploy/helm/kars/templates/_sre-private-continuity.tpl`, `${dir}/templates/_private-continuity.tpl`);
    if (exposeValues) {
      const controller = readFileSync(`${root}deploy/helm/kars/templates/controller-deployment.yaml`, "utf8");
      const start = controller.indexOf("            - name: KARS_INFERENCE_BUDGET_ROUTER_DIGEST\n");
      const end = controller.indexOf("            - name: KARS_INFERENCE_BUDGET_ADDR\n", start);
      expect(start).toBeGreaterThanOrEqual(0);
      expect(end).toBeGreaterThan(start);
      writeFileSync(`${dir}/templates/router-env.yaml`, "apiVersion: v1\nkind: Pod\nmetadata:\n  name: controller-env\nspec:\n  containers:\n    - name: controller\n      env:\n" + controller.slice(start, end));
    }
    writeFileSync(`${dir}/templates/result.yaml`, '{{ $input := .Files.Get "fixture.json" | fromJson }}{{ $_ := set $input.root "Values" .Values }}{{ if hasKey $input.registration.spec.firstInstallFunding.grant.limits "tokens" }}{{ $_ := set $input.registration.spec.firstInstallFunding.grant.limits "tokens" (int64 $input.registration.spec.firstInstallFunding.grant.limits.tokens) }}{{ end }}{{ include "kars.sre.firstInstallTransition" $input }}\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: verified\ndata: {{ $input.receipt | toJson }}\n---\n{{ range $input.resources }}{{ toYaml . }}\n---\n{{ end }}' + (exposeValues ? '\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: selected-values\ndata:\n  values.json: {{ $input.root.Values | toJson | quote }}\n' : ''));
    return execFileSync("helm", ["template", witnessDependency ? "kars" : "transition-check", dir,
      ...(witnessDependency ? ["--namespace", "kars-system"] : [])], { encoding: "utf8", timeout: 10000, stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, ...(timeZone ? { TZ: timeZone } : {}) } });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
function adoptedFixture(tokenTopUps?: unknown[], expiresAt?: string): any {
  const f = fixture(1000, tokenTopUps, expiresAt);
  const values = structuredClone(f.root.Values);
  delete values.controller.image;
  f.adoptionMap = { immutable: true, metadata: { uid: "adoption-uid", annotations: { ...f.intentMap.metadata.annotations } },
    data: { intentUID: f.intentMap.metadata.uid, valuesDigest: hash(canonicalSchema(values)) } };
  return f;
}
describe("SRE controller operation fences", () => {
  function fencedFixture() {
    const f = adoptedFixture();
    f.controller.metadata.resourceVersion = "123";
    f.root.Values.controller.upgradePrecondition = { uid: "controller-uid", resourceVersion: "123" };
    return f;
  }

  it("preserves the sealed adoption receipt while retaining the exact live operation fence", () => {
    const f = fencedFixture();
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.controller.upgradePrecondition).toEqual(f.root.Values.controller.upgradePrecondition);
  });

  it.each([
    null, [], "123", {}, { uid: "controller-uid" },
    { uid: "", resourceVersion: "123" }, { uid: "controller-uid", resourceVersion: "" },
    { uid: "controller-uid", resourceVersion: 123 }, { uid: 123, resourceVersion: "123" },
    { uid: "changed", resourceVersion: "123" }, { uid: "controller-uid", resourceVersion: "122" },
    { uid: "controller-uid", resourceVersion: "123", extra: true },
  ].map(fence => ({ fence })))("rejects invalid or stale fences: $fence", ({ fence }) => {
    const f = fencedFixture();
    f.root.Values.controller.upgradePrecondition = fence;
    expect(() => render(f)).toThrow(/upgradePrecondition/);
  });

  it.each(["adoption", "live-version", "credentials", "receipt"])("does not exempt %s changes", fault => {
    const f = fencedFixture();
    if (fault === "adoption") f.adoptionMap = {};
    if (fault === "live-version") delete f.controller.metadata.resourceVersion;
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "AZURE_OPENAI_API_KEY", value: "changed" });
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    expect(() => render(f)).toThrow();
  });
});

describe("SRE Kind kube-state-metrics capacity refresh", () => {
  function capacityFixture() {
    const f = adoptedFixture();
    f.root.Values.global = { installationProfile: "kind-beta" };
    f.root.Values.monitoring = { enabled: true, "kube-state-metrics": {
      resources: { requests: { cpu: "25m", memory: "32Mi" }, limits: { cpu: "250m", memory: "128Mi" } },
      image: { repository: "registry.example/metrics", tag: "latest" } } };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.monitoring.kubeStateMetricsResourcesBaseline = structuredClone(baseline.monitoring["kube-state-metrics"].resources);
    f.root.Values.monitoring["kube-state-metrics"].resources = {
      requests: { cpu: "25m", memory: "64Mi" }, limits: { cpu: "500m", memory: "256Mi" } };
    return f;
  }
  it("retains the sealed receipt while emitting the bounded selected resources", () => {
    const f = capacityFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.monitoring).toEqual(f.root.Values.monitoring);
    expect(f).toEqual(before);
  });
  it.each(["unadopted", "production", "disabled", "forged-baseline", "mutable-receipt", "baseline-type", "selected-type",
    "missing-limits", "extra-group", "group-type", "extra-resource", "numeric", "zero", "negative", "overflow", "unit",
    "request-exceeds-limit", "baseline-request-exceeds-limit", "decrease", "image", "probe", "credentials", "other-component"])(
    "rejects capacity refresh with %s", fault => {
      const f = capacityFixture();
      const monitoring = f.root.Values.monitoring;
      const metrics = monitoring["kube-state-metrics"];
      if (fault === "unadopted") f.adoptionMap = {};
      if (fault === "production") f.root.Values.global.installationProfile = "production";
      if (fault === "disabled") monitoring.enabled = false;
      if (fault === "forged-baseline") monitoring.kubeStateMetricsResourcesBaseline.requests.memory = "16Mi";
      if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
      if (fault === "baseline-type") monitoring.kubeStateMetricsResourcesBaseline = [];
      if (fault === "selected-type") metrics.resources = [];
      if (fault === "missing-limits") delete metrics.resources.limits;
      if (fault === "extra-group") metrics.resources.claims = [];
      if (fault === "group-type") metrics.resources.limits = "500m";
      if (fault === "extra-resource") metrics.resources.limits["nvidia.com/gpu"] = "1";
      if (fault === "numeric") metrics.resources.limits.cpu = 500;
      if (fault === "zero") metrics.resources.limits.cpu = "0m";
      if (fault === "negative") metrics.resources.limits.cpu = "-1m";
      if (fault === "overflow") metrics.resources.limits.cpu = "999999999999999999999m";
      if (fault === "unit") metrics.resources.limits.memory = "256MB";
      if (fault === "request-exceeds-limit") metrics.resources.requests.cpu = "501m";
      if (fault === "baseline-request-exceeds-limit") monitoring.kubeStateMetricsResourcesBaseline.requests.cpu = "251m";
      if (fault === "decrease") metrics.resources.requests.cpu = "24m";
      if (fault === "image") metrics.image.repository = "unreviewed";
      if (fault === "probe") metrics.livenessProbe = { enabled: false };
      if (fault === "credentials") metrics.extraEnv = [{ name: "TOKEN", value: "unreviewed" }];
      if (fault === "other-component") monitoring.prometheus = { enabled: false };
      expect(() => render(f)).toThrow();
    });
});

describe("SRE Kind metrics node address refresh", () => {
  function metricsFixture() {
    const f = adoptedFixture();
    f.root.Values.global = { installationProfile: "kind-beta" };
    f.root.Values.metricsServer = { enabled: true,
      hostAliases: [{ ip: "172.20.0.2", hostnames: ["kind-control-plane"] }] };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.metricsServer.hostAliasesBaseline = structuredClone(baseline.metricsServer.hostAliases);
    f.root.Values.metricsServer.hostAliases[0].ip = "172.20.0.3";
    f.metricsNodes = { "kind-control-plane": { apiVersion: "v1", kind: "Node",
      metadata: { name: "kind-control-plane", uid: "node-uid" },
      status: { addresses: [{ type: "InternalIP", address: "172.20.0.3" }] } } };
    return f;
  }
  it("retains the sealed receipt and only selects a live InternalIP for the original node hostname", () => {
    const f = metricsFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.metricsServer).toEqual(f.root.Values.metricsServer);
    expect(f).toEqual(before);
  });
  it.each(["unadopted", "profile", "disabled", "baseline", "baseline-type", "selected-type", "empty", "extra-alias",
    "hostname", "extra-hostname", "extra-field", "missing-node", "node-uid", "node-name", "node-kind", "deleting-node",
    "external-ip", "wrong-ip", "mutable-receipt", "other-settings"])("rejects metrics address refresh with %s", fault => {
    const f = metricsFixture();
    const metrics = f.root.Values.metricsServer;
    const node = f.metricsNodes["kind-control-plane"];
    if (fault === "unadopted") f.adoptionMap = {};
    if (fault === "profile") f.root.Values.global.installationProfile = "production";
    if (fault === "disabled") metrics.enabled = false;
    if (fault === "baseline") metrics.hostAliasesBaseline[0].ip = "172.20.0.4";
    if (fault === "baseline-type") metrics.hostAliasesBaseline = {};
    if (fault === "selected-type") metrics.hostAliases = {};
    if (fault === "empty") metrics.hostAliases = [];
    if (fault === "extra-alias") metrics.hostAliases.push(structuredClone(metrics.hostAliases[0]));
    if (fault === "hostname") metrics.hostAliases[0].hostnames[0] = "foreign.example";
    if (fault === "extra-hostname") metrics.hostAliases[0].hostnames.push("foreign.example");
    if (fault === "extra-field") metrics.hostAliases[0].bypass = true;
    if (fault === "missing-node") f.metricsNodes = {};
    if (fault === "node-uid") node.metadata.uid = "";
    if (fault === "node-name") node.metadata.name = "replacement";
    if (fault === "node-kind") node.kind = "Pod";
    if (fault === "deleting-node") node.metadata.deletionTimestamp = "2026-11-04T12:00:00Z";
    if (fault === "external-ip") node.status.addresses[0].type = "ExternalIP";
    if (fault === "wrong-ip") node.status.addresses[0].address = "172.20.0.4";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "other-settings") metrics.insecureSkipTLSVerify = true;
    expect(() => render(f)).toThrow();
  });
});

function dispatcherFixture(adopted = true): any {
  const f = fixture();
  f.root.Values.global = { installationProfile: "kind-beta" };
  f.root.Values.missionDispatcher = { enabled: false, authMode: "", image: {
    repository: "karsacr.azurecr.io/kars-mission-dispatcher", digest: "" }, relayUrl: "", registryUrl: "" };
  const original = structuredClone(f.root.Values);
  original.schemaHook.enabled = true;
  const intent = JSON.parse(f.intentMap.data["intent.json"]);
  intent.valuesDigest = hash(canonicalSchema(original));
  f.intentMap.data["intent.json"] = JSON.stringify(intent);
  f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
  if (adopted) {
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap = { immutable: true, metadata: { uid: "adoption-uid", annotations: { ...f.intentMap.metadata.annotations } },
      data: { intentUID: f.intentMap.metadata.uid, valuesDigest: hash(canonicalSchema(baseline)) } };
  }
  Object.assign(f.root.Values.missionDispatcher, { enabled: true, authMode: "development",
    image: { repository: "localhost:5077/kars-mission-dispatcher", digest: `sha256:${"c".repeat(64)}` } });
  return f;
}
function dispatcherRefreshFixture(tag = false, authMode = "development"): any {
  const f = dispatcherFixture();
  const mission = f.root.Values.missionDispatcher;
  mission.authMode = authMode;
  if (tag) mission.image.tag = `latest@${mission.image.digest}`;
  const original = structuredClone(f.root.Values);
  delete original.controller.image;
  f.adoptionMap.data.valuesDigest = hash(canonicalSchema(original));
  mission.imageBaseline = structuredClone(mission.image);
  mission.image.digest = `sha256:${"d".repeat(64)}`;
  if (tag) mission.image.tag = `latest@${mission.image.digest}`;
  return f;
}
function runtimeFixture(adopted = true, dispatcher = false, explicitDigest = false): any {
  const f = dispatcher ? dispatcherFixture(false) : fixture();
  if (dispatcher) f.root.Values.missionDispatcher = { enabled: false, authMode: "", image: {
    repository: "karsacr.azurecr.io/kars-mission-dispatcher", digest: "" }, relayUrl: "", registryUrl: "" };
  f.root.Values.sandbox = { image: { repository: "registry.example/openclaw", tag: `latest@sha256:${"a".repeat(64)}`, pullPolicy: "IfNotPresent" } };
  f.root.Values.runtimes = { hermes: { image: `registry.example/hermes:latest@sha256:${"a".repeat(64)}` } };
  if (explicitDigest) f.root.Values.sandbox.image.digest = `sha256:${"a".repeat(64)}`;
  const original = structuredClone(f.root.Values);
  original.schemaHook.enabled = true;
  const intent = JSON.parse(f.intentMap.data["intent.json"]);
  intent.valuesDigest = hash(canonicalSchema(original));
  f.intentMap.data["intent.json"] = JSON.stringify(intent);
  f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
  if (adopted) {
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
  }
  f.root.Values.sre.runtimeImageBaseline = {
    openclaw: `${f.root.Values.sandbox.image.repository}:${f.root.Values.sandbox.image.tag}`,
    hermes: f.root.Values.runtimes.hermes.image,
  };
  f.root.Values.sandbox.image.tag = `latest@sha256:${"b".repeat(64)}`;
  if (explicitDigest) f.root.Values.sandbox.image.digest = `sha256:${"b".repeat(64)}`;
  f.root.Values.runtimes.hermes.image = `registry.example/hermes:latest@sha256:${"c".repeat(64)}`;
  if (dispatcher) Object.assign(f.root.Values.missionDispatcher, { enabled: true, authMode: "development",
    image: { repository: "localhost:5077/kars-mission-dispatcher", digest: `sha256:${"d".repeat(64)}` } });
  return f;
}
function routerFixture(composed = false, adopted = true): any {
  const f = composed ? runtimeFixture(adopted, true) : adopted ? adoptedFixture() : fixture();
  f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
  f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
  f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
  return f;
}
function contractRenewalFixture(adopted = true): any {
  const f = fixture();
  Object.assign(f.root.Values.inferenceBudget, { catalogVersion: "review-v1", contracts: [{
    id: "model-chat", version: "bounds-v1", providerId: "azure-openai", endpoint: "https://provider.example",
    model: "reviewed-model", operation: "ChatCompletions", maximumInputTokens: 400000,
    maximumOutputTokens: 128000, maximumWireBytes: 1048576, outputBoundIncludesReasoning: true,
    outputField: "MaxCompletionTokens", validUntil: "2020-01-01T00:00:00Z",
  }] });
  const original = structuredClone(f.root.Values);
  original.schemaHook.enabled = true;
  const intent = JSON.parse(f.intentMap.data["intent.json"]);
  intent.budget = structuredClone(original.inferenceBudget);
  intent.valuesDigest = hash(canonicalSchema(original));
  f.intentMap.data["intent.json"] = JSON.stringify(intent);
  f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
  if (adopted) {
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
  }
  f.root.Values.inferenceBudget.catalogVersion = "review-v2";
  Object.assign(f.root.Values.inferenceBudget.contracts[0], { version: "bounds-v2", validUntil: "2999-01-01T00:00:00Z" });
  return f;
}
function dailyLimitFixture(adopted = true): any {
  const f = fixture();
  f.root.Values.sre.dailyTokens = 1500000;
  f.resources.push({ apiVersion: "kars.azure.com/v1alpha1", kind: "InferencePolicy",
    metadata: { name: "sre-inference", namespace: "kars-system" }, spec: {
      appliesTo: { sandboxName: "sre" }, tokenBudget: { dailyTokens: 1500000, perRequestTokens: 528000 },
      contentSafety: { requirePromptShields: true },
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
  f.root.Values.sre.dailyTokens = 5000000;
  f.resources[1].spec.tokenBudget.dailyTokens = 5000000;
  return f;
}
const originalQualifiedWitnessImage = "localhost:5077/kars-beta-fresh4-e865b5c5/kars-datapath-witness:latest@sha256:586f44a447f185dae637fe3aef23a860374caff96995be73aafeaac07a86e599";
const refreshedQualifiedWitnessImage = "localhost:5077/kars-beta-fleet-witness-20261025-r1/kars-datapath-witness:latest@sha256:bb6356930d955bfb1fce2b14262768ec151dfddfe8d97eab5e7fb6636e98fad2";
function witnessFixture(scope?: "selected" | "all", image = `registry.example/witness:latest@sha256:${"a".repeat(64)}`): any {
  const f = adoptedFixture();
  const witness = parse(readFileSync(`${root}deploy/helm/kars-datapath-witness/values.yaml`, "utf8"));
  Object.assign(witness, { enabled: true, managed: true, sandboxes: scope === "all" ? [] : ["sre"],
    nodes: ["approved-node"], global: {} });
  if (scope) witness.scope = scope;
  else delete witness.scope;
  witness.aggregator.image = image;
  witness.gadget.image = image;
  witness.gadget.hookMode = "podinformer";
  f.root.Values.datapathWitness = witness;
  const original = structuredClone(f.root.Values);
  delete original.controller.image;
  f.adoptionMap.data.valuesDigest = hash(canonicalSchema(original));
  f.root.Values.sre.datapathWitnessBaseline = structuredClone(witness);
  return f;
}

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

describe("adopted optional Grafana lifecycle", () => {
  function coalesceMonitoring(monitoring: any) {
    const dir = `${root}.sre-grafana-check-${randomUUID()}`;
    mkdirSync(`${dir}/templates`, { recursive: true });
    try {
      for (const file of ["Chart.yaml", "values.yaml"]) copyFileSync(`${root}deploy/helm/kars/${file}`, `${dir}/${file}`);
      cpSync(`${root}deploy/helm/kars/charts`, `${dir}/charts`, { recursive: true });
      writeFileSync(`${dir}/selected.json`, JSON.stringify({ monitoring }));
      writeFileSync(`${dir}/templates/values.yaml`, 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: computed-monitoring\ndata:\n  values.json: {{ .Values.monitoring | toJson | quote }}\n');
      const output = execFileSync("helm", ["template", "kars", dir, "-n", "kars-system", "-f", `${dir}/selected.json`],
        { encoding: "utf8", timeout: 10000, maxBuffer: 16 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
      const doc = parseAllDocuments(output).map(d => d.toJSON()).find(d => d?.metadata?.name === "computed-monitoring");
      return JSON.parse(doc.data["values.json"]);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }
  function grafanaFixture(enabled = false, monitoring?: any) {
    const f = adoptedFixture();
    f.root.Values.monitoring = { enabled: true, prometheus: { enabled: true }, grafana: {
      enabled, image: { registry: "registry.example:5443", repository: "qualified/grafana", tag: "latest", pullPolicy: "Always" },
      sidecar: { image: { registry: "registry.example:5443", repository: "qualified/sidecar", tag: "latest" } },
      "grafana.ini": { "auth.anonymous": { enabled: false } },
      resources: { limits: { memory: "512Mi" } }, serviceAccount: { create: true }, rbac: { namespaced: true },
    } };
    if (monitoring) f.root.Values.monitoring = monitoring;
    const original = structuredClone(f.root.Values);
    delete original.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(original));
    const grafana = f.root.Values.monitoring.grafana;
    f.root.Values.monitoring.grafanaBaseline = structuredClone(grafana);
    grafana.enabled = !enabled;
    for (const image of [grafana.image, grafana.sidecar.image]) {
      image.registry = "another.registry.example:5443";
      image.tag = `latest@sha256:${"b".repeat(64)}`;
    }
    return f;
  }
  it.each([false, true])("enables, refreshes, disables and re-enables Grafana against its original enabled=%s receipt", enabled => {
    const f = grafanaFixture(enabled);
    const receipt = structuredClone(f.adoptionMap.data);
    for (const selected of [true, false, true]) {
      f.root.Values.monitoring.grafana.enabled = selected;
      f.root.Values.monitoring.grafana.image.tag = `latest@sha256:${(selected ? "c" : "d").repeat(64)}`;
      const before = structuredClone(f);
      const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt);
      expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
      expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
      expect(f).toEqual(before);
    }
  });
  it.each([false, true])("preserves the receipt across actual dependency coalescing from enabled=%s", enabled => {
    const f = grafanaFixture(enabled, coalesceMonitoring({ grafana: { enabled } }));
    for (const selected of [true, false, true]) {
      f.root.Values.monitoring.grafana.enabled = selected;
      f.root.Values.monitoring = coalesceMonitoring(f.root.Values.monitoring);
      const docs = parseAllDocuments(render(f)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    }
    f.root.Values.monitoring.grafana["grafana.ini"]["auth.anonymous"] = { enabled: true };
    expect(() => render(f)).toThrow("preserve authentication");
  });
  it("tightens a legacy cluster-wide Grafana baseline without permitting re-expansion", () => {
    const f = grafanaFixture(false, coalesceMonitoring({ grafana: { enabled: false, rbac: { namespaced: false } } }));
    f.root.Values.monitoring.grafana.rbac.namespaced = true;
    expect(() => render(f)).not.toThrow();
    f.root.Values.monitoring.grafana.rbac.namespaced = false;
    expect(() => render(f)).toThrow("namespace-scoped RBAC");
    f.root.Values.monitoring.grafana.rbac.namespaced = true;
    f.root.Values.monitoring.grafana.rbac.namespaces = ["another-tenant"];
    expect(() => render(f)).toThrow("preserve authentication");
  });
  it("leaves existing sealed configurations valid without adding a default baseline", () => {
    const f = grafanaFixture();
    const original = f.root.Values.monitoring.grafanaBaseline;
    f.root.Values.monitoring.grafana.enabled = original.enabled;
    Object.assign(f.root.Values.monitoring.grafana.image, original.image);
    Object.assign(f.root.Values.monitoring.grafana.sidecar.image, original.sidecar.image);
    delete f.root.Values.monitoring.grafanaBaseline;
    expect(() => render(f)).not.toThrow();
  });
  it.each(["adoption", "missing-baseline", "forged-baseline", "last-selected-baseline", "baseline-type", "baseline-extra", "switch-type",
    "missing-image-field", "image-extra", "image-type", "mutable-grafana", "mutable-sidecar", "bad-digest", "auth", "rbac", "resources",
    "sidecar-rbac", "prometheus", "funding", "source", "controller", "receipt", "hook"])("rejects changed %s", fault => {
    const f = grafanaFixture();
    const baseline = f.root.Values.monitoring.grafanaBaseline;
    const grafana = f.root.Values.monitoring.grafana;
    if (fault === "adoption") f.adoptionMap = {};
    if (fault === "missing-baseline") delete f.root.Values.monitoring.grafanaBaseline;
    if (fault === "forged-baseline") baseline.enabled = true;
    if (fault === "last-selected-baseline") baseline.image.tag = grafana.image.tag;
    if (fault === "baseline-type") f.root.Values.monitoring.grafanaBaseline = [];
    if (fault === "baseline-extra") baseline.authentication = false;
    if (fault === "switch-type") grafana.enabled = "true";
    if (fault === "missing-image-field") { delete baseline.image.registry; baseline.image.extra = "ignored"; }
    if (fault === "image-extra") baseline.sidecar.image.extra = "ignored";
    if (fault === "image-type") grafana.image.repository = {};
    if (fault === "mutable-grafana") grafana.image.tag = "latest";
    if (fault === "mutable-sidecar") grafana.sidecar.image.tag = "latest";
    if (fault === "bad-digest") grafana.sidecar.image.tag += "bad";
    if (fault === "auth") grafana["grafana.ini"]["auth.anonymous"].enabled = true;
    if (fault === "rbac") grafana.serviceAccount.create = false;
    if (fault === "resources") grafana.resources.limits.memory = "8Gi";
    if (fault === "sidecar-rbac") grafana.sidecar.enableUniqueFilenames = true;
    if (fault === "prometheus") f.root.Values.monitoring.prometheus.enabled = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "foreign";
    if (fault === "controller") f.controller.metadata.uid = "foreign";
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
});

describe("adopted Headlamp telemetry transitions", () => {
  const readers = [{ kind: "User", name: "mapped:operator@example.test" }, { kind: "Group", name: "mapped:auditors" }];
  const originalImage = "registry.example/headlamp";
  function telemetryFixture(originalSubjects: unknown[] = [], originalTag = "latest", adopted = true): any {
    const f = fixture();
    f.root.Values.headlamp = { enabled: true,
      image: { repository: originalImage, tag: originalTag, pullPolicy: "Always" }, imagePullSecrets: [],
      oidc: { enabled: true, issuerURL: "https://identity.example", clientID: "headlamp",
        clientSecret: { name: "existing-oidc", key: "client-secret" } } };
    f.root.Values.monitoring = { enabled: true, namespaceOverride: "monitoring", fullnameOverride: "kps-kube-prometheus-stack",
      prometheus: { enabled: true }, querySubjects: structuredClone(originalSubjects) };
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    if (adopted) {
      const baseline = structuredClone(f.root.Values);
      delete baseline.controller.image;
      f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
      f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    }
    f.root.Values.headlamp.imageBaseline = `${originalImage}:${originalTag}`;
    f.root.Values.headlamp.image.repository = "another.registry.example:5443/qualified/headlamp";
    f.root.Values.headlamp.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.monitoring.querySubjectsBaseline = structuredClone(originalSubjects);
    f.root.Values.monitoring.querySubjects = structuredClone(readers);
    return f;
  }
  function coldTelemetryFixture(): any {
    const f = telemetryFixture([], "latest", false);
    f.root.Values.evals = { runnerImageBaseline: "registry.example/runner:latest",
      runnerImage: `registry.example/runner:latest@sha256:${"b".repeat(64)}` };
    const baseline = { enabled: false, image: { registry: "registry.example", repository: "grafana", tag: "latest" },
      sidecar: { image: { registry: "registry.example", repository: "sidecar", tag: "latest" } }, rbac: { namespaced: true } };
    f.root.Values.monitoring.grafanaBaseline = baseline;
    f.root.Values.monitoring.grafana = structuredClone(baseline);
    f.root.Values.monitoring.grafana.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.monitoring.grafana.sidecar.image.tag = `latest@sha256:${"d".repeat(64)}`;
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    return f;
  }
  it("adopts originally sealed runner, Headlamp and monitoring baselines before a controller update", () => {
    const f = coldTelemetryFixture();
    const originalValues = structuredClone(f.root.Values);
    const receipt = parseAllDocuments(render(f))[0].toJSON().data;
    expect(f.root.Values).toEqual(originalValues);
    f.adoptionMap = { ...adoptedFixture().adoptionMap, data: receipt };
    f.root.Values.controller.image.tag = `latest@sha256:${"e".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"e".repeat(64)}`;
    expect(() => render(f)).not.toThrow();
  });
  it.each(["headlamp", "querySubjects", "grafana", "controller", "runner"])(
    "rejects %s changes before adopting the exact cold installation", field => {
      const f = coldTelemetryFixture();
      if (field === "headlamp") f.root.Values.headlamp.image.tag = `latest@sha256:${"e".repeat(64)}`;
      if (field === "querySubjects") f.root.Values.monitoring.querySubjects = [];
      if (field === "grafana") f.root.Values.monitoring.grafana.enabled = true;
      if (field === "controller") f.root.Values.controller.image.digest = `sha256:${"e".repeat(64)}`;
      if (field === "runner") f.root.Values.evals.runnerImage = `registry.example/runner:latest@sha256:${"e".repeat(64)}`;
      expect(() => render(f)).toThrow();
    });
  it.each([false, true])("preserves unchanged sealed values without new baseline defaults (adopted: %s)", adopted => {
    const f = telemetryFixture([], "latest", adopted);
    delete f.root.Values.headlamp.imageBaseline;
    Object.assign(f.root.Values.headlamp.image, { repository: originalImage, tag: "latest" });
    delete f.root.Values.monitoring.querySubjectsBaseline;
    f.root.Values.monitoring.querySubjects = [];
    expect(() => render(f)).not.toThrow();
  });
  it.each(["latest", `latest@sha256:${"a".repeat(64)}`])(
    "refreshes original %s images while adding, changing and revoking initially empty or nonempty readers", originalTag => {
      for (const originalSubjects of [[], [{ kind: "Group", name: "original:readers" }]]) {
        const f = telemetryFixture(originalSubjects, originalTag);
        const receipt = structuredClone(f.adoptionMap);
        for (const selectedSubjects of [readers, [{ kind: "Group", name: "replacement:readers" }], []]) {
          f.root.Values.monitoring.querySubjects = structuredClone(selectedSubjects);
          const before = structuredClone(f);
          const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
          expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt.data);
          expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
          expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
          expect(f).toEqual(before);
        }
      }
    });
  it.each(["image", "subjects"])("supports the independent %s transition", transition => {
    const f = telemetryFixture();
    if (transition === "image") {
      delete f.root.Values.monitoring.querySubjectsBaseline;
      f.root.Values.monitoring.querySubjects = [];
    } else {
      delete f.root.Values.headlamp.imageBaseline;
      Object.assign(f.root.Values.headlamp.image, { repository: originalImage, tag: "latest" });
    }
    expect(() => render(f)).not.toThrow();
    f.adoptionMap = {};
    expect(() => render(f)).toThrow("completed SRE adoption");
  });
  it("retains the original baselines across image upgrades and composes with router and controller refreshes", () => {
    const f = telemetryFixture();
    const receipt = structuredClone(f.adoptionMap.data);
    f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
    f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    f.root.Values.controller.image.tag = `latest@sha256:${"d".repeat(64)}`;
    for (const digest of ["b", "e"]) {
      f.root.Values.headlamp.image.tag = `latest@sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt);
      expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(f.root.Values);
    }
  });
  it.each(["image-missing", "image-forged", "image-last-selected", "subjects-missing", "subjects-forged", "subjects-last-selected"])(
    "rejects unbound or replaced originals: %s", fault => {
      const f = telemetryFixture([{ kind: "Group", name: "original:readers" }]);
      if (fault === "image-missing") delete f.root.Values.headlamp.imageBaseline;
      if (fault === "image-forged") f.root.Values.headlamp.imageBaseline = "foreign.example/headlamp:latest";
      if (fault === "image-last-selected") f.root.Values.headlamp.imageBaseline = `${f.root.Values.headlamp.image.repository}:${f.root.Values.headlamp.image.tag}`;
      if (fault === "subjects-missing") delete f.root.Values.monitoring.querySubjectsBaseline;
      if (fault === "subjects-forged") f.root.Values.monitoring.querySubjectsBaseline = [];
      if (fault === "subjects-last-selected") f.root.Values.monitoring.querySubjectsBaseline = structuredClone(readers);
      expect(() => render(f)).toThrow("original adoption receipt and unchanged values");
    });
  it.each(["baseline-type", "baseline-empty", "baseline-tag", "baseline-digest", "image-type", "repository-type", "tag-type", "mutable-selected", "selected-digest", "selected-url", "selected-tag-prefix"])(
    "rejects invalid image references: %s", fault => {
      const f = telemetryFixture();
      if (fault === "baseline-type") f.root.Values.headlamp.imageBaseline = {};
      if (fault === "baseline-empty") f.root.Values.headlamp.imageBaseline = "";
      if (fault === "baseline-tag") f.root.Values.headlamp.imageBaseline = `${originalImage}:v1`;
      if (fault === "baseline-digest") f.root.Values.headlamp.imageBaseline = `${originalImage}:latest@sha256:bad`;
      if (fault === "image-type") f.root.Values.headlamp.image = "invalid";
      if (fault === "repository-type") f.root.Values.headlamp.image.repository = {};
      if (fault === "tag-type") f.root.Values.headlamp.image.tag = {};
      if (fault === "mutable-selected") f.root.Values.headlamp.image.tag = "latest";
      if (fault === "selected-digest") f.root.Values.headlamp.image.tag += "bad";
      if (fault === "selected-tag-prefix") f.root.Values.headlamp.image.tag = `wrong:${f.root.Values.headlamp.image.tag}`;
      if (fault === "selected-url") f.root.Values.headlamp.image.repository = "https://registry.example/headlamp?unreviewed=true";
      expect(() => render(f)).toThrow("Headlamp image");
    });
  const invalidSubjects = [null, {}, "readers", ["reader"], [{}], [{ kind: "Group" }], [{ name: "reader" }],
    [{ kind: "ServiceAccount", name: "headlamp" }], [{ kind: "User", name: "reader", namespace: "headlamp" }],
    [{ kind: "User", name: "reader", apiGroup: "rbac.authorization.k8s.io" }], [{ kind: "Group", name: "" }],
    [{ kind: "Group", name: " reader" }], [{ kind: "Group", name: "reader " }], [{ kind: "User", name: "reader\nother" }],
    [{ kind: "Group", name: 3 }], [{ kind: false, name: "reader" }], [readers[0], readers[0]]];
  it.each(["querySubjectsBaseline", "querySubjects"])("validates every original and selected subject in %s", field => {
    for (const subjects of invalidSubjects) {
      const f = telemetryFixture();
      f.root.Values.monitoring[field] = subjects;
      expect(() => render(f), JSON.stringify(subjects)).toThrow(`monitoring.${field}`);
    }
  });
  it.each(["oidc", "auth", "service-account", "credential", "pull-secret", "pull-policy", "headlamp-enabled", "monitoring-enabled",
    "monitoring-namespace", "monitoring-service", "prometheus", "sre", "funding", "budget", "source", "registration", "controller-uid", "receipt", "mutable-receipt"])(
    "does not relax unrelated authority or configuration: %s", fault => {
      const f = telemetryFixture();
      if (fault === "oidc") f.root.Values.headlamp.oidc.issuerURL = "https://unreviewed.example";
      if (fault === "auth") f.root.Values.headlamp.oidc.enabled = false;
      if (fault === "service-account") f.root.Values.headlamp.serviceAccountName = "unreviewed";
      if (fault === "credential") f.root.Values.headlamp.oidc.clientSecret.name = "different-credential";
      if (fault === "pull-secret") f.root.Values.headlamp.imagePullSecrets = [{ name: "different-registry-credential" }];
      if (fault === "pull-policy") f.root.Values.headlamp.image.pullPolicy = "Never";
      if (fault === "headlamp-enabled") f.root.Values.headlamp.enabled = false;
      if (fault === "monitoring-enabled") f.root.Values.monitoring.enabled = false;
      if (fault === "monitoring-namespace") f.root.Values.monitoring.namespaceOverride = "other";
      if (fault === "monitoring-service") f.root.Values.monitoring.fullnameOverride = "other";
      if (fault === "prometheus") f.root.Values.monitoring.prometheus.enabled = false;
      if (fault === "sre") f.root.Values.sre.enabled = false;
      if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
      if (fault === "budget") f.root.Values.inferenceBudget.enabled = false;
      if (fault === "source") f.source.spec.unreviewed = true;
      if (fault === "registration") f.registration.spec.controller.release = "other";
      if (fault === "controller-uid") f.controller.metadata.uid = "other";
      if (fault === "receipt") f.adoptionMap.data.intentUID = "other";
      if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
      expect(() => render(f)).toThrow();
    });
});

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

const topUp = {
  id: "operator-capacity-1", configUid: "catalog-uid", accountUid: "account-uid",
  root: { kind: "KarsSandbox", resource: { name: "sre", namespace: "kars-system", uid: "source-uid" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" },
  originalLimits: { tokens: 1000 }, tokens: 5000, expiresAt: "2999-01-01T00:00:00Z",
  nodes: { "source-uid": `sha256:${"a".repeat(64)}` },
};
function renderFunding(tokenTopUps: unknown[], enabled = true, unboundedTokenAllowances: unknown[] = []): any[] {
  const output = execFileSync("helm", ["template", "kars", `${root}deploy/helm/kars`, "-n", "kars-system", "-f", "-"], {
    encoding: "utf8", timeout: 30000, stdio: ["pipe", "pipe", "pipe"],
    input: JSON.stringify({ inferenceBudget: { enabled, tokenTopUps, unboundedTokenAllowances, catalogVersion: "test-v1", contracts: [{ id: "test" }],
      caBundle: "public-test-ca", tlsSecretName: "budget-tls", routerImageDigest: `sha256:${"a".repeat(64)}` } }),
  });
  return parseAllDocuments(output).map(document => document.toJSON()).filter(Boolean);
}
describe("adopted Core installed-image continuity", () => {
  const allowance = { id: "operator-unbounded-1", configUid: topUp.configUid, accountUid: topUp.accountUid,
    root: topUp.root, originalLimits: topUp.originalLimits, ledgerDigest: `sha256:${"a".repeat(64)}`, previousTokenTopUpIds: [topUp.id] };
  function retainedRouterFixture() {
    const f = adoptedFixture();
    f.root.Values.evals = { runnerImage: "ghcr.io/azure/kars-conformance-runner:latest" };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.evals.runnerImageBaseline = f.root.Values.evals.runnerImage;
    f.root.Values.evals.runnerImage = `localhost:5077/new-runner:latest@sha256:${"c".repeat(64)}`;
    f.root.Values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"a".repeat(64)}`;
    f.root.Values.inferenceRouter.image = { repository: "localhost:5077/installed-router", tag: `latest@sha256:${"b".repeat(64)}` };
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [];
    f.root.Values.controller.image = { repository: "localhost:5077/installed-controller", tag: `latest@sha256:${"d".repeat(64)}`, digest: `sha256:${"d".repeat(64)}` };
    f.controller.metadata = { ...f.controller.metadata, name: "kars-controller", namespace: "kars-system" };
    f.controller.spec = { template: { spec: { containers: [{ name: "controller",
      image: `${f.root.Values.controller.image.repository}:${f.root.Values.controller.image.tag}`, env: [
        { name: "INFERENCE_ROUTER_IMAGE", value: `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}` },
        { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: f.root.Values.inferenceBudget.routerImageDigest },
        { name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
      ] }] } } };
    return f;
  }
  it.each([false, true])("retains installed images, adoption and selected receipts during runner refresh (allowance: %s)", funded => {
    const f = retainedRouterFixture();
    if (funded) {
      f.root.Values.inferenceBudget.tokenTopUps = [topUp];
      f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    }
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    expect(f).toEqual(before);
    const emitted = renderFunding(funded ? [topUp] : [], true, funded ? [allowance] : [])
      .find(d => d.kind === "ConfigMap" && d.metadata.name === "kars-inference-budget-contracts");
    expect(JSON.parse(emitted.data["unbounded-token-allowances.json"])).toEqual(funded ? [allowance] : []);
    expect(JSON.parse(emitted.data["token-top-ups.json"])).toEqual(funded ? [topUp] : []);
  });
  it.each(["missing-controller", "controller-uid", "workspace-uid", "controller-name", "controller-namespace", "retiring",
    "duplicate-container", "missing-image", "changed-image", "duplicate-image", "secret-image", "changed-verifier", "missing-original-verifier",
    "mutable-selected", "mismatched-selected-digest", "forged-baseline-repository", "forged-baseline-digest", "receipt", "mutable-receipt",
    "funding", "credentials", "pull-policy", "source", "malformed-allowances", "unrelated-budget"])('rejects retained router continuity with %s', fault => {
    const f = retainedRouterFixture();
    const containers = f.controller.spec.template.spec.containers;
    const env = containers[0].env;
    if (fault === "missing-controller") delete f.controller.spec;
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "controller-name") f.controller.metadata.name = "other";
    if (fault === "controller-namespace") f.controller.metadata.namespace = "other";
    if (fault === "retiring") f.controller.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "duplicate-container") containers.push(structuredClone(containers[0]));
    if (fault === "missing-image") env.splice(0, 1);
    if (fault === "changed-image") env[0].value = `foreign.example/router:latest@sha256:${"b".repeat(64)}`;
    if (fault === "duplicate-image") env.push(structuredClone(env[0]));
    if (fault === "secret-image") env[0] = { name: env[0].name, valueFrom: { secretKeyRef: { name: "image", key: "value" } } };
    if (fault === "changed-verifier") env[1].value = `sha256:${"c".repeat(64)}`;
    if (fault === "missing-original-verifier") env.pop();
    if (fault === "mutable-selected") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "mismatched-selected-digest") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "forged-baseline-repository") f.root.Values.sre.routerImageBaseline = `foreign.example/router:latest@sha256:${"a".repeat(64)}`;
    if (fault === "forged-baseline-digest") f.root.Values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"f".repeat(64)}`;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "pull-policy") f.root.Values.inferenceRouter.image.pullPolicy = "Never";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "malformed-allowances") f.root.Values.inferenceBudget.unboundedTokenAllowances = { enabled: true };
    if (fault === "unrelated-budget") f.root.Values.inferenceBudget.disableAccounting = true;
    expect(() => render(f)).toThrow();
  });
  function retainedRuntimeFixture(changed: "openclaw" | "hermes" | "both" = "both") {
    const f = runtimeFixture();
    if (changed !== "hermes") f.root.Values.sandbox.image.repository = "localhost:5077/installed-openclaw";
    if (changed !== "openclaw") f.root.Values.runtimes.hermes.image = `localhost:5077/installed-hermes:latest@sha256:${"c".repeat(64)}`;
    f.controller.metadata = { ...f.controller.metadata, name: "kars-controller", namespace: "kars-system" };
    f.controller.spec = { template: { spec: { containers: [{ name: "controller", env: [
      { name: "SANDBOX_IMAGE", value: `${f.root.Values.sandbox.image.repository}:${f.root.Values.sandbox.image.tag}` },
      { name: "HERMES_RUNTIME_IMAGE", value: f.root.Values.runtimes.hermes.image },
    ] }] } } };
    return f;
  }
  it.each(["openclaw", "hermes", "both"] as const)("retains the exact installed %s repository migration without changing selected values", changed => {
    const f = retainedRuntimeFixture(changed);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    expect(f).toEqual(before);
  });
  it.each(["missing-controller", "controller-uid", "workspace-uid", "controller-name", "controller-namespace", "retiring",
    "duplicate-container", "missing-openclaw", "missing-hermes", "different-openclaw", "different-hermes",
    "duplicate-openclaw", "duplicate-hermes", "indirect-openclaw", "indirect-hermes", "extra-image-field",
    "selected-openclaw", "selected-hermes", "mutable-openclaw", "mutable-hermes", "baseline-openclaw", "baseline-hermes",
    "baseline-digest", "receipt", "mutable-receipt", "funding", "runtime-env", "pull-policy", "source", "credentials"])("rejects retained runtime continuity with %s", fault => {
    const f = retainedRuntimeFixture();
    const containers = f.controller.spec.template.spec.containers;
    const env = containers[0].env;
    if (fault === "missing-controller") delete f.controller.spec;
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "controller-name") f.controller.metadata.name = "other";
    if (fault === "controller-namespace") f.controller.metadata.namespace = "other";
    if (fault === "retiring") f.controller.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "duplicate-container") containers.push(structuredClone(containers[0]));
    if (fault === "missing-openclaw") env.splice(0, 1);
    if (fault === "missing-hermes") env.pop();
    if (fault === "different-openclaw") env[0].value = `foreign.example/openclaw:latest@sha256:${"b".repeat(64)}`;
    if (fault === "different-hermes") env[1].value = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "duplicate-openclaw") env.push(structuredClone(env[0]));
    if (fault === "duplicate-hermes") env.push(structuredClone(env[1]));
    for (const [name, index] of [["indirect-openclaw", 0], ["indirect-hermes", 1]] as const) {
      if (fault === name) env[index] = { name: env[index].name, valueFrom: { secretKeyRef: { name: "image", key: "value" } } };
    }
    if (fault === "extra-image-field") env[0].valueFrom = { fieldRef: { fieldPath: "metadata.name" } };
    if (fault === "selected-openclaw") f.root.Values.sandbox.image.repository = "foreign.example/openclaw";
    if (fault === "selected-hermes") f.root.Values.runtimes.hermes.image = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "mutable-openclaw") f.root.Values.sandbox.image.tag = "latest";
    if (fault === "mutable-hermes") f.root.Values.runtimes.hermes.image = "localhost:5077/installed-hermes:latest";
    if (fault === "baseline-openclaw") f.root.Values.sre.runtimeImageBaseline.openclaw = `foreign.example/openclaw:latest@sha256:${"a".repeat(64)}`;
    if (fault === "baseline-hermes") f.root.Values.sre.runtimeImageBaseline.hermes = `foreign.example/hermes:latest@sha256:${"a".repeat(64)}`;
    if (fault === "baseline-digest") f.root.Values.sre.runtimeImageBaseline.hermes = `registry.example/hermes:latest@sha256:${"f".repeat(64)}`;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "runtime-env") f.root.Values.runtimes.hermes.extraEnv = { DISABLE_GOVERNANCE: "true" };
    if (fault === "pull-policy") f.root.Values.sandbox.image.pullPolicy = "Never";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it("accepts only the empty new allowance default during legacy initial adoption", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [];
    expect(render(f)).toContain("intentUID");
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    expect(() => render(f)).toThrow(/Unbounded token allowances require completed SRE adoption/);
  });
  it.each([{ sealed: [] }, { sealed: [allowance] }])("restores the exact originally sealed allowance list only for comparison (%j)", ({ sealed }) => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.unboundedTokenAllowances = sealed;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.budget.unboundedTokenAllowances = sealed;
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]).inferenceBudget.unboundedTokenAllowances).toEqual([allowance]);
  });
});
describe("receipt-bound private Core continuity", () => {
  const retirementKey = "kars.azure.com/private-root-retirement";
  const publicationKey = "kars.azure.com/private-core-publication";
  function privateFixture(qualified = false, replicas = 1, managedTls = false): any {
    const f = fixture();
    f.root.Values.controller.replicas = replicas;
    f.root.Values.evals = { enabled: true, runnerImage: "ghcr.io/azure/kars-conformance-runner:latest" };
    Object.assign(f.root.Values.inferenceBudget, { tlsSecretName: "budget-tls", caBundle: "original-public-ca" });
    if (managedTls) f.root.Values.inferenceBudget.tls = { certificate: "test-certificate", privateKey: "test-key" };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.budget = structuredClone(f.root.Values.inferenceBudget);
    delete intent.budget.tls;
    intent.valuesDigest = hash(canonicalSchema({ ...f.root.Values, schemaHook: { enabled: true } }));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    const identity = (name: string, uid: string, resourceVersion = "10") => ({ name, uid, resourceVersion });
    const ns = identity("kars-system", "workspace-uid");
    f.controller = { metadata: { ...identity("kars-controller", "controller-uid"), namespace: ns.name }, spec: {
      replicas: qualified ? replicas : 0,
      template: { metadata: { annotations: { "kars.azure.com/private-epoch": "current-epoch", "test/encoding": "<bound>&\u2028" } },
        spec: { serviceAccountName: "kars-controller", containers: [{ name: "controller", image: "registry.example/controller:latest@sha256:" + "a".repeat(64) }] } },
    } };
    const definition = JSON.parse(readFileSync(`${root}deploy/helm/kars/files/private-consumption.json`, "utf8"));
    f.privateDefinition = { controllers: definition.controllers, secrets: definition.secrets, tokenAudiences: definition.tokenAudiences };
    f.privateControllers = definition.controllers;
    const activation: any = { contract: "kars.azure.com/private-consumption/v1", phase: qualified ? "qualified" : "reviewed",
      bundleRevision: "b".repeat(64), profile: "service-accounts", controllerUids: Object.fromEntries(f.privateControllers.map((name: string) => [name, `uid-${name}`])),
      root: { namespace: ns, deployment: identity("kars-controller", "controller-uid"), account: identity("kars-controller", "root-account-uid"),
        replicaIntent: replicas, templateDigest: templateDigest(f.controller),
        budgetTls: { namespace: ns, secret: identity("budget-tls", "tls-uid", qualified ? "12" : "10"), keyDigest: (qualified ? "c" : "a").repeat(64) } },
      namespaces: [{ namespace: ns, ...(qualified ? { epoch: "current-epoch" } : {}), consumers: [{ kind: "Deployment", object: identity("kars-controller", "controller-uid"), templateDigest: templateDigest(f.controller) }] }],
    };
    const state = { version: 1, attempt: "d".repeat(64), binding: retirementBinding(activation), replicaIntent: replicas,
      originalVersion: "10", pauseRoot: true, phase: qualified ? "restoring" : "retired", captured: { [ns.uid]: ["retired-pod-uid"] },
      exposedKeys: ["a".repeat(64)], baseline: { secretUid: "tls-uid", resourceVersion: "11", keyDigest: "a".repeat(64) } };
    const proof = qualified ? { version: 2, activation, retirement: canonical(state), retirementDigest: hash(canonical(state)) } : state;
    f.workspace = { metadata: { ...ns, annotations: { ...annotations(activation, activation.namespaces[0], qualified ? "Qualified" : "Pending"),
      [retirementKey]: canonical(proof), ...(!qualified ? { [publicationKey]: canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" }) } : { "kars.azure.com/private-epoch": "current-epoch" }) } } };
    const owner = { annotations: { ...f.intentMap.metadata.annotations }, labels: { "app.kubernetes.io/managed-by": "Helm" } };
    const ca = "-----BEGIN CERTIFICATE-----\nrotated-public-ca\n-----END CERTIFICATE-----\n";
    f.catalog = { metadata: { ...identity("kars-inference-budget-contracts", "catalog-uid", "12"), namespace: ns.name,
      ...owner, annotations: { ...owner.annotations, "kars.azure.com/inference-budget-contracts": "v1" } }, data: { "ca.crt": ca } };
    f.budgetTls = { metadata: { ...identity("budget-tls", "tls-uid", "12"), namespace: ns.name, ...owner }, type: "kubernetes.io/tls" };
    f.root.Values.controller.replicas = qualified ? replicas : 0;
    f.root.Values.inferenceBudget.caBundle = ca;
    f.root.Values.evals.runnerImageBaseline = f.root.Values.evals.runnerImage;
    f.root.Values.evals.runnerImage = `localhost:5077/runner:latest@sha256:${"d".repeat(64)}`;
    return f;
  }
  function alterProof(f: any, change: (state: any, proof: any) => void) {
    const proof = JSON.parse(f.workspace.metadata.annotations[retirementKey]);
    const state = proof.version === 2 ? JSON.parse(proof.retirement) : proof;
    change(state, proof);
    if (proof.version === 2) { proof.retirement = canonical(state); proof.retirementDigest = hash(proof.retirement); }
    f.workspace.metadata.annotations[retirementKey] = canonical(proof);
  }
  function multiNamespaceFixture(): any {
    const f = privateFixture(true);
    const live = { apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "sre", namespace: "kars-sre", uid: "sre-deployment-uid", resourceVersion: "14" },
      spec: { replicas: 1, template: { metadata: { annotations: { "kars.azure.com/private-epoch": "sre-epoch" } },
        spec: { serviceAccountName: "sre-api-router", containers: [{ name: "sre", image: "registry.example/sre:latest@sha256:" + "a".repeat(64) }] } } } };
    f.privateConsumers = { "kars-sre/Deployment/sre": live };
    alterProof(f, (state, proof) => {
      const scope = { namespace: { name: "kars-sre", uid: "sre-namespace-uid", resourceVersion: "13" }, epoch: "sre-epoch",
        consumers: [{ kind: "Deployment", object: { name: live.metadata.name, uid: live.metadata.uid, resourceVersion: live.metadata.resourceVersion }, templateDigest: templateDigest(live) }] };
      proof.activation.namespaces.unshift(scope);
      state.captured[scope.namespace.uid] = ["retired-sre-pod"];
      state.binding = retirementBinding(proof.activation);
      f.privateNamespaces = { "kars-sre": { metadata: { ...scope.namespace,
        annotations: { ...annotations(proof.activation, scope, "Qualified"), "kars.azure.com/private-epoch": scope.epoch } } } };
    });
    return f;
  }
  it("preserves qualified multi-namespace retirement with SRE before the root in receipt order", () => {
    const f = multiNamespaceFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(f).toEqual(before);
  });
  it.each([
    ["missing namespace", (f: any) => { delete f.privateNamespaces["kars-sre"]; }],
    ["replaced namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.uid = "replacement"; }],
    ["deleting namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.deletionTimestamp = "2026-10-29T00:00:00Z"; }],
    ["namespace epoch", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-epoch"] = "wrong"; }],
    ["namespace authority", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-root-uid"] = "wrong"; }],
    ["namespace profile", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations[`kars.azure.com/private-${f.privateControllers[0]}-uid`] = "wrong"; }],
    ["missing SRE consumer", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["unscoped SRE consumer", (f: any) => { f.privateConsumers["Deployment/sre"] = f.privateConsumers["kars-sre/Deployment/sre"]; delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["consumer namespace", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.namespace = "kars-system"; }],
    ["consumer UID", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.uid = "replacement"; }],
    ["consumer epoch", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"].spec.template.metadata.annotations["kars.azure.com/private-epoch"]; }],
    ["consumer template", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.template.spec.containers[0].image = "changed"; }],
    ["omitted namespace", (f: any) => alterProof(f, (_state, proof) => { proof.activation.namespaces.shift(); })],
    ["duplicate namespace", (f: any) => alterProof(f, (_state, proof) => { proof.activation.namespaces.push(structuredClone(proof.activation.namespaces[0])); })],
    ["unknown captured scope", (f: any) => alterProof(f, state => { state.captured["foreign-uid"] = []; })],
    ["changed binding", (f: any) => alterProof(f, state => { state.binding = "f".repeat(64); })],
  ] as const)("rejects multi-namespace continuity with %s", (_name, change) => {
    const f = multiNamespaceFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function pendingMultiNamespaceFixture(): any {
    const f = multiNamespaceFixture();
    const fields = f.workspace.metadata.annotations;
    const proof = JSON.parse(fields[retirementKey]), state = JSON.parse(proof.retirement);
    state.phase = "retired";
    fields[retirementKey] = canonical(state);
    fields[publicationKey] = canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" });
    fields["kars.azure.com/private-state"] = "Pending";
    const scope = f.privateNamespaces["kars-sre"].metadata.annotations;
    scope["kars.azure.com/private-state"] = "Pending";
    scope["kars.azure.com/private-parent-sre-deployment-uid"] = "sre-epoch";
    f.controller.spec.replicas = f.root.Values.controller.replicas = f.privateConsumers["kars-sre/Deployment/sre"].spec.replicas = 0;
    f.registration.status.phase = "Provisioning";
    delete f.registration.status.privacyRevision;
    return f;
  }
  it("rejects Provisioning SRE outside an owned retired Pending publication", () => {
    const f = multiNamespaceFixture();
    f.registration.status.phase = "Provisioning";
    delete f.registration.status.privacyRevision;
    expect(() => render(f)).toThrow(/live original identities/);
  });
  it("preserves owned Pending multi-namespace publication with retired SRE consumers", () => {
    const f = pendingMultiNamespaceFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]).controller.replicas).toBe(0);
    expect(f).toEqual(before);
  });
  it.each([
    ["unexpected registration phase", (f: any) => { f.registration.status.phase = "Failed"; }],
    ["unexpected privacy revision", (f: any) => { f.registration.status.privacyRevision = "foreign"; }],
    ["stale registration generation", (f: any) => { f.registration.status.observedGeneration = 0; }],
    ["disabled registration", (f: any) => { f.registration.spec.enabled = false; }],
    ["changed registration identity", (f: any) => { f.registration.spec.controller.deployment.uid = "replacement"; }],
    ["missing namespace", (f: any) => { delete f.privateNamespaces["kars-sre"]; }],
    ["replaced namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.uid = "replacement"; }],
    ["deleting namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.deletionTimestamp = "2026-11-04T00:00:00Z"; }],
    ["namespace name", (f: any) => { f.privateNamespaces["kars-sre"].metadata.name = "foreign"; }],
    ["namespace authority", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-root-uid"] = "wrong"; }],
    ["namespace state", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-state"] = "Qualified"; }],
    ["namespace profile", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations[`kars.azure.com/private-${f.privateControllers[0]}-uid`] = "wrong"; }],
    ["parent enrollment", (f: any) => { delete f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-parent-sre-deployment-uid"]; }],
    ["missing consumer", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["unscoped consumer", (f: any) => { f.privateConsumers["Deployment/sre"] = f.privateConsumers["kars-sre/Deployment/sre"]; delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["running consumer", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.replicas = 1; }],
    ["consumer namespace", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.namespace = "kars-system"; }],
    ["consumer UID", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.uid = "replacement"; }],
    ["consumer template", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.template.spec.containers[0].image = "changed"; }],
    ["unsupported consumer", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].kind = "StatefulSet"; }],
    ["unknown capture", (f: any) => alterProof(f, state => { state.captured["foreign-uid"] = []; })],
    ["changed binding", (f: any) => alterProof(f, state => { state.binding = "f".repeat(64); })],
    ["extra enrolled consumer", (f: any) => {
      const extra = structuredClone(f.privateConsumers["kars-sre/Deployment/sre"]);
      extra.metadata.name = "extra"; extra.metadata.uid = "extra-uid";
      f.privateConsumers["kars-sre/Deployment/extra"] = extra;
      f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-parent-extra-uid"] = "sre-epoch";
    }],
  ] as const)("rejects Pending multi-namespace publication with %s", (_name, change) => {
    const f = pendingMultiNamespaceFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function pausingFixture(): any {
    const f = privateFixture();
    f.root.Values.inferenceBudget.caBundle = "original-public-ca";
    f.root.Values.controller.upgradePrecondition = { uid: f.controller.metadata.uid, resourceVersion: f.controller.metadata.resourceVersion };
    delete f.workspace.metadata.annotations[publicationKey];
    delete f.controller.spec.template.metadata.annotations["kars.azure.com/private-epoch"];
    alterProof(f, state => { state.phase = "pausing"; delete state.baseline; state.captured["sre-namespace-uid"] = ["sre-pod-uid"]; });
    return f;
  }
  it("publishes unchanged trust while preserving an initially paused root and multi-namespace retirement", () => {
    const f = pausingFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.controller.replicas).toBe(0);
    expect(values.inferenceBudget.caBundle).toBe("original-public-ca");
    expect(f).toEqual(before);
  });
  it.each([
    ["missing fence", (f: any) => { delete f.root.Values.controller.upgradePrecondition; }],
    ["stale fence", (f: any) => { f.root.Values.controller.upgradePrecondition.resourceVersion = "9"; }],
    ["changed CA", (f: any) => { f.root.Values.inferenceBudget.caBundle = "changed"; }],
    ["changed root UID", (f: any) => { f.workspace.metadata.annotations["kars.azure.com/private-root-deployment-uid"] = "changed"; }],
    ["changed template", (f: any) => { f.controller.spec.template.spec.containers[0].image = "changed"; }],
    ["running root", (f: any) => { f.controller.spec.replicas = 1; }],
    ["missing adoption", (f: any) => { f.adoptionMap = {}; }],
    ["unsealed intent", (f: any) => alterProof(f, state => { state.replicaIntent = 2; })],
    ["unowned publication", (f: any) => { f.workspace.metadata.annotations[publicationKey] = "{}"; }],
    ["missing root capture", (f: any) => alterProof(f, state => { delete state.captured["workspace-uid"]; })],
    ["invalid exposed key", (f: any) => alterProof(f, state => { state.exposedKeys = ["invalid"]; })],
    ["other configuration", (f: any) => { f.root.Values.controller.extraEnv.push({ name: "NEW", value: "changed" }); }],
  ] as const)("rejects initial-pause maintenance with %s", (_name, change) => {
    const f = pausingFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function initialPrivateFixture(): any {
    const f = privateFixture(true);
    f.adoptionMap = {};
    f.root.Values.evals.runnerImage = f.root.Values.evals.runnerImageBaseline;
    delete f.root.Values.evals.runnerImageBaseline;
    return f;
  }
  function extraPrivateConsumer(f: any, kind = "Deployment"): string {
    const name = "credential-writer", uid = "writer-uid";
    const apiVersion = ["Pod", "ReplicationController"].includes(kind) ? "v1" : ["Job", "CronJob"].includes(kind) ? "batch/v1" : "apps/v1";
    const template = structuredClone(f.controller.spec.template);
    template.spec.serviceAccountName = "credential-writer";
    const metadata = { name, namespace: "kars-system", uid, resourceVersion: "15" };
    const consumer = kind === "Pod" ? { apiVersion, kind, metadata: { ...template.metadata, ...metadata }, spec: template.spec }
      : { apiVersion, kind, metadata, spec: kind === "CronJob" ? { jobTemplate: { spec: { template } } } : { template } };
    const id = `${kind}/${name}`;
    f.privateConsumers = { [id]: consumer };
    alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.unshift({ kind, object: { name, uid, resourceVersion: "15" }, templateDigest: templateDigest(consumer) });
      state.binding = retirementBinding(proof.activation);
    });
    return id;
  }
  function pendingCompanionFixture(): { f: any; id: string } {
    const f = privateFixture(true), id = extraPrivateConsumer(f);
    const fields = f.workspace.metadata.annotations;
    const proof = JSON.parse(fields[retirementKey]), state = JSON.parse(proof.retirement);
    state.phase = "retired";
    fields[retirementKey] = canonical(state);
    fields[publicationKey] = canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" });
    fields["kars.azure.com/private-state"] = "Pending";
    fields["kars.azure.com/private-parent-writer-uid"] = "current-epoch";
    f.controller.spec.replicas = f.root.Values.controller.replicas = f.privateConsumers[id].spec.replicas = 0;
    return { f, id };
  }
  it("renders owned Pending publication with a bound, paused Deployment companion", () => {
    const { f } = pendingCompanionFixture();
    expect(render(f)).toContain("verified");
  });
  it.each([
    ["missing companion", (f: any, id: string) => { delete f.privateConsumers[id]; }],
    ["missing parent enrollment", (f: any) => { delete f.workspace.metadata.annotations["kars.azure.com/private-parent-writer-uid"]; }],
    ["replacement UID", (f: any, id: string) => { f.privateConsumers[id].metadata.uid = "replacement"; }],
    ["running companion", (f: any, id: string) => { f.privateConsumers[id].spec.replicas = 1; }],
    ["foreign namespace", (f: any, id: string) => { f.privateConsumers[id].metadata.namespace = "foreign"; }],
    ["deleting companion", (f: any, id: string) => { f.privateConsumers[id].metadata.deletionTimestamp = "2026-01-01T00:00:00Z"; }],
    ["changed template", (f: any, id: string) => { f.privateConsumers[id].spec.template.spec.containers[0].image = "changed:latest"; }],
    ["unsupported kind", (f: any, id: string) => { f.privateConsumers[id].kind = "StatefulSet"; }],
    ["changed API", (f: any, id: string) => { f.privateConsumers[id].apiVersion = "apps/v2"; }],
    ["extra enrolled companion", (f: any, id: string) => {
      const extra = structuredClone(f.privateConsumers[id]);
      extra.metadata.name = "extra"; extra.metadata.uid = "extra-uid";
      f.privateConsumers["Deployment/extra"] = extra;
      f.workspace.metadata.annotations["kars.azure.com/private-parent-extra-uid"] = "current-epoch";
    }],
  ] as const)("rejects Pending publication with %s", (_name, change) => {
    const { f, id } = pendingCompanionFixture(); change(f, id);
    expect(() => render(f)).toThrow();
  });
  it("does not add unrelated, unenrolled Deployments to Pending retirement scope", () => {
    const { f, id } = pendingCompanionFixture();
    const unrelated = structuredClone(f.privateConsumers[id]);
    unrelated.metadata.name = "unrelated"; unrelated.metadata.uid = "unrelated-uid"; unrelated.spec.replicas = 1;
    f.privateConsumers["Deployment/unrelated"] = unrelated;
    expect(render(f)).toContain("verified");
  });
  const privateDefinition = JSON.parse(readFileSync(`${root}deploy/helm/kars/files/private-consumption.json`, "utf8"));
  const secret = privateDefinition.secrets[0];
  const classificationCases: [string, any, boolean][] = [
    ["ordinary credential writer", {}, false],
    ["ordinary API token", { volumes: [{ projected: { sources: [{ serviceAccountToken: { audience: "api" } }] } }] }, false],
    ["unrelated Secret", { volumes: [{ secret: { secretName: "ordinary-credential" } }] }, false],
    ["root API token", { serviceAccountName: "kars-controller" }, true],
    ["root without token", { serviceAccountName: "kars-controller", automountServiceAccountToken: false }, false],
    ["root projected token", { serviceAccountName: "kars-controller", automountServiceAccountToken: false,
      volumes: [{ projected: { sources: [{ serviceAccountToken: {} }] } }] }, true],
    ["budget TLS", { volumes: [{ secret: { secretName: "budget-tls" } }] }, true],
    ["private Secret", { volumes: [{ secret: { secretName: secret } }] }, true],
    ["private CSI Secret", { volumes: [{ csi: { nodePublishSecretRef: { name: secret } } }] }, true],
    ["private projected Secret", { volumes: [{ projected: { sources: [{ secret: { name: secret } }] } }] }, true],
    ["private pull Secret", { imagePullSecrets: [{ name: secret }] }, true],
    ["host path", { volumes: [{ hostPath: {} }] }, true],
  ];
  for (const audience of privateDefinition.tokenAudiences) classificationCases.push([
    `private token ${audience}`, { volumes: [{ projected: { sources: [{ serviceAccountToken: { audience } }] } }] }, true,
  ]);
  for (const field of ["hostNetwork", "hostPID", "hostIPC"]) classificationCases.push([field, { [field]: true }, true]);
  for (const kind of ["azureFile", "cephfs", "cinder", "flexVolume", "iscsi", "rbd", "scaleIO", "storageos"]) {
    classificationCases.push([`${kind} name`, { volumes: [{ [kind]: { secretName: secret } }] }, true]);
    classificationCases.push([`${kind} ref`, { volumes: [{ [kind]: { secretRef: { name: secret } } }] }, true]);
  }
  for (const group of ["containers", "initContainers", "ephemeralContainers"]) {
    for (const [name, fields] of [
      ["envFrom", { envFrom: [{ secretRef: { name: secret } }] }],
      ["env", { env: [{ name: "KEY", valueFrom: { secretKeyRef: { name: secret, key: "key" } } }] }],
      ["privileged", { securityContext: { privileged: true } }],
    ] as const) classificationCases.push([`${group} ${name}`, { [group]: [{ name: "consumer", ...fields }] }, true]);
    for (const capability of ["ALL", "SYS_ADMIN", "SYS_PTRACE", "SYS_MODULE", "SYS_RAWIO", "BPF", "PERFMON", "CHECKPOINT_RESTORE", "DAC_READ_SEARCH"]) {
      classificationCases.push([`${group} ${capability}`, { [group]: [{ name: "consumer", securityContext: { capabilities: { add: [capability] } } }] }, true]);
    }
    classificationCases.push([`${group} ordinary capability`, { [group]: [{ name: "consumer", securityContext: { capabilities: { add: ["NET_BIND_SERVICE"] } } }] }, false]);
  }
  it.each(classificationCases)("matches existing private-authority classification for %s", (_name, fields, expected) => {
    const f = initialPrivateFixture(), id = extraPrivateConsumer(f);
    const live = f.privateConsumers[id];
    Object.assign(live.spec.template.spec, fields);
    delete live.spec.template.metadata.annotations;
    alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers[0].templateDigest = templateDigest(live);
      state.binding = retirementBinding(proof.activation);
    });
    const proof = JSON.parse(f.workspace.metadata.annotations[retirementKey]);
    expect(consumesPrivateAuthority(live, "kars-system", proof.activation)).toBe(expected);
    if (expected) expect(() => render(f)).toThrow(/live consumer epoch changed/);
    else expect(render(f)).toContain("verified");
    live.spec.template.metadata.annotations = { "kars.azure.com/private-epoch": "current-epoch" };
    expect(render(f)).toContain("verified");
  });
  it.each(["Ready", "Blocked"])("adopts the sealed initial intent after independently qualified CA rotation with SRE %s", phase => {
    const f = initialPrivateFixture();
    extraPrivateConsumer(f);
    f.registration.status.phase = phase;
    if (phase === "Blocked") delete f.registration.status.privacyRevision;
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON());
    const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
    expect(selected.inferenceBudget.caBundle).toBe(f.catalog.data["ca.crt"]);
    expect(selected.controller.replicas).toBe(1);
    const original = structuredClone(f.root.Values);
    delete original.controller.image;
    original.inferenceBudget.caBundle = JSON.parse(f.intentMap.data["intent.json"]).budget.caBundle;
    expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(hash(canonicalSchema(original)));
  });
  it.each([
    ["unrelated image change", (f: any) => { f.root.Values.controller.image.digest = `sha256:${"f".repeat(64)}`; }],
    ["unrelated runtime change", (f: any) => { f.root.Values.evals.runnerImage += "changed"; }],
    ["changed funding", (f: any) => { f.root.Values.sre.firstInstallFunding.tokens++; }],
    ["stale SRE generation", (f: any) => { f.registration.status.observedGeneration = 0; }],
    ["unqualified migration", (f: any) => { f.registration.status.phase = "Migrating"; }],
    ["disabled authority", (f: any) => { f.registration.spec.enabled = false; }],
    ["changed source UID", (f: any) => { f.source.metadata.uid = "other"; }],
    ["changed root UID", (f: any) => { f.controller.metadata.uid = "other"; }],
    ["changed runtime UID", (f: any) => { f.runtime.metadata.uid = "other"; }],
    ["paused root", (f: any) => { f.controller.spec.replicas = 0; f.root.Values.controller.replicas = 0; }],
    ["missing retirement", (f: any) => { delete f.workspace.metadata.annotations[retirementKey]; }],
    ["changed retired binding", (f: any) => alterProof(f, state => { state.binding = "0".repeat(64); })],
  ] as const)("rejects initial private adoption with %s", (_name, change) => {
    const f = initialPrivateFixture();
    f.registration.status.phase = "Blocked";
    delete f.registration.status.privacyRevision;
    change(f);
    expect(() => render(f)).toThrow();
  });
  it.each(["Deployment", "ReplicaSet", "StatefulSet", "DaemonSet", "ReplicationController", "Job", "CronJob", "Pod"])(
    "verifies the actual additional %s consumer and order-independent retirement binding", kind => {
      const f = privateFixture(true);
      extraPrivateConsumer(f, kind);
      expect(render(f)).toContain("verified");
      alterProof(f, (_state, proof) => { proof.activation.namespaces[0].consumers.reverse(); });
      expect(render(f)).toContain("verified");
    });
  it.each([
    ["missing live object", (f: any, id: string) => { delete f.privateConsumers[id]; }],
    ["changed UID", (f: any, id: string) => { f.privateConsumers[id].metadata.uid = "replacement"; }],
    ["changed namespace", (f: any, id: string) => { f.privateConsumers[id].metadata.namespace = "other"; }],
    ["deleting object", (f: any, id: string) => { f.privateConsumers[id].metadata.deletionTimestamp = "now"; }],
    ["changed kind", (f: any, id: string) => { f.privateConsumers[id].kind = "Secret"; }],
    ["changed API", (f: any, id: string) => { f.privateConsumers[id].apiVersion = "v1"; }],
    ["stale epoch", (f: any, id: string) => { f.privateConsumers[id].spec.template.metadata.annotations["kars.azure.com/private-epoch"] = "old"; }],
    ["changed template", (f: any, id: string) => { f.privateConsumers[id].spec.template.spec.serviceAccountName = "other"; }],
    ["self-consistent changed template", (f: any, id: string) => {
      f.privateConsumers[id].spec.template.spec.serviceAccountName = "other";
      alterProof(f, (_state, proof) => { proof.activation.namespaces[0].consumers[0].templateDigest = templateDigest(f.privateConsumers[id]); });
    }],
    ["duplicate consumer", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.push(proof.activation.namespaces[0].consumers[0]); state.binding = retirementBinding(proof.activation);
    })],
    ["missing root consumer", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.pop(); state.binding = retirementBinding(proof.activation);
    })],
    ["foreign scope", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].namespace.name = "other"; state.binding = retirementBinding(proof.activation);
    })],
  ] as const)("rejects additional qualified consumer with %s", (_name, change) => {
    const f = privateFixture(true), id = extraPrivateConsumer(f);
    change(f, id);
    expect(() => render(f)).toThrow();
  });
  it.each([false, true])("retains the public CA and exact adoption receipt with qualified=%s", qualified => {
    const f = privateFixture(qualified, 3), original = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON());
    const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
    expect(selected.controller.replicas).toBe(qualified ? 3 : 0);
    expect(selected.inferenceBudget.caBundle).toBe(f.catalog.data["ca.crt"]);
    expect(selected.evals).toEqual(f.root.Values.evals);
    expect(f).toEqual(original);
    expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(f.adoptionMap.data.valuesDigest);
  });
  it.each([false, true])("retains externally managed TLS without adopting it with qualified=%s", qualified => {
    for (const manager of [undefined, "external-issuer"]) {
      const f = privateFixture(qualified);
      delete f.budgetTls.metadata.annotations;
      f.budgetTls.metadata.labels = manager ? { "app.kubernetes.io/managed-by": manager } : {};
      f.budgetTls.data = { "tls.crt": Buffer.from("rotated-certificate").toString("base64"), "tls.key": Buffer.from("rotated-key").toString("base64") };
      const original = structuredClone(f);
      const output = render(f, true);
      const docs = parseAllDocuments(output).map(d => d.toJSON());
      const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
      expect(selected.inferenceBudget.tls).toEqual(original.root.Values.inferenceBudget.tls);
      expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(f.adoptionMap.data.valuesDigest);
      expect(output).not.toContain("rotated-key");
      expect(output).not.toContain(f.budgetTls.data["tls.key"]);
      expect(f).toEqual(original);
    }
  });
  it.each([false, true])("requires release ownership for chart-managed TLS with qualified=%s", qualified => {
    const f = privateFixture(qualified, 1, true);
    expect(render(f)).toContain("verified");
    delete f.budgetTls.metadata.annotations;
    delete f.budgetTls.metadata.labels;
    expect(() => render(f)).toThrow(/release-owned catalog and TLS identities/);
  });
  it.each([
    ["replaced UID", (f: any) => { f.budgetTls.metadata.uid = "replacement"; }],
    ["changed version", (f: any) => { f.budgetTls.metadata.resourceVersion = "13"; }],
    ["deleting Secret", (f: any) => { f.budgetTls.metadata.deletionTimestamp = "now"; }],
    ["wrong namespace", (f: any) => { f.budgetTls.metadata.namespace = "other"; }],
    ["different selected Secret", (f: any) => { f.root.Values.inferenceBudget.tlsSecretName = "other"; }],
    ["missing Secret", (f: any) => { f.budgetTls = {}; }],
  ] as const)("rejects external TLS continuity with %s", (_name, change) => {
    const f = privateFixture(true);
    delete f.budgetTls.metadata.annotations;
    delete f.budgetTls.metadata.labels;
    expect(render(f)).toContain("verified");
    change(f);
    expect(() => render(f)).toThrow();
  });
  it("accepts the maintained certificate controller profile", () => {
    const f = privateFixture(true);
    alterProof(f, (state, proof) => {
      proof.activation.profile = "kcm-certificate"; proof.activation.controllerUids = {};
      state.binding = retirementBinding(proof.activation);
      f.workspace.metadata.annotations["kars.azure.com/private-profile"] = "kcm-certificate";
    });
    expect(render(f)).toContain("verified");
  });
  it.each([
    ["no completed adoption", (f: any) => { f.adoptionMap = {}; }],
    ["missing publication", (f: any) => { delete f.workspace.metadata.annotations[publicationKey]; }],
    ["publication already launched", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[publicationKey]); p.phase = "publishing"; f.workspace.metadata.annotations[publicationKey] = canonical(p); }],
    ["changed publication shape", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[publicationKey]); p.extra = true; f.workspace.metadata.annotations[publicationKey] = canonical(p); }],
    ["not retired", (f: any) => alterProof(f, s => { s.phase = "pausing"; })],
    ["unbound retirement", (f: any) => alterProof(f, s => { s.binding = "0".repeat(64); })],
    ["unknown receipt field", (f: any) => alterProof(f, s => { s.extra = true; })],
    ["false pauseRoot", (f: any) => alterProof(f, s => { s.pauseRoot = false; })],
    ["changed replica intent", (f: any) => alterProof(f, s => { s.replicaIntent = 2; })],
    ["self-consistent but unsealed replica intent", (f: any) => alterProof(f, s => { const other = JSON.parse(privateFixture(false, 2).workspace.metadata.annotations[retirementKey]); s.replicaIntent = other.replicaIntent; s.binding = other.binding; })],
    ["mutable runner image", (f: any) => { f.root.Values.evals.runnerImage = "localhost:5077/runner:latest"; }],
    ["forged runner baseline", (f: any) => { f.root.Values.evals.runnerImageBaseline = "localhost:5077/other:latest"; }],
    ["running root", (f: any) => { f.controller.spec.replicas = 1; }],
    ["changed root UID", (f: any) => { f.controller.metadata.uid = "replacement"; }],
    ["changed namespace UID", (f: any) => { f.workspace.metadata.uid = "replacement"; }],
    ["changed service account", (f: any) => { f.controller.spec.template.spec.serviceAccountName = "other"; }],
    ["changed template", (f: any) => { f.controller.spec.template.spec.containers[0].image += "changed"; }],
    ["missing controller profile identity", (f: any) => { delete f.workspace.metadata.annotations["kars.azure.com/private-job-controller-uid"]; }],
    ["changed TLS UID", (f: any) => { f.budgetTls.metadata.uid = "replacement"; }],
    ["unchanged TLS version", (f: any) => { f.budgetTls.metadata.resourceVersion = "11"; }],
    ["missing retired TLS baseline", (f: any) => alterProof(f, s => { delete s.baseline; })],
    ["missing exposed-key evidence", (f: any) => alterProof(f, s => { s.exposedKeys = []; })],
    ["foreign captured scope", (f: any) => alterProof(f, s => { s.captured.other = []; })],
    ["wrong TLS type", (f: any) => { f.budgetTls.type = "Opaque"; }],
    ["foreign catalog", (f: any) => { f.catalog.metadata.annotations["meta.helm.sh/release-name"] = "other"; }],
    ["deleting catalog", (f: any) => { f.catalog.metadata.deletionTimestamp = "now"; }],
    ["non-live CA", (f: any) => { f.root.Values.inferenceBudget.caBundle += "different"; }],
    ["private CA material", (f: any) => { f.root.Values.inferenceBudget.caBundle += "PRIVATE KEY"; f.catalog.data["ca.crt"] = f.root.Values.inferenceBudget.caBundle; }],
    ["funding changed", (f: any) => { f.root.Values.sre.firstInstallFunding.tokens++; }],
    ["router verifier changed", (f: any) => { f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`; }],
    ["adoption digest changed", (f: any) => { f.adoptionMap.data.valuesDigest = "0".repeat(64); }],
  ] as const)("rejects paused publication with %s", (_name, change) => { const f = privateFixture(); change(f); expect(() => render(f)).toThrow(); });
  it.each([
    ["stale published replica zero", (f: any) => { f.root.Values.controller.replicas = 0; }],
    ["wrong restored replicas", (f: any) => { f.controller.spec.replicas = 2; }],
    ["bad retirement digest", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[retirementKey]); p.retirementDigest = "0".repeat(64); f.workspace.metadata.annotations[retirementKey] = canonical(p); }],
    ["wrong activation phase", (f: any) => alterProof(f, (_s, p) => { p.activation.phase = "reviewed"; })],
    ["changed epoch", (f: any) => { f.controller.spec.template.metadata.annotations["kars.azure.com/private-epoch"] = "other"; }],
    ["different activation account", (f: any) => alterProof(f, (_s, p) => { p.activation.root.account.uid = "replacement"; })],
    ["additional consumer", (f: any) => alterProof(f, (_s, p) => { p.activation.namespaces[0].consumers.push(p.activation.namespaces[0].consumers[0]); })],
    ["different consumer template", (f: any) => alterProof(f, (_s, p) => { p.activation.namespaces[0].consumers[0].templateDigest = "0".repeat(64); })],
    ["stale qualified TLS", (f: any) => { f.budgetTls.metadata.resourceVersion = "13"; }],
    ["previously exposed key", (f: any) => alterProof(f, s => { s.exposedKeys.push("c".repeat(64)); })],
  ] as const)("rejects qualified continuity with %s", (_name, change) => { const f = privateFixture(true); change(f); expect(() => render(f)).toThrow(); });
});

describe("ordinary SRE ownership transition", () => {
  it("adopts the exact original unbounded authority without inventing tokens or expiry", () => {
    const f = fixture(1000, undefined, undefined, true);
    expect(() => render(f)).not.toThrow();
    expect(f.registration.spec.firstInstallFunding.grant).not.toHaveProperty("expiresAt");
    f.registration.spec.firstInstallFunding.grant.unboundedTokens = false;
    expect(() => render(f)).toThrow();
  });
  it.each(["UTC", "Etc/GMT-2", "Etc/GMT+7"])("honors UTC funding expiry in %s", timeZone => {
    for (const adopted of [false, true]) {
      for (const minutes of [-30, 30]) {
        const expiresAt = new Date(Date.now() + minutes * 60_000).toISOString().replace(/\.\d{3}Z$/, "Z");
        const f = adopted ? adoptedFixture(undefined, expiresAt) : fixture(1000, undefined, expiresAt);
        if (minutes > 0 || adopted) expect(() => render(f, false, timeZone)).not.toThrow();
        else expect(() => render(f, false, timeZone)).toThrow(/initial adoption also requires unexpired authority/);
      }
    }
  });
  it("renders an expired adopted repair without rewriting funding or manufacturing renewal authority", () => {
    const f = adoptedFixture(undefined, "2020-01-01T00:00:00Z");
    f.registration.status.phase = "Blocked";
    delete f.registration.status.privacyRevision;
    const before = structuredClone(f);
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(documents.some(document => document.kind === "KarsSRERegistration")).toBe(false);
    expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"]).inferenceBudget.tokenTopUps).toBeUndefined();
    expect(f).toEqual(before);
  });
  it.each([false, true])("allows adopted controller repair without granting blocked authority (cleared qualification: %s)", cleared => {
    const f = routerFixture();
    f.registration.status.phase = "Blocked";
    if (cleared) delete f.registration.status.privacyRevision;
    f.root.Values.controller.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"c".repeat(64)}`;
    const before = structuredClone(f);
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(before.resources);
    expect(documents.some(document => document.kind === "KarsSRERegistration")).toBe(false);
    expect(f).toEqual(before);
  });
  it.each([false, true])("requires qualified privacy for Ready authority (adopted: %s)", adopted => {
    const f = adopted ? adoptedFixture() : fixture();
    delete f.registration.status.privacyRevision;
    expect(() => render(f)).toThrow(/live original identities/);
  });
  it.each([false, true])("rejects unknown or non-current authority during recovery (adopted: %s)", adopted => {
    for (const phase of ["", "Pending", "Retired", "Unknown", ...(adopted ? [] : ["Blocked"])]) {
      const f = adopted ? adoptedFixture() : fixture();
      f.registration.status.phase = phase;
      expect(() => render(f)).toThrow(/live original identities/);
    }
  });
  it.each([
    "receipt-uid", "receipt-owner", "receipt-digest", "mutable-receipt", "deleted-receipt",
    "intent-uid", "intent-digest", "mutable-intent", "deleted-intent", "source-uid",
    "controller-uid", "runtime-uid", "workspace-uid", "privacy-revision", "generation",
    "disabled", "funding", "expiry", "values",
  ])("rejects changed %s while adopted SRE is blocked", fault => {
    const f = routerFixture();
    f.registration.status.phase = "Blocked";
    if (fault === "receipt-uid") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "receipt-digest") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "deleted-receipt") f.adoptionMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "intent-uid") f.intentMap.metadata.uid = "replacement";
    if (fault === "intent-digest") f.registration.spec.firstInstallFunding.intent.digest = "f".repeat(64);
    if (fault === "mutable-intent") f.intentMap.immutable = false;
    if (fault === "deleted-intent") f.intentMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "source-uid") f.source.metadata.uid = "replacement";
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "runtime-uid") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "privacy-revision") f.registration.status.privacyRevision = "unknown";
    if (fault === "generation") f.registration.status.observedGeneration = 0;
    if (fault === "disabled") f.registration.spec.enabled = false;
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens += 1;
    if (fault === "expiry") f.registration.spec.firstInstallFunding.grant.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "values") f.root.Values.controller.extraEnv.push({ name: "UNREVIEWED", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it("keeps the original runtime digest rule without an explicit upgrade baseline", () => {
    const documents = parseAllDocuments(render(adoptedFixture(), true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "controller-env").spec.containers[0].env).toEqual([
      { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
    ]);
  });
  it.each([false, true])("refreshes only the router tag/digest against the original sealed baseline (composed: %s)", composed => {
    const f = routerFixture(composed);
    const original = structuredClone(f);
    if (composed) {
      f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
      f.root.Values.controller.image.tag = `latest@sha256:${"c".repeat(64)}`;
      f.root.Values.controller.image.digest = `sha256:${"c".repeat(64)}`;
    }
    for (const digit of ["b", "c", "c"]) {
      f.root.Values.inferenceRouter.image.tag = `latest@sha256:${digit.repeat(64)}`;
      f.root.Values.inferenceBudget.routerImageDigest = `sha256:${digit.repeat(64)}`;
      for (const grants of [[], [topUp]]) {
        f.root.Values.inferenceBudget.tokenTopUps = grants;
        const selected = structuredClone(f.root.Values);
        const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
        expect(documents.find(document => document.metadata.name === "verified").data).toEqual(original.adoptionMap.data);
        expect(documents.find(document => document.metadata.name === "controller-env").spec.containers[0].env).toEqual([
          { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: selected.inferenceBudget.routerImageDigest },
          { name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
        ]);
        expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"])).toEqual(selected);
        expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(original.resources);
        expect(f.intentMap).toEqual(original.intentMap);
        expect(f.registration).toEqual(original.registration);
        expect(f.source).toEqual(original.source);
        expect(f.root.Values.sre.runtimeImageBaseline).toEqual(original.root.Values.sre.runtimeImageBaseline);
      }
    }
  });
  it.each(["matching", "stale", "removed"])("preserves an originally explicit router digest during refresh: %s", mode => {
    const f = adoptedFixture();
    f.root.Values.inferenceRouter.image.digest = `sha256:${"a".repeat(64)}`;
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
    f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    if (mode === "matching") f.root.Values.inferenceRouter.image.digest = f.root.Values.inferenceBudget.routerImageDigest;
    if (mode === "removed") delete f.root.Values.inferenceRouter.image.digest;
    if (mode !== "matching") {
      expect(() => render(f)).toThrow();
      return;
    }
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"]).inferenceRouter.image.digest)
      .toBe(`sha256:${"b".repeat(64)}`);
  });
  it("does not introduce the router baseline during initial ownership adoption", () => {
    expect(() => render(routerFixture(false, false))).toThrow(/requires completed SRE adoption/);
  });
  it.each([
    "missing-baseline", "forged-baseline", "mutable-baseline", "baseline-type", "empty-baseline",
    "mutable-selected", "wrong-tag", "mismatched-digest", "repository", "forged-repository-pair",
    "extra-image-field", "pull-policy", "router-env", "runtime-env", "runtime-baseline", "catalog", "tls",
    "built-in-funding", "original-tokens", "original-expiry", "bound-grant", "bound-expiry", "credential",
    "receipt-digest", "receipt-uid", "mutable-receipt", "receipt-owner", "source", "source-spec", "source-label",
    "intent-uid", "intent-digest", "intent-mutable", "status", "retired", "controller", "runtime", "workspace", "hook",
  ])("rejects an explicitly requested router transition with changed %s", fault => {
    const f = routerFixture(true);
    const values = f.root.Values;
    values.inferenceBudget.tokenTopUps = [topUp];
    if (fault === "missing-baseline") delete values.sre.routerImageBaseline;
    if (fault === "forged-baseline") values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"f".repeat(64)}`;
    if (fault === "mutable-baseline") values.sre.routerImageBaseline = "registry.example/router:latest";
    if (fault === "baseline-type") values.sre.routerImageBaseline = { image: values.sre.routerImageBaseline };
    if (fault === "empty-baseline") values.sre.routerImageBaseline = "";
    if (fault === "mutable-selected") values.inferenceRouter.image.tag = "latest";
    if (fault === "wrong-tag") values.inferenceRouter.image.tag = `v2@sha256:${"b".repeat(64)}`;
    if (fault === "mismatched-digest") values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "repository") values.inferenceRouter.image.repository = "foreign.example/router";
    if (fault === "forged-repository-pair") {
      values.inferenceRouter.image.repository = "foreign.example/router";
      values.sre.routerImageBaseline = `foreign.example/router:latest@sha256:${"a".repeat(64)}`;
    }
    if (fault === "extra-image-field") values.inferenceRouter.image.digest = `sha256:${"b".repeat(64)}`;
    if (fault === "pull-policy") values.inferenceRouter.image.pullPolicy = "Always";
    if (fault === "router-env") values.inferenceRouter.extraEnv = [{ name: "SKIP_AUTH", value: "true" }];
    if (fault === "runtime-env") values.runtimes.hermes.extraEnv = { SKIP_AUTH: "true" };
    if (fault === "runtime-baseline") values.sre.runtimeImageBaseline.hermes = values.runtimes.hermes.image;
    if (fault === "catalog") values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "tls") values.inferenceBudget.tls = { certificate: "replacement" };
    if (fault === "built-in-funding") values.inferenceBudget.builtInFunding = [{ tokens: 9000 }];
    if (fault === "original-tokens") values.sre.firstInstallFunding.tokens++;
    if (fault === "original-expiry") values.sre.firstInstallFunding.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "bound-grant") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "bound-expiry") f.registration.spec.firstInstallFunding.grant.expiresAt = "2000-01-01T00:00:00Z";
    if (fault === "credential") values.sre.existingSecret = "replacement";
    if (fault === "receipt-digest") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "receipt-uid") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "source-spec") f.source.spec.runtime = "replacement";
    if (fault === "source-label") {
      f.resources[0].metadata.labels = { "kars.azure.com/role": "sre" };
      f.source.metadata.labels = { "kars.azure.com/role": "replacement" };
    }
    if (fault === "intent-uid") f.intentMap.metadata.uid = "replacement";
    if (fault === "intent-digest") f.registration.spec.firstInstallFunding.intent.digest = "f".repeat(64);
    if (fault === "intent-mutable") f.intentMap.immutable = false;
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace") f.workspace.metadata.uid = "replacement";
    if (fault === "hook") values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
  it.each([{ baseline: undefined }, { baseline: [] }])("keeps the original receipt and funding across empty and published top-ups (sealed $baseline)", ({ baseline }) => {
    const f = adoptedFixture(baseline);
    const original = structuredClone(f);
    for (const grants of [[], [topUp], [topUp, { ...topUp, id: "operator-capacity-2", tokens: 7000 }]]) {
      f.root.Values.inferenceBudget.tokenTopUps = grants;
      for (let replay = 0; replay < 2; replay++) {
        const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
        expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(original.adoptionMap.data);
        expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(original.resources);
      }
      expect(f.intentMap).toEqual(original.intentMap);
      expect(f.registration).toEqual(original.registration);
      expect(f.source).toEqual(original.source);
    }
  });
  it("permits the new empty default during legacy initial adoption, but not new funding", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.tokenTopUps = [];
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(adoptedFixture().adoptionMap.data);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    expect(() => render(f)).toThrow(/Token top-ups require completed SRE adoption/);
  });
  it("composes top-ups with only the already supported image, pull and dispatcher transitions", () => {
    const f = runtimeFixture(true, true);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
  });
  it.each(["original-tokens", "original-expiry", "bound-grant", "catalog", "router", "tls", "built-in-funding", "receipt", "source", "schema-hook", "other-key", "malformed-list"])("does not conceal changed %s alongside a top-up", fault => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    if (fault === "original-tokens") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "original-expiry") f.root.Values.sre.firstInstallFunding.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "bound-grant") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "changed";
    if (fault === "router") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    if (fault === "tls") f.root.Values.inferenceBudget.tls = { certificate: "changed" };
    if (fault === "built-in-funding") f.root.Values.inferenceBudget.builtInFunding = [{ tokens: 9000 }];
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "schema-hook") f.root.Values.schemaHook.enabled = true;
    if (fault === "other-key") f.root.Values.inferenceBudget.ignoreAccounting = true;
    if (fault === "malformed-list") f.root.Values.inferenceBudget.tokenTopUps = { tokens: 5000 };
    expect(() => render(f)).toThrow();
  });
  it("emits the actual authorization list and retains the separate fund admission permission", () => {
    for (const grants of [[], [topUp]]) {
      const documents = renderFunding(grants);
      const config = documents.find(document => document.kind === "ConfigMap" && document.metadata.name === "kars-inference-budget-contracts");
      expect(JSON.parse(config.data["token-top-ups.json"])).toEqual(grants);
      const policy = documents.find(document => document.kind === "ValidatingAdmissionPolicy" && document.metadata.name === "kars-inference-budget-token-top-ups");
      expect(policy.spec.failurePolicy).toBe("Fail");
      expect(policy.spec.matchConstraints.resourceRules).toEqual([{
        apiGroups: [""], apiVersions: ["v1"], operations: ["CREATE", "UPDATE", "DELETE"], resources: ["configmaps"],
      }]);
      expect(policy.spec.validations.map((validation: any) => validation.expression)).toEqual([
        "((variables.oldFunding == variables.newFunding || (variables.oldFunding == '' && variables.newFunding == '[]')) && (variables.oldUnbounded == variables.newUnbounded || (variables.oldUnbounded == '' && variables.newUnbounded == '[]')) && variables.oldCalendarPolicy == variables.newCalendarPolicy) || authorizer.group('kars.azure.com').resource('karsbudgetaccounts').namespace(request.namespace).check('fund').allowed()",
      ]);
      const binding = documents.find(document => document.kind === "ValidatingAdmissionPolicyBinding" && document.spec.policyName === policy.metadata.name);
      expect(binding.spec.validationActions).toContain("Deny");
      for (const role of documents.filter(document => ["Role", "ClusterRole"].includes(document.kind))) {
        expect((role.rules ?? []).some((rule: any) => rule.verbs?.includes("fund"))).toBe(false);
      }
    }
    expect(() => renderFunding([topUp], false)).toThrow(/inferenceBudget.tokenTopUps requires inferenceBudget.enabled/);
  });
  it.each([
    { dispatcher: false, coverage: false }, { dispatcher: false, coverage: true },
    { dispatcher: true, coverage: false }, { dispatcher: true, coverage: true },
  ])("refreshes pinned runtime images against the sealed baseline ($dispatcher / $coverage)", ({ dispatcher, coverage }) => {
    const f = runtimeFixture(true, dispatcher);
    if (coverage) f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example,localhost:5077" });
    for (const digest of ["b", "d"]) {
      f.root.Values.sandbox.image.tag = `latest@sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("refreshes explicit runtime digests while preserving the sealed baseline and field shape", () => {
    const f = runtimeFixture(true, false, true);
    const baseline = structuredClone(f.root.Values.sre.runtimeImageBaseline);
    for (const digest of ["b", "d"]) {
      f.root.Values.sandbox.image.tag = `latest@sha256:${digest.repeat(64)}`;
      f.root.Values.sandbox.image.digest = `sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
      const values = JSON.parse(docs.find(document => document.metadata.name === "selected-values").data["values.json"]);
      expect(values.sandbox.image).toEqual(f.root.Values.sandbox.image);
      expect(values.sre.runtimeImageBaseline).toEqual(baseline);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it.each(["stale", "empty", "type", "removed", "added", "forged-baseline"])("rejects runtime digest drift: %s", fault => {
    const f = runtimeFixture(true, false, fault !== "added");
    const image = f.root.Values.sandbox.image;
    if (fault === "stale") image.digest = `sha256:${"a".repeat(64)}`;
    if (fault === "empty") image.digest = "";
    if (fault === "type") image.digest = { digest: `sha256:${"b".repeat(64)}` };
    if (fault === "removed") delete image.digest;
    if (fault === "added") image.digest = `sha256:${"b".repeat(64)}`;
    if (fault === "forged-baseline") f.root.Values.sre.runtimeImageBaseline.openclaw = `registry.example/openclaw:latest@sha256:${"f".repeat(64)}`;
    expect(() => render(f)).toThrow();
  });
  it("does not refresh runtime images during initial adoption", () => {
    expect(() => render(runtimeFixture(false))).toThrow();
  });
  it.each(["missing-baseline", "forged-baseline", "extra-baseline-field", "mutable-baseline", "baseline-type", "openclaw-mutable", "hermes-mutable", "openclaw-repository", "hermes-repository", "pull-policy", "runtime-env", "router", "funding", "source", "policy", "receipt", "mutable-receipt", "owner", "hook", "controller-env"])('rejects runtime image refresh with changed %s', fault => {
    const f = runtimeFixture();
    if (fault === "missing-baseline") delete f.root.Values.sre.runtimeImageBaseline;
    if (fault === "forged-baseline") f.root.Values.sre.runtimeImageBaseline.openclaw = `registry.example/openclaw:latest@sha256:${"f".repeat(64)}`;
    if (fault === "extra-baseline-field") f.root.Values.sre.runtimeImageBaseline.router = "ignored";
    if (fault === "mutable-baseline") f.root.Values.sre.runtimeImageBaseline.hermes = "registry.example/hermes:latest";
    if (fault === "baseline-type") f.root.Values.sre.runtimeImageBaseline = "invalid";
    if (fault === "openclaw-mutable") f.root.Values.sandbox.image.tag = "latest";
    if (fault === "hermes-mutable") f.root.Values.runtimes.hermes.image = "registry.example/hermes:latest";
    if (fault === "openclaw-repository") f.root.Values.sandbox.image.repository = "foreign.example/openclaw";
    if (fault === "hermes-repository") f.root.Values.runtimes.hermes.image = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "pull-policy") f.root.Values.sandbox.image.pullPolicy = "Always";
    if (fault === "runtime-env") f.root.Values.runtimes.hermes.extraEnv = { DISABLE_GOVERNANCE: "true" };
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "policy") f.resources[0].spec.policyProfile = "changed";
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    if (fault === "controller-env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it.each([false, true])("activates the pinned Kind dispatcher preserving receipt and source (new pull coverage: %s)", coverage => {
    const f = dispatcherFixture();
    if (coverage) f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    for (let attempt = 0; attempt < 2; attempt++) {
      const docs = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("preserves pull coverage already sealed before dispatcher activation", () => {
    const f = dispatcherFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    baseline.missionDispatcher = { enabled: false, authMode: "", image: {
      repository: "karsacr.azurecr.io/kars-mission-dispatcher", digest: "" }, relayUrl: "", registryUrl: "" };
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.controller.extraEnv[0].value = "localhost:5077";
    expect(() => render(f)).toThrow();
  });
  it("does not replace dispatcher configuration already bound into the adoption receipt", () => {
    const f = dispatcherFixture();
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.missionDispatcher.image.digest = `sha256:${"d".repeat(64)}`;
    expect(() => render(f)).toThrow();
  });
  it.each([
    { tag: false, authMode: "development" }, { tag: true, authMode: "development" },
    { tag: false, authMode: "entra" }, { tag: true, authMode: "entra" },
  ])("refreshes an originally sealed dispatcher image without changing authority ($tag / $authMode)", ({ tag, authMode }) => {
    const f = dispatcherRefreshFixture(tag, authMode);
    const original = structuredClone(f.root.Values.missionDispatcher.imageBaseline);
    for (const digest of ["d", "e"]) {
      const mission = f.root.Values.missionDispatcher;
      mission.image.digest = `sha256:${digest.repeat(64)}`;
      mission.image.repository = `localhost:5077/qualified-${digest}/dispatcher`;
      if (tag) mission.image.tag = `latest@${mission.image.digest}`;
      const docs = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
      const values = JSON.parse(docs.find(document => document.metadata.name === "selected-values").data["values.json"]);
      expect(values.missionDispatcher).toEqual(mission);
      expect(values.missionDispatcher.imageBaseline).toEqual(original);
    }
  });
  it("composes sealed dispatcher refresh with controller, runtime and pull-coverage changes", () => {
    const f = dispatcherRefreshFixture();
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    baseline.missionDispatcher.image = baseline.missionDispatcher.imageBaseline;
    delete baseline.missionDispatcher.imageBaseline;
    baseline.sandbox = { image: { repository: "registry.example/openclaw", tag: `latest@sha256:${"a".repeat(64)}` } };
    baseline.runtimes = { hermes: { image: `registry.example/hermes:latest@sha256:${"a".repeat(64)}` } };
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.sandbox = structuredClone(baseline.sandbox);
    f.root.Values.runtimes = structuredClone(baseline.runtimes);
    f.root.Values.sre.runtimeImageBaseline = {
      openclaw: `${baseline.sandbox.image.repository}:${baseline.sandbox.image.tag}`, hermes: baseline.runtimes.hermes.image,
    };
    f.root.Values.sandbox.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.runtimes.hermes.image = `registry.example/hermes:latest@sha256:${"b".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"f".repeat(64)}`;
    f.root.Values.controller.image.tag = `latest@${f.root.Values.controller.image.digest}`;
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    expect(render(f)).toContain("name: verified");
  });
  it.each(["missing-baseline", "forged-baseline", "last-selected-baseline", "baseline-type", "baseline-mutable",
    "extra-baseline-field", "baseline-tag", "baseline-repository-type", "selected-type", "selected-mutable", "selected-tag", "extra-image-field",
    "repository", "tag-shape", "disabled", "auth", "relay", "registry", "extra-field", "funding", "env", "source", "account", "receipt", "mutable-receipt", "unadopted"])("rejects sealed dispatcher refresh with %s", fault => {
    const f = dispatcherRefreshFixture(true);
    const mission = f.root.Values.missionDispatcher;
    if (fault === "missing-baseline") delete mission.imageBaseline;
    if (fault === "forged-baseline") mission.imageBaseline.digest = `sha256:${"f".repeat(64)}`;
    if (fault === "last-selected-baseline") mission.imageBaseline = structuredClone(mission.image);
    if (fault === "baseline-type") mission.imageBaseline = "invalid";
    if (fault === "baseline-mutable") mission.imageBaseline.digest = "latest";
    if (fault === "extra-baseline-field") mission.imageBaseline.bypass = true;
    if (fault === "baseline-tag") mission.imageBaseline.tag = "latest";
    if (fault === "baseline-repository-type") mission.imageBaseline.repository = [];
    if (fault === "selected-type") mission.image = "invalid";
    if (fault === "selected-mutable") mission.image.digest = "";
    if (fault === "selected-tag") mission.image.tag = "latest";
    if (fault === "extra-image-field") mission.image.bypass = true;
    if (fault === "repository") mission.image.repository = "foreign.example/dispatcher";
    if (fault === "tag-shape") delete mission.image.tag;
    if (fault === "disabled") mission.enabled = false;
    if (fault === "auth") mission.authMode = "entra";
    if (fault === "relay") mission.relayUrl = "wss://foreign.example";
    if (fault === "registry") mission.registryUrl = "https://foreign.example";
    if (fault === "extra-field") mission.bypass = true;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "account") delete f.source.spec.inferenceBudgetRef;
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "unadopted") f.adoptionMap = {};
    expect(() => render(f)).toThrow();
  });
  it("does not activate a dispatcher during initial SRE adoption", () => {
    expect(() => render(dispatcherFixture(false))).toThrow();
  });
  it.each(["profile", "auth", "mutable-image", "repository", "relay", "registry", "extra-field", "extra-image-field", "funding", "router", "env", "source", "account", "receipt", "mutable-receipt"])("rejects dispatcher activation with changed %s", fault => {
    const f = dispatcherFixture();
    const mission = f.root.Values.missionDispatcher;
    if (fault === "profile") f.root.Values.global.installationProfile = "";
    if (fault === "auth") mission.authMode = "entra";
    if (fault === "mutable-image") mission.image.digest = "";
    if (fault === "repository") mission.image.repository = "";
    if (fault === "relay") mission.relayUrl = "ws://foreign.example";
    if (fault === "registry") mission.registryUrl = "http://foreign.example";
    if (fault === "extra-field") mission.bypass = true;
    if (fault === "extra-image-field") mission.image.tag = "latest";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "account") delete f.source.spec.inferenceBudgetRef;
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    expect(() => render(f)).toThrow();
  });
  it("allows only the original funded authority with schemaHook disabled and records adoption", () => {
    const f = fixture();
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    const receipt = documents.find(document => document.kind === "ConfigMap");
    expect(receipt.data).toEqual(adoptedFixture().adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
  });
  it("allows a controller image refresh only after the unchanged ownership transition", () => {
    for (const adopted of [false, true]) {
      const f = adopted ? adoptedFixture() : fixture();
      f.root.Values.controller.image.digest = `sha256:${"b".repeat(64)}`;
      f.root.Values.controller.image.tag = `latest@${f.root.Values.controller.image.digest}`;
      if (adopted) expect(render(f)).toContain("name: verified");
      else expect(() => render(f)).toThrow();
    }
  });
  it("preserves the release receipt and source on repeated ordinary upgrades", () => {
    const f = adoptedFixture();
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
  });
  it("adds literal pull coverage after adoption without changing the receipt or funded source", () => {
    const f = adoptedFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    for (let attempt = 0; attempt < 2; attempt++) {
      const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("does not add pull coverage during the initial ownership transition", () => {
    const f = fixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    expect(() => render(f)).toThrow();
  });
  it.each(["duplicate", "valueFrom", "empty", "non-string", "extra-field", "other-env", "router", "funding", "source", "receipt-owner", "mutable-receipt"])("rejects pull coverage with changed %s", fault => {
    const f = adoptedFixture();
    const coverage: any = { name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" };
    f.root.Values.controller.extraEnv.push(coverage);
    if (fault === "duplicate") f.root.Values.controller.extraEnv.push({ ...coverage });
    if (fault === "valueFrom") {
      delete coverage.value;
      coverage.valueFrom = { configMapKeyRef: { name: "coverage", key: "registries" } };
    }
    if (fault === "empty") coverage.value = "";
    if (fault === "non-string") coverage.value = 5077;
    if (fault === "extra-field") coverage.valueFrom = {};
    if (fault === "other-env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    expect(() => render(f)).toThrow();
  });
  it("does not replace a pull declaration already bound into the adoption receipt", () => {
    const f = adoptedFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.controller.extraEnv[0].value = "localhost:5077";
    expect(() => render(f)).toThrow();
  });
  it.each(["receipt", "missing-values", "mutable-receipt", "receipt-owner", "deleting-receipt", "live-spec", "live-label", "values", "router", "catalog", "funding", "policy", "source", "intent", "controller", "runtime", "workspace", "retired", "status", "binding", "hook"])("keeps adopted upgrades fail-closed for changed %s", fault => {
    const f = adoptedFixture();
    if (fault === "receipt") f.adoptionMap.data.intentUID = "foreign";
    if (fault === "missing-values") delete f.adoptionMap.data.valuesDigest;
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "deleting-receipt") f.adoptionMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "live-spec") f.source.spec.runtime = "changed";
    if (fault === "live-label") {
      f.resources[0].metadata.labels = { "kars.azure.com/role": "sre" };
      const intent = JSON.parse(f.intentMap.data["intent.json"]);
      intent.resources = f.resources;
      f.intentMap.data["intent.json"] = JSON.stringify(intent);
      f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
      f.source.metadata.labels = { "kars.azure.com/role": "changed" };
    }
    if (fault === "values") f.root.Values.sre.model = "changed";
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "policy") f.resources[0].spec.policyProfile = "changed";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "intent") f.intentMap.metadata.uid = "replacement";
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace") f.workspace.metadata.uid = "replacement";
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "binding") delete f.source.spec.inferenceBudgetRef;
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
  it("preserves million-token funding across JSON and Kubernetes numeric representations", () => {
    expect(render(fixture(1500000))).toContain("name: verified");
    const changed = fixture(1500000);
    changed.registration.spec.firstInstallFunding.grant.limits.tokens++;
    expect(() => render(changed)).toThrow();
  });
  it("rejects router changes that require migration of the sealed first-install contract", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceRouter.image.tag = `latest@${f.root.Values.inferenceBudget.routerImageDigest}`;
    expect(() => render(f)).toThrow();
  });
  it.each(["mismatch", "mutable", "repository", "catalog", "funding", "source", "status"])("rejects router refresh with changed %s", fault => {
    const f = fixture();
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceRouter.image.tag = `latest@${f.root.Values.inferenceBudget.routerImageDigest}`;
    if (fault === "mismatch") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "mutable") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "repository") f.root.Values.inferenceRouter.image.repository = "unreviewed.example/router";
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "status") f.registration.status.phase = "Retired";
    expect(() => render(f)).toThrow();
  });
  it.each(["values", "source", "intent", "funding", "binding", "retired", "controller", "runtime", "status", "owner"])("rejects changed %s without replacing authority", fault => {
    const f = fixture();
    if (fault === "values") f.root.Values.sre.model = "changed";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "intent") f.intentMap.metadata.uid = "replacement";
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "binding") delete f.source.spec.inferenceBudgetRef;
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "owner") f.intentMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    expect(() => render(f)).toThrow();
  });
});
