// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect } from "vitest";
import { parse, parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";

export const root = fileURLToPath(new URL("../../../", import.meta.url));

export const hash = (value: string) => createHash("sha256").update(value).digest("hex");

export function fixture(tokens = 1000, tokenTopUps?: unknown[], expiresAt = "2999-01-01T00:00:00Z", unbounded = false): any {
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

export function render(input: any, exposeValues = false, timeZone?: string, witnessDependency = false) {
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

export function adoptedFixture(tokenTopUps?: unknown[], expiresAt?: string): any {
  const f = fixture(1000, tokenTopUps, expiresAt);
  const values = structuredClone(f.root.Values);
  delete values.controller.image;
  f.adoptionMap = { immutable: true, metadata: { uid: "adoption-uid", annotations: { ...f.intentMap.metadata.annotations } },
    data: { intentUID: f.intentMap.metadata.uid, valuesDigest: hash(canonicalSchema(values)) } };
  return f;
}

export function dispatcherFixture(adopted = true): any {
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

export function dispatcherRefreshFixture(tag = false, authMode = "development"): any {
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

export function runtimeFixture(adopted = true, dispatcher = false, explicitDigest = false): any {
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

export function routerFixture(composed = false, adopted = true): any {
  const f = composed ? runtimeFixture(adopted, true) : adopted ? adoptedFixture() : fixture();
  f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
  f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
  f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
  return f;
}

export function contractRenewalFixture(adopted = true): any {
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

export function dailyLimitFixture(adopted = true): any {
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

export const originalQualifiedWitnessImage = "localhost:5077/kars-beta-fresh4-e865b5c5/kars-datapath-witness:latest@sha256:586f44a447f185dae637fe3aef23a860374caff96995be73aafeaac07a86e599";

export const refreshedQualifiedWitnessImage = "localhost:5077/kars-beta-fleet-witness-20261025-r1/kars-datapath-witness:latest@sha256:bb6356930d955bfb1fce2b14262768ec151dfddfe8d97eab5e7fb6636e98fad2";

export function witnessFixture(scope?: "selected" | "all", image = `registry.example/witness:latest@sha256:${"a".repeat(64)}`): any {
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

export const topUp = {
  id: "operator-capacity-1", configUid: "catalog-uid", accountUid: "account-uid",
  root: { kind: "KarsSandbox", resource: { name: "sre", namespace: "kars-system", uid: "source-uid" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid" },
  originalLimits: { tokens: 1000 }, tokens: 5000, expiresAt: "2999-01-01T00:00:00Z",
  nodes: { "source-uid": `sha256:${"a".repeat(64)}` },
};

export function renderFunding(tokenTopUps: unknown[], enabled = true, unboundedTokenAllowances: unknown[] = []): any[] {
  const output = execFileSync("helm", ["template", "kars", `${root}deploy/helm/kars`, "-n", "kars-system", "-f", "-"], {
    encoding: "utf8", timeout: 30000, stdio: ["pipe", "pipe", "pipe"],
    input: JSON.stringify({ inferenceBudget: { enabled, tokenTopUps, unboundedTokenAllowances, catalogVersion: "test-v1", contracts: [{ id: "test" }],
      caBundle: "public-test-ca", tlsSecretName: "budget-tls", routerImageDigest: `sha256:${"a".repeat(64)}` } }),
  });
  return parseAllDocuments(output).map(document => document.toJSON()).filter(Boolean);
}
