// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { parse, parseAllDocuments, stringify } from "yaml";

type Map = Record<string, any>;
const chart = resolve(dirname(fileURLToPath(import.meta.url)), "../../../deploy/helm/kars");
const directories: string[] = [];
const capability = "kars.azure.com/v1alpha1/KarsAuthConfig";
const tenantId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const blueprintClientId = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const blueprintObjectId = "cccccccc-cccc-cccc-cccc-cccccccccccc";
const mi = "dddddddd-dddd-dddd-dddd-dddddddddddd";
const prefix = "AzureAd__ClientCredentials__0__";

function temporary(): string {
  const path = mkdtempSync(join(tmpdir(), "kars-helm-identity-"));
  directories.push(path);
  return path;
}
afterEach(() => { for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true }); });
function spec(mode = "WorkloadIdentity"): Map {
  return {
    tenant: { tenantId }, agentId: { blueprintClientId, blueprintObjectId },
    controller: { credentialMode: mode, ...(mode === "ManagedIdentityImds" ? { managedIdentityClientId: mi } : {}) },
  };
}
function values(auth = spec()): Map { return { entraSidecar: { enabled: true, authConfig: auth } }; }
function render(v: Map = {}, options: { chart?: string; namespace?: string; noCapability?: boolean; unresolvedLookup?: boolean; args?: string[] } = {}): Map[] {
  const input = join(temporary(), "values.yaml");
  writeFileSync(input, stringify(v));
  // Enabled offline cases use an explicit supported-CRD lookup fixture, never
  // discovery alone. No fixture is a separate fail-closed regression below.
  const selectedChart = options.chart ?? (v.entraSidecar?.enabled && !options.noCapability && !options.unresolvedLookup ? lookups() : chart);
  const output = execFileSync("helm", [
    "template", "kars", selectedChart, "--namespace", options.namespace ?? "kars-system", "-f", input,
    ...(!options.noCapability ? ["--api-versions", capability] : []), ...(options.args ?? []),
  ], { encoding: "utf8", timeout: 30_000, stdio: ["ignore", "pipe", "pipe"] });
  return parseAllDocuments(output).map(doc => {
    if (doc.errors.length) throw doc.errors[0];
    return doc.toJSON() as Map;
  }).filter(Boolean);
}
function resource(docs: Map[], kind: string, name: string): Map {
  const result = docs.find(d => d.kind === kind && d.metadata.name === name);
  if (!result) throw new Error(`Missing ${kind}/${name}`);
  return result;
}
function projection(docs: Map[]): Map {
  const deployment = resource(docs, "Deployment", "entra-auth-sidecar");
  const ref = deployment.spec.template.spec.containers[0].envFrom[0].configMapRef;
  expect(ref.optional).toBe(false);
  const config = resource(docs, "ConfigMap", ref.name);
  expect(config.immutable).toBe(true);
  expect(config.metadata.annotations["helm.sh/resource-policy"]).toBe("keep");
  expect(config.metadata.name).toMatch(/^kars-auth-env-[0-9a-f]{48}$/);
  expect(deployment.spec.template.metadata.annotations["kars.azure.com/helm-auth-env-sha256"])
    .toBe(config.metadata.annotations["kars.azure.com/helm-auth-env-sha256"]);
  return config;
}
function canonical(value: any): any {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])]));
  return value;
}
function hash(value: any): string { return createHash("sha256").update(JSON.stringify(canonical(value))).digest("hex"); }
function establishedCrd(): Map {
  const crd = resource(render(), "CustomResourceDefinition", "karsauthconfigs.kars.azure.com");
  crd.status = { conditions: [{ type: "Established", status: "True" }], storedVersions: ["v1alpha1"] };
  return crd;
}
function owned(): Map {
  const config = structuredClone(resource(render(values()), "KarsAuthConfig", "default"));
  config.metadata.resourceVersion = "123";
  config.status = { phase: "Ready", observedGeneration: 2 };
  return config;
}
// Replace only lookup results in an isolated chart: these are offline render
// simulations, not evidence of API discovery, admission, ownership or rollout.
function lookups(live: Map = {}, crd?: Map): string {
  const copy = join(temporary(), "chart");
  cpSync(chart, copy, { recursive: true });
  const path = join(copy, "templates/_auth-config.tpl");
  let source = readFileSync(path, "utf8");
  const substitutions: Array<[string, Map]> = [[
    'lookup "kars.azure.com/v1alpha1" "KarsAuthConfig" "" "default"', live,
  ]];
  substitutions.push([
    'lookup "apiextensions.k8s.io/v1" "CustomResourceDefinition" "" "karsauthconfigs.kars.azure.com"',
    crd ?? establishedCrd(),
  ]);
  for (const [expression, object] of substitutions) {
    expect(source.split(expression)).toHaveLength(2);
    source = source.replace(expression, `(${JSON.stringify(JSON.stringify(object))} | fromJson)`);
  }
  writeFileSync(path, source);
  return copy;
}

describe("Helm-owned Entra identity configuration (actual Helm, offline)", () => {
  it("preserves disabled generic behavior in the supported Core namespace without an identity capability", () => {
    const docs = render({}, { noCapability: true });
    expect(docs.some(d => d.kind === "KarsAuthConfig")).toBe(false);
    expect(docs.some(d => d.metadata.name === "entra-auth-sidecar")).toBe(false);
  });
  it("accepts legacy saved disabled values without the new authConfig key", () => {
    const copy = lookups();
    const path = join(copy, "values.yaml");
    const previous = parse(readFileSync(path, "utf8"));
    delete previous.entraSidecar.authConfig;
    writeFileSync(path, stringify(previous));
    expect(render({}, { chart: copy, noCapability: true }).some(d => d.kind === "KarsAuthConfig")).toBe(false);
  });
  it.each(["ManagedIdentityImds", "WorkloadIdentity"])("renders canonical %s identity and exact required projection", mode => {
    const docs = render(values(spec(mode)));
    const auth = resource(docs, "KarsAuthConfig", "default");
    expect(auth.metadata.namespace).toBeUndefined();
    expect(auth.spec.controller.credentialMode).toBe(mode);
    expect(auth.metadata.annotations["meta.helm.sh/release-name"]).toBe("kars");
    expect(auth.metadata.annotations["meta.helm.sh/release-namespace"]).toBe("kars-system");
    expect(auth.metadata.annotations["kars.azure.com/helm-auth-spec-sha256"]).toBe(hash(auth.spec));
    expect(auth.metadata.labels["app.kubernetes.io/managed-by"]).toBe("Helm");
    expect(auth.metadata.annotations["helm.sh/resource-policy"]).toBe("keep");
    const env = projection(docs).data;
    expect(env.AzureAd__TenantId).toBe(tenantId);
    expect(env.AzureAd__ClientId).toBe(blueprintClientId);
    expect(env.AzureAd__Instance).toBe("https://login.microsoftonline.com/");
    const sa = resource(docs, "ServiceAccount", "entra-auth-sidecar");
    const pod = resource(docs, "Deployment", "entra-auth-sidecar").spec.template;
    if (mode === "WorkloadIdentity") {
      expect(sa.metadata.annotations["azure.workload.identity/client-id"]).toBe(blueprintClientId);
      expect(sa.metadata.annotations["azure.workload.identity/tenant-id"]).toBe(tenantId);
      expect(pod.metadata.labels["azure.workload.identity/use"]).toBe("true");
      expect(env[`${prefix}SourceType`]).toBe("SignedAssertionFilePath");
      expect(env[`${prefix}SignedAssertionFileDiskPath`]).toBe("/var/run/secrets/azure/tokens/azure-identity-token");
      expect(env[`${prefix}ManagedIdentityClientId`]).toBeUndefined();
    } else {
      expect(sa.metadata.annotations).toBeUndefined();
      expect(pod.metadata.labels["azure.workload.identity/use"]).toBeUndefined();
      expect(env[`${prefix}SourceType`]).toBe("SignedAssertionFromManagedIdentity");
      expect(env[`${prefix}ManagedIdentityClientId`]).toBe(mi);
      expect(env[`${prefix}SignedAssertionFileDiskPath`]).toBeUndefined();
    }
  });
  it("leaves controller workload identity independent from sidecar IMDS", () => {
    const v = values(spec("ManagedIdentityImds"));
    v.azure = { workloadIdentity: { enabled: true, clientId: blueprintObjectId } };
    const docs = render(v);
    expect(resource(docs, "ServiceAccount", "kars-controller").metadata.annotations["azure.workload.identity/client-id"])
      .toBe(blueprintObjectId);
    expect(resource(docs, "ServiceAccount", "entra-auth-sidecar").metadata.annotations).toBeUndefined();
  });
  it("allows well-formed recorded MI metadata in WI without emitting an IMDS credential", () => {
    const auth = spec(); auth.controller.managedIdentityClientId = mi;
    expect(projection(render(values(auth))).data[`${prefix}ManagedIdentityClientId`]).toBeUndefined();
  });
  function dispatcherValues(mode = "WorkloadIdentity"): Map {
    const auth = spec(mode);
    auth.controller.managedIdentityClientId = mi;
    auth.meshAuthBackend = "EntraAgentIdentity";
    auth.agentId.sponsorUserObjectIds = ["eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"];
    return { ...values(auth), azure: { entraAuth: { enabled: true } }, missionDispatcher: { enabled: true, authMode: "entra" } };
  }
  it.each(["ManagedIdentityImds", "WorkloadIdentity"])("wires the existing Entra dispatcher with %s sidecar credentials", mode => {
    const docs = render(dispatcherValues(mode));
    const deployment = resource(docs, "Deployment", "kars-mission-dispatcher");
    expect(deployment.spec.replicas).toBe(1);
    expect(deployment.spec.strategy).toEqual({ type: "Recreate" });
    const container = deployment.spec.template.spec.containers[0];
    expect(container.env).toEqual(expect.arrayContaining([
      { name: "KARS_MISSION_AUTH_MODE", value: "entra" },
      { name: "KARS_MISSION_TENANT_ID", value: tenantId },
      { name: "KARS_MISSION_BLUEPRINT_ID", value: blueprintClientId },
    ]));
    expect(container.envFrom).toBeUndefined();
    expect(resource(docs, "Role", "kars-mission-dispatcher-root").rules).toEqual([
      { apiGroups: [""], resources: ["secrets"], resourceNames: ["kars-mission-dispatcher-identity", "kars-mission-dispatcher-entra"], verbs: ["get"] },
    ]);
    const rules = resource(docs, "ClusterRole", "kars-kars-system-mission-dispatcher").rules;
    expect(rules.some((rule: Map) => rule.resources.includes("secrets") || rule.resources.includes("*") || rule.verbs.includes("*"))).toBe(false);
    expect(docs.some(d => ["Secret", "Service", "Ingress"].includes(d.kind) && d.metadata.name.startsWith("kars-mission-dispatcher"))).toBe(false);
    expect(resource(docs, "Deployment", "kars-controller").spec.template.spec.containers[0].env)
      .toContainEqual({ name: "KARS_DISABLE_ENTRA_AUTH", value: "0" });
    expect(projection(docs).data.DownstreamApis__AgentMesh__RequestAppToken).toBe("true");
  });
  const dispatcherInvalid: Array<[string, (v: Map) => void, RegExp]> = [
    ["disabled sidecar", v => { v.entraSidecar.enabled = false; }, /requires entraSidecar.enabled=true/],
    ["disabled controller Entra", v => { v.azure.entraAuth.enabled = false; }, /requires azure.entraAuth.enabled=true/],
    ["anonymous mesh", v => { v.entraSidecar.authConfig.meshAuthBackend = "Anonymous"; }, /requires meshAuthBackend=EntraAgentIdentity/],
    ["missing mesh backend", v => { delete v.entraSidecar.authConfig.meshAuthBackend; }, /requires meshAuthBackend=EntraAgentIdentity/],
    ["missing provisioner MI", v => { delete v.entraSidecar.authConfig.controller.managedIdentityClientId; }, /managedIdentityClientId.*lowercase/],
    ["uppercase provisioner MI", v => { v.entraSidecar.authConfig.controller.managedIdentityClientId = mi.toUpperCase(); }, /managedIdentityClientId.*lowercase/],
    ["uppercase tenant", v => { v.entraSidecar.authConfig.tenant.tenantId = tenantId.toUpperCase(); }, /tenantId.*lowercase/],
    ["uppercase blueprint", v => { v.entraSidecar.authConfig.agentId.blueprintClientId = blueprintClientId.toUpperCase(); }, /blueprintClientId.*lowercase/],
    ["missing sponsors", v => { delete v.entraSidecar.authConfig.agentId.sponsorUserObjectIds; }, /requires nonempty sponsorUserObjectIds/],
    ["empty sponsors", v => { v.entraSidecar.authConfig.agentId.sponsorUserObjectIds = []; }, /requires nonempty sponsorUserObjectIds/],
    ["uppercase sponsor", v => { v.entraSidecar.authConfig.agentId.sponsorUserObjectIds = [mi.toUpperCase()]; }, /sponsorUserObjectIds.*lowercase/],
  ];
  it.each(dispatcherInvalid)("rejects dispatcher Entra with %s before installation", (_name, mutate, message) => {
    const v = dispatcherValues(); mutate(v);
    expect(() => render(v)).toThrow(message);
  });
  it("retains established-schema activation requirements for the Entra dispatcher", () => {
    expect(() => render(dispatcherValues(), { noCapability: true })).toThrow(/bootstrap Core with the sidecar disabled/);
    expect(() => render(dispatcherValues(), { unresolvedLookup: true })).toThrow(/CRD|schema/);
  });
  it("accepts matching deprecated anchors case-insensitively", () => {
    const v = values(); v.entraSidecar.tenantId = tenantId.toUpperCase(); v.entraSidecar.blueprintClientId = blueprintClientId;
    expect(projection(render(v)).data.AzureAd__TenantId).toBe(tenantId);
  });
  it.each(["tenantId", "blueprintClientId"])("refuses conflicting deprecated %s", key => {
    const v = values(); v.entraSidecar[key] = mi;
    expect(() => render(v)).toThrow(/conflicts with authConfig/);
  });
  const invalid: Array<[string, (s: Map) => void]> = [
    ["missing tenant", s => { delete s.tenant; }],
    ["missing tenant ID", s => { delete s.tenant.tenantId; }],
    ["blank tenant ID", s => { s.tenant.tenantId = ""; }],
    ["padded tenant ID", s => { s.tenant.tenantId = ` ${tenantId}`; }],
    ["invalid tenant ID", s => { s.tenant.tenantId = "tenant"; }],
    ["missing blueprint app ID", s => { delete s.agentId.blueprintClientId; }],
    ["missing blueprint object ID", s => { delete s.agentId.blueprintObjectId; }],
    ["numeric blueprint ID", s => { s.agentId.blueprintClientId = 42; }],
    ["missing mode", s => { delete s.controller.credentialMode; }],
    ["invalid mode", s => { s.controller.credentialMode = "Automatic"; }],
    ["missing IMDS client", s => { s.controller = { credentialMode: "ManagedIdentityImds" }; }],
    ["blank IMDS client", s => { s.controller = { credentialMode: "ManagedIdentityImds", managedIdentityClientId: " " }; }],
    ["invalid MI resource", s => { s.controller.managedIdentityResourceId = "bad"; }],
    ["unknown controller key", s => { s.controller.clientSecret = "not-allowed"; }],
    ["unknown spec key", s => { s.unrecognized = true; }],
    ["invalid authority", s => { s.tenant.authorityHost = "http://login.example.test"; }],
    ["authority credentials", s => { s.tenant.authorityHost = "https://user:password@host.test"; }],
    ["invalid backend", s => { s.meshAuthBackend = "Other"; }],
    ["null controller", s => { s.controller = null; }],
    ["invalid sponsors", s => { s.agentId.sponsorUserObjectIds = ["not-a-guid"]; }],
    ["empty RBAC role list", s => { s.foundryRbac = [{ scope: "/subscriptions/example", roleDefinitionIds: [] }]; }],
  ];
  it.each(invalid)("refuses %s before rendering manifests", (_name, mutate) => {
    const auth = spec(); mutate(auth);
    expect(() => render(values(auth))).toThrow(/schema|Invalid|Must|must|required/);
  });
  it("refuses enablement with missing canonical values", () => {
    expect(() => render({ entraSidecar: { enabled: true } })).toThrow(/authConfig/);
  });
  it("refuses unsupported namespace", () => {
    expect(() => render(values(), { namespace: "other" })).toThrow(/requires namespace kars-system|in kars-system/);
  });
  it("requires established schema before identity activation", () => {
    expect(() => render(values(), { noCapability: true })).toThrow(/bootstrap Core with the sidecar disabled/);
  });
  const hook = { enabled: true, architecture: "amd64", image: { repository: "example.test/kars-controller", digest: `sha256:${"a".repeat(64)}` }, apiDestinations: [{ cidr: "10.0.0.1/32", port: 443 }] };
  it("refuses identity alongside the cold hook before Helm can attempt unmapped resources", () => {
    const v = values(); v.schemaHook = hook;
    expect(() => render(v, { noCapability: true, args: ["--kube-version", "1.35.0"] })).toThrow(/cannot run with the cold schemaHook|schemaHook requires entraSidecar.enabled=false/);
  });
  it("cannot bypass established-schema validation by enabling the hook with API discovery", () => {
    const v = values(); v.schemaHook = hook;
    expect(() => render(v, { args: ["--kube-version", "1.35.0"] })).toThrow(/cannot run with the cold schemaHook|schemaHook requires entraSidecar.enabled=false/);
  });
  it("renders disabled Core CRDs and only post-hook custom resources without a bootstrap Job", () => {
    const docs = render({}, { noCapability: true });
    expect(resource(docs, "CustomResourceDefinition", "karsauthconfigs.kars.azure.com")).toBeDefined();
    expect(docs.some(d => d.kind === "Job" && d.metadata.namespace === "kars-system"
      && d.metadata.annotations?.["helm.sh/hook"]?.includes("pre-install"))).toBe(false);
    const custom = docs.filter(d => d.apiVersion?.startsWith("kars.azure.com/"));
    expect(custom.filter(d => !d.metadata.annotations?.["helm.sh/hook"])).toEqual([]);
    expect(custom.map(d => ({ kind: d.kind, name: d.metadata.name, hook: d.metadata.annotations?.["helm.sh/hook"] })))
      .toEqual([
        { kind: "ToolPolicy", name: "kars-default", hook: "post-install,post-upgrade" },
      ]);
  });
  it("permits explicit same-release identity upgrade only with an established supported schema", () => {
    expect(() => render(values(), { noCapability: true, args: ["--is-upgrade"] })).toThrow(/bootstrap Core with the sidecar disabled/);
    expect(() => render(values(), { unresolvedLookup: true, args: ["--is-upgrade"] })).toThrow(/CRD lookup is empty/);
    const activated = render(values(), { args: ["--is-upgrade"] });
    const auth = resource(activated, "KarsAuthConfig", "default");
    expect(auth.metadata.annotations["meta.helm.sh/release-name"]).toBe("kars");
    expect(auth.metadata.annotations["meta.helm.sh/release-namespace"]).toBe("kars-system");
    expect(activated.some(d => d.metadata.namespace === "kars-system"
      && d.metadata.annotations?.["helm.sh/hook"]?.includes("pre-install"))).toBe(false);
  });
  it("overlays optional MI fields and explicit mode without modifying the generated baseline", () => {
    const actual = resource(render(), "CustomResourceDefinition", "karsauthconfigs.kars.azure.com");
    const original = parse(readFileSync(join(chart, "files/auth-config-schema-baseline.yaml"), "utf8"));
    const schema = actual.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.controller;
    expect(schema.required).toBeUndefined();
    expect(schema.properties.credentialMode).toEqual({ type: "string", default: "ManagedIdentityImds", enum: ["ManagedIdentityImds", "WorkloadIdentity"] });
    for (const key of ["managedIdentityClientId", "managedIdentityResourceId", "managedIdentityPrincipalId"]) {
      expect(schema.properties[key]).toEqual({ type: "string", nullable: true });
    }
    actual.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.controller = original.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.controller;
    expect(actual).toEqual(original);
  });
  it("preserves false and emits complete downstream defaults", () => {
    const auth = spec();
    auth.downstreamApis = {
      Graph: { baseUrl: "https://graph.microsoft.com", scopes: ["https://graph.microsoft.com/.default"], requestAppToken: false },
      Foundry: { baseUrl: "https://example.services.ai.azure.com", scopes: ["https://ai.azure.com/.default"] },
    };
    const docs = render(values(auth)); const env = projection(docs).data;
    expect(env.DownstreamApis__Graph__RequestAppToken).toBe("false");
    expect(env.DownstreamApis__Foundry__RequestAppToken).toBe("true");
    expect(env.DownstreamApis__Graph__Scopes__0).toBe("https://graph.microsoft.com/.default");
    expect(resource(docs, "KarsAuthConfig", "default").spec.downstreamApis.Graph.requestAppToken).toBe(false);
  });
  it.each(["Graph__Scopes__0", "Graph:Scopes", "_Graph", "Graph-Api", "Graph.Api", "Graph Api"])("refuses unsafe ASP.NET API key %s", key => {
    const auth = spec(); auth.downstreamApis = { [key]: { baseUrl: "https://example.test", scopes: ["scope"] } };
    expect(() => render(values(auth))).toThrow(/schema|Additional|additional/);
  });
  it("refuses case-insensitive duplicate API keys", () => {
    const api = { baseUrl: "https://example.test", scopes: ["scope"] };
    const auth = spec(); auth.downstreamApis = { Graph: api, graph: api };
    expect(() => render(values(auth))).toThrow(/unique ignoring case/);
  });
  it.each(["false", null, 0])("refuses nonboolean requestAppToken %j", token => {
    const auth = spec(); auth.downstreamApis = { Graph: { baseUrl: "https://example.test", scopes: ["scope"], requestAppToken: token } };
    expect(() => render(values(auth))).toThrow(/schema|boolean/);
  });
  it.each(["", "  ", " api://custom/.default "])("matches automatic AgentMesh audience trimming %j", audience => {
    const auth = spec(); auth.meshAuthBackend = "EntraAgentIdentity"; auth.meshAuthAudience = audience;
    const env = projection(render(values(auth))).data;
    expect(env.DownstreamApis__AgentMesh__Scopes__0).toBe(audience.trim() || "api://agentmesh/.default");
    expect(env.DownstreamApis__AgentMesh__BaseUrl).toBe("https://agentmesh.invalid/");
  });
  it("respects explicit case-insensitive AgentMesh including false", () => {
    const auth = spec(); auth.meshAuthBackend = "EntraAgentIdentity";
    auth.downstreamApis = { agentMesh: { baseUrl: "https://relay.example.test", scopes: ["api://custom/.default"], requestAppToken: false } };
    const env = projection(render(values(auth))).data;
    expect(env.DownstreamApis__agentMesh__RequestAppToken).toBe("false");
    expect(env.DownstreamApis__AgentMesh__BaseUrl).toBeUndefined();
  });
  it("uses stable immutable names, changing the reference for every environment change", () => {
    const initial = projection(render(values()));
    expect(projection(render(values())).metadata.name).toBe(initial.metadata.name);
    const variants = [spec("ManagedIdentityImds"), spec(), spec(), spec()];
    variants[1].tenant.tenantId = mi;
    variants[2].agentId.blueprintClientId = mi;
    variants[3].downstreamApis = { Graph: { baseUrl: "https://graph.microsoft.com", scopes: ["scope"] } };
    for (const auth of variants) expect(projection(render(values(auth))).metadata.name).not.toBe(initial.metadata.name);
    const metadataOnly = spec(); metadataOnly.agentId.sponsorUserObjectIds = [mi];
    expect(projection(render(values(metadataOnly))).metadata.name).toBe(initial.metadata.name);
  }, 30_000);
  it("accepts current owned singleton after server metadata/status changes in a lookup simulation", () => {
    const live = owned();
    expect(live.metadata.annotations["kars.azure.com/helm-auth-spec-sha256"]).toBe(hash(live.spec));
    expect(render(values(), { chart: lookups(live) }).some(d => d.kind === "KarsAuthConfig")).toBe(true);
  });
  it("allows a reviewed new spec when the old live spec still matches its recorded hash", () => {
    const next = spec(); next.tenant.tenantId = mi;
    expect(resource(render(values(next), { chart: lookups(owned()) }), "KarsAuthConfig", "default").spec.tenant.tenantId).toBe(mi);
  });
  it.each(["release-name", "release-namespace", "managed-by", "hash", "spec"])("refuses invalid retained %s in lookup simulation", change => {
    const live = owned();
    if (change === "managed-by") live.metadata.labels["app.kubernetes.io/managed-by"] = "kubectl";
    else if (change === "hash") delete live.metadata.annotations["kars.azure.com/helm-auth-spec-sha256"];
    else if (change === "spec") live.spec.tenant.tenantId = mi;
    else live.metadata.annotations[`meta.helm.sh/${change}`] = "other";
    expect(() => render(values(), { chart: lookups(live) })).toThrow(/not owned|explicit configuration migration|changed outside Helm/);
  });
  it("refuses discovery without a retrieved schema during an enabled offline render", () => {
    expect(() => render(values(), { unresolvedLookup: true })).toThrow(/CRD lookup is empty; API discovery alone/);
  });
  it("accepts an established CRD with unrelated server metadata and nonterminating status (lookup simulation)", () => {
    const current = establishedCrd();
    current.metadata.resourceVersion = "123";
    current.status.conditions.push({ type: "Terminating", status: "False" });
    expect(render(values(), { chart: lookups({}, current) }).some(d => d.kind === "KarsAuthConfig")).toBe(true);
  });
  it.each([
    ["missing status", (c: Map) => { delete c.status; }],
    ["missing conditions", (c: Map) => { delete c.status.conditions; }],
    ["empty conditions", (c: Map) => { c.status.conditions = []; }],
    ["unestablished CRD", (c: Map) => { c.status.conditions[0].status = "False"; }],
    ["unknown establishment", (c: Map) => { c.status.conditions[0].status = "Unknown"; }],
    ["only NamesAccepted", (c: Map) => { c.status.conditions[0].type = "NamesAccepted"; }],
    ["terminating condition", (c: Map) => { c.status.conditions.push({ type: "Terminating", status: "True" }); }],
    ["CRD deletion timestamp", (c: Map) => { c.metadata.deletionTimestamp = "2026-09-25T12:00:00Z"; }],
  ] as [string, (crd: Map) => void][]) ("refuses activation with %s despite compatible discovered schema (lookup simulation)", (_name, mutate) => {
    const current = establishedCrd();
    mutate(current);
    expect(() => render(values(), { chart: lookups({}, current), args: ["--is-upgrade"] }))
      .toThrow(/CRD must be Established=True and not terminating/);
  });
  it("refuses activation against a terminating owned singleton even when its reviewed hash matches", () => {
    const live = owned();
    live.metadata.deletionTimestamp = "2026-09-25T12:00:00Z";
    expect(live.metadata.annotations["kars.azure.com/helm-auth-spec-sha256"]).toBe(hash(live.spec));
    expect(() => render(values(), { chart: lookups(live), args: ["--is-upgrade"] }))
      .toThrow(/KarsAuthConfig\/default is terminating/);
  });
  it.each([
    ["absent spec", ["spec"], undefined],
    ["scalar spec", ["spec"], "invalid"],
    ["wrong group", ["spec", "group"], {}],
    ["wrong scope", ["spec", "scope"], "Namespaced"],
    ["absent names", ["spec", "names"], undefined],
    ["scalar names", ["spec", "names"], "invalid"],
    ["wrong kind", ["spec", "names", "kind"], []],
    ["wrong plural", ["spec", "names", "plural"], "others"],
    ["absent versions", ["spec", "versions"], undefined],
    ["object versions", ["spec", "versions"], {}],
    ["empty versions", ["spec", "versions"], []],
    ["scalar version", ["spec", "versions", "0"], "invalid"],
    ["wrong version", ["spec", "versions", "0", "name"], {}],
    ["unserved version", ["spec", "versions", "0", "served"], false],
    ["string served", ["spec", "versions", "0", "served"], "true"],
    ["non-storage version", ["spec", "versions", "0", "storage"], false],
    ["absent schema", ["spec", "versions", "0", "schema"], undefined],
    ["scalar schema", ["spec", "versions", "0", "schema"], "invalid"],
    ["missing properties", ["spec", "versions", "0", "schema", "openAPIV3Schema", "properties"], undefined],
  ] as [string, string[], unknown][]) ("refuses %s with a precise schema error (lookup simulation)", (_name, path, value) => {
    const current = resource(render(), "CustomResourceDefinition", "karsauthconfigs.kars.azure.com");
    const parent = path.slice(0, -1).reduce((obj, key) => obj[key], current);
    if (value === undefined) delete parent[path.at(-1)!];
    else parent[path.at(-1)!] = value;
    expect(() => render(values(), { chart: lookups({}, current) })).toThrow(/does not match the supported credential-mode schema/);
  });
  it.each([
    ["wrong mode type", (c: Map) => { c.properties.credentialMode.type = "object"; }],
    ["wrong mode default", (c: Map) => { c.properties.credentialMode.default = "WorkloadIdentity"; }],
    ["extra mode", (c: Map) => { c.properties.credentialMode.enum.push("Other"); }],
    ["missing mode", (c: Map) => { delete c.properties.credentialMode; }],
    ["required managed identity", (c: Map) => { c.required = ["managedIdentityClientId"]; }],
    ["wrong managed identity type", (c: Map) => { c.properties.managedIdentityClientId.type = "integer"; }],
    ["non-nullable managed identity", (c: Map) => { delete c.properties.managedIdentityResourceId.nullable; }],
  ] as [string, (controller: Map) => void][])("refuses %s despite discovery (lookup simulation)", (_name, mutate) => {
    const current = resource(render(), "CustomResourceDefinition", "karsauthconfigs.kars.azure.com");
    mutate(current.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.controller);
    expect(() => render(values(), { chart: lookups({}, current) })).toThrow(/does not match the supported credential-mode schema/);
  });
  it("fails when the required preserved schema baseline is missing", () => {
    const copy = join(temporary(), "chart"); cpSync(chart, copy, { recursive: true });
    rmSync(join(copy, "files/auth-config-schema-baseline.yaml"));
    expect(() => render({}, { chart: copy })).toThrow(/AuthConfig schema baseline is required/);
  });
  it("refuses a stale live CRD even when discovery advertises the API (lookup simulation)", () => {
    const stale = parse(readFileSync(join(chart, "files/auth-config-schema-baseline.yaml"), "utf8"));
    expect(() => render(values(), { chart: lookups({}, stale) })).toThrow(/does not match the supported credential-mode schema/);
    const current = establishedCrd();
    expect(render(values(), { chart: lookups({}, current) }).some(d => d.kind === "KarsAuthConfig")).toBe(true);
  });
});
