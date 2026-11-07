// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const digest = `sha256:${"a".repeat(64)}`;
const image = { repository: "registry.example/image", tag: `latest@${digest}` };
function fixture(): any {
  return {
    global: { installationProfile: "kind-beta" }, kindBeta: { cni: "calico" },
    schemaHook: { enabled: true }, sre: { enabled: true, provider: "foundry", model: "test-model",
      firstInstallFunding: { unboundedTokens: true } },
    inferenceBudget: { enabled: true, catalogVersion: "test", contracts: [{}], caBundle: "test-ca", tlsSecretName: "budget-tls", tls: {} },
    headlamp: { enabled: true, oidc: { enabled: true, issuerURL: "https://identity.example" }, image },
    agentMesh: { enabled: true, registry: { image }, relay: { image } },
    datapathWitness: { managed: true, enabled: true, scope: "all", sandboxes: [], nodes: ["worker"] },
    sandbox: { image, nodeSelector: { "kubernetes.io/hostname": "worker" }, seccompInstallerImage: `registry.example/seccomp:latest@${digest}` },
    controller: { image: { ...image, digest }, extraEnv: [], inferenceDefaults: { provider: "foundry" } },
    runtimes: { hermes: { image: `registry.example/hermes:latest@${digest}` } },
    inferenceRouter: { image, azure: { openai: { endpoint: "", credentials: { existingSecret: "provider", key: "key" } } } },
    foundry: { endpoint: "https://models.example" }, localInference: {},
    entraSidecar: { enabled: false }, networkPolicy: { defaultDenyEgress: true },
    metricsServer: { enabled: false, kubelet: { insecureTLS: false } }, monitoring: { enabled: false }, cilium: { enabled: false },
  };
}
function render(values: any, timeZone?: string, upgrade = false, receipts = false, providerSecret?: Record<string, unknown>, continuity?: { workspace: any; grant: any; bootstrap: any }) {
  const directory = mkdtempSync(join(tmpdir(), "kars-kind-profile-"));
  try {
    mkdirSync(join(directory, "templates"));
    writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: profile-test\nversion: 0.1.0\n");
    writeFileSync(join(directory, "values.yaml"), JSON.stringify(values));
    writeFileSync(join(directory, "templates/_sre-resources.tpl"), readFileSync(join(root, "deploy/helm/kars/templates/_sre-resources.tpl")));
    let template = readFileSync(join(root, "deploy/helm/kars/templates/kind-beta.yaml"), "utf8");
    if (providerSecret !== undefined) template = template.replace(
      'lookup "v1" "Secret" .Release.Namespace "kars-inference-providers"',
      `(fromJson ${JSON.stringify(JSON.stringify(providerSecret))})`,
    );
    if (continuity) {
      for (const [lookup, object] of [
        ['lookup "v1" "Namespace" "" .Release.Namespace', continuity.workspace],
        ['lookup "kars.azure.com/v1alpha1" "KarsCredentialGrant" .Release.Namespace "workspace"', continuity.grant],
        ['lookup "v1" "Secret" .Release.Namespace $credentials.existingSecret', continuity.bootstrap],
      ] as const) template = template.replace(lookup, `(fromJson ${JSON.stringify(JSON.stringify(object))})`);
    }
    writeFileSync(join(directory, "templates/profile.yaml"), receipts
      ? template.replaceAll(/lookup "v1" "ConfigMap" \.Release.Namespace "kars-sre-(first-install|helm-adoption)"/g, 'dict "immutable" true')
      : template);
    return execFileSync("helm", ["template", "kars", directory, "--namespace", "kars-system", "--kube-version", "1.35.0", "--api-versions", "crd.projectcalico.org/v1", ...(upgrade ? ["--is-upgrade"] : [])],
      { encoding: "utf8", timeout: 10000, stdio: ["ignore", "pipe", "pipe"],
        env: { ...process.env, ...(timeZone ? { TZ: timeZone } : {}) } });
  } finally { rmSync(directory, { recursive: true, force: true }); }
}
describe("Kind beta runtime pull coverage", () => {
  it.each(["UTC", "Etc/GMT-2", "Etc/GMT+7"])("requires non-expiring beta funding in %s", timeZone => {
    expect(() => render(fixture(), timeZone)).not.toThrow();
    for (const minutes of [-30, 30]) {
      const values = fixture();
      values.sre.firstInstallFunding = { tokens: 1000,
        expiresAt: new Date(Date.now() + minutes * 60_000).toISOString().replace(/\.\d{3}Z$/, "Z") };
      expect(() => render(values, timeZone)).toThrow(/unbounded non-expiring first-install funding/);
    }
  });
  it("permits only adopted upgrade repairs past expiry, leaving sealed receipt validation to the SRE transition", () => {
    const values = fixture();
    values.sre.firstInstallFunding = { tokens: 1000, expiresAt: "2020-01-01T00:00:00Z" };
    values.schemaHook.enabled = false;
    const before = structuredClone(values);
    expect(() => render(values, "UTC", true, true)).not.toThrow();
    expect(() => render(values, "UTC", true, false)).toThrow(/unbounded non-expiring first-install funding/);
    expect(() => render(values, "UTC", false, true)).toThrow(/requires first-install schemaHook/);
    values.schemaHook.enabled = true;
    expect(() => render(values, "UTC", true, true)).toThrow(/unbounded non-expiring first-install funding/);
    expect(() => render(values, "UTC", false, true)).toThrow(/unbounded non-expiring first-install funding/);
    values.schemaHook.enabled = false;
    expect(values).toEqual(before);
  });
  it("permits qualified optional Grafana only after adoption without relaxing initial monitoring", () => {
    const values = fixture();
    values.monitoring = { enabled: true,
      prometheusOperator: { image, prometheusConfigReloader: { image }, admissionWebhooks: { patch: { image } } },
      prometheus: { prometheusSpec: { image } }, "kube-state-metrics": { image }, "prometheus-node-exporter": { image },
      jsonExporter: { enabled: false }, inspektorGadget: { enabled: false }, alertmanager: { enabled: false },
      grafana: { enabled: true, image: { ...image }, sidecar: { image: { ...image } } } };
    expect(() => render(values)).toThrow(/initial monitoring excludes/);
    values.schemaHook.enabled = false;
    expect(() => render(values, undefined, true, false)).toThrow(/initial monitoring excludes/);
    expect(() => render(values, undefined, true, true)).not.toThrow();
    for (const target of [values.monitoring.grafana.image, values.monitoring.grafana.sidecar.image]) {
      target.tag = "latest";
      expect(() => render(values, undefined, true, true)).toThrow(/operator-qualified immutable/);
      target.tag = image.tag;
    }
    for (const component of [values.monitoring.alertmanager, values.monitoring.inspektorGadget]) {
      component.enabled = true;
      expect(() => render(values, undefined, true, true)).toThrow(/initial monitoring excludes/);
      component.enabled = false;
    }
  });
  it("retains the original profile without optional registry metadata", () => {
    expect(() => render(fixture())).not.toThrow();
  });
  it.each([{}, { metadata: { name: "kars-inference-providers" } }, { data: {}, stringData: {} }])("permits a missing or empty provider store: %j", providerSecret => {
    expect(() => render(fixture(), undefined, false, false, providerSecret)).not.toThrow();
  });
  it.each([
    { data: { COPILOT_GITHUB_TOKEN: "dGVzdA==" } },
    { data: { COPILOT_GITHUB_TOKEN: "" } },
    { stringData: { COPILOT_GITHUB_TOKEN: "test" } },
    { data: {}, stringData: { COPILOT_GITHUB_TOKEN: "" } },
  ])("still rejects a provider store with any credential keys: %j", providerSecret => {
    expect(() => render(fixture(), undefined, false, false, providerSecret)).toThrow(/default provider route must not be shadowed/);
  });
  function enrolledProviders() {
    const values = fixture();
    values.schemaHook.enabled = false;
    const secret: any = { type: "Opaque", metadata: { name: "kars-inference-providers", namespace: "kars-system", uid: "store",
      annotations: { "kars.azure.com/credential-store-grant-uid": "grant" } },
      data: { COPILOT_GITHUB_TOKEN: "dGVzdA==", KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT: Buffer.from("https://api.githubcopilot.com").toString("base64") } };
    const authority = {
      workspace: { metadata: { uid: "workspace" } } as any,
      grant: { metadata: { uid: "grant", generation: 2 }, spec: { enabled: true, workspaceUid: "workspace",
        integrationStores: [{ purpose: "providers", secret: { name: "kars-inference-providers", uid: "store" } }] },
      status: { phase: "Ready", observedGeneration: 2 } } as any,
      bootstrap: { metadata: { uid: "bootstrap" }, data: { key: "b3JpZ2luYWw=" } } as any,
    };
    return { values, secret, authority };
  }
  it("retains enrolled additional routes on adopted upgrades without publishing credential values", () => {
    const f = enrolledProviders();
    const before = structuredClone(f);
    const rendered = render(f.values, undefined, true, true, f.secret, f.authority);
    expect(rendered).not.toContain("COPILOT_GITHUB_TOKEN");
    expect(rendered).not.toContain("dGVzdA==");
    expect(f).toEqual(before);
    expect(() => render(f.values, undefined, true, false, f.secret, f.authority)).toThrow();
    f.values.schemaHook.enabled = true;
    expect(() => render(f.values, undefined, false, true, f.secret, f.authority)).toThrow(/must not be shadowed/);
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).toThrow(/must not be shadowed/);
  });
  function selectedProvider() {
    const f = enrolledProviders();
    f.values.sre.modelRouteBaseline = { provider: "foundry", model: "test-model" };
    f.values.sre.provider = "github-copilot";
    f.values.sre.model = "claude-opus-5.5";
    f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN = "c2VhdA==";
    f.values.inferenceBudget.contracts = [{ providerId: "github-copilot", model: "claude-opus-5.5",
      endpoint: "https://api.githubcopilot.com" }];
    return f;
  }
  it("selects an enrolled contracted SRE provider without changing the bootstrap route", () => {
    const f = selectedProvider();
    const before = structuredClone(f);
    const rendered = render(f.values, undefined, true, true, f.secret, f.authority);
    expect(rendered).not.toContain("COPILOT_GITHUB_TOKEN");
    expect(rendered).not.toContain("dGVzdA==");
    expect(f).toEqual(before);
    f.secret.data.KARS_PROVIDER_FOUNDRY_ENDPOINT = Buffer.from(f.values.foundry.endpoint).toString("base64");
    f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY = f.authority.bootstrap.data.key;
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).not.toThrow();
    f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY = "b3RoZXI=";
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).toThrow(/retain the original endpoint and credential/);
  });
  it("preserves an enrolled uncontracted SRE route under unbounded token-only funding", () => {
    const f = selectedProvider();
    f.values.inferenceBudget.contracts = [{}];
    const before = structuredClone(f);
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).not.toThrow();
    expect(f).toEqual(before);
  });
  it.each([{}, { unboundedTokens: false }, { unboundedTokens: "true" },
    { unboundedTokens: true, usdMicros: 100 }, { tokens: 100, expiresAt: "2999-01-01T00:00:00Z" }])(
    "requires a selected-route contract for non-token-only funding %j", funding => {
      const f = selectedProvider();
      f.values.sre.firstInstallFunding = funding;
      f.values.inferenceBudget.contracts = [{}];
      expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).toThrow(/matching provider\/model\/endpoint contract/);
    });
  it.each(["api-key", "string-data", "explicit-port", "endpoint-path", "no-legacy-token"])("selects the enrolled SRE route with %s", variant => {
    const f = selectedProvider();
    if (variant === "api-key") {
      f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_API_KEY = f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN;
      delete f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN;
    }
    if (variant === "string-data") {
      f.secret.stringData = Object.fromEntries(Object.entries(f.secret.data).map(([key, value]) => [key, Buffer.from(value as string, "base64").toString()]));
      delete f.secret.data;
    }
    if (variant === "explicit-port" || variant === "endpoint-path") {
      const endpoint = variant === "explicit-port" ? "https://api.githubcopilot.com:443" : "https://api.githubcopilot.com/api";
      f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT = Buffer.from(endpoint).toString("base64");
      f.values.inferenceBudget.contracts[0].endpoint = endpoint;
    }
    if (variant === "no-legacy-token") delete f.secret.data.COPILOT_GITHUB_TOKEN;
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).not.toThrow();
  });
  it.each(["no-baseline", "no-adoption", "no-store", "empty-store", "no-endpoint", "http", "non-443", "url-auth",
    "query", "fragment", "no-auth", "empty-auth", "whitespace-auth", "no-contract", "wrong-model", "wrong-provider", "wrong-endpoint", "duplicate-endpoint",
    "duplicate-token", "non-string-endpoint", "non-string-token"])("rejects selected SRE provider with %s", fault => {
    const f = selectedProvider();
    if (fault === "no-baseline") delete f.values.sre.modelRouteBaseline;
    if (fault === "no-store") f.secret = {};
    if (fault === "empty-store") f.secret.data = {};
    if (fault === "no-endpoint") delete f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT;
    const endpoints: Record<string, string> = { http: "http://api.githubcopilot.com", "non-443": "https://api.githubcopilot.com:8443",
      "url-auth": "https://user@api.githubcopilot.com", query: "https://api.githubcopilot.com?secret=x", fragment: "https://api.githubcopilot.com#fragment" };
    if (endpoints[fault]) f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT = Buffer.from(endpoints[fault]).toString("base64");
    if (fault === "no-auth") delete f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN;
    if (fault === "empty-auth") f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN = "";
    if (fault === "whitespace-auth") f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN = "ICA=";
    if (fault === "duplicate-token") f.secret.stringData = { KARS_PROVIDER_GITHUB_COPILOT_TOKEN: "seat" };
    if (fault === "non-string-endpoint") f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT = 123;
    if (fault === "non-string-token") f.secret.data.KARS_PROVIDER_GITHUB_COPILOT_TOKEN = 123;
    if (["no-contract", "wrong-model", "wrong-provider", "wrong-endpoint"].includes(fault)) {
      f.values.sre.firstInstallFunding = { tokens: 100, expiresAt: "2999-01-01T00:00:00Z" };
    }
    if (fault === "no-contract") f.values.inferenceBudget.contracts = [{}];
    if (fault === "wrong-model") f.values.inferenceBudget.contracts[0].model = "other";
    if (fault === "wrong-provider") f.values.inferenceBudget.contracts[0].providerId = "other";
    if (fault === "wrong-endpoint") f.values.inferenceBudget.contracts[0].endpoint = "https://other.example";
    if (fault === "duplicate-endpoint") f.secret.stringData = { KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT: "https://api.githubcopilot.com" };
    expect(() => render(f.values, undefined, true, fault !== "no-adoption", f.secret, f.authority)).toThrow();
  });
  it.each(["foundry", "azure-openai"])("retains only the identical %s bootstrap alias", provider => {
    const f = enrolledProviders();
    f.values.sre.provider = provider;
    f.values.controller.inferenceDefaults.provider = provider;
    const endpoint = f.values.foundry.endpoint;
    if (provider === "azure-openai") {
      f.values.foundry.endpoint = "";
      f.values.inferenceRouter.azure.openai.endpoint = endpoint;
    }
    const prefix = `KARS_PROVIDER_${provider.toUpperCase().replaceAll("-", "_")}_`;
    f.secret.data[`${prefix}ENDPOINT`] = Buffer.from(endpoint).toString("base64");
    f.secret.data[`${prefix}API_KEY`] = f.authority.bootstrap.data.key;
    const before = structuredClone(f);
    expect(render(f.values, undefined, true, true, f.secret, f.authority)).not.toContain("original");
    expect(f).toEqual(before);
    f.secret.data[`${prefix}TOKEN`] = f.authority.bootstrap.data.key;
    delete f.secret.data[`${prefix}API_KEY`];
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).not.toThrow();
  });
  it.each(["no-grant", "no-workspace", "no-store-uid", "disabled", "unready", "stale", "no-generation", "wrong-workspace",
    "wrong-grant", "missing-annotation", "wrong-store", "wrong-purpose", "unenrolled", "duplicate", "grant-deleting", "workspace-deleting",
    "store-deleting", "wrong-type", "integration-error", "env-injection", "string-env-injection"])("rejects provider continuity with %s", fault => {
    const f = enrolledProviders();
    const grant = f.authority.grant;
    if (fault === "no-grant") f.authority.grant = {};
    if (fault === "no-workspace") f.authority.workspace = {};
    if (fault === "no-store-uid") delete f.secret.metadata.uid;
    if (fault === "disabled") grant.spec.enabled = false;
    if (fault === "unready") grant.status.phase = "Pending";
    if (fault === "stale") grant.status.observedGeneration = 1;
    if (fault === "no-generation") delete grant.metadata.generation;
    if (fault === "wrong-workspace") grant.spec.workspaceUid = "replacement";
    if (fault === "wrong-grant") grant.metadata.uid = "replacement";
    if (fault === "missing-annotation") delete f.secret.metadata.annotations;
    if (fault === "wrong-store") grant.spec.integrationStores[0].secret.uid = "replacement";
    if (fault === "wrong-purpose") grant.spec.integrationStores[0].purpose = "teams";
    if (fault === "unenrolled") grant.spec.integrationStores = [];
    if (fault === "duplicate") grant.spec.integrationStores.push(structuredClone(grant.spec.integrationStores[0]));
    if (fault === "grant-deleting") grant.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "workspace-deleting") f.authority.workspace.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "store-deleting") f.secret.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "wrong-type") f.secret.type = "kubernetes.io/basic-auth";
    if (fault === "integration-error") grant.status.integrationError = "unreconciled";
    if (fault === "env-injection") f.secret.data.HTTPS_PROXY = "";
    if (fault === "string-env-injection") f.secret.stringData = { FOUNDRY_ENDPOINT: "https://other.example" };
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).toThrow();
  });
  it.each(["endpoint", "key", "token", "no-credential", "no-endpoint", "no-bootstrap", "empty-bootstrap", "deleting-bootstrap", "duplicate", "non-string"])("rejects default route shadowing with %s", fault => {
    const f = enrolledProviders();
    f.secret.data.KARS_PROVIDER_FOUNDRY_ENDPOINT = Buffer.from(f.values.foundry.endpoint).toString("base64");
    f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY = f.authority.bootstrap.data.key;
    if (fault === "endpoint") f.secret.data.KARS_PROVIDER_FOUNDRY_ENDPOINT = Buffer.from("https://other.example").toString("base64");
    if (fault === "key") f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY = "b3RoZXI=";
    if (fault === "token") f.secret.data.KARS_PROVIDER_FOUNDRY_TOKEN = "b3RoZXI=";
    if (fault === "no-credential") delete f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY;
    if (fault === "no-endpoint") delete f.secret.data.KARS_PROVIDER_FOUNDRY_ENDPOINT;
    if (fault === "no-bootstrap") f.authority.bootstrap = {};
    if (fault === "empty-bootstrap") f.authority.bootstrap.data.key = "";
    if (fault === "deleting-bootstrap") f.authority.bootstrap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "duplicate") f.secret.stringData = { KARS_PROVIDER_FOUNDRY_API_KEY: "original" };
    if (fault === "non-string") f.secret.data.KARS_PROVIDER_FOUNDRY_API_KEY = 123;
    expect(() => render(f.values, undefined, true, true, f.secret, f.authority)).toThrow();
  });
  it("accepts one explicit literal registry declaration", () => {
    const values = fixture();
    values.controller.extraEnv = [{ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" }];
    expect(() => render(values)).not.toThrow();
  });
  it.each(["override", "duplicate", "valueFrom", "empty", "non-string", "extra-field", "egress", "tls", "entra", "prototype"])("still rejects %s", fault => {
    const values = fixture();
    const coverage: any = { name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" };
    values.controller.extraEnv = [coverage];
    if (fault === "override") coverage.name = "SANDBOX_IMAGE";
    if (fault === "duplicate") values.controller.extraEnv.push({ ...coverage });
    if (fault === "valueFrom") {
      delete coverage.value;
      coverage.valueFrom = { secretKeyRef: { name: "private", key: "registries" } };
    }
    if (fault === "empty") coverage.value = "";
    if (fault === "non-string") coverage.value = 5077;
    if (fault === "extra-field") coverage.valueFrom = {};
    if (fault === "egress") values.networkPolicy.defaultDenyEgress = false;
    if (fault === "tls") values.metricsServer.kubelet.insecureTLS = true;
    if (fault === "entra") values.entraSidecar.enabled = true;
    if (fault === "prototype") values.policyPrototype = { enabled: true };
    expect(() => render(values)).toThrow();
  });
});
