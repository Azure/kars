// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { parseAllDocuments, stringify } from "yaml";

type Map = Record<string, any>;
const root = process.env.KARS_TEST_REPO_ROOT ?? fileURLToPath(new URL("../../../", import.meta.url));
const chart = resolve(root, "deploy/helm/kars");
const capability = "kars.azure.com/v1alpha1/KarsAuthConfig";
const tenantId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const blueprintClientId = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const resourceClientId = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
const otherResourceClientId = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const directories: string[] = [];
let supportedCrd: Map | undefined;

function workspace(): string {
  const path = join(root, `.helm-agentmesh-entra-${randomUUID()}`);
  mkdirSync(path);
  directories.push(path);
  return path;
}
afterEach(() => {
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true });
});
function spec(): Map {
  return {
    tenant: { tenantId },
    agentId: { blueprintClientId, blueprintObjectId: "cccccccc-cccc-cccc-cccc-cccccccccccc" },
    controller: { credentialMode: "WorkloadIdentity" },
    meshAuthBackend: "EntraAgentIdentity",
    meshAuthAudience: `api://${resourceClientId}/.default`,
  };
}
function values(auth = spec()): Map {
  return { agentMesh: { enabled: true }, entraSidecar: { enabled: true, authConfig: auth } };
}
function render(v: Map = {}, options: { chart?: string; noCapability?: boolean; unresolvedLookup?: boolean; args?: string[] } = {}): Map[] {
  const input = join(workspace(), "values.yaml");
  writeFileSync(input, stringify(v, { blockQuote: false }));
  const selectedChart = options.chart ?? (v.entraSidecar?.enabled && !options.noCapability && !options.unresolvedLookup ? lookups() : chart);
  const output = execFileSync("helm", [
    "template", "kars", selectedChart, "--namespace", "kars-system", "-f", input,
    ...(!options.noCapability ? ["--api-versions", capability] : []), ...(options.args ?? []),
  ], { encoding: "utf8", timeout: 30_000, stdio: ["ignore", "pipe", "pipe"] });
  return parseAllDocuments(output).map(doc => {
    if (doc.errors.length) throw doc.errors[0];
    return doc.toJSON() as Map;
  }).filter(Boolean);
}
function resource(docs: Map[], kind: string, name: string): Map {
  const result = docs.find(doc => doc.kind === kind && doc.metadata.name === name);
  if (!result) throw new Error(`Missing ${kind}/${name}`);
  return result;
}
function establishedCrd(): Map {
  supportedCrd ??= resource(render(), "CustomResourceDefinition", "karsauthconfigs.kars.azure.com");
  const crd = structuredClone(supportedCrd);
  crd.status = { conditions: [{ type: "Established", status: "True" }], storedVersions: ["v1alpha1"] };
  return crd;
}
// Same lookup-only substitution as helm-identity.test.ts: preserve the actual
// schema and ownership guards in disposable charts; this is not live acceptance.
function lookups(live: Map = {}, crd = establishedCrd()): string {
  const copy = join(workspace(), "chart");
  cpSync(chart, copy, { recursive: true });
  const path = join(copy, "templates/_auth-config.tpl");
  let source = readFileSync(path, "utf8");
  const substitutions: Array<[string, Map]> = [
    ['lookup "kars.azure.com/v1alpha1" "KarsAuthConfig" "" "default"', live],
    ['lookup "apiextensions.k8s.io/v1" "CustomResourceDefinition" "" "karsauthconfigs.kars.azure.com"', crd],
  ];
  for (const [expression, object] of substitutions) {
    expect(source.split(expression)).toHaveLength(2);
    source = source.replace(expression, `(${JSON.stringify(JSON.stringify(object))} | fromJson)`);
  }
  writeFileSync(path, source);
  return copy;
}
function projection(docs: Map[]): Map {
  const pod = resource(docs, "Deployment", "entra-auth-sidecar").spec.template;
  return resource(docs, "ConfigMap", pod.spec.containers[0].envFrom[0].configMapRef.name).data;
}
function meshEnv(docs: Map[], name: string): Map[] {
  return resource(docs, "Deployment", name).spec.template.spec.containers[0].env;
}
function verifierPair(docs: Map[], audience = resourceClientId, tenant = tenantId): void {
  for (const name of ["registry", "relay"]) {
    const env = meshEnv(docs, name);
    expect(env.filter(item => item.name.startsWith("AGENTMESH_ENTRA_"))).toEqual([
      { name: "AGENTMESH_ENTRA_AUDIENCE", value: audience },
      { name: "AGENTMESH_ENTRA_TENANT_ID", value: tenant },
    ]);
    expect(env.some(item => item.name === "AGENTMESH_RELAY_ALLOW_UNAUTHED_DID")).toBe(false);
  }
}

describe("managed Helm AgentMesh Entra verifier (actual Helm, lookup fixtures)", () => {
  it("leaves default external mesh and no-Entra chart behavior unchanged", () => {
    const defaults = render({}, { noCapability: true });
    expect(defaults.some(doc => doc.kind === "Deployment" && ["registry", "relay"].includes(doc.metadata.name))).toBe(false);
    const docs = render({ agentMesh: { enabled: true } }, { noCapability: true });
    for (const name of ["registry", "relay"]) {
      expect(meshEnv(docs, name)).toEqual([
        { name: "HOST", value: "0.0.0.0" },
        { name: "PORT", value: name === "registry" ? "8082" : "8083" },
        { name: "LOG_LEVEL", value: "info" },
      ]);
      const container = resource(docs, "Deployment", name).spec.template.spec.containers[0];
      expect(container.image).toBe(`ghcr.io/azure/kars-agentmesh-${name}:latest`);
      expect(container.imagePullPolicy).toBe("Always");
      expect(container.readinessProbe.httpGet.path).toBe("/health");
    }
  });
  it("uses the AL3 release repositories when image settings are omitted", () => {
    const docs = render({ agentMesh: { enabled: true, registry: { image: null }, relay: { image: null } } }, { noCapability: true });
    for (const name of ["registry", "relay"]) {
      expect(resource(docs, "Deployment", name).spec.template.spec.containers[0].image)
        .toBe(`ghcr.io/azure/kars-agentmesh-${name}:latest`);
    }
  });
  it("does not impose managed-verifier scope, authority or sidecar requirements on external mesh", () => {
    const v = values();
    v.agentMesh.enabled = false;
    v.entraSidecar.authConfig.meshAuthAudience = "api://agentmesh/.default";
    v.entraSidecar.authConfig.tenant.authorityHost = "https://login.microsoftonline.us/";
    expect(projection(render(v)).DownstreamApis__AgentMesh__Scopes__0).toBe("api://agentmesh/.default");
    v.entraSidecar.enabled = false;
    expect(() => render(v, { noCapability: true })).not.toThrow();
  });
  it("does not add a verifier for Anonymous even with a configured sidecar and downstream API", () => {
    const auth = spec();
    auth.meshAuthBackend = "Anonymous";
    auth.downstreamApis = { AgentMesh: { baseUrl: "https://mesh.example.test", scopes: ["api://custom/.default"], requestAppToken: false } };
    const docs = render(values(auth));
    for (const name of ["registry", "relay"]) expect(meshEnv(docs, name).some(item => item.name.startsWith("AGENTMESH_ENTRA_"))).toBe(false);
  });
  it.each(["WorkloadIdentity", "ManagedIdentityImds"])("configures both initial Pod templates using %s without equating blueprint and resource", mode => {
    const auth = spec();
    auth.controller = { credentialMode: mode, ...(mode === "ManagedIdentityImds" ? { managedIdentityClientId: "dddddddd-dddd-dddd-dddd-dddddddddddd" } : {}) };
    const docs = render(values(auth));
    verifierPair(docs);
    expect(projection(docs).AzureAd__ClientId).toBe(blueprintClientId);
    expect(projection(docs).DownstreamApis__AgentMesh__Scopes__0).toBe(`api://${resourceClientId}/.default`);
    expect(projection(docs).DownstreamApis__AgentMesh__RequestAppToken).toBe("true");
    for (const name of ["registry", "relay"]) expect(meshEnv(docs, name).find(item => item.name === "AGENTMESH_ENTRA_AUDIENCE")?.value).not.toBe(blueprintClientId);
  });
  it.each([`${resourceClientId}/.default`, `api://${resourceClientId}/.default`, ` \tapi://${resourceClientId}/.default\n`])("maps only the normalized fallback resource scope %j to its v2 GUID audience", scope => {
    const auth = spec(); auth.meshAuthAudience = scope;
    const docs = render(values(auth));
    verifierPair(docs);
    expect(projection(docs).DownstreamApis__AgentMesh__Scopes__0).toBe(scope.trim());
  });
  it.each(["AgentMesh", "agentMesh", "AGENTMESH"])("uses explicit %s scopes and default app-token mode over meshAuthAudience", name => {
    const auth = spec();
    auth.meshAuthAudience = "api://agentmesh/.default";
    auth.downstreamApis = {
      [name]: { baseUrl: "https://mesh.example.test", scopes: [`${resourceClientId}/.default`] },
      Graph: { baseUrl: "https://graph.microsoft.com", scopes: ["https://graph.microsoft.com/.default"], requestAppToken: false },
    };
    const docs = render(values(auth));
    verifierPair(docs);
    const env = projection(docs);
    expect(env[`DownstreamApis__${name}__Scopes__0`]).toBe(`${resourceClientId}/.default`);
    expect(env[`DownstreamApis__${name}__RequestAppToken`]).toBe("true");
    expect(env.DownstreamApis__Graph__RequestAppToken).toBe("false");
    expect(resource(docs, "KarsAuthConfig", "default").spec.downstreamApis[name].requestAppToken).toBe(true);
  });
  it("prefers an explicit resource over a different valid fallback, with explicit requestAppToken=true", () => {
    const auth = spec();
    auth.downstreamApis = { agentMesh: { baseUrl: "https://mesh.example.test", scopes: [`api://${otherResourceClientId}/.default`], requestAppToken: true } };
    verifierPair(render(values(auth)), otherResourceClientId);
  });
  it("canonicalizes GUID casing for the v2 verifier without rewriting the sidecar spec or OAuth scope", () => {
    const auth = spec();
    auth.tenant.tenantId = tenantId.toUpperCase();
    auth.meshAuthAudience = `api://${resourceClientId.toUpperCase()}/.default`;
    const docs = render(values(auth));
    verifierPair(docs);
    expect(projection(docs).AzureAd__TenantId).toBe(tenantId.toUpperCase());
    expect(projection(docs).DownstreamApis__AgentMesh__Scopes__0).toBe(auth.meshAuthAudience);
  });
  it.each(["https://login.microsoftonline.com", "https://LOGIN.MICROSOFTONLINE.COM/"])("accepts only equivalent public authority %s", authority => {
    const auth = spec(); auth.tenant.authorityHost = authority;
    verifierPair(render(values(auth)));
  });
  it("updates both Pod templates and the sidecar projection on a reviewed resource/tenant change", () => {
    const initial = render(values());
    const live = structuredClone(resource(initial, "KarsAuthConfig", "default"));
    const auth = spec();
    auth.tenant.tenantId = "dddddddd-dddd-dddd-dddd-dddddddddddd";
    auth.meshAuthAudience = `api://${otherResourceClientId}/.default`;
    const next = render(values(auth), { chart: lookups(live), args: ["--is-upgrade"] });
    verifierPair(next, otherResourceClientId, auth.tenant.tenantId);
    for (const name of ["registry", "relay", "entra-auth-sidecar"]) {
      expect(resource(next, "Deployment", name).spec.template).not.toEqual(resource(initial, "Deployment", name).spec.template);
    }
    expect(projection(next).DownstreamApis__AgentMesh__Scopes__0).toBe(auth.meshAuthAudience);
  });
  it("requires the canonical sidecar when managed Entra mesh is enabled", () => {
    const v = values(); v.entraSidecar.enabled = false;
    expect(() => render(v)).toThrow(/requires entraSidecar.enabled=true/);
  });
  it.each([undefined, "", " \t\n", "api://agentmesh/.default"])("rejects an absent or ambiguous normalized fallback %j rather than guessing the blueprint", scope => {
    const auth = spec();
    if (scope === undefined) delete auth.meshAuthAudience;
    else auth.meshAuthAudience = scope;
    expect(() => render(values(auth))).toThrow(/unambiguous v2 resource scope/);
  });
  it.each([
    "api://custom/.default", `https://mesh.example.test/${resourceClientId}/.default`,
    resourceClientId, `api://${resourceClientId}`, `api://${resourceClientId}/read`,
    `api://${resourceClientId}/.DEFAULT`, `API://${resourceClientId}/.default`,
    `api://${resourceClientId}/.default/`, `api://${resourceClientId}/.default?version=2`,
    `api://${resourceClientId}/.default https://graph.microsoft.com/.default`,
    "api://eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeeZ/.default",
  ])("rejects ambiguous or delegated effective scope %j despite a valid fallback", scope => {
    const auth = spec();
    auth.downstreamApis = { agentMesh: { baseUrl: "https://mesh.example.test", scopes: [scope] } };
    expect(() => render(values(auth))).toThrow(/unambiguous v2 resource scope/);
  });
  it.each([false, "true", null])("rejects explicit delegated or malformed app-token setting %j", setting => {
    const auth = spec();
    auth.downstreamApis = { AGENTMESH: { baseUrl: "https://mesh.example.test", scopes: [auth.meshAuthAudience], requestAppToken: setting } };
    expect(() => render(values(auth))).toThrow(/requestAppToken|boolean/);
  });
  it.each([
    { scopes: [] },
    { scopes: [`api://${resourceClientId}/.default`, `api://${otherResourceClientId}/.default`] },
  ])("rejects zero or multiple effective scopes $scopes", ({ scopes }) => {
    const auth = spec();
    auth.downstreamApis = { AgentMesh: { baseUrl: "https://mesh.example.test", scopes } };
    expect(() => render(values(auth))).toThrow(/exactly one|scopes|items/);
  });
  it("rejects duplicate AgentMesh names ignoring case rather than choosing one verifier", () => {
    const auth = spec();
    const api = { baseUrl: "https://mesh.example.test", scopes: [auth.meshAuthAudience] };
    auth.downstreamApis = { AgentMesh: api, AGENTMESH: api };
    expect(() => render(values(auth))).toThrow(/unique ignoring case/);
  });
  it.each([undefined, "", "common", "organizations", ` ${tenantId}`, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaZ"])("rejects a missing/invalid tenant %j rather than emitting a partial verifier", tenant => {
    const auth = spec();
    if (tenant === undefined) delete auth.tenant.tenantId;
    else auth.tenant.tenantId = tenant;
    expect(() => render(values(auth))).toThrow(/tenantId|tenant/);
  });
  it.each([
    "https://login.microsoftonline.us/", "https://login.chinacloudapi.cn/",
    "https://login.microsoftonline.com.example.test/", "http://login.microsoftonline.com/",
    `https://login.microsoftonline.com/${tenantId}/`, "https://login.microsoftonline.com:443/",
    "https://login.microsoftonline.com//", "https://login.microsoftonline.com/?tenant=common",
  ])("rejects authority %s incompatible with the official public tenant v2 issuer", authority => {
    const auth = spec(); auth.tenant.authorityHost = authority;
    expect(() => render(values(auth))).toThrow(/authorityHost/);
  });
  it("does not accept a conflicting deprecated tenant anchor", () => {
    const v = values(); v.entraSidecar.tenantId = otherResourceClientId;
    expect(() => render(v)).toThrow(/conflicts with authConfig/);
  });
  it("preserves no-discovery and unresolved-lookup bootstrap refusal", () => {
    expect(() => render(values(), { noCapability: true })).toThrow(/bootstrap Core with the sidecar disabled/);
    expect(() => render(values(), { unresolvedLookup: true })).toThrow(/CRD lookup is empty/);
  });
  it("preserves the cold-schema-hook identity activation prohibition", () => {
    const v = values();
    v.schemaHook = { enabled: true, architecture: "amd64", image: { repository: "example.test/controller", digest: `sha256:${"a".repeat(64)}` }, apiDestinations: [{ cidr: "10.0.0.1/32", port: 443 }] };
    expect(() => render(v, { args: ["--kube-version", "1.35.0"] })).toThrow(/cannot run with the cold schemaHook|schemaHook requires entraSidecar.enabled=false/);
  });
  it.each(["unestablished", "terminating", "stale schema"])("preserves bootstrap rejection for %s CRDs", state => {
    const crd = establishedCrd();
    if (state === "unestablished") crd.status.conditions[0].status = "False";
    else if (state === "terminating") crd.metadata.deletionTimestamp = "2026-10-07T00:00:00Z";
    else delete crd.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.controller.properties.credentialMode;
    expect(() => render(values(), { chart: lookups({}, crd) })).toThrow(/Established=True|supported credential-mode schema/);
  });
  it.each(["release-name", "release-namespace", "managed-by", "hash", "spec", "terminating"])("preserves existing singleton %s rejection", change => {
    const live = structuredClone(resource(render(values()), "KarsAuthConfig", "default"));
    if (change === "managed-by") live.metadata.labels["app.kubernetes.io/managed-by"] = "kubectl";
    else if (change === "hash") delete live.metadata.annotations["kars.azure.com/helm-auth-spec-sha256"];
    else if (change === "spec") live.spec.tenant.tenantId = otherResourceClientId;
    else if (change === "terminating") live.metadata.deletionTimestamp = "2026-10-07T00:00:00Z";
    else live.metadata.annotations[`meta.helm.sh/${change}`] = "other";
    expect(() => render(values(), { chart: lookups(live), args: ["--is-upgrade"] })).toThrow(/not owned|reviewed Helm spec hash|changed outside Helm|is terminating/);
  });
});
